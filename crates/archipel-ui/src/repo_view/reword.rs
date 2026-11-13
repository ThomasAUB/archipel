//! Rewording a commit from the details pane: a double-click on its message
//! turns it into an editor; Save gives the commit the new message and
//! rewrites the commits after it on top, unchanged.

use archipel_model::Oid;
use archipel_theme::ColorToken as T;
use gpui_kit::component::input::{Textarea, TextareaState};
use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, Context, Entity, KeyDownEvent, Window, div};

use crate::theme::color;
use crate::widgets::{SHORT_OID, button, primary_button};

use super::RepoView;

/// The message of a commit being edited in the details pane.
pub(super) struct RewordEdit {
    oid: Oid,
    original: String,
    message: Entity<TextareaState>,
}

impl RewordEdit {
    pub(super) fn oid(&self) -> Oid {
        self.oid
    }
}

/// Whether `edited` differs from `original` once both are normalized as
/// Git stores a message (outer blank lines and trailing spaces dropped).
fn changed(original: &str, edited: &str) -> bool {
    let norm =
        |m: &str| -> Vec<String> { m.trim().lines().map(|l| l.trim_end().to_owned()).collect() };
    norm(original) != norm(edited)
}

impl RepoView {
    /// Opens the message editor on `oid`, filled with its full message
    /// (its summary while the message is not read yet).
    pub(super) fn start_reword(&mut self, oid: Oid, window: &mut Window, cx: &mut Context<Self>) {
        let original = match &self.message {
            Some((o, m)) if *o == oid => m.to_string(),
            _ => match self.selected.and_then(|(r, _)| self.meta.get(&r)) {
                Some(meta) => meta.summary.clone(),
                None => return,
            },
        };
        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(3, 14)
                .placeholder("Commit message")
                .default_value(original.clone())
        });
        input.update(cx, |i, cx| i.focus(window, cx));
        self.reword = Some(RewordEdit {
            oid,
            original,
            message: input,
        });
        cx.notify();
    }

    pub(super) fn cancel_reword(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.reword = None;
        self.focus.focus(window, cx);
        cx.notify();
    }

    fn submit_reword(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(edit) = self.reword.as_ref() else {
            return;
        };
        let message = edit.message.read(cx).value().to_string();
        if message.trim().is_empty() {
            self.status = Some("Write a message for the commit".into());
            cx.notify();
            return;
        }
        let oid = edit.oid;
        let unchanged = !changed(&edit.original, &message);
        self.cancel_reword(window, cx);
        if unchanged {
            return;
        }
        self.run(
            format!("Reword {}", oid.short(SHORT_OID)),
            cx,
            move |a, p, _| a.reword(p, oid, &message),
        );
    }

    /// The message editor of the details pane: the message, Save / Cancel.
    pub(super) fn render_reword(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let edit = self.reword.as_ref()?;
        Some(
            div()
                .id("reword-editor")
                .flex()
                .flex_col()
                .gap_2()
                .on_key_down(cx.listener(|v, e: &KeyDownEvent, window, cx| {
                    let k = &e.keystroke;
                    if k.key == "escape" {
                        cx.stop_propagation();
                        v.cancel_reword(window, cx);
                    } else if k.key == "enter" && (k.modifiers.control || k.modifiers.platform) {
                        cx.stop_propagation();
                        v.submit_reword(window, cx);
                    }
                }))
                .child(Textarea::new(&edit.message))
                .child(div().text_xs().text_color(color(cx, T::FgMuted)).child(
                    "The commits after it are rewritten on top, unchanged; pushed commits \
                     then need a force push. Ctrl+Enter saves, Esc cancels.",
                ))
                .child(
                    div()
                        .flex()
                        .justify_end()
                        .gap_2()
                        .child(
                            button("reword-cancel", "Cancel", cx).on_click(
                                cx.listener(|v, _, window, cx| v.cancel_reword(window, cx)),
                            ),
                        )
                        .child(
                            primary_button("reword-ok", "Save", cx).on_click(
                                cx.listener(|v, _, window, cx| v.submit_reword(window, cx)),
                            ),
                        ),
                )
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::changed;

    #[test]
    fn only_a_real_edit_counts_as_a_change() {
        assert!(!changed("title\n\nbody\n", "  title\n\nbody  \n\n"));
        assert!(changed("title\n", "title!\n"));
        assert!(changed("title\n\nbody\n", "title\nbody\n"));
    }
}
