//! Worktrees in a repo tab (spec §10, WORKTREES.md §4.4, §7): the
//! WORKTREES section of the refs panel, the branches checked out in another
//! worktree, the prompt replacing Git's "already checked out" refusal, and
//! the Worktree Chip of the toolbar.

use std::collections::HashMap;
use std::path::PathBuf;

use archipel_app::dashboard::WORKTREE_GLYPH;
use archipel_model::refs::RefKind;
use archipel_model::requests::CheckoutTarget;
use archipel_model::{Oid, RepoId};
use archipel_theme::ColorToken as T;
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, ClickEvent, Context, KeyDownEvent, MouseButton, MouseDownEvent, SharedString, div,
    px,
};

use crate::staging::StagingEvent;
use crate::theme::{color, radius, row_height};
use crate::widgets::{SHORT_OID, TextTooltip, button, modal, primary_button};
use crate::worktree_dialog::WorktreeStart;

use super::RepoView;
use super::refs_panel::RefRow;

/// Branches checked out in another worktree of the repo: its row (when
/// shown) and folder.
pub(super) type Elsewhere = HashMap<String, (Option<RepoId>, PathBuf)>;

/// A worktree of the repo in the refs panel's WORKTREES section.
#[derive(Clone, Debug)]
pub(super) struct WorktreeEntry {
    /// Its row, when the project shows it.
    pub(super) id: Option<RepoId>,
    /// Branch or `detached @ …`.
    pub(super) label: String,
    /// Folder.
    pub(super) path: PathBuf,
    /// The worktree of this tab.
    pub(super) current: bool,
    /// The main worktree.
    pub(super) is_main: bool,
    /// Lock reason.
    pub(super) locked: Option<String>,
    /// Its folder is gone.
    pub(super) missing: bool,
}

/// "Check out B" when B is checked out in another worktree (§7).
pub(super) struct WorktreePrompt {
    branch: String,
    id: Option<RepoId>,
    path: PathBuf,
    /// Its commit, for "check out detached".
    target: Option<Oid>,
}

impl RepoView {
    /// The worktrees of this tab's repo, and the branches the other ones
    /// have checked out (from the app's lists: no Git call).
    pub(super) fn worktree_entries(&self) -> (Vec<WorktreeEntry>, Elsewhere) {
        let here = self.path.id();
        let rows = self.core.all_rows();
        let shown = |path: &PathBuf| {
            let id = archipel_model::RepoPath::new(path.clone()).id();
            rows.iter().any(|r| r.id == id).then_some(id)
        };
        let mut entries = Vec::new();
        let mut elsewhere = Elsewhere::new();
        for w in self.core.worktrees_of(here) {
            let path = w.path.as_path().to_path_buf();
            let current = w.path.id() == here;
            if let (Some(b), false) = (w.branch(), current) {
                elsewhere.insert(b.to_owned(), (shown(&path), path.clone()));
            }
            entries.push(WorktreeEntry {
                id: shown(&path),
                label: w.label(),
                current,
                is_main: w.is_main,
                locked: w.locked.clone(),
                missing: w.is_missing(),
                path,
            });
        }
        (entries, elsewhere)
    }

    /// Shows the prompt instead of checking out `target` when its branch is
    /// checked out in another worktree; `true` when it did.
    pub(super) fn guard_checkout(
        &mut self,
        target: &CheckoutTarget,
        cx: &mut Context<Self>,
    ) -> bool {
        let branch = match target {
            CheckoutTarget::Branch(b) => b.clone(),
            CheckoutTarget::TrackRemote(r) => {
                r.split_once('/').map_or(r.as_str(), |(_, b)| b).to_owned()
            }
            CheckoutTarget::Detached(_) => return false,
        };
        let Some((id, path)) = self.worktree_branches.get(&branch).cloned() else {
            return false;
        };
        let target = self.refs.rows.iter().find_map(|r| match r {
            RefRow::Tip { tip, .. } if tip.kind == RefKind::LocalBranch && tip.name == branch => {
                Some(tip.target)
            }
            _ => None,
        });
        self.worktree_prompt = Some(WorktreePrompt {
            branch,
            id,
            path,
            target,
        });
        cx.notify();
        true
    }

    fn answer_prompt(&mut self, answer: PromptAnswer, cx: &mut Context<Self>) {
        let Some(prompt) = self.worktree_prompt.take() else {
            return;
        };
        match answer {
            PromptAnswer::Open => match prompt.id {
                Some(id) => cx.emit(StagingEvent::OpenRepo(id)),
                None => {
                    self.status = Some(format!(
                        "{} is checked out at {}",
                        prompt.branch,
                        prompt.path.display()
                    ));
                }
            },
            PromptAnswer::Detached => {
                if let Some(oid) = prompt.target {
                    self.checkout(CheckoutTarget::Detached(oid), cx);
                }
            }
            PromptAnswer::Cancel => {}
        }
        cx.notify();
    }

    /// The prompt replacing Git's "already checked out" refusal.
    pub(super) fn render_worktree_prompt(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let prompt = self.worktree_prompt.as_ref()?;
        let detached = prompt.target.map(|oid| {
            button(
                "wtp-detached",
                format!("Check out detached at {}", oid.short(SHORT_OID)),
                cx,
            )
            .on_click(cx.listener(|v, _, _, cx| v.answer_prompt(PromptAnswer::Detached, cx)))
        });
        let dialog = div()
            .id("worktree-prompt")
            .occlude()
            .track_focus(&self.prompt_focus)
            .on_key_down(cx.listener(|v, e: &KeyDownEvent, _, cx| {
                match e.keystroke.key.as_str() {
                    "escape" => v.answer_prompt(PromptAnswer::Cancel, cx),
                    "enter" => v.answer_prompt(PromptAnswer::Open, cx),
                    _ => return,
                }
                cx.stop_propagation();
            }))
            .w(px(480.0))
            .flex()
            .flex_col()
            .gap_3()
            .p_4()
            .bg(color(cx, T::BgElevated))
            .border_1()
            .border_color(color(cx, T::Border))
            .rounded(radius(cx))
            .shadow_lg()
            .child(format!(
                "{} is checked out in another worktree, at {}.",
                prompt.branch,
                prompt.path.display()
            ))
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .child(button("wtp-cancel", "Cancel", cx).on_click(
                        cx.listener(|v, _, _, cx| v.answer_prompt(PromptAnswer::Cancel, cx)),
                    ))
                    .children(detached)
                    .child(
                        primary_button("wtp-open", "Open that worktree", cx).on_click(
                            cx.listener(|v, _, _, cx| v.answer_prompt(PromptAnswer::Open, cx)),
                        ),
                    ),
            );
        Some(modal(dialog, cx))
    }

    /// The Worktree Chip of the toolbar: `⧉ worktree of libfoo` on a linked
    /// worktree, `⧉ 2 worktrees` on a repo with some; a click opens the
    /// Worktree Switcher.
    pub(super) fn render_worktree_chip(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let here = self.path.id();
        let row = self.core.all_rows().into_iter().find(|r| r.id == here)?;
        let label = match (&row.worktree, row.linked_worktrees) {
            (Some(w), _) => format!("{WORKTREE_GLYPH} worktree of {} · {}", w.main_name, w.label),
            (None, 0) => return None,
            (None, 1) => format!("{WORKTREE_GLYPH} 1 worktree"),
            (None, n) => format!("{WORKTREE_GLYPH} {n} worktrees"),
        };
        let hover = color(cx, T::BgHover);
        let path = self.path.as_path().display().to_string();
        Some(
            div()
                .id("worktree-chip")
                .px_2()
                .rounded(radius(cx))
                .border_1()
                .border_color(color(cx, T::Border))
                .text_sm()
                .text_color(color(cx, T::FgMuted))
                .cursor_pointer()
                .hover(move |s| s.bg(hover))
                .tooltip(move |_, cx| {
                    TextTooltip::view(
                        &[path.clone(), "Click: switch worktree (Ctrl+Shift+O)".into()],
                        cx,
                    )
                })
                .on_click(cx.listener(|_, _, _, cx| cx.emit(StagingEvent::SwitchWorktree)))
                .child(label)
                .into_any_element(),
        )
    }
}

/// What the user answered to the "checked out elsewhere" prompt.
#[derive(Clone, Copy)]
enum PromptAnswer {
    Open,
    Detached,
    Cancel,
}

/// One row of the WORKTREES section: double click opens it, right click
/// gives its menu (through the workspace's Worktree Menu).
pub(super) fn render_worktree_row(
    i: usize,
    entry: &WorktreeEntry,
    cx: &mut Context<RepoView>,
) -> AnyElement {
    let hover = color(cx, T::BgHover);
    let id = entry.id;
    let mut tooltip = vec![entry.path.display().to_string()];
    if entry.is_main {
        tooltip.push("Main worktree".into());
    }
    if let Some(reason) = &entry.locked {
        tooltip.push(if reason.is_empty() {
            "Locked".into()
        } else {
            format!("Locked: {reason}")
        });
    }
    if entry.missing {
        tooltip.push("Missing: its folder is gone".into());
    }
    let short = entry
        .path
        .file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    div()
        .id(SharedString::from(format!("ref-{i}")))
        .h(row_height(cx))
        .flex()
        .items_center()
        .gap_1()
        .px_2()
        .pl_4()
        .whitespace_nowrap()
        .overflow_hidden()
        .cursor_pointer()
        .hover(move |s| s.bg(hover))
        .when(entry.current, |d| {
            d.text_color(color(cx, T::AccentText))
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
        })
        .when(entry.missing, |d| d.opacity(0.5))
        .tooltip(move |_, cx| TextTooltip::view(&tooltip, cx))
        .on_click(cx.listener(move |_, e: &ClickEvent, _, cx| {
            if e.click_count() > 1
                && let Some(id) = id
            {
                cx.emit(StagingEvent::OpenRepo(id));
            }
        }))
        .on_mouse_down(
            MouseButton::Right,
            cx.listener(move |_, e: &MouseDownEvent, _, cx| {
                if let Some(id) = id {
                    cx.emit(StagingEvent::WorktreeMenu(id, e.position));
                }
            }),
        )
        .child(
            div()
                .flex_none()
                .text_color(color(cx, T::FgSubtle))
                .child(if entry.current { "●" } else { WORKTREE_GLYPH }),
        )
        .child(div().min_w_0().truncate().child(entry.label.clone()))
        .child(
            div()
                .flex_none()
                .text_xs()
                .text_color(color(cx, T::FgSubtle))
                .child(match (&entry.locked, entry.missing) {
                    (_, true) => "missing".to_owned(),
                    (Some(_), _) => format!("🔒 {short}"),
                    _ => short,
                }),
        )
        .into_any_element()
}

/// The "+" of the WORKTREES header: a new worktree of the repo.
pub(super) fn new_worktree_button(repo: RepoId, cx: &mut Context<RepoView>) -> AnyElement {
    let hover = color(cx, T::BgHover);
    div()
        .id("worktrees-add")
        .ml_auto()
        .px_1()
        .rounded(radius(cx))
        .text_color(color(cx, T::FgMuted))
        .hover(move |s| s.bg(hover))
        .cursor_pointer()
        .tooltip(|_, cx| TextTooltip::view(&["New worktree…".into()], cx))
        .on_click(cx.listener(move |_, _, _, cx| {
            cx.stop_propagation();
            cx.emit(StagingEvent::NewWorktree(repo, WorktreeStart::default()));
        }))
        .child("+")
        .into_any_element()
}
