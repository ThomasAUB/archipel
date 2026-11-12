//! The dashboard tab: toolbar (filters, category filter, view chips),
//! the column header and the repo rows, and the quick edit pane lying over
//! their right edge.

use std::sync::Arc;

use archipel_app::dashboard::{Cell, CellBadge, DashboardRow};
use archipel_app::{App as Core, Command, Extensions};
use archipel_model::forge::CiRun;
use archipel_model::ui_state::{SortDirection, TabState, UiState};
use archipel_model::{RepoId, Timestamp};
use archipel_theme::ColorToken as T;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, ClickEvent, Context, DragMoveEvent, Entity, KeyDownEvent, MouseButton,
    MouseDownEvent, Pixels, SharedString, StyleRefinement, WeakEntity, Window, div, px,
};

use crate::ci_tooltip::CiTooltip;
use crate::forecast_badge::pill;
use crate::quick_edit::{QuickEdit, QuickEditEvent};
use crate::theme::{color, radius, row_height, staging_surface, text_size, tone, ui_font};
use crate::widgets::{
    Pane, PaneDrag, TextTooltip, button, chip, popup_at, primary_button, resize_pane, splitter,
    unmodified,
};

use super::Workspace;

/// Relative widths of the dashboard columns.
pub(super) const WIDTHS: [f32; 8] = [2.0, 1.4, 1.0, 1.2, 1.0, 0.8, 1.0, 0.9];

impl Workspace {
    /// Opens the quick edit pane on a repo, or closes it when it already
    /// shows that repo.
    fn toggle_quick_edit(&mut self, id: RepoId, window: &mut Window, cx: &mut Context<Self>) {
        if self
            .quick_edit
            .as_ref()
            .is_some_and(|(p, _)| p.read(cx).repo() == id)
        {
            self.quick_edit = None;
            self.focus.focus(window, cx);
            cx.notify();
            return;
        }
        let Some(row) = self.core.rows().into_iter().find(|r| r.id == id) else {
            return;
        };
        let core = self.core.clone();
        let pane = cx.new(|cx| QuickEdit::new(core, &row, window, cx));
        let sub = cx.subscribe_in(
            &pane,
            window,
            |this: &mut Self, _, event: &QuickEditEvent, window, cx| {
                match event {
                    QuickEditEvent::Closed => {
                        this.quick_edit = None;
                        this.focus.focus(window, cx);
                    }
                    QuickEditEvent::OpenTab(id) => {
                        this.quick_edit = None;
                        this.dispatch(Command::OpenRepoTab(*id), cx);
                    }
                }
                cx.notify();
            },
        );
        let staging = cx.subscribe_in(&pane, window, Self::on_staging);
        // Its open diff is drawn here, in place of the rows.
        let redraw = cx.observe(&pane, |_, _, cx| cx.notify());
        self.quick_edit = Some((pane, [sub, staging, redraw]));
        cx.notify();
    }

    /// ↑ / ↓ / Esc on the dashboard go to the quick edit pane (its open
    /// diff first), unless a menu or overlay is up or an input has focus.
    pub(super) fn quick_edit_key(
        &mut self,
        e: &KeyDownEvent,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        if !unmodified(e.keystroke.modifiers) {
            return;
        }
        if self.overlay.is_some() || self.row_menu.is_some() {
            return;
        }
        let on_dashboard = self
            .core
            .ui_state()
            .is_some_and(|ui| !matches!(ui.tabs.get(ui.active_tab), Some(TabState::Repo(_))));
        let Some((pane, _)) = &self.quick_edit else {
            return;
        };
        let keys_free = self.focus.is_focused(window) || pane.read(cx).is_focused(window);
        if on_dashboard && keys_free && pane.update(cx, |p, cx| p.on_key(&e.keystroke.key, cx)) {
            cx.stop_propagation();
        }
    }

    fn render_dashboard_toolbar(&mut self, ui: &UiState, cx: &mut Context<Self>) -> AnyElement {
        let active = &ui.filters;
        let ext = self.core.extensions().clone();
        let hidden = self.core.hidden_count();
        let view_chips = div()
            .flex()
            .gap_1()
            .child(
                chip("view-tree", "Tree", ui.show_dependency_tree, cx).on_click(
                    cx.listener(|this, _, _, cx| this.dispatch(Command::ToggleDependencyTree, cx)),
                ),
            )
            .child(
                chip("view-branches", "Branches…", false, cx)
                    .on_click(cx.listener(|this, _, window, cx| this.open_branches(window, cx))),
            )
            .when(hidden > 0 || ui.show_hidden, |d| {
                d.child(
                    chip(
                        "view-hidden",
                        format!("Hidden ({hidden})"),
                        ui.show_hidden,
                        cx,
                    )
                    .on_click(
                        cx.listener(|this, _, _, cx| this.dispatch(Command::ToggleShowHidden, cx)),
                    ),
                )
            });
        let scan = match self.scan {
            Some((found, true)) => Some(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_xs()
                    .text_color(color(cx, T::FgMuted))
                    .child(format!("Scanning the folder… {found} found"))
                    .child(button("scan-stop", "Stop", cx).on_click(
                        cx.listener(|this, _, _, cx| this.dispatch(Command::CancelScan, cx)),
                    )),
            ),
            _ => None,
        };
        let category_dropdown = self.render_category_filter(ui, cx);
        let filters = ext.filters.filters().iter().map(|f| {
            let id = f.id().to_owned();
            let active = active.contains(&id);
            chip(
                SharedString::from(format!("filter-{id}")),
                f.label().to_owned(),
                active,
                cx,
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.dispatch(Command::ToggleFilter(id.clone()), cx);
            }))
        });
        div()
            .flex()
            .flex_wrap()
            .justify_between()
            .items_center()
            .gap_2()
            .px_3()
            .py_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .flex_wrap()
                    .gap_3()
                    .child(div().flex().flex_wrap().gap_1().children(filters))
                    .children(category_dropdown)
                    .child(view_chips)
                    .children(scan),
            )
            .into_any_element()
    }

    #[allow(clippy::too_many_lines)] // declarative element tree
    pub(super) fn render_dashboard(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if self.core.project().is_none() {
            return div()
                .flex_1()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_3()
                .text_color(color(cx, T::FgMuted))
                .child("Archipel works on projects: groups of repositories.")
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .child(
                            primary_button("new-project", "New project from a folder…", cx)
                                .on_click(
                                    cx.listener(|this, _, _, cx| this.new_project_from_folder(cx)),
                                ),
                        )
                        .child(
                            button("open-project", "Open an archipel.toml…", cx)
                                .on_click(cx.listener(|this, _, _, cx| this.open_project_file(cx))),
                        ),
                )
                .into_any_element();
        }
        let ui = self.core.ui_state().unwrap_or_default();
        let ext = self.core.extensions().clone();
        let rows = self.core.visible_rows();
        let now = Timestamp::now();
        let toolbar = self.render_dashboard_toolbar(&ui, cx);
        let widths = WIDTHS;
        let header = div()
            .flex()
            .px_3()
            .h(row_height(cx))
            .items_center()
            .text_xs()
            .text_color(color(cx, T::FgSubtle))
            .children(ext.columns.columns().iter().enumerate().map(|(i, col)| {
                let id = col.id().to_owned();
                let arrow = match (
                    ui.sort_column.as_deref() == Some(col.id()),
                    ui.sort_direction,
                ) {
                    (true, SortDirection::Ascending) => " ▲",
                    (true, SortDirection::Descending) => " ▼",
                    _ => "",
                };
                div()
                    .id(SharedString::from(format!("col-{id}")))
                    .flex_grow(widths.get(i).copied().unwrap_or(1.0))
                    .flex_basis(px(0.0))
                    .min_w_0()
                    .truncate()
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.dispatch(Command::SortBy(id.clone()), cx);
                    }))
                    .child(format!("{}{arrow}", col.header().to_uppercase()))
            }));
        let selected = ui.batch_selection();
        let body: Vec<AnyElement> = if ui.show_dependency_tree {
            let tree = self.core.dependency_tree();
            self.render_tree(&tree, &rows, &selected, cx)
        } else {
            self.dash_rows
                .retain(|id, _| rows.iter().any(|r| r.id == *id));
            rows.iter()
                .map(|row| {
                    let content = RowContent::new(&ext, row, now, &selected);
                    let two_lines = content
                        .cells
                        .first()
                        .is_some_and(|c| c.cell.detail.is_some());
                    let height = row_box_height(two_lines, window, cx);
                    self.dash_row(content, cx)
                        .cached(StyleRefinement::default().w_full().h(height))
                        .into_any_element()
                })
                .collect()
        };
        // A repo removed or hidden since its pane opened closes the pane.
        if let Some((pane, _)) = &self.quick_edit {
            let repo = pane.read(cx).repo();
            if !rows.iter().any(|r| r.id == repo) {
                self.quick_edit = None;
            }
        }
        let pane = self.quick_edit.as_ref().map(|(p, _)| p.clone());
        // A file clicked in the pane shows its diff in place of the rows.
        let diff = pane
            .as_ref()
            .and_then(|p| p.update(cx, QuickEdit::render_diff));
        let diff_shown = diff.is_some();
        let main = match diff {
            Some(diff) => diff,
            None => div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .overflow_hidden()
                .child(toolbar)
                .child(header)
                .child(
                    div()
                        .id("dash-rows")
                        .flex_1()
                        .overflow_y_scrollbar()
                        .children(body),
                )
                .into_any_element(),
        };
        let width = ui.quick_edit_width;
        // The pane lies over the rows' right edge instead of narrowing them,
        // so toggling it leaves the list in place. A diff (only shown with
        // the pane open) is kept clear of it, splitter included.
        let main = if diff_shown {
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .pr(px(width + 5.0))
                .child(main)
                .into_any_element()
        } else {
            main
        };
        div()
            .flex_1()
            .flex()
            .relative()
            .overflow_hidden()
            .on_drag_move(cx.listener(|this, e: &DragMoveEvent<PaneDrag>, _, cx| {
                // Every row listening for `PaneDrag` sees every splitter's
                // drag: only the quick edit pane is ours.
                let pane = e.drag(cx).0;
                if pane != Pane::QuickEdit {
                    return;
                }
                let (x, bounds) = (e.event.position.x, e.bounds);
                this.core
                    .update_ui_state(|ui| resize_pane(ui, pane, x, bounds));
                cx.notify();
            }))
            .child(main)
            .when_some(pane, |d, pane| {
                d.child(
                    div()
                        .id("quick-edit-overlay")
                        .absolute()
                        .top_0()
                        .bottom_0()
                        .right_0()
                        .flex()
                        .occlude()
                        .child(splitter(Pane::QuickEdit, cx).bg(staging_surface(cx)))
                        .child(div().w(px(width)).h_full().flex().flex_col().child(pane)),
                )
            })
            .into_any_element()
    }

    /// The view of a dashboard row, notified only when what it shows
    /// changes: the others are drawn again from the last frame.
    fn dash_row(&mut self, content: RowContent, cx: &mut Context<Self>) -> Entity<DashRow> {
        if let Some(view) = self.dash_rows.get(&content.id) {
            if view.read(cx).content != content {
                view.update(cx, |r, cx| {
                    r.content = content;
                    cx.notify();
                });
            }
            return view.clone();
        }
        let id = content.id;
        let workspace = cx.weak_entity();
        let core = self.core.clone();
        let view = cx.new(|_| DashRow {
            workspace,
            core,
            content,
        });
        self.dash_rows.insert(id, view.clone());
        view
    }

    /// The clickable frame of a dashboard row (selection, dimming, menus).
    pub(super) fn row_shell(
        row: &DashboardRow,
        selected: &[RepoId],
        cx: &mut Context<Self>,
    ) -> gpui_kit::Stateful<gpui_kit::Div> {
        let id = row.id;
        row_frame(id, selected.contains(&id), row.stale, row.hidden, cx)
            .on_click(cx.listener(move |this, e: &ClickEvent, window, cx| {
                this.row_clicked(id, e, window, cx);
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                    this.open_row_menu(id, e, cx);
                }),
            )
    }

    fn row_clicked(
        &mut self,
        id: RepoId,
        e: &ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let command = row_click(id, e);
        let plain = command == Command::Select(Some(id));
        self.dispatch(command, cx);
        if plain {
            self.toggle_quick_edit(id, window, cx);
        }
    }

    fn open_row_menu(&mut self, id: RepoId, e: &MouseDownEvent, cx: &mut Context<Self>) {
        self.row_menu = Some((id, e.position));
        cx.notify();
    }

    /// The category filter: one dropdown button whatever the number of
    /// categories, its menu a checklist (several can be selected).
    #[allow(clippy::too_many_lines)] // declarative element tree
    fn render_category_filter(
        &mut self,
        ui: &UiState,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let keys = self.core.category_filter_keys();
        if keys.is_empty() {
            return None;
        }
        let label_of = |key: &str| {
            if key.is_empty() {
                "Uncategorized".to_owned()
            } else {
                key.to_owned()
            }
        };
        let active: Vec<&String> = keys
            .iter()
            .filter(|k| ui.category_filters.contains(k))
            .collect();
        let summary = match active.as_slice() {
            [] => "Category: all".to_owned(),
            [one] => format!("Category: {}", label_of(one)),
            [first, rest @ ..] => format!("Category: {} +{}", label_of(first), rest.len()),
        };
        let button = chip(
            "category-filter",
            format!("{summary} ▾"),
            !active.is_empty(),
            cx,
        )
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, e: &MouseDownEvent, _, cx| {
                let closed_now = this.category_filter_closed_at.take() == Some(e.position);
                this.category_filter_menu = match this.category_filter_menu {
                    Some(_) => None,
                    None if closed_now => None,
                    None => Some(e.position),
                };
                cx.stop_propagation();
                cx.notify();
            }),
        );
        let menu = self.category_filter_menu.map(|position| {
            let hover = color(cx, T::BgHover);
            let accent = color(cx, T::AccentText);
            let items = keys.iter().map(|key| {
                let checked = ui.category_filters.contains(key);
                let k = key.clone();
                div()
                    .id(SharedString::from(format!("category-filter-{key}")))
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_1()
                    .cursor_pointer()
                    .hover(move |s| s.bg(hover))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.dispatch(Command::ToggleCategoryFilter(k.clone()), cx);
                    }))
                    .child(div().w(px(14.0)).text_color(accent).child(if checked {
                        "✓"
                    } else {
                        ""
                    }))
                    .child(label_of(key))
            });
            let clear = (!ui.category_filters.is_empty()).then(|| {
                let selected = ui.category_filters.clone();
                div()
                    .id("category-filter-clear")
                    .mt_1()
                    .pt_1()
                    .px_3()
                    .py_1()
                    .border_t_1()
                    .border_color(color(cx, T::BorderSubtle))
                    .text_color(color(cx, T::FgMuted))
                    .cursor_pointer()
                    .hover(move |s| s.bg(hover))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        for key in &selected {
                            this.dispatch(Command::ToggleCategoryFilter(key.clone()), cx);
                        }
                    }))
                    .child("Show all categories")
            });
            let menu = div()
                .id("category-filter-menu")
                .occlude()
                .min_w(px(180.0))
                .max_h(px(360.0))
                .overflow_y_scrollbar()
                .py_1()
                .flex()
                .flex_col()
                .font_family(ui_font(cx))
                .font_weight(crate::theme::ui_weight(cx))
                .text_size(text_size(cx))
                .text_color(color(cx, T::FgDefault))
                .whitespace_nowrap()
                .bg(color(cx, T::BgElevated))
                .border_1()
                .border_color(color(cx, T::Border))
                .rounded(radius(cx))
                .shadow_md()
                .on_mouse_down_out(cx.listener(|this, e: &MouseDownEvent, _, cx| {
                    this.category_filter_menu = None;
                    this.category_filter_closed_at = Some(e.position);
                    cx.notify();
                }))
                .children(items)
                .children(clear);
            popup_at(position, menu)
        });
        Some(div().child(button).children(menu).into_any_element())
    }
}

/// Plain click selects (and opens the quick edit pane), Ctrl+click adds to
/// the selection, double click opens.
pub(super) fn row_click(id: RepoId, e: &ClickEvent) -> Command {
    if e.click_count() > 1 {
        Command::OpenRepoTab(id)
    } else if e.modifiers().control || e.modifiers().platform {
        Command::ToggleSelect(id)
    } else {
        Command::Select(Some(id))
    }
}

/// The frame of a dashboard row: selection, dimming and hover.
fn row_frame(
    id: RepoId,
    selected: bool,
    stale: bool,
    hidden: bool,
    cx: &gpui_kit::App,
) -> gpui_kit::Stateful<gpui_kit::Div> {
    let hover = color(cx, T::BgHover);
    div()
        .id(SharedString::from(format!("dash-{id}")))
        .flex()
        .items_center()
        .px_3()
        .py_1()
        .min_h(row_height(cx))
        .border_b_1()
        .border_color(color(cx, T::BorderSubtle))
        .cursor_pointer()
        .when(selected, |d| d.bg(color(cx, T::BgSelected)))
        .when(stale && !hidden, |d| d.opacity(0.75))
        .when(hidden, |d| d.opacity(0.45))
        .hover(move |s| s.bg(hover))
}

/// What a dashboard row shows; its view is drawn again only when this
/// changes.
#[derive(PartialEq)]
struct RowContent {
    id: RepoId,
    selected: bool,
    stale: bool,
    hidden: bool,
    cells: Vec<CellContent>,
}

impl RowContent {
    fn new(ext: &Extensions, row: &DashboardRow, now: Timestamp, selected: &[RepoId]) -> Self {
        Self {
            id: row.id,
            selected: selected.contains(&row.id),
            stale: row.stale,
            hidden: row.hidden,
            cells: cell_contents(ext, row, now, 0),
        }
    }
}

/// A dashboard row as its own view. A hover change redraws the view it
/// happens in and the views around it: with the rows drawn by the
/// workspace, hovering one redrew every row; as views, the other rows are
/// reused from the last frame.
pub(super) struct DashRow {
    workspace: WeakEntity<Workspace>,
    core: Arc<Core>,
    content: RowContent,
}

impl Render for DashRow {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = &self.content;
        let id = c.id;
        let (on_click, on_menu) = (self.workspace.clone(), self.workspace.clone());
        row_frame(id, c.selected, c.stale, c.hidden, cx)
            .size_full()
            .on_click(move |e, window, cx| {
                let _ = on_click.update(cx, |this, cx| this.row_clicked(id, e, window, cx));
            })
            .on_mouse_down(MouseButton::Right, move |e, _, cx| {
                let _ = on_menu.update(cx, |this, cx| this.open_row_menu(id, e, cx));
            })
            .children(
                c.cells
                    .iter()
                    .map(|cell| cell_element(cell, id, &self.core, cx)),
            )
    }
}

/// Height of a dashboard row with one or two lines of text: a cached view
/// is laid out at a given size instead of being measured. A line is the
/// font size times φ, GPUI's default line height.
fn row_box_height(two_lines: bool, window: &Window, cx: &gpui_kit::App) -> Pixels {
    let line = |size: f32| (size * 1.618_034).round();
    let rem = f32::from(window.rem_size());
    let mut content = line(f32::from(text_size(cx)));
    if two_lines {
        content += line(0.75 * rem);
    }
    // `py_1` above and below, and the bottom border.
    row_height(cx).max(px(0.5 * rem + content + 1.0))
}

/// A dashboard cell: its column, what it shows and its tooltip.
#[derive(PartialEq)]
struct CellContent {
    column: usize,
    cell: Cell,
    tooltip: Option<Vec<String>>,
    /// Pill after the text (the Sync column's conflict forecast).
    badge: Option<CellBadge>,
    /// The failing CI run of the "CI" cell: its tooltip shows the logs of
    /// the failed jobs and offers to rerun them.
    failing_ci: Option<CiRun>,
}

/// The dashboard cells of a row, from column `from` on.
pub(super) fn row_cells(
    ext: &Extensions,
    core: &Arc<Core>,
    row: &DashboardRow,
    now: Timestamp,
    from: usize,
    cx: &gpui_kit::App,
) -> Vec<AnyElement> {
    cell_contents(ext, row, now, from)
        .iter()
        .map(|cell| cell_element(cell, row.id, core, cx))
        .collect()
}

fn cell_contents(
    ext: &Extensions,
    row: &DashboardRow,
    now: Timestamp,
    from: usize,
) -> Vec<CellContent> {
    ext.columns
        .columns()
        .iter()
        .enumerate()
        .skip(from)
        .map(|(i, col)| {
            let mut cell = col.cell(row, now);
            if i == 0 && row.hidden {
                cell.detail = Some(
                    cell.detail
                        .map_or_else(|| "hidden".to_owned(), |d| format!("hidden · {d}")),
                );
            }
            let failing_ci = (col.id() == "ci")
                .then(|| row.ci.run())
                .flatten()
                .filter(|run| run.failed_jobs().next().is_some())
                .cloned();
            CellContent {
                column: i,
                cell,
                tooltip: col.tooltip(row),
                badge: col.badge(row),
                failing_ci,
            }
        })
        .collect()
}

fn cell_element(
    content: &CellContent,
    repo: RepoId,
    core: &Arc<Core>,
    cx: &gpui_kit::App,
) -> AnyElement {
    let i = content.column;
    let cell = &content.cell;
    div()
        .id(i)
        .flex_grow(WIDTHS.get(i).copied().unwrap_or(1.0))
        .flex_basis(px(0.0))
        .overflow_hidden()
        .flex()
        .flex_col()
        .child(
            div()
                .flex()
                .items_center()
                .gap_1()
                .min_w_0()
                .child(
                    div()
                        .truncate()
                        .text_color(tone(cx, cell.tone))
                        .child(cell.text.clone()),
                )
                .children(content.badge.as_ref().map(|b| pill(b, cx))),
        )
        .when(i == 0, |d| {
            d.children(cell.detail.clone().map(|t| {
                div()
                    .truncate()
                    .text_xs()
                    .text_color(color(cx, T::FgSubtle))
                    .child(t)
            }))
        })
        .when_some(content.tooltip.clone(), |d, lines| {
            match content.failing_ci.clone() {
                Some(run) => {
                    let core = core.clone();
                    d.hoverable_tooltip(move |_, cx| {
                        CiTooltip::view(core.clone(), repo, &lines, Some(&run), cx)
                    })
                }
                None => d.tooltip(move |_, cx| TextTooltip::view(&lines, cx)),
            }
        })
        .into_any_element()
}
