//! Squashing commits of the current branch from the graph menu: the range
//! is checked and its messages read in the background, then a dialog lets
//! the combined message be edited before the squash runs.

use archipel_model::Oid;
use archipel_ops::OpError;
use archipel_theme::ColorToken as T;
use gpui_kit::component::input::{Textarea, TextareaState};
use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, Context, Entity, KeyDownEvent, Window, div, px};

use crate::theme::{color, radius};
use crate::widgets::{SHORT_OID, button, modal, primary_button};

use super::RepoView;

/// Which commits a squash takes, from the commit right-clicked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum SquashRange {
    /// The commit and its parent.
    IntoParent(Oid),
    /// The commit and every commit after it, up to `HEAD`.
    UpToHead(Oid),
    /// The commits selected in the graph, in graph order (newest first):
    /// they must follow one another, with no gap.
    Selected(Vec<Oid>),
}

/// The open squash dialog.
pub(super) struct SquashDialog {
    oldest: Oid,
    newest: Oid,
    count: usize,
    message: Entity<TextareaState>,
}

/// Why the selected commits cannot be squashed as they are: the squash
/// from the oldest to the newest would take other commits as well.
fn gap(selected: &[Oid], taken: &[Oid]) -> Option<String> {
    let missing = taken.iter().filter(|c| !selected.contains(c)).count();
    (missing > 0 || taken.len() != selected.len()).then(|| {
        format!(
            "the selected commits do not follow one another in the history of the current              branch ({missing} commit(s) between them are not selected)"
        )
    })
}

impl RepoView {
    /// Checks `range` and reads its messages in the background, then opens
    /// the dialog; a range that cannot be squashed only sets the status.
    pub(super) fn start_squash(
        &mut self,
        range: SquashRange,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let actions = self.core.repo_actions().clone();
        let path = self.path.clone();
        let task = cx.background_spawn(async move {
            let refs = &actions.ports().refs;
            let resolve = |rev: String| -> Result<Oid, OpError> {
                refs.resolve(&path, &rev)?
                    .ok_or_else(|| OpError::Refused(format!("{rev} has no commit")))
            };
            let (oldest, newest) = match range {
                SquashRange::IntoParent(oid) => (
                    resolve(format!("{}^", oid.to_hex())).map_err(|_| {
                        OpError::Refused(format!(
                            "{} has no parent to squash into",
                            oid.short(SHORT_OID)
                        ))
                    })?,
                    oid,
                ),
                SquashRange::UpToHead(oid) => (oid, resolve("HEAD".into())?),
                SquashRange::Selected(ref oids) => match (oids.last(), oids.first()) {
                    (Some(&oldest), Some(&newest)) => (oldest, newest),
                    _ => return Err(OpError::Refused("no commit selected".into())),
                },
            };
            let preview = actions.squash_preview(&path, oldest, newest)?;
            if let SquashRange::Selected(oids) = &range
                && let Some(why) = gap(oids, &preview.commits)
            {
                return Err(OpError::Refused(why));
            }
            Ok::<_, OpError>((oldest, newest, preview))
        });
        self.status = Some("Squash…".into());
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |v, window, cx| {
                match result {
                    Ok((oldest, newest, preview)) => {
                        v.status = None;
                        let input = cx.new(|cx| {
                            TextareaState::new(window, cx)
                                .rows(8)
                                .placeholder("Message of the squashed commit")
                                .default_value(preview.message)
                        });
                        input.update(cx, |i, cx| i.focus(window, cx));
                        v.squash = Some(SquashDialog {
                            oldest,
                            newest,
                            count: preview.commits.len(),
                            message: input,
                        });
                    }
                    Err(e) => v.status = Some(format!("Cannot squash: {e}")),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn close_squash(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.squash = None;
        self.focus.focus(window, cx);
        cx.notify();
    }

    fn submit_squash(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dialog) = self.squash.as_ref() else {
            return;
        };
        let message = dialog.message.read(cx).value().to_string();
        if message.trim().is_empty() {
            self.status = Some("Write a message for the squashed commit".into());
            cx.notify();
            return;
        }
        let (oldest, newest) = (dialog.oldest, dialog.newest);
        self.close_squash(window, cx);
        self.run(
            format!(
                "Squash {}..{}",
                oldest.short(SHORT_OID),
                newest.short(SHORT_OID)
            ),
            cx,
            move |a, p, _| a.squash(p, oldest, newest, &message),
        );
    }

    /// The squash dialog: the range, the message, Squash / Cancel.
    pub(super) fn render_squash(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let dialog = self.squash.as_ref()?;
        let title = format!(
            "Squash {} commits ({} to {}) into one",
            dialog.count,
            dialog.oldest.short(SHORT_OID),
            dialog.newest.short(SHORT_OID)
        );
        let body = div()
            .id("squash-dialog")
            .occlude()
            .on_key_down(cx.listener(|v, e: &KeyDownEvent, window, cx| {
                let k = &e.keystroke;
                if k.key == "escape" {
                    cx.stop_propagation();
                    v.close_squash(window, cx);
                } else if k.key == "enter" && (k.modifiers.control || k.modifiers.platform) {
                    cx.stop_propagation();
                    v.submit_squash(window, cx);
                }
            }))
            .w(px(560.0))
            .flex()
            .flex_col()
            .gap_3()
            .p_4()
            .bg(color(cx, T::BgElevated))
            .border_1()
            .border_color(color(cx, T::Border))
            .rounded(radius(cx))
            .shadow_lg()
            .child(title)
            .child(Textarea::new(&dialog.message))
            .child(div().text_xs().text_color(color(cx, T::FgMuted)).child(
                "The commits after them are rewritten on top, unchanged. Commits \
                         already pushed then need a force push. Ctrl+Enter squashes.",
            ))
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        button("squash-cancel", "Cancel", cx)
                            .on_click(cx.listener(|v, _, window, cx| v.close_squash(window, cx))),
                    )
                    .child(
                        primary_button("squash-ok", "Squash", cx)
                            .on_click(cx.listener(|v, _, window, cx| v.submit_squash(window, cx))),
                    ),
            );
        Some(modal(body, cx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn oid(n: u8) -> Oid {
        Oid::from_bytes(&[n; 20]).unwrap()
    }

    #[test]
    fn selected_commits_must_follow_one_another() {
        let taken = [oid(1), oid(2), oid(3)];
        assert_eq!(gap(&[oid(3), oid(2), oid(1)], &taken), None);
        assert!(gap(&[oid(3), oid(1)], &taken).is_some_and(|w| w.contains("1 commit")));
        // A selected commit off the range (another branch).
        assert!(gap(&[oid(3), oid(9), oid(2), oid(1)], &taken).is_some());
    }
}
