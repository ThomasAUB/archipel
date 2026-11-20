//! Integrated terminals (issue #98): what a new terminal runs, starting it,
//! and the name of its tab.

use std::path::PathBuf;
use std::sync::Arc;

use archipel_model::RepoId;
use archipel_model::ports::{TerminalSession, TerminalSink};
use archipel_model::terminal::{TerminalSize, TerminalSpec};

use super::{App, AppError};
use crate::terminal::{default_shell, parse_shell, run_command};

impl App {
    /// What a new terminal in `cwd` runs, from the settings: the shell, or
    /// `command` through the shell when given.
    #[must_use]
    pub fn terminal_spec(
        &self,
        cwd: PathBuf,
        command: Option<&str>,
        size: TerminalSize,
    ) -> TerminalSpec {
        let shell = self
            .settings()
            .terminal_shell
            .as_deref()
            .and_then(parse_shell);
        let command = match command {
            Some(command) => Some(run_command(&shell.unwrap_or_else(default_shell), command)),
            // No shell set: the adapter starts the login shell itself.
            None => shell,
        };
        TerminalSpec {
            cwd,
            command,
            size,
            scrollback: self.settings().terminal_scrollback,
            env: Vec::new(),
        }
    }

    /// Starts a terminal; its events go to `sink`.
    pub fn launch_terminal(
        &self,
        spec: TerminalSpec,
        sink: Arc<dyn TerminalSink>,
    ) -> Result<Arc<dyn TerminalSession>, AppError> {
        Ok(self.services.terminals.launch(spec, sink)?)
    }

    /// The name of a terminal tab on repo row `id`: `repo · branch`.
    #[must_use]
    pub fn terminal_title(&self, id: RepoId) -> Option<String> {
        let row = self.row(id)?;
        let branch = row
            .summary()
            .and_then(|s| s.head.branch().map(str::to_owned));
        Some(match branch {
            Some(branch) => format!("{} · {branch}", row.name),
            None => row.name,
        })
    }
}
