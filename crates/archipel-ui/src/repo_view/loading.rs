//! Background reads of the repository tab: the graph and the stashes and
//! HEAD placed in it, refs, submodules, the working copy, commit metadata,
//! and the diff of what gets selected.

use std::collections::HashSet;
use std::ops::Range;
use std::sync::{Arc, Mutex};

use archipel_app::dashboard::DashboardRow;
use archipel_graph::GraphError;
use archipel_model::refs::{RefKind, RefTip};
use archipel_model::{CancelToken, HeadState, NoProgress, Oid, RepoSummary, Upstream};
use gpui_kit::Context;
use gpui_kit::prelude::*;

use crate::diff_view::{self, DiffDoc};
use crate::staging;

use super::graph_paint::{lanes_above, stash_lane};
use super::refs_panel::{RefList, RefRow};
use super::{HeadLink, MAX_LANES_SHOWN, OpenDiff, PlacedStash, RepoView, SharedView, lock};

/// Graph rows laid out at a time while looking for the HEAD commit.
const HEAD_SCAN_ROWS: u32 = 512;
/// Metadata entries kept by the view (rows around the visible window).
const META_KEEP: usize = 4000;

/// The parts of a repo summary the graph and refs panel depend on, and the
/// ref tips: when they move, the graph is reloaded; a working-copy edit
/// alone does not. Nor does a fetch that moved no ref: the last fetch time
/// is left out, else every background fetch of an open tab reloaded (and
/// flashed) its graph and refs panel; a fetch that moves refs moves `tips`.
#[derive(PartialEq, Eq)]
pub(super) struct RefsState {
    head: HeadState,
    head_oid: Option<Oid>,
    upstream: Option<Upstream>,
    describe: Option<String>,
    stash_count: u32,
    /// Every branch and tag: one created or deleted elsewhere (terminal,
    /// another tool) moves none of the fields above.
    tips: Option<Vec<RefTip>>,
}

impl RefsState {
    fn of(s: &RepoSummary) -> Self {
        Self {
            head: s.head.clone(),
            head_oid: s.head_oid,
            upstream: s.upstream.clone(),
            describe: s.describe.clone(),
            stash_count: s.stash_count,
            tips: None,
        }
    }
}

impl RepoView {
    pub(super) fn load_graph(&mut self, cx: &mut Context<Self>) {
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
                        v.graph_error = None;
                        v.row_count = rows;
                        v.lanes = lanes.clamp(1, MAX_LANES_SHOWN);
                        v.meta.clear();
                        v.stashes.clear();
                        v.head_link = None;
                        v.load_stashes(cx);
                        v.load_head_link(cx);
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
                let view = lock(&graph);
                let mut placed: Vec<PlacedStash> = entries
                    .into_iter()
                    .filter_map(|entry| {
                        let row = *rows.get(&entry.base)?;
                        let base = view.rows(row..row + 1).ok()?.pop()?;
                        Some(PlacedStash {
                            entry,
                            row,
                            lane: stash_lane(&base),
                        })
                    })
                    .collect();
                drop(view);
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

    /// Finds the HEAD commit's row and the columns taken above it, for the
    /// lane linking the uncommitted-changes row to it.
    fn load_head_link(&mut self, cx: &mut Context<Self>) {
        let Some(graph) = self.graph.clone() else {
            return;
        };
        let task = {
            let graph = Arc::clone(&graph);
            cx.background_spawn(async move { find_head(&graph) })
        };
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |v, cx| {
                if !v.graph.as_ref().is_some_and(|g| Arc::ptr_eq(g, &graph)) {
                    return; // reloaded meanwhile
                }
                match result {
                    Ok(link) => v.head_link = link,
                    Err(e) if e.is_cancelled() => {}
                    Err(e) => v.status = Some(e.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn load_changes(&mut self, cx: &mut Context<Self>) {
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
                if v.wip_selected && v.changes.is_empty() {
                    v.wip_selected = false;
                }
                // The open file diff follows the working copy: reloaded while
                // the file has changes on that side, moved to the other side
                // once they all went there (file or hunks staged), else closed.
                if let Some(OpenDiff::Worktree(file, side, _)) = &v.open_diff {
                    let file = file.clone();
                    match diff_view::follow(&v.changes, &file, *side) {
                        Some(side) if v.wip_selected => v.open_worktree_file(file, side, cx),
                        _ => v.open_diff = None,
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Selects the uncommitted-changes row: the details pane shows the
    /// staging lists.
    pub(super) fn select_wip(&mut self, cx: &mut Context<Self>) {
        self.selected = None;
        self.stash_selected = None;
        self.wip_selected = true;
        self.details = None;
        self.open_diff = None;
        cx.notify();
    }

    pub(super) fn load_refs(&mut self, cx: &mut Context<Self>) {
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
    pub(super) fn reload(&mut self, cx: &mut Context<Self>) {
        self.refs_state = None;
        self.core.refresh_repo(self.path.id());
        self.load_graph(cx);
        self.load_refs(cx);
        self.load_changes(cx);
        self.load_submodules(cx);
    }

    /// The repo was refreshed (file watcher, focus, F5): reloads the
    /// working copy, and the graph and refs when `HEAD` or refs moved.
    pub fn on_summary(&mut self, summary: Option<&RepoSummary>, cx: &mut Context<Self>) {
        if let Some(summary) = summary {
            self.check_refs(summary, cx);
        }
        staging::on_summary(self, summary, cx);
        self.load_changes(cx);
        // A checkout or an update inside a submodule only shows in the
        // working copy, not in the refs.
        self.load_submodules(cx);
    }

    /// Reads the ref tips, then reloads the graph and refs panel when they or
    /// the refs-related parts of `summary` moved since the last check.
    fn check_refs(&mut self, summary: &RepoSummary, cx: &mut Context<Self>) {
        let refs = self.core.repo_actions().ports().refs.clone();
        let path = self.path.clone();
        let mut state = RefsState::of(summary);
        let task = cx.background_spawn(async move { refs.ref_tips(&path).ok() });
        cx.spawn(async move |this, cx| {
            state.tips = task.await;
            let _ = this.update(cx, |v, cx| {
                if v.refs_state.as_ref().is_some_and(|s| *s != state) {
                    v.load_graph(cx);
                    v.load_refs(cx);
                }
                v.refs_state = Some(state);
            });
        })
        .detach();
    }

    /// The forge status of the repo was read again (or its branch changed).
    pub fn on_forge(&mut self, row: &DashboardRow, cx: &mut Context<Self>) {
        if self.ci != row.ci || self.forge != row.forge || self.forge_limit != row.forge_limit {
            self.ci = row.ci.clone();
            self.forge = row.forge.clone();
            self.forge_limit = row.forge_limit;
            cx.notify();
        }
    }

    pub(super) fn load_submodules(&mut self, cx: &mut Context<Self>) {
        let reader = self.core.repo_actions().ports().submodules.clone();
        let path = self.path.clone();
        let task = cx.background_spawn(async move { reader.submodules(&path) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |v, cx| {
                match result {
                    Ok(list) if *v.submodules != list => v.submodules = Arc::new(list),
                    Ok(_) => return,
                    Err(e) => v.status = Some(e.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn fetch_meta(&mut self, range: Range<u32>, cx: &mut Context<Self>) {
        let Some(graph) = self.graph.clone() else {
            return;
        };
        if self.pending.is_some() {
            return;
        }
        self.pending = Some(range.clone());
        let source = Arc::clone(&graph);
        let task = cx
            .background_spawn(async move { lock(&graph).meta(range.clone()).map(|m| (range, m)) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |v, cx| {
                v.pending = None;
                // Rows of a graph reloaded meanwhile no longer match.
                let current = v.graph.as_ref().is_some_and(|g| Arc::ptr_eq(g, &source));
                if let (true, Ok((range, metas))) = (current, result) {
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

    pub(super) fn select_commit(&mut self, row: u32, oid: Oid, cx: &mut Context<Self>) {
        self.selected = Some((row, oid));
        self.wip_selected = false;
        self.stash_selected = None;
        self.open_diff = None;
        let diffs = self.core.diff_reader();
        let history = self.core.history_reader();
        let path = self.path.clone();
        let task = cx.background_spawn(async move {
            let message = history.commit_message(&path, oid).ok();
            diffs
                .commit_diff(&path, oid)
                .map(|d| (Arc::new(DiffDoc::new(d)), message))
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |v, cx| {
                if v.selected.is_none_or(|(_, o)| o != oid) {
                    return; // another row was selected meanwhile
                }
                match result {
                    Ok((d, message)) => {
                        v.details = Some((oid, d));
                        v.message = message.map(|m| (oid, m.into()));
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
    pub(super) fn select_stash(&mut self, k: usize, cx: &mut Context<Self>) {
        let Some(oid) = self.placed().get(k).map(|s| s.entry.oid) else {
            return;
        };
        self.selected = None;
        self.wip_selected = false;
        self.stash_selected = Some(oid);
        self.open_diff = None;
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
                    Ok(d) => v.details = Some((oid, d)),
                    Err(e) => v.status = Some(e.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}

/// Scans the graph from the top for the HEAD commit, collecting the
/// columns taken above it (see [`HeadLink`]). Waits for the topology
/// build when HEAD is past the streamed rows; `None` when HEAD is not in
/// the history (unborn branch).
fn find_head(graph: &SharedView) -> Result<Option<HeadLink>, GraphError> {
    let mut taken = HashSet::new();
    let mut start = 0;
    loop {
        let rows = lock(graph).rows(start..start + HEAD_SCAN_ROWS)?;
        if rows.is_empty() {
            let completion = lock(graph).completion().filter(|c| !c.is_complete());
            match completion {
                Some(c) => {
                    c.wait(&NoProgress)?;
                    continue;
                }
                None => return Ok(None),
            }
        }
        for row in &rows {
            if row.refs.iter().any(|t| t.kind == RefKind::Head) {
                taken.extend(lanes_above(row).map(|e| e.from));
                return Ok(Some(HeadLink {
                    row: row.row,
                    column: row.column,
                    taken,
                }));
            }
            taken.insert(row.column);
            taken.extend(row.edges.iter().flat_map(|e| [e.from, e.to]));
        }
        start += u32::try_from(rows.len()).unwrap_or(u32::MAX);
    }
}
