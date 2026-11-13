//! The details pane: the staging panel for the uncommitted changes, else
//! the selected commit or stash and its changed files.

use std::ops::Range;
use std::sync::Arc;

use archipel_model::Timestamp;
use archipel_model::changes::ChangeKind;
use archipel_model::diff::FileDiff;
use archipel_model::requests::StashApplyMode;
use archipel_theme::ColorToken as T;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, ClickEvent, Context, SharedString, div, px, uniform_list};

use crate::diff_view::DiffDoc;
use crate::staging::{self};
use crate::theme::{color, mono_font, row_height};
use crate::widgets::{SHORT_OID, button, scroll_list, section};

use super::dialogs::NameForm;
use super::{OpenDiff, RepoView};

/// Height of the commit message body in the details pane before it scrolls.
const MESSAGE_BODY_HEIGHT: f32 = 160.0;

impl RepoView {
    /// The details pane: the staging panel for the uncommitted changes,
    /// else the selected commit or stash. The repo actions are in the
    /// toolbar.
    pub(super) fn render_details(&mut self, cx: &mut Context<Self>) -> AnyElement {
        if self.browser.is_some() {
            return self.render_browser(cx);
        }
        if self.wip_selected {
            return staging::render_body(self, cx);
        }
        let selection = self.render_selection(cx);
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(selection)
            .into_any_element()
    }

    /// The Repo Actions Bar of the toolbar, led by the Branch button.
    pub(super) fn render_repo_actions(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let branch = staging::small(button("r-branch", "Branch", cx))
            .on_click(cx.listener(|v, _, window, cx| {
                let form = match v.name_form {
                    Some(_) => None,
                    None => Some(NameForm::Branch(None)),
                };
                v.open_name_form(form, window, cx);
            }))
            .into_any_element();
        staging::render_actions(self, Some(branch), cx)
    }

    /// Details of the selected commit or stash.
    fn render_selection(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let Some((oid, doc)) = &self.details else {
            return div()
                .p_3()
                .text_color(color(cx, T::FgMuted))
                .child("Select a commit")
                .into_any_element();
        };
        let (oid, doc) = (*oid, doc.clone());
        if self.stash_selected == Some(oid)
            && let Some(k) = self.stash_at(oid)
        {
            return self.render_stash_details(k, doc, cx);
        }
        let meta = self.selected.and_then(|(r, _)| self.meta.get(&r).cloned());
        let message = self
            .message
            .as_ref()
            .filter(|(o, _)| *o == oid)
            .map(|(_, m)| m.clone());
        let mut el = div().p_2().flex().flex_col().gap_1();
        let editing = self.reword.as_ref().is_some_and(|r| r.oid() == oid);
        if editing && let Some(editor) = self.render_reword(cx) {
            el = el.child(editor);
        }
        if let Some(m) = meta {
            let (title, body) = match &message {
                Some(msg) => match msg.split_once('\n') {
                    Some((t, b)) => (SharedString::from(t.to_owned()), b.trim()),
                    None => (msg.clone(), ""),
                },
                None => (m.summary.clone().into(), ""),
            };
            if !editing {
                // A double-click on the message edits it.
                let mut text = div()
                    .id("details-message-text")
                    .flex()
                    .flex_col()
                    .gap_1()
                    .on_click(cx.listener(move |v, e: &ClickEvent, window, cx| {
                        if e.click_count() > 1 {
                            v.start_reword(oid, window, cx);
                        }
                    }))
                    .child(
                        div()
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .child(title),
                    );
                if !body.is_empty() {
                    text = text.child(
                        div()
                            .id("details-message")
                            .max_h(px(MESSAGE_BODY_HEIGHT))
                            .overflow_y_scrollbar()
                            .text_sm()
                            .text_color(color(cx, T::FgMuted))
                            .child(body.to_owned()),
                    );
                }
                el = el.child(text);
            }
            el = el.child(
                div()
                    .text_xs()
                    .text_color(color(cx, T::FgMuted))
                    .child(format!("{} <{}>", m.author_name, m.author_email)),
            );
        }
        el = el.child(
            div()
                .text_xs()
                .font_family(mono_font(cx))
                .text_color(color(cx, T::FgSubtle))
                .child(oid.to_hex()),
        );
        Self::render_diff_details(el, &oid.to_hex(), doc, cx)
    }

    /// Details of a stash: its message, the commit it was made on, and
    /// Apply / Pop / Drop.
    fn render_stash_details(
        &mut self,
        k: usize,
        doc: Arc<DiffDoc>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let entry = self.placed()[k].entry.clone();
        let oid = entry.oid;
        let op = |id: &'static str, label: &'static str, mode, cx: &mut Context<Self>| {
            button(id, label, cx)
                .on_click(cx.listener(move |v, _, _, cx| v.stash_op(oid, mode, cx)))
        };
        let header = div()
            .p_2()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .child(format!(
                        "stash@{{{}}}: {}",
                        entry.index,
                        stash_title(&entry.message)
                    )),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(color(cx, T::FgMuted))
                    .child(format!(
                        "Made on {}, {}",
                        entry.base.short(SHORT_OID),
                        archipel_app::dashboard::relative_time(entry.time, Timestamp::now())
                    )),
            )
            .child(
                div()
                    .flex()
                    .gap_1()
                    .child(op("s-apply", "Apply", Some(StashApplyMode::Keep), cx))
                    .child(op("s-pop", "Pop", Some(StashApplyMode::Pop), cx))
                    .child(op("s-drop", "Drop", None, cx)),
            );
        Self::render_diff_details(header, &format!("stash-{oid}"), doc, cx)
    }

    /// A details header and the changed-files list below it.
    fn render_diff_details(
        header: gpui_kit::Div,
        key: &str,
        doc: Arc<DiffDoc>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let count = doc.files.len();
        // "All files" heads the list.
        let list = uniform_list(
            SharedString::from(format!("details-files-{key}")),
            count + 1,
            cx.processor(move |this: &mut Self, range: Range<usize>, _window, cx| {
                let open = match &this.open_diff {
                    Some(OpenDiff::Commit(file, _)) => Some(*file),
                    _ => None,
                };
                range
                    .filter(|&i| i <= doc.files.len())
                    .map(|i| {
                        let file = i.checked_sub(1);
                        render_changed_file(&doc, file, open == Some(file), cx)
                    })
                    .collect()
            }),
        );
        let list = scroll_list(SharedString::from(format!("details-scroll-{key}")), list);
        div()
            .flex_1()
            .flex()
            .flex_col()
            .overflow_hidden()
            .child(header)
            .child(
                section(format!("FILES ({count})"), cx)
                    .pt_1()
                    .border_t_1()
                    .border_color(color(cx, T::BorderSubtle)),
            )
            .map(|d| {
                if count == 0 {
                    d.child(
                        div()
                            .px_2()
                            .text_color(color(cx, T::FgMuted))
                            .child("No changes"),
                    )
                } else {
                    d.child(list)
                }
            })
            .into_any_element()
    }
}

/// Message of a stash without the `On <branch>: ` Git prefixes to the
/// message given at push time.
pub(super) fn stash_title(message: &str) -> &str {
    message
        .strip_prefix("On ")
        .and_then(|m| m.split_once(": "))
        .map_or(message, |(_, m)| m)
}

/// A file's path, both of them when it was renamed.
pub(super) fn file_title(file: &FileDiff) -> String {
    match (&file.old_path, &file.new_path) {
        (Some(old), Some(new)) if old != new => format!("{old} → {new}"),
        _ => file.display_path().to_owned(),
    }
}

/// One row of the changed-files list: change kind, path, line counts;
/// `file` `None` is the "All files" row. A click opens it in place of the
/// graph, or, when `open` (shown), brings the graph back.
fn render_changed_file(
    doc: &Arc<DiffDoc>,
    file: Option<usize>,
    open: bool,
    cx: &mut Context<RepoView>,
) -> AnyElement {
    let (label, token, path, stats) = match file {
        None => {
            let (added, removed) = doc
                .files
                .iter()
                .map(FileDiff::line_stats)
                .fold((0, 0), |(a, r), (x, y)| (a + x, r + y));
            (
                "∗",
                T::FgMuted,
                "All files".to_owned(),
                format!("+{added} −{removed}"),
            )
        }
        Some(i) => {
            let file = &doc.files[i];
            let (label, token) = match file.kind {
                ChangeKind::Added | ChangeKind::Copied => ("+", T::DiffAddFg),
                ChangeKind::Deleted => ("−", T::DiffDelFg),
                ChangeKind::Renamed
                | ChangeKind::TypeChanged
                | ChangeKind::Modified
                | ChangeKind::Unmodified => ("✎", T::StatusDirty),
            };
            let stats = if file.binary {
                "binary".to_owned()
            } else {
                let (added, removed) = file.line_stats();
                format!("+{added} −{removed}")
            };
            (label, token, file_title(file), stats)
        }
    };
    let hover = color(cx, T::BgHover);
    div()
        .id(SharedString::from(format!(
            "cf-{}",
            file.map_or_else(|| "all".to_owned(), |i| i.to_string())
        )))
        .w_full()
        .h(row_height(cx))
        .flex()
        .items_center()
        .gap_2()
        .px_2()
        .cursor_pointer()
        .when(open, |d| d.bg(color(cx, T::BgSelected)))
        .hover(move |s| s.bg(hover))
        .on_click(cx.listener(move |v, _, window, cx| {
            if open {
                v.close_diff(window, cx);
            } else {
                v.focus.focus(window, cx);
                v.open_commit_file(file, cx);
            }
        }))
        .child(
            div()
                .w(px(14.0))
                .flex_none()
                .text_color(color(cx, token))
                .child(label),
        )
        .child(div().flex_1().min_w_0().truncate().child(path))
        .child(
            div()
                .flex_none()
                .text_xs()
                .text_color(color(cx, T::FgMuted))
                .child(stats),
        )
        .into_any_element()
}
