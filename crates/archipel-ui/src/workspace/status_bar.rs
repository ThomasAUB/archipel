//! The status bar (notice, update badge, project color and theme pickers)
//! and the operations drawer above it.

use archipel_app::UpdateInstall;
use archipel_theme::ColorToken as T;
use gpui_kit::component::color_picker::ColorPicker;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{Colorize as _, Sizable as _};
use gpui_kit::prelude::*;
use gpui_kit::{Anchor, Context, MouseButton, MouseDownEvent, SharedString, Window, div, px};

use crate::theme::{UiTheme, color, radius};
use crate::widgets::{TextTooltip, check_menu_item, scroll_menu, section};

use super::Workspace;
use super::menus::Menu;

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
            .child(section(
                match self.shortcut("drawer.toggle") {
                    Some(key) => format!("OPERATIONS  ({key})"),
                    None => "OPERATIONS".to_owned(),
                },
                cx,
            ))
            .children(lines)
    }

    /// The status bar's line when it has nothing to say: the main
    /// shortcuts, as bound now (unbound ones left out).
    fn shortcut_hints(&self) -> String {
        [
            ("palette.projects", "switch project"),
            ("palette.actions", "actions"),
            ("commit.open", "commit"),
            ("history.undo", "undo"),
            ("repos.toggle_hidden", "hide"),
            ("window.new", "new window"),
        ]
        .into_iter()
        .filter_map(|(id, what)| self.shortcut(id).map(|key| format!("{key} {what}")))
        .collect::<Vec<_>>()
        .join(" · ")
    }

    pub(super) fn render_status(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let text = self
            .notice
            .clone()
            .or_else(|| self.warnings.first().cloned())
            .unwrap_or_else(|| self.shortcut_hints());
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
                // A notice with a link (a created issue) opens it.
                if let Some((text, url)) = this.notice_link.take()
                    && this.notice.as_ref() == Some(&text)
                {
                    cx.open_url(&url);
                }
                this.notice = None;
                cx.notify();
            }))
            .child(div().flex_1().min_w_0().truncate().child(text))
            .children(self.render_update_badge(cx))
            .children(self.render_accent_picker(window, cx))
            .child(self.render_theme_picker(cx))
    }

    /// The status bar's update badge, by where installing the found release
    /// stands: "↑ Update x" installs it in the background, then "↻ Restart
    /// to update" restarts into it (it runs from the next launch anyway); a
    /// failure offers to try again. Right-click opens the release page, ×
    /// hides the badge until a newer release. None when up to date.
    fn render_update_badge(&mut self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let release = self.core.available_update()?;
        if self.dismissed_update.as_ref() == Some(&release.version) {
            return None;
        }
        let fg = color(cx, T::FgDefault);
        let version = release.version;
        let UpdateBadge {
            label,
            tip,
            restart,
            dismissable,
        } = UpdateBadge::of(&version, self.core.update_install());
        let url = release.url;
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
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            match &restart {
                                Some(binary) => crate::windows::restart(cx, binary.clone()),
                                None => this.core.install_update(),
                            }
                            cx.notify();
                        }))
                        .on_mouse_down(MouseButton::Right, move |_, _, cx| {
                            cx.open_url(&url);
                            cx.stop_propagation();
                        })
                        .child(label),
                )
                .when(dismissable, |el| {
                    el.child(
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
                    )
                }),
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
                    this.toggle_menu(Menu::Theme, e, cx);
                }),
            )
            // The status bar's click dismisses its notice; not this one.
            .on_click(|_, _, cx| cx.stop_propagation())
            .child(format!("{name} ▾"));
        let menu = self.menu_position(&Menu::Theme).map(|position| {
            let items = themes.into_iter().map(|(id, name)| {
                let checked = id == current;
                check_menu_item(SharedString::from(format!("theme-{id}")), name, checked, cx)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.menu = None;
                        this.set_theme(&id, window, cx);
                        cx.stop_propagation();
                    }))
            });
            let menu = scroll_menu("theme-menu", cx).children(items);
            Self::menu_popup(position, menu, cx)
        });
        div().child(label).children(menu)
    }
}

/// What the update badge shows for one state of the install.
struct UpdateBadge {
    label: String,
    tip: Vec<String>,
    /// The installed binary a click restarts into; a click installs when
    /// `None`.
    restart: Option<std::path::PathBuf>,
    /// Whether × may hide it (not while installing).
    dismissable: bool,
}

impl UpdateBadge {
    /// The badge of `version`, the release found, given `install`.
    fn of(version: &str, install: UpdateInstall) -> Self {
        let page = "Right-click for the release page".to_owned();
        // An install of an older release found earlier does not count.
        let install = match install {
            UpdateInstall::Installing { version: v, .. }
            | UpdateInstall::Installed { version: v, .. }
            | UpdateInstall::Failed { version: v, .. }
                if v != version =>
            {
                UpdateInstall::Idle
            }
            install => install,
        };
        match install {
            UpdateInstall::Idle => Self {
                label: format!("↑ Update {version}"),
                tip: vec![
                    format!("Archipel {version} is available"),
                    "Click to download and install it; it runs from the next launch".to_owned(),
                    page,
                ],
                restart: None,
                dismissable: true,
            },
            UpdateInstall::Installing { percent, .. } => Self {
                label: format!("↓ Updating {percent}%"),
                tip: vec![format!("Downloading and installing Archipel {version}…")],
                restart: None,
                dismissable: false,
            },
            UpdateInstall::Installed { binary, .. } => Self {
                label: "↻ Restart to update".to_owned(),
                tip: vec![
                    format!("Archipel {version} is installed; it runs from the next launch"),
                    "Click to restart now".to_owned(),
                    page,
                ],
                restart: Some(binary),
                dismissable: true,
            },
            UpdateInstall::Failed { message, .. } => Self {
                label: "⚠ Update failed".to_owned(),
                tip: vec![
                    format!("Archipel {version} could not be installed: {message}"),
                    "Click to try again".to_owned(),
                    page,
                ],
                restart: None,
                dismissable: true,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_update_badge_follows_the_install_of_the_found_release() {
        let badge = |install| UpdateBadge::of("2.0.0", install);
        assert_eq!(badge(UpdateInstall::Idle).label, "↑ Update 2.0.0");
        let installing = badge(UpdateInstall::Installing {
            version: "2.0.0".into(),
            percent: 42,
        });
        assert_eq!(installing.label, "↓ Updating 42%");
        assert!(!installing.dismissable);
        let installed = badge(UpdateInstall::Installed {
            version: "2.0.0".into(),
            binary: "archipel".into(),
        });
        assert_eq!(installed.label, "↻ Restart to update");
        assert_eq!(installed.restart, Some("archipel".into()));
        let failed = badge(UpdateInstall::Failed {
            version: "2.0.0".into(),
            message: "update: bad checksum".into(),
        });
        assert_eq!(failed.label, "⚠ Update failed");
        assert!(failed.tip[0].ends_with("update: bad checksum"));
        // An older release installed earlier: the new one is offered.
        let older = badge(UpdateInstall::Installed {
            version: "1.5.0".into(),
            binary: "archipel".into(),
        });
        assert_eq!(older.label, "↑ Update 2.0.0");
        assert_eq!(older.restart, None);
    }
}
