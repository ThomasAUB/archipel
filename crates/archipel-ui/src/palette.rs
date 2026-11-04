//! A fuzzy palette (project switcher, action palette) that can also act as a
//! one-line prompt (branch or tag name).

use archipel_app::fuzzy;
use archipel_theme::ColorToken as T;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::prelude::*;
use gpui_kit::{Context, EventEmitter, KeyDownEvent, SharedString, Subscription, Window, div, px};

use crate::theme::{color, radius};

/// One entry of the palette.
#[derive(Clone, Debug)]
pub struct PaletteItem {
    /// Value returned when confirmed.
    pub id: String,
    /// Main text (matched by the query).
    pub label: String,
    /// Secondary text (shortcut, path…).
    pub detail: String,
}

/// What the palette reports to its owner.
pub enum PaletteEvent {
    /// An item (its id) or, in prompt mode, the typed text.
    Confirmed(String),
    /// Escape.
    Dismissed,
}

/// Maximum rows shown at once.
const VISIBLE_ITEMS: usize = 14;

/// The palette view.
pub struct Palette {
    title: SharedString,
    items: Vec<PaletteItem>,
    query: gpui_kit::Entity<InputState>,
    selected: usize,
    /// Prompt mode: Enter confirms the typed text.
    prompt: bool,
    _subscription: Subscription,
}

impl EventEmitter<PaletteEvent> for Palette {}

impl Palette {
    /// A palette over `items`; with `prompt`, Enter returns the typed text.
    pub fn new(
        title: impl Into<SharedString>,
        items: Vec<PaletteItem>,
        prompt: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let placeholder = if prompt {
            "Type a name and press Enter"
        } else {
            "Type to search…"
        };
        let query = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
        query.update(cx, |q, cx| q.focus(window, cx));
        let subscription =
            cx.subscribe(
                &query,
                |this: &mut Self, _, event: &InputEvent, cx| match event {
                    InputEvent::Change => {
                        this.selected = 0;
                        cx.notify();
                    }
                    InputEvent::PressEnter { .. } => this.confirm(cx),
                    _ => {}
                },
            );
        Self {
            title: title.into(),
            items,
            query,
            selected: 0,
            prompt,
            _subscription: subscription,
        }
    }

    fn matches(&self, cx: &gpui_kit::App) -> Vec<PaletteItem> {
        let q = self.query.read(cx).value();
        fuzzy::rank(&q, &self.items, |i| i.label.as_str())
            .into_iter()
            .cloned()
            .collect()
    }

    fn confirm(&mut self, cx: &mut Context<Self>) {
        if self.prompt {
            let text = self.query.read(cx).value().trim().to_owned();
            if !text.is_empty() {
                cx.emit(PaletteEvent::Confirmed(text));
            }
            return;
        }
        if let Some(item) = self.matches(cx).get(self.selected) {
            cx.emit(PaletteEvent::Confirmed(item.id.clone()));
        }
    }

    fn on_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let count = self.matches(cx).len();
        match event.keystroke.key.as_str() {
            "down" if count > 0 => self.selected = (self.selected + 1).min(count - 1),
            "up" => self.selected = self.selected.saturating_sub(1),
            "escape" => cx.emit(PaletteEvent::Dismissed),
            _ => return,
        }
        cx.notify();
    }
}

impl Render for Palette {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let matches = self.matches(cx);
        let first = self.selected.saturating_sub(VISIBLE_ITEMS - 1);
        let selected_bg = color(cx, T::BgSelected);
        let hover = color(cx, T::BgHover);
        let rows = matches
            .into_iter()
            .enumerate()
            .skip(first)
            .take(VISIBLE_ITEMS)
            .map(|(i, item)| {
                let id = item.id.clone();
                div()
                    .id(SharedString::from(format!("palette-item-{i}")))
                    .flex()
                    .justify_between()
                    .gap_4()
                    .px_3()
                    .py_1()
                    .rounded(radius(cx))
                    .cursor_pointer()
                    .when(i == self.selected, |d| d.bg(selected_bg))
                    .hover(move |s| s.bg(hover))
                    .on_click(cx.listener(move |_, _, _, cx| {
                        cx.emit(PaletteEvent::Confirmed(id.clone()));
                    }))
                    .child(
                        div()
                            .flex_none()
                            .text_color(color(cx, T::FgDefault))
                            .child(item.label),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .text_color(color(cx, T::FgMuted))
                            .child(item.detail),
                    )
            });
        div()
            .absolute()
            .top(px(60.0))
            .left_0()
            .right_0()
            .flex()
            .justify_center()
            .child(
                div()
                    .w(px(560.0))
                    .flex()
                    .flex_col()
                    .gap_1()
                    .p_2()
                    .rounded(radius(cx))
                    .border_1()
                    .border_color(color(cx, T::Border))
                    .bg(color(cx, T::BgElevated))
                    .shadow_lg()
                    .on_key_down(cx.listener(|this, e: &KeyDownEvent, _, cx| this.on_key(e, cx)))
                    .child(
                        div()
                            .px_1()
                            .text_xs()
                            .text_color(color(cx, T::FgSubtle))
                            .child(self.title.clone()),
                    )
                    .child(Input::new(&self.query))
                    .children(rows),
            )
    }
}
