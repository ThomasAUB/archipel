//! The New Worktree Set and Finish Worktree Set dialogs (WORKTREES.md §5.1,
//! §5.4): one branch checked out as a worktree in several repos under one
//! folder, for an agent run across repos; then removed with what it would
//! lose said per repo.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use archipel_app::{
    App as Core, Command, NewSetRequest, SetMemberPlan, SetMemberState, SetRepoInfo, SetReport,
};
use archipel_model::RepoId;
use archipel_ops::RemoveOptions;
use archipel_theme::ColorToken as T;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, ClipboardItem, Context, Entity, EventEmitter, KeyDownEvent, SharedString,
    Subscription, Task, Window, div, px,
};

use crate::theme::color;
use crate::widgets::{button, primary_button, section};
use crate::worktree_dialog::{WorktreeDialogEvent, check, dialog_frame, field};

/// Wait after the last keystroke before checking the branch in every repo.
const CHECK_DELAY: Duration = Duration::from_millis(350);

/// A repo of the New Worktree Set dialog.
struct SetRow {
    info: SetRepoInfo,
    checked: bool,
    base: Entity<InputState>,
}

/// The New Worktree Set dialog.
#[allow(clippy::struct_excessive_bools)] // the form's checkboxes and state
pub struct NewSetDialog {
    core: Arc<Core>,
    name: Entity<InputState>,
    folder: Entity<InputState>,
    folder_edited: bool,
    setting_folder: bool,
    /// Repos pre-checked: the selection, or every repo when none.
    preselected: Vec<RepoId>,
    rows: Vec<SetRow>,
    init_submodules: bool,
    copy_files: bool,
    busy: bool,
    report: Option<SetReport>,
    error: Option<String>,
    check: Option<Task<()>>,
    _subs: Vec<Subscription>,
}

impl EventEmitter<WorktreeDialogEvent> for NewSetDialog {}

impl NewSetDialog {
    /// The dialog, with `preselected` repos checked (every repo when empty).
    pub fn new(
        core: Arc<Core>,
        preselected: Vec<RepoId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let name = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Name, also the branch (agent/parser-rework)")
        });
        let folder = cx.new(|cx| InputState::new(window, cx).placeholder("Set Folder"));
        name.update(cx, |n, cx| n.focus(window, cx));
        let subs = vec![
            cx.subscribe_in(
                &name,
                window,
                |this: &mut Self, _, e: &InputEvent, window, cx| {
                    if matches!(e, InputEvent::Change) {
                        this.sync_folder(window, cx);
                        this.schedule_check(window, cx);
                    }
                },
            ),
            cx.subscribe_in(
                &folder,
                window,
                |this: &mut Self, _, e: &InputEvent, _, _| {
                    if matches!(e, InputEvent::Change) && !this.setting_folder {
                        this.folder_edited = true;
                    }
                },
            ),
        ];
        let preselected = preselected
            .into_iter()
            .map(|id| core.main_repo_of(id))
            .collect();
        let mut dialog = Self {
            core,
            name,
            folder,
            folder_edited: false,
            setting_folder: false,
            preselected,
            rows: Vec::new(),
            init_submodules: false,
            copy_files: true,
            busy: false,
            report: None,
            error: None,
            check: None,
            _subs: subs,
        };
        dialog.sync_folder(window, cx);
        dialog.schedule_check(window, cx);
        dialog
    }

    fn branch(&self, cx: &gpui_kit::App) -> String {
        self.name.read(cx).value().trim().to_owned()
    }

    fn sync_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.folder_edited {
            return;
        }
        let name = self.branch(cx);
        let name = if name.is_empty() { "set" } else { &name };
        let Some(folder) = self.core.default_set_folder(name) else {
            return;
        };
        self.setting_folder = true;
        self.folder.update(cx, |f, cx| {
            f.set_value(folder.display().to_string(), window, cx);
        });
        self.setting_folder = false;
    }

    /// Reads how every repo stands for the typed branch, once typing stops.
    fn schedule_check(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let branch = self.branch(cx);
        let core = self.core.clone();
        self.check = Some(cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(CHECK_DELAY).await;
            let infos = cx
                .background_spawn(async move { core.set_repo_infos(&branch) })
                .await;
            let _ = this.update_in(cx, |d, window, cx| d.apply_infos(infos, window, cx));
        }));
    }

    fn apply_infos(
        &mut self,
        infos: Vec<SetRepoInfo>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let first = self.rows.is_empty();
        let mut rows = Vec::with_capacity(infos.len());
        for info in infos {
            let old = self.rows.iter().position(|r| r.info.repo == info.repo);
            let row = if let Some(i) = old {
                let old = self.rows.swap_remove(i);
                SetRow {
                    checked: old.checked && info.checked_out_in.is_none(),
                    base: old.base,
                    info,
                }
            } else {
                let base_text = info.default_base.clone().unwrap_or_default();
                let base = cx.new(|cx| {
                    InputState::new(window, cx)
                        .placeholder("base")
                        .default_value(base_text)
                });
                let wanted = self.preselected.is_empty() || self.preselected.contains(&info.repo);
                SetRow {
                    checked: wanted && info.checked_out_in.is_none(),
                    base,
                    info,
                }
            };
            rows.push(row);
        }
        if first {
            self.init_submodules = rows.iter().any(|r| r.checked && r.info.has_submodules);
        }
        self.rows = rows;
        cx.notify();
    }

    fn create(&mut self, cx: &mut Context<Self>, only: Option<&[RepoId]>) {
        let branch = self.branch(cx);
        if branch.is_empty() {
            self.error = Some("Name the set: the name is also its branch".into());
            cx.notify();
            return;
        }
        let folder = PathBuf::from(self.folder.read(cx).value().trim());
        if folder.as_os_str().is_empty() {
            self.error = Some("Choose the Set Folder".into());
            cx.notify();
            return;
        }
        let members: Vec<SetMemberPlan> = self
            .rows
            .iter()
            .filter(|r| r.checked && r.info.checked_out_in.is_none())
            .filter(|r| only.is_none_or(|o| o.contains(&r.info.repo)))
            .map(|r| SetMemberPlan {
                repo: r.info.repo,
                base: if r.info.branch_exists {
                    None
                } else {
                    let base = r.base.read(cx).value().trim().to_owned();
                    Some(base).filter(|b| !b.is_empty())
                },
            })
            .collect();
        if members.is_empty() {
            self.error = Some("Check at least one repository".into());
            cx.notify();
            return;
        }
        let request = NewSetRequest {
            name: branch.clone(),
            branch,
            folder,
            members,
            init_submodules: self.init_submodules,
            copy_files: self.copy_files,
        };
        match self.core.dispatch(Command::CreateWorktreeSet(request)) {
            Ok(()) => {
                self.busy = true;
                self.error = None;
                self.report = None;
            }
            Err(e) => self.error = Some(e.to_string()),
        }
        cx.notify();
    }

    /// The per-repo report of the creation (sent on by the workspace).
    pub fn on_report(&mut self, report: &SetReport, cx: &mut Context<Self>) {
        if report.name != self.branch(cx) {
            return;
        }
        self.busy = false;
        self.report = Some(report.clone());
        cx.notify();
    }

    fn render_rows(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        self.rows
            .iter()
            .enumerate()
            .map(|(i, r)| {
                let blocked = r.info.checked_out_in.clone();
                let result = if let Some(path) = &blocked {
                    format!("checked out at {}", path.as_path().display())
                } else if r.info.branch_exists {
                    "branch exists → use it".to_owned()
                } else {
                    "new branch".to_owned()
                };
                let tone = if blocked.is_some() {
                    T::StatusWarning
                } else {
                    T::FgMuted
                };
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .py(px(2.0))
                    .child(
                        check(
                            SharedString::from(format!("set-repo-{i}")),
                            r.info.name.clone(),
                            r.checked,
                            blocked.is_none(),
                            cx,
                        )
                        .w(px(170.0))
                        .flex_none()
                        .on_click(cx.listener(move |d, _, _, cx| {
                            if let Some(row) = d.rows.get_mut(i)
                                && row.info.checked_out_in.is_none()
                            {
                                row.checked = !row.checked;
                            }
                            cx.notify();
                        })),
                    )
                    .child(
                        div()
                            .w(px(150.0))
                            .flex_none()
                            .when(!r.info.branch_exists && blocked.is_none(), |d| {
                                d.child(Input::new(&r.base))
                            }),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .text_color(color(cx, tone))
                            .child(result),
                    )
                    .into_any_element()
            })
            .collect()
    }

    #[allow(clippy::unused_self)] // a view's render helper, like the others
    fn render_report(&self, report: &SetReport, cx: &mut Context<Self>) -> AnyElement {
        let failed = report.failed();
        let folder = report.folder.clone();
        let lines = report.results.iter().map(|(_, name, r)| {
            let (text, tone) = match r {
                Ok(()) => (format!("{name}: created"), T::StatusSuccess),
                Err(e) => (format!("{name}: {e}"), T::StatusError),
            };
            div().text_xs().text_color(color(cx, tone)).child(text)
        });
        let (copy, reveal) = (folder.clone(), folder);
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(section("RESULT", cx))
            .children(lines)
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        button("set-copy", "Copy folder path", cx).on_click(cx.listener(
                            move |_, _, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(
                                    copy.display().to_string(),
                                ));
                                cx.emit(WorktreeDialogEvent::Notice(
                                    "Set folder path copied".into(),
                                ));
                            },
                        )),
                    )
                    .child(
                        button("set-reveal", "Reveal folder", cx)
                            .on_click(cx.listener(move |_, _, _, cx| cx.reveal_path(&reveal))),
                    )
                    .when(!failed.is_empty(), |d| {
                        d.child(
                            primary_button("set-retry", "Retry failed", cx).on_click(
                                cx.listener(move |d, _, _, cx| d.create(cx, Some(&failed))),
                            ),
                        )
                    })
                    .child(
                        button("set-done", "Close", cx).on_click(
                            cx.listener(|_, _, _, cx| cx.emit(WorktreeDialogEvent::Closed)),
                        ),
                    ),
            )
            .into_any_element()
    }
}

impl Render for NewSetDialog {
    #[allow(clippy::too_many_lines)] // declarative element tree
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let count = self.rows.iter().filter(|r| r.checked).count();
        let rows = self.render_rows(cx);
        let report = self.report.clone().map(|r| self.render_report(&r, cx));
        let any_submodules = self.rows.iter().any(|r| r.info.has_submodules);
        dialog_frame("new-set", "New worktree set", cx)
            .w(px(680.0))
            .on_key_down(cx.listener(|_, e: &KeyDownEvent, _, cx| {
                if e.keystroke.key == "escape" {
                    cx.stop_propagation();
                    cx.emit(WorktreeDialogEvent::Closed);
                }
            }))
            .child(field("Name / branch", Input::new(&self.name), cx))
            .child(field("Folder", Input::new(&self.folder), cx))
            .child(section("REPOS · BASE · RESULT", cx))
            .child(
                div()
                    .id("set-rows")
                    .max_h(px(280.0))
                    .overflow_y_scrollbar()
                    .children(rows),
            )
            .child(
                div()
                    .flex()
                    .gap_4()
                    .when(any_submodules, |d| {
                        d.child(
                            check("set-sub", "Init submodules", self.init_submodules, true, cx)
                                .on_click(cx.listener(|d, _, _, cx| {
                                    d.init_submodules = !d.init_submodules;
                                    cx.notify();
                                })),
                        )
                    })
                    .child(
                        check(
                            "set-copy-files",
                            "Copy local files (.env, …)",
                            self.copy_files,
                            true,
                            cx,
                        )
                        .on_click(cx.listener(|d, _, _, cx| {
                            d.copy_files = !d.copy_files;
                            cx.notify();
                        })),
                    ),
            )
            .children(
                self.error
                    .clone()
                    .map(|e| div().text_color(color(cx, T::StatusError)).child(e)),
            )
            .when(self.busy, |d| {
                d.child(
                    div()
                        .text_color(color(cx, T::FgMuted))
                        .child("Creating the worktrees… (see the operations drawer)"),
                )
            })
            .children(report)
            .when(self.report.is_none(), |d| {
                d.child(
                    div()
                        .flex()
                        .justify_end()
                        .gap_2()
                        .child(button("set-cancel", "Cancel", cx).on_click(
                            cx.listener(|_, _, _, cx| cx.emit(WorktreeDialogEvent::Closed)),
                        ))
                        .child(
                            primary_button("set-create", format!("Create {count} worktree(s)"), cx)
                                .when(self.busy, |b| b.opacity(0.5))
                                .on_click(cx.listener(|d, _, _, cx| {
                                    if !d.busy {
                                        d.create(cx, None);
                                    }
                                })),
                        ),
                )
            })
    }
}

/// A member of the Finish dialog.
struct FinishRow {
    state: SetMemberState,
    remove: bool,
    delete_branch: bool,
}

/// The Finish Worktree Set dialog: per member what removing it would lose,
/// safe ones checked.
pub struct FinishSetDialog {
    core: Arc<Core>,
    name: String,
    rows: Option<Vec<FinishRow>>,
    busy: bool,
    report: Option<SetReport>,
}

impl EventEmitter<WorktreeDialogEvent> for FinishSetDialog {}

impl FinishSetDialog {
    /// The dialog for set `name`.
    pub fn new(core: Arc<Core>, name: String, cx: &mut Context<Self>) -> Self {
        let (c, n) = (core.clone(), name.clone());
        let task = cx.background_spawn(async move { c.set_member_states(&n) });
        cx.spawn(async move |this, cx| {
            let states = task.await;
            let _ = this.update(cx, |d, cx| {
                d.rows = Some(
                    states
                        .into_iter()
                        .map(|state| {
                            let (safe, branch_safe) =
                                state.loss.as_ref().map_or((false, false), |l| {
                                    (l.is_safe(), l.branch.is_some() && l.branch_safe_to_delete)
                                });
                            FinishRow {
                                remove: safe && state.row.is_some(),
                                delete_branch: safe && branch_safe,
                                state,
                            }
                        })
                        .collect(),
                );
                cx.notify();
            });
        })
        .detach();
        Self {
            core,
            name,
            rows: None,
            busy: false,
            report: None,
        }
    }

    fn finish(&mut self, cx: &mut Context<Self>) {
        let Some(rows) = &self.rows else { return };
        let removals: Vec<(RepoId, RemoveOptions)> = rows
            .iter()
            .filter(|r| r.remove)
            .filter_map(|r| {
                let loss = r.state.loss.as_ref().ok()?;
                Some((
                    r.state.row?,
                    RemoveOptions {
                        force: !loss.is_safe(),
                        delete_branch: r.delete_branch,
                        batch: None,
                    },
                ))
            })
            .collect();
        if removals.is_empty() {
            cx.emit(WorktreeDialogEvent::Notice(
                "Nothing checked to remove".into(),
            ));
            return;
        }
        let command = Command::FinishWorktreeSet {
            name: self.name.clone(),
            removals,
        };
        match self.core.dispatch(command) {
            Ok(()) => self.busy = true,
            Err(e) => cx.emit(WorktreeDialogEvent::Notice(e.to_string())),
        }
        cx.notify();
    }

    /// The per-repo report (sent on by the workspace).
    pub fn on_report(&mut self, report: &SetReport, cx: &mut Context<Self>) {
        if report.name != self.name {
            return;
        }
        self.busy = false;
        if report.failed().is_empty() {
            cx.emit(WorktreeDialogEvent::Notice(format!(
                "Finished worktree set {} (Ctrl+Z on each repo brings its worktree back)",
                report.name
            )));
            cx.emit(WorktreeDialogEvent::Closed);
        } else {
            self.report = Some(report.clone());
        }
        cx.notify();
    }
}

impl Render for FinishSetDialog {
    #[allow(clippy::too_many_lines)] // declarative element tree
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut body: Vec<AnyElement> = Vec::new();
        match &self.rows {
            None => body.push(
                div()
                    .text_color(color(cx, T::FgMuted))
                    .child("Checking each worktree…")
                    .into_any_element(),
            ),
            Some(rows) => {
                for (i, r) in rows.iter().enumerate() {
                    let (status, tone, checkable) = match &r.state.loss {
                        Err(e) => (e.clone(), T::FgMuted, false),
                        Ok(l) if l.in_progress.is_some() => {
                            (l.lines().join(", "), T::StatusConflict, false)
                        }
                        Ok(l) if l.is_safe() => ("safe".to_owned(), T::StatusSuccess, true),
                        Ok(l) => (l.lines().join(", "), T::StatusWarning, true),
                    };
                    let branch = r.state.loss.as_ref().ok().and_then(|l| l.branch.clone());
                    body.push(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                check(
                                    SharedString::from(format!("fin-{i}")),
                                    r.state.name.clone(),
                                    r.remove,
                                    checkable,
                                    cx,
                                )
                                .w(px(170.0))
                                .flex_none()
                                .on_click(cx.listener(
                                    move |d, _, _, cx| {
                                        if checkable
                                            && let Some(row) =
                                                d.rows.as_mut().and_then(|r| r.get_mut(i))
                                        {
                                            row.remove = !row.remove;
                                        }
                                        cx.notify();
                                    },
                                )),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_xs()
                                    .text_color(color(cx, tone))
                                    .child(status),
                            )
                            .when_some(branch, |d, b| {
                                d.child(
                                    check(
                                        SharedString::from(format!("fin-br-{i}")),
                                        format!("delete {b}"),
                                        r.delete_branch,
                                        r.remove,
                                        cx,
                                    )
                                    .text_xs()
                                    .on_click(cx.listener(
                                        move |d, _, _, cx| {
                                            if let Some(row) =
                                                d.rows.as_mut().and_then(|r| r.get_mut(i))
                                            {
                                                row.delete_branch = !row.delete_branch;
                                            }
                                            cx.notify();
                                        },
                                    )),
                                )
                            })
                            .into_any_element(),
                    );
                }
            }
        }
        let report = self.report.as_ref().map(|r| {
            div()
                .flex()
                .flex_col()
                .children(r.results.iter().filter_map(|(_, n, res)| {
                    res.as_ref().err().map(|e| {
                        div()
                            .text_xs()
                            .text_color(color(cx, T::StatusError))
                            .child(format!("{n}: {e}"))
                    })
                }))
        });
        dialog_frame(
            "finish-set",
            format!("Finish worktree set {}", self.name),
            cx,
        )
        .on_key_down(cx.listener(|_, e: &KeyDownEvent, _, cx| {
            if e.keystroke.key == "escape" {
                cx.stop_propagation();
                cx.emit(WorktreeDialogEvent::Closed);
            }
        }))
        .child(div().text_xs().text_color(color(cx, T::FgMuted)).child(
            "Checked worktrees are removed (undoable per repo), then the set folder when empty.",
        ))
        .children(body)
        .children(report)
        .child(
            div()
                .flex()
                .justify_end()
                .gap_2()
                .child(
                    button("fin-cancel", "Cancel", cx)
                        .on_click(cx.listener(|_, _, _, cx| cx.emit(WorktreeDialogEvent::Closed))),
                )
                .child(
                    primary_button(
                        "fin-ok",
                        if self.busy { "Finishing…" } else { "Finish" },
                        cx,
                    )
                    .on_click(cx.listener(|d, _, _, cx| {
                        if !d.busy {
                            d.finish(cx);
                        }
                    })),
                ),
        )
    }
}
