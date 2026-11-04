//! A repository tab (spec §5): virtualized commit graph with commit details
//! and diff, and the working copy with file / hunk staging and a commit box.
//!
//! Every Git call runs on a background thread; the graph rows come from the
//! on-disk topology cache and commit metadata from its bounded LRU.

use std::collections::HashMap;
use std::ops::Range;
use std::sync::{Arc, Mutex, PoisonError};

use archipel_app::App as Core;
use archipel_graph::{EdgeKind, GraphRow, HistoryView};
use archipel_model::changes::{FileChange, FileState};
use archipel_model::diff::{FileDiff, LineOrigin};
use archipel_model::history::CommitMeta;
use archipel_model::ports::DiffSide;
use archipel_model::requests::{
    CommitMode, CommitRequest, FetchRequest, PatchDirection, PullMode, PushRequest, StashApplyMode,
    StashRequest,
};
use archipel_model::{CancelToken, NoProgress, Oid, RepoPath, Timestamp};
use archipel_ops::patch::hunk_patch;
use archipel_theme::ColorToken as T;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, App, Bounds, Context, Entity, PathBuilder, Pixels, SharedString, Window, canvas,
    div, point, px, uniform_list,
};

use crate::theme::{color, lane, mono_font, radius, row_height};
use crate::widgets::{button, primary_button, section};

/// Width of one graph lane.
const LANE_WIDTH: f32 = 14.0;
/// Lanes shown before the graph column stops growing (wide graphs clip).
const MAX_LANES_SHOWN: u32 = 24;
/// Metadata entries kept by the view (rows around the visible window).
const META_KEEP: usize = 4000;

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
}

impl DiffDoc {
    fn new(files: Vec<FileDiff>) -> Self {
        let mut rows = Vec::new();
        for (f, file) in files.iter().enumerate() {
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
            }
        }
        Self { files, rows }
    }
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
    mode: Mode,
    graph: Option<SharedView>,
    row_count: u32,
    lanes: u32,
    graph_error: Option<String>,
    meta: HashMap<u32, Arc<CommitMeta>>,
    pending: Option<Range<u32>>,
    selected: Option<(u32, Oid)>,
    details: Option<(Oid, Arc<DiffDoc>)>,
    changes: Vec<FileChange>,
    file_diff: Option<(String, DiffSide, Arc<DiffDoc>)>,
    message: Entity<InputState>,
    amend: bool,
    status: Option<String>,
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
            details: None,
            changes: Vec::new(),
            file_diff: None,
            message,
            amend: false,
            status: None,
        };
        view.load_graph(cx);
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
                cx.notify();
            });
        })
        .detach();
    }

    /// Reloads graph and changes (after an operation).
    fn reload(&mut self, cx: &mut Context<Self>) {
        self.core.refresh_repo(self.path.id());
        self.load_graph(cx);
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

    /// Runs a write on a background thread, then reloads.
    fn run(
        &mut self,
        label: &'static str,
        cx: &mut Context<Self>,
        job: impl FnOnce(&archipel_ops::RepoActions, &RepoPath) -> Result<(), archipel_ops::OpError>
        + Send
        + 'static,
    ) {
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
                    if mode == Mode::Changes {
                        v.load_changes(cx);
                    }
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
                        button("r-push", "Push", cx).on_click(cx.listener(|v, _, _, cx| {
                            v.run("Push", cx, |a, p| {
                                a.push(
                                    p,
                                    &PushRequest::default(),
                                    &NoProgress,
                                    &CancelToken::new(),
                                    None,
                                )
                            });
                        })),
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
        let count = usize::try_from(self.row_count).unwrap_or(0);
        uniform_list(
            "graph",
            count,
            cx.processor(|this: &mut Self, range: Range<usize>, _window, cx| {
                let start = u32::try_from(range.start).unwrap_or(0);
                let end = u32::try_from(range.end).unwrap_or(0);
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
                rows.iter().map(|row| this.render_row(row, cx)).collect()
            }),
        )
        .flex_1()
        .into_any_element()
    }

    fn render_row(&mut self, row: &GraphRow, cx: &mut Context<Self>) -> AnyElement {
        let meta = self.meta.get(&row.row).cloned();
        let selected = self.selected.is_some_and(|(r, _)| r == row.row);
        // Same width on every row so the text columns line up.
        let width = px(lane_x(self.lanes));
        let lanes = graph_cell(row, cx);
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
        let badges = row.refs.iter().map(|r| {
            div()
                .px_1()
                .rounded(radius(cx))
                .text_xs()
                .bg(color(cx, T::AccentSubtle))
                .text_color(color(cx, T::Accent))
                .child(r.name.clone())
        });
        let oid = row.oid;
        let index = row.row;
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
            .on_click(cx.listener(move |v, _, _, cx| v.select_commit(index, oid, cx)))
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

    fn render_details(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let Some((oid, doc)) = &self.details else {
            return div()
                .p_3()
                .text_color(color(cx, T::FgMuted))
                .child("Select a commit")
                .into_any_element();
        };
        let (oid, doc) = (*oid, doc.clone());
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
        div()
            .flex_1()
            .flex()
            .flex_col()
            .overflow_hidden()
            .child(el)
            .child(diff_list(
                SharedString::from(format!("details-{}", oid.to_hex())),
                doc,
                None,
                cx,
            ))
            .into_any_element()
    }

    fn render_changes(&mut self, cx: &mut Context<Self>) -> AnyElement {
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
                FileState::Untracked => ("U", T::StatusAhead),
                FileState::Conflicted => ("!", T::StatusConflict),
                FileState::Tracked { .. } => ("M", T::StatusDirty),
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
        let amend = self.amend;
        let left = div()
            .w(px(380.0))
            .flex_none()
            .flex()
            .flex_col()
            .border_r_1()
            .border_color(color(cx, T::BorderSubtle))
            .child(section(format!("UNSTAGED ({})", unstaged.len()), cx))
            .child(
                div()
                    .id("unstaged")
                    .flex_1()
                    .overflow_y_scroll()
                    .children(unstaged_rows),
            )
            .child(section(format!("STAGED ({})", staged.len()), cx))
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
                    .child(diff_list(id, doc, Some(side), cx))
                    .into_any_element()
            }
        };
        div()
            .flex_1()
            .flex()
            .overflow_hidden()
            .child(left)
            .child(right)
            .into_any_element()
    }
}

/// Horizontal center of lane `col` (lanes are a few dozen at most).
#[allow(clippy::cast_precision_loss)]
fn lane_x(col: u32) -> f32 {
    LANE_WIDTH * col as f32 + LANE_WIDTH / 2.0
}

/// Paints the lanes and the commit node of one graph row.
fn graph_cell(row: &GraphRow, cx: &App) -> impl IntoElement {
    let edges: Vec<(EdgeKind, u32, u32, gpui_kit::Hsla)> = row
        .edges
        .iter()
        .map(|e| (e.kind, e.from, e.to, lane(cx, e.color)))
        .collect();
    let node = (row.column, lane(cx, row.color));
    canvas(
        |_, _, _| {},
        move |bounds: Bounds<Pixels>, (), window, _| {
            let x = |col: u32| bounds.origin.x + px(lane_x(col));
            let (top, bottom) = (bounds.origin.y, bounds.origin.y + bounds.size.height);
            let mid = bounds.origin.y + bounds.size.height / 2.0;
            for (kind, from, to, c) in &edges {
                let (a, b) = match kind {
                    EdgeKind::Through => (point(x(*from), top), point(x(*to), bottom)),
                    EdgeKind::Incoming => (point(x(*from), top), point(x(*to), mid)),
                    EdgeKind::Outgoing => (point(x(*from), mid), point(x(*to), bottom)),
                };
                let mut path = PathBuilder::stroke(px(1.6));
                path.move_to(a);
                if a.x == b.x {
                    path.line_to(b);
                } else {
                    path.curve_to(b, point(b.x, a.y));
                }
                if let Ok(p) = path.build() {
                    window.paint_path(p, *c);
                }
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
        },
    )
    .size_full()
}

/// A virtualized diff: only the visible rows are built each frame. With
/// `side`, hunk headers get a stage / unstage button.
fn diff_list(
    id: SharedString,
    doc: Arc<DiffDoc>,
    side: Option<DiffSide>,
    cx: &mut Context<RepoView>,
) -> AnyElement {
    let count = doc.rows.len();
    uniform_list(
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
    )
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
            let line = &doc.files[f].hunks[h].lines[l];
            let (prefix, bg) = match line.origin {
                LineOrigin::Added => ("+", Some(color(cx, T::DiffAddBg))),
                LineOrigin::Removed => ("-", Some(color(cx, T::DiffDelBg))),
                LineOrigin::Context => (" ", None),
            };
            base.font_family(mono_font(cx))
                .text_xs()
                .when_some(bg, gpui_kit::Styled::bg)
                .child(format!("{prefix}{}", line.content.trim_end_matches('\r')))
                .into_any_element()
        }
    }
}

impl Render for RepoView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body: AnyElement = match self.mode {
            Mode::History => {
                let graph = self.render_graph(cx);
                let details = self.render_details(cx);
                div()
                    .flex_1()
                    .flex()
                    .overflow_hidden()
                    .child(
                        div()
                            .flex_1()
                            .flex()
                            .flex_col()
                            .overflow_hidden()
                            .child(graph),
                    )
                    .child(
                        div()
                            .w(px(460.0))
                            .flex_none()
                            .flex()
                            .flex_col()
                            .border_l_1()
                            .border_color(color(cx, T::BorderSubtle))
                            .child(details),
                    )
                    .into_any_element()
            }
            Mode::Changes => self.render_changes(cx),
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(self.render_toolbar(cx))
            .child(body)
            .children(self.status.clone().map(|s| {
                div()
                    .px_3()
                    .py_1()
                    .text_xs()
                    .text_color(color(cx, T::FgMuted))
                    .child(s)
            }))
    }
}
