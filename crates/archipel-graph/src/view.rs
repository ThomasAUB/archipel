//! The paged history view: opens a repository's topology cache, lays out any
//! window of rows from the nearest checkpoint, and loads commit metadata for
//! the visible window through a byte-budgeted LRU.

use std::collections::HashMap;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use archipel_cache::{CacheRegistry, SharedLru};
use archipel_model::history::{CommitMeta, WalkOrder};
use archipel_model::ports::{HistoryReader, RefReader};
use archipel_model::refs::{RefKind, RefTip};
use archipel_model::settings::Settings;
use archipel_model::{CancelToken, Oid, ProgressSink, RepoPath};

use crate::error::GraphError;
use crate::layout::{Edge, LaneState, RowLayout};
use crate::search::{SearchQuery, SearchResult, Searcher};
use crate::stream::{BuildJob, Ready, Stream};
use crate::topology::{self, TopologyFile, Unusable};

/// Name prefix of the commit metadata caches in the [`CacheRegistry`].
const META_CACHE_NAME: &str = "commit-meta";
/// Bookkeeping bytes per metadata cache entry on top of
/// [`CommitMeta::approx_size`]: the key, the `Arc` header and LRU links.
const META_ENTRY_OVERHEAD: usize =
    2 * std::mem::size_of::<Oid>() + 8 * std::mem::size_of::<usize>();
/// File extension of topology cache files.
const CACHE_EXTENSION: &str = "topo";

/// Tunables of a [`HistoryView`]; every default comes from [`Settings`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GraphOptions {
    /// Rows between two lane-layout checkpoints (minimum 1).
    pub checkpoint_interval: usize,
    /// Rows of metadata loaded above and below the requested window.
    pub overscan: usize,
    /// Byte budget of the commit metadata LRU.
    pub meta_cache_bytes: usize,
    /// Order of the topology walk.
    pub walk_order: WalkOrder,
    /// Rows whose metadata is loaded per search step.
    pub search_chunk_rows: usize,
    /// When the topology has to be built, rows laid out while the walk runs:
    /// `open` returns as soon as they are ready (or the walk ended), and they
    /// are all the view serves until the build completes.
    pub stream_rows: usize,
}

impl GraphOptions {
    /// Default rows per search step: large enough to amortize backend calls,
    /// small enough to react quickly to cancellation.
    pub const DEFAULT_SEARCH_CHUNK_ROWS: usize = 2048;
    /// Default rows streamed before the build completes: a few screens, so the
    /// first frame shows early and the user can start scrolling.
    pub const DEFAULT_STREAM_ROWS: usize = 1024;

    /// Options taken from the application settings.
    #[must_use]
    pub fn from_settings(settings: &Settings) -> Self {
        Self {
            checkpoint_interval: settings.graph_checkpoint_interval,
            overscan: settings.graph_overscan,
            meta_cache_bytes: settings.commit_cache_bytes,
            walk_order: WalkOrder::Topological,
            search_chunk_rows: Self::DEFAULT_SEARCH_CHUNK_ROWS,
            stream_rows: Self::DEFAULT_STREAM_ROWS,
        }
    }

    fn interval(&self) -> u32 {
        u32::try_from(self.checkpoint_interval.max(1)).unwrap_or(u32::MAX)
    }
}

impl Default for GraphOptions {
    fn default() -> Self {
        Self::from_settings(&Settings::default())
    }
}

/// One laid-out row of the graph.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GraphRow {
    /// Row index (0 = newest in display order).
    pub row: u32,
    /// Commit id.
    pub oid: Oid,
    /// Column of the commit node.
    pub column: u32,
    /// Color index of the commit's lane (mapped modulo the theme palette).
    pub color: u32,
    /// Segments to draw in this row (see [`crate::EdgeKind`]).
    pub edges: Vec<Edge>,
    /// Columns used by this row.
    pub width: u32,
    /// Whether some parents are outside the walk (shallow history).
    pub hidden_parents: bool,
    /// Refs pointing at this commit, `HEAD` first, then branches, remotes, tags.
    pub refs: Vec<RefTip>,
}

/// Whether the topology cache was reused or rebuilt, and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CacheStatus {
    /// The file on disk matched the current refs.
    Reused,
    /// No cache file existed.
    BuiltMissing,
    /// The refs moved since the file was written.
    BuiltStale,
    /// The file was damaged (reason given).
    BuiltCorrupt(String),
}

/// Result of [`HistoryView::open`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenReport {
    /// Rows available now: all of them when `complete`, otherwise the rows
    /// streamed so far (see [`HistoryView::completion`]).
    pub rows: u32,
    /// What happened to the cache file.
    pub cache: CacheStatus,
    /// Whether the whole topology is loaded.
    pub complete: bool,
}

/// Size of a complete topology.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GraphSize {
    /// Number of rows.
    pub rows: u32,
    /// Widest row, in columns.
    pub max_width: u32,
}

/// Waits for the topology of an open view to be complete, without holding
/// the view (it can be locked by other readers meanwhile).
#[derive(Clone, Debug)]
pub struct Completion(CompletionInner);

#[derive(Clone, Debug)]
enum CompletionInner {
    Done(GraphSize),
    Building(Arc<Stream>),
}

impl Completion {
    /// Blocks until the topology is complete, relaying the build progress.
    /// Fails with the build error, or with [`GitError::Cancelled`] when the
    /// view was unloaded first.
    ///
    /// [`GitError::Cancelled`]: archipel_model::GitError::Cancelled
    pub fn wait(&self, progress: &dyn ProgressSink) -> Result<GraphSize, GraphError> {
        match &self.0 {
            CompletionInner::Done(size) => Ok(*size),
            CompletionInner::Building(stream) => {
                let ready = stream.wait_ready(progress)?;
                Ok(GraphSize {
                    rows: ready.file.rows(),
                    max_width: ready.max_width,
                })
            }
        }
    }

    /// Whether the topology is complete.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        match &self.0 {
            CompletionInner::Done(_) => true,
            CompletionInner::Building(stream) => stream.ready().is_some(),
        }
    }
}

/// Result of [`HistoryView::refresh`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RefreshOutcome {
    /// Nothing changed.
    Unchanged,
    /// Only ref names changed (badges updated, topology kept).
    BadgesUpdated,
    /// The topology changed and was rebuilt.
    Rebuilt {
        /// New number of rows.
        rows: u32,
    },
}

/// Bytes held in memory by an open view.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MemoryUsage {
    /// Lane-layout checkpoints.
    pub checkpoints: usize,
    /// Number of checkpoints.
    pub checkpoint_count: usize,
    /// Ref badges index.
    pub badges: usize,
    /// Commit metadata currently in the LRU.
    pub meta_cache: usize,
    /// Budget of the metadata LRU.
    pub meta_budget: usize,
}

impl MemoryUsage {
    /// Total bytes held.
    #[must_use]
    pub fn total(&self) -> usize {
        self.checkpoints + self.badges + self.meta_cache
    }
}

pub(crate) type MetaCache = SharedLru<Oid, Arc<CommitMeta>>;

/// The topology of an open repository.
enum Topo {
    Ready(Ready),
    /// Being built: the streamed rows are served until it is ready.
    Streaming(Arc<Stream>),
}

/// State of an open repository.
struct Loaded {
    repo: RepoPath,
    key: u64,
    topo: Topo,
    tips: Vec<RefTip>,
    badges: HashMap<Oid, Vec<RefTip>>,
    meta: Arc<MetaCache>,
}

impl Loaded {
    fn ready(&self) -> Option<&Ready> {
        match &self.topo {
            Topo::Ready(r) => Some(r),
            Topo::Streaming(s) => s.ready(),
        }
    }

    /// Rows that can be served now.
    fn available(&self) -> u32 {
        match (&self.topo, self.ready()) {
            (_, Some(r)) => r.file.rows(),
            (Topo::Streaming(s), None) => s.head_len(),
            (Topo::Ready(_), None) => 0,
        }
    }

    fn wait_ready(&self, progress: &dyn ProgressSink) -> Result<&Ready, GraphError> {
        match &self.topo {
            Topo::Ready(r) => Ok(r),
            Topo::Streaming(s) => s.wait_ready(progress),
        }
    }

    fn graph_row(&self, row: u32, oid: Oid, layout: RowLayout) -> GraphRow {
        GraphRow {
            row,
            oid,
            column: layout.column,
            color: layout.color,
            edges: layout.edges,
            width: layout.width,
            hidden_parents: layout.hidden_parents,
            refs: self.badges.get(&oid).cloned().unwrap_or_default(),
        }
    }

    /// Object ids of `range` (clamped to the available rows).
    fn oids(&self, range: Range<u32>) -> Result<Vec<Oid>, GraphError> {
        let range = clamp(range, self.available());
        if let Topo::Streaming(s) = &self.topo
            && let Some(head) = s.head(range.clone())
        {
            return Ok(head.into_iter().map(|(_, r)| r.oid).collect());
        }
        match self.ready() {
            Some(r) => r.file.read_oids(clamp(range, r.file.rows())),
            None => Ok(Vec::new()),
        }
    }
}

impl Drop for Loaded {
    fn drop(&mut self) {
        if let Topo::Streaming(s) = &self.topo {
            s.abandon();
        }
    }
}

/// Paged, virtualized history of one repository (PLAN §2.5).
///
/// * `open` loads the on-disk topology cache and computes lane checkpoints in
///   one sequential pass; when the cache must be (re)built, the build runs on
///   a worker thread and `open` returns once the first
///   [`GraphOptions::stream_rows`] rows are laid out (see [`Self::completion`]);
/// * `rows` lays out any window by replaying from the nearest checkpoint;
/// * `meta` loads commit metadata for a window ± overscan through a
///   byte-budgeted LRU registered in the [`CacheRegistry`];
/// * `searcher` gives a detached handle to search in the background;
/// * `unload` (or drop) releases checkpoints, badges and the LRU.
///
/// Queries take `&self` and are thread-safe; `open`, `refresh` and `unload`
/// take `&mut self`. All calls block: run them off the UI thread.
pub struct HistoryView {
    history: Arc<dyn HistoryReader>,
    refs: Arc<dyn RefReader>,
    caches: Arc<CacheRegistry>,
    cache_dir: PathBuf,
    options: GraphOptions,
    loaded: Option<Loaded>,
}

impl HistoryView {
    /// A view with nothing open. Cache files go to `cache_dir`, one per repository.
    #[must_use]
    pub fn new(
        history: Arc<dyn HistoryReader>,
        refs: Arc<dyn RefReader>,
        caches: Arc<CacheRegistry>,
        cache_dir: impl Into<PathBuf>,
        options: GraphOptions,
    ) -> Self {
        Self {
            history,
            refs,
            caches,
            cache_dir: cache_dir.into(),
            options,
            loaded: None,
        }
    }

    /// Path of the topology cache file of `repo`.
    #[must_use]
    pub fn cache_path(&self, repo: &RepoPath) -> PathBuf {
        self.cache_dir
            .join(format!("{}.{CACHE_EXTENSION}", repo.id()))
    }

    /// Opens `repo`, replacing whatever was open. Reuses the cache file when
    /// its key matches the current refs, otherwise (missing, stale, corrupt)
    /// rebuilds it by walking the history once.
    pub fn open(
        &mut self,
        repo: &RepoPath,
        progress: &dyn ProgressSink,
        cancel: &CancelToken,
    ) -> Result<OpenReport, GraphError> {
        self.unload();
        let tips = self.refs.ref_tips(repo)?;
        let meta = self.caches.create(
            format!("{META_CACHE_NAME}:{}", repo.id()),
            self.options.meta_cache_bytes,
        );
        let (loaded, cache) = self.load(repo, tips, meta, progress, cancel)?;
        let rows = loaded.available();
        let complete = loaded.ready().is_some();
        self.loaded = Some(loaded);
        Ok(OpenReport {
            rows,
            cache,
            complete,
        })
    }

    /// A handle to wait for the complete topology (`None` when nothing is open).
    #[must_use]
    pub fn completion(&self) -> Option<Completion> {
        let loaded = self.loaded.as_ref()?;
        Some(Completion(match (&loaded.topo, loaded.ready()) {
            (Topo::Streaming(s), None) => CompletionInner::Building(Arc::clone(s)),
            (_, ready) => CompletionInner::Done(GraphSize {
                rows: ready.map_or(0, |r| r.file.rows()),
                max_width: ready.map_or(0, |r| r.max_width),
            }),
        }))
    }

    /// Blocks until the topology is complete; see [`Completion::wait`].
    pub fn wait_complete(&self, progress: &dyn ProgressSink) -> Result<GraphSize, GraphError> {
        self.completion().ok_or(GraphError::NotOpen)?.wait(progress)
    }

    /// Whether the whole topology is loaded (false when nothing is open).
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.loaded.as_ref().is_some_and(|l| l.ready().is_some())
    }

    /// Re-reads the refs: rebuilds the topology when the set of tips changed,
    /// otherwise only refreshes the badges. Commit metadata stays cached
    /// (it is keyed by commit id). Waits for a build in progress first, and
    /// returns once the new topology is complete.
    pub fn refresh(
        &mut self,
        progress: &dyn ProgressSink,
        cancel: &CancelToken,
    ) -> Result<RefreshOutcome, GraphError> {
        self.loaded
            .as_ref()
            .ok_or(GraphError::NotOpen)?
            .wait_ready(progress)?;
        let loaded = self.loaded.as_mut().ok_or(GraphError::NotOpen)?;
        let tips = self.refs.ref_tips(&loaded.repo)?;
        let key = topology::topology_key(&loaded.repo, self.options.walk_order, &tips);
        if key == loaded.key {
            if tips == loaded.tips {
                return Ok(RefreshOutcome::Unchanged);
            }
            loaded.badges = index_badges(&tips);
            loaded.tips = tips;
            return Ok(RefreshOutcome::BadgesUpdated);
        }
        // Release our handle on the old file before replacing it.
        let Some(old) = self.loaded.take() else {
            return Err(GraphError::NotOpen);
        };
        let (repo, meta) = (old.repo.clone(), Arc::clone(&old.meta));
        drop(old);
        let (loaded, _) = self.load(&repo, tips, meta, progress, cancel)?;
        let rows = loaded.wait_ready(progress)?.file.rows();
        self.loaded = Some(loaded);
        Ok(RefreshOutcome::Rebuilt { rows })
    }

    /// Releases the checkpoints, the badges, the metadata LRU and the file handle.
    pub fn unload(&mut self) {
        self.loaded = None;
    }

    /// Whether a repository is open.
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.loaded.is_some()
    }

    /// The open repository.
    #[must_use]
    pub fn repo(&self) -> Option<&RepoPath> {
        self.loaded.as_ref().map(|l| &l.repo)
    }

    /// Number of rows available (0 when nothing is open): all of them once
    /// the topology is complete, the streamed rows while it is being built.
    #[must_use]
    pub fn row_count(&self) -> u32 {
        self.loaded.as_ref().map_or(0, Loaded::available)
    }

    /// Widest row of the whole graph, in columns (for horizontal sizing);
    /// of the streamed rows while the topology is being built.
    #[must_use]
    pub fn max_width(&self) -> u32 {
        let Some(loaded) = &self.loaded else { return 0 };
        match (&loaded.topo, loaded.ready()) {
            (_, Some(r)) => r.max_width,
            (Topo::Streaming(s), None) => s.head_width(),
            (Topo::Ready(_), None) => 0,
        }
    }

    /// Lays out rows `range` (clamped to the available rows) by replaying from
    /// the nearest checkpoint at or before `range.start`.
    pub fn rows(&self, range: Range<u32>) -> Result<Vec<GraphRow>, GraphError> {
        let loaded = self.loaded.as_ref().ok_or(GraphError::NotOpen)?;
        if let Topo::Streaming(s) = &loaded.topo
            && let Some(head) = s.head(range.clone())
        {
            return Ok(head
                .into_iter()
                .map(|(row, r)| loaded.graph_row(row, r.oid, r.layout))
                .collect());
        }
        let Some(ready) = loaded.ready() else {
            return Ok(Vec::new());
        };
        let file = &ready.file;
        let range = clamp(range, file.rows());
        if range.is_empty() {
            return Ok(Vec::new());
        }
        let interval = self.options.interval();
        let index = range.start / interval;
        let replay_start = index * interval;
        let mut state = ready
            .checkpoints
            .get(usize::try_from(index).unwrap_or(usize::MAX))
            .cloned()
            .unwrap_or_default();
        let records = file.read_parent_records(replay_start..range.end)?;
        let oids = file.read_oids(range.clone())?;
        let mut links = Vec::new();
        let mut out = Vec::with_capacity(oids.len());
        for (row, record) in (replay_start..).zip(records) {
            file.decode_parents(row, record, &mut links)?;
            let layout = state.step(row, &links);
            if row < range.start {
                continue;
            }
            let oid = oids[usize::try_from(row - range.start).unwrap_or(usize::MAX)];
            out.push(loaded.graph_row(row, oid, layout));
        }
        Ok(out)
    }

    /// Object ids of rows `range` (clamped to the available rows).
    pub fn oids(&self, range: Range<u32>) -> Result<Vec<Oid>, GraphError> {
        self.loaded.as_ref().ok_or(GraphError::NotOpen)?.oids(range)
    }

    /// Metadata of rows `range` (clamped to the available rows), in row
    /// order. Rows of the window ± overscan missing from the LRU are loaded in
    /// one backend call.
    pub fn meta(&self, range: Range<u32>) -> Result<Vec<Arc<CommitMeta>>, GraphError> {
        let loaded = self.loaded.as_ref().ok_or(GraphError::NotOpen)?;
        let rows = loaded.available();
        let range = clamp(range, rows);
        if range.is_empty() {
            return Ok(Vec::new());
        }
        let oids = loaded.oids(range.clone())?;
        let mut out: Vec<Option<Arc<CommitMeta>>> =
            oids.iter().map(|oid| loaded.meta.get(oid)).collect();
        if out.iter().any(Option::is_none) {
            // Load every missing row of the window ± overscan in one call.
            let overscan = u32::try_from(self.options.overscan).unwrap_or(u32::MAX);
            let before = loaded.oids(range.start.saturating_sub(overscan)..range.start)?;
            let after = loaded.oids(range.end..range.end.saturating_add(overscan).min(rows))?;
            let mut missing: Vec<Oid> = oids
                .iter()
                .zip(&out)
                .filter(|(_, m)| m.is_none())
                .map(|(oid, _)| *oid)
                .collect();
            missing.extend(
                before
                    .iter()
                    .chain(&after)
                    .filter(|oid| loaded.meta.get(oid).is_none()),
            );
            let mut fetched: HashMap<Oid, Arc<CommitMeta>> = HashMap::with_capacity(missing.len());
            for m in self.history.commit_meta(&loaded.repo, &missing)? {
                let m = Arc::new(m);
                loaded
                    .meta
                    .insert(m.oid, Arc::clone(&m), m.approx_size() + META_ENTRY_OVERHEAD);
                fetched.insert(m.oid, m);
            }
            for (slot, oid) in out.iter_mut().zip(&oids) {
                if slot.is_none() {
                    *slot = fetched.get(oid).cloned();
                }
            }
        }
        out.into_iter()
            .zip(&oids)
            .map(|(m, oid)| m.ok_or(GraphError::MissingMeta(*oid)))
            .collect()
    }

    /// A handle to search the open history from another thread. It keeps the
    /// current snapshot alive (its rows stay valid even after a refresh).
    /// Waits for the topology to be complete.
    pub fn searcher(&self) -> Result<Searcher, GraphError> {
        let loaded = self.loaded.as_ref().ok_or(GraphError::NotOpen)?;
        let ready = loaded.wait_ready(&archipel_model::NoProgress)?;
        Ok(Searcher::new(
            Arc::clone(&self.history),
            loaded.repo.clone(),
            Arc::clone(&ready.file),
            self.options.search_chunk_rows,
        ))
    }

    /// Searches on the calling thread; see [`Searcher::search`].
    pub fn search(
        &self,
        query: &SearchQuery,
        cancel: &CancelToken,
    ) -> Result<SearchResult, GraphError> {
        self.searcher()?.search(query, cancel)
    }

    /// Row of `oid`, if it is in the history (sequential scan of the ids).
    pub fn row_of(&self, oid: Oid, cancel: &CancelToken) -> Result<Option<u32>, GraphError> {
        self.searcher()?.row_of(oid, cancel)
    }

    /// Bytes currently held by the open view.
    #[must_use]
    pub fn memory_usage(&self) -> MemoryUsage {
        let Some(l) = &self.loaded else {
            return MemoryUsage::default();
        };
        let (checkpoints, checkpoint_count) = match (&l.topo, l.ready()) {
            (_, Some(r)) => (
                r.checkpoints.capacity() * std::mem::size_of::<LaneState>()
                    + r.checkpoints
                        .iter()
                        .map(LaneState::heap_bytes)
                        .sum::<usize>(),
                r.checkpoints.len(),
            ),
            // The streamed rows stand in for the checkpoints while building.
            (Topo::Streaming(s), None) => (s.head_bytes(), 0),
            (Topo::Ready(_), None) => (0, 0),
        };
        MemoryUsage {
            checkpoints,
            checkpoint_count,
            badges: badges_bytes(&l.badges, &l.tips),
            meta_cache: l.meta.bytes(),
            meta_budget: self.options.meta_cache_bytes,
        }
    }

    fn load(
        &self,
        repo: &RepoPath,
        tips: Vec<RefTip>,
        meta: Arc<MetaCache>,
        progress: &dyn ProgressSink,
        cancel: &CancelToken,
    ) -> Result<(Loaded, CacheStatus), GraphError> {
        let key = topology::topology_key(repo, self.options.walk_order, &tips);
        let path = self.cache_path(repo);
        let interval = self.options.interval();
        let assemble = move |topo| Loaded {
            repo: repo.clone(),
            key,
            topo,
            badges: index_badges(&tips),
            tips,
            meta,
        };
        let status = match TopologyFile::open(&path, key)? {
            Ok(file) => match topology::checkpoints(&file, interval, progress, cancel) {
                Ok((checkpoints, max_width)) => {
                    debug_assert_eq!(file.key(), key);
                    let ready = Ready {
                        file: Arc::new(file),
                        checkpoints,
                        max_width,
                    };
                    return Ok((assemble(Topo::Ready(ready)), CacheStatus::Reused));
                }
                Err(GraphError::Corrupt { reason, .. }) => {
                    tracing::warn!(path = %path.display(), %reason, "topology cache corrupt, rebuilding");
                    CacheStatus::BuiltCorrupt(reason)
                }
                Err(e) => return Err(e),
            },
            Err(Unusable::Missing) => CacheStatus::BuiltMissing,
            Err(Unusable::Stale) => CacheStatus::BuiltStale,
            Err(Unusable::Corrupt(reason)) => {
                tracing::warn!(path = %path.display(), %reason, "topology cache corrupt, rebuilding");
                CacheStatus::BuiltCorrupt(reason)
            }
        };
        cancel.check()?;
        let stream = Stream::spawn(BuildJob {
            path,
            history: Arc::clone(&self.history),
            repo: repo.clone(),
            order: self.options.walk_order,
            key,
            checkpoint_interval: interval,
            head_rows: u32::try_from(self.options.stream_rows.max(1)).unwrap_or(u32::MAX),
            cancel: cancel.clone(),
        })?;
        // On failure, dropping `loaded` stops the worker.
        let loaded = assemble(Topo::Streaming(stream));
        if let Topo::Streaming(s) = &loaded.topo {
            s.wait_first_window(progress)?;
        }
        Ok((loaded, status))
    }

    /// Directory holding the cache files.
    #[must_use]
    pub fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }
}

fn clamp(range: Range<u32>, rows: u32) -> Range<u32> {
    let end = range.end.min(rows);
    range.start.min(end)..end
}

fn kind_rank(kind: RefKind) -> u8 {
    match kind {
        RefKind::Head => 0,
        RefKind::LocalBranch => 1,
        RefKind::RemoteBranch => 2,
        RefKind::Tag => 3,
        RefKind::Stash => 4,
    }
}

fn index_badges(tips: &[RefTip]) -> HashMap<Oid, Vec<RefTip>> {
    let mut map: HashMap<Oid, Vec<RefTip>> = HashMap::new();
    for t in tips {
        map.entry(t.target).or_default().push(t.clone());
    }
    for list in map.values_mut() {
        list.sort_by(|a, b| {
            kind_rank(a.kind)
                .cmp(&kind_rank(b.kind))
                .then_with(|| a.name.cmp(&b.name))
        });
    }
    map
}

fn badges_bytes(badges: &HashMap<Oid, Vec<RefTip>>, tips: &[RefTip]) -> usize {
    let per_tip = |t: &RefTip| std::mem::size_of::<RefTip>() + t.name.capacity();
    let map = badges.capacity() * (std::mem::size_of::<Oid>() + std::mem::size_of::<Vec<RefTip>>())
        + badges.values().flatten().map(per_tip).sum::<usize>();
    map + tips.iter().map(per_tip).sum::<usize>()
}
