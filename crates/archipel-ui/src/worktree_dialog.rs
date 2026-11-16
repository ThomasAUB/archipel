//! The New Worktree dialog and the Remove Worktree confirmation
//! (WORKTREES.md §6.2, §6.4).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use archipel_app::{App as Core, Command};
use archipel_model::refs::BranchInfo;
use archipel_model::requests::SequenceAction;
use archipel_model::worktree::{WorktreeAddRequest, WorktreeTarget};
use archipel_model::{RepoId, RepoPath};
use archipel_ops::{OpClass, WorktreeLoss};
use archipel_theme::ColorToken as T;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, Context, Entity, EventEmitter, KeyDownEvent, Subscription, Window, div, px,
};

use crate::armed::Armed;
use crate::theme::color;
use crate::widgets::{
    ClickIf as _, FIELD_LABEL_WIDTH, button, check, dialog_frame, field, primary_button,
};

/// What a worktree dialog reports to the workspace.
pub enum WorktreeDialogEvent {
    /// Closed (cancelled or done).
    Closed,
    /// A line for the status bar.
    Notice(String),
    /// Open this worktree's tab instead (it is where the branch is).
    Open(RepoId),
}

/// What the New Worktree dialog starts from (its entry point, §6.1).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WorktreeStart {
    /// Branch to check out or create (`origin/x` checks out `x`).
    pub branch: Option<String>,
    /// Base of a new branch, or the commit of a detached one.
    pub base: Option<String>,
    /// Detached at `base`.
    pub detached: bool,
}

/// The New Worktree dialog.
#[allow(clippy::struct_excessive_bools)] // the form's checkboxes
pub struct NewWorktreeDialog {
    core: Arc<Core>,
    /// The repo row (main worktree).
    repo: RepoId,
    repo_name: String,
    branch: Entity<InputState>,
    base: Entity<InputState>,
    folder: Entity<InputState>,
    /// The folder was typed: the branch no longer drives it.
    folder_edited: bool,
    /// Ignore the next folder change (we set it).
    setting_folder: bool,
    detached: bool,
    init_submodules: bool,
    copy_files: bool,
    has_copy_patterns: bool,
    open_tab: bool,
    branches: Vec<BranchInfo>,
    error: Option<String>,
    _subs: Vec<Subscription>,
}

impl EventEmitter<WorktreeDialogEvent> for NewWorktreeDialog {}

impl NewWorktreeDialog {
    /// The dialog for the repo of row `repo`, prefilled from `start`.
    pub fn new(
        core: Arc<Core>,
        repo: RepoId,
        start: &WorktreeStart,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let main = core.main_repo_of(repo);
        let row = core.row(main);
        let repo_name = row.as_ref().map(|r| r.name.clone()).unwrap_or_default();
        let has_submodules = row
            .as_ref()
            .and_then(|r| r.summary().map(|s| s.has_submodules))
            .unwrap_or(false);
        let branch_text = start
            .branch
            .clone()
            .map(|b| local_name(&b).to_owned())
            .unwrap_or_default();
        let branch = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Branch: an existing one, or a new name")
                .default_value(branch_text.clone())
        });
        let base = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Base of a new branch (HEAD when empty)")
                .default_value(start.base.clone().unwrap_or_default())
        });
        let folder = cx.new(|cx| InputState::new(window, cx).placeholder("Folder"));
        branch.update(cx, |b, cx| b.focus(window, cx));
        let subs = vec![
            cx.subscribe_in(
                &branch,
                window,
                |this: &mut Self, _, e: &InputEvent, window, cx| match e {
                    InputEvent::Change => this.sync_folder(window, cx),
                    InputEvent::PressEnter { .. } => this.create(window, cx),
                    _ => {}
                },
            ),
            cx.subscribe_in(
                &folder,
                window,
                |this: &mut Self, _, e: &InputEvent, window, cx| match e {
                    InputEvent::Change if !this.setting_folder => this.folder_edited = true,
                    InputEvent::PressEnter { .. } => this.create(window, cx),
                    _ => {}
                },
            ),
        ];
        let has_copy_patterns = !core.copy_patterns(main).is_empty();
        let mut dialog = Self {
            core,
            repo: main,
            repo_name,
            branch,
            base,
            folder,
            folder_edited: false,
            setting_folder: false,
            detached: start.detached,
            init_submodules: has_submodules,
            copy_files: has_copy_patterns,
            has_copy_patterns,
            open_tab: true,
            branches: Vec::new(),
            error: None,
            _subs: subs,
        };
        dialog.sync_folder(window, cx);
        dialog.load_branches(cx);
        dialog
    }

    fn load_branches(&self, cx: &mut Context<Self>) {
        let Some(path) = self.path() else { return };
        let refs = self.core.repo_actions().ports().refs.clone();
        let task = cx.background_spawn(async move { refs.branches(&path) });
        cx.spawn(async move |this, cx| {
            if let Ok(branches) = task.await {
                let _ = this.update(cx, |d, cx| {
                    d.branches = branches;
                    cx.notify();
                });
            }
        })
        .detach();
    }

    fn path(&self) -> Option<RepoPath> {
        self.core.row(self.repo).map(|r| r.path)
    }

    /// The folder follows the branch until typed.
    fn sync_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.folder_edited {
            return;
        }
        let branch = self.branch.read(cx).value().trim().to_owned();
        let name = if branch.is_empty() {
            if self.detached {
                "detached"
            } else {
                "worktree"
            }
            .to_owned()
        } else {
            local_name(&branch).to_owned()
        };
        let Some(folder) = self.core.default_worktree_folder(self.repo, &name) else {
            return;
        };
        self.setting_folder = true;
        self.folder.update(cx, |f, cx| {
            f.set_value(folder.display().to_string(), window, cx);
        });
        self.setting_folder = false;
        cx.notify();
    }

    /// What the typed branch means: an existing local branch, a remote one
    /// to track, or a new one.
    fn target(&self, cx: &gpui_kit::App) -> Result<WorktreeTarget, String> {
        let branch = self.branch.read(cx).value().trim().to_owned();
        let base = self.base.read(cx).value().trim().to_owned();
        let base = (!base.is_empty()).then_some(base);
        if self.detached {
            return Ok(WorktreeTarget::Detached(
                base.or((!branch.is_empty()).then_some(branch))
                    .unwrap_or_else(|| "HEAD".to_owned()),
            ));
        }
        if branch.is_empty() {
            return Err("Name the branch (or check “Detached”)".into());
        }
        if self
            .branches
            .iter()
            .any(|b| !b.is_remote && b.name == branch)
        {
            return Ok(WorktreeTarget::Branch(branch));
        }
        if let Some(remote) = self
            .branches
            .iter()
            .find(|b| b.is_remote && (b.name == branch || local_name(&b.name) == branch))
        {
            let local = local_name(&remote.name);
            if self
                .branches
                .iter()
                .any(|b| !b.is_remote && b.name == local)
            {
                return Ok(WorktreeTarget::Branch(local.to_owned()));
            }
            return Ok(WorktreeTarget::TrackRemote(remote.name.clone()));
        }
        Ok(WorktreeTarget::NewBranch { name: branch, base })
    }

    fn create(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let target = match self.target(cx) {
            Ok(t) => t,
            Err(e) => {
                self.error = Some(e);
                cx.notify();
                return;
            }
        };
        let folder = PathBuf::from(self.folder.read(cx).value().trim());
        if folder.as_os_str().is_empty() {
            self.error = Some("Choose a folder".into());
            cx.notify();
            return;
        }
        let worktrees = self.core.worktrees_of(self.repo);
        if let Some(b) = target.branch()
            && let Some(w) = worktrees.iter().find(|w| w.branch() == Some(b))
        {
            let id = w.path.id();
            if self.core.all_rows().iter().any(|r| r.id == id) {
                cx.emit(WorktreeDialogEvent::Open(id));
                cx.emit(WorktreeDialogEvent::Notice(format!(
                    "{b} is already checked out at {}: opened it",
                    w.path.as_path().display()
                )));
                return;
            }
            self.error = Some(format!(
                "{b} is already checked out at {}",
                w.path.as_path().display()
            ));
            cx.notify();
            return;
        }
        if worktrees
            .iter()
            .any(|w| folder.starts_with(w.path.as_path()))
        {
            self.error = Some("The folder is inside another worktree of this repo".into());
            cx.notify();
            return;
        }
        if folder_not_empty(&folder) {
            self.error = Some(format!("{} exists and is not empty", folder.display()));
            cx.notify();
            return;
        }
        let command = Command::AddWorktree {
            repo: self.repo,
            request: WorktreeAddRequest {
                path: folder,
                target,
            },
            init_submodules: self.init_submodules,
            copy_files: self.copy_files,
            open: self.open_tab,
        };
        if let Err(e) = self.core.dispatch(command) {
            self.error = Some(e.to_string());
            cx.notify();
            return;
        }
        cx.emit(WorktreeDialogEvent::Closed);
    }

    #[allow(clippy::unused_self)] // called like every other view action
    fn pick_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(gpui_kit::PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: None,
        });
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(paths))) = rx.await
                && let Some(path) = paths.into_iter().next()
            {
                let _ = this.update_in(cx, |d, window, cx| {
                    d.folder_edited = true;
                    d.folder.update(cx, |f, cx| {
                        f.set_value(path.display().to_string(), window, cx);
                    });
                    cx.notify();
                });
            }
        })
        .detach();
    }
}

/// `origin/feat` → `feat`; a local name unchanged.
fn local_name(branch: &str) -> &str {
    branch
        .strip_prefix("origin/")
        .or_else(|| branch.strip_prefix("upstream/"))
        .unwrap_or(branch)
}

/// The folder exists with something in it.
fn folder_not_empty(folder: &Path) -> bool {
    std::fs::read_dir(folder).is_ok_and(|mut d| d.next().is_some()) || folder.is_file()
}

impl Render for NewWorktreeDialog {
    #[allow(clippy::too_many_lines)] // declarative element tree
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let target = self.target(cx);
        let what = match &target {
            Ok(WorktreeTarget::Branch(b)) => format!("checks out the existing branch {b}"),
            Ok(WorktreeTarget::TrackRemote(r)) => {
                format!("creates {} tracking {r}", local_name(r))
            }
            Ok(WorktreeTarget::NewBranch { name, base }) => format!(
                "creates the branch {name} from {}",
                base.as_deref().unwrap_or("HEAD")
            ),
            Ok(WorktreeTarget::Detached(rev)) => format!("detached at {rev}"),
            Err(_) => String::new(),
        };
        let new_branch = matches!(target, Ok(WorktreeTarget::NewBranch { .. }));
        dialog_frame(
            "new-worktree",
            format!("New worktree of {}", self.repo_name),
            cx,
        )
        .on_key_down(cx.listener(|_, e: &KeyDownEvent, _, cx| {
            if e.keystroke.key == "escape" {
                cx.stop_propagation();
                cx.emit(WorktreeDialogEvent::Closed);
            }
        }))
        .child(field("Branch", Input::new(&self.branch), cx))
        .when(new_branch || self.detached, |d| {
            d.child(field(
                if self.detached { "Commit" } else { "Base" },
                Input::new(&self.base),
                cx,
            ))
        })
        .child(
            div()
                .pl(px(FIELD_LABEL_WIDTH + 8.0))
                .text_xs()
                .text_color(color(cx, T::FgMuted))
                .child(what),
        )
        .child(field(
            "Folder",
            div()
                .flex()
                .gap_2()
                .child(div().flex_1().min_w_0().child(Input::new(&self.folder)))
                .child(
                    button("wt-pick", "…", cx)
                        .on_click(cx.listener(|d, _, window, cx| d.pick_folder(window, cx))),
                ),
            cx,
        ))
        .child(
            div()
                .flex()
                .flex_wrap()
                .gap_4()
                .child(
                    check("wt-detached", "Detached", self.detached, cx).on_click(cx.listener(
                        |d, _, window, cx| {
                            d.detached = !d.detached;
                            d.sync_folder(window, cx);
                        },
                    )),
                )
                .child(
                    check("wt-sub", "Init submodules", self.init_submodules, cx).on_click(
                        cx.listener(|d, _, _, cx| {
                            d.init_submodules = !d.init_submodules;
                            cx.notify();
                        }),
                    ),
                )
                .when(self.has_copy_patterns, |d| {
                    d.child(
                        check("wt-copy", "Copy local files", self.copy_files, cx).on_click(
                            cx.listener(|d, _, _, cx| {
                                d.copy_files = !d.copy_files;
                                cx.notify();
                            }),
                        ),
                    )
                })
                .child(
                    check("wt-open", "Open in a tab", self.open_tab, cx).on_click(cx.listener(
                        |d, _, _, cx| {
                            d.open_tab = !d.open_tab;
                            cx.notify();
                        },
                    )),
                ),
        )
        .children(
            self.error
                .clone()
                .map(|e| div().text_color(color(cx, T::StatusError)).child(e)),
        )
        .child(
            div()
                .flex()
                .justify_end()
                .gap_2()
                .child(
                    button("wt-cancel", "Cancel", cx)
                        .on_click(cx.listener(|_, _, _, cx| cx.emit(WorktreeDialogEvent::Closed))),
                )
                .child(
                    primary_button("wt-create", "Create", cx)
                        .on_click(cx.listener(|d, _, window, cx| d.create(window, cx))),
                ),
        )
    }
}

/// The Remove Worktree confirmation: what would be lost, "Also delete
/// branch", a second click when something would be lost.
pub struct RemoveWorktreeDialog {
    core: Arc<Core>,
    worktree: RepoId,
    label: String,
    path: RepoPath,
    loss: Option<Result<WorktreeLoss, String>>,
    delete_branch: bool,
    /// "Remove anyway" was clicked once.
    armed: Armed<()>,
}

impl EventEmitter<WorktreeDialogEvent> for RemoveWorktreeDialog {}

impl RemoveWorktreeDialog {
    /// The confirmation for linked worktree row `worktree`.
    pub fn new(core: Arc<Core>, worktree: RepoId, cx: &mut Context<Self>) -> Self {
        let row = core.row(worktree);
        let label = row
            .as_ref()
            .and_then(|r| r.worktree.as_ref().map(|w| w.label.clone()))
            .unwrap_or_default();
        let path = row.map_or_else(|| RepoPath::new(""), |r| r.path);
        let dialog = Self {
            core,
            worktree,
            label,
            path,
            loss: None,
            delete_branch: false,
            armed: Armed::default(),
        };
        dialog.load(cx);
        dialog
    }

    fn load(&self, cx: &mut Context<Self>) {
        let (core, id) = (self.core.clone(), self.worktree);
        let task = cx.background_spawn(async move { core.worktree_loss(id) });
        cx.spawn(async move |this, cx| {
            let loss = task.await.map_err(|e| e.to_string());
            let _ = this.update(cx, |d, cx| {
                d.delete_branch = loss
                    .as_ref()
                    .is_ok_and(|l| l.branch.is_some() && l.branch_safe_to_delete);
                d.loss = Some(loss);
                cx.notify();
            });
        })
        .detach();
    }

    fn remove(&mut self, cx: &mut Context<Self>) {
        let Some(Ok(loss)) = &self.loss else { return };
        if loss.in_progress.is_some() {
            return;
        }
        if !loss.is_safe() && !self.armed.click((), cx, |d| &mut d.armed) {
            return;
        }
        let command = Command::RemoveWorktree {
            worktree: self.worktree,
            force: !loss.is_safe(),
            delete_branch: self.delete_branch,
        };
        if let Err(e) = self.core.dispatch(command) {
            cx.emit(WorktreeDialogEvent::Notice(e.to_string()));
        }
        cx.emit(WorktreeDialogEvent::Closed);
    }

    /// Aborts the operation in progress in the worktree, then reads again.
    fn abort(&mut self, cx: &mut Context<Self>) {
        let handle = self.core.submit_repo_job(
            &self.path,
            OpClass::Write,
            "Abort the operation in progress",
            |a, p, _| a.sequence(p, SequenceAction::Abort),
        );
        self.loss = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = handle.wait().await;
            let _ = this.update(cx, |d, cx| {
                if let Err(e) = result {
                    cx.emit(WorktreeDialogEvent::Notice(e.to_string()));
                }
                d.load(cx);
            });
        })
        .detach();
    }
}

impl Render for RemoveWorktreeDialog {
    #[allow(clippy::too_many_lines)] // declarative element tree
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut body: Vec<AnyElement> = Vec::new();
        let mut in_progress = false;
        let mut safe = true;
        let mut branch = None;
        match &self.loss {
            None => body.push(
                div()
                    .text_color(color(cx, T::FgMuted))
                    .child("Checking what would be lost…")
                    .into_any_element(),
            ),
            Some(Err(e)) => body.push(
                div()
                    .text_color(color(cx, T::StatusError))
                    .child(e.clone())
                    .into_any_element(),
            ),
            Some(Ok(loss)) => {
                in_progress = loss.in_progress.is_some();
                safe = loss.is_safe();
                branch.clone_from(&loss.branch);
                if safe {
                    body.push(
                        div()
                            .text_color(color(cx, T::StatusSuccess))
                            .child("Nothing would be lost.")
                            .into_any_element(),
                    );
                } else {
                    body.push(
                        div()
                            .text_color(color(cx, T::StatusWarning))
                            .child("Removing it would lose (undo brings the files back):")
                            .into_any_element(),
                    );
                    for line in loss.lines() {
                        body.push(div().pl_4().child(format!("• {line}")).into_any_element());
                    }
                    for f in loss.changes.iter().take(8) {
                        body.push(
                            div()
                                .pl_8()
                                .text_xs()
                                .text_color(color(cx, T::FgMuted))
                                .child(f.clone())
                                .into_any_element(),
                        );
                    }
                }
            }
        }
        let remove_label = match (in_progress, safe, self.armed.is(&())) {
            (true, _, _) | (false, true, _) => "Remove",
            (false, false, false) => "Remove anyway…",
            (false, false, true) => "Click again to remove",
        };
        let ready = matches!(self.loss, Some(Ok(_))) && !in_progress;
        dialog_frame(
            "remove-worktree",
            format!(
                "Remove worktree {} at {}?",
                self.label,
                self.path.as_path().display()
            ),
            cx,
        )
        .on_key_down(cx.listener(|_, e: &KeyDownEvent, _, cx| {
            if e.keystroke.key == "escape" {
                cx.stop_propagation();
                cx.emit(WorktreeDialogEvent::Closed);
            }
        }))
        .children(body)
        .when_some(branch, |d, b| {
            d.child(
                check(
                    "rm-branch",
                    format!("Also delete branch {b}"),
                    self.delete_branch,
                    cx,
                )
                .on_click(cx.listener(|d, _, _, cx| {
                    d.delete_branch = !d.delete_branch;
                    cx.notify();
                })),
            )
        })
        .child(
            div()
                .flex()
                .justify_end()
                .gap_2()
                .when(in_progress, |d| {
                    d.child(
                        button("rm-abort", "Abort the operation in progress", cx)
                            .on_click(cx.listener(|d, _, _, cx| d.abort(cx))),
                    )
                })
                .child(
                    button("rm-cancel", "Cancel", cx)
                        .on_click(cx.listener(|_, _, _, cx| cx.emit(WorktreeDialogEvent::Closed))),
                )
                .child(
                    primary_button("rm-ok", remove_label, cx)
                        .when(!safe && ready, |b| b.bg(color(cx, T::StatusError)))
                        .on_click_if(ready, cx.listener(|d, _, _, cx| d.remove(cx))),
                ),
        )
    }
}
