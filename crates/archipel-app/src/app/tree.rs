//! The repo dependency tree (spec §9): submodules read in the background
//! while the tree is shown, the tree view-model, and submodule updates.

use std::collections::HashSet;

use archipel_model::requests::{SubmoduleAdd, SubmoduleUpdate};
use archipel_model::{RepoId, RepoPath};
use archipel_ops::{OpClass, OpError, OpSpec};

use super::{App, AppError, AppEvent, lock};
use crate::Command;
use crate::deptree::{TreeInput, TreeRow, build_tree};

impl App {
    /// The submodule commands of `dispatch`.
    pub(super) fn dispatch_submodule(&self, command: Command) -> Result<(), AppError> {
        match command {
            Command::UpdateSubmodules {
                parent,
                paths,
                init,
                recursive,
            } => self.update_submodules(
                parent,
                SubmoduleUpdate {
                    paths,
                    init,
                    recursive,
                },
            ),
            Command::OpenSubmodule(path) => {
                let id = path.id();
                if !self.all_rows().iter().any(|r| r.id == id) {
                    self.add_repos(std::slice::from_ref(&path))?;
                }
                self.set_hidden(&[id], false);
                self.dispatch(Command::OpenRepoTab(id))?;
            }
            Command::AddSubmodule { repo, url, path } => self.add_submodule(repo, &url, &path)?,
            Command::RemoveSubmodule { parent, path } => self.remove_submodule(parent, path),
            _ => {}
        }
        Ok(())
    }

    /// The dependency tree of the visible rows (spec §9); empty when the
    /// tree is not shown. Nodes whose submodules are still being read show
    /// without children until [`AppEvent::TreeChanged`].
    #[must_use]
    pub fn dependency_tree(&self) -> Vec<TreeRow> {
        let guard = lock(&self.session);
        let Some(s) = guard.as_ref().filter(|s| s.ui.show_dependency_tree) else {
            return Vec::new();
        };
        // Linked worktrees listed (expanded, matching, in the set) go under
        // their repo, as in the flat list.
        let rows = Self::visible_rows_of(&self.ext, s);
        let skip: HashSet<RepoId> = if s.ui.show_hidden {
            HashSet::new()
        } else {
            s.ui.hidden_repos.iter().copied().collect()
        };
        build_tree(&TreeInput {
            rows: &rows,
            dependencies: &s.dependencies,
            collapsed: &s.ui.collapsed_nodes,
            skip: &skip,
        })
    }

    /// Reads the submodules of every shown repo, when the tree is shown.
    pub(super) fn load_tree_if_shown(&self) {
        let ids: Vec<RepoId> = lock(&self.session)
            .as_ref()
            .filter(|s| s.ui.show_dependency_tree)
            .map(|s| {
                s.rows
                    .iter()
                    .filter(|r| !r.hidden && r.parent().is_none())
                    .map(|r| r.id)
                    .collect()
            })
            .unwrap_or_default();
        for id in ids {
            self.load_tree(id);
        }
    }

    /// Reads the submodules of one repo in the background, when the tree is
    /// shown (coalesced: at most one pending read per repo).
    pub(super) fn load_tree(&self, repo: RepoId) {
        let path = {
            let mut guard = lock(&self.session);
            let Some(s) = guard.as_mut().filter(|s| s.ui.show_dependency_tree) else {
                return;
            };
            let Some(path) = s
                .rows
                .iter()
                .find(|r| r.id == repo && !r.hidden)
                .map(|r| r.path.clone())
            else {
                return;
            };
            if !path.as_path().is_dir() || !s.tree_pending.insert(repo) {
                return;
            }
            path
        };
        let source = self.ext.dependencies.clone();
        let git = crate::deptree::DependencyReaders::from_ports(&self.services.git);
        let me = self.me.clone();
        let spec = OpSpec::new(
            Some(repo),
            OpClass::Read,
            format!("Read submodules of {}", path.name()),
        );
        let _handle = self.scheduler.submit(spec, move |_| {
            let result = source.dependencies(&git, &path);
            if let Some(app) = me.upgrade() {
                let changed = {
                    let mut guard = lock(&app.session);
                    guard.as_mut().is_some_and(|s| {
                        s.tree_pending.remove(&repo);
                        let nodes = result.as_ref().cloned().unwrap_or_default();
                        s.dependencies.insert(repo, nodes.clone()) != Some(nodes)
                    })
                };
                if changed {
                    app.emit(AppEvent::TreeChanged);
                }
            }
            result.map(drop).map_err(OpError::from)
        });
    }

    /// Inits or updates submodules of `parent` in the background, then
    /// refreshes the rows and the tree.
    pub(super) fn update_submodules(&self, parent: RepoPath, request: SubmoduleUpdate) {
        let writer = self.services.git.submodule_writer.clone();
        let me = self.me.clone();
        let what = if request.paths.is_empty() {
            parent.name()
        } else {
            request.paths.join(", ")
        };
        let verb = if request.init { "Init" } else { "Update" };
        let spec = OpSpec::new(
            Some(parent.id()),
            OpClass::Network,
            format!("{verb} submodule {what}"),
        );
        let _handle = self.scheduler.submit(spec, move |ctx| {
            let result = writer.update_submodules(&parent, &request, ctx, ctx.cancel());
            if let Some(app) = me.upgrade() {
                app.refresh_all();
                app.load_tree_if_shown();
            }
            result.map_err(OpError::from)
        });
    }

    /// `git submodule add` of `url` at `path` in the working tree of row
    /// `repo`, on the network queue (it clones). Refused when the URL or the
    /// path is empty, or the path leaves the working tree.
    pub(super) fn add_submodule(
        &self,
        repo: RepoId,
        url: &str,
        path: &str,
    ) -> Result<(), AppError> {
        let parent = self
            .path_of(repo)
            .ok_or_else(|| AppError::NotFound(repo.to_string()))?;
        let url = url.trim().to_owned();
        let path = path.trim().replace('\\', "/");
        let path = path.trim_matches('/').to_owned();
        if url.is_empty() {
            return Err(OpError::Refused("give the URL of the repository to add".into()).into());
        }
        if path.is_empty()
            || path
                .split('/')
                .any(|c| c.is_empty() || c == "." || c == "..")
            || std::path::Path::new(&path).is_absolute()
            || path.contains(':')
        {
            return Err(OpError::Refused(format!(
                "“{path}” is not a path inside {}",
                parent.name()
            ))
            .into());
        }
        let writer = self.services.git.submodule_writer.clone();
        let me = self.me.clone();
        let spec = OpSpec::new(
            Some(parent.id()),
            OpClass::Network,
            format!("Add submodule {path}"),
        );
        let request = SubmoduleAdd { url, path };
        let _handle = self.scheduler.submit(spec, move |ctx| {
            let result = writer.add_submodule(&parent, &request, ctx, ctx.cancel());
            if let Some(app) = me.upgrade() {
                if result.is_ok() {
                    app.emit(AppEvent::Notice(format!(
                        "Added submodule {} to {}: commit to record it",
                        request.path,
                        parent.name()
                    )));
                }
                app.refresh_all();
                app.load_tree_if_shown();
            }
            result.map_err(OpError::from)
        });
        Ok(())
    }

    /// Removes the submodule at `path` from `parent` in the background
    /// (see [`Command::RemoveSubmodule`](crate::Command::RemoveSubmodule)),
    /// then refreshes the rows and the tree.
    pub(super) fn remove_submodule(&self, parent: RepoPath, path: String) {
        let writer = self.services.git.submodule_writer.clone();
        let me = self.me.clone();
        let spec = OpSpec::new(
            Some(parent.id()),
            OpClass::Write,
            format!("Remove submodule {path}"),
        );
        let _handle = self.scheduler.submit(spec, move |ctx| {
            let result = writer.remove_submodule(&parent, &path, ctx.cancel());
            if let Some(app) = me.upgrade() {
                if result.is_ok() {
                    app.emit(AppEvent::Notice(format!(
                        "Removed submodule {path} from {}: commit to record it",
                        parent.name()
                    )));
                }
                app.refresh_all();
                app.load_tree_if_shown();
            }
            result.map_err(OpError::from)
        });
    }
}
