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

/// UI state remembered per project.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
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
    /// Whether the sidebar is collapsed.
    pub sidebar_collapsed: bool,
    /// Height of the operations drawer in logical pixels.
    pub drawer_height: f32,
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
            sidebar_collapsed: false,
            drawer_height: 180.0,
        }
    }
}
