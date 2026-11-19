//! Resolving conflicts (spec §5): in the built-in merge view, or in the
//! user's external merge tool.
//!
//! * Opened by itself when a write or network operation of Archipel (pull,
//!   merge, rebase, cherry-pick, stash pop, continue…) leaves a repo with
//!   conflicts it did not have before; on demand with "Resolve conflicts".
//! * What opens is the choice of the `merge_tool` setting, also set from
//!   the action palette ([`MergeToolChoice`]): the built-in merge view by
//!   default (the UI opens the repo tab on the first conflicted file, from
//!   [`AppEvent::ConflictsToResolve`]), else `git mergetool` with the tool
//!   named, or Git's `merge.tool`. With neither, nothing is launched (Git
//!   would guess one, possibly a terminal tool that cannot run here) and a
//!   notice says how to pick one.
//! * One external tool at a time per repo; the choice and the open tools
//!   are shared by every window.

use std::collections::HashSet;
use std::sync::{Arc, Mutex, PoisonError, RwLock};

use archipel_model::RepoId;
use archipel_model::changes::FileState;
use archipel_model::ports::MergeToolInfo;
use archipel_model::settings::MergeToolChoice;
use archipel_ops::{OpClass, OpError, OpEvent, OpEventKind};

use super::{App, AppError, AppEvent, lock};
use crate::dashboard::DashboardRow;

/// Label of the external merge tool operations: they are not watched for
/// conflicts themselves (closing the tool with conflicts left must not
/// reopen it). The merge view's own "Merge tool on <file>" shares it.
const MERGE_TOOL_OP: &str = "Merge tool";

/// The merge tool choice and the repos whose external tool is open, shared
/// by the sibling cores (one per window).
#[derive(Default)]
pub(super) struct MergeTools {
    choice: RwLock<MergeToolChoice>,
    /// Repos whose external tool is open (or about to be).
    open: Mutex<HashSet<RepoId>>,
}

impl MergeTools {
    pub(super) fn new(choice: MergeToolChoice) -> Arc<Self> {
        Arc::new(Self {
            choice: RwLock::new(choice),
            open: Mutex::default(),
        })
    }

    /// Marks `repo`'s tool open; `false` when it already was.
    fn begin(&self, repo: RepoId) -> bool {
        lock(&self.open).insert(repo)
    }

    fn end(&self, repo: RepoId) {
        lock(&self.open).remove(&repo);
    }
}

/// Conflicted files of a row, as last read.
fn conflicted(row: &DashboardRow) -> u32 {
    row.summary().map_or(0, |s| s.changes.conflicted)
}

impl App {
    /// What resolves conflicts, in every window.
    #[must_use]
    pub fn merge_tool_choice(&self) -> MergeToolChoice {
        self.merge_tools
            .choice
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Sets what resolves conflicts in every window. Saving it is the
    /// caller's (the settings file is the user's).
    pub fn set_merge_tool_choice(&self, choice: MergeToolChoice) {
        *self
            .merge_tools
            .choice
            .write()
            .unwrap_or_else(PoisonError::into_inner) = choice;
    }

    /// The merge tools Git knows, installed first, read in the repo of row
    /// `repo` (any row when `None`; none without a project). Blocking: run
    /// it off the UI thread.
    pub fn merge_tool_choices(&self, repo: Option<RepoId>) -> Result<Vec<MergeToolInfo>, AppError> {
        let path = {
            let guard = lock(&self.session);
            let rows = guard
                .as_ref()
                .map(|s| s.rows.as_slice())
                .unwrap_or_default();
            repo.and_then(|id| rows.iter().find(|r| r.id == id))
                .or_else(|| rows.first())
                .map(|r| r.path.clone())
        };
        let Some(path) = path else {
            return Ok(Vec::new());
        };
        Ok(self
            .services
            .git
            .merge_tool
            .tools(&path)
            .map_err(OpError::from)?)
    }

    /// Watches the operations of the open project's repos: one that leaves
    /// conflicts where there were none opens what resolves them.
    pub(super) fn on_op_event(&self, event: &OpEvent) {
        if !matches!(event.class, OpClass::Write | OpClass::Network)
            || event.label.starts_with(MERGE_TOOL_OP)
        {
            return;
        }
        let Some(repo) = event.repo else { return };
        match &event.kind {
            OpEventKind::Started => {
                let mut guard = lock(&self.session);
                let Some(s) = guard.as_mut() else { return };
                if let Some(row) = s.rows.iter().find(|r| r.id == repo) {
                    let before = conflicted(row);
                    s.conflicts_before.insert(repo, before);
                }
            }
            OpEventKind::Finished(_) => {
                let before = lock(&self.session)
                    .as_mut()
                    .and_then(|s| s.conflicts_before.remove(&repo));
                if before == Some(0) {
                    self.resolve_conflicts(repo, true);
                }
            }
            OpEventKind::Queued | OpEventKind::Progress(_) => {}
        }
    }

    /// Opens what resolves the conflicts of `repo`, once its conflicted
    /// files are read (off the caller's thread). `auto`: after an
    /// operation, so silent when there is none or the tool is open.
    pub(super) fn resolve_conflicts(&self, repo: RepoId, auto: bool) {
        let Some(path) = self.path_of(repo) else {
            return;
        };
        let status = self.services.git.status.clone();
        let me = self.me.clone();
        self.scheduler.runtime().spawn_blocking(move || {
            let first = match status.changes(&path) {
                Ok(changes) => changes
                    .into_iter()
                    .find(|c| c.state == FileState::Conflicted)
                    .map(|c| c.path),
                Err(e) => {
                    tracing::warn!(error = %e, "cannot read the conflicted files");
                    None
                }
            };
            let Some(app) = me.upgrade() else { return };
            let Some(file) = first else {
                if !auto {
                    app.emit(AppEvent::Notice(format!("No conflict in {}", path.name())));
                }
                return;
            };
            match app.merge_tool_choice() {
                MergeToolChoice::BuiltIn => {
                    // The dashboard shows them too, without waiting for
                    // the watcher.
                    app.refresh_repo(repo);
                    app.emit(AppEvent::ConflictsToResolve { repo, file });
                }
                choice => app.open_external_merge_tool(repo, choice.external_tool(), auto),
            }
        });
    }

    /// Runs `git mergetool` (with `tool`, Git's `merge.tool` when `None`) on
    /// every conflicted file of `repo`, on its write queue (the tool stages
    /// what it resolves), then refreshes it.
    fn open_external_merge_tool(&self, repo: RepoId, tool: Option<&str>, auto: bool) {
        let Some(path) = self.path_of(repo) else {
            return;
        };
        let name = path.name();
        if !self.merge_tools.begin(repo) {
            if !auto {
                self.emit(AppEvent::Notice(format!(
                    "The merge tool is already open on {name}"
                )));
            }
            return;
        }
        let tool = tool.map(str::to_owned);
        let tools = self.merge_tools.clone();
        let me = self.me.clone();
        let _h = self.submit_repo_job(
            &path,
            OpClass::Write,
            format!("{MERGE_TOOL_OP} · {name}"),
            move |a, p, _| {
                let result = (|| {
                    let tool = match tool {
                        Some(tool) => Some(tool),
                        None => a.configured_merge_tool(p)?,
                    };
                    let Some(tool) = tool else { return Ok(false) };
                    a.merge_tool(p, None, Some(&tool)).map(|()| true)
                })();
                tools.end(repo);
                if let Some(app) = me.upgrade() {
                    match &result {
                        Ok(true) => {}
                        Ok(false) => app.emit(AppEvent::Notice(format!(
                            "Conflicts in {name}: Git names no merge tool (merge.tool); \
                             pick one with “Set merge tool…” in the action palette"
                        ))),
                        Err(e) => app.emit(AppEvent::Notice(format!(
                            "The merge tool failed on {name}: {e}"
                        ))),
                    }
                    app.refresh_repo(repo);
                }
                result.map(drop)
            },
        );
    }
}
