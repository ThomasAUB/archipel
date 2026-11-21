//! Per-project UI state (spec §2). Never stored in the shared project file.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::ids::RepoId;
use crate::worktree::{SetView, WorktreeSet};

/// A tab of the main area.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TabState {
    /// The project dashboard (always first).
    Dashboard,
    /// A repository view.
    Repo(RepoId),
}

/// What the current view is about: the scope of every action on "the
/// selected repos" (fetch, statistics, issues, branches…). The active tab
/// dictates it, so an action never reaches past what is on screen: a repo
/// tab focuses its repo alone, the dashboard its batch selection.
///
/// Built only by [`UiState::focus`]. Actions read the dashboard selection
/// through it, never through [`UiState::batch_selection`], which is for
/// drawing the dashboard and the sidebar.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Focus {
    /// The dashboard, on its batch selection (empty: every repo).
    Dashboard(Vec<RepoId>),
    /// A repo tab, on its repo.
    Repo(RepoId),
}

impl Default for Focus {
    /// The dashboard with nothing selected: every repo.
    fn default() -> Self {
        Self::Dashboard(Vec::new())
    }
}

impl Focus {
    /// The repos in focus as batch commands take them: the repo tab's
    /// repo, else the dashboard selection, empty meaning "all" (the core
    /// resolves it, `App::resolve_targets`).
    #[must_use]
    pub fn targets(&self) -> Vec<RepoId> {
        match self {
            Self::Dashboard(selected) => selected.clone(),
            Self::Repo(id) => vec![*id],
        }
    }

    /// The one repo in focus: the repo tab's, else the dashboard's only
    /// selected repo.
    #[must_use]
    pub fn repo(&self) -> Option<RepoId> {
        match self {
            Self::Dashboard(selected) => match selected[..] {
                [only] => Some(only),
                _ => None,
            },
            Self::Repo(id) => Some(*id),
        }
    }

    /// The repo of the active tab, when it is a repo tab.
    #[must_use]
    pub fn tab_repo(&self) -> Option<RepoId> {
        match self {
            Self::Dashboard(_) => None,
            Self::Repo(id) => Some(*id),
        }
    }

    /// Whether the dashboard is the view.
    #[must_use]
    pub fn is_dashboard(&self) -> bool {
        matches!(self, Self::Dashboard(_))
    }

    /// Whether repos were picked explicitly: a repo tab, or a dashboard
    /// selection (not the implicit "all").
    #[must_use]
    pub fn is_explicit(&self) -> bool {
        match self {
            Self::Dashboard(selected) => !selected.is_empty(),
            Self::Repo(_) => true,
        }
    }
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

/// An optional column of a repo view's commit graph, after the message.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum GraphColumn {
    /// Author name.
    Author,
    /// Commit time, relative to now.
    Age,
    /// Commit date and time, to the minute.
    Date,
    /// Short commit id.
    Commit,
}

impl GraphColumn {
    /// Every optional column, in display order.
    pub const ALL: [Self; 4] = [Self::Author, Self::Age, Self::Date, Self::Commit];

    /// Title shown in the graph header and its menu.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Author => "Author",
            Self::Age => "Age",
            Self::Date => "Date",
            Self::Commit => "Commit",
        }
    }
}

/// UI state remembered per project: the *private* project state, never
/// written to the shared `archipel.toml`.
///
/// Fields added after the first release are `#[serde(default)]` so states
/// saved by older builds still load.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
// Independent view toggles, each saved as is.
#[allow(clippy::struct_excessive_bools)]
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
    /// Whether a repo view's refs pane (branches, tags, submodules) is
    /// collapsed to a thin strip, shared by all repo views.
    pub refs_collapsed: bool,
    /// Width of a repo view's details pane in logical pixels.
    pub details_width: f32,
    /// Width of the dashboard's quick edit pane in logical pixels.
    pub quick_edit_width: f32,
    /// Relative widths of the dashboard columns the user resized, by
    /// column id; the others keep their default width.
    pub column_widths: BTreeMap<String, f32>,
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
    /// Collapsed sections of a repo view's refs pane, by title (`LOCAL`,
    /// `REMOTE`, `TAGS`, `SUBMODULES`), shared by all repo views.
    pub collapsed_ref_sections: Vec<String>,
    /// Commit graph columns hidden in the repo views; the exact date and
    /// time is hidden until asked for.
    pub hidden_graph_columns: Vec<GraphColumn>,
    /// Categories the dashboard is filtered on (any of them); the empty
    /// name is "Uncategorized". Empty: no category filter.
    pub category_filters: Vec<String>,
    /// Legacy personal accent color (`#rrggbb`), still honored over the
    /// project's; cleared when the color is changed (spec §2).
    pub accent: Option<String>,
    /// Legacy personal icon / emoji; cleared when the icon is changed.
    pub icon: Option<String>,
    /// Repos whose linked worktrees are listed under them (spec §10).
    pub expanded_worktrees: Vec<RepoId>,
    /// Worktree Sets of the project (spec §10).
    pub worktree_sets: Vec<WorktreeSet>,
    /// What the dashboard shows of worktrees (the Set Selector).
    pub set_view: SetView,
    /// In a set view, list the repos outside the set (dimmed).
    pub set_show_others: bool,
}

impl UiState {
    /// Whether `repo` is hidden.
    #[must_use]
    pub fn is_hidden(&self, repo: RepoId) -> bool {
        self.hidden_repos.contains(&repo)
    }

    /// Whether the commit graph shows `column`.
    #[must_use]
    pub fn shows_graph_column(&self, column: GraphColumn) -> bool {
        !self.hidden_graph_columns.contains(&column)
    }

    /// Hides `column` of the commit graph when shown, shows it when hidden.
    pub fn toggle_graph_column(&mut self, column: GraphColumn) {
        if let Some(i) = self.hidden_graph_columns.iter().position(|c| *c == column) {
            self.hidden_graph_columns.remove(i);
        } else {
            self.hidden_graph_columns.push(column);
        }
    }

    /// The active Worktree Set, if the dashboard shows one.
    #[must_use]
    pub fn active_set(&self) -> Option<&WorktreeSet> {
        match &self.set_view {
            SetView::Set(name) => self.worktree_sets.iter().find(|s| s.name == *name),
            _ => None,
        }
    }

    /// Closes the repo tabs of `ids`. The active tab stays active when it
    /// is still open, else the dashboard (the first tab) is.
    pub fn close_repo_tabs(&mut self, ids: &[RepoId]) {
        let active = self.tabs.get(self.active_tab).cloned();
        self.tabs
            .retain(|t| !matches!(t, TabState::Repo(r) if ids.contains(r)));
        self.active_tab = active
            .and_then(|a| self.tabs.iter().position(|t| *t == a))
            .unwrap_or(0);
    }

    /// Forgets repos gone from the project: their tabs, selection, hidden
    /// and expanded state. Every removal goes through it, so no tab or
    /// selection is left pointing at a repo that is no longer there.
    pub fn forget_repos(&mut self, ids: &[RepoId]) {
        self.close_repo_tabs(ids);
        self.selection.retain(|r| !ids.contains(r));
        // As when it is Ctrl+clicked away: another selected repo takes over.
        if self.selected_repo.is_some_and(|r| ids.contains(&r)) {
            self.selected_repo = self.selection.pop();
        }
        self.hidden_repos.retain(|r| !ids.contains(r));
        self.expanded_worktrees.retain(|r| !ids.contains(r));
    }

    /// What the current view focuses on (see [`Focus`]): the scope of
    /// actions on "the selected repos".
    #[must_use]
    pub fn focus(&self) -> Focus {
        match self.tabs.get(self.active_tab) {
            Some(TabState::Repo(id)) => Focus::Repo(*id),
            Some(TabState::Dashboard) | None => Focus::Dashboard(self.batch_selection()),
        }
    }

    /// The dashboard batch selection: the multi-selection, else the
    /// selected repo, else none (meaning "all"). It is what the dashboard
    /// and the sidebar show selected; actions take [`Self::focus`].
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
            refs_collapsed: false,
            details_width: 460.0,
            quick_edit_width: 360.0,
            column_widths: BTreeMap::new(),
            sidebar_collapsed: false,
            drawer_height: 180.0,
            selection: Vec::new(),
            hidden_repos: Vec::new(),
            show_hidden: false,
            show_dependency_tree: false,
            collapsed_nodes: Vec::new(),
            collapsed_categories: Vec::new(),
            collapsed_ref_sections: Vec::new(),
            hidden_graph_columns: vec![GraphColumn::Date],
            category_filters: Vec::new(),
            accent: None,
            icon: None,
            expanded_worktrees: Vec::new(),
            worktree_sets: Vec::new(),
            set_view: SetView::All,
            set_show_others: false,
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
        assert!(state.column_widths.is_empty());
        assert_eq!(state.hidden_graph_columns, [GraphColumn::Date]);
    }

    #[test]
    fn graph_columns_toggle() {
        let mut state = UiState::default();
        state.toggle_graph_column(GraphColumn::Author);
        assert!(!state.shows_graph_column(GraphColumn::Author));
        assert!(state.shows_graph_column(GraphColumn::Age));
        state.toggle_graph_column(GraphColumn::Author);
        assert!(state.shows_graph_column(GraphColumn::Author));
    }

    #[test]
    fn closing_repo_tabs_keeps_the_active_tab_or_falls_back_to_the_dashboard() {
        let mut state = UiState {
            tabs: vec![
                TabState::Dashboard,
                TabState::Repo(RepoId(1)),
                TabState::Repo(RepoId(2)),
            ],
            active_tab: 2,
            ..UiState::default()
        };
        state.close_repo_tabs(&[RepoId(1)]);
        assert_eq!(state.tabs[state.active_tab], TabState::Repo(RepoId(2)));
        state.close_repo_tabs(&[RepoId(2)]);
        assert_eq!(state.active_tab, 0);
        assert_eq!(state.tabs, [TabState::Dashboard]);
    }

    #[test]
    fn forgotten_repos_leave_no_trace() {
        let gone = RepoId(1);
        let mut state = UiState {
            tabs: vec![TabState::Dashboard, TabState::Repo(gone)],
            active_tab: 1,
            selected_repo: Some(gone),
            selection: vec![gone, RepoId(2)],
            hidden_repos: vec![gone],
            expanded_worktrees: vec![gone],
            ..UiState::default()
        };
        state.forget_repos(&[gone]);
        assert_eq!(state.tabs, [TabState::Dashboard]);
        assert_eq!(state.active_tab, 0);
        assert_eq!(state.selected_repo, Some(RepoId(2)));
        assert!(state.selection.is_empty());
        assert!(state.hidden_repos.is_empty());
        assert!(state.expanded_worktrees.is_empty());
    }

    #[test]
    fn the_active_tab_dictates_the_focus() {
        let mut state = UiState {
            tabs: vec![TabState::Dashboard, TabState::Repo(RepoId(3))],
            active_tab: 0,
            selected_repo: Some(RepoId(1)),
            selection: vec![RepoId(2)],
            ..UiState::default()
        };
        let dashboard = state.focus();
        assert_eq!(dashboard, Focus::Dashboard(vec![RepoId(1), RepoId(2)]));
        assert_eq!(dashboard.targets(), vec![RepoId(1), RepoId(2)]);
        assert_eq!(dashboard.repo(), None);
        assert_eq!(dashboard.tab_repo(), None);
        state.active_tab = 1;
        let tab = state.focus();
        // The dashboard selection does not leak into a repo tab.
        assert_eq!(tab.targets(), vec![RepoId(3)]);
        assert_eq!(tab.repo(), Some(RepoId(3)));
        assert_eq!(tab.tab_repo(), Some(RepoId(3)));
        assert!(tab.is_explicit());
        state.active_tab = 0;
        state.selected_repo = None;
        state.selection.clear();
        assert!(!state.focus().is_explicit());
        assert_eq!(state.focus().targets(), Vec::new());
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
