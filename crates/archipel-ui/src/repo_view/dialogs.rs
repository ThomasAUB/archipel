//! The name form under the toolbar (new branch or tag, rename) and the
//! dialog confirming a destructive action.

use archipel_model::Oid;
use archipel_model::requests::{CheckoutTarget, TagRequest};
use archipel_theme::ColorToken as T;
use gpui_kit::component::input::Input;
use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, Context, KeyDownEvent, Window, div, px};

use crate::theme::{color, radius};
use crate::widgets::{SHORT_OID, button, modal, primary_button};

use super::RepoView;
use super::menu::MenuAction;

/// What the name form under the toolbar creates or renames.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum NameForm {
    /// A branch at a commit, then checked out; `None` follows the
    /// selection (`HEAD` when nothing is selected).
    Branch(Option<Oid>),
    /// A lightweight tag at a commit.
    Tag(Oid),
    /// A new name for a local branch.
    Rename(String),
}

/// An action waiting for confirmation in a dialog.
pub(super) struct Confirm {
    pub(super) question: String,
    pub(super) action: MenuAction,
}

impl RepoView {
    /// Opens the name form for `form`, or closes it (`None`). A rename
    /// starts from the old name.
    pub(super) fn open_name_form(
        &mut self,
        form: Option<NameForm>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let initial = match &form {
            Some(NameForm::Rename(old)) => old.clone(),
            _ => String::new(),
        };
        self.name_input
            .update(cx, |b, cx| b.set_value(initial, window, cx));
        self.name_form = form;
        if self.name_form.is_some() {
            self.name_input.update(cx, |b, cx| b.focus(window, cx));
        } else {
            self.focus.focus(window, cx);
        }
        cx.notify();
    }

    /// Creates the branch (then checks it out) or tag, or renames the
    /// branch, of the name form.
    pub(super) fn submit_name(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(form) = self.name_form.clone() else {
            return;
        };
        let name = self.name_input.read(cx).value().trim().to_owned();
        if name.is_empty() {
            self.status = Some("Write a name first".into());
            cx.notify();
            return;
        }
        let selected = self.selected.map(|(_, oid)| oid);
        self.open_name_form(None, window, cx);
        match form {
            NameForm::Branch(start) => {
                let start = start.or(selected).map(|oid| oid.to_hex());
                self.run(format!("Create branch {name}"), cx, move |a, p, _| {
                    a.create_branch(p, &name, start.as_deref(), None)?;
                    a.checkout(p, &CheckoutTarget::Branch(name), None)
                });
            }
            NameForm::Tag(oid) => {
                let request = TagRequest {
                    name: name.clone(),
                    target: Some(oid.to_hex()),
                    message: None,
                };
                self.run(format!("Create tag {name}"), cx, move |a, p, _| {
                    a.create_tag(p, &request, None)
                });
            }
            NameForm::Rename(old) if old == name => {}
            NameForm::Rename(old) => {
                self.run(format!("Rename {old} → {name}"), cx, move |a, p, _| {
                    a.rename_branch(p, &old, &name)
                });
            }
        }
    }

    pub(super) fn confirm(&mut self, accept: bool, window: &mut Window, cx: &mut Context<Self>) {
        let pending = self.confirm.take();
        self.focus.focus(window, cx);
        match pending {
            Some(c) if accept => self.perform(c.action, window, cx),
            _ => cx.notify(),
        }
    }

    /// The name form: what is named, the name, OK / Cancel.
    pub(super) fn render_name_form(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (title, ok) = match self.name_form.as_ref()? {
            NameForm::Branch(start) => {
                let start = start
                    .or(self.selected.map(|(_, oid)| oid))
                    .map_or_else(|| "HEAD".to_owned(), |oid| oid.short(SHORT_OID));
                (format!("New branch at {start}"), "Create")
            }
            NameForm::Tag(oid) => (format!("New tag at {}", oid.short(SHORT_OID)), "Create"),
            NameForm::Rename(old) => (format!("Rename {old} to"), "Rename"),
        };
        Some(
            div()
                .flex()
                .items_center()
                .gap_2()
                .px_3()
                .py_1()
                .border_b_1()
                .border_color(color(cx, T::BorderSubtle))
                .on_key_down(cx.listener(|v, e: &KeyDownEvent, window, cx| {
                    if e.keystroke.key == "escape" {
                        cx.stop_propagation();
                        v.open_name_form(None, window, cx);
                    }
                }))
                .child(
                    div()
                        .flex_none()
                        .text_color(color(cx, T::FgMuted))
                        .child(title),
                )
                .child(div().w(px(260.0)).child(Input::new(&self.name_input)))
                .child(
                    primary_button("name-ok", ok, cx)
                        .on_click(cx.listener(|v, _, window, cx| v.submit_name(window, cx))),
                )
                .child(
                    button("name-cancel", "Cancel", cx).on_click(
                        cx.listener(|v, _, window, cx| v.open_name_form(None, window, cx)),
                    ),
                )
                .into_any_element(),
        )
    }

    /// The dialog confirming a destructive menu action.
    pub(super) fn render_confirm(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let confirm = self.confirm.as_ref()?;
        let dialog = div()
            .id("confirm-prompt")
            .occlude()
            .track_focus(&self.prompt_focus)
            .on_key_down(cx.listener(|v, e: &KeyDownEvent, window, cx| {
                match e.keystroke.key.as_str() {
                    "escape" => v.confirm(false, window, cx),
                    "enter" => v.confirm(true, window, cx),
                    _ => return,
                }
                cx.stop_propagation();
            }))
            .w(px(440.0))
            .flex()
            .flex_col()
            .gap_3()
            .p_4()
            .bg(color(cx, T::BgElevated))
            .border_1()
            .border_color(color(cx, T::Border))
            .rounded(radius(cx))
            .shadow_lg()
            .child(confirm.question.clone())
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        button("confirm-cancel", "Cancel", cx)
                            .on_click(cx.listener(|v, _, window, cx| v.confirm(false, window, cx))),
                    )
                    .child(
                        primary_button("confirm-ok", "Continue", cx)
                            .on_click(cx.listener(|v, _, window, cx| v.confirm(true, window, cx))),
                    ),
            );
        Some(modal(dialog, cx))
    }
}
