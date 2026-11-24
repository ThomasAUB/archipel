//! A fuzzy palette (project switcher, action palette) that can also act as a
//! one-line prompt (branch or tag name), optionally with suggestions
//! (category name).

use std::collections::HashSet;

use archipel_app::fuzzy;
use archipel_theme::ColorToken as T;
use gpui_kit::assets::IconName;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::prelude::*;
use gpui_kit::{
    Context, Div, EventEmitter, KeyDownEvent, SharedString, Stateful, Subscription, Window, div,
    px, svg,
};

use crate::theme::{color, radius};
use crate::widgets::{ListMove, TextTooltip, list_row};

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
    /// The remove button of a removable item (its id) was clicked: the item
    /// is gone from the palette, which stays open.
    Removed(String),
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
    /// The selection was moved with the arrow keys since the last edit.
    moved: bool,
    /// Ids of the items that show a remove button when hovered.
    removable: HashSet<String>,
    _subscription: Subscription,
}

impl EventEmitter<PaletteEvent> for Palette {}

impl Palette {
    /// A palette over `items`; with `prompt`, Enter returns the typed text,
    /// or an item when it was picked with the arrows, when its label is the
    /// typed text (case-insensitive) or when nothing is typed.
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
                        this.moved = false;
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
            moved: false,
            removable: HashSet::new(),
            _subscription: subscription,
        }
    }

    /// Lets the items of `ids` be removed from the palette (a discreet ×
    /// shown when the row is hovered), reported as [`PaletteEvent::Removed`].
    pub fn set_removable(&mut self, ids: HashSet<String>, cx: &mut Context<Self>) {
        self.removable = ids;
        cx.notify();
    }

    /// Drops item `id` and reports it to the owner.
    fn remove(&mut self, id: &str, cx: &mut Context<Self>) {
        self.items.retain(|i| i.id != id);
        self.removable.remove(id);
        let count = self.matches(cx).len();
        self.selected = self.selected.min(count.saturating_sub(1));
        cx.emit(PaletteEvent::Removed(id.to_owned()));
        cx.notify();
    }

    /// Puts `text` in the field (a prompt starting from the current value).
    pub fn set_query(&mut self, text: String, window: &mut Window, cx: &mut Context<Self>) {
        self.query.update(cx, |q, cx| q.set_value(text, window, cx));
    }

    fn matches(&self, cx: &gpui_kit::App) -> Vec<PaletteItem> {
        let q = self.query.read(cx).value();
        fuzzy::rank(&q, &self.items, |i| i.label.as_str())
            .into_iter()
            .cloned()
            .collect()
    }

    fn confirm(&mut self, cx: &mut Context<Self>) {
        let matches = self.matches(cx);
        if self.prompt {
            let text = self.query.read(cx).value().trim().to_owned();
            let item = if self.moved || text.is_empty() {
                matches.get(self.selected)
            } else {
                matches
                    .iter()
                    .find(|i| i.label.to_lowercase() == text.to_lowercase())
            };
            if let Some(item) = item {
                cx.emit(PaletteEvent::Confirmed(item.id.clone()));
            } else if !text.is_empty() {
                cx.emit(PaletteEvent::Confirmed(text));
            }
            return;
        }
        if let Some(item) = matches.get(self.selected) {
            cx.emit(PaletteEvent::Confirmed(item.id.clone()));
        }
    }

    /// The discreet × of row `i` (item `id`), shown while row `group` is
    /// hovered.
    fn remove_button(
        i: usize,
        id: String,
        group: SharedString,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let hover = color(cx, T::FgDefault);
        // An svg paints with its own text color only (not the parent's).
        let button = SharedString::from(format!("palette-remove-{i}"));
        div()
            .id(button.clone())
            .group(button.clone())
            .flex_none()
            .opacity(0.0)
            .group_hover(group, |s| s.opacity(1.0))
            .tooltip(|_, cx| TextTooltip::view(&["Remove from the list".into()], cx))
            .on_click(cx.listener(move |this, _, _, cx| {
                // Not a click on the row: that would open it.
                cx.stop_propagation();
                this.remove(&id, cx);
            }))
            .child(
                svg()
                    .path(IconName::Close.path())
                    .size(px(12.0))
                    .text_color(color(cx, T::FgSubtle))
                    .group_hover(button, move |s| s.text_color(hover)),
            )
    }

    fn on_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let count = self.matches(cx).len();
        match event.keystroke.key.as_str() {
            "escape" => cx.emit(PaletteEvent::Dismissed),
            // Home and End stay the text field's.
            key => match ListMove::from_key(key) {
                Some(to @ ListMove::By(_)) => {
                    if let Some(i) = to.apply(Some(self.selected), count) {
                        self.selected = i;
                        self.moved = true;
                    }
                }
                _ => return,
            },
        }
        cx.notify();
    }
}

impl Render for Palette {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let matches = self.matches(cx);
        let first = self.selected.saturating_sub(VISIBLE_ITEMS - 1);
        // Every row keeps the slot of the remove button when any item has
        // one, so the details stay aligned.
        let with_remove = !self.removable.is_empty();
        let rows = matches
            .into_iter()
            .enumerate()
            .skip(first)
            .take(VISIBLE_ITEMS)
            .map(|(i, item)| {
                let id = item.id.clone();
                let group = SharedString::from(format!("palette-row-{i}"));
                let remove = self
                    .removable
                    .contains(&item.id)
                    .then(|| Self::remove_button(i, item.id.clone(), group.clone(), cx));
                list_row(
                    SharedString::from(format!("palette-item-{i}")),
                    i == self.selected,
                    cx,
                )
                .group(group)
                .flex()
                .items_center()
                .justify_between()
                .gap_4()
                .px_3()
                .py_1()
                .rounded(radius(cx))
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
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_xs()
                                .text_color(color(cx, T::FgMuted))
                                .child(item.detail),
                        )
                        .when(with_remove, |d| {
                            d.child(div().flex_none().w(px(12.0)).children(remove))
                        }),
                )
            })
            .collect::<Vec<_>>();
        div()
            .absolute()
            .top(px(60.0))
            .left_0()
            .right_0()
            .flex()
            .justify_center()
            .child(
                div()
                    .id("palette")
                    .occlude()
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
