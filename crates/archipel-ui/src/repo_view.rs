//! A repository tab (spec §5): virtualized commit graph with commit details
//! and diff, and the working copy with file / hunk staging and a commit box.
//!
//! Every Git call runs on a background thread; the graph rows come from the
//! on-disk topology cache and commit metadata from its bounded LRU.

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::sync::{Arc, Mutex, PoisonError};

use archipel_app::App as Core;
use archipel_app::forge::pick_remote;
use archipel_graph::{Edge, EdgeKind, GraphRow, HistoryView};
use archipel_model::changes::{ChangeKind, FileChange, FileState};
use archipel_model::diff::{DiffLine, FileDiff, LineOrigin};
use archipel_model::history::CommitMeta;
use archipel_model::ports::DiffSide;
use archipel_model::refs::{RefKind, RefTip, StashEntry};
use archipel_model::requests::{
    CheckoutTarget, CommitMode, CommitRequest, FetchRequest, PatchDirection, PullMode, PushRequest,
    StashApplyMode, StashRequest,
};
use archipel_model::{
    CancelToken, HeadState, NoProgress, Oid, RepoPath, RepoSummary, Timestamp, Upstream,
};
use archipel_ops::patch::hunk_patch;
use archipel_theme::ColorToken as T;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, App, Bounds, ClickEvent, Context, DragMoveEvent, Entity, FocusHandle, Hsla,
    KeyDownEvent, PathBuilder, Pixels, Point, ScrollStrategy, SharedString, Subscription,
    UniformListScrollHandle, Window, canvas, div, point, px, uniform_list,
};

use crate::theme::{color, lane, mono_font, radius, row_height};
use crate::widgets::{
    Pane, PaneDrag, button, chip, primary_button, resize_pane, section, splitter,
};

/// Width of one graph lane.
const LANE_WIDTH: f32 = 14.0;
/// Lanes shown before the graph column stops growing (wide graphs clip).
const MAX_LANES_SHOWN: u32 = 24;
/// Metadata entries kept by the view (rows around the visible window).
const META_KEEP: usize = 4000;
/// Rows moved by Page Up / Page Down in the graph.
const PAGE_ROWS: i64 = 20;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    History,
    Changes,
}

/// One fixed-height row of a flattened diff.
#[derive(Clone, Copy)]
enum DiffRow {
    /// File header: file index.
    File(usize),
    /// A note in place of the hunks (binary, too large).
    Note(&'static str),
    /// Hunk header: file, hunk.
    Hunk(usize, usize),
    /// Diff line: file, hunk, line.
    Line(usize, usize, usize),
}

/// A diff flattened into rows once, so the view renders only the visible
/// ones (a diff may hold tens of thousands of lines).
struct DiffDoc {
    files: Vec<FileDiff>,
    rows: Vec<DiffRow>,
    /// Row of each file's header, to jump to a file from the file list.
    file_rows: Vec<usize>,
    /// Digits of the largest line number, so the gutter has one width.
    lineno_width: usize,
}

impl DiffDoc {
    fn new(files: Vec<FileDiff>) -> Self {
        let mut rows = Vec::new();
        let mut file_rows = Vec::with_capacity(files.len());
        let mut max_lineno = 0;
        for (f, file) in files.iter().enumerate() {
            file_rows.push(rows.len());
            rows.push(DiffRow::File(f));
            if file.binary {
                rows.push(DiffRow::Note("Binary file"));
                continue;
            }
            if file.truncated {
                rows.push(DiffRow::Note("Diff too large to display"));
                continue;
            }
            for (h, hunk) in file.hunks.iter().enumerate() {
                rows.push(DiffRow::Hunk(f, h));
                rows.extend((0..hunk.lines.len()).map(|l| DiffRow::Line(f, h, l)));
                max_lineno = hunk
                    .lines
                    .iter()
                    .flat_map(|l| [l.old_lineno, l.new_lineno])
                    .flatten()
                    .fold(max_lineno, u32::max);
            }
        }
        Self {
            files,
            rows,
            file_rows,
            lineno_width: max_lineno.to_string().len(),
        }
    }
}

/// One fixed-height row of the refs panel.
enum RefRow {
    /// Section title and entry count.
    Header(&'static str, usize),
    /// A branch or tag; `current` when `HEAD` is on it.
    Tip { tip: RefTip, current: bool },
}

/// Branches and tags of the refs panel, flattened into rows (a repo may
/// have thousands of tags, so the panel is virtualized too).
#[derive(Default)]
struct RefList {
    rows: Vec<RefRow>,
    /// Local branch names, to check out a remote branch through its local
    /// counterpart when it exists.
    locals: HashSet<String>,
}

/// A stash drawn in the graph, in a row of its own just above the commit
/// it was made on.
struct PlacedStash {
    entry: StashEntry,
    /// Graph row of `entry.base`.
    row: u32,
}

/// What one item of the graph list shows.
#[derive(Clone, Copy)]
enum Item {
    /// The uncommitted-changes row.
    Wip,
    /// A stash: index in `RepoView::stashes`.
    Stash(usize),
    /// A commit: graph row.
    Commit(u32),
}

/// A push of a branch without upstream, waiting for confirmation.
struct UpstreamPrompt {
    branch: String,
    remotes: Vec<String>,
    /// Index of the chosen remote.
    remote: usize,
}

type SharedView = Arc<Mutex<HistoryView>>;

/// The parts of a repo summary the graph and refs panel depend on: when
/// they move, the graph is reloaded; a working-copy edit alone does not.
#[derive(PartialEq, Eq)]
struct RefsState {
    head: HeadState,
    head_oid: Option<Oid>,
    upstream: Option<Upstream>,
    describe: Option<String>,
    stash_count: u32,
    last_fetch: Option<Timestamp>,
}

impl RefsState {
    fn of(s: &RepoSummary) -> Self {
        Self {
            head: s.head.clone(),
            head_oid: s.head_oid,
            upstream: s.upstream.clone(),
            describe: s.describe.clone(),
            stash_count: s.stash_count,
            last_fetch: s.last_fetch,
        }
    }
}

fn lock(v: &SharedView) -> std::sync::MutexGuard<'_, HistoryView> {
    v.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The repository tab.
pub struct RepoView {
    core: Arc<Core>,
    path: RepoPath,
    name: String,
    mode: Mode,
    graph: Option<SharedView>,
    row_count: u32,
    lanes: u32,
    graph_error: Option<String>,
    meta: HashMap<u32, Arc<CommitMeta>>,
    pending: Option<Range<u32>>,
    selected: Option<(u32, Oid)>,
    /// The uncommitted-changes row at the top of the graph is selected.
    wip_selected: bool,
    /// Stashes placed in the graph, by row then stash index (newest first
    /// among the stashes of one commit).
    stashes: Vec<PlacedStash>,
    /// Commit id of the selected stash.
    stash_selected: Option<Oid>,
    /// Diff shown in the details pane: a commit's, or the working copy's
    /// (`None`).
    details: Option<(Option<Oid>, Arc<DiffDoc>)>,
    /// Scroll of the commit diff, driven by the changed-files list.
    details_scroll: UniformListScrollHandle,
    changes: Vec<FileChange>,
    file_diff: Option<(String, DiffSide, Arc<DiffDoc>)>,
    message: Entity<InputState>,
    amend: bool,
    /// Name of the branch to create; the form shows while `branching`.
    branch_name: Entity<InputState>,
    branching: bool,
    _branch_sub: Subscription,
    /// Shown while a push waits for the user to create the upstream.
    upstream_prompt: Option<UpstreamPrompt>,
    prompt_focus: FocusHandle,
    status: Option<String>,
    refs: Arc<RefList>,
    /// Refs state of the last summary seen; `None` until the next summary,
    /// which is then taken as is (the graph was just loaded).
    refs_state: Option<RefsState>,
    scroll: UniformListScrollHandle,
    focus: FocusHandle,
}

impl RepoView {
    /// Creates the tab and starts loading the graph and the working copy.
    pub fn new(
        core: Arc<Core>,
        path: RepoPath,
        name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let message = cx.new(|cx| InputState::new(window, cx).placeholder("Commit message"));
        let branch_name = cx.new(|cx| InputState::new(window, cx).placeholder("Branch name"));
        let branch_sub = cx.subscribe_in(
            &branch_name,
            window,
            |v: &mut Self, _, event: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    v.create_branch(window, cx);
                }
            },
        );
        let mut view = Self {
            core,
            path,
            name,
            mode: Mode::History,
            graph: None,
            row_count: 0,
            lanes: 1,
            graph_error: None,
            meta: HashMap::new(),
            pending: None,
            selected: None,
            wip_selected: false,
            stashes: Vec::new(),
            stash_selected: None,
            details: None,
            details_scroll: UniformListScrollHandle::new(),
            changes: Vec::new(),
            file_diff: None,
            message,
            amend: false,
            branch_name,
            branching: false,
            _branch_sub: branch_sub,
            upstream_prompt: None,
            prompt_focus: cx.focus_handle(),
            status: None,
            refs: Arc::default(),
            refs_state: None,
            scroll: UniformListScrollHandle::new(),
            focus: cx.focus_handle(),
        };
        view.load_graph(cx);
        view.load_refs(cx);
        view.load_changes(cx);
        view
    }

    fn load_graph(&mut self, cx: &mut Context<Self>) {
        let mut graph = self.core.history_view();
        let path = self.path.clone();
        let task = cx.background_spawn(async move {
            let report = graph
                .open(&path, &NoProgress, &CancelToken::new())
                .map_err(|e| e.to_string())?;
            let lanes = graph.max_width();
            let completion = graph.completion().filter(|c| !c.is_complete());
            Ok::<_, String>((graph, report.rows, lanes, completion))
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let mut pending = None;
            let _ = this.update(cx, |v, cx| {
                match result {
                    Ok((graph, rows, lanes, completion)) => {
                        let graph = Arc::new(Mutex::new(graph));
                        pending = completion.map(|c| (c, Arc::clone(&graph)));
                        v.graph = Some(graph);
                        v.row_count = rows;
                        v.lanes = lanes.clamp(1, MAX_LANES_SHOWN);
                        v.meta.clear();
                        v.stashes.clear();
                        v.load_stashes(cx);
                    }
                    Err(e) => v.graph_error = Some(e),
                }
                cx.notify();
            });
            // The first rows are shown; the rest of the topology is still
            // being built. Wait for it without holding the view's lock.
            let Some((completion, graph)) = pending else {
                return;
            };
            let size = cx
                .background_executor()
                .spawn(async move { completion.wait(&NoProgress) })
                .await;
            let _ = this.update(cx, |v, cx| {
                if !v.graph.as_ref().is_some_and(|g| Arc::ptr_eq(g, &graph)) {
                    return; // reloaded meanwhile
                }
                match size {
                    Ok(size) => {
                        v.row_count = size.rows;
                        v.lanes = size.max_width.clamp(1, MAX_LANES_SHOWN);
                    }
                    Err(e) if e.is_cancelled() => {}
                    Err(e) => v.graph_error = Some(e.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Reads the stash list and places each stash above the commit it was
    /// made on. Finding those rows needs the whole topology, so this waits
    /// for the build. Stashes made on commits outside the history are left out.
    fn load_stashes(&mut self, cx: &mut Context<Self>) {
        let Some(graph) = self.graph.clone() else {
            return;
        };
        let reader = self.core.repo_actions().ports().stashes.clone();
        let path = self.path.clone();
        let task = {
            let graph = Arc::clone(&graph);
            cx.background_spawn(async move {
                let entries = reader.stashes(&path).map_err(|e| e.to_string())?;
                if entries.is_empty() {
                    return Ok(Vec::new());
                }
                let completion = lock(&graph).completion();
                if let Some(c) = completion {
                    c.wait(&NoProgress).map_err(|e| e.to_string())?;
                }
                let searcher = lock(&graph).searcher().map_err(|e| e.to_string())?;
                let bases: Vec<Oid> = entries.iter().map(|s| s.base).collect();
                let rows = searcher
                    .rows_of(&bases, &CancelToken::new())
                    .map_err(|e| e.to_string())?;
                let mut placed: Vec<PlacedStash> = entries
                    .into_iter()
                    .filter_map(|entry| {
                        let row = *rows.get(&entry.base)?;
                        Some(PlacedStash { entry, row })
                    })
                    .collect();
                placed.sort_by_key(|s| (s.row, s.entry.index));
                Ok::<_, String>(placed)
            })
        };
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |v, cx| {
                if !v.graph.as_ref().is_some_and(|g| Arc::ptr_eq(g, &graph)) {
                    return; // reloaded meanwhile
                }
                match result {
                    Ok(placed) => v.stashes = placed,
                    Err(e) => v.status = Some(e),
                }
                if v.stash_selected.is_some_and(|o| v.stash_at(o).is_none()) {
                    v.stash_selected = None;
                    v.details = None;
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Stashes whose row is loaded (all of them once the build is complete).
    fn placed(&self) -> &[PlacedStash] {
        let n = self.stashes.partition_point(|s| s.row < self.row_count);
        &self.stashes[..n]
    }

    /// Index in `stashes` of the stash with commit id `oid`.
    fn stash_at(&self, oid: Oid) -> Option<usize> {
        self.placed().iter().position(|s| s.entry.oid == oid)
    }

    /// Items of the graph list: the uncommitted-changes row, commits and stashes.
    fn item_count(&self) -> u32 {
        self.wip_rows() + self.row_count + u32::try_from(self.placed().len()).unwrap_or(0)
    }

    /// What list item `item` shows. Stash `k` sits right above its base
    /// commit, at position `row + k` once the uncommitted-changes row is
    /// skipped.
    fn item_at(&self, item: u32) -> Item {
        let wip = self.wip_rows();
        if item < wip {
            return Item::Wip;
        }
        let i = item - wip;
        let mut before = 0;
        for (k, s) in self.placed().iter().enumerate() {
            let at = s.row + u32::try_from(k).unwrap_or(u32::MAX);
            match at.cmp(&i) {
                std::cmp::Ordering::Less => before += 1,
                std::cmp::Ordering::Equal => return Item::Stash(k),
                std::cmp::Ordering::Greater => break,
            }
        }
        Item::Commit(i - before)
    }

    /// List item of graph row `row`.
    fn item_of_row(&self, row: u32) -> u32 {
        let stashes = self.placed().iter().take_while(|s| s.row <= row).count();
        self.wip_rows() + row + u32::try_from(stashes).unwrap_or(0)
    }

    /// List item of stash `k`.
    fn item_of_stash(&self, k: usize) -> u32 {
        self.wip_rows() + self.placed()[k].row + u32::try_from(k).unwrap_or(0)
    }

    /// Lanes drawn in the graph column: one more for the stash lane.
    fn lane_count(&self) -> u32 {
        (self.lanes + u32::from(!self.placed().is_empty())).min(MAX_LANES_SHOWN)
    }

    fn load_changes(&mut self, cx: &mut Context<Self>) {
        let status = self.core.repo_actions().ports().status.clone();
        let path = self.path.clone();
        let task =
            cx.background_spawn(async move { status.changes(&path).map_err(|e| e.to_string()) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |v, cx| {
                match result {
                    Ok(c) => v.changes = c,
                    Err(e) => v.status = Some(e),
                }
                // The shown file diff follows the working copy: reloaded
                // while the file still has changes on that side, else cleared.
                if let Some((file, side, _)) = &v.file_diff {
                    let (file, side) = (file.clone(), *side);
                    let still_changed = v.changes.iter().any(|c| {
                        c.path == file
                            && if side == DiffSide::Staged {
                                c.is_staged()
                            } else {
                                c.is_unstaged()
                            }
                    });
                    if still_changed {
                        v.select_file(file, side, cx);
                    } else {
                        v.file_diff = None;
                    }
                }
                if v.wip_selected {
                    if v.changes.is_empty() {
                        v.wip_selected = false;
                        v.details = None;
                    } else {
                        v.load_wip_diff(cx);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Rows before the first commit in the graph list: the uncommitted
    /// changes row, shown while the working copy is dirty.
    fn wip_rows(&self) -> u32 {
        u32::from(!self.changes.is_empty())
    }

    /// Selects the uncommitted-changes row.
    fn select_wip(&mut self, cx: &mut Context<Self>) {
        self.selected = None;
        self.stash_selected = None;
        self.wip_selected = true;
        self.load_wip_diff(cx);
        cx.notify();
    }

    /// Loads the working copy's diff into the details pane: staged
    /// changes, then unstaged ones, then untracked files.
    fn load_wip_diff(&mut self, cx: &mut Context<Self>) {
        let diffs = self.core.diff_reader();
        let path = self.path.clone();
        // `git diff` skips untracked files; they are read one by one.
        let untracked: Vec<String> = self
            .changes
            .iter()
            .filter(|c| c.state == FileState::Untracked)
            .map(|c| c.path.clone())
            .collect();
        let task = cx.background_spawn(async move {
            let mut files = diffs.worktree_diff(&path, DiffSide::Staged, None)?;
            files.extend(diffs.worktree_diff(&path, DiffSide::Unstaged, None)?);
            for file in &untracked {
                files.extend(diffs.worktree_diff(&path, DiffSide::Unstaged, Some(file))?);
            }
            Ok::<_, archipel_model::GitError>(Arc::new(DiffDoc::new(files)))
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |v, cx| {
                if !v.wip_selected {
                    return; // another row was selected meanwhile
                }
                match result {
                    Ok(d) => {
                        // Keep the scroll when the same working copy reloads.
                        if !matches!(v.details, Some((None, _))) {
                            v.details_scroll = UniformListScrollHandle::new();
                        }
                        v.details = Some((None, d));
                    }
                    Err(e) => v.status = Some(e.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn load_refs(&mut self, cx: &mut Context<Self>) {
        let refs = self.core.repo_actions().ports().refs.clone();
        let path = self.path.clone();
        let task = cx.background_spawn(async move {
            let branches = refs.branches(&path)?;
            let tags = refs.tags(&path)?;
            let (remote, local): (Vec<_>, Vec<_>) = branches.into_iter().partition(|b| b.is_remote);
            let mut list = RefList {
                locals: local.iter().map(|b| b.name.clone()).collect(),
                ..RefList::default()
            };
            for (title, kind, branches) in [
                ("LOCAL", RefKind::LocalBranch, local),
                ("REMOTE", RefKind::RemoteBranch, remote),
            ] {
                list.rows.push(RefRow::Header(title, branches.len()));
                list.rows.extend(branches.into_iter().map(|b| RefRow::Tip {
                    current: b.is_head,
                    tip: RefTip {
                        name: b.name,
                        kind,
                        target: b.target,
                    },
                }));
            }
            list.rows.push(RefRow::Header("TAGS", tags.len()));
            list.rows
                .extend(tags.into_iter().rev().map(|t| RefRow::Tip {
                    current: false,
                    tip: RefTip {
                        name: t.name,
                        kind: RefKind::Tag,
                        target: t.target,
                    },
                }));
            Ok::<_, archipel_model::GitError>(list)
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |v, cx| {
                match result {
                    Ok(list) => v.refs = Arc::new(list),
                    Err(e) => v.status = Some(e.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Reloads graph, refs and changes (after an operation).
    fn reload(&mut self, cx: &mut Context<Self>) {
        self.refs_state = None;
        self.core.refresh_repo(self.path.id());
        self.load_graph(cx);
        self.load_refs(cx);
        self.load_changes(cx);
    }

    /// The repo was refreshed (file watcher, focus, F5): reloads the
    /// working copy, and the graph and refs when `HEAD` or refs moved.
    pub fn on_summary(&mut self, summary: Option<&RepoSummary>, cx: &mut Context<Self>) {
        let state = summary.map(RefsState::of);
        if self.refs_state.is_some() && state.is_some() && state != self.refs_state {
            self.load_graph(cx);
            self.load_refs(cx);
        }
        if state.is_some() {
            self.refs_state = state;
        }
        self.load_changes(cx);
    }

    fn fetch_meta(&mut self, range: Range<u32>, cx: &mut Context<Self>) {
        let Some(graph) = self.graph.clone() else {
            return;
        };
        if self.pending.is_some() {
            return;
        }
        self.pending = Some(range.clone());
        let task = cx
            .background_spawn(async move { lock(&graph).meta(range.clone()).map(|m| (range, m)) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |v, cx| {
                v.pending = None;
                if let Ok((range, metas)) = result {
                    if v.meta.len() > META_KEEP {
                        v.meta.clear();
                    }
                    for (row, m) in range.zip(metas) {
                        v.meta.insert(row, m);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn select_commit(&mut self, row: u32, oid: Oid, cx: &mut Context<Self>) {
        self.selected = Some((row, oid));
        self.wip_selected = false;
        self.stash_selected = None;
        let diffs = self.core.diff_reader();
        let path = self.path.clone();
        let task = cx.background_spawn(async move {
            diffs
                .commit_diff(&path, oid)
                .map(|d| Arc::new(DiffDoc::new(d)))
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |v, cx| {
                if v.selected.is_none_or(|(_, o)| o != oid) {
                    return; // another row was selected meanwhile
                }
                match result {
                    Ok(d) => {
                        v.details = Some((Some(oid), d));
                        v.details_scroll = UniformListScrollHandle::new();
                    }
                    Err(e) => v.status = Some(e.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Selects stash `k`: its diff against the commit it was made on
    /// (untracked files it holds are not shown).
    fn select_stash(&mut self, k: usize, cx: &mut Context<Self>) {
        let Some(oid) = self.placed().get(k).map(|s| s.entry.oid) else {
            return;
        };
        self.selected = None;
        self.wip_selected = false;
        self.stash_selected = Some(oid);
        let diffs = self.core.diff_reader();
        let path = self.path.clone();
        let task = cx.background_spawn(async move {
            diffs
                .commit_diff(&path, oid)
                .map(|d| Arc::new(DiffDoc::new(d)))
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |v, cx| {
                if v.stash_selected != Some(oid) {
                    return; // another row was selected meanwhile
                }
                match result {
                    Ok(d) => {
                        v.details = Some((Some(oid), d));
                        v.details_scroll = UniformListScrollHandle::new();
                    }
                    Err(e) => v.status = Some(e.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Applies, pops (`Some`) or drops (`None`) the stash with commit id
    /// `oid`. Its index is looked up when the job runs: another stash may
    /// have been pushed since the list was read.
    fn stash_op(&mut self, oid: Oid, mode: Option<StashApplyMode>, cx: &mut Context<Self>) {
        let label = match mode {
            Some(StashApplyMode::Pop) => "Pop stash",
            Some(_) => "Apply stash",
            None => "Drop stash",
        };
        self.run(label, cx, move |a, p| {
            let index = a
                .ports()
                .stashes
                .stashes(p)?
                .into_iter()
                .find(|s| s.oid == oid)
                .map(|s| s.index)
                .ok_or_else(|| archipel_ops::OpError::Refused("the stash is gone".into()))?;
            match mode {
                Some(mode) => a.stash_apply(p, index, mode),
                None => a.stash_drop(p, index),
            }
        });
    }

    fn select_file(&mut self, file: String, side: DiffSide, cx: &mut Context<Self>) {
        let diffs = self.core.diff_reader();
        let path = self.path.clone();
        let task = {
            let file = file.clone();
            cx.background_spawn(async move {
                diffs
                    .worktree_diff(&path, side, Some(&file))
                    .map(|d| Arc::new(DiffDoc::new(d)))
            })
        };
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |v, cx| {
                match result {
                    Ok(d) => v.file_diff = Some((file, side, d)),
                    Err(e) => v.status = Some(e.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Selects graph row `row` and scrolls it into view.
    fn show_row(&mut self, row: u32, oid: Oid, strategy: ScrollStrategy, cx: &mut Context<Self>) {
        let item = self.item_of_row(row);
        self.scroll
            .scroll_to_item(usize::try_from(item).unwrap_or(0), strategy);
        self.select_commit(row, oid, cx);
    }

    /// Moves the graph selection by `delta` rows (keyboard navigation).
    /// Rows are counted in list items, uncommitted changes and stashes included.
    fn step(&mut self, delta: i64, cx: &mut Context<Self>) {
        let Some(graph) = &self.graph else {
            return;
        };
        let last = i64::from(self.item_count()) - 1;
        if last < 0 {
            return;
        }
        let from = if self.wip_selected {
            Some(0)
        } else if let Some(oid) = self.stash_selected {
            self.stash_at(oid).map(|k| self.item_of_stash(k))
        } else {
            self.selected.map(|(r, _)| self.item_of_row(r))
        };
        let item = (from.map_or(-1, i64::from) + delta).clamp(0, last);
        let item = u32::try_from(item).unwrap_or(0);
        match self.item_at(item) {
            Item::Wip => {
                self.scroll.scroll_to_item(0, ScrollStrategy::Nearest);
                self.select_wip(cx);
            }
            Item::Stash(k) => {
                self.scroll
                    .scroll_to_item(usize::try_from(item).unwrap_or(0), ScrollStrategy::Nearest);
                self.select_stash(k, cx);
            }
            Item::Commit(row) => {
                let oid = lock(graph)
                    .oids(row..row + 1)
                    .ok()
                    .and_then(|o| o.first().copied());
                if let Some(oid) = oid {
                    self.show_row(row, oid, ScrollStrategy::Nearest, cx);
                }
            }
        }
    }

    fn on_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let m = &event.keystroke.modifiers;
        if m.control || m.alt || m.platform || m.shift {
            return;
        }
        match event.keystroke.key.as_str() {
            "down" => self.step(1, cx),
            "up" => self.step(-1, cx),
            "pagedown" => self.step(PAGE_ROWS, cx),
            "pageup" => self.step(-PAGE_ROWS, cx),
            "home" => self.step(i64::MIN / 2, cx),
            "end" => self.step(i64::MAX / 2, cx),
            "enter" if self.wip_selected => self.open_changes(cx),
            "enter" if self.stash_selected.is_some() => {
                if let Some(oid) = self.stash_selected {
                    self.stash_op(oid, Some(StashApplyMode::Keep), cx);
                }
            }
            "enter" => match self.selected {
                Some((row, _)) => self.checkout_row(row, cx),
                None => return,
            },
            _ => return,
        }
        cx.stop_propagation();
    }

    /// Selects the commit `oid` and scrolls to it (refs panel). The row is
    /// found once the topology is complete, without holding the view lock.
    fn jump_to(&mut self, oid: Oid, cx: &mut Context<Self>) {
        let Some(graph) = self.graph.clone() else {
            return;
        };
        let task = cx.background_spawn(async move {
            let completion = lock(&graph).completion();
            if let Some(c) = completion {
                c.wait(&NoProgress)?;
            }
            let searcher = lock(&graph).searcher()?;
            searcher.row_of(oid, &CancelToken::new())
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |v, cx| match result {
                Ok(Some(row)) => v.show_row(row, oid, ScrollStrategy::Center, cx),
                Ok(None) => {
                    v.status = Some(format!("{} is not in the history shown", oid.short(8)));
                    cx.notify();
                }
                Err(e) => {
                    v.status = Some(e.to_string());
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Switches to the Changes tab (double click / Enter on the
    /// uncommitted-changes row).
    fn open_changes(&mut self, cx: &mut Context<Self>) {
        self.mode = Mode::Changes;
        self.load_changes(cx);
        cx.notify();
    }

    fn has_local(&self, branch: &str) -> bool {
        self.refs.locals.contains(branch)
    }

    /// Checks out a ref (double click on a badge or in the refs panel).
    fn checkout_ref(&mut self, tip: &RefTip, cx: &mut Context<Self>) {
        if let Some(target) = CheckoutTarget::for_ref(tip, |b| self.has_local(b)) {
            self.checkout(target, cx);
        }
    }

    /// Checks out the commit of graph row `row`: through a branch pointing
    /// at it when there is one, detached otherwise.
    fn checkout_row(&mut self, row: u32, cx: &mut Context<Self>) {
        let Some(graph_row) = self
            .graph
            .as_ref()
            .and_then(|g| lock(g).rows(row..row + 1).ok())
            .and_then(|mut r| r.pop())
        else {
            return;
        };
        let target =
            CheckoutTarget::for_commit(graph_row.oid, &graph_row.refs, |b| self.has_local(b));
        self.checkout(target, cx);
    }

    fn checkout(&mut self, target: CheckoutTarget, cx: &mut Context<Self>) {
        let label = match &target {
            CheckoutTarget::Branch(b) => format!("Checkout {b}"),
            CheckoutTarget::TrackRemote(r) => format!("Checkout {r}"),
            CheckoutTarget::Detached(oid) => format!("Checkout {} (detached)", oid.short(8)),
        };
        self.run(label, cx, move |a, p| a.checkout(p, &target, None));
    }

    /// Runs a write on a background thread, then reloads.
    fn run(
        &mut self,
        label: impl Into<String>,
        cx: &mut Context<Self>,
        job: impl FnOnce(&archipel_ops::RepoActions, &RepoPath) -> Result<(), archipel_ops::OpError>
        + Send
        + 'static,
    ) {
        let label = label.into();
        let actions = self.core.repo_actions().clone();
        let path = self.path.clone();
        let task = cx.background_spawn(async move { job(&actions, &path) });
        self.status = Some(format!("{label}…"));
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |v, cx| {
                v.status = Some(match result {
                    Ok(()) => format!("{label}: done"),
                    Err(e) => format!("{label} failed: {e}"),
                });
                v.reload(cx);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Pushes the current branch. A branch without upstream asks first
    /// on which remote to create it.
    fn push(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let ports = self.core.repo_actions().ports();
        let (status, remotes) = (ports.status.clone(), ports.remotes.clone());
        let path = self.path.clone();
        let task = cx.background_spawn(async move {
            let summary = status.summary(&path).map_err(|e| e.to_string())?;
            let branch = match (summary.head.branch(), summary.upstream) {
                (Some(b), None) => b.to_owned(),
                _ => return Ok(None),
            };
            let remotes = remotes.remotes(&path).map_err(|e| e.to_string())?;
            Ok::<_, String>(Some((branch, remotes)))
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |v, window, cx| match result {
                Ok(None) => v.run("Push", cx, |a, p| {
                    a.push(
                        p,
                        &PushRequest::default(),
                        &NoProgress,
                        &CancelToken::new(),
                        None,
                    )
                }),
                Ok(Some((branch, remotes))) if remotes.is_empty() => {
                    v.status = Some(format!(
                        "Push failed: {branch} has no upstream and the repository has no remote"
                    ));
                    cx.notify();
                }
                Ok(Some((branch, remotes))) => {
                    let remote = pick_remote(&remotes, None)
                        .and_then(|r| remotes.iter().position(|o| o.name == r.name))
                        .unwrap_or(0);
                    v.upstream_prompt = Some(UpstreamPrompt {
                        branch,
                        remotes: remotes.into_iter().map(|r| r.name).collect(),
                        remote,
                    });
                    v.prompt_focus.focus(window, cx);
                    cx.notify();
                }
                Err(e) => {
                    v.status = Some(format!("Push failed: {e}"));
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Pushes the prompted branch to the chosen remote and sets it as the
    /// upstream.
    fn push_set_upstream(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(prompt) = self.upstream_prompt.take() else {
            return;
        };
        self.focus.focus(window, cx);
        let remote = prompt.remotes[prompt.remote].clone();
        let request = PushRequest {
            remote: Some(remote.clone()),
            refspec: Some(prompt.branch.clone()),
            set_upstream: true,
            ..PushRequest::default()
        };
        self.run(
            format!("Push {} to {remote}", prompt.branch),
            cx,
            move |a, p| a.push(p, &request, &NoProgress, &CancelToken::new(), None),
        );
    }

    fn cancel_upstream_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.upstream_prompt = None;
        self.focus.focus(window, cx);
        cx.notify();
    }

    /// Opens or closes the new-branch form.
    fn toggle_branch_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.branching = !self.branching;
        if self.branching {
            self.branch_name.update(cx, |b, cx| b.focus(window, cx));
        } else {
            self.focus.focus(window, cx);
        }
        cx.notify();
    }

    /// Creates a branch at the selected commit (`HEAD` when none is) and
    /// checks it out.
    fn create_branch(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = self.branch_name.read(cx).value().trim().to_owned();
        if name.is_empty() {
            self.status = Some("Write a branch name first".into());
            cx.notify();
            return;
        }
        let start = self.selected.map(|(_, oid)| oid.to_hex());
        self.branch_name
            .update(cx, |b, cx| b.set_value("", window, cx));
        self.branching = false;
        self.focus.focus(window, cx);
        self.run(format!("Create branch {name}"), cx, move |a, p| {
            a.create_branch(p, &name, start.as_deref(), None)?;
            a.checkout(p, &CheckoutTarget::Branch(name), None)
        });
    }

    fn commit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let message = self.message.read(cx).value().to_string();
        if message.trim().is_empty() {
            self.status = Some("Write a commit message first".into());
            cx.notify();
            return;
        }
        let mode = if self.amend {
            CommitMode::Amend
        } else {
            CommitMode::New
        };
        self.message.update(cx, |m, cx| m.set_value("", window, cx));
        self.amend = false;
        self.run("Commit", cx, move |a, p| {
            a.commit(p, &CommitRequest { message, mode }, None)
                .map(drop)
        });
    }

    // ----- rendering ----------------------------------------------------

    fn render_toolbar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let tab = |id: &'static str, label: &'static str, mode: Mode, cx: &mut Context<Self>| {
            let active = self.mode == mode;
            div()
                .id(id)
                .px_2()
                .py_1()
                .cursor_pointer()
                .border_b_2()
                .border_color(if active {
                    color(cx, T::Accent)
                } else {
                    color(cx, T::BgApp)
                })
                .on_click(cx.listener(move |v, _, _, cx| {
                    v.mode = mode;
                    // Both tabs show the working copy.
                    v.load_changes(cx);
                    cx.notify();
                }))
                .child(label)
        };
        let changes_label = if self.changes.is_empty() {
            "Changes"
        } else {
            "Changes •"
        };
        div()
            .flex()
            .items_center()
            .justify_between()
            .px_3()
            .py_1()
            .border_b_1()
            .border_color(color(cx, T::BorderSubtle))
            .child(
                div()
                    .flex()
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .child(self.name.clone()),
                    )
                    .child(tab("m-history", "History", Mode::History, cx))
                    .child(tab("m-changes", changes_label, Mode::Changes, cx)),
            )
            .child(
                div()
                    .flex()
                    .gap_1()
                    .when(self.mode == Mode::History, |d| {
                        d.child(button("r-branch", "Branch", cx).on_click(
                            cx.listener(|v, _, window, cx| v.toggle_branch_form(window, cx)),
                        ))
                    })
                    .child(
                        button("r-fetch", "Fetch", cx).on_click(cx.listener(|v, _, _, cx| {
                            v.run("Fetch", cx, |a, p| {
                                a.fetch(
                                    p,
                                    &FetchRequest::default(),
                                    &NoProgress,
                                    &CancelToken::new(),
                                )
                            });
                        })),
                    )
                    .child(
                        button("r-pull", "Pull", cx).on_click(cx.listener(|v, _, _, cx| {
                            v.run("Pull", cx, |a, p| {
                                a.pull(
                                    p,
                                    PullMode::FastForwardOnly,
                                    &NoProgress,
                                    &CancelToken::new(),
                                    None,
                                )
                            });
                        })),
                    )
                    .child(
                        button("r-push", "Push", cx)
                            .on_click(cx.listener(|v, _, window, cx| v.push(window, cx))),
                    )
                    .child(
                        button("r-stash", "Stash", cx).on_click(cx.listener(|v, _, _, cx| {
                            v.run("Stash", cx, |a, p| {
                                a.stash_push(p, &StashRequest::default()).map(drop)
                            });
                        })),
                    )
                    .child(
                        button("r-pop", "Pop", cx).on_click(cx.listener(|v, _, _, cx| {
                            v.run("Pop stash", cx, |a, p| {
                                a.stash_apply(p, 0, StashApplyMode::Pop)
                            });
                        })),
                    )
                    .child(
                        button("r-undo", "Undo", cx).on_click(cx.listener(|v, _, _, cx| {
                            let _ = v.core.dispatch(archipel_app::Command::Undo(v.path.id()));
                            v.reload(cx);
                        })),
                    )
                    .child(
                        button("r-redo", "Redo", cx).on_click(cx.listener(|v, _, _, cx| {
                            let _ = v.core.dispatch(archipel_app::Command::Redo(v.path.id()));
                            v.reload(cx);
                        })),
                    ),
            )
    }

    /// The new-branch form: name, start point, Create / Cancel.
    fn render_branch_form(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let start = self
            .selected
            .map_or_else(|| "HEAD".to_owned(), |(_, oid)| oid.short(8));
        div()
            .flex()
            .items_center()
            .gap_2()
            .px_3()
            .py_1()
            .border_b_1()
            .border_color(color(cx, T::BorderSubtle))
            .on_key_down(cx.listener(|v, e: &KeyDownEvent, window, cx| {
                if e.keystroke.key == "escape" {
                    cx.stop_propagation();
                    v.toggle_branch_form(window, cx);
                }
            }))
            .child(
                div()
                    .flex_none()
                    .text_color(color(cx, T::FgMuted))
                    .child(format!("New branch at {start}")),
            )
            .child(div().w(px(260.0)).child(Input::new(&self.branch_name)))
            .child(
                primary_button("branch-create", "Create", cx)
                    .on_click(cx.listener(|v, _, window, cx| v.create_branch(window, cx))),
            )
            .child(
                button("branch-cancel", "Cancel", cx)
                    .on_click(cx.listener(|v, _, window, cx| v.toggle_branch_form(window, cx))),
            )
    }

    /// The dialog offering to create the upstream of a branch on push.
    fn render_upstream_prompt(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let prompt = self.upstream_prompt.as_ref()?;
        let remote = &prompt.remotes[prompt.remote];
        let remotes = prompt.remotes.iter().enumerate().map(|(i, name)| {
            chip(("up-remote", i), name.clone(), i == prompt.remote, cx).on_click(cx.listener(
                move |v, _, _, cx| {
                    if let Some(p) = &mut v.upstream_prompt {
                        p.remote = i;
                    }
                    cx.notify();
                },
            ))
        });
        let dialog = div()
            .id("upstream-prompt")
            .occlude()
            .track_focus(&self.prompt_focus)
            .on_key_down(cx.listener(|v, e: &KeyDownEvent, window, cx| {
                match e.keystroke.key.as_str() {
                    "escape" => v.cancel_upstream_prompt(window, cx),
                    "enter" => v.push_set_upstream(window, cx),
                    _ => return,
                }
                cx.stop_propagation();
            }))
            .w(px(440.0))
            .flex()
            .flex_col()
            .gap_3()
            .p_4()
            .bg(color(cx, T::BgElevated))
            .border_1()
            .border_color(color(cx, T::Border))
            .rounded(radius(cx))
            .shadow_lg()
            .child(
                div()
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .child("No upstream branch"),
            )
            .child(div().text_color(color(cx, T::FgMuted)).child(format!(
                "{} is not tracking a remote branch. Push it to {remote}/{} and track it?",
                prompt.branch, prompt.branch
            )))
            .when(prompt.remotes.len() > 1, |d| {
                d.child(div().flex().flex_wrap().gap_1().children(remotes))
            })
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .child(button("up-cancel", "Cancel", cx).on_click(
                        cx.listener(|v, _, window, cx| v.cancel_upstream_prompt(window, cx)),
                    ))
                    .child(
                        primary_button("up-push", "Push and set upstream", cx).on_click(
                            cx.listener(|v, _, window, cx| v.push_set_upstream(window, cx)),
                        ),
                    ),
            );
        Some(
            div()
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .occlude()
                .bg(color(cx, T::BgApp).opacity(0.6))
                .child(dialog)
                .into_any_element(),
        )
    }

    fn render_graph(&mut self, cx: &mut Context<Self>) -> AnyElement {
        if let Some(e) = &self.graph_error {
            return div()
                .p_3()
                .text_color(color(cx, T::StatusError))
                .child(e.clone())
                .into_any_element();
        }
        if self.graph.is_none() {
            return div()
                .p_3()
                .text_color(color(cx, T::FgMuted))
                .child("Loading history…")
                .into_any_element();
        }
        let count = usize::try_from(self.item_count()).unwrap_or(0);
        uniform_list(
            "graph",
            count,
            cx.processor(|this: &mut Self, range: Range<usize>, _window, cx| {
                let items: Vec<Item> = (u32::try_from(range.start).unwrap_or(0)
                    ..u32::try_from(range.end).unwrap_or(0))
                    .map(|i| this.item_at(i))
                    .collect();
                // Graph rows to lay out: the commits shown, and the base
                // commits of the stashes shown (their lanes run through the
                // stash rows).
                let wanted = items.iter().filter_map(|item| match *item {
                    Item::Wip => None,
                    Item::Stash(k) => Some(this.placed()[k].row),
                    Item::Commit(r) => Some(r),
                });
                let start = wanted.clone().min().unwrap_or(0);
                let end = wanted.max().map_or(start, |r| r + 1);
                let rows = this
                    .graph
                    .as_ref()
                    .and_then(|g| lock(g).rows(start..end).ok())
                    .unwrap_or_default();
                if (start..end).any(|r| !this.meta.contains_key(&r)) {
                    let overscan =
                        u32::try_from(this.core.settings().graph_overscan).unwrap_or(u32::MAX / 4);
                    this.fetch_meta(start.saturating_sub(overscan)..end + overscan, cx);
                }
                let row_at = |r: u32| rows.get(usize::try_from(r - start).ok()?);
                let mut out = Vec::with_capacity(items.len());
                for item in items {
                    match item {
                        Item::Wip => {
                            // Sits above HEAD's lane when HEAD is on screen.
                            let column = rows
                                .iter()
                                .find(|r| r.refs.iter().any(|t| t.kind == RefKind::Head))
                                .map_or(0, |r| r.column);
                            out.push(this.render_wip_row(column, cx));
                        }
                        Item::Stash(k) => {
                            if let Some(base) = row_at(this.placed()[k].row) {
                                out.push(this.render_stash_row(k, base, cx));
                            }
                        }
                        Item::Commit(r) => {
                            if let Some(row) = row_at(r) {
                                let stashed = this.placed().iter().any(|s| s.row == r);
                                let lane = stashed.then(|| stash_lane(row));
                                out.push(this.render_row(row, lane, cx));
                            }
                        }
                    }
                }
                out
            }),
        )
        .track_scroll(&self.scroll)
        .flex_1()
        .into_any_element()
    }

    /// Branches and tags: click jumps to the commit, double click checks out.
    fn render_refs(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let refs = Arc::clone(&self.refs);
        uniform_list(
            "refs",
            refs.rows.len(),
            cx.processor(move |_this: &mut Self, range: Range<usize>, _window, cx| {
                range
                    .filter_map(|i| refs.rows.get(i).map(|r| (i, r)))
                    .map(|(i, row)| render_ref_row(i, row, cx))
                    .collect()
            }),
        )
        .flex_1()
        .into_any_element()
    }

    /// The uncommitted-changes row: a hollow node in HEAD's lane and the
    /// staged / unstaged counts. Click shows the diff, double click opens
    /// the Changes tab.
    fn render_wip_row(&mut self, column: u32, cx: &mut Context<Self>) -> AnyElement {
        let staged = self.changes.iter().filter(|c| c.is_staged()).count();
        let unstaged = self.changes.iter().filter(|c| c.is_unstaged()).count();
        let counts = match (staged, unstaged) {
            (0, u) => format!("{u} unstaged"),
            (s, 0) => format!("{s} staged"),
            (s, u) => format!("{s} staged, {u} unstaged"),
        };
        let ring = color(cx, T::StatusDirty);
        let node = canvas(
            |_, _, _| {},
            move |bounds: Bounds<Pixels>, (), window, _| {
                let scale = window.scale_factor();
                let snap = |v: Pixels| px((f32::from(v) * scale).round() / scale);
                let r = px(4.0);
                let center = point(
                    snap(bounds.origin.x + px(lane_x(column))),
                    snap(bounds.origin.y + bounds.size.height / 2.0),
                );
                window.paint_quad(
                    gpui_kit::fill(
                        Bounds::new(
                            point(center.x - r, center.y - r),
                            gpui_kit::size(r * 2.0, r * 2.0),
                        ),
                        gpui_kit::transparent_black(),
                    )
                    .corner_radii(r)
                    .border_widths(px(1.5))
                    .border_color(ring),
                );
            },
        )
        .size_full();
        let hover = color(cx, T::BgHover);
        div()
            .id("g-wip")
            .w_full()
            .h(row_height(cx))
            .flex()
            .items_center()
            .gap_2()
            .pr_3()
            .cursor_pointer()
            .when(self.wip_selected, |d| d.bg(color(cx, T::BgSelected)))
            .hover(move |s| s.bg(hover))
            .on_click(cx.listener(move |v, e: &ClickEvent, window, cx| {
                v.focus.focus(window, cx);
                if e.click_count() > 1 {
                    v.open_changes(cx);
                } else {
                    v.select_wip(cx);
                }
            }))
            .child(
                div()
                    .w(px(lane_x(self.lane_count())))
                    .h_full()
                    .flex_none()
                    .overflow_hidden()
                    .child(node),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .truncate()
                    .italic()
                    .text_color(color(cx, T::FgMuted))
                    .child("Uncommitted changes"),
            )
            .child(
                div()
                    .flex_none()
                    .text_xs()
                    .text_color(color(cx, T::StatusDirty))
                    .child(counts),
            )
            .into_any_element()
    }

    /// A commit row. `stash_lane`: the lane of the stashes drawn above it,
    /// which joins its node.
    fn render_row(
        &mut self,
        row: &GraphRow,
        stash_lane: Option<u32>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let meta = self.meta.get(&row.row).cloned();
        let selected = self.selected.is_some_and(|(r, _)| r == row.row);
        // Same width on every row so the text columns line up.
        let width = px(lane_x(self.lane_count()));
        let lanes = graph_cell(row, stash_lane, cx);
        let (summary, author, date) = meta.map_or_else(
            || (String::new(), String::new(), String::new()),
            |m| {
                (
                    m.summary.clone(),
                    m.author_name.clone(),
                    archipel_app::dashboard::relative_time(m.commit_time, Timestamp::now()),
                )
            },
        );
        let oid = row.oid;
        let index = row.row;
        let badges = row.refs.iter().enumerate().map(|(i, r)| {
            let tip = r.clone();
            div()
                .id(SharedString::from(format!("badge-{index}-{i}")))
                .px_1()
                .rounded(radius(cx))
                .text_xs()
                .bg(color(cx, T::AccentSubtle))
                .text_color(color(cx, T::Accent))
                .when(r.kind == RefKind::Head, |d| {
                    d.font_weight(gpui_kit::FontWeight::SEMIBOLD)
                })
                // A single click bubbles to the row (select); a double click
                // checks out this ref rather than the row's preferred one.
                .on_click(cx.listener(move |v, e: &ClickEvent, _, cx| {
                    if e.click_count() > 1 {
                        cx.stop_propagation();
                        v.checkout_ref(&tip, cx);
                    }
                }))
                .child(r.name.clone())
        });
        let hover = color(cx, T::BgHover);
        div()
            .id(SharedString::from(format!("g-{index}")))
            .w_full()
            .h(row_height(cx))
            .flex()
            .items_center()
            .gap_2()
            .pr_3()
            .cursor_pointer()
            .when(selected, |d| d.bg(color(cx, T::BgSelected)))
            .hover(move |s| s.bg(hover))
            .on_click(cx.listener(move |v, e: &ClickEvent, window, cx| {
                v.focus.focus(window, cx);
                if e.click_count() > 1 {
                    v.checkout_row(index, cx);
                } else {
                    v.select_commit(index, oid, cx);
                }
            }))
            .child(
                div()
                    .w(width)
                    .h_full()
                    .flex_none()
                    .overflow_hidden()
                    .child(lanes),
            )
            .child(
                div()
                    .flex()
                    .gap_1()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .children(badges),
            )
            .child(div().flex_1().min_w(px(0.0)).truncate().child(summary))
            .child(
                div()
                    .w(px(140.0))
                    .flex_none()
                    .truncate()
                    .text_color(color(cx, T::FgMuted))
                    .child(author),
            )
            .child(
                div()
                    .w(px(80.0))
                    .flex_none()
                    .truncate()
                    .text_xs()
                    .text_color(color(cx, T::FgSubtle))
                    .child(date),
            )
            .child(
                div()
                    .w(px(70.0))
                    .text_xs()
                    .font_family(mono_font(cx))
                    .text_color(color(cx, T::FgSubtle))
                    .child(oid.short(8)),
            )
            .into_any_element()
    }

    /// Stash `k`, in a row of its own above `base`, the commit it was made
    /// on: a hollow square on the stash lane, linked to the stashes of the
    /// same commit and down to it. Click shows its diff, double click
    /// applies it.
    fn render_stash_row(
        &mut self,
        k: usize,
        base: &GraphRow,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let stash = &self.placed()[k];
        let oid = stash.entry.oid;
        // The first stash of its commit starts the stash lane.
        let linked = k > 0 && self.placed()[k - 1].row == stash.row;
        let cell = stash_cell(base, stash_lane(base), linked, cx);
        let name = format!("stash@{{{}}}", stash.entry.index);
        let title = stash_title(&stash.entry.message).to_owned();
        let date = archipel_app::dashboard::relative_time(stash.entry.time, Timestamp::now());
        let selected = self.stash_selected == Some(oid);
        let tint = color(cx, T::StatusDirty);
        let hover = color(cx, T::BgHover);
        div()
            .id(SharedString::from(format!("g-stash-{oid}")))
            .w_full()
            .h(row_height(cx))
            .flex()
            .items_center()
            .gap_2()
            .pr_3()
            .cursor_pointer()
            .when(selected, |d| d.bg(color(cx, T::BgSelected)))
            .hover(move |s| s.bg(hover))
            .on_click(cx.listener(move |v, e: &ClickEvent, window, cx| {
                v.focus.focus(window, cx);
                if e.click_count() > 1 {
                    v.stash_op(oid, Some(StashApplyMode::Keep), cx);
                } else if let Some(k) = v.stash_at(oid) {
                    v.select_stash(k, cx);
                }
            }))
            .child(
                div()
                    .w(px(lane_x(self.lane_count())))
                    .h_full()
                    .flex_none()
                    .overflow_hidden()
                    .child(cell),
            )
            .child(
                div()
                    .flex_none()
                    .px_1()
                    .rounded(radius(cx))
                    .text_xs()
                    .border_1()
                    .border_color(tint)
                    .text_color(tint)
                    .child(name),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .truncate()
                    .italic()
                    .text_color(color(cx, T::FgMuted))
                    .child(title),
            )
            .child(
                div()
                    .w(px(80.0))
                    .flex_none()
                    .truncate()
                    .text_xs()
                    .text_color(color(cx, T::FgSubtle))
                    .child(date),
            )
            .child(
                div()
                    .w(px(70.0))
                    .text_xs()
                    .font_family(mono_font(cx))
                    .text_color(color(cx, T::FgSubtle))
                    .child(oid.short(8)),
            )
            .into_any_element()
    }

    fn render_details(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let Some((oid, doc)) = &self.details else {
            return div()
                .p_3()
                .text_color(color(cx, T::FgMuted))
                .child("Select a commit")
                .into_any_element();
        };
        let (oid, doc) = (*oid, doc.clone());
        let Some(oid) = oid else {
            return self.render_wip_details(doc, cx);
        };
        if self.stash_selected == Some(oid) {
            if let Some(k) = self.stash_at(oid) {
                return self.render_stash_details(k, doc, cx);
            }
        }
        let meta = self.selected.and_then(|(r, _)| self.meta.get(&r).cloned());
        let mut el = div().p_2().flex().flex_col().gap_1();
        if let Some(m) = meta {
            el = el
                .child(
                    div()
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .child(m.summary.clone()),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(color(cx, T::FgMuted))
                        .child(format!("{} <{}>", m.author_name, m.author_email)),
                );
        }
        el = el.child(
            div()
                .text_xs()
                .font_family(mono_font(cx))
                .text_color(color(cx, T::FgSubtle))
                .child(oid.to_hex()),
        );
        self.render_diff_details(el, &oid.to_hex(), doc, cx)
    }

    /// Details of the uncommitted-changes row: staged diff, then unstaged.
    fn render_wip_details(&mut self, doc: Arc<DiffDoc>, cx: &mut Context<Self>) -> AnyElement {
        let header = div()
            .p_2()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .child("Uncommitted changes"),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(color(cx, T::FgMuted))
                    .child("Staged changes first, then unstaged ones"),
            );
        self.render_diff_details(header, "wip", doc, cx)
    }

    /// Details of a stash: its message, the commit it was made on, and
    /// Apply / Pop / Drop.
    fn render_stash_details(
        &mut self,
        k: usize,
        doc: Arc<DiffDoc>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let entry = self.placed()[k].entry.clone();
        let oid = entry.oid;
        let op = |id: &'static str, label: &'static str, mode, cx: &mut Context<Self>| {
            button(id, label, cx)
                .on_click(cx.listener(move |v, _, _, cx| v.stash_op(oid, mode, cx)))
        };
        let header = div()
            .p_2()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .child(format!(
                        "stash@{{{}}}: {}",
                        entry.index,
                        stash_title(&entry.message)
                    )),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(color(cx, T::FgMuted))
                    .child(format!(
                        "Made on {}, {}",
                        entry.base.short(8),
                        archipel_app::dashboard::relative_time(entry.time, Timestamp::now())
                    )),
            )
            .child(
                div()
                    .flex()
                    .gap_1()
                    .child(op("s-apply", "Apply", Some(StashApplyMode::Keep), cx))
                    .child(op("s-pop", "Pop", Some(StashApplyMode::Pop), cx))
                    .child(op("s-drop", "Drop", None, cx)),
            );
        self.render_diff_details(header, &format!("stash-{oid}"), doc, cx)
    }

    /// A details header, the changed-files list and the diff below it.
    fn render_diff_details(
        &mut self,
        header: gpui_kit::Div,
        key: &str,
        doc: Arc<DiffDoc>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let files = Self::render_changed_files(key, &doc, cx);
        div()
            .flex_1()
            .flex()
            .flex_col()
            .overflow_hidden()
            .child(header)
            .child(files)
            .child(diff_list(
                SharedString::from(format!("details-{key}")),
                doc,
                None,
                Some(&self.details_scroll),
                cx,
            ))
            .into_any_element()
    }

    /// The files a commit changes; a click scrolls the diff to the file.
    /// Virtualized and capped in height, so a huge commit leaves room for
    /// the diff.
    fn render_changed_files(
        key: &str,
        doc: &Arc<DiffDoc>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        const MAX_ROWS_SHOWN: usize = 8;
        let count = doc.files.len();
        let shown = u16::try_from(count.clamp(1, MAX_ROWS_SHOWN)).unwrap_or(1);
        let doc = Arc::clone(doc);
        let list = uniform_list(
            SharedString::from(format!("details-files-{key}")),
            count,
            cx.processor(move |_this: &mut Self, range: Range<usize>, _window, cx| {
                range
                    .filter(|&i| i < doc.files.len())
                    .map(|i| render_changed_file(&doc, i, cx))
                    .collect()
            }),
        )
        .h(row_height(cx) * f32::from(shown));
        div()
            .flex_none()
            .flex()
            .flex_col()
            .border_t_1()
            .border_b_1()
            .border_color(color(cx, T::BorderSubtle))
            .child(section(format!("FILES ({count})"), cx).pt_1())
            .when(count == 0, |d| {
                d.child(
                    div()
                        .px_2()
                        .pb_1()
                        .text_color(color(cx, T::FgMuted))
                        .child("No changes"),
                )
            })
            .when(count > 0, |d| d.child(list))
    }

    /// Resizes a pane while its splitter is dragged.
    fn on_pane_drag(
        &mut self,
        e: &DragMoveEvent<PaneDrag>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Every row listening for `PaneDrag` sees every splitter's drag,
        // the workspace sidebar's included.
        let pane = e.drag(cx).0;
        if pane == Pane::Sidebar {
            return;
        }
        let (x, bounds) = (e.event.position.x, e.bounds);
        self.core
            .update_ui_state(|ui| resize_pane(ui, pane, x, bounds));
        cx.notify();
    }

    fn render_changes(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let ui = self.core.ui_state().unwrap_or_default();
        let hover = color(cx, T::BgHover);
        let mut staged = Vec::new();
        let mut unstaged = Vec::new();
        for c in &self.changes {
            if c.is_staged() {
                staged.push(c.clone());
            }
            if c.is_unstaged() {
                unstaged.push(c.clone());
            }
        }
        let file_row = |c: &FileChange, side: DiffSide, cx: &mut Context<Self>| {
            let path = c.path.clone();
            let (label, token) = match c.state {
                FileState::Untracked => ("+", T::StatusAhead),
                FileState::Conflicted => ("!", T::StatusConflict),
                FileState::Tracked { .. } => ("✎", T::StatusDirty),
            };
            let (p1, p2, p3) = (path.clone(), path.clone(), path.clone());
            let action = if side == DiffSide::Staged {
                "Unstage"
            } else {
                "Stage"
            };
            div()
                .id(SharedString::from(format!("f-{side:?}-{path}")))
                .flex()
                .items_center()
                .gap_2()
                .px_2()
                .h(row_height(cx))
                .cursor_pointer()
                .hover(move |s| s.bg(hover))
                .on_click(cx.listener(move |v, _, _, cx| v.select_file(p1.clone(), side, cx)))
                .child(
                    div()
                        .w(px(14.0))
                        .flex_none()
                        .text_color(color(cx, token))
                        .child(label),
                )
                .child(div().flex_1().min_w_0().truncate().child(path))
                .when(side == DiffSide::Unstaged, |d| {
                    d.child(
                        button(SharedString::from(format!("discard-{p3}")), "Discard", cx)
                            .on_click(cx.listener(move |v, _, _, cx| {
                                let file = p3.clone();
                                v.run("Discard", cx, move |a, p| a.discard(p, &[file]));
                            })),
                    )
                })
                .child(
                    button(
                        SharedString::from(format!("stage-{side:?}-{p2}")),
                        action,
                        cx,
                    )
                    .on_click(cx.listener(move |v, _, _, cx| {
                        let file = p2.clone();
                        if side == DiffSide::Staged {
                            v.run("Unstage", cx, move |a, p| a.unstage(p, &[file]));
                        } else {
                            v.run("Stage", cx, move |a, p| a.stage(p, &[file]));
                        }
                    })),
                )
        };
        let staged_rows: Vec<_> = staged
            .iter()
            .map(|c| file_row(c, DiffSide::Staged, cx))
            .collect();
        let unstaged_rows: Vec<_> = unstaged
            .iter()
            .map(|c| file_row(c, DiffSide::Unstaged, cx))
            .collect();
        let unstaged_paths: Vec<String> = unstaged.iter().map(|c| c.path.clone()).collect();
        let staged_paths: Vec<String> = staged.iter().map(|c| c.path.clone()).collect();
        let discard_paths = unstaged_paths.clone();
        let unstaged_header = div()
            .flex()
            .items_center()
            .justify_between()
            .pr_2()
            .child(section(format!("UNSTAGED ({})", unstaged.len()), cx))
            .when(!unstaged_paths.is_empty(), |d| {
                d.child(
                    div()
                        .flex()
                        .gap_1()
                        .pt_2()
                        .child(
                            button("discard-all", "Discard all", cx).on_click(cx.listener(
                                move |v, _, _, cx| {
                                    let files = discard_paths.clone();
                                    v.run("Discard all", cx, move |a, p| a.discard(p, &files));
                                },
                            )),
                        )
                        .child(button("stage-all", "Stage all", cx).on_click(cx.listener(
                            move |v, _, _, cx| {
                                let files = unstaged_paths.clone();
                                v.run("Stage all", cx, move |a, p| a.stage(p, &files));
                            },
                        ))),
                )
            });
        let staged_header = div()
            .flex()
            .items_center()
            .justify_between()
            .pr_2()
            .child(section(format!("STAGED ({})", staged.len()), cx))
            .when(!staged_paths.is_empty(), |d| {
                d.child(
                    div()
                        .pt_2()
                        .child(
                            button("unstage-all", "Unstage all", cx).on_click(cx.listener(
                                move |v, _, _, cx| {
                                    let files = staged_paths.clone();
                                    v.run("Unstage all", cx, move |a, p| a.unstage(p, &files));
                                },
                            )),
                        ),
                )
            });
        let amend = self.amend;
        let left = div()
            .w(px(ui.changes_width))
            .flex_none()
            .flex()
            .flex_col()
            .child(unstaged_header)
            .child(
                div()
                    .id("unstaged")
                    .flex_1()
                    .overflow_y_scroll()
                    .children(unstaged_rows),
            )
            .child(staged_header)
            .child(
                div()
                    .id("staged")
                    .flex_1()
                    .overflow_y_scroll()
                    .children(staged_rows),
            )
            .child(
                div()
                    .p_2()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .border_t_1()
                    .border_color(color(cx, T::BorderSubtle))
                    .child(Input::new(&self.message))
                    .child(
                        div()
                            .flex()
                            .justify_between()
                            .child(
                                div()
                                    .id("amend")
                                    .cursor_pointer()
                                    .text_color(color(
                                        cx,
                                        if amend { T::Accent } else { T::FgMuted },
                                    ))
                                    .on_click(cx.listener(|v, _, _, cx| {
                                        v.amend = !v.amend;
                                        cx.notify();
                                    }))
                                    .child(if amend {
                                        "☑ Amend last commit"
                                    } else {
                                        "☐ Amend last commit"
                                    }),
                            )
                            .child(
                                primary_button("commit", "Commit", cx)
                                    .on_click(cx.listener(|v, _, w, cx| v.commit(w, cx))),
                            ),
                    ),
            );
        let right: AnyElement = match &self.file_diff {
            None => div()
                .p_3()
                .text_color(color(cx, T::FgMuted))
                .child("Select a file")
                .into_any_element(),
            Some((file, side, doc)) => {
                let side_tag = if *side == DiffSide::Staged { "s" } else { "u" };
                let id = SharedString::from(format!("file-diff-{side_tag}-{file}"));
                let (side, doc) = (*side, doc.clone());
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .flex()
                    .flex_col()
                    .overflow_hidden()
                    .p_2()
                    .child(diff_list(id, doc, Some(side), None, cx))
                    .into_any_element()
            }
        };
        div()
            .flex_1()
            .flex()
            .overflow_hidden()
            .on_drag_move(cx.listener(Self::on_pane_drag))
            .child(left)
            .child(splitter(Pane::Changes, cx))
            .child(right)
            .into_any_element()
    }
}

/// Horizontal center of lane `col` (lanes are a few dozen at most).
#[allow(clippy::cast_precision_loss)]
fn lane_x(col: u32) -> f32 {
    LANE_WIDTH * col as f32 + LANE_WIDTH / 2.0
}

/// One row of the refs panel.
fn render_ref_row(i: usize, row: &RefRow, cx: &mut Context<RepoView>) -> AnyElement {
    let base = div()
        .h(row_height(cx))
        .flex()
        .items_center()
        .px_2()
        .whitespace_nowrap()
        .overflow_hidden();
    match row {
        RefRow::Header(title, count) => base
            .text_xs()
            .text_color(color(cx, T::FgSubtle))
            .child(format!("{title} ({count})"))
            .into_any_element(),
        RefRow::Tip { tip, current } => {
            let hover = color(cx, T::BgHover);
            let name = tip.name.clone();
            let tip = tip.clone();
            base.id(SharedString::from(format!("ref-{i}")))
                .pl_4()
                .cursor_pointer()
                .hover(move |s| s.bg(hover))
                .when(*current, |d| {
                    d.text_color(color(cx, T::Accent))
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                })
                .on_click(cx.listener(move |v, e: &ClickEvent, window, cx| {
                    if e.click_count() > 1 {
                        v.checkout_ref(&tip, cx);
                    } else {
                        v.focus.focus(window, cx);
                        v.jump_to(tip.target, cx);
                    }
                }))
                .child(div().truncate().child(if *current {
                    format!("● {name}")
                } else {
                    name
                }))
                .into_any_element()
        }
    }
}

/// Message of a stash without the `On <branch>: ` Git prefixes to the
/// message given at push time.
fn stash_title(message: &str) -> &str {
    message
        .strip_prefix("On ")
        .and_then(|m| m.split_once(": "))
        .map_or(message, |(_, m)| m)
}

/// Lanes crossing the top edge of `row`; they cross the stash rows drawn
/// above it too.
fn lanes_above(row: &GraphRow) -> impl Iterator<Item = &Edge> {
    row.edges.iter().filter(|e| e.kind != EdgeKind::Outgoing)
}

/// Lane of the stashes drawn above `base`: the first one from the base
/// commit's that no lane crosses, so a stash on a branch tip sits right
/// above it.
fn stash_lane(base: &GraphRow) -> u32 {
    // n lanes leave a free one among n + 1 columns.
    let n = u32::try_from(base.edges.len()).unwrap_or(u32::MAX);
    (base.column..=base.column.saturating_add(n))
        .find(|c| !lanes_above(base).any(|e| e.from == *c))
        .unwrap_or(base.column)
}

fn stroke(window: &mut Window, a: Point<Pixels>, b: Point<Pixels>, color: Hsla) {
    let mut path = PathBuilder::stroke(px(1.6));
    path.move_to(a);
    if a.x == b.x {
        path.line_to(b);
    } else {
        path.curve_to(b, point(b.x, a.y));
    }
    if let Ok(p) = path.build() {
        window.paint_path(p, color);
    }
}

/// Paints a stash row above `base`: the lanes passing by, and the stash
/// node on lane `column`, linked to the stash above it when `linked` and
/// down toward the base commit.
fn stash_cell(base: &GraphRow, column: u32, linked: bool, cx: &App) -> impl IntoElement {
    let lanes: Vec<(u32, Hsla)> = lanes_above(base)
        .map(|e| (e.from, lane(cx, e.color)))
        .collect();
    let tint = color(cx, T::StatusDirty);
    canvas(
        |_, _, _| {},
        move |bounds: Bounds<Pixels>, (), window, _| {
            let scale = window.scale_factor();
            let snap = |v: Pixels| px((f32::from(v) * scale).round() / scale);
            let x = |col: u32| snap(bounds.origin.x + px(lane_x(col)));
            let (top, bottom) = (bounds.origin.y, bounds.origin.y + bounds.size.height);
            let mid = snap(bounds.origin.y + bounds.size.height / 2.0);
            for (col, c) in &lanes {
                stroke(window, point(x(*col), top), point(x(*col), bottom), *c);
            }
            // The node is hollow: the stash lane stops at its edge.
            let r = px(4.0);
            let cx = x(column);
            if linked {
                stroke(window, point(cx, top), point(cx, mid - r), tint);
            }
            stroke(window, point(cx, mid + r), point(cx, bottom), tint);
            window.paint_quad(
                gpui_kit::fill(
                    Bounds::new(point(cx - r, mid - r), gpui_kit::size(r * 2.0, r * 2.0)),
                    gpui_kit::transparent_black(),
                )
                .corner_radii(px(1.5))
                .border_widths(px(1.5))
                .border_color(tint),
            );
        },
    )
    .size_full()
}

/// Paints the lanes and the commit node of one graph row, and the end of
/// the stash lane (`stash_lane`) when stashes are drawn above it.
fn graph_cell(row: &GraphRow, stash_lane: Option<u32>, cx: &App) -> impl IntoElement {
    let mut edges: Vec<(EdgeKind, u32, u32, Hsla)> = row
        .edges
        .iter()
        .map(|e| (e.kind, e.from, e.to, lane(cx, e.color)))
        .collect();
    if let Some(col) = stash_lane {
        edges.push((
            EdgeKind::Incoming,
            col,
            row.column,
            color(cx, T::StatusDirty),
        ));
    }
    let node = (row.column, lane(cx, row.color));
    // The checked-out commit gets a ring around its node.
    let head_ring = row
        .refs
        .iter()
        .any(|r| r.kind == RefKind::Head)
        .then(|| color(cx, T::FgDefault));
    canvas(
        |_, _, _| {},
        move |bounds: Bounds<Pixels>, (), window, _| {
            // Quads snap to device pixels but paths do not: snapping the
            // centers here keeps the node on its lane line.
            let scale = window.scale_factor();
            let snap = |v: Pixels| px((f32::from(v) * scale).round() / scale);
            let x = |col: u32| snap(bounds.origin.x + px(lane_x(col)));
            let (top, bottom) = (bounds.origin.y, bounds.origin.y + bounds.size.height);
            let mid = snap(bounds.origin.y + bounds.size.height / 2.0);
            for (kind, from, to, c) in &edges {
                let (a, b) = match kind {
                    EdgeKind::Through => (point(x(*from), top), point(x(*to), bottom)),
                    EdgeKind::Incoming => (point(x(*from), top), point(x(*to), mid)),
                    EdgeKind::Outgoing => (point(x(*from), mid), point(x(*to), bottom)),
                };
                stroke(window, a, b, *c);
            }
            let r = px(4.0);
            let center = point(x(node.0), mid);
            window.paint_quad(
                gpui_kit::fill(
                    Bounds::new(
                        point(center.x - r, center.y - r),
                        gpui_kit::size(r * 2.0, r * 2.0),
                    ),
                    node.1,
                )
                .corner_radii(r),
            );
            if let Some(ring) = head_ring {
                // An even diameter keeps the snapped ring centered on the node.
                let r = px(LANE_WIDTH / 2.0);
                window.paint_quad(
                    gpui_kit::fill(
                        Bounds::new(
                            point(center.x - r, center.y - r),
                            gpui_kit::size(r * 2.0, r * 2.0),
                        ),
                        gpui_kit::transparent_black(),
                    )
                    .corner_radii(r)
                    .border_widths(px(1.5))
                    .border_color(ring),
                );
            }
        },
    )
    .size_full()
}

/// One row of the changed-files list: change kind, path, line counts.
fn render_changed_file(doc: &Arc<DiffDoc>, i: usize, cx: &mut Context<RepoView>) -> AnyElement {
    let file = &doc.files[i];
    let (label, token) = match file.kind {
        ChangeKind::Added => ("A", T::DiffAddFg),
        ChangeKind::Deleted => ("D", T::DiffDelFg),
        ChangeKind::Renamed => ("R", T::StatusDirty),
        ChangeKind::Copied => ("C", T::StatusDirty),
        ChangeKind::TypeChanged => ("T", T::StatusDirty),
        ChangeKind::Modified | ChangeKind::Unmodified => ("M", T::StatusDirty),
    };
    let path = match (&file.old_path, &file.new_path) {
        (Some(old), Some(new)) if old != new => format!("{old} → {new}"),
        _ => file.display_path().to_owned(),
    };
    let stats = if file.binary {
        "binary".to_owned()
    } else {
        let (added, removed) = file.line_stats();
        format!("+{added} −{removed}")
    };
    let target = doc.file_rows[i];
    let hover = color(cx, T::BgHover);
    div()
        .id(SharedString::from(format!("cf-{i}")))
        .w_full()
        .h(row_height(cx))
        .flex()
        .items_center()
        .gap_2()
        .px_2()
        .cursor_pointer()
        .hover(move |s| s.bg(hover))
        .on_click(cx.listener(move |v, _, _, cx| {
            v.details_scroll.scroll_to_item(target, ScrollStrategy::Top);
            cx.notify();
        }))
        .child(
            div()
                .w(px(14.0))
                .flex_none()
                .text_color(color(cx, token))
                .child(label),
        )
        .child(div().flex_1().min_w_0().truncate().child(path))
        .child(
            div()
                .flex_none()
                .text_xs()
                .text_color(color(cx, T::FgMuted))
                .child(stats),
        )
        .into_any_element()
}

/// A virtualized diff: only the visible rows are built each frame. With
/// `side`, hunk headers get a stage / unstage button; with `scroll`, the
/// list can be scrolled from outside.
fn diff_list(
    id: SharedString,
    doc: Arc<DiffDoc>,
    side: Option<DiffSide>,
    scroll: Option<&UniformListScrollHandle>,
    cx: &mut Context<RepoView>,
) -> AnyElement {
    let count = doc.rows.len();
    let list = uniform_list(
        id,
        count,
        cx.processor(
            move |_this: &mut RepoView, range: Range<usize>, _window, cx| {
                range
                    .filter_map(|i| doc.rows.get(i).copied())
                    .map(|row| render_diff_row(&doc, row, side, cx))
                    .collect()
            },
        ),
    );
    match scroll {
        Some(handle) => list.track_scroll(handle),
        None => list,
    }
    .flex_1()
    .into_any_element()
}

/// Renders one row of a flattened diff (all rows share one height).
fn render_diff_row(
    doc: &Arc<DiffDoc>,
    row: DiffRow,
    side: Option<DiffSide>,
    cx: &mut Context<RepoView>,
) -> AnyElement {
    let base = div()
        .w_full()
        .h(row_height(cx))
        .flex()
        .items_center()
        .px_2()
        .whitespace_nowrap()
        .overflow_hidden();
    match row {
        DiffRow::File(f) => {
            let file = &doc.files[f];
            let (added, removed) = file.line_stats();
            base.justify_between()
                .bg(color(cx, T::BgPanel))
                .border_t_1()
                .border_color(color(cx, T::BorderSubtle))
                .child(div().truncate().child(file.display_path().to_owned()))
                .child(
                    div()
                        .flex_none()
                        .text_xs()
                        .text_color(color(cx, T::FgMuted))
                        .child(format!("+{added} −{removed}")),
                )
                .into_any_element()
        }
        DiffRow::Note(note) => base
            .text_color(color(cx, T::FgMuted))
            .child(note)
            .into_any_element(),
        DiffRow::Hunk(f, h) => {
            let file = &doc.files[f];
            let hunk = &file.hunks[h];
            let mut header = base
                .justify_between()
                .text_xs()
                .text_color(color(cx, T::FgSubtle))
                .child(format!(
                    "@@ -{},{} +{},{} @@ {}",
                    hunk.old_start, hunk.old_lines, hunk.new_start, hunk.new_lines, hunk.header
                ));
            if let Some(side) = side {
                let (label, direction) = if side == DiffSide::Staged {
                    ("Unstage hunk", PatchDirection::Reverse)
                } else {
                    ("Stage hunk", PatchDirection::Forward)
                };
                let doc = doc.clone();
                header = header.child(
                    button(
                        SharedString::from(format!("hunk-{}-{h}", file.display_path())),
                        label,
                        cx,
                    )
                    // The patch is built on click, not on every frame.
                    .on_click(cx.listener(
                        move |v, _, _, cx| match hunk_patch(&doc.files[f], h) {
                            Ok(p) => {
                                v.run(label, cx, move |a, repo| a.apply_patch(repo, &p, direction));
                            }
                            Err(e) => {
                                v.status = Some(e.to_string());
                                cx.notify();
                            }
                        },
                    )),
                );
            }
            header.into_any_element()
        }
        DiffRow::Line(f, h, l) => {
            diff_line_row(&doc.files[f].hunks[h].lines[l], doc.lineno_width, cx)
        }
    }
}

/// One line of a hunk: line-number gutter, +/- marker, then the file's text.
fn diff_line_row(line: &DiffLine, lineno_width: usize, cx: &App) -> AnyElement {
    let base = div()
        .w_full()
        .h(row_height(cx))
        .flex()
        .items_center()
        .whitespace_nowrap()
        .overflow_hidden();
    let (marker, marker_fg, bg) = match line.origin {
        LineOrigin::Added => ("+", T::DiffAddFg, Some(color(cx, T::DiffAddBg))),
        LineOrigin::Removed => ("-", T::DiffDelFg, Some(color(cx, T::DiffDelBg))),
        LineOrigin::Context => (" ", T::FgSubtle, None),
    };
    // The font is monospace, so padding aligns the gutter.
    let width = lineno_width;
    let number = |n: Option<u32>| n.map_or_else(String::new, |n| n.to_string());
    // Gutter (line numbers and marker) sits apart from the file's text,
    // on the panel color behind a separator: the marker must not read as
    // part of the line.
    base.font_family(mono_font(cx))
        .text_xs()
        .when_some(bg, gpui_kit::Styled::bg)
        .child(
            div()
                .flex_none()
                .h_full()
                .flex()
                .items_center()
                .gap_2()
                .px_2()
                .bg(color(cx, T::BgPanel))
                .border_r_1()
                .border_color(color(cx, T::BorderSubtle))
                .text_color(color(cx, T::FgSubtle))
                .child(format!(
                    "{:>width$} {:>width$}",
                    number(line.old_lineno),
                    number(line.new_lineno)
                ))
                .child(div().text_color(color(cx, marker_fg)).child(marker)),
        )
        .child(
            div()
                .pl_2()
                .child(line.content.trim_end_matches('\r').to_owned()),
        )
        .into_any_element()
}

impl Render for RepoView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body: AnyElement = match self.mode {
            Mode::History => {
                let refs = self.render_refs(cx);
                let graph = self.render_graph(cx);
                let details = self.render_details(cx);
                let ui = self.core.ui_state().unwrap_or_default();
                div()
                    .flex_1()
                    .flex()
                    .overflow_hidden()
                    .on_drag_move(cx.listener(Self::on_pane_drag))
                    .child(
                        div()
                            .w(px(ui.refs_width))
                            .flex_none()
                            .flex()
                            .flex_col()
                            .child(refs),
                    )
                    .child(splitter(Pane::Refs, cx))
                    .child(
                        div()
                            .id("graph-pane")
                            .flex_1()
                            .flex()
                            .flex_col()
                            .overflow_hidden()
                            .track_focus(&self.focus)
                            .on_key_down(cx.listener(|v, e: &KeyDownEvent, _, cx| v.on_key(e, cx)))
                            .child(graph),
                    )
                    .child(splitter(Pane::Details, cx))
                    .child(
                        div()
                            .w(px(ui.details_width))
                            .flex_none()
                            .flex()
                            .flex_col()
                            .child(details),
                    )
                    .into_any_element()
            }
            Mode::Changes => self.render_changes(cx),
        };
        let prompt = self.render_upstream_prompt(cx);
        div()
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .child(self.render_toolbar(cx))
            .when(self.branching && self.mode == Mode::History, |d| {
                d.child(self.render_branch_form(cx))
            })
            .child(body)
            .children(self.status.clone().map(|s| {
                div()
                    .px_3()
                    .py_1()
                    .text_xs()
                    .text_color(color(cx, T::FgMuted))
                    .child(s)
            }))
            .children(prompt)
    }
}

#[cfg(test)]
mod render_tests;
