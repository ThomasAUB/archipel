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
use archipel_model::{CancelToken, Oid, Progress, ProgressSink, RepoPath};

use crate::error::GraphError;
use crate::layout::{Edge, LaneState, ParentLink};
use crate::search::{SearchQuery, SearchResult, Searcher};
use crate::topology::{self, PROGRESS_EVERY_ROWS, TopologyFile, Unusable};

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
}

impl GraphOptions {
    /// Default rows per search step: large enough to amortize backend calls,
    /// small enough to react quickly to cancellation.
    pub const DEFAULT_SEARCH_CHUNK_ROWS: usize = 2048;

    /// Options taken from the application settings.
    #[must_use]
    pub fn from_settings(settings: &Settings) -> Self {
        Self {
            checkpoint_interval: settings.graph_checkpoint_interval,
            overscan: settings.graph_overscan,
            meta_cache_bytes: settings.commit_cache_bytes,
            walk_order: WalkOrder::Topological,
            search_chunk_rows: Self::DEFAULT_SEARCH_CHUNK_ROWS,
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
    /// Number of rows.
    pub rows: u32,
    /// What happened to the cache file.
    pub cache: CacheStatus,
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

/// State of an open repository.
struct Loaded {
    repo: RepoPath,
    key: u64,
    file: Arc<TopologyFile>,
    checkpoints: Vec<LaneState>,
    max_width: u32,
    tips: Vec<RefTip>,
    badges: HashMap<Oid, Vec<RefTip>>,
    meta: Arc<MetaCache>,
}

/// Paged, virtualized history of one repository (PLAN §2.5).
///
/// * `open` loads (or builds) the on-disk topology cache and computes lane
///   checkpoints in one sequential pass;
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
        let rows = loaded.file.rows();
        self.loaded = Some(loaded);
        Ok(OpenReport { rows, cache })
    }

    /// Re-reads the refs: rebuilds the topology when the set of tips changed,
    /// otherwise only refreshes the badges. Commit metadata stays cached
    /// (it is keyed by commit id).
    pub fn refresh(
        &mut self,
        progress: &dyn ProgressSink,
        cancel: &CancelToken,
    ) -> Result<RefreshOutcome, GraphError> {
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
        let Loaded { repo, meta, .. } = old;
        let (loaded, _) = self.load(&repo, tips, meta, progress, cancel)?;
        let rows = loaded.file.rows();
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

    /// Number of rows (0 when nothing is open).
    #[must_use]
    pub fn row_count(&self) -> u32 {
        self.loaded.as_ref().map_or(0, |l| l.file.rows())
    }

    /// Widest row of the whole graph, in columns (for horizontal sizing).
    #[must_use]
    pub fn max_width(&self) -> u32 {
        self.loaded.as_ref().map_or(0, |l| l.max_width)
    }

    /// Lays out rows `range` (clamped to the row count) by replaying from the
    /// nearest checkpoint at or before `range.start`.
    pub fn rows(&self, range: Range<u32>) -> Result<Vec<GraphRow>, GraphError> {
        let loaded = self.loaded.as_ref().ok_or(GraphError::NotOpen)?;
        let range = clamp(range, loaded.file.rows());
        if range.is_empty() {
            return Ok(Vec::new());
        }
        let interval = self.options.interval();
        let index = range.start / interval;
        let replay_start = index * interval;
        let mut state = loaded
            .checkpoints
            .get(usize::try_from(index).unwrap_or(usize::MAX))
            .cloned()
            .unwrap_or_default();
        let records = loaded.file.read_parent_records(replay_start..range.end)?;
        let oids = loaded.file.read_oids(range.clone())?;
        let mut links = Vec::new();
        let mut out = Vec::with_capacity(oids.len());
        for (row, record) in (replay_start..).zip(records) {
            loaded.file.decode_parents(row, record, &mut links)?;
            let layout = state.step(row, &links);
            if row < range.start {
                continue;
            }
            let oid = oids[usize::try_from(row - range.start).unwrap_or(usize::MAX)];
            out.push(GraphRow {
                row,
                oid,
                column: layout.column,
                color: layout.color,
                edges: layout.edges,
                width: layout.width,
                hidden_parents: layout.hidden_parents,
                refs: loaded.badges.get(&oid).cloned().unwrap_or_default(),
            });
        }
        Ok(out)
    }

    /// Object ids of rows `range` (clamped).
    pub fn oids(&self, range: Range<u32>) -> Result<Vec<Oid>, GraphError> {
        let loaded = self.loaded.as_ref().ok_or(GraphError::NotOpen)?;
        loaded.file.read_oids(clamp(range, loaded.file.rows()))
    }

    /// Metadata of rows `range` (clamped), in row order. Rows of the window ±
    /// overscan missing from the LRU are loaded in one backend call.
    pub fn meta(&self, range: Range<u32>) -> Result<Vec<Arc<CommitMeta>>, GraphError> {
        let loaded = self.loaded.as_ref().ok_or(GraphError::NotOpen)?;
        let rows = loaded.file.rows();
        let range = clamp(range, rows);
        if range.is_empty() {
            return Ok(Vec::new());
        }
        let oids = loaded.file.read_oids(range.clone())?;
        let mut out: Vec<Option<Arc<CommitMeta>>> =
            oids.iter().map(|oid| loaded.meta.get(oid)).collect();
        if out.iter().any(Option::is_none) {
            // Load every missing row of the window ± overscan in one call.
            let overscan = u32::try_from(self.options.overscan).unwrap_or(u32::MAX);
            let before = loaded
                .file
                .read_oids(range.start.saturating_sub(overscan)..range.start)?;
            let after = loaded
                .file
                .read_oids(range.end..range.end.saturating_add(overscan).min(rows))?;
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
    pub fn searcher(&self) -> Result<Searcher, GraphError> {
        let loaded = self.loaded.as_ref().ok_or(GraphError::NotOpen)?;
        Ok(Searcher::new(
            Arc::clone(&self.history),
            loaded.repo.clone(),
            Arc::clone(&loaded.file),
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
        MemoryUsage {
            checkpoints: l.checkpoints.capacity() * std::mem::size_of::<LaneState>()
                + l.checkpoints
                    .iter()
                    .map(LaneState::heap_bytes)
                    .sum::<usize>(),
            checkpoint_count: l.checkpoints.len(),
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
        let status = match TopologyFile::open(&path, key)? {
            Ok(file) => match self.checkpoint(file, progress, cancel) {
                Ok(layout) => {
                    return Ok((
                        Self::assemble(repo, key, tips, meta, layout),
                        CacheStatus::Reused,
                    ));
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
        topology::build(
            &path,
            self.history.as_ref(),
            repo,
            self.options.walk_order,
            key,
            progress,
            cancel,
        )?;
        let file = match TopologyFile::open(&path, key)? {
            Ok(f) => f,
            Err(why) => {
                return Err(GraphError::corrupt(
                    &path,
                    format!("freshly built cache unusable: {why:?}"),
                ));
            }
        };
        let layout = self.checkpoint(file, progress, cancel)?;
        Ok((Self::assemble(repo, key, tips, meta, layout), status))
    }

    /// One sequential pass over the file: verifies it and saves the lane
    /// state every `checkpoint_interval` rows.
    fn checkpoint(
        &self,
        file: TopologyFile,
        progress: &dyn ProgressSink,
        cancel: &CancelToken,
    ) -> Result<(TopologyFile, Vec<LaneState>, u32), GraphError> {
        let interval = self.options.interval();
        let total = u64::from(file.rows());
        let mut checkpoints =
            Vec::with_capacity(usize::try_from(file.rows() / interval + 1).unwrap_or(0));
        let mut state = LaneState::new();
        let mut max_width = 0u32;
        file.scan(cancel, &mut |row, links: &[ParentLink]| {
            if row % interval == 0 {
                checkpoints.push(state.clone());
            }
            if row % PROGRESS_EVERY_ROWS == 0 {
                progress.report(Progress {
                    phase: "Laying out graph".into(),
                    current: u64::from(row),
                    total: Some(total),
                });
            }
            max_width = max_width.max(state.advance(row, links));
        })?;
        checkpoints.shrink_to_fit();
        Ok((file, checkpoints, max_width))
    }

    fn assemble(
        repo: &RepoPath,
        key: u64,
        tips: Vec<RefTip>,
        meta: Arc<MetaCache>,
        (file, checkpoints, max_width): (TopologyFile, Vec<LaneState>, u32),
    ) -> Loaded {
        debug_assert_eq!(file.key(), key);
        Loaded {
            repo: repo.clone(),
            key,
            file: Arc::new(file),
            checkpoints,
            max_width,
            badges: index_badges(&tips),
            tips,
            meta,
        }
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
