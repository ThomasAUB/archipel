//! The sidebar: the project switcher and the repos of the project,
//! grouped by category, sized to fit their names.

use archipel_app::dashboard::{DashboardRow, RepoStatus};
use archipel_app::{Command, ProjectIcon};
use archipel_theme::ColorToken as T;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, ClickEvent, Context, MouseButton, MouseDownEvent, ObjectFit, Pixels, SharedString,
    TextRun, Window, div, font, img, px, rems,
};

use crate::forge_panel::ForgeTab;
use crate::theme::{clean_dot, color, radius, repo_surface, row_height, text_size, ui_font};
use crate::widgets::{button, dot, section};

use super::Workspace;
use super::dashboard::row_click;
use super::overlays::PaletteKind;

impl Workspace {
    #[allow(clippy::too_many_lines)] // declarative element tree
    pub(super) fn render_sidebar(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let identity = self.core.identity();
        let groups = self.core.sidebar_groups();
        let selected = self
            .core
            .ui_state()
            .map(|u| u.batch_selection())
            .unwrap_or_default();
        let hover = color(cx, T::BgHover);
        let selected_bg = color(cx, T::BgSelected);
        // Headers only once at least one repo has a category (spec §1).
        let grouped = groups.len() > 1 || groups.first().is_some_and(|g| g.category.is_some());
        let repo_row = |row: DashboardRow, cx: &mut Context<Self>| {
            let id = row.id;
            let (token, extra) = side_status(&row.status);
            div()
                .id(SharedString::from(format!("side-{}", row.id)))
                .flex()
                .items_center()
                .gap_2()
                .px_2()
                .when(grouped, Styled::pl_5)
                .h(row_height(cx))
                .rounded(radius(cx))
                .cursor_pointer()
                .when(selected.contains(&id), |d| d.bg(selected_bg))
                .hover(move |s| s.bg(hover))
                .on_click(cx.listener(move |this, e: &ClickEvent, _, cx| {
                    // Unlike the dashboard, a plain click opens the repo.
                    let cmd = row_click(id, e);
                    if matches!(cmd, Command::Select(_)) {
                        this.dispatch(cmd, cx);
                        this.dispatch(Command::OpenRepoTab(id), cx);
                    } else {
                        this.dispatch(cmd, cx);
                    }
                }))
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                        this.row_menu = Some((id, e.position));
                        cx.notify();
                    }),
                )
                .child(dot(if token == T::StatusSuccess {
                    clean_dot()
                } else {
                    color(cx, token)
                }))
                .child(div().flex_1().min_w_0().truncate().child(row.name))
                .child(
                    div()
                        .flex_none()
                        .whitespace_nowrap()
                        .text_xs()
                        .text_color(color(cx, T::FgMuted))
                        .child(extra),
                )
                .into_any_element()
        };
        // Fits the longest visible name (repo or category), never narrower
        // than its footer buttons; capped at half the window.
        let (size, xs) = (text_size(cx), rems(0.75).to_pixels(window.rem_size()));
        let measure = |text: &str, size| text_width(text, size, window, cx).ceil();
        // List padding (px_1) and right border, plus a pixel of slack so
        // rounding never truncates the longest name.
        let frame = 8.0 + 1.0 + 1.0;
        // Repo row: left padding (pl_5 grouped, else px_2), right padding,
        // dot, and the gaps around the name.
        let row_frame = if grouped { 20.0 } else { 8.0 } + 8.0 + 8.0 + 2.0 * 8.0;
        // Group header: padding (px_2), arrow, and the gaps around the name.
        let header_frame = 16.0 + 12.0 + 2.0 * 4.0;
        // Footer: padding, the two buttons' padding and border, the gap.
        let mut fit: f32 =
            16.0 + 2.0 * 18.0 + 8.0 + measure("Add repos…", size) + measure("Clone…", size);
        for group in &groups {
            if grouped {
                let name = group.category.as_deref().unwrap_or("Uncategorized");
                let count = group.rows.len().to_string();
                fit = fit.max(frame + header_frame + measure(name, xs) + measure(&count, xs));
            }
            if group.collapsed {
                continue;
            }
            for row in &group.rows {
                let extra = side_status(&row.status).1;
                fit = fit.max(frame + row_frame + measure(&row.name, size) + measure(&extra, xs));
            }
        }
        let fit = fit.min(f32::from(window.viewport_size().width) / 2.0);
        let mut repos: Vec<AnyElement> = Vec::new();
        for (i, group) in groups.into_iter().enumerate() {
            if grouped {
                let key = group.key().to_owned();
                // A collapsed group holding the selection stays noticeable.
                let holds_selection =
                    group.collapsed && group.rows.iter().any(|r| selected.contains(&r.id));
                repos.push(
                    div()
                        .id(SharedString::from(format!("side-group-{i}")))
                        .flex()
                        .items_center()
                        .gap_1()
                        .px_2()
                        .h(row_height(cx))
                        .rounded(radius(cx))
                        .cursor_pointer()
                        .text_xs()
                        .text_color(color(
                            cx,
                            if holds_selection {
                                T::Accent
                            } else {
                                T::FgSubtle
                            },
                        ))
                        .hover(move |s| s.bg(hover))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.dispatch(Command::ToggleCategory(key.clone()), cx);
                        }))
                        .when_some(group.category.clone(), |d, category| {
                            d.on_mouse_down(
                                MouseButton::Right,
                                cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                                    this.category_menu = Some((category.clone(), e.position));
                                    cx.notify();
                                }),
                            )
                        })
                        .child(div().w(px(12.0)).flex_none().child(if group.collapsed {
                            "▸"
                        } else {
                            "▾"
                        }))
                        .child(
                            div().flex_1().min_w_0().truncate().child(
                                group
                                    .category
                                    .clone()
                                    .unwrap_or_else(|| "Uncategorized".into()),
                            ),
                        )
                        .child(div().flex_none().child(group.rows.len().to_string()))
                        .into_any_element(),
                );
            }
            if !group.collapsed {
                for row in group.rows {
                    if self.reveal == Some(row.id) {
                        self.sidebar_scroll.scroll_to_item(repos.len());
                        self.reveal = None;
                    }
                    repos.push(repo_row(row, cx));
                }
            }
        }
        div()
            .w(px(fit))
            .border_r_1()
            .border_color(color(cx, T::BorderSubtle))
            .h_full()
            .flex()
            .flex_col()
            .flex_none()
            .bg(repo_surface(cx))
            .child(
                div()
                    .id("project-switcher")
                    .m_2()
                    .px_2()
                    .py_1()
                    .rounded(radius(cx))
                    .border_1()
                    .border_color(color(cx, T::Border))
                    .cursor_pointer()
                    .hover(move |s| s.bg(hover))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.open_palette(PaletteKind::Projects, window, cx);
                    }))
                    .child(
                        div()
                            .text_xs()
                            .text_color(color(cx, T::FgSubtle))
                            .child("PROJECT  (Ctrl+P)"),
                    )
                    .child(match identity {
                        None => div().child("No project — open or create one"),
                        Some(identity) => div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_color(color(cx, T::AccentText))
                            .children(identity.icon.map(|icon| project_icon(icon, cx)))
                            .child(div().truncate().child(identity.name)),
                    }),
            )
            .child(section("REPOSITORIES", cx))
            .child(
                div()
                    .id("side-repos")
                    .flex_1()
                    .overflow_y_scroll()
                    .track_scroll(&self.sidebar_scroll)
                    .vertical_scrollbar(&self.sidebar_scroll)
                    .px_1()
                    .children(repos),
            )
            .child(
                div()
                    .p_2()
                    .flex()
                    .gap_2()
                    .border_t_1()
                    .border_color(color(cx, T::BorderSubtle))
                    .child(
                        button("add-repo", "Add repos…", cx)
                            .on_click(cx.listener(|this, _, _, cx| this.add_repos(cx))),
                    )
                    .child(button("clone-repo", "Clone…", cx).on_click(cx.listener(
                        |this, _, window, cx| this.open_forge(ForgeTab::Clone, window, cx),
                    ))),
            )
    }
}

/// A sidebar row's status dot color and its ahead/behind badge.
fn side_status(status: &RepoStatus) -> (T, String) {
    match status {
        RepoStatus::Ready(s) if s.changes.conflicted > 0 || s.in_progress.is_some() => {
            (T::StatusConflict, String::new())
        }
        RepoStatus::Ready(s) => {
            let sync = match (s.ahead(), s.behind()) {
                (0, 0) => String::new(),
                (a, 0) => format!("↑{a}"),
                (0, b) => format!("↓{b}"),
                (a, b) => format!("↑{a}↓{b}"),
            };
            (
                if s.changes.is_dirty() {
                    T::StatusDirty
                } else {
                    T::StatusSuccess
                },
                sync,
            )
        }
        RepoStatus::Loading => (T::FgSubtle, String::new()),
        RepoStatus::Missing | RepoStatus::Error(_) => (T::StatusError, String::new()),
    }
}

/// Width of one line of `text` in the UI font at `size`.
fn text_width(text: &str, size: Pixels, window: &Window, cx: &gpui_kit::App) -> f32 {
    let run = TextRun {
        len: text.len(),
        font: font(ui_font(cx)),
        color: color(cx, T::FgDefault),
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let line = window
        .text_system()
        .shape_line(text.to_owned().into(), size, &[run], None);
    f32::from(line.width)
}

/// A project icon next to its name: the text, or the image sized to the
/// line (nothing is drawn when the file cannot be read).
pub(super) fn project_icon(icon: ProjectIcon, cx: &gpui_kit::App) -> AnyElement {
    match icon {
        ProjectIcon::Text(text) => div().child(text).into_any_element(),
        ProjectIcon::Image(path) => img(path)
            .size(rems(1.25))
            .flex_none()
            .rounded(radius(cx))
            .object_fit(ObjectFit::Contain)
            .into_any_element(),
    }
}
