//! Which repos an action acts on.
//!
//! Every action on "the selected repos (all when none)" resolves its
//! targets here, so a batch, the forge, the branches panel and the CI
//! agree on what "all" means:
//!
//! - repos given: those of them that are shown (hidden ones never are);
//! - none given, in a Worktree Set view: the set's shown worktrees
//!   (spec §10);
//! - none given otherwise: every shown repo, not its linked worktrees.

use archipel_model::RepoId;

use super::session::Session;
use super::{App, lock};
use crate::dashboard::DashboardRow;

impl Session {
    /// The rows an action on `ids` acts on (see the [module docs](self)),
    /// in project order (set order in a set view).
    pub(super) fn target_rows(&self, ids: &[RepoId]) -> Vec<&DashboardRow> {
        if ids.is_empty()
            && let Some(set) = self.ui.active_set()
        {
            return self.set_rows(set).filter(|r| !r.hidden).collect();
        }
        self.rows
            .iter()
            .filter(|r| !r.hidden)
            .filter(|r| {
                if ids.is_empty() {
                    r.parent().is_none()
                } else {
                    ids.contains(&r.id)
                }
            })
            .collect()
    }
}

impl App {
    /// The repos an action on `ids` acts on: `ids` when given, else every
    /// repo, or the active Worktree Set's worktrees (see the
    /// [module docs](self)). Every "selected repos (all when none)" action
    /// goes through it.
    #[must_use]
    pub fn resolve_targets(&self, ids: &[RepoId]) -> Vec<RepoId> {
        lock(&self.session)
            .as_ref()
            .map(|s| s.target_rows(ids).into_iter().map(|r| r.id).collect())
            .unwrap_or_default()
    }

    /// The rows of [`Self::resolve_targets`].
    pub(super) fn target_rows(&self, ids: &[RepoId]) -> Vec<DashboardRow> {
        lock(&self.session)
            .as_ref()
            .map(|s| s.target_rows(ids).into_iter().cloned().collect())
            .unwrap_or_default()
    }
}
