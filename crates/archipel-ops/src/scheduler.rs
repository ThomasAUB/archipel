//! The operation scheduler (PLAN §3.2).
//!
//! * Writes are serialized **per repository** (network operations too, since
//!   they move refs); different repositories run in parallel.
//! * Background operations (periodic fetch) give way to the user: skipped
//!   when their repository is busy, cancelled when a write or network
//!   operation is submitted for it, and silent (no events).
//! * Reads run in a bounded pool; network operations in a smaller bounded pool.
//! * Every operation runs on a blocking worker thread (Git calls are
//!   blocking), returns an [`OpHandle`] with cancellation and a typed result,
//!   and emits [`OpEvent`]s (queued, started, progress, finished) for the
//!   operations drawer.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use archipel_model::settings::Settings;
use archipel_model::{CancelToken, Progress, ProgressSink, RepoId};
use tokio::sync::{Semaphore, broadcast, oneshot};

use crate::error::OpError;

/// Identifier of a scheduled operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct OpId(pub u64);

/// Resource class of an operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpClass {
    /// Read-only: bounded read pool, no repo lock.
    Read,
    /// Local write: serialized per repository.
    Write,
    /// Network: serialized per repository and bounded by the network pool.
    Network,
    /// Unattended work on one repository (periodic fetch): fails with
    /// [`OpError::Busy`] when the repository is locked, is cancelled by any
    /// later write or network operation on it, emits no event and uses no
    /// pool (callers run them one at a time).
    Background,
}

/// Description of an operation to schedule.
#[derive(Clone, Debug)]
pub struct OpSpec {
    /// Repository the operation touches (`None` for global operations).
    pub repo: Option<RepoId>,
    /// Resource class.
    pub class: OpClass,
    /// Human readable label for the operations drawer.
    pub label: String,
}

impl OpSpec {
    /// Shorthand constructor.
    #[must_use]
    pub fn new(repo: Option<RepoId>, class: OpClass, label: impl Into<String>) -> Self {
        Self {
            repo,
            class,
            label: label.into(),
        }
    }
}

/// What happened to an operation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OpEventKind {
    /// Waiting for a permit or the repo lock.
    Queued,
    /// Running.
    Started,
    /// Progress report.
    Progress(Progress),
    /// Finished; `Err` carries the error message.
    Finished(Result<(), String>),
}

/// An event of the operations drawer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpEvent {
    /// Operation.
    pub id: OpId,
    /// Repository, if any.
    pub repo: Option<RepoId>,
    /// Label given at submission.
    pub label: String,
    /// What happened.
    pub kind: OpEventKind,
}

/// Context handed to a running operation.
pub struct OpContext {
    id: OpId,
    spec: OpSpec,
    cancel: CancelToken,
    events: broadcast::Sender<OpEvent>,
}

impl OpContext {
    /// The operation's cancellation token (pass it to Git calls).
    #[must_use]
    pub fn cancel(&self) -> &CancelToken {
        &self.cancel
    }

    /// The operation id.
    #[must_use]
    pub fn id(&self) -> OpId {
        self.id
    }

    fn emit(&self, kind: OpEventKind) {
        if self.spec.class == OpClass::Background {
            return;
        }
        // No subscriber is fine: events are informational.
        let _ = self.events.send(OpEvent {
            id: self.id,
            repo: self.spec.repo,
            label: self.spec.label.clone(),
            kind,
        });
    }
}

impl ProgressSink for OpContext {
    fn report(&self, progress: Progress) {
        self.emit(OpEventKind::Progress(progress));
    }
}

/// Handle on a scheduled operation.
pub struct OpHandle<T> {
    id: OpId,
    cancel: CancelToken,
    result: oneshot::Receiver<Result<T, OpError>>,
}

impl<T> OpHandle<T> {
    /// The operation id.
    #[must_use]
    pub fn id(&self) -> OpId {
        self.id
    }

    /// Requests cancellation (queued operations never start; running ones
    /// stop at their next cancellation point).
    pub fn cancel(&self) {
        self.cancel.cancel();
    }

    /// A clone of the cancellation token.
    #[must_use]
    pub fn cancel_token(&self) -> CancelToken {
        self.cancel.clone()
    }

    /// Waits for the result asynchronously.
    pub async fn wait(self) -> Result<T, OpError> {
        self.result
            .await
            .unwrap_or_else(|_| Err(OpError::Worker("scheduler dropped".into())))
    }

    /// Waits for the result from a non-async thread. Never call it on a
    /// runtime worker thread.
    pub fn wait_blocking(self) -> Result<T, OpError> {
        self.result
            .blocking_recv()
            .unwrap_or_else(|_| Err(OpError::Worker("scheduler dropped".into())))
    }
}

/// The repo lock of a running background operation; unregisters its
/// cancellation token before releasing the lock.
struct BackgroundGuard {
    repo: RepoId,
    running: Arc<Mutex<HashMap<RepoId, CancelToken>>>,
    _lock: tokio::sync::OwnedMutexGuard<()>,
}

impl Drop for BackgroundGuard {
    fn drop(&mut self) {
        lock(&self.running).remove(&self.repo);
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Capacity of the event channel; slow subscribers miss old events.
const EVENT_CAPACITY: usize = 1024;

/// Schedules operations on a tokio runtime.
pub struct Scheduler {
    runtime: tokio::runtime::Handle,
    reads: Arc<Semaphore>,
    network: Arc<Semaphore>,
    repo_locks: Mutex<HashMap<RepoId, Arc<tokio::sync::Mutex<()>>>>,
    /// Cancellation of the background operation holding each repo lock.
    background: Arc<Mutex<HashMap<RepoId, CancelToken>>>,
    events: broadcast::Sender<OpEvent>,
    next_id: AtomicU64,
}

impl Scheduler {
    /// Creates a scheduler running on `runtime`, with pool sizes from `settings`.
    #[must_use]
    pub fn new(runtime: tokio::runtime::Handle, settings: &Settings) -> Self {
        let reads = if settings.read_concurrency == 0 {
            std::thread::available_parallelism().map_or(4, std::num::NonZero::get)
        } else {
            settings.read_concurrency
        };
        Self {
            runtime,
            reads: Arc::new(Semaphore::new(reads)),
            network: Arc::new(Semaphore::new(settings.network_concurrency.max(1))),
            repo_locks: Mutex::new(HashMap::new()),
            background: Arc::default(),
            events: broadcast::channel(EVENT_CAPACITY).0,
            next_id: AtomicU64::new(1),
        }
    }

    /// Subscribes to operation events.
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<OpEvent> {
        self.events.subscribe()
    }

    /// The runtime operations run on.
    #[must_use]
    pub fn runtime(&self) -> &tokio::runtime::Handle {
        &self.runtime
    }

    fn repo_lock(&self, repo: RepoId) -> Arc<tokio::sync::Mutex<()>> {
        lock(&self.repo_locks).entry(repo).or_default().clone()
    }

    /// Schedules `job`. It runs on a blocking worker once its resources are free.
    #[allow(clippy::needless_pass_by_value)] // owned spec reads best at call sites
    pub fn submit<T, F>(&self, spec: OpSpec, job: F) -> OpHandle<T>
    where
        T: Send + 'static,
        F: FnOnce(&OpContext) -> Result<T, OpError> + Send + 'static,
    {
        let id = OpId(self.next_id.fetch_add(1, Ordering::Relaxed));
        let cancel = CancelToken::new();
        let (tx, rx) = oneshot::channel();
        let ctx = OpContext {
            id,
            spec: spec.clone(),
            cancel: cancel.clone(),
            events: self.events.clone(),
        };
        ctx.emit(OpEventKind::Queued);
        // Taken right away for a background operation, waited for otherwise.
        let mut held = None;
        let repo_lock = match (spec.class, spec.repo) {
            (OpClass::Read, _) | (_, None) => None,
            (OpClass::Background, Some(repo)) => {
                let mut running = lock(&self.background);
                let Ok(guard) = self.repo_lock(repo).try_lock_owned() else {
                    let _ = tx.send(Err(OpError::Busy));
                    return OpHandle {
                        id,
                        cancel,
                        result: rx,
                    };
                };
                running.insert(repo, cancel.clone());
                held = Some(BackgroundGuard {
                    repo,
                    running: self.background.clone(),
                    _lock: guard,
                });
                None
            }
            (OpClass::Write | OpClass::Network, Some(repo)) => {
                if let Some(running) = lock(&self.background).get(&repo) {
                    running.cancel();
                }
                Some(self.repo_lock(repo))
            }
        };
        let pool = match spec.class {
            OpClass::Read => Some(self.reads.clone()),
            OpClass::Network => Some(self.network.clone()),
            OpClass::Write | OpClass::Background => None,
        };
        self.runtime.spawn(async move {
            let _held = held;
            // Repo lock first, then the pool permit, so no permit is held idle
            // while waiting for a busy repository.
            let _guard = match repo_lock {
                Some(l) => Some(l.lock_owned().await),
                None => None,
            };
            let _permit = match pool {
                Some(p) => p.acquire_owned().await.ok(),
                None => None,
            };
            let result = if ctx.cancel.is_cancelled() {
                Err(OpError::Cancelled)
            } else {
                ctx.emit(OpEventKind::Started);
                let joined = tokio::task::spawn_blocking(move || {
                    let r = job(&ctx);
                    (r, ctx)
                })
                .await;
                match joined {
                    Ok((r, ctx)) => {
                        ctx.emit(OpEventKind::Finished(
                            r.as_ref().map(drop).map_err(ToString::to_string),
                        ));
                        r
                    }
                    Err(e) => Err(OpError::Worker(e.to_string())),
                }
            };
            // The receiver may have been dropped: nobody waits for the result.
            let _ = tx.send(result);
        });
        OpHandle {
            id,
            cancel,
            result: rx,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;
    use std::time::Duration;

    use super::*;

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap()
    }

    fn settings(network: usize) -> Settings {
        Settings {
            network_concurrency: network,
            read_concurrency: 8,
            ..Settings::default()
        }
    }

    #[test]
    fn writes_to_one_repo_are_serialized() {
        let rt = runtime();
        let s = Scheduler::new(rt.handle().clone(), &settings(6));
        let running = Arc::new(AtomicUsize::new(0));
        let max_seen = Arc::new(AtomicUsize::new(0));
        let handles: Vec<_> = (0..6)
            .map(|_| {
                let (running, max_seen) = (running.clone(), max_seen.clone());
                s.submit(
                    OpSpec::new(Some(RepoId(1)), OpClass::Write, "w"),
                    move |_| {
                        let now = running.fetch_add(1, Ordering::SeqCst) + 1;
                        max_seen.fetch_max(now, Ordering::SeqCst);
                        std::thread::sleep(Duration::from_millis(15));
                        running.fetch_sub(1, Ordering::SeqCst);
                        Ok(())
                    },
                )
            })
            .collect();
        for h in handles {
            h.wait_blocking().unwrap();
        }
        assert_eq!(max_seen.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn network_pool_is_bounded_and_repos_run_in_parallel() {
        let rt = runtime();
        let s = Scheduler::new(rt.handle().clone(), &settings(2));
        let running = Arc::new(AtomicUsize::new(0));
        let max_seen = Arc::new(AtomicUsize::new(0));
        let handles: Vec<_> = (0..8)
            .map(|i| {
                let (running, max_seen) = (running.clone(), max_seen.clone());
                s.submit(
                    OpSpec::new(Some(RepoId(i)), OpClass::Network, "fetch"),
                    move |_| {
                        let now = running.fetch_add(1, Ordering::SeqCst) + 1;
                        max_seen.fetch_max(now, Ordering::SeqCst);
                        std::thread::sleep(Duration::from_millis(20));
                        running.fetch_sub(1, Ordering::SeqCst);
                        Ok(())
                    },
                )
            })
            .collect();
        for h in handles {
            h.wait_blocking().unwrap();
        }
        assert_eq!(max_seen.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn queued_operation_can_be_cancelled() {
        let rt = runtime();
        let s = Scheduler::new(rt.handle().clone(), &settings(6));
        let blocker = s.submit(OpSpec::new(Some(RepoId(1)), OpClass::Write, "slow"), |_| {
            std::thread::sleep(Duration::from_millis(50));
            Ok(1)
        });
        let queued = s.submit(
            OpSpec::new(Some(RepoId(1)), OpClass::Write, "queued"),
            |_| Ok(2),
        );
        queued.cancel();
        assert_eq!(blocker.wait_blocking(), Ok(1));
        assert_eq!(queued.wait_blocking(), Err(OpError::Cancelled));
    }

    #[test]
    fn background_operation_skips_a_busy_repo() {
        let rt = runtime();
        let s = Scheduler::new(rt.handle().clone(), &settings(6));
        let (started, release) = (Arc::new(std::sync::Barrier::new(2)), CancelToken::new());
        let (b, r) = (started.clone(), release.clone());
        let write = s.submit(
            OpSpec::new(Some(RepoId(1)), OpClass::Write, "w"),
            move |_| {
                b.wait();
                while !r.is_cancelled() {
                    std::thread::sleep(Duration::from_millis(1));
                }
                Ok(())
            },
        );
        started.wait();
        let bg = s.submit(
            OpSpec::new(Some(RepoId(1)), OpClass::Background, "fetch"),
            |_| Ok(()),
        );
        assert_eq!(bg.wait_blocking(), Err(OpError::Busy));
        release.cancel();
        write.wait_blocking().unwrap();
        let bg = s.submit(
            OpSpec::new(Some(RepoId(1)), OpClass::Background, "fetch"),
            |_| Ok(()),
        );
        assert_eq!(bg.wait_blocking(), Ok(()));
    }

    #[test]
    fn write_cancels_a_running_background_operation() {
        let rt = runtime();
        let s = Scheduler::new(rt.handle().clone(), &settings(6));
        let mut events = s.subscribe();
        let started = Arc::new(std::sync::Barrier::new(2));
        let b = started.clone();
        let bg = s.submit(
            OpSpec::new(Some(RepoId(1)), OpClass::Background, "fetch"),
            move |ctx| {
                b.wait();
                while !ctx.cancel().is_cancelled() {
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err::<(), _>(OpError::Cancelled)
            },
        );
        started.wait();
        let write = s.submit(
            OpSpec::new(Some(RepoId(1)), OpClass::Write, "commit"),
            |_| Ok(()),
        );
        assert_eq!(bg.wait_blocking(), Err(OpError::Cancelled));
        write.wait_blocking().unwrap();
        // Only the write shows in the operations drawer.
        while let Ok(e) = events.try_recv() {
            assert_eq!(e.label, "commit");
        }
    }

    #[test]
    fn events_and_progress_are_emitted() {
        let rt = runtime();
        let s = Scheduler::new(rt.handle().clone(), &settings(6));
        let mut events = s.subscribe();
        let h = s.submit(
            OpSpec::new(Some(RepoId(3)), OpClass::Read, "status"),
            |ctx| {
                ctx.report(Progress {
                    phase: "Counting".into(),
                    current: 1,
                    total: Some(2),
                });
                Err::<(), _>(OpError::Refused("nope".into()))
            },
        );
        assert!(h.wait_blocking().is_err());
        let mut kinds = Vec::new();
        while let Ok(e) = events.try_recv() {
            assert_eq!(e.repo, Some(RepoId(3)));
            kinds.push(e.kind);
        }
        assert_eq!(kinds[0], OpEventKind::Queued);
        assert_eq!(kinds[1], OpEventKind::Started);
        assert!(matches!(kinds[2], OpEventKind::Progress(_)));
        assert_eq!(kinds[3], OpEventKind::Finished(Err("nope".into())));
    }

    #[test]
    fn panicking_job_reports_worker_error() {
        let rt = runtime();
        let s = Scheduler::new(rt.handle().clone(), &settings(6));
        let h = s.submit(
            OpSpec::new(None, OpClass::Read, "boom"),
            |_| -> Result<(), OpError> { panic!("boom") },
        );
        assert!(matches!(h.wait_blocking(), Err(OpError::Worker(_))));
    }
}
