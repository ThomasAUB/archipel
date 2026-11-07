//! The repo dependency tree view-model (spec §9).
//!
//! Pure: [`build_tree`] turns the dashboard rows and the submodules read for
//! each repo into flat, render-ready [`TreeRow`]s (indentation, connectors,
//! collapsed nodes). Repos of the project that are submodules of another
//! project repo appear under their parent instead of twice.
//!
//! Dependency sources are pluggable: [`DependencySource`] is the extension
//! point, registered in `Extensions::dependencies` (submodules are the
//! built-in source; a fork can read `repo` manifests or package-manager
//! dependencies instead, or combine several).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use archipel_model::ports::{StatusReader, SubmoduleReader};
use archipel_model::submodule::SubmoduleNode;
use archipel_model::{GitError, Oid, RepoId, RepoPath};
use archipel_ops::GitPorts;

use crate::dashboard::DashboardRow;

/// Deepest nesting followed (guards against pathological or cyclic layouts).
pub const MAX_DEPTH: usize = 12;

/// The read-only Git ports a [`DependencySource`] may use.
#[derive(Clone)]
pub struct DependencyReaders {
    /// Submodule list.
    pub submodules: Arc<dyn SubmoduleReader>,
    /// Working copy status (a dependency's dirty state).
    pub status: Arc<dyn StatusReader>,
}

impl DependencyReaders {
    /// The readers among `git`.
    #[must_use]
    pub fn from_ports(git: &GitPorts) -> Self {
        Self {
            submodules: git.submodules.clone(),
            status: git.status.clone(),
        }
    }
}

/// Reads the dependencies of a repository as a tree of nodes.
///
/// Contract: blocking (called on a scheduler read worker, never on the UI
/// thread), may be called concurrently for different repos; a node whose
/// path is a repo of the project is shown as that repo.
pub trait DependencySource: Send + Sync {
    /// Dependencies of `repo`, nested.
    fn dependencies(
        &self,
        git: &DependencyReaders,
        repo: &RepoPath,
    ) -> Result<Vec<SubmoduleNode>, GitError>;
}

/// The built-in source: submodules at any depth, with their dirty state.
#[derive(Clone, Copy, Debug, Default)]
pub struct SubmoduleSource;

impl SubmoduleSource {
    fn collect(
        git: &DependencyReaders,
        repo: &RepoPath,
        depth: usize,
    ) -> Result<Vec<SubmoduleNode>, GitError> {
        if depth >= MAX_DEPTH {
            return Ok(Vec::new());
        }
        let infos = git.submodules.submodules(repo)?;
        Ok(infos
            .into_iter()
            .map(|info| {
                let present = info.initialized && info.path.as_path().is_dir();
                let (dirty, children) = if present {
                    let dirty = git
                        .status
                        .summary(&info.path)
                        .is_ok_and(|s| s.changes.is_dirty());
                    // A broken nested submodule must not hide its siblings.
                    let children = Self::collect(git, &info.path, depth + 1).unwrap_or_default();
                    (dirty, children)
                } else {
                    (false, Vec::new())
                };
                SubmoduleNode {
                    info,
                    dirty,
                    children,
                }
            })
            .collect())
    }
}

impl DependencySource for SubmoduleSource {
    fn dependencies(
        &self,
        git: &DependencyReaders,
        repo: &RepoPath,
    ) -> Result<Vec<SubmoduleNode>, GitError> {
        Self::collect(git, repo, 0)
    }
}

/// Submodule state of a tree node (absent for a top-level repo).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NodeState {
    /// Initialized (cloned and checked out).
    pub initialized: bool,
    /// Commit pinned by the parent.
    pub pinned: Option<Oid>,
    /// Commit checked out.
    pub checked_out: Option<Oid>,
    /// Checked out at another commit than the pinned one.
    pub out_of_sync: bool,
    /// Uncommitted changes.
    pub dirty: bool,
    /// The repository declaring this submodule.
    pub parent: RepoPath,
    /// Path relative to `parent` (for `git submodule update -- <path>`).
    pub rel_path: String,
}

/// One rendered line of the tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeRow {
    /// Stable key (collapse state): the node's repo id as text.
    pub key: String,
    /// Nesting depth (0 for roots).
    pub depth: usize,
    /// Connector text drawn before the name (`"│  ├─ "`).
    pub prefix: String,
    /// Display name.
    pub name: String,
    /// Working tree path.
    pub path: RepoPath,
    /// The project repo this node is, if it is one (it has a dashboard row).
    pub repo: Option<RepoId>,
    /// Submodule state when the node is a submodule of its parent.
    pub state: Option<NodeState>,
    /// The node has children (shown or collapsed).
    pub has_children: bool,
    /// Children are hidden.
    pub collapsed: bool,
}

/// Inputs of [`build_tree`].
pub struct TreeInput<'a> {
    /// Visible dashboard rows, in display order (after filters and sort).
    pub rows: &'a [DashboardRow],
    /// Submodules read per project repo.
    pub dependencies: &'a HashMap<RepoId, Vec<SubmoduleNode>>,
    /// Keys of collapsed nodes.
    pub collapsed: &'a [String],
    /// Project repos never shown (hidden).
    pub skip: &'a HashSet<RepoId>,
}

/// Builds the rows of the dependency tree.
#[must_use]
pub fn build_tree(input: &TreeInput<'_>) -> Vec<TreeRow> {
    let visible: HashMap<RepoId, &DashboardRow> = input.rows.iter().map(|r| (r.id, r)).collect();
    // Project repos reachable as a submodule of another visible project repo.
    let mut nested = HashSet::new();
    for row in input.rows {
        if let Some(children) = input.dependencies.get(&row.id) {
            mark_nested(children, row.id, &visible, &mut nested, 0);
        }
    }
    let mut out = Vec::new();
    let roots: Vec<&DashboardRow> = input
        .rows
        .iter()
        .filter(|r| !nested.contains(&r.id))
        .collect();
    let builder = Builder { input, visible };
    for row in roots {
        let mut seen = HashSet::from([row.id]);
        let children = input
            .dependencies
            .get(&row.id)
            .map_or(&[][..], Vec::as_slice);
        builder.push(
            &mut out,
            Node {
                name: row.name.clone(),
                path: row.path.clone(),
                repo: Some(row.id),
                state: None,
                children,
            },
            &mut Vec::new(),
            &mut seen,
        );
    }
    out
}

fn mark_nested(
    nodes: &[SubmoduleNode],
    owner: RepoId,
    visible: &HashMap<RepoId, &DashboardRow>,
    nested: &mut HashSet<RepoId>,
    depth: usize,
) {
    if depth >= MAX_DEPTH {
        return;
    }
    for n in nodes {
        let id = n.info.path.id();
        if id != owner && visible.contains_key(&id) {
            nested.insert(id);
        }
        mark_nested(&n.children, owner, visible, nested, depth + 1);
    }
}

struct Node<'a> {
    name: String,
    path: RepoPath,
    repo: Option<RepoId>,
    state: Option<NodeState>,
    children: &'a [SubmoduleNode],
}

struct Builder<'a> {
    input: &'a TreeInput<'a>,
    visible: HashMap<RepoId, &'a DashboardRow>,
}

impl Builder<'_> {
    /// Emits `node` then its children; `last` holds, per ancestor level,
    /// whether that ancestor was the last of its siblings.
    fn push(
        &self,
        out: &mut Vec<TreeRow>,
        node: Node<'_>,
        last: &mut Vec<bool>,
        seen: &mut HashSet<RepoId>,
    ) {
        let id = node.path.id();
        let key = id.to_string();
        let children: Vec<&SubmoduleNode> = node
            .children
            .iter()
            .filter(|c| !self.input.skip.contains(&c.info.path.id()))
            .collect();
        let collapsed = self.input.collapsed.contains(&key);
        out.push(TreeRow {
            key,
            depth: last.len(),
            prefix: prefix(last),
            name: node.name,
            path: node.path.clone(),
            repo: node.repo,
            state: node.state,
            has_children: !children.is_empty(),
            collapsed,
        });
        if collapsed || last.len() >= MAX_DEPTH {
            return;
        }
        let count = children.len();
        for (i, child) in children.into_iter().enumerate() {
            let child_id = child.info.path.id();
            if !seen.insert(child_id) {
                continue; // cycle
            }
            let repo = self.visible.get(&child_id).map(|r| r.id);
            let name = self
                .visible
                .get(&child_id)
                .map_or_else(|| child.info.name.clone(), |r| r.name.clone());
            last.push(i + 1 == count);
            self.push(
                out,
                Node {
                    name,
                    path: child.info.path.clone(),
                    repo,
                    state: Some(NodeState {
                        initialized: child.info.initialized,
                        pinned: child.info.pinned,
                        checked_out: child.info.checked_out,
                        out_of_sync: child.info.is_out_of_sync(),
                        dirty: child.dirty,
                        parent: node.path.clone(),
                        rel_path: child.info.name.clone(),
                    }),
                    children: &child.children,
                },
                last,
                seen,
            );
            last.pop();
            seen.remove(&child_id);
        }
    }
}

/// Connector text for a node whose ancestors' "is last sibling" flags are `last`.
fn prefix(last: &[bool]) -> String {
    let Some((own, ancestors)) = last.split_last() else {
        return String::new();
    };
    let mut s = String::new();
    for &done in ancestors {
        s.push_str(if done { "   " } else { "│  " });
    }
    s.push_str(if *own { "└─ " } else { "├─ " });
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dashboard::RepoStatus;
    use archipel_model::submodule::SubmoduleInfo;

    fn row(path: &str) -> DashboardRow {
        let path = RepoPath::new(path);
        DashboardRow {
            id: path.id(),
            name: path.name(),
            path,
            status: RepoStatus::Loading,
            stale: false,
            hidden: false,
            forge: crate::forge::ForgeStatus::Unknown,
            ci: crate::forge::BranchCi::Unknown,
            forge_limit: None,
        }
    }

    fn sub(parent: &str, rel: &str, children: Vec<SubmoduleNode>) -> SubmoduleNode {
        SubmoduleNode {
            info: SubmoduleInfo {
                name: rel.into(),
                path: RepoPath::new(format!("{parent}/{rel}")),
                initialized: true,
                pinned: None,
                checked_out: None,
            },
            dirty: false,
            children,
        }
    }

    fn build(
        rows: &[DashboardRow],
        deps: &HashMap<RepoId, Vec<SubmoduleNode>>,
        collapsed: &[String],
    ) -> Vec<TreeRow> {
        build_tree(&TreeInput {
            rows,
            dependencies: deps,
            collapsed,
            skip: &HashSet::new(),
        })
    }

    #[test]
    fn nests_submodules_with_connectors() {
        let app = row("/w/app");
        let deps = HashMap::from([(
            app.id,
            vec![
                sub("/w/app", "a", vec![sub("/w/app/a", "deep", vec![])]),
                sub("/w/app", "b", vec![]),
            ],
        )]);
        let rows = build(std::slice::from_ref(&app), &deps, &[]);
        let lines: Vec<String> = rows
            .iter()
            .map(|r| format!("{}{}", r.prefix, r.name))
            .collect();
        assert_eq!(lines, ["app", "├─ a", "│  └─ deep", "└─ b"]);
        assert_eq!(rows[1].state.as_ref().unwrap().rel_path, "a");
        assert_eq!(rows[2].depth, 2);
    }

    #[test]
    fn project_repo_that_is_a_submodule_appears_once_under_its_parent() {
        let app = row("/w/app");
        let lib = row("/w/app/lib");
        let deps = HashMap::from([(app.id, vec![sub("/w/app", "lib", vec![])])]);
        let rows = build(&[lib.clone(), app.clone()], &deps, &[]);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].repo, Some(app.id));
        assert_eq!(rows[1].repo, Some(lib.id));
        assert!(rows[1].state.is_some());
    }

    #[test]
    fn filtered_out_parent_leaves_its_project_child_as_a_root() {
        let app = row("/w/app");
        let lib = row("/w/app/lib");
        let deps = HashMap::from([(app.id, vec![sub("/w/app", "lib", vec![])])]);
        // Only `lib` passes the filters.
        let rows = build(std::slice::from_ref(&lib), &deps, &[]);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].depth, 0);
    }

    #[test]
    fn collapsed_nodes_hide_their_children() {
        let app = row("/w/app");
        let deps = HashMap::from([(app.id, vec![sub("/w/app", "a", vec![])])]);
        let rows = build(std::slice::from_ref(&app), &deps, &[app.id.to_string()]);
        assert_eq!(rows.len(), 1);
        assert!(rows[0].has_children && rows[0].collapsed);
    }

    #[test]
    fn hidden_project_repos_are_skipped() {
        let app = row("/w/app");
        let lib = row("/w/app/lib");
        let deps = HashMap::from([(app.id, vec![sub("/w/app", "lib", vec![])])]);
        let rows = build_tree(&TreeInput {
            rows: std::slice::from_ref(&app),
            dependencies: &deps,
            collapsed: &[],
            skip: &HashSet::from([lib.id]),
        });
        assert_eq!(rows.len(), 1);
        assert!(!rows[0].has_children);
    }
}
