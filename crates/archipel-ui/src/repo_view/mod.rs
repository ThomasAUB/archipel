//! A repository tab (spec §5): refs, the virtualized commit graph and a
//! details pane following the selection: a commit's or stash's files, or,
//! for the uncommitted-changes row, the staging lists and the commit box.
//! A file clicked there opens its diff in place of the graph.
//!
//! Every Git call runs on a background thread; the graph rows come from the
//! on-disk topology cache and commit metadata from its bounded LRU.
//!
//! The view is split by concern: [`loading`] (background reads and the
//! selection), [`graph_paint`] (the graph list), [`refs_panel`],
//! [`details`], [`navigation`] (keys and the open diff), [`operations`]
//! (Git writes), [`menu`] (right-click menus) and [`dialogs`].

mod details;
mod dialogs;
mod graph_paint;
mod loading;
mod menu;
mod navigation;
mod operations;
mod refs_panel;

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::sync::{Arc, Mutex, PoisonError};

use archipel_app::App as Core;
use archipel_app::forge::{BranchCi, ForgeLimit, ForgeStatus, ci_cell, ci_tooltip};
use archipel_graph::HistoryView;
use archipel_model::changes::FileChange;
use archipel_model::history::CommitMeta;
use archipel_model::ports::DiffSide;
use archipel_model::refs::StashEntry;
use archipel_model::submodule::SubmoduleInfo;
use archipel_model::{Oid, RepoPath};
use archipel_ops::OpClass;
use archipel_theme::ColorToken as T;
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, Context, DragMoveEvent, Entity, EventEmitter, FocusHandle, KeyDownEvent, Pixels,
    Point, SharedString, Subscription, UniformListScrollHandle, Window, div, px,
};

use crate::diff_view::DiffDoc;
use crate::staging::{self, Staging, StagingEvent};
use crate::theme::{color, radius, repo_surface, staging_surface, tone};
use crate::widgets::{Pane, PaneDrag, TextTooltip, condense_status, resize_pane, splitter};

use dialogs::{Confirm, NameForm};
use loading::RefsState;
use menu::MenuTarget;
use refs_panel::RefList;

/// Lanes shown before the graph column stops growing (wide graphs clip).
const MAX_LANES_SHOWN: u32 = 24;

/// A diff open in the centre pane, in place of the graph.
enum OpenDiff {
    /// The selected commit's or stash's diff: one file (index into the
    /// details diff), or all of them (`None`).
    Commit(Option<usize>, Arc<DiffDoc>),
    /// A working-copy file on one side, its hunks staged or unstaged from
    /// the diff; `None` while it loads.
    Worktree(String, DiffSide, Option<Arc<DiffDoc>>),
}

/// A stash drawn in the graph, in a row of its own just above the commit
/// it was made on.
struct PlacedStash {
    entry: StashEntry,
    /// Graph row of `entry.base`.
    row: u32,
    /// Lane of the stash nodes: see [`graph_paint::stash_lane`].
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

type SharedView = Arc<Mutex<HistoryView>>;

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
    forge_limit: Option<ForgeLimit>,
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
        let (ci, forge, forge_limit) = row
            .map(|r| (r.ci, r.forge, r.forge_limit))
            .unwrap_or_default();
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
            forge_limit,
            scroll: UniformListScrollHandle::new(),
            focus: cx.focus_handle(),
        };
        view.load_graph(cx);
        view.load_refs(cx);
        view.load_changes(cx);
        view.load_submodules(cx);
        view
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

    /// Rows before the first commit in the graph list: the uncommitted
    /// changes row, shown while the working copy is dirty.
    fn wip_rows(&self) -> u32 {
        u32::from(!self.changes.is_empty())
    }

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
        let now = archipel_model::Timestamp::now();
        let cell = ci_cell(&self.ci, &self.forge, self.forge_limit, now);
        if cell.text.is_empty() {
            return None;
        }
        let tooltip = ci_tooltip(&self.ci, &self.forge, self.forge_limit, now);
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
}

impl EventEmitter<StagingEvent> for RepoView {}

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

    fn run_noted_job(
        &mut self,
        class: OpClass,
        label: String,
        cx: &mut Context<Self>,
        job: staging::NotedJob,
    ) {
        self.run_noted(class, label, cx, job);
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
                    .bg(repo_surface(cx))
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
                    .bg(staging_surface(cx))
                    .flex()
                    .flex_col()
                    .child(details),
            );
        let confirm = self.render_confirm(cx);
        let force_push = staging::render_force_push(self, cx);
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
            .children(force_push)
            .children(menu)
    }
}
