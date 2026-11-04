//! Per-project UI state (spec §2). Never stored in the shared project file.

use serde::{Deserialize, Serialize};

use crate::ids::RepoId;

/// A tab of the main area.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TabState {
    /// The project dashboard (always first).
    Dashboard,
    /// A repository view.
    Repo(RepoId),
}

/// Sort direction.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SortDirection {
    /// Ascending.
    #[default]
    Ascending,
    /// Descending.
    Descending,
}

/// UI state remembered per project: the *private* project state, never
/// written to the shared `archipel.toml`.
///
/// Fields added after the first release are `#[serde(default)]` so states
/// saved by older builds still load.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UiState {
    /// Open tabs, in order.
    pub tabs: Vec<TabState>,
    /// Index of the active tab.
    pub active_tab: usize,
    /// Selected repository in the dashboard / sidebar.
    pub selected_repo: Option<RepoId>,
    /// Ids of the active dashboard filters.
    pub filters: Vec<String>,
    /// Id of the sort column.
    pub sort_column: Option<String>,
    /// Sort direction.
    pub sort_direction: SortDirection,
    /// Sidebar width in logical pixels.
    pub sidebar_width: f32,
    /// Width of a repo view's refs pane in logical pixels.
    pub refs_width: f32,
    /// Width of a repo view's commit details pane in logical pixels.
    pub details_width: f32,
    /// Width of a repo view's changed-files pane in logical pixels.
    pub changes_width: f32,
    /// Whether the sidebar is collapsed.
    pub sidebar_collapsed: bool,
    /// Height of the operations drawer in logical pixels.
    pub drawer_height: f32,
    /// Extra repos selected for batch actions (Ctrl+click), besides
    /// `selected_repo`.
    pub selection: Vec<RepoId>,
    /// Hidden repos (spec §1): not shown, watched, refreshed nor targeted.
    pub hidden_repos: Vec<RepoId>,
    /// List hidden repos (dimmed) in the dashboard.
    pub show_hidden: bool,
    /// Show the repo dependency tree instead of the flat list (spec §9).
    pub show_dependency_tree: bool,
    /// Collapsed nodes of the dependency tree (absolute paths).
    pub collapsed_nodes: Vec<String>,
    /// Collapsed category groups of the sidebar (spec §1); the empty name
    /// is the "Uncategorized" group.
    pub collapsed_categories: Vec<String>,
    /// Legacy personal accent color (`#rrggbb`), still honored over the
    /// project's; cleared when the color is changed (spec §2).
    pub accent: Option<String>,
    /// Legacy personal icon / emoji; cleared when the icon is changed.
    pub icon: Option<String>,
}

impl UiState {
    /// Whether `repo` is hidden.
    #[must_use]
    pub fn is_hidden(&self, repo: RepoId) -> bool {
        self.hidden_repos.contains(&repo)
    }

    /// The repos batch actions apply to: the multi-selection, else the
    /// selected repo, else none (meaning "all").
    #[must_use]
    pub fn batch_selection(&self) -> Vec<RepoId> {
        let mut out: Vec<RepoId> = self.selected_repo.into_iter().collect();
        for r in &self.selection {
            if !out.contains(r) {
                out.push(*r);
            }
        }
        out
    }
}

impl Default for UiState {
    fn default() -> Self {
        Self {
            tabs: vec![TabState::Dashboard],
            active_tab: 0,
            selected_repo: None,
            filters: Vec::new(),
            sort_column: None,
            sort_direction: SortDirection::Ascending,
            sidebar_width: 240.0,
            refs_width: 220.0,
            details_width: 460.0,
            changes_width: 380.0,
            sidebar_collapsed: false,
            drawer_height: 180.0,
            selection: Vec::new(),
            hidden_repos: Vec::new(),
            show_hidden: false,
            show_dependency_tree: false,
            collapsed_nodes: Vec::new(),
            collapsed_categories: Vec::new(),
            accent: None,
            icon: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_saved_by_an_older_build_still_loads() {
        let old = r#"{"tabs":["Dashboard"],"active_tab":0,"selected_repo":null,"filters":[],
            "sort_column":null,"sort_direction":"Ascending","sidebar_width":240.0,
            "sidebar_collapsed":false,"drawer_height":180.0}"#;
        let state: UiState = serde_json::from_str(old).unwrap();
        assert!(state.hidden_repos.is_empty());
        assert!(!state.show_dependency_tree);
    }

    #[test]
    fn batch_selection_merges_primary_and_extra() {
        let state = UiState {
            selected_repo: Some(RepoId(1)),
            selection: vec![RepoId(2), RepoId(1)],
            ..UiState::default()
        };
        assert_eq!(state.batch_selection(), vec![RepoId(1), RepoId(2)]);
    }
}
