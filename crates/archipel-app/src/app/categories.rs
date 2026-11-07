//! Repo categories (spec §1): setting and renaming them in the project
//! definition, the sidebar groups and the dashboard category filter.
//!
//! Category names match regardless of case: a name typed with another case
//! than an existing category reuses that category's spelling.

use std::collections::HashMap;

use archipel_model::RepoId;

use super::{App, AppError, AppEvent, Session, lock, toggle};
use crate::groups::{
    RepoGroup, category_filter_keys, distinct_categories, group_by_category, normalize_category,
};

impl Session {
    /// The spelling of the existing category matching `name` but for case,
    /// leaving out `except`; `name` itself when there is none.
    fn canonical_category(&self, name: String, except: Option<&str>) -> String {
        distinct_categories(self.repo_categories())
            .into_iter()
            .find(|e| Some(e.as_str()) != except && e.to_lowercase() == name.to_lowercase())
            .unwrap_or(name)
    }
}

impl App {
    /// Sets (or clears, when `None` or blank) the category of repos and
    /// saves the project definition (spec §1). A name matching an existing
    /// category but for case reuses that category's spelling.
    pub fn set_category(&self, ids: &[RepoId], category: Option<&str>) -> Result<(), AppError> {
        if ids.is_empty() {
            return Ok(());
        }
        self.edit_definition(|s| {
            let category = normalize_category(category).map(|c| s.canonical_category(c, None));
            for entry in s
                .def
                .repos
                .iter_mut()
                .filter(|r| ids.contains(&r.path.id()))
            {
                entry.category.clone_from(&category);
            }
            Some(())
        })?;
        self.emit(AppEvent::RowsChanged(ids.to_vec()));
        Ok(())
    }

    /// Renames a category on every repo using it, or removes it (its repos
    /// become uncategorized) when `new` is `None` or blank. Renaming onto
    /// another existing category (any case) merges into it; a change of
    /// case only is kept as typed. The collapsed state follows the rename.
    pub(super) fn rename_category(&self, old: &str, new: Option<&str>) -> Result<(), AppError> {
        let new = normalize_category(new);
        let renamed = self.edit_definition(|s| {
            let new = new.map(|c| s.canonical_category(c, Some(old)));
            let mut ids = Vec::new();
            for entry in s
                .def
                .repos
                .iter_mut()
                .filter(|r| r.category.as_deref() == Some(old))
            {
                entry.category.clone_from(&new);
                ids.push(entry.path.id());
            }
            if ids.is_empty() {
                return None;
            }
            let ui = &mut s.ui;
            let was_collapsed = ui.collapsed_categories.iter().any(|k| k == old);
            ui.collapsed_categories.retain(|k| k != old);
            if let Some(new) = new.clone().filter(|_| was_collapsed)
                && !ui.collapsed_categories.contains(&new)
            {
                ui.collapsed_categories.push(new);
            }
            // The category filter follows too; a removed category's repos
            // are now uncategorized.
            if ui.category_filters.iter().any(|k| k == old) {
                ui.category_filters.retain(|k| k != old);
                let new = new.unwrap_or_default();
                if !ui.category_filters.contains(&new) {
                    ui.category_filters.push(new);
                }
            }
            Some(ids)
        })?;
        let Some(ids) = renamed else {
            return Ok(());
        };
        self.emit(AppEvent::RowsChanged(ids));
        self.emit(AppEvent::UiStateChanged);
        Ok(())
    }

    /// Collapses or expands a sidebar category group (empty: "Uncategorized").
    pub(super) fn toggle_category(&self, key: String) {
        self.update_ui_state(|ui| toggle(&mut ui.collapsed_categories, key));
    }

    /// Categories of the open project's repos, sorted by name.
    #[must_use]
    pub fn categories(&self) -> Vec<String> {
        lock(&self.session)
            .as_ref()
            .map_or_else(Vec::new, |s| distinct_categories(s.repo_categories()))
    }

    /// Toggles a dashboard category filter (empty: "Uncategorized").
    pub(super) fn toggle_category_filter(&self, key: String) {
        self.update_ui_state(|ui| toggle(&mut ui.category_filters, key));
    }

    /// Keys of the dashboard category filter chips: the categories sorted by
    /// name, then `""` ("Uncategorized") when some repo has none; empty when
    /// the project has no category.
    #[must_use]
    pub fn category_filter_keys(&self) -> Vec<String> {
        lock(&self.session)
            .as_ref()
            .map_or_else(Vec::new, |s| category_filter_keys(s.repo_categories()))
    }

    /// The sidebar: rows that are not hidden, grouped by category (spec §1).
    #[must_use]
    pub fn sidebar_groups(&self) -> Vec<RepoGroup> {
        let guard = lock(&self.session);
        let Some(s) = guard.as_ref() else {
            return Vec::new();
        };
        let categories: HashMap<RepoId, &str> = s
            .def
            .repos
            .iter()
            .filter_map(|r| Some((r.path.id(), r.category.as_deref()?)))
            .collect();
        let rows = s.rows.iter().filter(|r| !r.hidden).cloned().collect();
        group_by_category(
            rows,
            |id| categories.get(&id).copied(),
            &s.ui.collapsed_categories,
        )
    }
}
