//! Dashboard view state of the open project: rows (filtered and sorted),
//! hidden repos, the UI state and the action context.

use std::collections::HashMap;

use archipel_model::RepoId;
use archipel_model::ui_state::{TabState, UiState};

use super::{App, AppEvent, Extensions, Session, lock};
use crate::actions::ActionContext;
use crate::dashboard::{DashboardRow, visible_rows};
use crate::groups::{category_filter_keys, normalize_category, passes_category_filter};

impl App {
    /// Every row of the open project that is not hidden, in project order
    /// (sidebar, tabs, multi-repo commit).
    #[must_use]
    pub fn rows(&self) -> Vec<DashboardRow> {
        lock(&self.session)
            .as_ref()
            .map(|s| s.rows.iter().filter(|r| !r.hidden).cloned().collect())
            .unwrap_or_default()
    }

    /// Every row, hidden ones included.
    #[must_use]
    pub fn all_rows(&self) -> Vec<DashboardRow> {
        lock(&self.session)
            .as_ref()
            .map(|s| s.rows.clone())
            .unwrap_or_default()
    }

    /// Number of hidden repos of the open project.
    #[must_use]
    pub fn hidden_count(&self) -> usize {
        lock(&self.session)
            .as_ref()
            .map_or(0, |s| s.rows.iter().filter(|r| r.hidden).count())
    }

    /// Rows after filters and sorting from the UI state; hidden rows are
    /// left out unless "show hidden" is on.
    #[must_use]
    pub fn visible_rows(&self) -> Vec<DashboardRow> {
        let guard = lock(&self.session);
        let Some(s) = guard.as_ref() else {
            return Vec::new();
        };
        Self::visible_rows_of(&self.ext, s)
    }

    /// [`App::visible_rows`] of session `s`, with the lock already held.
    pub(super) fn visible_rows_of(ext: &Extensions, s: &Session) -> Vec<DashboardRow> {
        let sort =
            s.ui.sort_column
                .as_deref()
                .and_then(|id| ext.columns.get(id))
                .map(|c| (c.as_ref(), s.ui.sort_direction));
        let categories: HashMap<RepoId, Option<&str>> = s
            .def
            .repos
            .iter()
            .map(|r| (r.path.id(), r.category.as_deref()))
            .collect();
        let keys = category_filter_keys(categories.values().copied());
        let shown: Vec<DashboardRow> = s
            .rows
            .iter()
            .filter(|r| s.ui.show_hidden || !r.hidden)
            .filter(|r| {
                let category = categories.get(&r.id).copied().flatten();
                passes_category_filter(&s.ui.category_filters, &keys, category)
            })
            .cloned()
            .collect();
        visible_rows(&shown, &ext.filters, &s.ui.filters, sort)
            .into_iter()
            .cloned()
            .collect()
    }

    /// The UI state of the open project.
    #[must_use]
    pub fn ui_state(&self) -> Option<UiState> {
        lock(&self.session).as_ref().map(|s| s.ui.clone())
    }

    /// Updates the UI state (panel sizes, …) with `f`.
    pub fn update_ui_state(&self, f: impl FnOnce(&mut UiState)) {
        if let Some(s) = lock(&self.session).as_mut() {
            f(&mut s.ui);
        }
        self.emit(AppEvent::UiStateChanged);
    }

    /// Whether the operations drawer is open.
    #[must_use]
    pub fn drawer_open(&self) -> bool {
        lock(&self.session).as_ref().is_some_and(|s| s.drawer_open)
    }

    /// Opens or closes the operations drawer.
    pub(super) fn toggle_drawer(&self) {
        if let Some(s) = lock(&self.session).as_mut() {
            s.drawer_open = !s.drawer_open;
        }
        self.emit(AppEvent::UiStateChanged);
    }

    /// Context for action availability.
    #[must_use]
    pub fn action_context(&self) -> ActionContext {
        let guard = lock(&self.session);
        let Some(s) = guard.as_ref() else {
            return ActionContext::default();
        };
        let active_repo = match s.ui.tabs.get(s.ui.active_tab) {
            Some(TabState::Repo(r)) => Some(*r),
            _ => None,
        };
        ActionContext {
            project_open: true,
            selected: s.ui.batch_selection(),
            active_repo,
        }
    }

    /// Makes repos visible wherever the user looks for them: unhidden, their
    /// sidebar category expanded, their category let through the dashboard
    /// category filter, and selected (the first one active). Emits
    /// [`AppEvent::ReposRevealed`] so the frontend scrolls to them.
    pub fn reveal_repos(&self, ids: &[RepoId]) {
        if ids.is_empty() {
            return;
        }
        let hidden: Vec<RepoId> = self
            .all_rows()
            .into_iter()
            .filter(|r| r.hidden && ids.contains(&r.id))
            .map(|r| r.id)
            .collect();
        self.set_hidden(&hidden, false);
        let ids: Vec<RepoId> = {
            let mut guard = lock(&self.session);
            let Some(s) = guard.as_mut() else { return };
            let categories: HashMap<RepoId, Option<&str>> = s
                .def
                .repos
                .iter()
                .map(|r| (r.path.id(), r.category.as_deref()))
                .collect();
            let keys = category_filter_keys(categories.values().copied());
            let ids: Vec<RepoId> = ids
                .iter()
                .copied()
                .filter(|id| categories.contains_key(id))
                .collect();
            let ui = &mut s.ui;
            for id in &ids {
                let category = categories.get(id).copied().flatten();
                let key = normalize_category(category).unwrap_or_default();
                ui.collapsed_categories.retain(|k| *k != key);
                if !passes_category_filter(&ui.category_filters, &keys, category) {
                    ui.category_filters.push(key);
                }
            }
            if let Some((first, rest)) = ids.split_first() {
                ui.selected_repo = Some(*first);
                ui.selection = rest.to_vec();
            }
            ids
        };
        self.emit(AppEvent::UiStateChanged);
        self.emit(AppEvent::ReposRevealed(ids));
    }

    /// Hides or unhides repos: hidden ones leave the selection and tabs and
    /// stop being watched and refreshed; unhidden ones are watched and
    /// refreshed again.
    pub(super) fn set_hidden(&self, ids: &[RepoId], hidden: bool) {
        if ids.is_empty() {
            return;
        }
        let (id, ui) = {
            let mut guard = lock(&self.session);
            let Some(s) = guard.as_mut() else { return };
            for row in s.rows.iter_mut().filter(|r| ids.contains(&r.id)) {
                row.hidden = hidden;
            }
            let ui = &mut s.ui;
            if hidden {
                for id in ids {
                    if !ui.hidden_repos.contains(id) {
                        ui.hidden_repos.push(*id);
                    }
                }
                if !ui.show_hidden && ui.selected_repo.is_some_and(|r| ids.contains(&r)) {
                    ui.selected_repo = None;
                }
                ui.selection.retain(|r| !ids.contains(r));
                let active = ui.tabs.get(ui.active_tab).cloned();
                ui.tabs
                    .retain(|t| !matches!(t, TabState::Repo(r) if ids.contains(r)));
                ui.active_tab = active
                    .and_then(|a| ui.tabs.iter().position(|t| *t == a))
                    .unwrap_or(0);
            } else {
                ui.hidden_repos.retain(|r| !ids.contains(r));
            }
            // Persist now: hiding is a deliberate choice, not layout noise.
            (s.id.clone(), s.ui.clone())
        };
        self.save_ui_state(&id, &ui);
        self.start_watching();
        if !hidden {
            for id in ids {
                self.refresh_repo(*id);
                self.load_tree(*id);
            }
        }
        self.emit(AppEvent::RowsChanged(ids.to_vec()));
        self.emit(AppEvent::UiStateChanged);
    }
}
