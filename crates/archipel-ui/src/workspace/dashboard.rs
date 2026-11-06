//! The dashboard tab: toolbar (filters, category filter, view chips),
//! the column header and the repo rows, and the quick edit pane lying over
//! their right edge.

use archipel_app::dashboard::DashboardRow;
use archipel_app::{Command, Extensions};
use archipel_model::ui_state::{SortDirection, TabState, UiState};
use archipel_model::{RepoId, Timestamp};
use archipel_theme::ColorToken as T;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, ClickEvent, Context, DragMoveEvent, KeyDownEvent, MouseButton, MouseDownEvent,
    SharedString, Window, div, px,
};

use crate::quick_edit::{QuickEdit, QuickEditEvent};
use crate::theme::{color, radius, row_height, text_size, tone, ui_font};
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
        // Its open diff is drawn here, in place of the rows.
        let redraw = cx.observe(&pane, |_, _, cx| cx.notify());
        self.quick_edit = Some((pane, [sub, redraw]));
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

    pub(super) fn render_dashboard(&mut self, cx: &mut Context<Self>) -> AnyElement {
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
            rows.iter()
                .map(|row| {
                    let cells = row_cells(&ext, row, now, 0, cx);
                    Self::row_shell(row, &selected, cx)
                        .children(cells)
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
                        .child(splitter(Pane::QuickEdit, cx).bg(color(cx, T::BgPanel)))
                        .child(div().w(px(width)).h_full().flex().flex_col().child(pane)),
                )
            })
            .into_any_element()
    }

    /// The clickable frame of a dashboard row (selection, dimming, menus).
    pub(super) fn row_shell(
        row: &DashboardRow,
        selected: &[RepoId],
        cx: &mut Context<Self>,
    ) -> gpui_kit::Stateful<gpui_kit::Div> {
        let id = row.id;
        let hover = color(cx, T::BgHover);
        let selected_bg = color(cx, T::BgSelected);
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
            .when(selected.contains(&id), |d| d.bg(selected_bg))
            .when(row.stale && !row.hidden, |d| d.opacity(0.75))
            .when(row.hidden, |d| d.opacity(0.45))
            .hover(move |s| s.bg(hover))
            .on_click(cx.listener(move |this, e: &ClickEvent, window, cx| {
                let command = row_click(id, e);
                let plain = command == Command::Select(Some(id));
                this.dispatch(command, cx);
                if plain {
                    this.toggle_quick_edit(id, window, cx);
                }
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                    this.row_menu = Some((id, e.position));
                    cx.notify();
                }),
            )
    }

    /// The category filter: one dropdown button whatever the number of
    /// categories, its menu a checklist (several can be selected).
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

/// The dashboard cells of a row, from column `from` on.
pub(super) fn row_cells(
    ext: &Extensions,
    row: &DashboardRow,
    now: Timestamp,
    from: usize,
    cx: &gpui_kit::App,
) -> Vec<AnyElement> {
    ext.columns
        .columns()
        .iter()
        .enumerate()
        .skip(from)
        .map(|(i, col)| {
            let cell = col.cell(row, now);
            let tooltip = col.tooltip(row);
            let detail = if i == 0 && row.hidden {
                Some(
                    cell.detail
                        .map_or_else(|| "hidden".to_owned(), |d| format!("hidden · {d}")),
                )
            } else {
                cell.detail
            };
            div()
                .id(i)
                .flex_grow(WIDTHS.get(i).copied().unwrap_or(1.0))
                .flex_basis(px(0.0))
                .overflow_hidden()
                .flex()
                .flex_col()
                .child(
                    div()
                        .truncate()
                        .text_color(tone(cx, cell.tone))
                        .child(cell.text),
                )
                .when(i == 0, |d| {
                    d.children(detail.map(|t| {
                        div()
                            .truncate()
                            .text_xs()
                            .text_color(color(cx, T::FgSubtle))
                            .child(t)
                    }))
                })
                .when_some(tooltip, |d, lines| {
                    d.tooltip(move |_, cx| TextTooltip::view(&lines, cx))
                })
                .into_any_element()
        })
        .collect()
}
