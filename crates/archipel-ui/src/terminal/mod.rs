//! The integrated terminal panel (issue #98): docked under the workspace,
//! one tab per terminal, its height dragged from its top edge.
//!
//! Nothing runs until a terminal is opened: the panel itself is created on
//! first use, and each tab starts its shell when it is opened. Git commands
//! typed in a terminal need nothing special: the file watcher refreshes the
//! repo as for any change made outside Archipel.

mod input;
mod links;
mod view;

use std::path::PathBuf;
use std::sync::Arc;

use archipel_app::App as Core;
use archipel_model::terminal::TerminalSize;
use archipel_theme::ColorToken as T;
use gpui_kit::assets::IconName;
use gpui_kit::prelude::*;
use gpui_kit::{
    Context, DragMoveEvent, Empty, Entity, EventEmitter, Focusable as _, MouseButton, MouseUpEvent,
    SharedString, Subscription, Window, div, px,
};

use crate::theme::color;
use crate::widgets::{TextTooltip, icon_button};
use view::{Ended, TerminalView, TerminalViewEvent};

/// Default panel height, in pixels.
const DEFAULT_HEIGHT: f32 = 260.0;
/// Lowest panel height, and the least height it leaves the window.
const MIN_HEIGHT: f32 = 80.0;
/// Height of the panel's tab bar.
const TAB_BAR_HEIGHT: f32 = 28.0;

/// What the panel tells the workspace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TerminalPanelEvent {
    /// A message for the status bar.
    Notice(String),
    /// Hide the panel: the user asked, or its last tab closed.
    Hide,
}

/// Drag payload of the panel's top edge.
struct PanelDrag;

/// A terminal tab.
struct Tab {
    view: Entity<TerminalView>,
    /// `repo · branch`, or the folder's name.
    title: String,
    /// The one-off command it runs, if not a shell.
    command: Option<String>,
    _events: Subscription,
}

/// The terminal panel of a window.
pub struct TerminalPanel {
    core: Arc<Core>,
    tabs: Vec<Tab>,
    active: usize,
    height: f32,
}

impl EventEmitter<TerminalPanelEvent> for TerminalPanel {}

impl TerminalPanel {
    /// An empty panel.
    pub fn new(core: Arc<Core>) -> Self {
        Self {
            core,
            tabs: Vec::new(),
            active: 0,
            height: DEFAULT_HEIGHT,
        }
    }

    /// Whether it has no tab.
    pub fn is_empty(&self) -> bool {
        self.tabs.is_empty()
    }

    /// Size for a new terminal: the shown one's, so its output is laid
    /// out right even before it is shown.
    fn new_size(&self, cx: &gpui_kit::App) -> TerminalSize {
        self.tabs.get(self.active).map_or(
            TerminalSize {
                cols: 120,
                rows: 16,
                ..TerminalSize::default()
            },
            |t| t.view.read(cx).size(),
        )
    }

    /// Opens a terminal in `cwd` (a shell, or `command` through the shell),
    /// as the shown tab; `focus` gives it the keyboard. Returns whether it
    /// started.
    pub fn open(
        &mut self,
        cwd: PathBuf,
        title: String,
        command: Option<String>,
        focus: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let spec = self
            .core
            .terminal_spec(cwd, command.as_deref(), self.new_size(cx));
        let view = match view::start(&self.core, spec) {
            Ok(started) => cx.new(|cx| TerminalView::new(started, cx)),
            Err(e) => {
                cx.emit(TerminalPanelEvent::Notice(e.to_string()));
                return false;
            }
        };
        let events = cx.subscribe(&view, |this, view, e: &TerminalViewEvent, cx| {
            this.on_view_event(&view, e, cx);
        });
        self.tabs.push(Tab {
            view: view.clone(),
            title,
            command,
            _events: events,
        });
        self.active = self.tabs.len() - 1;
        if focus {
            view.focus_handle(cx).focus(window, cx);
        }
        cx.notify();
        true
    }

    fn on_view_event(
        &mut self,
        view: &Entity<TerminalView>,
        event: &TerminalViewEvent,
        cx: &mut Context<Self>,
    ) {
        let Some(i) = self.tabs.iter().position(|t| t.view == *view) else {
            return;
        };
        match event {
            // A shell left with `exit`: its tab goes, as in other
            // terminals. Commands and failures stay, to read them.
            TerminalViewEvent::Exited(Some(0)) if self.tabs[i].command.is_none() => {
                self.close(i, cx);
            }
            TerminalViewEvent::Exited(_) => cx.notify(),
            TerminalViewEvent::Copied => {
                cx.emit(TerminalPanelEvent::Notice("Terminal output copied".into()));
            }
        }
    }

    /// Gives the shown terminal the keyboard.
    pub fn focus_active(&self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(tab) = self.tabs.get(self.active) {
            tab.view.focus_handle(cx).focus(window, cx);
        }
    }

    fn close(&mut self, i: usize, cx: &mut Context<Self>) {
        if i >= self.tabs.len() {
            return;
        }
        // Dropping the view hangs its program up.
        self.tabs.remove(i);
        if self.active > i || self.active >= self.tabs.len() {
            self.active = self.active.saturating_sub(1);
        }
        if self.tabs.is_empty() {
            cx.emit(TerminalPanelEvent::Hide);
        }
        cx.notify();
    }

    fn render_tab(&self, i: usize, tab: &Tab, cx: &mut Context<Self>) -> impl IntoElement {
        let active = i == self.active;
        let status = match tab.view.read(cx).exited() {
            None if tab.command.is_some() => Some(("…".to_owned(), T::FgMuted)),
            None => None,
            Some(Ended::Code(0)) => Some(("✓".to_owned(), T::StatusSuccess)),
            Some(Ended::Code(code)) => Some((format!("✗ {code}"), T::StatusError)),
            Some(Ended::Killed) => Some(("✗".to_owned(), T::StatusError)),
        };
        let mut hover = vec![tab.title.clone()];
        if let Some(command) = &tab.command {
            hover.push(format!("$ {command}"));
        }
        div()
            .id(SharedString::from(format!("term-tab-{i}")))
            .flex()
            .flex_none()
            .items_center()
            .gap_2()
            .px_3()
            .h_full()
            .whitespace_nowrap()
            .cursor_pointer()
            .border_b_2()
            .border_color(if active {
                color(cx, T::Accent)
            } else {
                color(cx, T::BgPanel)
            })
            .tooltip(move |_, cx| TextTooltip::view(&hover, cx))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.active = i;
                this.focus_active(window, cx);
                cx.notify();
            }))
            // Middle click closes, as for the repo tabs.
            .on_mouse_up(
                MouseButton::Middle,
                cx.listener(move |this, _: &MouseUpEvent, _, cx| this.close(i, cx)),
            )
            .when_some(status, |d, (glyph, tone)| {
                d.child(div().text_color(color(cx, tone)).child(glyph))
            })
            .child(tab.title.clone())
            .child(
                div()
                    .id(SharedString::from(format!("term-close-{i}")))
                    .text_color(color(cx, T::FgMuted))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.close(i, cx);
                    }))
                    .child("×"),
            )
    }
}

impl Render for TerminalPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tabs: Vec<_> = self
            .tabs
            .iter()
            .enumerate()
            .map(|(i, t)| self.render_tab(i, t, cx).into_any_element())
            .collect();
        let max = (f32::from(window.viewport_size().height) - 3.0 * MIN_HEIGHT).max(MIN_HEIGHT);
        let height = self.height.clamp(MIN_HEIGHT, max);
        let accent = color(cx, T::Accent);
        div()
            .id("terminal-panel")
            .key_context("TerminalPanel")
            .h(px(height))
            .flex_none()
            .flex()
            .flex_col()
            .bg(color(cx, T::BgPanel))
            .on_drag_move(cx.listener(|this, e: &DragMoveEvent<PanelDrag>, _, cx| {
                this.height = f32::from(e.bounds.bottom() - e.event.position.y);
                cx.notify();
            }))
            .child(
                // The top edge: drag it to resize.
                div()
                    .id("terminal-resize")
                    .group("terminal-resize")
                    .h(px(5.0))
                    .w_full()
                    .flex_none()
                    .flex()
                    .flex_col()
                    .justify_start()
                    .cursor_row_resize()
                    .on_drag(PanelDrag, |_, _, _, cx| cx.new(|_| Empty))
                    .child(
                        div()
                            .h(px(1.0))
                            .w_full()
                            .bg(color(cx, T::BorderSubtle))
                            .group_hover("terminal-resize", move |s| s.bg(accent)),
                    ),
            )
            .child(
                div()
                    .h(px(TAB_BAR_HEIGHT))
                    .flex_none()
                    .flex()
                    .items_center()
                    .border_b_1()
                    .border_color(color(cx, T::BorderSubtle))
                    .child(
                        div()
                            .id("terminal-tabs")
                            .flex_1()
                            .h_full()
                            .flex()
                            .overflow_x_scroll()
                            .children(tabs),
                    )
                    .child(
                        icon_button("terminal-new", IconName::Plus, "New terminal", cx).on_click(
                            cx.listener(|this, _, window, cx| {
                                let (dir, title) = this.tabs.get(this.active).map_or_else(
                                    || (home_dir(), "Terminal".to_owned()),
                                    |t| (t.view.read(cx).cwd().to_path_buf(), t.title.clone()),
                                );
                                this.open(dir, title, None, true, window, cx);
                            }),
                        ),
                    )
                    .child(
                        icon_button(
                            "terminal-hide",
                            IconName::Close,
                            "Hide the terminal panel",
                            cx,
                        )
                        .mr_1()
                        .on_click(cx.listener(|_, _, _, cx| {
                            cx.emit(TerminalPanelEvent::Hide);
                        })),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .overflow_hidden()
                    .children(self.tabs.get(self.active).map(|t| t.view.clone())),
            )
    }
}

/// Where a terminal with no repo starts: the user's home folder.
pub fn home_dir() -> PathBuf {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_default()
}
