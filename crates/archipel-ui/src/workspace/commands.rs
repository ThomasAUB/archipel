//! Actions run from the keymap, the action palette or a menu: UI
//! commands, forge links, diagnostics, and the project file pickers.

use std::fmt::Write as _;

use archipel_app::{App as Core, Command};
use archipel_model::RepoId;
use gpui_kit::prelude::*;
use gpui_kit::{Context, PathPromptOptions, Point, Window};

use crate::commit_panel::CommitPanel;
use crate::forge_panel::ForgeTab;
use crate::new_repo_dialog::{NewRepoDialog, NewRepoEvent};
use crate::publish_dialog::{PublishDialog, PublishEvent};

use super::Workspace;
use super::menus::Menu;
use super::overlays::{Overlay, PaletteKind};

impl Workspace {
    /// The label of the key bound to action `id` (rebindable), if any.
    pub(super) fn shortcut(&self, id: &str) -> Option<String> {
        crate::keys::label_for(&self.core.extensions().keymap, id)
    }

    /// The repos in focus, as commands on "the selected repos (all when
    /// none)" take them: the active tab's repo, else the dashboard
    /// selection ([`archipel_model::ui_state::Focus`]). Every panel and
    /// dialog scoped to "the selected repos" takes it, so none can act on
    /// the dashboard selection from a repo tab. The core resolves an empty
    /// one ([`Core::resolve_targets`]).
    pub(super) fn targets(&self) -> Vec<RepoId> {
        self.core.action_context().targets()
    }

    /// Opens the statistics on the repos in focus ([`Self::targets`]),
    /// a repo tab's repo shown first.
    pub(super) fn open_stats_in_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let ctx = self.core.action_context();
        self.open_stats(ctx.targets(), ctx.active_repo(), window, cx);
    }

    /// Runs the action `id` of the registry: the one way the palette, the
    /// keymap and menus run an action, so they cannot disagree. An action
    /// not available now says so rather than doing something else.
    pub(super) fn run_action(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.close_menu(cx);
        let ctx = self.core.action_context();
        let Some(action) = self.core.extensions().actions.get(id).cloned() else {
            self.notice = Some(format!("“{id}” is not an action"));
            cx.notify();
            return;
        };
        if !action.is_available(&ctx) {
            self.notice = Some(format!("“{}” is not available here", action.label()));
            cx.notify();
            return;
        }
        match action.command(&ctx) {
            Some(Command::Ui(ui)) => self.run_ui_command(&ui, window, cx),
            Some(command) => self.dispatch(command, cx),
            None => {}
        }
    }

    /// A keyboard shortcut. While an overlay is open, another one would
    /// replace it and lose what was typed in it: only the window shortcuts
    /// run then, and Ctrl+Enter ("commit.submit") commits in the commit
    /// panel.
    pub(super) fn run_shortcut(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        match &self.overlay {
            None => self.run_action(id, window, cx),
            Some((Overlay::Commit(panel), _)) if id == "commit.submit" => {
                panel.update(cx, CommitPanel::submit);
            }
            Some(_) if id.starts_with("window.") => self.run_action(id, window, cx),
            Some(_) => {}
        }
    }

    fn run_ui_command(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        match id {
            "palette.projects" => self.open_palette(PaletteKind::Projects, window, cx),
            "palette.actions" => self.open_palette(PaletteKind::Actions, window, cx),
            "project.new" => self.new_project_from_folder(cx),
            "project.add_repos" => self.add_repos(cx),
            "project.new_repo" => self.open_new_repo(window, cx),
            "project.clone" | "forge.clone" => self.open_forge(ForgeTab::Clone, window, cx),
            "commit.open" | "commit.submit" => self.open_commit(window, cx),
            "repos.checkout_all" => self.open_palette(PaletteKind::Checkout, window, cx),
            "repos.branch_all" => self.open_palette(PaletteKind::Branch, window, cx),
            "repos.tag_all" => self.open_palette(PaletteKind::Tag, window, cx),
            "repos.branches" => self.open_branches(window, cx),
            "repos.stats" => {
                self.open_stats_in_focus(window, cx);
            }
            "settings.merge_tool" => self.choose_merge_tool(window, cx),
            "repo.add_submodule" => {
                if let Some(id) = self.core.action_context().target_repo() {
                    self.open_palette(PaletteKind::SubmoduleUrl(id), window, cx);
                }
            }
            "settings.appearance" => {
                let size = window.viewport_size();
                self.open_menu(Menu::Theme, Point::new(size.width, size.height), cx);
            }
            "app.copy_diagnostics" => self.copy_diagnostics(cx),
            "window.new" => crate::windows::open(cx, None),
            "window.next" => self.cycle_window(true, window, cx),
            "window.previous" => self.cycle_window(false, window, cx),
            "project.set_color" => self.open_palette(PaletteKind::Color, window, cx),
            "project.set_background_tint" => {
                self.open_palette(PaletteKind::BackgroundTint, window, cx);
            }
            "project.set_icon" => self.open_palette(PaletteKind::Icon, window, cx),
            "project.set_icon_image" => self.pick_icon_image(cx),
            "repos.set_category" => {
                let targets = self.targets();
                self.set_category(targets, window, cx);
            }
            "forge.accounts" => self.open_forge(ForgeTab::Accounts, window, cx),
            "forge.pull_requests" => {
                let ids = self.targets();
                self.open_forge(ForgeTab::Open(ids), window, cx);
            }
            "forge.create_pull_requests" => {
                let ids = self.targets();
                self.open_forge(ForgeTab::Create(ids), window, cx);
            }
            "forge.issues" => {
                let ids = self.targets();
                self.open_forge(ForgeTab::Issues(ids), window, cx);
            }
            "forge.new_issue" => {
                let ids = self.targets();
                self.open_forge(ForgeTab::NewIssue(ids), window, cx);
            }
            "forge.open_pull_request" => self.open_pull_request(cx),
            "forge.open_ci" => self.open_ci(cx),
            terminal if terminal.starts_with("terminal.") => {
                self.run_terminal_command(terminal, window, cx);
            }
            worktree if worktree.starts_with("worktree.") || worktree == "palette.worktrees" => {
                self.run_worktree_command(worktree, window, cx);
            }
            other => self.notice = Some(format!("“{other}” is not available in this build")),
        }
        cx.notify();
    }

    /// Opens the pull request of the active / selected repo in the browser.
    fn open_pull_request(&mut self, cx: &mut Context<Self>) {
        let url = self
            .core
            .action_context()
            .target_repo()
            .and_then(|id| self.core.row(id))
            .and_then(|r| r.forge.pull_request().map(|pr| pr.url.clone()));
        match url {
            Some(url) => cx.open_url(&url),
            None => self.notice = Some("No open pull request for this repository".into()),
        }
    }

    fn open_ci(&mut self, cx: &mut Context<Self>) {
        let url = self
            .core
            .action_context()
            .target_repo()
            .and_then(|id| self.core.row(id))
            .and_then(|r| r.ci.run().and_then(|run| run.url.clone()));
        match url {
            Some(url) => cx.open_url(&url),
            None => self.notice = Some("No CI run for this branch".into()),
        }
    }

    /// Activates the next (or previous) Archipel window; says so when this
    /// is the only one.
    pub(super) fn cycle_window(
        &mut self,
        forward: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !crate::windows::cycle(cx, window.window_handle(), forward) {
            self.notice = Some("No other Archipel window".into());
            cx.notify();
        }
    }

    fn copy_diagnostics(&mut self, cx: &mut Context<Self>) {
        let rss = memory_stats::memory_stats().map_or(0, |m| m.physical_mem / (1024 * 1024));
        let mut text = format!(
            "Archipel {}\nOS: {} {}\nRSS: {rss} MiB\n",
            env!("CARGO_PKG_VERSION"),
            std::env::consts::OS,
            std::env::consts::ARCH
        );
        for c in self.core.cache_report() {
            let _ = writeln!(
                text,
                "cache {}: {} entries, {} / {} bytes",
                c.name, c.entries, c.bytes, c.budget
            );
        }
        for (_, line, _) in self.log.iter().rev().take(50) {
            text.push_str(line);
            text.push('\n');
        }
        cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(text));
        self.notice = Some("Diagnostics copied to the clipboard".into());
    }

    fn pick(
        directories: bool,
        cx: &mut Context<Self>,
        then: impl FnOnce(&mut Self, std::path::PathBuf, &mut Context<Self>) + 'static,
    ) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: !directories,
            directories,
            multiple: false,
            prompt: None,
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = rx.await
                && let Some(path) = paths.into_iter().next()
            {
                let _ = this.update(cx, |w, cx| then(w, path, cx));
            }
        })
        .detach();
    }

    #[allow(clippy::unused_self)] // called like every other view action
    fn pick_icon_image(&mut self, cx: &mut Context<Self>) {
        Self::pick(false, cx, |w, file, cx| {
            let icon = file.to_string_lossy().into_owned();
            w.dispatch(Command::SetIcon(Some(icon)), cx);
        });
    }

    /// Runs blocking core work (scans, project files) off the UI thread and
    /// shows its error, if any.
    fn run_core(
        &mut self,
        cx: &mut Context<Self>,
        job: impl FnOnce(&Core) -> Result<(), archipel_app::AppError> + Send + 'static,
    ) {
        let core = self.core.clone();
        let task = cx.background_spawn(async move { job(&core) });
        cx.spawn(async move |this, cx| {
            if let Err(e) = task.await {
                // The workspace may have been closed meanwhile.
                let _ = this.update(cx, |w, cx| {
                    w.notice = Some(e.to_string());
                    cx.notify();
                });
            }
        })
        .detach();
    }

    #[allow(clippy::unused_self)] // called like every other view action
    pub(super) fn new_project_from_folder(&mut self, cx: &mut Context<Self>) {
        Self::pick(true, cx, |w, dir, cx| {
            w.run_core(cx, move |core| core.new_project_from_folder(&dir).map(drop));
        });
    }

    #[allow(clippy::unused_self)] // called like every other view action
    pub(super) fn open_project_file(&mut self, cx: &mut Context<Self>) {
        Self::pick(false, cx, |w, file, cx| {
            w.run_core(cx, move |core| {
                let id = core.import_project_file(&file)?;
                core.open_project(&id)
            });
        });
    }

    /// Opens the New Repository dialog (issue #68).
    pub(super) fn open_new_repo(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let core = self.core.clone();
        let dialog = cx.new(|cx| NewRepoDialog::new(core, window, cx));
        let sub = cx.subscribe_in(&dialog, window, |this, _, e, window, cx| match e {
            NewRepoEvent::Closed => this.close_overlay(window, cx),
        });
        self.show_overlay(Overlay::NewRepo(dialog), sub, cx);
    }

    /// Opens the Publish dialog for `branch` of repo row `repo` (at `dir`),
    /// which has no remote: Push on it creates the repository on a forge.
    pub(super) fn open_publish(
        &mut self,
        repo: RepoId,
        dir: &std::path::Path,
        branch: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let core = self.core.clone();
        let dialog = cx.new(|cx| PublishDialog::new(core, repo, dir, branch, window, cx));
        let sub = cx.subscribe_in(&dialog, window, |this, _, e, window, cx| match e {
            PublishEvent::Closed => this.close_overlay(window, cx),
            PublishEvent::SignIn => this.open_forge(ForgeTab::Accounts, window, cx),
        });
        self.show_overlay(Overlay::Publish(dialog), sub, cx);
    }

    #[allow(clippy::unused_self)] // called like every other view action
    pub(super) fn add_repos(&mut self, cx: &mut Context<Self>) {
        Self::pick(true, cx, |w, dir, cx| {
            w.run_core(cx, move |core| core.add_folder(&dir));
        });
    }
}
