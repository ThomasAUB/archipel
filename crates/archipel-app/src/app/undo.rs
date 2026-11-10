//! Undo and redo of the last journaled operation of a repo, run on the
//! scheduler; the outcome is told as an [`AppEvent::Notice`].

use archipel_model::RepoId;
use archipel_model::journal::JournalEntry;
use archipel_ops::{Direction, OpClass, OpSpec};

use super::{App, AppError, AppEvent};

impl App {
    /// Undoes or redoes the next journal entry of `repo` in the background.
    pub(super) fn spawn_undo(&self, repo: RepoId, direction: Direction) -> Result<(), AppError> {
        let path = self
            .path_of(repo)
            .ok_or_else(|| AppError::NotFound(repo.to_string()))?;
        let undo = self.undo.clone();
        let me = self.me.clone();
        let label = if direction == Direction::Undo {
            "Undo"
        } else {
            "Redo"
        };
        let depth = self.settings.journal_limit;
        let spec = OpSpec::new(Some(repo), OpClass::Write, label);
        let _h = self.scheduler.submit(spec, move |_| {
            // A stopped rebase (or merge…) is undone by aborting it: it is
            // not in the journal, and the entries before it cannot be
            // undone while it is in progress.
            let aborted = match direction {
                Direction::Undo => undo.abort_in_progress(&path).transpose(),
                Direction::Redo => None,
            };
            let outcome = if let Some(aborted) = aborted {
                aborted.map(|what| format!("{label}: aborted {what}"))
            } else {
                let entry: Option<JournalEntry> = match direction {
                    Direction::Undo => undo.next_undo(repo, depth)?,
                    Direction::Redo => undo.next_redo(repo, depth)?,
                };
                match entry {
                    None => Ok(format!("nothing to {}", label.to_lowercase())),
                    Some(e) => undo.execute(&path, &e, direction).map(|note| {
                        note.map_or_else(
                            || format!("{label}: {}", e.label),
                            |n| format!("{label}: {} ({n})", e.label),
                        )
                    }),
                }
            };
            if let Some(app) = me.upgrade() {
                let message = match &outcome {
                    Ok(message) => message.clone(),
                    Err(err) => format!("{label} refused: {err}"),
                };
                app.emit(AppEvent::Notice(message));
                app.refresh_repo(repo);
            }
            // A refused undo fails the op, so the drawer does not show it
            // as done.
            outcome.map(drop)
        });
        Ok(())
    }
}
