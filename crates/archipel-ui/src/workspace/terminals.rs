//! The window's integrated terminal panel (issue #98): showing and hiding
//! it, and opening terminals on repos and worktrees.

use archipel_model::RepoId;
use gpui_kit::prelude::*;
use gpui_kit::{Context, Entity, Window};

use crate::terminal::{TerminalPanel, TerminalPanelEvent, home_dir};

use super::Workspace;
use super::overlays::PaletteKind;

impl Workspace {
    /// The panel, created on first use (no terminal runs before).
    fn terminal_panel(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<TerminalPanel> {
        if let Some((panel, _)) = &self.terminal {
            return panel.clone();
        }
        let core = self.core.clone();
        let panel = cx.new(|_| TerminalPanel::new(core));
        let sub = cx.subscribe_in(&panel, window, |this, _, e, window, cx| match e {
            TerminalPanelEvent::Notice(text) => {
                this.notice = Some(text.clone());
                cx.notify();
            }
            TerminalPanelEvent::Hide => this.hide_terminal(window, cx),
        });
        self.terminal = Some((panel.clone(), sub));
        panel
    }

    /// The panel when shown.
    pub(super) fn shown_terminal(&self) -> Option<Entity<TerminalPanel>> {
        self.terminal
            .as_ref()
            .filter(|_| self.terminal_open)
            .map(|(p, _)| p.clone())
    }

    fn hide_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.terminal_open = false;
        self.focus.focus(window, cx);
        cx.notify();
    }

    /// Shows the panel (with a terminal on the active repo when it has
    /// none), or hides it.
    pub(super) fn toggle_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.terminal_open {
            self.hide_terminal(window, cx);
            return;
        }
        let panel = self.terminal_panel(window, cx);
        if panel.read(cx).is_empty() {
            self.new_terminal(window, cx);
            return;
        }
        self.terminal_open = true;
        panel.update(cx, |p, cx| p.focus_active(window, cx));
        cx.notify();
    }

    /// A new terminal on the active or selected repo, else in the home
    /// folder.
    pub(super) fn new_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.target_worktree() {
            Some(id) => self.open_terminals(&[id], window, cx),
            None => {
                self.open_terminal_in(home_dir(), "Terminal".into(), None, true, window, cx);
            }
        }
    }

    /// Opens a terminal on each repo or worktree row of `ids`, the last
    /// one shown.
    pub(super) fn open_terminals(
        &mut self,
        ids: &[RepoId],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        for (i, id) in ids.iter().enumerate() {
            let Some(row) = self.core.row(*id) else {
                continue;
            };
            let title = self.core.terminal_title(*id).unwrap_or(row.name);
            let last = i + 1 == ids.len();
            let cwd = row.path.as_path().to_path_buf();
            self.open_terminal_in(cwd, title, None, last, window, cx);
        }
    }

    /// Runs `command` in a terminal of its own on each of `ids` (the
    /// selected repos, all when none), so each one's output and exit status
    /// show in its tab.
    pub(super) fn run_in_repos(
        &mut self,
        command: &str,
        ids: &[RepoId],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let command = command.trim();
        if command.is_empty() {
            return;
        }
        let targets = self.core.resolve_targets(ids);
        let mut started = 0;
        for id in &targets {
            let Some(row) = self.core.row(*id) else {
                continue;
            };
            let title = self.core.terminal_title(*id).unwrap_or(row.name);
            let cwd = row.path.as_path().to_path_buf();
            if self.open_terminal_in(cwd, title, Some(command.to_owned()), false, window, cx) {
                started += 1;
            }
        }
        self.notice = Some(format!("Running “{command}” in {started} repo(s)"));
        cx.notify();
    }

    /// Asks which command to run in each of `ids` (all when empty).
    pub(super) fn ask_run_in_repos(
        &mut self,
        ids: Vec<RepoId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_palette(PaletteKind::RunCommand(ids), window, cx);
    }

    fn open_terminal_in(
        &mut self,
        cwd: std::path::PathBuf,
        title: String,
        command: Option<String>,
        focus: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let panel = self.terminal_panel(window, cx);
        let started = panel.update(cx, |p, cx| p.open(cwd, title, command, focus, window, cx));
        if started {
            self.close_menu(cx);
            self.terminal_open = true;
        }
        cx.notify();
        started
    }

    /// Runs a `terminal.*` UI command.
    pub(super) fn run_terminal_command(
        &mut self,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match id {
            "terminal.toggle" => self.toggle_terminal(window, cx),
            "terminal.new" => self.new_terminal(window, cx),
            "terminal.open_here" => match self.target_worktree() {
                Some(row) => self.open_terminals(&[row], window, cx),
                None => self.notice = Some("Select a repository or worktree first".into()),
            },
            "terminal.run_in_repos" => {
                let ids = self.selection();
                self.ask_run_in_repos(ids, window, cx);
            }
            _ => {}
        }
    }
}
