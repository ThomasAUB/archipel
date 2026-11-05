//! Sidebar grouping of repos by category (spec §1).

use std::collections::{BTreeMap, BTreeSet};

use archipel_model::RepoId;

use crate::dashboard::DashboardRow;

/// One group of the sidebar.
#[derive(Clone, Debug)]
pub struct RepoGroup {
    /// Category name; `None` for repos without a category (and for the
    /// single group of a project that has no category at all).
    pub category: Option<String>,
    /// Whether the group is collapsed (its rows are still listed here).
    pub collapsed: bool,
    /// Rows of the group, in project order.
    pub rows: Vec<DashboardRow>,
}

impl RepoGroup {
    /// Key of the group in [`UiState::collapsed_categories`]: the category
    /// name, or the empty string for "Uncategorized".
    ///
    /// [`UiState::collapsed_categories`]: archipel_model::ui_state::UiState::collapsed_categories
    #[must_use]
    pub fn key(&self) -> &str {
        self.category.as_deref().unwrap_or("")
    }
}

/// Normalizes a category name: trimmed, `None` when blank.
#[must_use]
pub fn normalize_category(category: Option<&str>) -> Option<String> {
    category
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .map(str::to_owned)
}

/// Groups `rows` by category: categories sorted by name (case-insensitive),
/// rows kept in their order inside a group, uncategorized rows last. With no
/// category at all, a single uncategorized, expanded group (flat list).
#[must_use]
pub fn group_by_category<'a>(
    rows: Vec<DashboardRow>,
    category_of: impl Fn(RepoId) -> Option<&'a str>,
    collapsed: &[String],
) -> Vec<RepoGroup> {
    let mut named: BTreeMap<(String, String), Vec<DashboardRow>> = BTreeMap::new();
    let mut other = Vec::new();
    for row in rows {
        match normalize_category(category_of(row.id)) {
            Some(c) => named.entry((c.to_lowercase(), c)).or_default().push(row),
            None => other.push(row),
        }
    }
    if named.is_empty() {
        return vec![RepoGroup {
            category: None,
            collapsed: false,
            rows: other,
        }];
    }
    let is_collapsed = |key: &str| collapsed.iter().any(|c| c == key);
    let mut groups: Vec<RepoGroup> = named
        .into_iter()
        .map(|((_, category), rows)| RepoGroup {
            collapsed: is_collapsed(&category),
            category: Some(category),
            rows,
        })
        .collect();
    if !other.is_empty() {
        groups.push(RepoGroup {
            category: None,
            collapsed: is_collapsed(""),
            rows: other,
        });
    }
    groups
}

/// Distinct categories of `categories`, sorted by name (case-insensitive).
#[must_use]
pub fn distinct_categories<'a>(
    categories: impl IntoIterator<Item = Option<&'a str>>,
) -> Vec<String> {
    let mut out: BTreeSet<(String, String)> = BTreeSet::new();
    for c in categories {
        if let Some(c) = normalize_category(c) {
            out.insert((c.to_lowercase(), c));
        }
    }
    out.into_iter().map(|(_, c)| c).collect()
}

/// The category filter keys of a project: its categories sorted by name,
/// then `""` ("Uncategorized") when some repo has none. Empty when the
/// project has no category at all (nothing to filter on).
#[must_use]
pub fn category_filter_keys<'a>(
    categories: impl IntoIterator<Item = Option<&'a str>> + Clone,
) -> Vec<String> {
    let mut keys = distinct_categories(categories.clone());
    if !keys.is_empty()
        && categories
            .into_iter()
            .any(|c| normalize_category(c).is_none())
    {
        keys.push(String::new());
    }
    keys
}

/// Whether a repo of category `category` passes the category filter:
/// `active` keys that are not in `keys` (stale) are ignored, and with no
/// key left every repo passes; otherwise the repo's category must be one
/// of them.
#[must_use]
pub fn passes_category_filter(active: &[String], keys: &[String], category: Option<&str>) -> bool {
    let key = normalize_category(category).unwrap_or_default();
    let mut effective = active.iter().filter(|a| keys.contains(a)).peekable();
    effective.peek().is_none() || effective.any(|a| *a == key)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use archipel_model::RepoPath;

    use super::*;
    use crate::dashboard::RepoStatus;
    use crate::forge::ForgeStatus;

    fn row(name: &str) -> DashboardRow {
        let path = RepoPath::new(format!("/work/{name}"));
        DashboardRow {
            id: path.id(),
            name: name.to_owned(),
            path,
            status: RepoStatus::Loading,
            stale: false,
            hidden: false,
            forge: ForgeStatus::Unknown,
        }
    }

    fn names(group: &RepoGroup) -> Vec<&str> {
        group.rows.iter().map(|r| r.name.as_str()).collect()
    }

    fn grouped(spec: &[(&str, Option<&str>)], collapsed: &[&str]) -> Vec<RepoGroup> {
        let rows: Vec<DashboardRow> = spec.iter().map(|(n, _)| row(n)).collect();
        let cats: HashMap<RepoId, Option<String>> = rows
            .iter()
            .zip(spec)
            .map(|(r, (_, c))| (r.id, c.map(str::to_owned)))
            .collect();
        let collapsed: Vec<String> = collapsed.iter().map(|c| (*c).to_owned()).collect();
        group_by_category(
            rows,
            |id| cats.get(&id).and_then(|c| c.as_deref()),
            &collapsed,
        )
    }

    #[test]
    fn no_category_is_one_flat_group() {
        let groups = grouped(&[("a", None), ("b", Some("  "))], &[]);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].category, None);
        assert!(!groups[0].collapsed);
        assert_eq!(names(&groups[0]), ["a", "b"]);
    }

    #[test]
    fn categories_sorted_uncategorized_last_rows_in_order() {
        let groups = grouped(
            &[
                ("app", Some("apps")),
                ("misc", None),
                ("libz", Some("Libs")),
                ("cli", Some("apps")),
                ("liba", Some("Libs")),
            ],
            &["Libs", ""],
        );
        let cats: Vec<Option<&str>> = groups.iter().map(|g| g.category.as_deref()).collect();
        assert_eq!(cats, [Some("apps"), Some("Libs"), None]);
        assert_eq!(names(&groups[0]), ["app", "cli"]);
        assert_eq!(names(&groups[1]), ["libz", "liba"]);
        assert_eq!(names(&groups[2]), ["misc"]);
        let collapsed: Vec<bool> = groups.iter().map(|g| g.collapsed).collect();
        assert_eq!(collapsed, [false, true, true]);
        assert_eq!(groups[2].key(), "");
    }

    #[test]
    fn category_filter_keys_and_matching() {
        assert!(category_filter_keys([None, Some(" ")]).is_empty());
        assert_eq!(
            category_filter_keys([Some("libs"), Some("apps")]),
            ["apps", "libs"]
        );
        let keys = category_filter_keys([Some("libs"), None, Some("apps")]);
        assert_eq!(keys, ["apps", "libs", ""]);

        let active = |a: &[&str]| a.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        // No filter, or only stale keys: everything passes.
        assert!(passes_category_filter(&[], &keys, Some("libs")));
        assert!(passes_category_filter(&active(&["gone"]), &keys, None));
        // Any of the selected categories.
        let a = active(&["libs", "", "gone"]);
        assert!(passes_category_filter(&a, &keys, Some("libs")));
        assert!(passes_category_filter(&a, &keys, None));
        assert!(!passes_category_filter(&a, &keys, Some("apps")));
    }

    #[test]
    fn distinct_categories_are_trimmed_and_sorted() {
        let cats = distinct_categories([Some(" libs"), None, Some("Apps"), Some("libs"), Some("")]);
        assert_eq!(cats, ["Apps", "libs"]);
    }
}
