//! Conflict forecast: whether bringing one commit into another would
//! conflict, computed in memory without touching the repository (spec §3).

use serde::{Deserialize, Serialize};

/// How the two sides are brought together.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MergeStrategy {
    /// A three-way merge (`git merge`, `git pull --no-rebase`).
    #[default]
    Merge,
    /// The local commits replayed one by one on the other side
    /// (`git rebase`, `git pull --rebase`). It can conflict where a merge
    /// does not, and the reverse.
    Rebase,
}

impl MergeStrategy {
    /// `merge` or `rebase`.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Merge => "merge",
            Self::Rebase => "rebase",
        }
    }
}

/// What bringing `theirs` into `ours` would do.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergeForecast {
    /// The strategy actually checked: a rebase of too many commits is
    /// checked as a merge.
    pub strategy: MergeStrategy,
    /// The paths that would conflict, sorted, without duplicates; empty
    /// when it would go through cleanly. For a rebase, those of the first
    /// commit that would stop it.
    pub conflicts: Vec<String>,
}

impl MergeForecast {
    /// Whether it would conflict.
    #[must_use]
    pub fn conflicts(&self) -> bool {
        !self.conflicts.is_empty()
    }
}
