//! Worktrees in the window (spec §10, WORKTREES.md): their dialogs, the
//! Worktree Menu entries, the Set Selector of the dashboard toolbar, the
//! Worktree Switcher items and the hand-off actions (reveal, copy path,
//! terminal, editor).

use archipel_app::dashboard::WORKTREE_GLYPH;
use archipel_app::{Command, ProjectWorktree};
use archipel_model::RepoId;
use archipel_model::worktree::SetView;
use archipel_theme::ColorToken as T;
use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, ClipboardItem, Context, MouseButton, MouseDownEvent, Window, div};

use crate::palette::PaletteItem;
use crate::theme::color;
use crate::widgets::{check_menu_item, chip, context_menu, list_row, menu_item, menu_separator};
use crate::worktree_dialog::{
    NewWorktreeDialog, RemoveWorktreeDialog, WorktreeDialogEvent, WorktreeStart,
};
use crate::worktree_set_dialog::{FinishSetDialog, NewSetDialog};

use super::Workspace;
use super::menus::Menu;
use super::overlays::{Overlay, PaletteKind};

impl Workspace {
    /// Reacts to what a worktree dialog reports.
    fn on_worktree_dialog(
        &mut self,
        event: &WorktreeDialogEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            WorktreeDialogEvent::Closed => self.close_overlay(window, cx),
            WorktreeDialogEvent::Notice(text) => self.notice = Some(text.clone()),
            WorktreeDialogEvent::Open(id) => {
                self.close_overlay(window, cx);
                self.dispatch(Command::OpenRepoTab(*id), cx);
            }
        }
        cx.notify();
    }

    /// Opens the New Worktree dialog for the repo of row `repo`.
    pub(super) fn open_new_worktree(
        &mut self,
        repo: RepoId,
        start: &WorktreeStart,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let core = self.core.clone();
        let dialog = cx.new(|cx| NewWorktreeDialog::new(core, repo, start, window, cx));
        let sub = cx.subscribe_in(&dialog, window, |this, _, e, window, cx| {
            this.on_worktree_dialog(e, window, cx);
        });
        self.show_overlay(Overlay::NewWorktree(dialog), sub, cx);
    }

    /// Opens the Remove Worktree confirmation for linked worktree row `id`.
    pub(super) fn open_remove_worktree(
        &mut self,
        id: RepoId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let row = self.core.row(id);
        let Some(info) = row.as_ref().and_then(|r| r.worktree.clone()) else {
            self.notice = Some("The main worktree cannot be removed".into());
            cx.notify();
            return;
        };
        if info.parent.is_none() {
            self.notice = Some("Its repository is not in the project: remove it from there".into());
            cx.notify();
            return;
        }
        if info.locked.is_some() {
            self.notice = Some("The worktree is locked: unlock it first".into());
            cx.notify();
            return;
        }
        let core = self.core.clone();
        let dialog = cx.new(|cx| RemoveWorktreeDialog::new(core, id, cx));
        let sub = cx.subscribe_in(&dialog, window, |this, _, e, window, cx| {
            this.on_worktree_dialog(e, window, cx);
        });
        self.show_overlay(Overlay::RemoveWorktree(dialog), sub, cx);
    }

    /// Opens the New Worktree Set dialog on the selected repos (all when
    /// none).
    pub(super) fn open_new_set(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.core.project().is_none() {
            return;
        }
        let core = self.core.clone();
        // Every targeted repo checked: the set's in a set view.
        let selected = self.core.resolve_targets(&self.selection());
        let dialog = cx.new(|cx| NewSetDialog::new(core, selected, window, cx));
        let sub = cx.subscribe_in(&dialog, window, |this, _, e, window, cx| {
            this.on_worktree_dialog(e, window, cx);
        });
        self.show_overlay(Overlay::NewSet(dialog), sub, cx);
    }

    /// Opens the Finish dialog of set `name` (the active set when `None`).
    pub(super) fn open_finish_set(
        &mut self,
        name: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let name = name.or_else(|| self.core.active_set().map(|s| s.name));
        let Some(name) = name else {
            self.notice = Some("Choose a worktree set first (Worktrees ▾ on the dashboard)".into());
            cx.notify();
            return;
        };
        let core = self.core.clone();
        let dialog = cx.new(|cx| FinishSetDialog::new(core, name, cx));
        let sub = cx.subscribe_in(&dialog, window, |this, _, e, window, cx| {
            this.on_worktree_dialog(e, window, cx);
        });
        self.show_overlay(Overlay::FinishSet(dialog), sub, cx);
    }

    /// Sends a set report to the open set dialog, or tells it in the
    /// status bar.
    pub(super) fn on_set_report(
        &mut self,
        report: &archipel_app::SetReport,
        finished: bool,
        cx: &mut Context<Self>,
    ) {
        match &self.overlay {
            Some((Overlay::NewSet(d), _)) if !finished => {
                d.update(cx, |d, cx| d.on_report(report, cx));
            }
            Some((Overlay::FinishSet(d), _)) if finished => {
                d.update(cx, |d, cx| d.on_report(report, cx));
            }
            _ => {
                let failed = report.failed().len();
                self.notice = Some(if failed == 0 {
                    format!(
                        "Worktree set {}: {} worktree(s) in {}",
                        report.name,
                        report.results.len(),
                        report.folder.display()
                    )
                } else {
                    format!("Worktree set {}: {failed} repo(s) failed", report.name)
                });
            }
        }
    }

    /// The worktree row the worktree actions apply to: the active tab's,
    /// else the single selected row.
    pub(super) fn target_worktree(&self) -> Option<RepoId> {
        self.core.action_context().target_repo()
    }

    /// Runs a worktree UI command of the action palette.
    pub(super) fn run_worktree_command(
        &mut self,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let target = self.target_worktree();
        match (id, target) {
            ("palette.worktrees", _) => self.open_palette(PaletteKind::Worktrees, window, cx),
            ("worktree.new_set", _) => self.open_new_set(window, cx),
            ("worktree.finish_set", _) => self.open_finish_set(None, window, cx),
            ("worktree.new", Some(row)) => {
                self.open_new_worktree(row, &WorktreeStart::default(), window, cx);
            }
            ("worktree.remove", Some(row)) => self.open_remove_worktree(row, window, cx),
            ("worktree.lock", Some(row)) => self.lock_or_unlock(row, window, cx),
            (
                action @ ("worktree.reveal" | "worktree.copy_path" | "worktree.terminal"
                | "worktree.editor"),
                Some(row),
            ) => self.hand_off(row, id_action(action), cx),
            _ => self.notice = Some("Select a repository or worktree first".into()),
        }
    }

    /// Locks a worktree (asking the reason) or unlocks it.
    pub(super) fn lock_or_unlock(
        &mut self,
        id: RepoId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let row = self.core.row(id);
        match row.and_then(|r| r.worktree) {
            Some(w) if w.locked.is_some() => self.dispatch(Command::LockWorktree(id, None), cx),
            Some(_) => {
                self.open_palette(PaletteKind::LockReason(id), window, cx);
            }
            None => self.notice = Some("The main worktree cannot be locked".into()),
        }
    }

    /// Reveals, copies, or opens a terminal or editor on row `id`'s folder.
    pub(super) fn hand_off(&mut self, id: RepoId, action: HandOff, cx: &mut Context<Self>) {
        let Some(path) = self.core.row(id).map(|r| r.path.as_path().to_path_buf()) else {
            return;
        };
        let settings = self.core.settings();
        let result = match action {
            HandOff::Reveal => {
                cx.reveal_path(&path);
                Ok(())
            }
            HandOff::CopyPath => {
                cx.write_to_clipboard(ClipboardItem::new_string(path.display().to_string()));
                self.notice = Some(format!("Copied {}", path.display()));
                Ok(())
            }
            HandOff::Terminal => {
                crate::worktree_open::open_terminal(&path, settings.terminal_command.as_deref())
            }
            HandOff::Editor => {
                crate::worktree_open::open_editor(&path, settings.editor_command.as_deref())
            }
        };
        if let Err(e) = result {
            self.notice = Some(e);
        }
        cx.notify();
    }

    /// Items of the Worktree Switcher: every worktree of the project, open
    /// tabs first.
    pub(super) fn worktree_items(&self) -> Vec<PaletteItem> {
        let tabs = self.core.ui_state().map(|u| u.tabs).unwrap_or_default();
        let open =
            |w: &ProjectWorktree| tabs.contains(&archipel_model::ui_state::TabState::Repo(w.id));
        let mut list = self.core.project_worktrees();
        list.sort_by_key(|w| !open(w));
        list.into_iter()
            .map(|w| {
                let glyph = if w.is_main { "  " } else { WORKTREE_GLYPH };
                let mut detail = w.path.as_path().display().to_string();
                if open(&w) {
                    detail = format!("open · {detail}");
                }
                if w.missing {
                    detail = format!("missing · {detail}");
                }
                PaletteItem {
                    id: w.id.0.to_string(),
                    label: format!("{glyph} {} · {}", w.name, w.label),
                    detail,
                }
            })
            .collect()
    }

    /// Worktree entries of a row's right-click menu (§6.5): new worktree on
    /// any repo; open elsewhere, lock, remove, prune on a worktree.
    #[allow(clippy::too_many_lines)] // declarative element tree
    pub(super) fn worktree_menu_items(
        &self,
        id: RepoId,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let row = self.core.row(id);
        let Some(row) = row else { return Vec::new() };
        let info = row.worktree.clone();
        let mut out = vec![menu_separator(cx).into_any_element()];
        let item = |eid: &'static str,
                    label: String,
                    danger: bool,
                    cx: &mut Context<Self>,
                    run: MenuRun| {
            menu_item(eid, label, danger, cx)
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.menu = None;
                    run(this, window, cx);
                    cx.notify();
                }))
                .into_any_element()
        };
        out.push(item(
            "wm-new",
            "New worktree…".into(),
            false,
            cx,
            Box::new(move |this, w, cx| {
                this.open_new_worktree(id, &WorktreeStart::default(), w, cx);
            }),
        ));
        out.push(item(
            "wm-reveal",
            "Reveal folder".into(),
            false,
            cx,
            Box::new(move |this, _, cx| {
                this.hand_off(id, HandOff::Reveal, cx);
            }),
        ));
        out.push(item(
            "wm-copy",
            "Copy path".into(),
            false,
            cx,
            Box::new(move |this, _, cx| {
                this.hand_off(id, HandOff::CopyPath, cx);
            }),
        ));
        out.push(item(
            "wm-term",
            "Open in terminal".into(),
            false,
            cx,
            Box::new(move |this, _, cx| {
                this.hand_off(id, HandOff::Terminal, cx);
            }),
        ));
        out.push(item(
            "wm-editor",
            "Open in external editor".into(),
            false,
            cx,
            Box::new(move |this, _, cx| {
                this.hand_off(id, HandOff::Editor, cx);
            }),
        ));
        if let Some(w) = info.filter(|w| w.parent.is_some()) {
            let lock = if w.locked.is_some() {
                "Unlock"
            } else {
                "Lock…"
            };
            out.push(item(
                "wm-lock",
                lock.into(),
                false,
                cx,
                Box::new(move |this, win, cx| {
                    this.lock_or_unlock(id, win, cx);
                }),
            ));
            if w.missing {
                out.push(item(
                    "wm-prune",
                    "Prune".into(),
                    false,
                    cx,
                    Box::new(move |this, _, cx| {
                        this.dispatch(Command::PruneWorktrees(vec![id]), cx);
                    }),
                ));
            } else {
                out.push(item(
                    "wm-remove",
                    "Remove worktree…".into(),
                    true,
                    cx,
                    Box::new(move |this, win, cx| {
                        this.open_remove_worktree(id, win, cx);
                    }),
                ));
            }
        } else if row.linked_worktrees > 0 {
            out.push(item(
                "wm-prune-all",
                "Prune missing worktrees".into(),
                false,
                cx,
                Box::new(move |this, _, cx| {
                    this.dispatch(Command::PruneWorktrees(vec![id]), cx);
                }),
            ));
        }
        out
    }

    /// The Set Selector chip of the dashboard toolbar (§5.3).
    #[allow(clippy::too_many_lines)] // declarative element tree
    pub(super) fn render_set_selector(
        &mut self,
        ui: &archipel_model::ui_state::UiState,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let label = match &ui.set_view {
            SetView::All => "Worktrees: all ▾".to_owned(),
            SetView::MainOnly => "Worktrees: main only ▾".to_owned(),
            SetView::Set(name) => format!("{WORKTREE_GLYPH} {name} ▾"),
        };
        let active = ui.set_view != SetView::All;
        let button = chip("set-selector", label, active, cx).on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, e: &MouseDownEvent, _, cx| {
                this.toggle_menu(Menu::SetSelector, e, cx);
            }),
        );
        let menu = self.menu_position(&Menu::SetSelector).map(|position| {
            let mut entries: Vec<AnyElement> = Vec::new();
            let pick = |eid: String,
                        label: String,
                        checked: bool,
                        command: Command,
                        cx: &mut Context<Self>| {
                check_menu_item(gpui_kit::ElementId::Name(eid.into()), label, checked, cx)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.menu = None;
                        this.dispatch(command.clone(), cx);
                    }))
                    .into_any_element()
            };
            entries.push(pick(
                "set-all".into(),
                "All (nested under their repo)".into(),
                ui.set_view == SetView::All,
                Command::SetWorktreeView(SetView::All),
                cx,
            ));
            entries.push(pick(
                "set-main".into(),
                "Main worktrees only".into(),
                ui.set_view == SetView::MainOnly,
                Command::SetWorktreeView(SetView::MainOnly),
                cx,
            ));
            if !ui.worktree_sets.is_empty() {
                entries.push(menu_separator(cx).into_any_element());
            }
            for set in &ui.worktree_sets {
                let checked = ui.set_view == SetView::Set(set.name.clone());
                entries.push(pick(
                    format!("set-{}", set.name),
                    format!("{} ({} repos)", set.name, set.members.len()),
                    checked,
                    Command::SetWorktreeView(SetView::Set(set.name.clone())),
                    cx,
                ));
            }
            entries.push(menu_separator(cx).into_any_element());
            if ui.active_set().is_some() {
                entries.push(pick(
                    "set-others".into(),
                    "Show other repos".into(),
                    ui.set_show_others,
                    Command::ToggleSetOthers,
                    cx,
                ));
            }
            let action =
                |eid: &'static str,
                 label: &'static str,
                 danger: bool,
                 cx: &mut Context<Self>,
                 run: fn(&mut Self, &mut Window, &mut Context<Self>)| {
                    menu_item(eid, label, danger, cx)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.menu = None;
                            run(this, window, cx);
                            cx.notify();
                        }))
                        .into_any_element()
                };
            entries.push(action("set-new", "New set…", false, cx, |this, w, cx| {
                this.open_new_set(w, cx);
            }));
            if ui.active_set().is_some() {
                entries.push(action(
                    "set-rename",
                    "Rename set…",
                    false,
                    cx,
                    |this, w, cx| {
                        this.open_palette(PaletteKind::RenameSet, w, cx);
                    },
                ));
                entries.push(action(
                    "set-finish",
                    "Finish set…",
                    false,
                    cx,
                    |this, w, cx| this.open_finish_set(None, w, cx),
                ));
                entries.push(action(
                    "set-forget",
                    "Forget set (keep its worktrees)",
                    true,
                    cx,
                    |this, _, cx| {
                        if let Some(set) = this.core.active_set() {
                            this.dispatch(Command::ForgetWorktreeSet(set.name), cx);
                        }
                    },
                ));
            }
            let menu = context_menu(cx).children(entries);
            Self::menu_popup(position, menu, cx)
        });
        div().child(button).children(menu).into_any_element()
    }

    /// Linked worktree rows of the sidebar under repo `id` (§4.1).
    pub(super) fn sidebar_worktree_rows(
        &self,
        id: RepoId,
        selected: &[RepoId],
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        self.core
            .rows()
            .into_iter()
            .filter(|r| r.parent() == Some(id))
            .map(|row| {
                let wid = row.id;
                let info = row.worktree.clone().unwrap_or_else(unreachable_info);
                let (token, extra) = super::sidebar::side_status(&row.status);
                list_row(
                    gpui_kit::ElementId::Name(format!("side-wt-{wid}").into()),
                    selected.contains(&wid),
                    cx,
                )
                .flex()
                .items_center()
                .gap_2()
                .pl_6()
                .pr_2()
                .h(crate::theme::row_height(cx))
                .rounded(crate::theme::radius(cx))
                .when(info.missing, |d| d.opacity(0.5))
                .on_click(cx.listener(move |this, e: &gpui_kit::ClickEvent, _, cx| {
                    let cmd = super::dashboard::row_click(wid, e);
                    let plain = matches!(cmd, Command::Select(_));
                    this.dispatch(cmd, cx);
                    if plain && e.click_count() > 1 {
                        this.dispatch(Command::OpenRepoTab(wid), cx);
                    }
                }))
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                        this.open_menu(Menu::Row(wid), e.position, cx);
                    }),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(color(cx, T::FgSubtle))
                        .child(WORKTREE_GLYPH),
                )
                .child(crate::widgets::dot(if token == T::StatusSuccess {
                    crate::theme::clean_dot()
                } else {
                    color(cx, token)
                }))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .child(info.label.clone()),
                )
                .child(
                    div()
                        .flex_none()
                        .text_xs()
                        .text_color(color(cx, T::FgMuted))
                        .child(if info.missing {
                            "missing".to_owned()
                        } else if info.locked.is_some() {
                            format!("🔒 {extra}")
                        } else {
                            extra
                        }),
                )
                .into_any_element()
            })
            .collect()
    }
}

/// A fallback never reached: rows filtered on their parent are worktrees.
fn unreachable_info() -> archipel_app::dashboard::WorktreeRowInfo {
    archipel_app::dashboard::WorktreeRowInfo {
        parent: None,
        main_name: String::new(),
        label: String::new(),
        locked: None,
        missing: false,
        listed: false,
    }
}

/// What a Worktree Menu entry runs.
type MenuRun = Box<dyn Fn(&mut Workspace, &mut Window, &mut Context<Workspace>)>;

/// Where a worktree's folder is handed off to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum HandOff {
    /// The file manager.
    Reveal,
    /// The clipboard.
    CopyPath,
    /// A terminal.
    Terminal,
    /// The external editor.
    Editor,
}

/// The hand-off of a `worktree.*` action id.
fn id_action(id: &str) -> HandOff {
    match id {
        "worktree.reveal" => HandOff::Reveal,
        "worktree.terminal" => HandOff::Terminal,
        "worktree.editor" => HandOff::Editor,
        _ => HandOff::CopyPath,
    }
}
