//! Worktrees of the project's repos (spec §10, WORKTREES.md): discovered
//! from Git, never written to the project file.
//!
//! A linked worktree is a row like any repo (its own [`RepoId`], derived
//! from its path), so every per-repo use case works on it unchanged; what is
//! new is its relation to the repo row it belongs to
//! ([`DashboardRow::worktree`]) and the worktree lists of the session.
//!
//! * Lists are read when the project opens, when repos are added, on a full
//!   refresh and when the watcher sees `<common dir>/worktrees/` change.
//! * A linked worktree is watched only while visible (expanded, open in a
//!   tab or in the active set); otherwise it is refreshed on focus.

use std::collections::{HashMap, HashSet};

use archipel_model::ui_state::TabState;
use archipel_model::worktree::{SetView, WorktreeInfo};
use archipel_model::{GitError, RepoId, RepoPath};
use archipel_ops::{OpClass, OpSpec};

use super::session::Session;
use super::{App, AppEvent, lock, toggle};
use crate::dashboard::{DashboardRow, RepoStatus, WorktreeRowInfo};
use crate::forecast::ConflictForecast;
use crate::forge::{BranchCi, ForgeStatus};

/// A worktree of the project, for the Worktree Switcher and menus.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectWorktree {
    /// Its row.
    pub id: RepoId,
    /// The repo row it belongs to (itself for a main worktree).
    pub repo: RepoId,
    /// Repo name.
    pub name: String,
    /// Branch, or `detached @ …`.
    pub label: String,
    /// Folder.
    pub path: RepoPath,
    /// A main worktree.
    pub is_main: bool,
    /// Lock reason, when locked.
    pub locked: Option<String>,
    /// Its folder is gone.
    pub missing: bool,
}

impl Session {
    /// The row of `id`.
    pub(super) fn row(&self, id: RepoId) -> Option<&DashboardRow> {
        self.rows.iter().find(|r| r.id == id)
    }

    /// Whether the linked worktree row `row` is visible, so watched:
    /// expanded under its repo, open in a tab, or a member of the active
    /// set. Repo rows (and worktrees whose repo is not in the project)
    /// always are.
    pub(super) fn worktree_visible(&self, row: &DashboardRow) -> bool {
        let Some(parent) = row.parent() else {
            return true;
        };
        self.ui.set_view != SetView::MainOnly && self.ui.expanded_worktrees.contains(&parent)
            || self.ui.tabs.contains(&TabState::Repo(row.id))
            || self
                .ui
                .active_set()
                .is_some_and(|set| set.members.iter().any(|m| m.path.id() == row.id))
    }

    /// The repo row `id` belongs to: its parent for a linked worktree row,
    /// else itself.
    pub(super) fn main_of(&self, id: RepoId) -> RepoId {
        self.row(id).and_then(DashboardRow::parent).unwrap_or(id)
    }

    /// The linked worktree rows of repo row `id`.
    pub(super) fn worktree_rows(&self, id: RepoId) -> impl Iterator<Item = &DashboardRow> {
        self.rows.iter().filter(move |r| r.parent() == Some(id))
    }
}

/// A row for linked worktree `wt` of repo row `parent`.
fn worktree_row(wt: &WorktreeInfo, parent: &DashboardRow, info: WorktreeRowInfo) -> DashboardRow {
    let status = if info.missing {
        RepoStatus::Missing
    } else {
        RepoStatus::Loading
    };
    DashboardRow {
        id: wt.path.id(),
        name: parent.name.clone(),
        path: wt.path.clone(),
        stale: status != RepoStatus::Missing,
        status,
        hidden: parent.hidden,
        forge: ForgeStatus::Unknown,
        ci: BranchCi::Unknown,
        forge_limit: None,
        forecast: ConflictForecast::default(),
        worktree: Some(info),
        linked_worktrees: 0,
        context_only: false,
    }
}

impl App {
    /// Reads the worktree list of every shown repo (not of the linked
    /// worktrees under them: their repo's list covers them).
    pub(super) fn load_all_worktrees(&self) {
        let ids: Vec<RepoId> = lock(&self.session)
            .as_ref()
            .map(|s| {
                s.rows
                    .iter()
                    .filter(|r| !r.hidden && r.parent().is_none())
                    .map(|r| r.id)
                    .collect()
            })
            .unwrap_or_default();
        for id in ids {
            self.load_worktrees(id);
        }
    }

    /// Reads the worktree list of the repo of row `repo` in the background
    /// (coalesced per repo).
    pub(super) fn load_worktrees(&self, repo: RepoId) {
        let path = {
            let mut guard = lock(&self.session);
            let Some(s) = guard.as_mut() else { return };
            let main = s.main_of(repo);
            let Some(path) = s
                .row(main)
                .filter(|r| r.path.as_path().is_dir())
                .map(|r| r.path.clone())
            else {
                return;
            };
            if !s.worktrees_pending.insert(main) {
                s.worktrees_reread.insert(main);
                return;
            }
            path
        };
        let reader = self.services.git.worktrees.clone();
        let me = self.me.clone();
        let asked = path.id();
        let spec = OpSpec::new(
            Some(asked),
            OpClass::Read,
            format!("Read worktrees of {}", path.name()),
        );
        let _handle = self.scheduler.submit(spec, move |_| {
            let result = reader.worktrees(&path);
            if let Some(app) = me.upgrade() {
                app.apply_worktrees(asked, result);
            }
            Ok(())
        });
    }

    /// Folds the worktree list read for row `asked` into the rows.
    pub(super) fn apply_worktrees(
        &self,
        asked: RepoId,
        result: Result<Vec<WorktreeInfo>, GitError>,
    ) {
        let again = {
            let mut guard = lock(&self.session);
            let Some(s) = guard.as_mut() else { return };
            s.worktrees_pending.remove(&asked);
            s.worktrees_reread.remove(&asked)
        };
        if again {
            self.load_worktrees(asked);
            return;
        }
        let list = match result {
            Ok(list) => list,
            Err(e) => {
                tracing::debug!(error = %e, repo = %asked, "cannot list worktrees");
                return;
            }
        };
        let Some(main) = list.first() else { return };
        let main_id = main.path.id();
        if main_id != asked {
            self.apply_linked_listing(asked, &list);
            return;
        }
        let Some(outcome) = self.fold_worktrees(main_id, list) else {
            return;
        };
        for id in &outcome.added {
            self.refresh_repo(*id);
        }
        if outcome.watch_changed {
            self.start_watching();
        }
        let mut changed = outcome.added;
        changed.extend(outcome.removed);
        changed.push(main_id);
        self.emit(AppEvent::RowsChanged(changed));
        self.emit(AppEvent::UiStateChanged);
    }

    /// Row `asked` is a linked worktree of another repo: folded under that
    /// repo when it is in the project (reading its list does), else marked
    /// "worktree of …".
    fn apply_linked_listing(&self, asked: RepoId, list: &[WorktreeInfo]) {
        let Some(main) = list.first() else { return };
        let main_id = main.path.id();
        let load_main = {
            let mut guard = lock(&self.session);
            let Some(s) = guard.as_mut() else { return };
            if s.row(main_id).is_some_and(|r| r.parent().is_none()) {
                true
            } else {
                let me = list.iter().find(|w| w.path.id() == asked);
                if let (Some(me), Some(row)) = (me, s.rows.iter_mut().find(|r| r.id == asked)) {
                    row.worktree = Some(WorktreeRowInfo {
                        parent: None,
                        main_name: main.path.name(),
                        label: me.label(),
                        locked: me.locked.clone(),
                        missing: me.is_missing(),
                        listed: true,
                    });
                }
                false
            }
        };
        if load_main {
            self.load_worktrees(main_id);
        } else {
            self.emit(AppEvent::RowsChanged(vec![asked]));
        }
    }

    /// Updates the rows of repo `main_id` from its worktree `list` (main
    /// first). `None` when the repo is no longer in the project.
    fn fold_worktrees(&self, main_id: RepoId, list: Vec<WorktreeInfo>) -> Option<FoldOutcome> {
        let auto_expand = self.settings.worktree_auto_expand;
        let mut guard = lock(&self.session);
        let s = guard.as_mut()?;
        let parent = s.row(main_id)?.clone();
        let listed: HashSet<RepoId> = s.def.repos.iter().map(|r| r.path.id()).collect();
        let known: Option<HashSet<RepoId>> = s
            .worktrees
            .get(&main_id)
            .map(|l| l.iter().map(|w| w.path.id()).collect());
        let linked: Vec<&WorktreeInfo> = list.iter().skip(1).collect();
        let ids: HashSet<RepoId> = linked.iter().map(|w| w.path.id()).collect();
        let mut out = FoldOutcome::default();
        // Rows of worktrees gone since: dropped (kept when listed in the
        // project file: they become plain repos, likely missing).
        let gone: Vec<RepoId> = s
            .worktree_rows(main_id)
            .filter(|r| !ids.contains(&r.id))
            .map(|r| r.id)
            .collect();
        for id in &gone {
            if listed.contains(id) {
                if let Some(row) = s.rows.iter_mut().find(|r| r.id == *id) {
                    row.worktree = None;
                }
            } else {
                s.rows.retain(|r| r.id != *id);
                s.ui.tabs.retain(|t| *t != TabState::Repo(*id));
                s.ui.selection.retain(|r| r != id);
                if s.ui.selected_repo == Some(*id) {
                    s.ui.selected_repo = Some(main_id);
                }
                s.ui.active_tab = s.ui.active_tab.min(s.ui.tabs.len().saturating_sub(1));
                forget_repo(s, *id);
            }
            out.watch_changed = true;
        }
        out.removed = gone;
        // Unlisted worktree rows are kept in Git's order, after the others.
        let mut kept: HashMap<RepoId, DashboardRow> = HashMap::new();
        s.rows.retain(|r| {
            if r.parent() == Some(main_id) && !listed.contains(&r.id) {
                kept.insert(r.id, r.clone());
                false
            } else {
                true
            }
        });
        for wt in &linked {
            let id = wt.path.id();
            let info = WorktreeRowInfo {
                parent: Some(main_id),
                main_name: main_name(&list),
                label: wt.label(),
                locked: wt.locked.clone(),
                missing: wt.is_missing(),
                listed: listed.contains(&id),
            };
            if let Some(row) = s.rows.iter_mut().find(|r| r.id == id) {
                // Listed in the project file: folded under its repo.
                if row.worktree.is_none() {
                    out.watch_changed = true;
                }
                if info.missing {
                    row.status = RepoStatus::Missing;
                }
                row.worktree = Some(info);
                continue;
            }
            let row = if let Some(mut row) = kept.remove(&id) {
                if info.missing && !row.worktree.as_ref().is_some_and(|w| w.missing) {
                    row.status = RepoStatus::Missing;
                } else if !info.missing && row.status == RepoStatus::Missing {
                    row.status = RepoStatus::Loading;
                    out.added.push(id);
                }
                row.worktree = Some(info);
                row
            } else {
                if !info.missing {
                    out.added.push(id);
                }
                out.watch_changed = true;
                worktree_row(wt, &parent, info)
            };
            s.rows.push(row);
        }
        if let Some(row) = s.rows.iter_mut().find(|r| r.id == main_id) {
            row.linked_worktrees = u32::try_from(linked.len()).unwrap_or(u32::MAX);
        }
        let appeared = known.is_some_and(|k| ids.iter().any(|id| !k.contains(id)));
        if auto_expand && appeared && !s.ui.expanded_worktrees.contains(&main_id) {
            s.ui.expanded_worktrees.push(main_id);
            out.watch_changed = true;
        }
        let linked_ids: Vec<RepoId> = ids.iter().copied().collect();
        if s.reconcile_sets(main_id, &linked_ids) {
            out.watch_changed = true;
        }
        s.worktrees.insert(main_id, list);
        Some(out)
    }

    /// Every worktree of the repo of row `repo` (main first), as last read.
    #[must_use]
    pub fn worktrees_of(&self, repo: RepoId) -> Vec<WorktreeInfo> {
        lock(&self.session)
            .as_ref()
            .and_then(|s| s.worktrees.get(&s.main_of(repo)).cloned())
            .unwrap_or_default()
    }

    /// The repo row `repo` belongs to (itself unless a linked worktree).
    #[must_use]
    pub fn main_repo_of(&self, repo: RepoId) -> RepoId {
        lock(&self.session)
            .as_ref()
            .map_or(repo, |s| s.main_of(repo))
    }

    /// Every worktree of the shown repos: each repo's main worktree, then
    /// its linked ones (Worktree Switcher).
    #[must_use]
    pub fn project_worktrees(&self) -> Vec<ProjectWorktree> {
        let guard = lock(&self.session);
        let Some(s) = guard.as_ref() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for main in s.rows.iter().filter(|r| !r.hidden && r.parent().is_none()) {
            let branch = |r: &DashboardRow| {
                r.worktree.as_ref().map(|w| w.label.clone()).or_else(|| {
                    r.summary()
                        .map(|sum| sum.head.branch().unwrap_or("detached").to_owned())
                })
            };
            out.push(ProjectWorktree {
                id: main.id,
                repo: main.id,
                name: main.name.clone(),
                label: branch(main).unwrap_or_default(),
                path: main.path.clone(),
                is_main: main.worktree.is_none(),
                locked: main.worktree.as_ref().and_then(|w| w.locked.clone()),
                missing: main.worktree.as_ref().is_some_and(|w| w.missing),
            });
            for wt in s.worktree_rows(main.id) {
                let Some(info) = &wt.worktree else { continue };
                out.push(ProjectWorktree {
                    id: wt.id,
                    repo: main.id,
                    name: wt.name.clone(),
                    label: info.label.clone(),
                    path: wt.path.clone(),
                    is_main: false,
                    locked: info.locked.clone(),
                    missing: info.missing,
                });
            }
        }
        out
    }

    /// The other worktree of the repo of row `repo` that has `branch`
    /// checked out, if any (Git refuses to check it out a second time).
    #[must_use]
    pub fn branch_worktree(&self, repo: RepoId, branch: &str) -> Option<ProjectWorktree> {
        let path = self.path_of(repo)?;
        let wt = self
            .worktrees_of(repo)
            .into_iter()
            .find(|w| w.branch() == Some(branch) && w.path.id() != path.id())?;
        let id = wt.path.id();
        let main = self.main_repo_of(repo);
        let name = self
            .all_rows()
            .into_iter()
            .find(|r| r.id == main)
            .map_or_else(|| wt.path.name(), |r| r.name);
        Some(ProjectWorktree {
            id,
            repo: main,
            name,
            label: wt.label(),
            is_main: wt.is_main,
            locked: wt.locked.clone(),
            missing: wt.is_missing(),
            path: wt.path,
        })
    }

    /// Lists or unlists the linked worktrees of repo row `repo` under it.
    pub(super) fn toggle_worktrees(&self, repo: RepoId) {
        self.update_ui_state(|ui| toggle(&mut ui.expanded_worktrees, repo));
        self.start_watching();
    }

    /// The linked worktree rows of the given repo rows, besides them
    /// (shared refs: what changes one's refs changes the others' sync).
    pub(super) fn with_worktree_rows(&self, ids: &[RepoId]) -> Vec<RepoId> {
        let guard = lock(&self.session);
        let Some(s) = guard.as_ref() else {
            return ids.to_vec();
        };
        let mut out = ids.to_vec();
        for id in ids {
            out.extend(s.worktree_rows(*id).filter(|r| !r.hidden).map(|r| r.id));
        }
        out
    }
}

/// Folder name of the main worktree of `list`.
fn main_name(list: &[WorktreeInfo]) -> String {
    list.first().map(|w| w.path.name()).unwrap_or_default()
}

/// What [`App::fold_worktrees`] changed.
#[derive(Default)]
struct FoldOutcome {
    /// New rows to read.
    added: Vec<RepoId>,
    /// Rows dropped.
    removed: Vec<RepoId>,
    /// The set of watched rows changed.
    watch_changed: bool,
}

/// Drops the per-repo session state of a row that is gone.
pub(super) fn forget_repo(s: &mut Session, repo: RepoId) {
    s.dependencies.remove(&repo);
    s.forge_checked.remove(&repo);
    s.forge_failed.remove(&repo);
    s.ci_watch.remove(&repo);
    s.forecast_inputs.remove(&repo);
    s.forecast_queue.retain(|r| *r != repo);
    s.worktrees.remove(&repo);
}
