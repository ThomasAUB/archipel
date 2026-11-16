//! Worktree Sets (spec §10, WORKTREES.md §5): the same branch checked out as
//! a worktree in several repos, under one Set Folder, for an agent run
//! across repos. Sets are personal state (the UI state), keyed by the
//! worktree paths; Git stays the source of truth for the worktrees.

use std::collections::HashMap;
use std::path::PathBuf;

use archipel_model::worktree::{
    LocationVars, SetMember, SetView, WorktreeAddRequest, WorktreeSet, WorktreeTarget,
    expand_location, path_slug,
};
use archipel_model::{RepoId, RepoPath};
use archipel_ops::batch::BatchTarget;
use archipel_ops::{
    BatchReport, Outcome, RemoveOptions, WorktreeLoss, WorktreePlan, WorktreeSetup, same_folder,
};

use super::session::Session;
use super::{App, AppError, AppEvent, lock};

/// A repo of a new Worktree Set.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SetMemberPlan {
    /// The repo row.
    pub repo: RepoId,
    /// Base of the new branch; `None` uses the existing branch.
    pub base: Option<String>,
}

/// A request to create (or complete) a Worktree Set.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewSetRequest {
    /// Set name.
    pub name: String,
    /// Branch checked out in every member.
    pub branch: String,
    /// The Set Folder: each member goes to `<folder>/<repo name>`.
    pub folder: PathBuf,
    /// The repos.
    pub members: Vec<SetMemberPlan>,
    /// Initialize submodules in the members.
    pub init_submodules: bool,
    /// Copy the repos' local files (`copy_to_worktrees`).
    pub copy_files: bool,
}

/// What a repo looks like for a new set (the dialog's rows).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SetRepoInfo {
    /// The repo row.
    pub repo: RepoId,
    /// Its name.
    pub name: String,
    /// Its default branch, else its current one.
    pub default_base: Option<String>,
    /// Its local branches, for the base dropdown.
    pub branches: Vec<String>,
    /// The set's branch exists already (used as is).
    pub branch_exists: bool,
    /// The set's branch is checked out in this worktree (the repo cannot
    /// take part: Git refuses a branch checked out twice).
    pub checked_out_in: Option<RepoPath>,
    /// It declares submodules.
    pub has_submodules: bool,
}

/// How a set operation went, per repo (the dialogs' reports).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SetReport {
    /// Set name.
    pub name: String,
    /// Set Folder.
    pub folder: PathBuf,
    /// Per repo: its row, its name, and the error when it failed.
    pub results: Vec<(RepoId, String, Result<(), String>)>,
}

impl SetReport {
    /// The repos that failed.
    #[must_use]
    pub fn failed(&self) -> Vec<RepoId> {
        self.results
            .iter()
            .filter(|(_, _, r)| r.is_err())
            .map(|(id, _, _)| *id)
            .collect()
    }
}

/// A member of a set as the Finish dialog shows it.
#[derive(Clone, Debug)]
pub struct SetMemberState {
    /// Its row (`None` when Git no longer lists it).
    pub row: Option<RepoId>,
    /// The repo row.
    pub repo: RepoId,
    /// Repo name.
    pub name: String,
    /// Folder.
    pub path: RepoPath,
    /// What removing it would lose (an error when it cannot be read).
    pub loss: Result<WorktreeLoss, String>,
}

impl Session {
    /// The rows of the members of `set` (in the set's order).
    pub(super) fn set_rows<'a>(
        &'a self,
        set: &'a WorktreeSet,
    ) -> impl Iterator<Item = &'a crate::dashboard::DashboardRow> + 'a {
        set.members.iter().filter_map(|m| self.row(m.path.id()))
    }

    /// Drops the members of the sets of repo `main` that Git no longer
    /// lists (`ids`: its linked worktrees), and the sets left empty.
    pub(super) fn reconcile_sets(&mut self, main: RepoId, ids: &[RepoId]) -> bool {
        let mut changed = false;
        for set in &mut self.ui.worktree_sets {
            let before = set.members.len();
            set.members
                .retain(|m| m.repo != main || ids.contains(&m.path.id()));
            changed |= set.members.len() != before;
        }
        let before = self.ui.worktree_sets.len();
        self.ui.worktree_sets.retain(|s| !s.members.is_empty());
        if self.ui.worktree_sets.len() != before {
            changed = true;
            if let SetView::Set(name) = &self.ui.set_view
                && !self.ui.worktree_sets.iter().any(|s| s.name == *name)
            {
                self.ui.set_view = SetView::All;
            }
        }
        changed
    }
}

impl App {
    /// The Set Folder a set named `name` gets from the
    /// `worktree_set_location` template: its members go to
    /// `<folder>/<repo name>` (the template's last `{repo}` component).
    #[must_use]
    pub fn default_set_folder(&self, name: &str) -> Option<PathBuf> {
        let guard = lock(&self.session);
        let s = guard.as_ref()?;
        let archipel_model::project::ProjectLocation::SharedFile(file) = &s.location;
        let base = file.parent()?;
        let template = self.settings.worktree_set_location.as_str();
        let folder = template
            .strip_suffix("/{repo}")
            .or_else(|| template.strip_suffix("\\{repo}"))
            .unwrap_or(template);
        let vars = LocationVars {
            project: &s.def.name,
            repo: "",
            branch: name,
            set: name,
        };
        Some(expand_location(folder, &vars, base))
    }

    /// The Worktree Sets of the project.
    #[must_use]
    pub fn worktree_sets(&self) -> Vec<WorktreeSet> {
        lock(&self.session)
            .as_ref()
            .map(|s| s.ui.worktree_sets.clone())
            .unwrap_or_default()
    }

    /// The set shown on the dashboard, if any.
    #[must_use]
    pub fn active_set(&self) -> Option<WorktreeSet> {
        lock(&self.session)
            .as_ref()
            .and_then(|s| s.ui.active_set().cloned())
    }

    /// How each shown repo stands for a set on `branch` (the New Worktree
    /// Set dialog). Blocking (reads branches): run it off the UI thread.
    #[must_use]
    pub fn set_repo_infos(&self, branch: &str) -> Vec<SetRepoInfo> {
        let repos: Vec<(RepoId, String, RepoPath, bool)> = {
            let guard = lock(&self.session);
            let Some(s) = guard.as_ref() else {
                return Vec::new();
            };
            s.rows
                .iter()
                .filter(|r| !r.hidden && r.parent().is_none() && r.worktree.is_none())
                .filter_map(|r| {
                    let summary = r.summary()?;
                    Some((r.id, r.name.clone(), r.path.clone(), summary.has_submodules))
                })
                .collect()
        };
        let git = &self.services.git;
        repos
            .into_iter()
            .map(|(id, name, path, has_submodules)| {
                let branches = git.refs.branches(&path).unwrap_or_default();
                let default_base = archipel_stats::detect_default_branch(
                    git.refs.as_ref(),
                    &path,
                    &branches,
                    None,
                )
                .ok()
                .flatten()
                .map(|d| d.name)
                .or_else(|| branches.iter().find(|b| b.is_head).map(|b| b.name.clone()));
                let local: Vec<String> = branches
                    .iter()
                    .filter(|b| !b.is_remote)
                    .map(|b| b.name.clone())
                    .collect();
                let checked_out_in = git
                    .worktrees
                    .worktrees(&path)
                    .unwrap_or_default()
                    .into_iter()
                    .find(|w| w.branch() == Some(branch))
                    .map(|w| w.path);
                SetRepoInfo {
                    repo: id,
                    name,
                    default_base,
                    branch_exists: local.iter().any(|b| b == branch),
                    branches: local,
                    checked_out_in,
                    has_submodules,
                }
            })
            .collect()
    }

    /// Creates the worktrees of a set in parallel, one batch (spec §10);
    /// creating again with the same name adds to the set (Retry failed).
    /// The report arrives as [`AppEvent::WorktreeSetCreated`].
    pub(super) fn create_worktree_set(&self, request: NewSetRequest) -> Result<(), AppError> {
        let name = request.name.trim().to_owned();
        if name.is_empty() || request.branch.trim().is_empty() {
            return Err(AppError::InvalidInput(
                "a set needs a name and a branch".into(),
            ));
        }
        let plans: Vec<(WorktreePlan, String)> = {
            let guard = lock(&self.session);
            let s = guard.as_ref().ok_or(AppError::NoProject)?;
            if let Some(other) = s.ui.worktree_sets.iter().find(|x| x.name == name)
                && (other.branch != request.branch || other.folder != request.folder)
            {
                return Err(AppError::InvalidInput(format!(
                    "a set named “{name}” exists on another branch or folder"
                )));
            }
            let def_copy: HashMap<RepoId, Vec<String>> = s
                .def
                .repos
                .iter()
                .map(|r| (r.path.id(), r.copy_to_worktrees.clone()))
                .collect();
            request
                .members
                .iter()
                .filter_map(|m| {
                    let row = s.row(m.repo)?;
                    let path = request.folder.join(path_slug(&row.name));
                    let target = match &m.base {
                        Some(base) => WorktreeTarget::NewBranch {
                            name: request.branch.clone(),
                            base: Some(base.clone()),
                        },
                        None => WorktreeTarget::Branch(request.branch.clone()),
                    };
                    let copy = if request.copy_files {
                        def_copy.get(&m.repo).cloned().unwrap_or_default()
                    } else {
                        Vec::new()
                    };
                    Some((
                        WorktreePlan {
                            target: BatchTarget {
                                id: m.repo,
                                path: row.path.clone(),
                            },
                            request: WorktreeAddRequest { path, target },
                            setup: WorktreeSetup {
                                copy,
                                init_submodules: request.init_submodules,
                                batch: None,
                            },
                        },
                        row.name.clone(),
                    ))
                })
                .collect()
        };
        let names: HashMap<RepoId, (String, PathBuf)> = plans
            .iter()
            .map(|(p, n)| (p.target.id, (n.clone(), p.request.path.clone())))
            .collect();
        let plans: Vec<WorktreePlan> = plans.into_iter().map(|(p, _)| p).collect();
        let batches = self.batches.clone();
        let me = self.me.clone();
        self.scheduler.runtime().spawn(async move {
            let report = batches.add_worktree_all(plans).await;
            if let Some(app) = me.upgrade() {
                app.finish_set_creation(name, request.branch, request.folder, &names, &report);
            }
        });
        Ok(())
    }

    /// Records the members created, shows the set, reports per repo.
    fn finish_set_creation(
        &self,
        name: String,
        branch: String,
        folder: PathBuf,
        names: &HashMap<RepoId, (String, PathBuf)>,
        report: &BatchReport<Vec<String>>,
    ) {
        // The lists first: the set's members must be rows when it shows.
        for repo in report.per_repo.iter().map(|(r, _)| *r) {
            if let Some(path) = self.path_of(repo) {
                let list = self.services.git.worktrees.worktrees(&path);
                self.apply_worktrees(repo, list);
            }
        }
        let mut results = Vec::new();
        let mut created = Vec::new();
        for (repo, outcome) in &report.per_repo {
            let (repo_name, path) = names.get(repo).cloned().unwrap_or_default();
            let result = match outcome {
                Outcome::Done(_) => {
                    // As Git spells it: the member is found by its row.
                    let path = self
                        .worktrees_of(*repo)
                        .into_iter()
                        .find(|w| same_folder(w.path.as_path(), &path))
                        .map_or_else(|| RepoPath::new(path), |w| w.path);
                    created.push(SetMember { repo: *repo, path });
                    Ok(())
                }
                Outcome::Failed(e) => Err(e.to_string()),
                Outcome::Skipped(why) => Err(why.clone()),
            };
            results.push((*repo, repo_name, result));
        }
        if !created.is_empty() {
            self.update_ui_state(|ui| {
                match ui.worktree_sets.iter_mut().find(|s| s.name == name) {
                    Some(set) => {
                        for m in created {
                            if !set.members.iter().any(|x| x.path.id() == m.path.id()) {
                                set.members.push(m);
                            }
                        }
                    }
                    None => ui.worktree_sets.push(WorktreeSet {
                        name: name.clone(),
                        branch,
                        folder: folder.clone(),
                        members: created,
                    }),
                }
                ui.set_view = SetView::Set(name.clone());
            });
            self.save_ui_state_now();
            self.start_watching();
        }
        self.emit(AppEvent::WorktreeSetCreated(SetReport {
            name,
            folder,
            results,
        }));
    }

    /// Each member of set `name` and what removing it would lose (the
    /// Finish dialog). Blocking: run it off the UI thread.
    #[must_use]
    pub fn set_member_states(&self, name: &str) -> Vec<SetMemberState> {
        let members: Vec<(SetMember, Option<RepoId>, String)> = {
            let guard = lock(&self.session);
            let Some(s) = guard.as_ref() else {
                return Vec::new();
            };
            let Some(set) = s.ui.worktree_sets.iter().find(|x| x.name == name) else {
                return Vec::new();
            };
            set.members
                .iter()
                .map(|m| {
                    let row = s.row(m.path.id()).map(|r| r.id);
                    let repo = s
                        .row(m.repo)
                        .map_or_else(|| m.path.name(), |r| r.name.clone());
                    (m.clone(), row, repo)
                })
                .collect()
        };
        members
            .into_iter()
            .map(|(m, row, name)| SetMemberState {
                loss: match row {
                    Some(_) => self
                        .actions
                        .worktree_loss(&m.path)
                        .map_err(|e| e.to_string()),
                    None => Err("not a worktree anymore".into()),
                },
                row,
                repo: m.repo,
                name,
                path: m.path,
            })
            .collect()
    }

    /// Removes the chosen members of set `name` as one batch (with their
    /// branch when asked), then the Set Folder when empty; members left
    /// keep the set. Reported as [`AppEvent::WorktreeSetFinished`].
    pub(super) fn finish_worktree_set(
        &self,
        name: &str,
        removals: Vec<(RepoId, RemoveOptions)>,
    ) -> Result<(), AppError> {
        let (targets, plan, folder, names) = {
            let guard = lock(&self.session);
            let s = guard.as_ref().ok_or(AppError::NoProject)?;
            let set =
                s.ui.worktree_sets
                    .iter()
                    .find(|x| x.name == name)
                    .ok_or_else(|| AppError::NotFound(name.to_owned()))?;
            let mut targets = Vec::new();
            let mut plan = HashMap::new();
            let mut names = HashMap::new();
            for (row, options) in removals {
                let Some(member) = set.members.iter().find(|m| m.path.id() == row) else {
                    continue;
                };
                let Some(main) = s.row(member.repo) else {
                    continue;
                };
                targets.push(BatchTarget {
                    id: member.repo,
                    path: main.path.clone(),
                });
                plan.insert(member.repo, (member.path.clone(), options));
                names.insert(member.repo, main.name.clone());
            }
            (targets, plan, set.folder.clone(), names)
        };
        let batches = self.batches.clone();
        let me = self.me.clone();
        let name = name.to_owned();
        self.scheduler.runtime().spawn(async move {
            let report = batches.remove_worktree_all(&targets, plan).await;
            if let Some(app) = me.upgrade() {
                for t in &targets {
                    let list = app.services.git.worktrees.worktrees(&t.path);
                    app.apply_worktrees(t.id, list);
                }
                // Only an empty folder goes: nothing of the user's is lost.
                if std::fs::remove_dir(&folder).is_ok() {
                    tracing::info!(folder = %folder.display(), "set folder removed");
                }
                let results = report
                    .per_repo
                    .iter()
                    .map(|(repo, outcome)| {
                        let n = names.get(repo).cloned().unwrap_or_default();
                        let r = match outcome {
                            Outcome::Done(()) => Ok(()),
                            Outcome::Failed(e) => Err(e.to_string()),
                            Outcome::Skipped(why) => Err(why.clone()),
                        };
                        (*repo, n, r)
                    })
                    .collect();
                app.save_ui_state_now();
                app.emit(AppEvent::WorktreeSetFinished(SetReport {
                    name,
                    folder,
                    results,
                }));
                app.emit(AppEvent::UiStateChanged);
            }
        });
        Ok(())
    }

    /// Forgets set `name` (its worktrees stay).
    pub(super) fn forget_worktree_set(&self, name: &str) {
        self.update_ui_state(|ui| {
            ui.worktree_sets.retain(|s| s.name != name);
            if ui.set_view == SetView::Set(name.to_owned()) {
                ui.set_view = SetView::All;
            }
        });
        self.start_watching();
    }

    /// Renames set `old` to `new` (refused when taken).
    pub(super) fn rename_worktree_set(&self, old: &str, new: &str) -> Result<(), AppError> {
        let new = new.trim();
        if new.is_empty() {
            return Err(AppError::InvalidInput("a set needs a name".into()));
        }
        if self.worktree_sets().iter().any(|s| s.name == new) {
            return Err(AppError::InvalidInput(format!(
                "a set named “{new}” exists"
            )));
        }
        self.update_ui_state(|ui| {
            for set in ui.worktree_sets.iter_mut().filter(|s| s.name == old) {
                new.clone_into(&mut set.name);
            }
            if ui.set_view == SetView::Set(old.to_owned()) {
                ui.set_view = SetView::Set(new.to_owned());
            }
        });
        Ok(())
    }

    /// Saves the UI state now (sets are deliberate choices, not layout).
    pub(super) fn save_ui_state_now(&self) {
        let state = lock(&self.session)
            .as_ref()
            .map(|s| (s.id.clone(), s.ui.clone()));
        if let Some((id, ui)) = state {
            self.save_ui_state(&id, &ui);
        }
    }
}
