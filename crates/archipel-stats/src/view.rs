//! View models produced by statistics providers.

use archipel_model::stats::{ActivityBucket, BranchAge, Hotspot};

/// Commits per week and author, sorted by week then author.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ActivityView {
    /// Buckets (author emails are lowercased).
    pub buckets: Vec<ActivityBucket>,
}

impl ActivityView {
    /// Distinct authors, sorted.
    #[must_use]
    pub fn authors(&self) -> Vec<&str> {
        let mut authors: Vec<&str> = self.buckets.iter().map(|b| b.author.as_str()).collect();
        authors.sort_unstable();
        authors.dedup();
        authors
    }

    /// Total number of commits.
    #[must_use]
    pub fn total_commits(&self) -> u64 {
        self.buckets.iter().map(|b| u64::from(b.commits)).sum()
    }
}

/// One branch of one repository.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BranchRow {
    /// Display name of the repository.
    pub repo: String,
    /// Age and merge state (never `merged` for the current branch).
    pub age: BranchAge,
    /// Whether `HEAD` is on this branch.
    pub current: bool,
}

/// Local branches (default branch excluded), oldest tip first.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BranchesView {
    /// Rows, oldest last commit first.
    pub branches: Vec<BranchRow>,
}

impl BranchesView {
    /// Branches fully merged into the default branch that still exist
    /// (candidates for a batch delete).
    pub fn merged_not_deleted(&self) -> impl Iterator<Item = &BranchRow> {
        self.branches.iter().filter(|r| r.age.merged)
    }
}

/// One churned path of one repository.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HotspotRow {
    /// Display name of the repository.
    pub repo: String,
    /// Churn of the path.
    pub hotspot: Hotspot,
}

/// Paths with the highest churn, highest first.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HotspotsView {
    /// Rows, highest churn first.
    pub hotspots: Vec<HotspotRow>,
}

/// A generic table, for providers that need no dedicated renderer.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TableView {
    /// Column headers.
    pub columns: Vec<String>,
    /// Rows of cells, one cell per column.
    pub rows: Vec<Vec<String>>,
}

/// What a provider produces, for one repository or a whole project.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum StatView {
    /// Activity over time.
    Activity(ActivityView),
    /// Branch age and merged-not-deleted branches.
    Branches(BranchesView),
    /// File hotspots.
    Hotspots(HotspotsView),
    /// Generic table.
    Table(TableView),
}
