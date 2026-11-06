//! The status bar (notice, update badge, project color and theme pickers)
//! and the operations drawer above it.

use archipel_theme::ColorToken as T;
use gpui_kit::component::color_picker::ColorPicker;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{Colorize as _, Sizable as _};
use gpui_kit::prelude::*;
use gpui_kit::{Anchor, Context, MouseButton, MouseDownEvent, SharedString, Window, div, px};

use crate::theme::{UiTheme, color, radius, text_size, ui_font};
use crate::widgets::{TextTooltip, popup_at, section};

use super::Workspace;

impl Workspace {
    fn set_theme(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        cx.global_mut::<UiTheme>().set(id);
        crate::theme::sync_toolkit_theme(cx);
        if let Err(e) = cx.global::<UiTheme>().remember() {
            self.notice = Some(format!("Theme not remembered: {e}"));
        }
        window.refresh();
        crate::windows::refresh_all(cx);
    }

    pub(super) fn render_drawer(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let lines = self.log.iter().rev().take(80).map(|(_, text, error)| {
            div()
                .text_xs()
                .font_family(crate::theme::mono_font(cx))
                .text_color(if *error {
                    color(cx, T::StatusError)
                } else {
                    color(cx, T::FgMuted)
                })
                .child(text.clone())
        });
        div()
            .id("drawer")
            .h(px(180.0))
            .flex_none()
            .overflow_y_scrollbar()
            .p_2()
            .bg(color(cx, T::BgPanel))
            .border_t_1()
            .border_color(color(cx, T::BorderSubtle))
            .child(section("OPERATIONS  (Ctrl+J)", cx))
            .children(lines)
    }

    pub(super) fn render_status(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let text = self.notice.clone().or_else(|| self.warnings.first().cloned()).unwrap_or_else(|| {
            "Ctrl+P switch project · Ctrl+Shift+P actions · Ctrl+K commit · Ctrl+Z undo · Ctrl+H hide · Ctrl+N new window".into()
        });
        div()
            .id("status")
            .h(px(24.0))
            .flex_none()
            .flex()
            .items_center()
            .px_3()
            .text_xs()
            .text_color(color(cx, T::FgMuted))
            .bg(color(cx, T::BgPanel))
            .border_t_1()
            .border_color(color(cx, T::BorderSubtle))
            .cursor_pointer()
            .on_click(cx.listener(|this, _, _, cx| {
                this.notice = None;
                cx.notify();
            }))
            .child(div().flex_1().min_w_0().truncate().child(text))
            .children(self.render_update_badge(cx))
            .children(self.render_accent_picker(window, cx))
            .child(self.render_theme_picker(cx))
    }

    /// The status bar's "update available" badge: opens the release page,
    /// × hides it until a newer release. None when up to date.
    fn render_update_badge(&mut self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let release = self.core.available_update()?;
        if self.dismissed_update.as_ref() == Some(&release.version) {
            return None;
        }
        let fg = color(cx, T::FgDefault);
        let tip = vec![
            format!("Archipel {} is available", release.version),
            "Click to open the release page".to_owned(),
        ];
        let url = release.url;
        let version = release.version;
        Some(
            div()
                .flex_none()
                .flex()
                .items_center()
                .ml_2()
                .text_color(color(cx, T::AccentText))
                .child(
                    div()
                        .id("update-badge")
                        .px_1()
                        .rounded(radius(cx))
                        .whitespace_nowrap()
                        .hover(move |s| s.text_color(fg))
                        .tooltip(move |_, cx| TextTooltip::view(&tip, cx))
                        .on_click(move |_, _, cx| {
                            cx.open_url(&url);
                            cx.stop_propagation();
                        })
                        .child(format!("↑ Update {version}")),
                )
                .child(
                    div()
                        .id("update-dismiss")
                        .px_1()
                        .rounded(radius(cx))
                        .hover(move |s| s.text_color(fg))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.dismissed_update = Some(version.clone());
                            cx.stop_propagation();
                            cx.notify();
                        }))
                        .child("×"),
                ),
        )
    }

    /// The status bar's project color button: a swatch of the window's
    /// accent, opening a color picker. None without a project.
    fn render_accent_picker(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement> {
        let accent = self.core.identity()?.accent;
        // Follow the accent (project switch, palette prompt) unless the
        // picker is open: its own changes come back here.
        self.accent_picker.update(cx, |state, cx| {
            let shown = state
                .value()
                .map(|c| gpui_kit::Hsla { a: 1.0, ..c }.to_hex());
            if !state.is_open() && shown.as_deref() != Some(accent.to_hex().as_str()) {
                state.set_value(crate::theme::to_hsla(accent), window, cx);
            }
        });
        Some(
            div()
                .id("accent-picker")
                .flex_none()
                .ml_2()
                // The status bar's click dismisses its notice; not this one.
                .on_click(|_, _, cx| cx.stop_propagation())
                .child(
                    ColorPicker::new(&self.accent_picker)
                        .xsmall()
                        .anchor(Anchor::BottomRight)
                        .accessibility_label("Project color"),
                ),
        )
    }

    /// The status bar's theme picker: the current theme's name, opening a
    /// list of every theme.
    fn render_theme_picker(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = cx.global::<UiTheme>();
        let current = ui.theme.id.clone();
        let themes: Vec<(String, String)> = ui
            .registry
            .themes()
            .map(|t| (t.id.clone(), t.name.clone()))
            .collect();
        let name = ui.theme.name.clone();
        let hover = color(cx, T::BgHover);
        let fg = color(cx, T::FgDefault);
        let label = div()
            .id("theme-picker")
            .flex_none()
            .ml_2()
            .px_1()
            .rounded(radius(cx))
            .whitespace_nowrap()
            .hover(move |s| s.text_color(fg))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, e: &MouseDownEvent, _, cx| {
                    let closed_now = this.theme_menu_closed_at.take() == Some(e.position);
                    this.theme_menu = match this.theme_menu {
                        Some(_) => None,
                        None if closed_now => None,
                        None => Some(e.position),
                    };
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
            // The status bar's click dismisses its notice; not this one.
            .on_click(|_, _, cx| cx.stop_propagation())
            .child(format!("{name} ▾"));
        let menu = self.theme_menu.map(|position| {
            let accent = color(cx, T::AccentText);
            let items = themes.into_iter().map(|(id, name)| {
                let checked = id == current;
                div()
                    .id(SharedString::from(format!("theme-{id}")))
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_1()
                    .cursor_pointer()
                    .hover(move |s| s.bg(hover))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.theme_menu = None;
                        this.set_theme(&id, window, cx);
                        cx.stop_propagation();
                    }))
                    .child(div().w(px(14.0)).text_color(accent).child(if checked {
                        "✓"
                    } else {
                        ""
                    }))
                    .child(name)
            });
            let menu = div()
                .id("theme-menu")
                .occlude()
                .min_w(px(160.0))
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
                    this.theme_menu = None;
                    this.theme_menu_closed_at = Some(e.position);
                    cx.notify();
                }))
                .children(items);
            popup_at(position, menu)
        });
        div().child(label).children(menu)
    }
}
