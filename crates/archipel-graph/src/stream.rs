//! Background topology builds: the walk and the cache file are built on a
//! worker thread while the first rows are laid out as soon as they are walked,
//! so the first screen shows before the whole history is read (PLAN §4.1).
//!
//! The lane layout only compares lane targets for equality, and a row's layout
//! only depends on the rows above it. While streaming, a parent not walked yet
//! gets a provisional id (above [`MAX_ROWS`], so it never collides with a row);
//! when that parent is walked, the lanes heading to it are renamed to its row.
//! The streamed rows are therefore laid out exactly as a replay from the
//! finished file would lay them out, except for parents that are never walked
//! (shallow history): they show as continuing lanes until the build completes.

use std::collections::HashMap;
use std::ops::Range;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError};

use archipel_model::history::{TopologyNode, WalkOrder};
use archipel_model::ports::HistoryReader;
use archipel_model::{CancelToken, GitError, Oid, Progress, ProgressSink, RepoPath};

use crate::error::GraphError;
use crate::layout::{LaneState, ParentLink, RowLayout};
use crate::topology::{self, BuildSpec, MAX_ROWS, TopologyFile};

/// Streamed rows between two wake-ups of the threads waiting for them.
const NOTIFY_EVERY_ROWS: u32 = 64;

/// A finished topology: the cache file and its lane checkpoints.
#[derive(Debug)]
pub(crate) struct Ready {
    pub(crate) file: Arc<TopologyFile>,
    pub(crate) checkpoints: Vec<LaneState>,
    pub(crate) max_width: u32,
}

/// One row laid out while the walk runs.
#[derive(Clone, Debug)]
pub(crate) struct StreamedRow {
    pub(crate) oid: Oid,
    pub(crate) layout: RowLayout,
}

/// Lays out rows in walk order without knowing parent rows in advance.
#[derive(Default)]
struct StreamLayout {
    state: LaneState,
    /// Provisional ids of the parents not walked yet.
    pending: HashMap<Oid, u32>,
    next_provisional: u32,
}

impl StreamLayout {
    fn push(&mut self, row: u32, node: &TopologyNode) -> RowLayout {
        if let Some(provisional) = self.pending.remove(&node.oid) {
            self.state.retarget(provisional, row);
        }
        let mut links: Vec<ParentLink> = Vec::with_capacity(node.parents.len());
        for p in &node.parents {
            let next = &mut self.next_provisional;
            let id = *self.pending.entry(*p).or_insert_with(|| {
                let id = MAX_ROWS.saturating_add(*next);
                *next = next.saturating_add(1);
                id
            });
            let link = ParentLink::Row(id);
            if !links.contains(&link) {
                links.push(link);
            }
        }
        self.state.step(row, &links)
    }
}

/// Mutable part of a [`Stream`], behind its mutex.
#[derive(Default)]
struct State {
    /// The first rows, laid out (emptied once the topology is ready).
    head: Vec<StreamedRow>,
    head_width: u32,
    /// Latest progress of the worker, relayed by waiting threads.
    progress: Option<Progress>,
    progress_seq: u64,
    finished: bool,
    failure: Option<GraphError>,
}

/// A topology being built on a worker thread.
pub(crate) struct Stream {
    state: Mutex<State>,
    changed: Condvar,
    ready: OnceLock<Ready>,
    /// Stops the worker (view unloaded, or the caller's token cancelled).
    abort: CancelToken,
    head_rows: u32,
}

impl std::fmt::Debug for Stream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Stream")
            .field("ready", &self.ready.get().is_some())
            .field("head_rows", &self.head_rows)
            .finish_non_exhaustive()
    }
}

/// Everything the worker thread needs.
pub(crate) struct BuildJob {
    pub(crate) path: PathBuf,
    pub(crate) history: Arc<dyn HistoryReader>,
    pub(crate) repo: RepoPath,
    pub(crate) order: WalkOrder,
    pub(crate) key: u64,
    pub(crate) checkpoint_interval: u32,
    pub(crate) head_rows: u32,
    pub(crate) cancel: CancelToken,
}

/// Relays the worker's progress to the threads waiting on the stream.
struct Relay<'a>(&'a Stream);

impl ProgressSink for Relay<'_> {
    fn report(&self, progress: Progress) {
        let mut state = self.0.lock();
        state.progress = Some(progress);
        state.progress_seq += 1;
        drop(state);
        self.0.changed.notify_all();
    }
}

impl Stream {
    /// Starts building on a worker thread.
    pub(crate) fn spawn(job: BuildJob) -> Result<Arc<Self>, GraphError> {
        let stream = Arc::new(Self {
            state: Mutex::new(State::default()),
            changed: Condvar::new(),
            ready: OnceLock::new(),
            abort: CancelToken::new(),
            head_rows: job.head_rows,
        });
        if job.cancel.is_cancelled() {
            stream.abort.cancel();
        }
        let worker = Arc::clone(&stream);
        let path = job.path.clone();
        std::thread::Builder::new()
            .name("archipel-graph-build".into())
            .spawn(move || worker.run(&job))
            .map_err(|e| GraphError::io(&path, e))?;
        Ok(stream)
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn run(&self, job: &BuildJob) {
        let result = self.build(job);
        let mut state = self.lock();
        match result {
            Ok(ready) => {
                // Set before `finished`, so a finished stream always has one or the other.
                let _ = self.ready.set(ready);
                state.head = Vec::new();
            }
            Err(e) => state.failure = Some(e),
        }
        state.finished = true;
        drop(state);
        self.changed.notify_all();
    }

    fn build(&self, job: &BuildJob) -> Result<Ready, GraphError> {
        let relay = Relay(self);
        let mut layout = StreamLayout::default();
        let spec = BuildSpec {
            path: &job.path,
            history: job.history.as_ref(),
            repo: &job.repo,
            order: job.order,
            key: job.key,
        };
        topology::build(&spec, &relay, &self.abort, &mut |row, node| {
            if job.cancel.is_cancelled() {
                self.abort.cancel();
            }
            if row >= self.head_rows {
                return;
            }
            let laid = layout.push(row, node);
            let mut state = self.lock();
            state.head_width = state.head_width.max(laid.width);
            state.head.push(StreamedRow {
                oid: node.oid,
                layout: laid,
            });
            drop(state);
            if row + 1 == self.head_rows || row % NOTIFY_EVERY_ROWS == 0 {
                self.changed.notify_all();
            }
        })?;
        // The streamed rows are all known: wake the opener before the
        // checkpoint pass when the history is shorter than the first window.
        self.changed.notify_all();
        let file = match TopologyFile::open(&job.path, job.key)? {
            Ok(f) => f,
            Err(why) => {
                return Err(GraphError::corrupt(
                    &job.path,
                    format!("freshly built cache unusable: {why:?}"),
                ));
            }
        };
        let (checkpoints, max_width) =
            topology::checkpoints(&file, job.checkpoint_interval, &relay, &self.abort)?;
        Ok(Ready {
            file: Arc::new(file),
            checkpoints,
            max_width,
        })
    }

    /// The finished topology, if the build is done.
    pub(crate) fn ready(&self) -> Option<&Ready> {
        self.ready.get()
    }

    /// Stops the worker unless it already finished.
    pub(crate) fn abandon(&self) {
        if self.ready.get().is_none() {
            self.abort.cancel();
        }
    }

    /// Blocks until `done` holds or the build finished, relaying the worker's
    /// progress to `progress` meanwhile. Returns the build failure, if any.
    fn wait_until(
        &self,
        progress: &dyn ProgressSink,
        done: impl Fn(&State) -> bool,
    ) -> Result<(), GraphError> {
        let mut seen = 0u64;
        let mut state = self.lock();
        loop {
            if state.progress_seq != seen {
                seen = state.progress_seq;
                if let Some(p) = state.progress.clone() {
                    drop(state);
                    progress.report(p);
                    state = self.lock();
                    continue;
                }
            }
            if let Some(e) = &state.failure {
                return Err(e.duplicate());
            }
            if state.finished || done(&state) {
                return Ok(());
            }
            state = self
                .changed
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    /// Blocks until the first window is laid out (or the build finished).
    pub(crate) fn wait_first_window(&self, progress: &dyn ProgressSink) -> Result<(), GraphError> {
        let head_rows = usize::try_from(self.head_rows).unwrap_or(usize::MAX);
        self.wait_until(progress, |s| s.head.len() >= head_rows)
    }

    /// Blocks until the topology is ready.
    pub(crate) fn wait_ready(&self, progress: &dyn ProgressSink) -> Result<&Ready, GraphError> {
        self.wait_until(progress, |_| false)?;
        self.ready.get().ok_or(GraphError::Git(GitError::Cancelled))
    }

    /// Rows available while building (the streamed ones).
    pub(crate) fn head_len(&self) -> u32 {
        u32::try_from(self.lock().head.len()).unwrap_or(u32::MAX)
    }

    /// Widest streamed row.
    pub(crate) fn head_width(&self) -> u32 {
        self.lock().head_width
    }

    /// Bytes held by the streamed rows.
    pub(crate) fn head_bytes(&self) -> usize {
        let state = self.lock();
        state.head.capacity() * std::mem::size_of::<StreamedRow>()
            + state
                .head
                .iter()
                .map(|r| r.layout.edges.capacity() * std::mem::size_of::<crate::Edge>())
                .sum::<usize>()
    }

    /// Streamed rows of `range` (clamped), or `None` once the topology is
    /// ready (the streamed rows are dropped then: read the file instead).
    pub(crate) fn head(&self, range: Range<u32>) -> Option<Vec<(u32, StreamedRow)>> {
        let state = self.lock();
        if self.ready.get().is_some() {
            return None;
        }
        let len = u32::try_from(state.head.len()).unwrap_or(u32::MAX);
        let end = range.end.min(len);
        let start = range.start.min(end);
        Some(
            (start..end)
                .zip(
                    &state.head
                        [usize::try_from(start).unwrap_or(0)..usize::try_from(end).unwrap_or(0)],
                )
                .map(|(row, r)| (row, r.clone()))
                .collect(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn oid(n: u8) -> Oid {
        Oid::from_bytes(&[n; 20]).unwrap_or_else(|| unreachable!())
    }

    /// Streaming with provisional ids lays out like the row-indexed layout.
    #[test]
    fn provisional_ids_match_row_layout() {
        // 0: merge (1, 2); 1: (3); 2: (3); 3: merge (4, 5); 4: (6); 5: (6); 6: root
        // plus a second tip at row 2 sharing parent 3, and an octopus at 3.
        let graph: &[&[u32]] = &[&[1, 2], &[3], &[3, 4], &[4, 5, 6], &[6], &[6], &[]];
        let nodes: Vec<TopologyNode> = graph
            .iter()
            .zip(0u8..)
            .map(|(ps, i)| TopologyNode {
                oid: oid(i),
                parents: ps
                    .iter()
                    .map(|p| oid(u8::try_from(*p).unwrap_or(0)))
                    .collect(),
            })
            .collect();
        let mut streamed = StreamLayout::default();
        let mut reference = LaneState::new();
        for (row, (node, ps)) in (0u32..).zip(nodes.iter().zip(graph)) {
            let links: Vec<ParentLink> = ps.iter().map(|p| ParentLink::Row(*p)).collect();
            assert_eq!(
                streamed.push(row, node),
                reference.step(row, &links),
                "row {row}"
            );
        }
        assert!(streamed.pending.is_empty());
    }
}
