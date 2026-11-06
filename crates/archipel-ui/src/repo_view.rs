//! A repository tab (spec §5): refs, the virtualized commit graph and a
//! details pane following the selection: a commit's or stash's files, or,
//! for the uncommitted-changes row, the staging lists and the commit box.
//! A file clicked there opens its diff in place of the graph.
//!
//! Every Git call runs on a background thread; the graph rows come from the
//! on-disk topology cache and commit metadata from its bounded LRU.

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::sync::{Arc, Mutex, PoisonError};

use archipel_app::App as Core;
use archipel_app::forge::{BranchCi, ForgeStatus, ci_cell, ci_tooltip, pick_remote};
use archipel_graph::{Edge, EdgeKind, GraphError, GraphRow, HistoryView};
use archipel_model::changes::{ChangeKind, FileChange};
use archipel_model::diff::FileDiff;
use archipel_model::history::CommitMeta;
use archipel_model::ports::DiffSide;
use archipel_model::refs::{RefKind, RefTip, StashEntry};
use archipel_model::requests::{
    CheckoutTarget, DeleteMode, MergeMode, PushRequest, ResetMode, StashApplyMode, TagRequest,
};
use archipel_model::submodule::SubmoduleInfo;
use archipel_model::{
    CancelToken, HeadState, NoProgress, Oid, RepoPath, RepoSummary, Timestamp, Upstream,
};
use archipel_theme::ColorToken as T;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, App, Bounds, ClickEvent, ClipboardItem, Context, DragMoveEvent, Entity,
    FocusHandle, Hsla, KeyDownEvent, MouseButton, MouseDownEvent, PathBuilder, Pixels, Point,
    ScrollStrategy, SharedString, Subscription, UniformListScrollHandle, Window, anchored, canvas,
    deferred, div, point, px, uniform_list,
};

use crate::diff_view::{self, DiffDoc};
use crate::staging::{self, Staging};
use crate::theme::{color, lane, mono_font, radius, row_height, surface_between, tone};
use crate::widgets::{
    Pane, PaneDrag, TextTooltip, button, condense_status, context_menu, menu_item, menu_separator,
    primary_button, resize_pane, scroll_list, section, splitter,
};

/// Width of one graph lane.
const LANE_WIDTH: f32 = 14.0;
/// Lanes shown before the graph column stops growing (wide graphs clip).
const MAX_LANES_SHOWN: u32 = 24;
/// Graph rows laid out at a time while looking for the HEAD commit.
const HEAD_SCAN_ROWS: u32 = 512;
/// Metadata entries kept by the view (rows around the visible window).
const META_KEEP: usize = 4000;
/// Rows moved by Page Up / Page Down in the graph.
const PAGE_ROWS: i64 = 20;
/// Height of the commit message body in the details pane before it scrolls.
const MESSAGE_BODY_HEIGHT: f32 = 160.0;

/// A diff open in the centre pane, in place of the graph.
enum OpenDiff {
    /// The selected commit's or stash's diff: one file (index into the
    /// details diff), or all of them (`None`).
    Commit(Option<usize>, Arc<DiffDoc>),
    /// A working-copy file on one side, its hunks staged or unstaged from
    /// the diff; `None` while it loads.
    Worktree(String, DiffSide, Option<Arc<DiffDoc>>),
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
    /// Lane of the stash nodes: see [`stash_lane`].
    lane: u32,
}

/// Where the uncommitted-changes row joins the graph: the HEAD commit,
/// reached by a lane of its own drawn down from the top of the graph.
struct HeadLink {
    /// Graph row of the HEAD commit.
    row: u32,
    /// Column of its node.
    column: u32,
    /// Columns holding a node or a lane above the HEAD commit, lanes
    /// entering its row included: the uncommitted lane must avoid them.
    taken: HashSet<u32>,
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

/// What the name form under the toolbar creates or renames.
#[derive(Clone, Debug, PartialEq, Eq)]
enum NameForm {
    /// A branch at a commit, then checked out; `None` follows the
    /// selection (`HEAD` when nothing is selected).
    Branch(Option<Oid>),
    /// A lightweight tag at a commit.
    Tag(Oid),
    /// A new name for a local branch.
    Rename(String),
}

/// What a right-click menu acts on.
#[derive(Clone)]
enum MenuTarget {
    /// A commit row: its id and the refs pointing at it.
    Commit(Oid, Vec<RefTip>),
    /// A branch or tag (graph badge or refs panel).
    Ref(RefTip),
    /// A stash, by commit id.
    Stash(Oid),
    /// A submodule (refs panel).
    Submodule(SubmoduleInfo),
    /// Branch `source` dropped on branch `target` (drag and drop of
    /// branch badges or refs panel rows).
    Drop { source: RefTip, target: RefTip },
}

/// Drag payload of a branch badge or a refs panel branch row.
struct BranchDrag(RefTip);

/// Whether `tip` can be dragged, and dropped on (local or remote branch).
fn is_branch(tip: &RefTip) -> bool {
    matches!(tip.kind, RefKind::LocalBranch | RefKind::RemoteBranch)
}

/// The badge following the pointer while a branch is dragged.
struct BranchGhost(SharedString);

impl Render for BranchGhost {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_1()
            .rounded(radius(cx))
            .text_xs()
            .whitespace_nowrap()
            .bg(color(cx, T::Accent))
            .text_color(color(cx, T::AccentFg))
            .shadow_md()
            .child(self.0.clone())
    }
}

/// An action picked in a right-click menu.
#[derive(Clone, Debug, PartialEq, Eq)]
enum MenuAction {
    Name(NameForm),
    Checkout(CheckoutTarget),
    CherryPick(Oid),
    Revert(Oid),
    Reset(Oid, ResetMode),
    Merge(String),
    Rebase(String),
    /// Checks out local branch `into`, then merges `rev` into it.
    MergeInto {
        rev: String,
        into: String,
    },
    /// Checks out local branch `branch`, then rebases it onto `onto`.
    RebaseBranch {
        branch: String,
        onto: String,
    },
    DeleteBranch(String, DeleteMode),
    /// A remote branch, `remote/branch`, deleted on the remote.
    DeleteRemoteBranch(String),
    DeleteTag(String),
    PushTag(String),
    Stash(Oid, Option<StashApplyMode>),
    /// A submodule opened as a repo tab.
    OpenSubmodule(RepoPath),
    /// `git submodule update` of one submodule (path relative to the
    /// repo), initializing it first when `init`.
    UpdateSubmodule {
        name: String,
        init: bool,
    },
    Copy(String),
}

impl MenuAction {
    /// The question asked before an action that cannot be undone from the
    /// journal or loses work; `None` runs it right away.
    fn confirmation(&self) -> Option<String> {
        match self {
            Self::Reset(oid, ResetMode::Hard) => Some(format!(
                "Reset the current branch to {} and discard every uncommitted change?",
                oid.short(8)
            )),
            Self::DeleteBranch(name, DeleteMode::Force) => Some(format!(
                "Delete {name} even though it is not merged? Its unmerged commits are only \
                 reachable through Undo afterwards."
            )),
            Self::DeleteRemoteBranch(name) => {
                Some(format!("Delete {name} on the remote? Others lose it too."))
            }
            _ => None,
        }
    }
}

/// One line of a right-click menu.
#[derive(Debug, PartialEq, Eq)]
enum MenuEntry {
    Item {
        label: String,
        action: MenuAction,
        danger: bool,
    },
    Separator,
}

fn item(label: impl Into<String>, action: MenuAction) -> MenuEntry {
    MenuEntry::Item {
        label: label.into(),
        action,
        danger: false,
    }
}

fn danger(label: impl Into<String>, action: MenuAction) -> MenuEntry {
    MenuEntry::Item {
        label: label.into(),
        action,
        danger: true,
    }
}

/// The entries of the menu on `target`. `current`: the checked-out branch;
/// `has_local`: whether a local branch has this name.
fn menu_entries(
    target: &MenuTarget,
    current: Option<&str>,
    has_local: impl Fn(&str) -> bool,
) -> Vec<MenuEntry> {
    use MenuAction as A;
    let mut out = Vec::new();
    match target {
        MenuTarget::Commit(oid, refs) => {
            let oid = *oid;
            out.push(item(
                "New branch here…",
                A::Name(NameForm::Branch(Some(oid))),
            ));
            out.push(item("New tag here…", A::Name(NameForm::Tag(oid))));
            out.push(item(
                "Checkout (detached)",
                A::Checkout(CheckoutTarget::Detached(oid)),
            ));
            out.push(MenuEntry::Separator);
            out.push(item("Cherry-pick", A::CherryPick(oid)));
            out.push(item("Revert", A::Revert(oid)));
            let onto = current.unwrap_or("HEAD");
            out.push(item(
                format!("Reset {onto} here (soft)"),
                A::Reset(oid, ResetMode::Soft),
            ));
            out.push(item(
                format!("Reset {onto} here (mixed)"),
                A::Reset(oid, ResetMode::Mixed),
            ));
            out.push(danger(
                format!("Reset {onto} here (hard)…"),
                A::Reset(oid, ResetMode::Hard),
            ));
            let deletes: Vec<MenuEntry> = refs
                .iter()
                .filter_map(|r| match r.kind {
                    RefKind::LocalBranch if Some(r.name.as_str()) != current => Some(danger(
                        format!("Delete branch {}", r.name),
                        A::DeleteBranch(r.name.clone(), DeleteMode::Safe),
                    )),
                    RefKind::Tag => Some(danger(
                        format!("Delete tag {}", r.name),
                        A::DeleteTag(r.name.clone()),
                    )),
                    _ => None,
                })
                .collect();
            if !deletes.is_empty() {
                out.push(MenuEntry::Separator);
                out.extend(deletes);
            }
            out.push(MenuEntry::Separator);
            out.push(item("Copy commit id", A::Copy(oid.to_hex())));
        }
        MenuTarget::Ref(tip) => {
            let name = &tip.name;
            let is_current = tip.kind == RefKind::LocalBranch && Some(name.as_str()) == current;
            if !is_current && let Some(t) = CheckoutTarget::for_ref(tip, &has_local) {
                out.push(item(format!("Checkout {name}"), A::Checkout(t)));
            }
            if matches!(tip.kind, RefKind::LocalBranch | RefKind::RemoteBranch) && !is_current {
                let into = current.unwrap_or("HEAD");
                out.push(item(
                    format!("Merge {name} into {into}"),
                    A::Merge(name.clone()),
                ));
                out.push(item(
                    format!("Rebase {into} onto {name}"),
                    A::Rebase(name.clone()),
                ));
            }
            out.push(item(
                format!("New branch from {name}…"),
                A::Name(NameForm::Branch(Some(tip.target))),
            ));
            match tip.kind {
                RefKind::LocalBranch => {
                    out.push(item("Rename…", A::Name(NameForm::Rename(name.clone()))));
                    if !is_current {
                        out.push(MenuEntry::Separator);
                        out.push(danger(
                            format!("Delete {name}"),
                            A::DeleteBranch(name.clone(), DeleteMode::Safe),
                        ));
                        out.push(danger(
                            format!("Force delete {name}…"),
                            A::DeleteBranch(name.clone(), DeleteMode::Force),
                        ));
                    }
                }
                RefKind::RemoteBranch => {
                    out.push(MenuEntry::Separator);
                    out.push(danger(
                        format!("Delete {name} on the remote…"),
                        A::DeleteRemoteBranch(name.clone()),
                    ));
                }
                RefKind::Tag => {
                    out.push(item(format!("Push {name}"), A::PushTag(name.clone())));
                    out.push(MenuEntry::Separator);
                    out.push(danger(format!("Delete {name}"), A::DeleteTag(name.clone())));
                }
                RefKind::Head | RefKind::Stash => {}
            }
            out.push(MenuEntry::Separator);
            out.push(item("Copy name", A::Copy(name.clone())));
        }
        MenuTarget::Stash(oid) => {
            let oid = *oid;
            out.push(item("Apply", A::Stash(oid, Some(StashApplyMode::Keep))));
            out.push(item("Pop", A::Stash(oid, Some(StashApplyMode::Pop))));
            out.push(danger("Drop", A::Stash(oid, None)));
        }
        MenuTarget::Drop { source, target } => {
            out.extend(drop_entries(source, target, current));
        }
        MenuTarget::Submodule(sub) => {
            let name = sub.name.clone();
            if sub.initialized {
                out.push(item("Open", A::OpenSubmodule(sub.path.clone())));
                let label = if sub.is_out_of_sync() {
                    "Update to the pinned commit"
                } else {
                    "Update"
                };
                out.push(item(label, A::UpdateSubmodule { name, init: false }));
            } else {
                out.push(item("Initialize", A::UpdateSubmodule { name, init: true }));
            }
            out.push(MenuEntry::Separator);
            out.push(item("Copy path", A::Copy(sub.name.clone())));
        }
    }
    out
}

/// The actions offered when branch `source` is dropped on branch
/// `target`: merge `source` into `target` (when `target` is local) and
/// rebase `source` onto `target` (when `source` is local). A branch other
/// than `current` is checked out first.
fn drop_entries(source: &RefTip, target: &RefTip, current: Option<&str>) -> Vec<MenuEntry> {
    use MenuAction as A;
    let mut out = Vec::new();
    if source == target {
        return out;
    }
    let (s, t) = (&source.name, &target.name);
    if target.kind == RefKind::LocalBranch {
        out.push(if Some(t.as_str()) == current {
            item(format!("Merge {s} into {t}"), A::Merge(s.clone()))
        } else {
            item(
                format!("Checkout {t} and merge {s} into it"),
                A::MergeInto {
                    rev: s.clone(),
                    into: t.clone(),
                },
            )
        });
    }
    if source.kind == RefKind::LocalBranch {
        out.push(if Some(s.as_str()) == current {
            item(format!("Rebase {s} onto {t}"), A::Rebase(t.clone()))
        } else {
            item(
                format!("Checkout {s} and rebase it onto {t}"),
                A::RebaseBranch {
                    branch: s.clone(),
                    onto: t.clone(),
                },
            )
        });
    }
    out
}

/// Whether `git branch -d` failed because the branch is not merged. Git's
/// message may be translated; its hint names `git branch -D` in any locale.
fn is_not_merged(e: &archipel_ops::OpError) -> bool {
    matches!(
        e,
        archipel_ops::OpError::Git(archipel_model::GitError::CommandFailed { stderr, .. })
            if stderr.contains("not fully merged") || stderr.contains("branch -D")
    )
}

/// An action waiting for confirmation in a dialog.
struct Confirm {
    question: String,
    action: MenuAction,
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
    graph: Option<SharedView>,
    row_count: u32,
    lanes: u32,
    graph_error: Option<String>,
    meta: HashMap<u32, Arc<CommitMeta>>,
    pending: Option<Range<u32>>,
    selected: Option<(u32, Oid)>,
    /// The uncommitted-changes row at the top of the graph is selected.
    wip_selected: bool,
    /// The HEAD commit the uncommitted-changes row is linked to, once found.
    head_link: Option<HeadLink>,
    /// Stashes placed in the graph, by row then stash index (newest first
    /// among the stashes of one commit).
    stashes: Vec<PlacedStash>,
    /// Commit id of the selected stash.
    stash_selected: Option<Oid>,
    /// Diff of the selected commit or stash, listed in the details pane.
    details: Option<(Oid, Arc<DiffDoc>)>,
    /// Full message of the selected commit, shown above its files.
    message: Option<(Oid, SharedString)>,
    changes: Vec<FileChange>,
    /// Diff shown in place of the graph.
    open_diff: Option<OpenDiff>,
    /// The staging panel shown for the uncommitted-changes row.
    staging: staging::Panel,
    /// Name typed in the name form, shown while `name_form` is set.
    name_input: Entity<InputState>,
    name_form: Option<NameForm>,
    _name_sub: Subscription,
    /// Open right-click menu and where it was opened.
    menu: Option<(MenuTarget, Point<Pixels>)>,
    /// Shown while a destructive menu action waits for confirmation.
    confirm: Option<Confirm>,
    prompt_focus: FocusHandle,
    status: Option<String>,
    refs: Arc<RefList>,
    /// Submodules, listed under the tags in the refs panel.
    submodules: Arc<Vec<SubmoduleInfo>>,
    /// Refs state of the last summary seen; `None` until the next summary,
    /// which is then taken as is (the graph was just loaded).
    refs_state: Option<RefsState>,
    /// CI of the current branch and forge status, as in the dashboard row.
    ci: BranchCi,
    forge: ForgeStatus,
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
        let row = core.rows().into_iter().find(|r| r.id == path.id());
        let summary = row.as_ref().and_then(|r| r.summary().cloned());
        let (ci, forge) = row.map(|r| (r.ci, r.forge)).unwrap_or_default();
        let staging = staging::Panel::new(summary, window, cx);
        let name_input = cx.new(|cx| InputState::new(window, cx).placeholder("Name"));
        let name_sub = cx.subscribe_in(
            &name_input,
            window,
            |v: &mut Self, _, event: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    v.submit_name(window, cx);
                }
            },
        );
        let mut view = Self {
            core,
            path,
            name,
            graph: None,
            row_count: 0,
            lanes: 1,
            graph_error: None,
            meta: HashMap::new(),
            pending: None,
            selected: None,
            wip_selected: false,
            head_link: None,
            stashes: Vec::new(),
            stash_selected: None,
            details: None,
            message: None,
            changes: Vec::new(),
            open_diff: None,
            staging,
            name_input,
            name_form: None,
            _name_sub: name_sub,
            menu: None,
            confirm: None,
            prompt_focus: cx.focus_handle(),
            status: None,
            refs: Arc::default(),
            submodules: Arc::default(),
            refs_state: None,
            ci,
            forge,
            scroll: UniformListScrollHandle::new(),
            focus: cx.focus_handle(),
        };
        view.load_graph(cx);
        view.load_refs(cx);
        view.load_changes(cx);
        view.load_submodules(cx);
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

    /// Lane of the line from the uncommitted-changes row down to the HEAD
    /// commit: HEAD's own column when nothing is drawn there above it, else
    /// the next free one. `None` while the working copy is clean or HEAD is
    /// not found.
    fn wip_lane(&self) -> Option<u32> {
        if self.changes.is_empty() {
            return None;
        }
        let link = self.head_link.as_ref()?;
        let stashes: Vec<u32> = self
            .placed()
            .iter()
            .take_while(|s| s.row <= link.row)
            .map(|s| s.lane)
            .collect();
        // n taken columns leave a free one among n + 1.
        let n = u32::try_from(link.taken.len() + stashes.len()).unwrap_or(u32::MAX);
        (link.column..=link.column.saturating_add(n))
            .find(|c| !link.taken.contains(c) && !stashes.contains(c))
    }

    /// The uncommitted lane where it crosses graph row `row` (or a stash row
    /// above it): rows down to the HEAD commit's.
    fn wip_lane_at(&self, row: u32) -> Option<u32> {
        self.wip_lane()
            .filter(|_| self.head_link.as_ref().is_some_and(|l| row <= l.row))
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

    /// Lanes drawn in the graph column: one more for the stash lane, and up
    /// to the uncommitted lane.
    fn lane_count(&self) -> u32 {
        let stash = self.lanes + u32::from(!self.placed().is_empty());
        let wip = self.wip_lane().map_or(0, |l| l + 1);
        stash.max(wip).min(MAX_LANES_SHOWN)
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

    /// Rows before the first commit in the graph list: the uncommitted
    /// changes row, shown while the working copy is dirty.
    fn wip_rows(&self) -> u32 {
        u32::from(!self.changes.is_empty())
    }

    /// Selects the uncommitted-changes row: the details pane shows the
    /// staging lists.
    fn select_wip(&mut self, cx: &mut Context<Self>) {
        self.selected = None;
        self.stash_selected = None;
        self.wip_selected = true;
        self.details = None;
        self.open_diff = None;
        cx.notify();
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
        self.load_submodules(cx);
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
        staging::on_summary(self, summary, cx);
        self.load_changes(cx);
        // A checkout or an update inside a submodule only shows in the
        // working copy, not in the refs.
        self.load_submodules(cx);
    }

    /// The forge status of the repo was read again (or its branch changed).
    pub fn on_forge(&mut self, ci: &BranchCi, forge: &ForgeStatus, cx: &mut Context<Self>) {
        if self.ci != *ci || self.forge != *forge {
            self.ci = ci.clone();
            self.forge = forge.clone();
            cx.notify();
        }
    }

    fn load_submodules(&mut self, cx: &mut Context<Self>) {
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
    fn select_stash(&mut self, k: usize, cx: &mut Context<Self>) {
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

    /// Opens working-copy file `file`'s diff on `side` in place of the
    /// graph. The diff shown stays until the new one is read when it is the
    /// same file (a reload), so the view does not flicker.
    fn open_worktree_file(&mut self, file: String, side: DiffSide, cx: &mut Context<Self>) {
        let shown = match self.open_diff.take() {
            Some(OpenDiff::Worktree(f, s, doc)) if f == file && s == side => doc,
            _ => None,
        };
        self.open_diff = Some(OpenDiff::Worktree(file.clone(), side, shown));
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
                let Some(OpenDiff::Worktree(f, s, doc)) = &mut v.open_diff else {
                    return;
                };
                if *f != file || *s != side {
                    return; // another file was opened meanwhile
                }
                match result {
                    Ok(d) => *doc = Some(d),
                    Err(e) => v.status = Some(e.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Opens file `file` of the selected commit's or stash's diff in place
    /// of the graph; `None`: all its files.
    fn open_commit_file(&mut self, file: Option<usize>, cx: &mut Context<Self>) {
        let Some((_, doc)) = &self.details else {
            return;
        };
        let doc = match file {
            None => Arc::clone(doc),
            Some(i) => match doc.files.get(i) {
                Some(f) => Arc::new(DiffDoc::new(vec![f.clone()])),
                None => return,
            },
        };
        self.open_diff = Some(OpenDiff::Commit(file, doc));
        cx.notify();
    }

    /// Opens the first working-copy file (double click / Enter on the
    /// uncommitted-changes row).
    fn open_first_change(&mut self, cx: &mut Context<Self>) {
        if let Some((file, side)) = diff_view::worktree_files(&self.changes).into_iter().next() {
            self.open_worktree_file(file, side, cx);
        }
    }

    /// Opens the file `delta` places from the open one in the details
    /// pane's list (keyboard navigation while a diff is open).
    fn step_file(&mut self, delta: i64, cx: &mut Context<Self>) {
        match &self.open_diff {
            Some(OpenDiff::Commit(file, _)) => {
                let count = self.details.as_ref().map_or(0, |(_, d)| d.files.len());
                let last = i64::try_from(count).unwrap_or(i64::MAX) - 1;
                // "All files" heads the list, at -1.
                let at = file.map_or(-1, |i| i64::try_from(i).unwrap_or(i64::MAX));
                let next = (at + delta).clamp(-1, last.max(-1));
                self.open_commit_file(usize::try_from(next).ok(), cx);
            }
            Some(OpenDiff::Worktree(file, side, _)) => {
                let files = diff_view::worktree_files(&self.changes);
                if let Some((file, side)) = diff_view::step(&files, file, *side, delta) {
                    self.open_worktree_file(file, side, cx);
                }
            }
            None => {}
        }
    }

    fn close_diff(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_diff = None;
        self.focus.focus(window, cx);
        cx.notify();
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

    fn on_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let m = &event.keystroke.modifiers;
        if m.control || m.alt || m.platform || m.shift {
            return;
        }
        if self.menu.is_some() && event.keystroke.key == "escape" {
            self.menu = None;
            cx.stop_propagation();
            cx.notify();
            return;
        }
        if self.open_diff.is_some() {
            match event.keystroke.key.as_str() {
                "down" => self.step_file(1, cx),
                "up" => self.step_file(-1, cx),
                "escape" => self.close_diff(window, cx),
                _ => return,
            }
            cx.stop_propagation();
            return;
        }
        match event.keystroke.key.as_str() {
            "down" => self.step(1, cx),
            "up" => self.step(-1, cx),
            "pagedown" => self.step(PAGE_ROWS, cx),
            "pageup" => self.step(-PAGE_ROWS, cx),
            "home" => self.step(i64::MIN / 2, cx),
            "end" => self.step(i64::MAX / 2, cx),
            "enter" if self.wip_selected => self.open_first_change(cx),
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

    /// Deletes local branch `name` with `git branch -d`. Git refuses a
    /// branch that is not merged (typical once its PR was squash-merged or
    /// its remote branch deleted): the force delete is then offered instead
    /// of leaving only a status line.
    fn delete_branch_safe(&mut self, name: String, window: &mut Window, cx: &mut Context<Self>) {
        let label = format!("Delete branch {name}");
        let actions = self.core.repo_actions().clone();
        let path = self.path.clone();
        let branch = name.clone();
        let task =
            cx.background_spawn(
                async move { actions.delete_branch(&path, &branch, DeleteMode::Safe) },
            );
        self.status = Some(format!("{label}…"));
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |v, window, cx| {
                match result {
                    Ok(()) => v.status = Some(format!("{label}: done")),
                    Err(e) if is_not_merged(&e) => {
                        v.status = None;
                        v.choose(
                            MenuAction::DeleteBranch(name, DeleteMode::Force),
                            window,
                            cx,
                        );
                    }
                    Err(e) => v.status = Some(format!("{label} failed: {e}")),
                }
                v.reload(cx);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Opens the name form for `form`, or closes it (`None`). A rename
    /// starts from the old name.
    fn open_name_form(
        &mut self,
        form: Option<NameForm>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let initial = match &form {
            Some(NameForm::Rename(old)) => old.clone(),
            _ => String::new(),
        };
        self.name_input
            .update(cx, |b, cx| b.set_value(initial, window, cx));
        self.name_form = form;
        if self.name_form.is_some() {
            self.name_input.update(cx, |b, cx| b.focus(window, cx));
        } else {
            self.focus.focus(window, cx);
        }
        cx.notify();
    }

    /// Creates the branch (then checks it out) or tag, or renames the
    /// branch, of the name form.
    fn submit_name(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(form) = self.name_form.clone() else {
            return;
        };
        let name = self.name_input.read(cx).value().trim().to_owned();
        if name.is_empty() {
            self.status = Some("Write a name first".into());
            cx.notify();
            return;
        }
        let selected = self.selected.map(|(_, oid)| oid);
        self.open_name_form(None, window, cx);
        match form {
            NameForm::Branch(start) => {
                let start = start.or(selected).map(|oid| oid.to_hex());
                self.run(format!("Create branch {name}"), cx, move |a, p| {
                    a.create_branch(p, &name, start.as_deref(), None)?;
                    a.checkout(p, &CheckoutTarget::Branch(name), None)
                });
            }
            NameForm::Tag(oid) => {
                let request = TagRequest {
                    name: name.clone(),
                    target: Some(oid.to_hex()),
                    message: None,
                };
                self.run(format!("Create tag {name}"), cx, move |a, p| {
                    a.create_tag(p, &request, None)
                });
            }
            NameForm::Rename(old) if old == name => {}
            NameForm::Rename(old) => {
                self.run(format!("Rename {old} → {name}"), cx, move |a, p| {
                    a.rename_branch(p, &old, &name)
                });
            }
        }
    }

    /// The checked-out branch, from the refs panel.
    fn current_branch(&self) -> Option<&str> {
        self.refs.rows.iter().find_map(|r| match r {
            RefRow::Tip { tip, current: true } if tip.kind == RefKind::LocalBranch => {
                Some(tip.name.as_str())
            }
            _ => None,
        })
    }

    /// Opens the right-click menu on `target` at `position`.
    fn open_menu(&mut self, target: MenuTarget, position: Point<Pixels>, cx: &mut Context<Self>) {
        self.menu = Some((target, position));
        cx.notify();
    }

    /// Runs a menu action, asking first when it is destructive.
    fn choose(&mut self, action: MenuAction, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        if let Some(question) = action.confirmation() {
            self.confirm = Some(Confirm { question, action });
            self.prompt_focus.focus(window, cx);
            cx.notify();
        } else {
            self.perform(action, window, cx);
        }
    }

    fn confirm(&mut self, accept: bool, window: &mut Window, cx: &mut Context<Self>) {
        let pending = self.confirm.take();
        self.focus.focus(window, cx);
        match pending {
            Some(c) if accept => self.perform(c.action, window, cx),
            _ => cx.notify(),
        }
    }

    fn perform(&mut self, action: MenuAction, window: &mut Window, cx: &mut Context<Self>) {
        match action {
            MenuAction::Name(form) => self.open_name_form(Some(form), window, cx),
            MenuAction::Checkout(target) => self.checkout(target, cx),
            MenuAction::CherryPick(oid) => {
                self.run(format!("Cherry-pick {}", oid.short(8)), cx, move |a, p| {
                    a.cherry_pick(p, &[oid])
                });
            }
            MenuAction::Revert(oid) => {
                self.run(format!("Revert {}", oid.short(8)), cx, move |a, p| {
                    a.revert(p, oid)
                });
            }
            MenuAction::Reset(oid, mode) => {
                self.run(format!("Reset to {}", oid.short(8)), cx, move |a, p| {
                    a.reset(p, &oid.to_hex(), mode)
                });
            }
            MenuAction::Merge(rev) => {
                self.run(format!("Merge {rev}"), cx, move |a, p| {
                    a.merge(p, &rev, MergeMode::Auto)
                });
            }
            MenuAction::Rebase(onto) => {
                self.run(format!("Rebase onto {onto}"), cx, move |a, p| {
                    a.rebase(p, &onto)
                });
            }
            MenuAction::MergeInto { rev, into } => {
                self.run(format!("Merge {rev} into {into}"), cx, move |a, p| {
                    a.checkout(p, &CheckoutTarget::Branch(into), None)?;
                    a.merge(p, &rev, MergeMode::Auto)
                });
            }
            MenuAction::RebaseBranch { branch, onto } => {
                self.run(format!("Rebase {branch} onto {onto}"), cx, move |a, p| {
                    a.checkout(p, &CheckoutTarget::Branch(branch), None)?;
                    a.rebase(p, &onto)
                });
            }
            MenuAction::DeleteBranch(name, DeleteMode::Safe) => {
                self.delete_branch_safe(name, window, cx);
            }
            MenuAction::DeleteBranch(name, mode) => {
                self.run(format!("Delete branch {name}"), cx, move |a, p| {
                    a.delete_branch(p, &name, mode)
                });
            }
            MenuAction::DeleteRemoteBranch(name) => {
                let Some((remote, branch)) = name.split_once('/') else {
                    return;
                };
                let request = PushRequest {
                    remote: Some(remote.to_owned()),
                    refspec: Some(format!(":refs/heads/{branch}")),
                    ..PushRequest::default()
                };
                self.run(format!("Delete {name}"), cx, move |a, p| {
                    a.push(p, &request, &NoProgress, &CancelToken::new(), None)
                });
            }
            MenuAction::DeleteTag(name) => {
                self.run(format!("Delete tag {name}"), cx, move |a, p| {
                    a.delete_tag(p, &name)
                });
            }
            MenuAction::PushTag(name) => {
                self.run(format!("Push tag {name}"), cx, move |a, p| {
                    let remotes = a.ports().remotes.remotes(p)?;
                    let remote = pick_remote(&remotes, None).ok_or_else(|| {
                        archipel_ops::OpError::Refused("the repository has no remote".into())
                    })?;
                    a.push_tag(p, &remote.name, &name, &NoProgress, &CancelToken::new())
                });
            }
            MenuAction::Stash(oid, mode) => self.stash_op(oid, mode, cx),
            MenuAction::OpenSubmodule(path) => {
                if let Err(e) = self
                    .core
                    .dispatch(archipel_app::Command::OpenSubmodule(path))
                {
                    self.status = Some(e.to_string());
                    cx.notify();
                }
            }
            MenuAction::UpdateSubmodule { name, init } => {
                let command = archipel_app::Command::UpdateSubmodules {
                    parent: self.path.clone(),
                    paths: vec![name],
                    init,
                    recursive: false,
                };
                if let Err(e) = self.core.dispatch(command) {
                    self.status = Some(e.to_string());
                    cx.notify();
                }
            }
            MenuAction::Copy(text) => {
                cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
                self.status = Some(format!("Copied {text}"));
                cx.notify();
            }
        }
    }

    // ----- rendering ----------------------------------------------------

    /// The repo name and the CI of the current branch; the repo actions are
    /// in the details pane.
    fn render_toolbar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .items_center()
            .gap_3()
            .px_3()
            .py_1()
            .border_b_1()
            .border_color(color(cx, T::BorderSubtle))
            .child(
                div()
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .child(self.name.clone()),
            )
            .children(self.render_ci(cx))
    }

    /// The CI badge, as the dashboard "CI" cell: jobs on hover, click opens
    /// the run.
    fn render_ci(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let cell = ci_cell(&self.ci, &self.forge);
        if cell.text.is_empty() {
            return None;
        }
        let tooltip = ci_tooltip(&self.ci, &self.forge);
        let url = self.ci.run().and_then(|run| run.url.clone());
        let hover = color(cx, T::BgHover);
        Some(
            div()
                .id("repo-ci")
                .flex()
                .gap_1()
                .px_1()
                .rounded(radius(cx))
                .text_sm()
                .child(div().text_color(color(cx, T::FgSubtle)).child("CI"))
                .child(div().text_color(tone(cx, cell.tone)).child(cell.text))
                .when_some(url, |d, url| {
                    d.cursor_pointer()
                        .hover(move |s| s.bg(hover))
                        .on_click(move |_, _, cx| cx.open_url(&url))
                })
                .when_some(tooltip, |d, lines| {
                    d.tooltip(move |_, cx| TextTooltip::view(&lines, cx))
                })
                .into_any_element(),
        )
    }

    /// The name form: what is named, the name, OK / Cancel.
    fn render_name_form(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (title, ok) = match self.name_form.as_ref()? {
            NameForm::Branch(start) => {
                let start = start
                    .or(self.selected.map(|(_, oid)| oid))
                    .map_or_else(|| "HEAD".to_owned(), |oid| oid.short(8));
                (format!("New branch at {start}"), "Create")
            }
            NameForm::Tag(oid) => (format!("New tag at {}", oid.short(8)), "Create"),
            NameForm::Rename(old) => (format!("Rename {old} to"), "Rename"),
        };
        Some(
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
                        v.open_name_form(None, window, cx);
                    }
                }))
                .child(
                    div()
                        .flex_none()
                        .text_color(color(cx, T::FgMuted))
                        .child(title),
                )
                .child(div().w(px(260.0)).child(Input::new(&self.name_input)))
                .child(
                    primary_button("name-ok", ok, cx)
                        .on_click(cx.listener(|v, _, window, cx| v.submit_name(window, cx))),
                )
                .child(
                    button("name-cancel", "Cancel", cx).on_click(
                        cx.listener(|v, _, window, cx| v.open_name_form(None, window, cx)),
                    ),
                )
                .into_any_element(),
        )
    }

    /// The dialog confirming a destructive menu action.
    fn render_confirm(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let confirm = self.confirm.as_ref()?;
        let dialog = div()
            .id("confirm-prompt")
            .occlude()
            .track_focus(&self.prompt_focus)
            .on_key_down(cx.listener(|v, e: &KeyDownEvent, window, cx| {
                match e.keystroke.key.as_str() {
                    "escape" => v.confirm(false, window, cx),
                    "enter" => v.confirm(true, window, cx),
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
            .child(confirm.question.clone())
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        button("confirm-cancel", "Cancel", cx)
                            .on_click(cx.listener(|v, _, window, cx| v.confirm(false, window, cx))),
                    )
                    .child(
                        primary_button("confirm-ok", "Continue", cx)
                            .on_click(cx.listener(|v, _, window, cx| v.confirm(true, window, cx))),
                    ),
            );
        Some(modal(dialog, cx))
    }

    /// The open right-click menu.
    fn render_menu(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (target, position) = self.menu.as_ref()?;
        let position = *position;
        let entries = menu_entries(target, self.current_branch(), |b| self.has_local(b));
        let rows = entries
            .into_iter()
            .enumerate()
            .map(|(i, entry)| match entry {
                MenuEntry::Separator => menu_separator(cx).into_any_element(),
                MenuEntry::Item {
                    label,
                    action,
                    danger,
                } => menu_item(("repo-menu", i), label, danger, cx)
                    .on_click(cx.listener(move |v, _, window, cx| {
                        v.choose(action.clone(), window, cx);
                    }))
                    .into_any_element(),
            });
        let menu = context_menu(cx)
            .on_mouse_down_out(cx.listener(|v, _, _, cx| {
                v.menu = None;
                cx.notify();
            }))
            .children(rows);
        Some(
            deferred(anchored().position(position).snap_to_window().child(menu))
                .with_priority(1)
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
        let list = uniform_list(
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
                let wip_lane = this.wip_lane();
                let mut out = Vec::with_capacity(items.len());
                for item in items {
                    match item {
                        Item::Wip => {
                            // Linked down to HEAD once it is found; until
                            // then, above HEAD's lane when HEAD is on screen.
                            let column = wip_lane.unwrap_or_else(|| {
                                rows.iter()
                                    .find(|r| r.refs.iter().any(|t| t.kind == RefKind::Head))
                                    .map_or(0, |r| r.column)
                            });
                            out.push(this.render_wip_row(column, wip_lane.is_some(), cx));
                        }
                        Item::Stash(k) => {
                            if let Some(base) = row_at(this.placed()[k].row) {
                                out.push(this.render_stash_row(k, base, cx));
                            }
                        }
                        Item::Commit(r) => {
                            if let Some(row) = row_at(r) {
                                let lane =
                                    this.placed().iter().find(|s| s.row == r).map(|s| s.lane);
                                let wip = this.wip_lane_at(r);
                                out.push(this.render_row(row, lane, wip, cx));
                            }
                        }
                    }
                }
                out
            }),
        );
        scroll_list("graph-scroll", list)
            .handle(&self.scroll)
            .into_any_element()
    }

    /// Branches and tags: click jumps to the commit, double click checks
    /// out. Then the submodules, when any: double click opens one. A click
    /// on a section header collapses or expands it.
    fn render_refs(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let refs = Arc::clone(&self.refs);
        let subs = Arc::clone(&self.submodules);
        let collapsed = self
            .core
            .ui_state()
            .unwrap_or_default()
            .collapsed_ref_sections;
        let is_collapsed = move |title: &str| collapsed.iter().any(|c| c == title);
        // Indices of the shown rows: the refs rows, then the submodules
        // header and the submodules.
        let mut shown = Vec::with_capacity(refs.rows.len());
        let mut open = true;
        for (i, row) in refs.rows.iter().enumerate() {
            if let RefRow::Header(title, _) = row {
                open = !is_collapsed(title);
                shown.push(i);
            } else if open {
                shown.push(i);
            }
        }
        if !subs.is_empty() {
            let base = refs.rows.len();
            shown.push(base);
            if !is_collapsed("SUBMODULES") {
                shown.extend(base + 1..=base + subs.len());
            }
        }
        let list = uniform_list(
            "refs",
            shown.len(),
            cx.processor(move |_this: &mut Self, range: Range<usize>, _window, cx| {
                range
                    .filter_map(|r| {
                        let i = shown[r];
                        match i.checked_sub(refs.rows.len()) {
                            None => {
                                let row = &refs.rows[i];
                                let folded = matches!(row, RefRow::Header(t, _) if is_collapsed(t));
                                Some(render_ref_row(i, row, folded, cx))
                            }
                            Some(0) => Some(render_ref_row(
                                i,
                                &RefRow::Header("SUBMODULES", subs.len()),
                                is_collapsed("SUBMODULES"),
                                cx,
                            )),
                            Some(s) => subs.get(s - 1).map(|sub| render_submodule_row(i, sub, cx)),
                        }
                    })
                    .collect()
            }),
        );
        scroll_list("refs-scroll", list).into_any_element()
    }

    /// Collapses or expands the refs pane section `title`.
    fn toggle_ref_section(&mut self, title: &'static str, cx: &mut Context<Self>) {
        self.core.update_ui_state(|ui| {
            let sections = &mut ui.collapsed_ref_sections;
            if let Some(i) = sections.iter().position(|c| c == title) {
                sections.remove(i);
            } else {
                sections.push(title.to_owned());
            }
        });
        cx.notify();
    }

    /// The uncommitted-changes row: a hollow node on lane `column`, with the
    /// lane going down to the HEAD commit when `linked`, and the staged /
    /// unstaged counts. Click shows the staging lists, double click also
    /// opens the first file's diff.
    fn render_wip_row(&mut self, column: u32, linked: bool, cx: &mut Context<Self>) -> AnyElement {
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
                if linked {
                    let bottom = bounds.origin.y + bounds.size.height;
                    stroke(
                        window,
                        point(center.x, center.y + r),
                        point(center.x, bottom),
                        ring,
                        true,
                    );
                }
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
                v.select_wip(cx);
                if e.click_count() > 1 {
                    v.open_first_change(cx);
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
    /// which joins its node; `wip_lane`: the uncommitted lane, passing by or
    /// joining the HEAD commit.
    fn render_row(
        &mut self,
        row: &GraphRow,
        stash_lane: Option<u32>,
        wip_lane: Option<u32>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let meta = self.meta.get(&row.row).cloned();
        let selected = self.selected.is_some_and(|(r, _)| r == row.row);
        // Same width on every row so the text columns line up.
        let width = px(lane_x(self.lane_count()));
        let lanes = graph_cell(row, stash_lane, wip_lane, cx);
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
        let refs = row.refs.clone();
        let is_head = row.refs.iter().any(|r| r.kind == RefKind::Head);
        // On the checked-out commit, the current branch stands for HEAD;
        // a detached HEAD keeps its own badge.
        let current = self.current_branch().filter(|name| {
            row.refs
                .iter()
                .any(|r| r.kind == RefKind::LocalBranch && r.name == *name)
        });
        let badges = row.refs.iter().enumerate().filter_map(|(i, r)| {
            if r.kind == RefKind::Head && current.is_some() {
                return None;
            }
            let tip = r.clone();
            let checked_out = is_head
                && match r.kind {
                    RefKind::Head => true,
                    RefKind::LocalBranch => current == Some(r.name.as_str()),
                    _ => false,
                };
            Some(
                div()
                    .id(SharedString::from(format!("badge-{index}-{i}")))
                    .px_1()
                    .rounded(radius(cx))
                    .text_xs()
                    .map(|d| {
                        if checked_out {
                            d.bg(color(cx, T::Accent))
                                .text_color(color(cx, T::AccentFg))
                                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        } else {
                            d.bg(color(cx, T::AccentSubtle))
                                .text_color(color(cx, T::AccentText))
                        }
                    })
                    // A single click bubbles to the row (select); a double click
                    // checks out this ref rather than the row's preferred one.
                    .on_click({
                        let tip = tip.clone();
                        cx.listener(move |v, e: &ClickEvent, _, cx| {
                            if e.click_count() > 1 {
                                cx.stop_propagation();
                                v.checkout_ref(&tip, cx);
                            }
                        })
                    })
                    // HEAD and stash badges leave the right click to the row.
                    .when(
                        matches!(
                            r.kind,
                            RefKind::LocalBranch | RefKind::RemoteBranch | RefKind::Tag
                        ),
                        |d| {
                            let menu_tip = tip.clone();
                            d.on_mouse_down(
                                MouseButton::Right,
                                cx.listener(move |v, e: &MouseDownEvent, _, cx| {
                                    cx.stop_propagation();
                                    v.open_menu(MenuTarget::Ref(menu_tip.clone()), e.position, cx);
                                }),
                            )
                        },
                    )
                    // Drag onto another branch to merge or rebase.
                    .when(is_branch(r), |d| branch_drag_and_drop(d, &tip, cx))
                    .child(r.name.clone()),
            )
        });
        let hover = color(cx, T::BgHover);
        div()
            .id(SharedString::from(format!("g-{index}")))
            .relative()
            .w_full()
            .h(row_height(cx))
            .flex()
            .items_center()
            .gap_2()
            .pr_3()
            .cursor_pointer()
            // The checked-out commit: tinted, with an accent bar on its edge.
            .when(is_head, |d| {
                d.bg(color(cx, T::AccentSubtle).opacity(0.5)).child(
                    div()
                        .absolute()
                        .left_0()
                        .top_0()
                        .h_full()
                        .w(px(3.0))
                        .bg(color(cx, T::Accent)),
                )
            })
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
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |v, e: &MouseDownEvent, window, cx| {
                    v.focus.focus(window, cx);
                    if v.selected.is_none_or(|(r, _)| r != index) {
                        v.select_commit(index, oid, cx);
                    }
                    v.open_menu(MenuTarget::Commit(oid, refs.clone()), e.position, cx);
                }),
            )
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
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .truncate()
                    .when(is_head, |d| d.font_weight(gpui_kit::FontWeight::SEMIBOLD))
                    .child(summary),
            )
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
        let cell = stash_cell(base, stash.lane, linked, self.wip_lane_at(stash.row), cx);
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
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |v, e: &MouseDownEvent, window, cx| {
                    v.focus.focus(window, cx);
                    if v.stash_selected != Some(oid)
                        && let Some(k) = v.stash_at(oid)
                    {
                        v.select_stash(k, cx);
                    }
                    v.open_menu(MenuTarget::Stash(oid), e.position, cx);
                }),
            )
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

    /// The details pane: the staging panel for the uncommitted changes,
    /// else the repo actions above the selected commit or stash.
    fn render_details(&mut self, cx: &mut Context<Self>) -> AnyElement {
        if self.wip_selected {
            return staging::render(self, cx);
        }
        let branch = staging::small(button("r-branch", "Branch", cx))
            .on_click(cx.listener(|v, _, window, cx| {
                let form = match v.name_form {
                    Some(_) => None,
                    None => Some(NameForm::Branch(None)),
                };
                v.open_name_form(form, window, cx);
            }))
            .into_any_element();
        let actions = staging::render_actions(self, Some(branch), cx);
        let selection = self.render_selection(cx);
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(
                div()
                    .p_2()
                    .border_b_1()
                    .border_color(color(cx, T::BorderSubtle))
                    .child(actions),
            )
            .child(selection)
            .into_any_element()
    }

    /// Details of the selected commit or stash.
    fn render_selection(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let Some((oid, doc)) = &self.details else {
            return div()
                .p_3()
                .text_color(color(cx, T::FgMuted))
                .child("Select a commit")
                .into_any_element();
        };
        let (oid, doc) = (*oid, doc.clone());
        if self.stash_selected == Some(oid) {
            if let Some(k) = self.stash_at(oid) {
                return self.render_stash_details(k, doc, cx);
            }
        }
        let meta = self.selected.and_then(|(r, _)| self.meta.get(&r).cloned());
        let message = self
            .message
            .as_ref()
            .filter(|(o, _)| *o == oid)
            .map(|(_, m)| m.clone());
        let mut el = div().p_2().flex().flex_col().gap_1();
        if let Some(m) = meta {
            let (title, body) = match &message {
                Some(msg) => match msg.split_once('\n') {
                    Some((t, b)) => (SharedString::from(t.to_owned()), b.trim()),
                    None => (msg.clone(), ""),
                },
                None => (m.summary.clone().into(), ""),
            };
            el = el.child(
                div()
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .child(title),
            );
            if !body.is_empty() {
                el = el.child(
                    div()
                        .id("details-message")
                        .max_h(px(MESSAGE_BODY_HEIGHT))
                        .overflow_y_scrollbar()
                        .text_sm()
                        .text_color(color(cx, T::FgMuted))
                        .child(body.to_owned()),
                );
            }
            el = el.child(
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
        Self::render_diff_details(el, &oid.to_hex(), doc, cx)
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
        Self::render_diff_details(header, &format!("stash-{oid}"), doc, cx)
    }

    /// A details header and the changed-files list below it.
    fn render_diff_details(
        header: gpui_kit::Div,
        key: &str,
        doc: Arc<DiffDoc>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let count = doc.files.len();
        // "All files" heads the list.
        let list = uniform_list(
            SharedString::from(format!("details-files-{key}")),
            count + 1,
            cx.processor(move |this: &mut Self, range: Range<usize>, _window, cx| {
                let open = match &this.open_diff {
                    Some(OpenDiff::Commit(file, _)) => Some(*file),
                    _ => None,
                };
                range
                    .filter(|&i| i <= doc.files.len())
                    .map(|i| {
                        let file = i.checked_sub(1);
                        render_changed_file(&doc, file, open == Some(file), cx)
                    })
                    .collect()
            }),
        );
        let list = scroll_list(SharedString::from(format!("details-scroll-{key}")), list);
        div()
            .flex_1()
            .flex()
            .flex_col()
            .overflow_hidden()
            .child(header)
            .child(
                section(format!("FILES ({count})"), cx)
                    .pt_1()
                    .border_t_1()
                    .border_color(color(cx, T::BorderSubtle)),
            )
            .map(|d| {
                if count == 0 {
                    d.child(
                        div()
                            .px_2()
                            .text_color(color(cx, T::FgMuted))
                            .child("No changes"),
                    )
                } else {
                    d.child(list)
                }
            })
            .into_any_element()
    }

    /// Resizes a pane while its splitter is dragged.
    fn on_pane_drag(
        &mut self,
        e: &DragMoveEvent<PaneDrag>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Every row listening for `PaneDrag` sees every splitter's drag,
        // the workspace sidebar's and the quick edit pane's included.
        let pane = e.drag(cx).0;
        if !matches!(pane, Pane::Refs | Pane::Details) {
            return;
        }
        let (x, bounds) = (e.event.position.x, e.bounds);
        self.core
            .update_ui_state(|ui| resize_pane(ui, pane, x, bounds));
        cx.notify();
    }

    /// Details of the uncommitted-changes row: unstaged and staged files,
    /// then the commit box. A file click opens its diff in place of the graph;
    /// a click on the file shown brings the graph back.
    /// The open diff, in place of the graph: a header naming it, then the
    /// diff (hunks stage / unstage on the working copy).
    fn render_open_diff(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let (title, note, side, doc) = match &self.open_diff {
            Some(OpenDiff::Commit(file, doc)) => {
                let title = match file {
                    Some(_) => doc.files.first().map(file_title).unwrap_or_default(),
                    None => format!("All files ({})", doc.files.len()),
                };
                let at = self.details.as_ref().map(|(oid, _)| oid.short(8));
                (title, at.unwrap_or_default(), None, Some(Arc::clone(doc)))
            }
            Some(OpenDiff::Worktree(file, side, doc)) => (
                file.clone(),
                diff_view::side_note(*side).to_owned(),
                Some(*side),
                doc.clone(),
            ),
            None => return div().into_any_element(),
        };
        diff_view::render_open(self, title, &note, side, doc, cx)
    }
}

/// `dialog` centered over the tab, which it dims and blocks.
fn modal(dialog: impl IntoElement, cx: &App) -> AnyElement {
    div()
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .justify_center()
        .occlude()
        .bg(color(cx, T::BgApp).opacity(0.6))
        .child(dialog)
        .into_any_element()
}

/// Horizontal center of lane `col` (lanes are a few dozen at most).
#[allow(clippy::cast_precision_loss)]
fn lane_x(col: u32) -> f32 {
    LANE_WIDTH * col as f32 + LANE_WIDTH / 2.0
}

/// Makes `element`, showing branch `tip`, draggable onto other branches
/// and a drop target for them: dropping opens the merge / rebase menu.
fn branch_drag_and_drop<E: StatefulInteractiveElement>(
    element: E,
    tip: &RefTip,
    cx: &Context<RepoView>,
) -> E {
    let target = tip.clone();
    let name = SharedString::from(tip.name.clone());
    element
        .on_drag(BranchDrag(tip.clone()), move |_, _, _, cx| {
            cx.new(|_| BranchGhost(name.clone()))
        })
        .drag_over::<BranchDrag>(|s, _, _, cx| {
            s.bg(color(cx, T::Accent))
                .text_color(color(cx, T::AccentFg))
        })
        .on_drop(cx.listener(move |v, d: &BranchDrag, window, cx| {
            cx.stop_propagation();
            if d.0 != target {
                let position = window.mouse_position();
                v.open_menu(
                    MenuTarget::Drop {
                        source: d.0.clone(),
                        target: target.clone(),
                    },
                    position,
                    cx,
                );
            }
        }))
}

/// One row of the refs panel; `collapsed` for a header whose section is
/// collapsed.
fn render_ref_row(
    i: usize,
    row: &RefRow,
    collapsed: bool,
    cx: &mut Context<RepoView>,
) -> AnyElement {
    let base = div()
        .h(row_height(cx))
        .flex()
        .items_center()
        .px_2()
        .whitespace_nowrap()
        .overflow_hidden();
    match row {
        RefRow::Header(title, count) => {
            let hover = color(cx, T::BgHover);
            let title = *title;
            base.id(SharedString::from(format!("ref-{i}")))
                .cursor_pointer()
                .hover(move |s| s.bg(hover))
                .text_xs()
                .text_color(color(cx, T::FgSubtle))
                .on_click(cx.listener(move |v, _, _, cx| v.toggle_ref_section(title, cx)))
                .child(
                    div()
                        .w(px(12.0))
                        .flex_none()
                        .child(if collapsed { "▸" } else { "▾" }),
                )
                .child(format!("{title} ({count})"))
                .into_any_element()
        }
        RefRow::Tip { tip, current } => {
            let hover = color(cx, T::BgHover);
            let name = tip.name.clone();
            let tip = tip.clone();
            let menu_tip = tip.clone();
            let drag_tip = tip.clone();
            base.id(SharedString::from(format!("ref-{i}")))
                .pl_4()
                .cursor_pointer()
                .hover(move |s| s.bg(hover))
                .when(*current, |d| {
                    d.text_color(color(cx, T::AccentText))
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
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |v, e: &MouseDownEvent, _, cx| {
                        v.open_menu(MenuTarget::Ref(menu_tip.clone()), e.position, cx);
                    }),
                )
                .when(is_branch(&drag_tip), |d| {
                    branch_drag_and_drop(d, &drag_tip, cx)
                })
                .child(div().truncate().child(if *current {
                    format!("● {name}")
                } else {
                    name
                }))
                .into_any_element()
        }
    }
}

/// One submodule of the refs panel: dimmed when not initialized, in the
/// warning color when checked out at another commit than the pinned one.
fn render_submodule_row(i: usize, sub: &SubmoduleInfo, cx: &mut Context<RepoView>) -> AnyElement {
    let hover = color(cx, T::BgHover);
    let (fg, state) = if !sub.initialized {
        (T::FgSubtle, "Not initialized")
    } else if sub.is_out_of_sync() {
        (
            T::StatusWarning,
            "Checked out at another commit than the pinned one",
        )
    } else {
        (T::FgDefault, "At the pinned commit")
    };
    let short = |oid: Option<Oid>| oid.map_or_else(|| "—".to_owned(), |o| o.short(8));
    let tooltip = vec![
        sub.name.clone(),
        state.to_owned(),
        format!("Pinned: {}", short(sub.pinned)),
        format!("Checked out: {}", short(sub.checked_out)),
    ];
    let path = sub.path.clone();
    let initialized = sub.initialized;
    let menu_sub = sub.clone();
    div()
        .id(SharedString::from(format!("ref-{i}")))
        .h(row_height(cx))
        .flex()
        .items_center()
        .px_2()
        .pl_4()
        .whitespace_nowrap()
        .overflow_hidden()
        .cursor_pointer()
        .hover(move |s| s.bg(hover))
        .text_color(color(cx, fg))
        .tooltip(move |_, cx| TextTooltip::view(&tooltip, cx))
        .on_click(cx.listener(move |v, e: &ClickEvent, window, cx| {
            if e.click_count() > 1 && initialized {
                v.choose(MenuAction::OpenSubmodule(path.clone()), window, cx);
            }
        }))
        .on_mouse_down(
            MouseButton::Right,
            cx.listener(move |v, e: &MouseDownEvent, _, cx| {
                v.open_menu(MenuTarget::Submodule(menu_sub.clone()), e.position, cx);
            }),
        )
        .child(div().truncate().child(sub.name.clone()))
        .into_any_element()
}

/// Message of a stash without the `On <branch>: ` Git prefixes to the
/// message given at push time.
fn stash_title(message: &str) -> &str {
    message
        .strip_prefix("On ")
        .and_then(|m| m.split_once(": "))
        .map_or(message, |(_, m)| m)
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

/// Strokes a lane from `a` down to `b`. A lane changing column runs
/// straight along one side and turns with a rounded corner, so it stays
/// vertical where it meets the rows above and below: `vertical_first`
/// drops from `a` then turns toward `b` (a lane ending at a node), else it
/// leaves `a` sideways then turns down to `b` (a lane leaving a node).
fn stroke(
    window: &mut Window,
    a: Point<Pixels>,
    b: Point<Pixels>,
    color: Hsla,
    vertical_first: bool,
) {
    // Bézier handle length approximating a quarter circle.
    const K: f32 = 0.552_285;
    let mut path = PathBuilder::stroke(px(1.6));
    path.move_to(a);
    let dx = f32::from(b.x - a.x);
    if dx.abs() < 0.5 {
        path.line_to(b);
    } else {
        let r = dx.abs().min(f32::from(b.y - a.y)).max(0.0);
        let (rx, ry) = (px(r.copysign(dx)), px(r));
        let (hx, hy) = (rx * K, ry * K);
        if vertical_first {
            let start = point(a.x, b.y - ry);
            let end = point(a.x + rx, b.y);
            path.line_to(start);
            path.cubic_bezier_to(end, point(start.x, start.y + hy), point(end.x - hx, end.y));
        } else {
            let start = point(b.x - rx, a.y);
            let end = point(b.x, a.y + ry);
            path.line_to(start);
            path.cubic_bezier_to(end, point(start.x + hx, start.y), point(end.x, end.y - hy));
        }
        path.line_to(b);
    }
    if let Ok(p) = path.build() {
        window.paint_path(p, color);
    }
}

/// Paints a stash row above `base`: the lanes passing by (the uncommitted
/// lane `wip_lane` too), and the stash node on lane `column`, linked to the
/// stash above it when `linked` and down toward the base commit.
fn stash_cell(
    base: &GraphRow,
    column: u32,
    linked: bool,
    wip_lane: Option<u32>,
    cx: &App,
) -> impl IntoElement {
    let tint = color(cx, T::StatusDirty);
    let lanes: Vec<(u32, Hsla)> = lanes_above(base)
        .map(|e| (e.from, lane(cx, e.color)))
        .chain(wip_lane.map(|l| (l, tint)))
        .collect();
    canvas(
        |_, _, _| {},
        move |bounds: Bounds<Pixels>, (), window, _| {
            let scale = window.scale_factor();
            let snap = |v: Pixels| px((f32::from(v) * scale).round() / scale);
            let x = |col: u32| snap(bounds.origin.x + px(lane_x(col)));
            let (top, bottom) = (bounds.origin.y, bounds.origin.y + bounds.size.height);
            let mid = snap(bounds.origin.y + bounds.size.height / 2.0);
            for (col, c) in &lanes {
                stroke(
                    window,
                    point(x(*col), top),
                    point(x(*col), bottom),
                    *c,
                    true,
                );
            }
            // The node is hollow: the stash lane stops at its edge.
            let r = px(4.0);
            let cx = x(column);
            if linked {
                stroke(window, point(cx, top), point(cx, mid - r), tint, true);
            }
            stroke(window, point(cx, mid + r), point(cx, bottom), tint, true);
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

/// Paints the lanes and the commit node of one graph row, the end of the
/// stash lane (`stash_lane`) when stashes are drawn above it, and the
/// uncommitted lane (`wip_lane`), ending at the node of the HEAD commit.
fn graph_cell(
    row: &GraphRow,
    stash_lane: Option<u32>,
    wip_lane: Option<u32>,
    cx: &App,
) -> impl IntoElement {
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
    let is_head = row.refs.iter().any(|r| r.kind == RefKind::Head);
    if let Some(col) = wip_lane {
        let (kind, to) = if is_head {
            (EdgeKind::Incoming, row.column)
        } else {
            (EdgeKind::Through, col)
        };
        edges.push((kind, col, to, color(cx, T::StatusDirty)));
    }
    let node = (row.column, lane(cx, row.color));
    // The checked-out commit gets an accent ring around its node.
    let head_ring = is_head.then(|| color(cx, T::Accent));
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
                stroke(window, a, b, *c, *kind != EdgeKind::Outgoing);
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
                    .border_widths(px(2.0))
                    .border_color(ring),
                );
            }
        },
    )
    .size_full()
}

/// A file's path, both of them when it was renamed.
fn file_title(file: &FileDiff) -> String {
    match (&file.old_path, &file.new_path) {
        (Some(old), Some(new)) if old != new => format!("{old} → {new}"),
        _ => file.display_path().to_owned(),
    }
}

/// One row of the changed-files list: change kind, path, line counts;
/// `file` `None` is the "All files" row. A click opens it in place of the
/// graph, or, when `open` (shown), brings the graph back.
fn render_changed_file(
    doc: &Arc<DiffDoc>,
    file: Option<usize>,
    open: bool,
    cx: &mut Context<RepoView>,
) -> AnyElement {
    let (label, token, path, stats) = match file {
        None => {
            let (added, removed) = doc
                .files
                .iter()
                .map(FileDiff::line_stats)
                .fold((0, 0), |(a, r), (x, y)| (a + x, r + y));
            (
                "∗",
                T::FgMuted,
                "All files".to_owned(),
                format!("+{added} −{removed}"),
            )
        }
        Some(i) => {
            let file = &doc.files[i];
            let (label, token) = match file.kind {
                ChangeKind::Added | ChangeKind::Copied => ("+", T::DiffAddFg),
                ChangeKind::Deleted => ("−", T::DiffDelFg),
                ChangeKind::Renamed
                | ChangeKind::TypeChanged
                | ChangeKind::Modified
                | ChangeKind::Unmodified => ("✎", T::StatusDirty),
            };
            let stats = if file.binary {
                "binary".to_owned()
            } else {
                let (added, removed) = file.line_stats();
                format!("+{added} −{removed}")
            };
            (label, token, file_title(file), stats)
        }
    };
    let hover = color(cx, T::BgHover);
    div()
        .id(SharedString::from(format!(
            "cf-{}",
            file.map_or_else(|| "all".to_owned(), |i| i.to_string())
        )))
        .w_full()
        .h(row_height(cx))
        .flex()
        .items_center()
        .gap_2()
        .px_2()
        .cursor_pointer()
        .when(open, |d| d.bg(color(cx, T::BgSelected)))
        .hover(move |s| s.bg(hover))
        .on_click(cx.listener(move |v, _, window, cx| {
            if open {
                v.close_diff(window, cx);
            } else {
                v.focus.focus(window, cx);
                v.open_commit_file(file, cx);
            }
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

impl Staging for RepoView {
    fn core(&self) -> &Arc<Core> {
        &self.core
    }

    fn repo_path(&self) -> &RepoPath {
        &self.path
    }

    fn changes(&self) -> &[FileChange] {
        &self.changes
    }

    fn panel(&mut self) -> &mut staging::Panel {
        &mut self.staging
    }

    fn run_job(&mut self, label: String, cx: &mut Context<Self>, job: staging::Job) {
        self.run(label, cx, job);
    }

    fn report(&mut self, text: String, cx: &mut Context<Self>) {
        self.status = Some(text);
        cx.notify();
    }

    fn open_file(&self) -> Option<(&str, DiffSide)> {
        match &self.open_diff {
            Some(OpenDiff::Worktree(file, side, _)) => Some((file, *side)),
            _ => None,
        }
    }

    fn click_file(
        &mut self,
        file: String,
        side: DiffSide,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.open_file() == Some((&file, side)) {
            self.close_diff(window, cx);
        } else {
            self.focus.focus(window, cx);
            self.open_worktree_file(file, side, cx);
        }
    }
}

impl Render for RepoView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let refs = self.render_refs(cx);
        let centre = if self.open_diff.is_some() {
            self.render_open_diff(cx)
        } else {
            self.render_graph(cx)
        };
        let details = self.render_details(cx);
        let ui = self.core.ui_state().unwrap_or_default();
        let body = div()
            .flex_1()
            .flex()
            .overflow_hidden()
            .on_drag_move(cx.listener(Self::on_pane_drag))
            .child(
                div()
                    .w(px(ui.refs_width))
                    .flex_none()
                    .bg(color(cx, T::BgPanel))
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
                    .on_key_down(cx.listener(|v, e: &KeyDownEvent, window, cx| {
                        v.on_key(e, window, cx);
                    }))
                    .child(centre),
            )
            .child(splitter(Pane::Details, cx))
            .child(
                div()
                    .w(px(ui.details_width))
                    .flex_none()
                    .bg(surface_between(cx, 0.5))
                    .flex()
                    .flex_col()
                    .child(details),
            );
        let confirm = self.render_confirm(cx);
        let menu = self.render_menu(cx);
        let name_form = self.render_name_form(cx);
        div()
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .child(self.render_toolbar(cx))
            .children(name_form)
            .child(body)
            .children(self.status.as_deref().map(|s| {
                div()
                    .id("status")
                    .flex_none()
                    .px_3()
                    .py_1()
                    .max_h(px(120.0))
                    .overflow_hidden()
                    .text_xs()
                    .text_color(color(cx, T::FgMuted))
                    .cursor_pointer()
                    .tooltip(|_, cx| TextTooltip::view(&["Click to dismiss".into()], cx))
                    .on_click(cx.listener(|v, _, _, cx| {
                        v.status = None;
                        cx.notify();
                    }))
                    .child(condense_status(s))
            }))
            .children(confirm)
            .children(menu)
    }
}

#[cfg(test)]
mod menu_tests {
    use super::*;

    #[test]
    fn unmerged_branch_refusal_is_recognized() {
        let failed = |stderr: &str| {
            archipel_ops::OpError::Git(archipel_model::GitError::CommandFailed {
                command: "git branch -d feat".into(),
                exit_code: Some(1),
                stderr: stderr.into(),
            })
        };
        assert!(is_not_merged(&failed(
            "error: the branch 'feat' is not fully merged\nhint: If you are sure you want \
             to delete it, run 'git branch -D feat'"
        )));
        assert!(is_not_merged(&failed(
            "erreur : la branche 'feat' n'est pas totalement fusionnée\nastuce : lancez \
             'git branch -D feat'"
        )));
        assert!(!is_not_merged(&failed(
            "error: cannot delete branch 'feat' used by worktree at 'C:/w'"
        )));
    }

    fn tip(name: &str, kind: RefKind) -> RefTip {
        RefTip {
            name: name.into(),
            kind,
            target: Oid::from_bytes(&[1; 20]).unwrap(),
        }
    }

    fn actions(entries: &[MenuEntry]) -> Vec<&MenuAction> {
        entries
            .iter()
            .filter_map(|e| match e {
                MenuEntry::Item { action, .. } => Some(action),
                MenuEntry::Separator => None,
            })
            .collect()
    }

    #[test]
    fn submodule_menu_opens_or_initializes() {
        let sub = |initialized| SubmoduleInfo {
            name: "libs/foo".into(),
            path: RepoPath::new("/repo/libs/foo"),
            initialized,
            pinned: None,
            checked_out: None,
        };
        let entries = menu_entries(&MenuTarget::Submodule(sub(true)), None, |_| true);
        let initialized = actions(&entries);
        assert!(initialized.contains(&&MenuAction::OpenSubmodule(sub(true).path)));
        assert!(initialized.contains(&&MenuAction::UpdateSubmodule {
            name: "libs/foo".into(),
            init: false
        }));
        let entries = menu_entries(&MenuTarget::Submodule(sub(false)), None, |_| true);
        let uninitialized = actions(&entries);
        assert!(
            !uninitialized
                .iter()
                .any(|a| matches!(a, MenuAction::OpenSubmodule(_)))
        );
        assert!(uninitialized.contains(&&MenuAction::UpdateSubmodule {
            name: "libs/foo".into(),
            init: true
        }));
    }

    #[test]
    fn commit_menu_offers_deleting_its_refs_but_not_the_current_branch() {
        let oid = Oid::from_bytes(&[2; 20]).unwrap();
        let refs = vec![
            tip("main", RefKind::LocalBranch),
            tip("feat", RefKind::LocalBranch),
            tip("v1", RefKind::Tag),
            tip("origin/main", RefKind::RemoteBranch),
        ];
        let entries = menu_entries(&MenuTarget::Commit(oid, refs), Some("main"), |_| true);
        let actions = actions(&entries);
        assert!(actions.contains(&&MenuAction::Name(NameForm::Branch(Some(oid)))));
        assert!(actions.contains(&&MenuAction::Name(NameForm::Tag(oid))));
        assert!(actions.contains(&&MenuAction::DeleteBranch("feat".into(), DeleteMode::Safe)));
        assert!(actions.contains(&&MenuAction::DeleteTag("v1".into())));
        assert!(!actions.iter().any(|a| matches!(
            a,
            MenuAction::DeleteBranch(n, _) if n == "main"
        )));
        assert!(
            !actions
                .iter()
                .any(|a| matches!(a, MenuAction::DeleteRemoteBranch(_)))
        );
    }

    #[test]
    fn current_branch_menu_has_no_checkout_merge_or_delete() {
        let entries = menu_entries(
            &MenuTarget::Ref(tip("main", RefKind::LocalBranch)),
            Some("main"),
            |_| true,
        );
        for a in actions(&entries) {
            assert!(
                !matches!(
                    a,
                    MenuAction::Checkout(_)
                        | MenuAction::Merge(_)
                        | MenuAction::Rebase(_)
                        | MenuAction::DeleteBranch(..)
                ),
                "{a:?}"
            );
        }
    }

    #[test]
    fn other_branch_menu_checks_out_merges_and_deletes() {
        let entries = menu_entries(
            &MenuTarget::Ref(tip("feat", RefKind::LocalBranch)),
            Some("main"),
            |_| true,
        );
        let actions = actions(&entries);
        assert!(
            actions.contains(&&MenuAction::Checkout(CheckoutTarget::Branch(
                "feat".into()
            )))
        );
        assert!(actions.contains(&&MenuAction::Merge("feat".into())));
        assert!(actions.contains(&&MenuAction::Name(NameForm::Rename("feat".into()))));
        assert!(actions.contains(&&MenuAction::DeleteBranch("feat".into(), DeleteMode::Force)));
    }

    #[test]
    fn dropping_a_branch_offers_merge_and_rebase() {
        let entries = |s: RefTip, t: RefTip| {
            let entries = drop_entries(&s, &t, Some("main"));
            entries
                .into_iter()
                .filter_map(|e| match e {
                    MenuEntry::Item { action, .. } => Some(action),
                    MenuEntry::Separator => None,
                })
                .collect::<Vec<_>>()
        };
        let main = tip("main", RefKind::LocalBranch);
        let feat = tip("feat", RefKind::LocalBranch);
        let remote = tip("origin/feat", RefKind::RemoteBranch);
        // feat dropped on the current branch: merge into it, or checkout
        // feat and rebase it.
        assert_eq!(
            entries(feat.clone(), main.clone()),
            vec![
                MenuAction::Merge("feat".into()),
                MenuAction::RebaseBranch {
                    branch: "feat".into(),
                    onto: "main".into()
                },
            ]
        );
        // The current branch dropped on feat.
        assert_eq!(
            entries(main.clone(), feat.clone()),
            vec![
                MenuAction::MergeInto {
                    rev: "main".into(),
                    into: "feat".into()
                },
                MenuAction::Rebase("feat".into()),
            ]
        );
        // A remote branch cannot be rebased nor merged into.
        assert_eq!(
            entries(remote.clone(), main.clone()),
            vec![MenuAction::Merge("origin/feat".into())]
        );
        assert_eq!(
            entries(main.clone(), remote.clone()),
            vec![MenuAction::Rebase("origin/feat".into())]
        );
        assert!(entries(main.clone(), main).is_empty());
    }

    #[test]
    fn tag_and_remote_menus() {
        let tag = menu_entries(&MenuTarget::Ref(tip("v1", RefKind::Tag)), None, |_| false);
        let tag = actions(&tag);
        assert!(tag.contains(&&MenuAction::PushTag("v1".into())));
        assert!(tag.contains(&&MenuAction::DeleteTag("v1".into())));
        let remote = menu_entries(
            &MenuTarget::Ref(tip("origin/feat", RefKind::RemoteBranch)),
            Some("main"),
            |_| false,
        );
        let remote = actions(&remote);
        assert!(
            remote.contains(&&MenuAction::Checkout(CheckoutTarget::TrackRemote(
                "origin/feat".into()
            )))
        );
        assert!(remote.contains(&&MenuAction::DeleteRemoteBranch("origin/feat".into())));
    }

    #[test]
    fn only_losing_or_remote_actions_ask_first() {
        let oid = Oid::from_bytes(&[3; 20]).unwrap();
        assert!(
            MenuAction::Reset(oid, ResetMode::Hard)
                .confirmation()
                .is_some()
        );
        assert!(
            MenuAction::Reset(oid, ResetMode::Mixed)
                .confirmation()
                .is_none()
        );
        assert!(
            MenuAction::DeleteBranch("x".into(), DeleteMode::Force)
                .confirmation()
                .is_some()
        );
        assert!(
            MenuAction::DeleteBranch("x".into(), DeleteMode::Safe)
                .confirmation()
                .is_none()
        );
        assert!(
            MenuAction::DeleteRemoteBranch("origin/x".into())
                .confirmation()
                .is_some()
        );
        assert!(MenuAction::DeleteTag("v1".into()).confirmation().is_none());
    }
}
