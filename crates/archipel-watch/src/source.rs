//! [`NotifyChangeSource`]: the [`ChangeSource`] backed by `notify`.
//!
//! One OS watcher is shared by every watched repository (one inotify
//! instance on Linux, one I/O thread on Windows) and one dispatcher thread
//! classifies events, feeds the [`Batcher`] and calls the sinks. The notify
//! callback only filters out access events and forwards paths over a channel:
//! it never blocks.
//!
//! The set of OS watches is *reconciled* from every repository's wanted paths:
//! a path already covered by a recursive watch of an ancestor is not watched
//! twice (e.g. a linked worktree's `refs/` inside the main worktree), so two
//! repositories never share an OS watch whose removal would blind the other.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Instant;

use archipel_model::ports::{
    ChangeSource, Coverage, Invalidation, InvalidationSink, Scope, WatchHandle,
};
use archipel_model::settings::Settings;
use archipel_model::{RepoId, RepoPath, WatchError};
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};

use crate::classify::{RepoClassifier, global_excludes};
use crate::debounce::{Batcher, DebounceConfig, Debouncer};
use crate::layout::{Depth, RepoLayout, WatchPath};

/// Name of the dispatcher thread.
const DISPATCHER_THREAD: &str = "archipel-watch";

/// A batch of changed paths, already stripped of notify types.
#[derive(Clone, Debug, Default)]
pub(crate) struct RawEvent {
    /// Changed paths (absolute, as built by the OS watcher).
    pub paths: Vec<PathBuf>,
    /// Events were lost: everything under `paths` (or everything when
    /// `paths` is empty) must be considered stale.
    pub rescan: bool,
}

/// The OS watcher, behind a seam so reconciliation can be tested.
pub(crate) trait Backend: Send {
    /// Adds a watch. On failure, nothing of it stays registered.
    fn watch(&mut self, path: &WatchPath) -> Result<(), WatchError>;
    /// Removes a watch (errors are logged, not returned).
    fn unwatch(&mut self, path: &Path);
}

struct NotifyBackend(RecommendedWatcher);

impl Backend for NotifyBackend {
    fn watch(&mut self, path: &WatchPath) -> Result<(), WatchError> {
        let mode = match path.depth {
            Depth::Shallow => RecursiveMode::NonRecursive,
            Depth::Recursive => RecursiveMode::Recursive,
        };
        self.0.watch(&path.path, mode).map_err(|err| {
            // inotify adds recursive watches one directory at a time: drop
            // the ones added before the failure.
            let _ = self.0.unwatch(&path.path);
            to_watch_error(&path.path, &err)
        })
    }

    fn unwatch(&mut self, path: &Path) {
        if let Err(err) = self.0.unwatch(path) {
            tracing::debug!(%err, path = %path.display(), "unwatch failed");
        }
    }
}

fn to_watch_error(path: &Path, err: &notify::Error) -> WatchError {
    let limit = match &err.kind {
        notify::ErrorKind::MaxFilesWatch => true,
        notify::ErrorKind::Io(io) => io.kind() == std::io::ErrorKind::StorageFull,
        _ => false,
    };
    if limit {
        WatchError::LimitReached {
            path: path.to_path_buf(),
            message: err.to_string(),
        }
    } else {
        WatchError::Backend(format!("{}: {err}", path.display()))
    }
}

/// Converts a notify callback argument into a [`RawEvent`] (`None` = drop).
fn raw_event(res: notify::Result<notify::Event>) -> Option<RawEvent> {
    match res {
        // Reads (including our own Git reads) must not invalidate anything.
        Ok(ev) if matches!(ev.kind, EventKind::Access(_)) => None,
        Ok(ev) => Some(RawEvent {
            rescan: ev.need_rescan(),
            paths: ev.paths,
        }),
        Err(err) => {
            tracing::warn!(%err, "file watcher error");
            Some(RawEvent {
                paths: err.paths,
                rescan: true,
            })
        }
    }
}

type RegId = u64;

/// One registered repository, as seen by the dispatcher.
struct Route {
    id: RegId,
    repo: RepoId,
    classifier: RepoClassifier,
    sink: Arc<dyn InvalidationSink>,
}

impl Route {
    fn owns(&self, path: &Path) -> bool {
        let l = self.classifier.layout();
        path.starts_with(&l.git_dir)
            || path.starts_with(&l.common_dir)
            || l.worktree.as_ref().is_some_and(|w| path.starts_with(w))
    }
}

/// Dispatcher state: routes and the debouncing policy.
struct Routes {
    routes: Vec<Route>,
    batcher: Box<dyn Batcher>,
}

impl Routes {
    fn handle(&mut self, event: &RawEvent, now: Instant) {
        let Self { routes, batcher } = self;
        for route in routes.iter_mut() {
            let mut scope = Scope::NONE;
            if event.rescan && (event.paths.is_empty() || event.paths.iter().any(|p| route.owns(p)))
            {
                scope = Scope::ALL;
            } else {
                for path in &event.paths {
                    scope |= route.classifier.classify(path);
                }
            }
            if !scope.is_empty() {
                batcher.push(
                    Invalidation {
                        repo: route.repo,
                        scope,
                    },
                    now,
                );
            }
        }
    }

    fn take_due(&mut self, now: Instant) -> Vec<(Arc<dyn InvalidationSink>, Invalidation)> {
        let due = self.batcher.take_due(now);
        let mut out = Vec::new();
        for inv in due {
            for route in self.routes.iter().filter(|r| r.repo == inv.repo) {
                out.push((Arc::clone(&route.sink), inv));
            }
        }
        out
    }
}

/// OS watch bookkeeping.
struct Control {
    backend: Box<dyn Backend>,
    /// Wanted watches per registration.
    plans: Vec<(RegId, Vec<WatchPath>)>,
    /// Watches currently registered with the OS.
    active: HashSet<WatchPath>,
}

impl Control {
    /// Brings the OS watches in line with the plans; returns failed adds.
    fn reconcile(&mut self) -> Vec<(WatchPath, WatchError)> {
        let wanted: HashSet<&WatchPath> = self.plans.iter().flat_map(|(_, p)| p).collect();
        let effective: HashSet<WatchPath> = wanted
            .iter()
            .filter(|w| !wanted.iter().any(|o| o != *w && o.covers(w)))
            .map(|w| (*w).clone())
            .collect();
        // Remove first: on inotify, removing a recursive watch also removes
        // the watches of its subdirectories, which may be re-added below.
        let stale: Vec<WatchPath> = self.active.difference(&effective).cloned().collect();
        for w in stale {
            self.backend.unwatch(&w.path);
            self.active.remove(&w);
        }
        let mut failures = Vec::new();
        let mut missing: Vec<WatchPath> = effective.difference(&self.active).cloned().collect();
        missing.sort_by(|a, b| a.path.cmp(&b.path));
        for w in missing {
            match self.backend.watch(&w) {
                Ok(()) => {
                    self.active.insert(w);
                }
                Err(err) => failures.push((w, err)),
            }
        }
        failures
    }

    fn set_plan(&mut self, id: RegId, plan: Vec<WatchPath>) {
        self.plans.retain(|(i, _)| *i != id);
        self.plans.push((id, plan));
    }

    fn remove_plan(&mut self, id: RegId) {
        self.plans.retain(|(i, _)| *i != id);
    }
}

struct Shared {
    control: Mutex<Control>,
    routes: Arc<Mutex<Routes>>,
    next_id: AtomicU64,
    /// Global excludes, read once (parsing the user's Git config is not free).
    global_excludes: ignore::gitignore::Gitignore,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Stops a repository's watch when dropped.
struct WatchGuard {
    shared: Arc<Shared>,
    id: RegId,
    repo: RepoId,
}

impl Drop for WatchGuard {
    fn drop(&mut self) {
        {
            let mut routes = lock(&self.shared.routes);
            routes.routes.retain(|r| r.id != self.id);
            if !routes.routes.iter().any(|r| r.repo == self.repo) {
                routes.batcher.forget(self.repo);
            }
        }
        let mut control = lock(&self.shared.control);
        control.remove_plan(self.id);
        for (w, err) in control.reconcile() {
            tracing::warn!(%err, path = %w.path.display(), "cannot restore watch");
        }
    }
}

/// [`ChangeSource`] backed by the platform's recommended `notify` watcher.
///
/// Per repository it watches the worktree recursively plus the Git
/// directories that lie outside it (linked worktrees). When the recursive
/// worktree watch cannot be added (e.g. inotify limit), it falls back to the
/// Git directories only and reports [`Coverage::GitDirOnly`].
pub struct NotifyChangeSource {
    shared: Arc<Shared>,
}

impl std::fmt::Debug for NotifyChangeSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NotifyChangeSource").finish_non_exhaustive()
    }
}

impl NotifyChangeSource {
    /// Creates the source with the default [`Debouncer`] policy.
    pub fn new(config: DebounceConfig) -> Result<Self, WatchError> {
        Self::with_batcher(Box::new(Debouncer::new(config)))
    }

    /// Creates the source with the debounce delay from `settings`.
    pub fn from_settings(settings: &Settings) -> Result<Self, WatchError> {
        Self::new(DebounceConfig::from_settings(settings))
    }

    /// Creates the source with a custom batching policy.
    pub fn with_batcher(batcher: Box<dyn Batcher>) -> Result<Self, WatchError> {
        let (tx, rx) = mpsc::channel();
        let watcher = notify::recommended_watcher(move |res| {
            if let Some(ev) = raw_event(res) {
                // The receiver only disappears when the source is gone.
                let _ = tx.send(ev);
            }
        })
        .map_err(|e| to_watch_error(Path::new(""), &e))?;
        Self::build(Box::new(NotifyBackend(watcher)), rx, batcher)
    }

    pub(crate) fn build(
        backend: Box<dyn Backend>,
        rx: Receiver<RawEvent>,
        batcher: Box<dyn Batcher>,
    ) -> Result<Self, WatchError> {
        let routes = Arc::new(Mutex::new(Routes {
            routes: Vec::new(),
            batcher,
        }));
        let dispatcher_routes = Arc::clone(&routes);
        std::thread::Builder::new()
            .name(DISPATCHER_THREAD.to_owned())
            .spawn(move || dispatch(&rx, &dispatcher_routes))
            .map_err(|e| WatchError::Backend(format!("cannot start watcher thread: {e}")))?;
        Ok(Self {
            shared: Arc::new(Shared {
                control: Mutex::new(Control {
                    backend,
                    plans: Vec::new(),
                    active: HashSet::new(),
                }),
                routes,
                next_id: AtomicU64::new(0),
                global_excludes: global_excludes(),
            }),
        })
    }

    /// Registers the OS watches for `layout`; returns the coverage obtained.
    fn add_watches(&self, id: RegId, layout: &RepoLayout) -> Result<Coverage, WatchError> {
        let mut control = lock(&self.shared.control);
        let Some(worktree) = layout.worktree_watch() else {
            control.set_plan(id, layout.git_dir_watches());
            return Self::finish(&mut control, id, Coverage::Full);
        };
        let mut plan = vec![worktree.clone()];
        plan.extend(layout.git_dir_watches_outside_worktree());
        control.set_plan(id, plan);
        let failures = control.reconcile();
        match failures.into_iter().find(|(w, _)| *w == worktree) {
            None => Self::finish(&mut control, id, Coverage::Full),
            Some((_, err)) => {
                tracing::warn!(
                    %err,
                    worktree = %worktree.path.display(),
                    "cannot watch the worktree; watching the Git directory only \
                     (worktree changes are picked up on window focus)"
                );
                control.set_plan(id, layout.git_dir_watches());
                Self::finish(&mut control, id, Coverage::GitDirOnly)
            }
        }
    }

    /// Reconciles; fails (and unregisters) if one of `id`'s watches failed.
    fn finish(
        control: &mut Control,
        id: RegId,
        coverage: Coverage,
    ) -> Result<Coverage, WatchError> {
        let failures = control.reconcile();
        let plan: Vec<WatchPath> = control
            .plans
            .iter()
            .filter(|(i, _)| *i == id)
            .flat_map(|(_, p)| p.iter().cloned())
            .collect();
        if let Some((_, err)) = failures.into_iter().find(|(w, _)| plan.contains(w)) {
            control.remove_plan(id);
            for (w, e) in control.reconcile() {
                tracing::warn!(err = %e, path = %w.path.display(), "cannot restore watch");
            }
            return Err(err);
        }
        Ok(coverage)
    }
}

impl ChangeSource for NotifyChangeSource {
    fn watch(
        &self,
        repo: RepoId,
        path: &RepoPath,
        sink: Arc<dyn InvalidationSink>,
    ) -> Result<WatchHandle, WatchError> {
        let layout = RepoLayout::locate(path.as_ref())?;
        let id = self.shared.next_id.fetch_add(1, Ordering::Relaxed);
        let coverage = self.add_watches(id, &layout)?;
        lock(&self.shared.routes).routes.push(Route {
            id,
            repo,
            classifier: RepoClassifier::with_global(layout, self.shared.global_excludes.clone()),
            sink,
        });
        Ok(WatchHandle {
            coverage,
            guard: Box::new(WatchGuard {
                shared: Arc::clone(&self.shared),
                id,
                repo,
            }),
        })
    }
}

/// Dispatcher loop: runs until every event sender is gone.
fn dispatch(rx: &Receiver<RawEvent>, routes: &Mutex<Routes>) {
    loop {
        let deadline = lock(routes).batcher.next_deadline();
        let received = match deadline {
            Some(d) => rx.recv_timeout(d.saturating_duration_since(Instant::now())),
            None => rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
        };
        match received {
            Ok(event) => {
                let mut r = lock(routes);
                let now = Instant::now();
                r.handle(&event, now);
                // Drain what is already queued under the same lock.
                while let Ok(event) = rx.try_recv() {
                    r.handle(&event, now);
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
        // Deliver outside the lock: a sink may drop a watch handle.
        let due = lock(routes).take_due(Instant::now());
        for (sink, inv) in due {
            sink.invalidate(inv);
        }
    }
}

/// Sender type used by tests to inject events.
#[cfg(test)]
pub(crate) type EventSender = mpsc::Sender<RawEvent>;

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// Records calls; fails watches of the configured paths.
    #[derive(Clone, Default)]
    struct FakeBackend {
        log: Arc<Mutex<Vec<String>>>,
        fail: Arc<Mutex<HashSet<PathBuf>>>,
    }

    impl Backend for FakeBackend {
        fn watch(&mut self, path: &WatchPath) -> Result<(), WatchError> {
            if lock(&self.fail).contains(&path.path) {
                return Err(WatchError::LimitReached {
                    path: path.path.clone(),
                    message: "limit".into(),
                });
            }
            lock(&self.log).push(format!("+{:?} {}", path.depth, path.path.display()));
            Ok(())
        }
        fn unwatch(&mut self, path: &Path) {
            lock(&self.log).push(format!("-{}", path.display()));
        }
    }

    /// Delivers immediately (no debouncing) to keep tests fast.
    struct Immediate(Vec<Invalidation>);

    impl Batcher for Immediate {
        fn push(&mut self, invalidation: Invalidation, _now: Instant) {
            self.0.push(invalidation);
        }
        fn take_due(&mut self, _now: Instant) -> Vec<Invalidation> {
            std::mem::take(&mut self.0)
        }
        fn next_deadline(&self) -> Option<Instant> {
            None
        }
        fn forget(&mut self, repo: RepoId) {
            self.0.retain(|i| i.repo != repo);
        }
    }

    fn source(backend: &FakeBackend) -> (NotifyChangeSource, EventSender) {
        let (tx, rx) = mpsc::channel();
        let src = NotifyChangeSource::build(
            Box::new(backend.clone()),
            rx,
            Box::new(Immediate(Vec::new())),
        )
        .unwrap();
        (src, tx)
    }

    fn collector() -> (Arc<dyn InvalidationSink>, Receiver<Invalidation>) {
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        let sink = move |inv: Invalidation| {
            let _ = lock(&tx).send(inv);
        };
        (Arc::new(sink), rx)
    }

    fn repo_dir(root: &Path, name: &str) -> PathBuf {
        let p = root.join(name);
        std::fs::create_dir_all(p.join(".git/refs/heads")).unwrap();
        std::fs::create_dir_all(p.join(".git/objects")).unwrap();
        std::fs::write(p.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        std::fs::canonicalize(p).unwrap()
    }

    #[test]
    fn full_coverage_watches_only_the_worktree_of_a_regular_repo() {
        let tmp = tempfile::tempdir().unwrap();
        let a = repo_dir(tmp.path(), "a");
        let fake = FakeBackend::default();
        let (src, _tx) = source(&fake);
        let (sink, _rx) = collector();
        let h = src.watch(RepoId(1), &RepoPath::new(&a), sink).unwrap();
        assert_eq!(h.coverage, Coverage::Full);
        assert_eq!(
            *lock(&fake.log),
            vec![format!("+Recursive {}", a.display())]
        );
        drop(h);
        assert_eq!(
            lock(&fake.log).last().unwrap(),
            &format!("-{}", a.display())
        );
    }

    #[test]
    fn degrades_to_git_dir_when_worktree_watch_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let a = repo_dir(tmp.path(), "a");
        let fake = FakeBackend::default();
        lock(&fake.fail).insert(a.clone());
        let (src, _tx) = source(&fake);
        let (sink, _rx) = collector();
        let h = src.watch(RepoId(1), &RepoPath::new(&a), sink).unwrap();
        assert_eq!(h.coverage, Coverage::GitDirOnly);
        let log = lock(&fake.log).clone();
        assert_eq!(
            log,
            vec![
                format!("+Shallow {}", a.join(".git").display()),
                format!("+Recursive {}", a.join(".git").join("refs").display()),
            ]
        );
    }

    #[test]
    fn fails_when_git_dir_cannot_be_watched() {
        let tmp = tempfile::tempdir().unwrap();
        let a = repo_dir(tmp.path(), "a");
        let fake = FakeBackend::default();
        lock(&fake.fail).insert(a.clone());
        lock(&fake.fail).insert(a.join(".git"));
        let (src, _tx) = source(&fake);
        let (sink, _rx) = collector();
        let err = src.watch(RepoId(1), &RepoPath::new(&a), sink).unwrap_err();
        assert!(matches!(err, WatchError::LimitReached { .. }));
        assert!(lock(&src.shared.control).active.is_empty());
    }

    #[test]
    fn nested_watches_are_not_duplicated_and_are_restored() {
        let tmp = tempfile::tempdir().unwrap();
        let outer = repo_dir(tmp.path(), "outer");
        let inner = repo_dir(&outer, "inner");
        let fake = FakeBackend::default();
        let (src, _tx) = source(&fake);
        let (sink, _rx) = collector();
        let hi = src
            .watch(RepoId(2), &RepoPath::new(&inner), Arc::clone(&sink))
            .unwrap();
        let ho = src.watch(RepoId(1), &RepoPath::new(&outer), sink).unwrap();
        // The outer recursive watch replaces the inner one.
        assert_eq!(
            lock(&src.shared.control).active,
            HashSet::from([WatchPath {
                path: outer.clone(),
                depth: Depth::Recursive
            }])
        );
        drop(ho);
        assert_eq!(
            lock(&src.shared.control).active,
            HashSet::from([WatchPath {
                path: inner.clone(),
                depth: Depth::Recursive
            }])
        );
        drop(hi);
        assert!(lock(&src.shared.control).active.is_empty());
    }

    #[test]
    fn events_are_routed_to_the_owning_repo_and_stop_after_drop() {
        let tmp = tempfile::tempdir().unwrap();
        let a = repo_dir(tmp.path(), "a");
        let b = repo_dir(tmp.path(), "b");
        let fake = FakeBackend::default();
        let (src, tx) = source(&fake);
        let (sink, rx) = collector();
        let ha = src
            .watch(RepoId(1), &RepoPath::new(&a), Arc::clone(&sink))
            .unwrap();
        let hb = src.watch(RepoId(2), &RepoPath::new(&b), sink).unwrap();
        let wait = Duration::from_secs(5);

        tx.send(RawEvent {
            paths: vec![a.join("file.txt"), b.join(".git").join("refs/stash")],
            rescan: false,
        })
        .unwrap();
        let mut got = [
            rx.recv_timeout(wait).unwrap(),
            rx.recv_timeout(wait).unwrap(),
        ];
        got.sort_by_key(|i| i.repo.0);
        assert_eq!(
            got,
            [
                Invalidation {
                    repo: RepoId(1),
                    scope: Scope::STATUS
                },
                Invalidation {
                    repo: RepoId(2),
                    scope: Scope::STASH
                }
            ]
        );

        tx.send(RawEvent {
            paths: vec![],
            rescan: true,
        })
        .unwrap();
        let mut got = [
            rx.recv_timeout(wait).unwrap(),
            rx.recv_timeout(wait).unwrap(),
        ];
        got.sort_by_key(|i| i.repo.0);
        assert!(got.iter().all(|i| i.scope == Scope::ALL));

        drop(ha);
        tx.send(RawEvent {
            paths: vec![a.join("file.txt"), b.join("file.txt")],
            rescan: false,
        })
        .unwrap();
        assert_eq!(rx.recv_timeout(wait).unwrap().repo, RepoId(2));
        assert!(rx.recv_timeout(Duration::from_millis(100)).is_err());
        drop(hb);
    }

    #[test]
    fn access_events_are_dropped() {
        let read = notify::Event::new(EventKind::Access(notify::event::AccessKind::Any))
            .add_path(PathBuf::from("/x"));
        assert!(raw_event(Ok(read)).is_none());
        let write = notify::Event::new(EventKind::Modify(notify::event::ModifyKind::Any))
            .add_path(PathBuf::from("/x"));
        let ev = raw_event(Ok(write)).unwrap();
        assert_eq!(ev.paths, vec![PathBuf::from("/x")]);
        assert!(!ev.rescan);
    }
}
