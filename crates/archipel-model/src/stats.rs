//! Statistics data (spec §7).

use serde::{Deserialize, Serialize};

use crate::ids::Oid;
use crate::time::Timestamp;

/// Commits of one author during one week.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivityBucket {
    /// Monday-based week index (see [`Timestamp::week_index`]).
    pub week: i64,
    /// Author (email, lowercased).
    pub author: String,
    /// Number of commits.
    pub commits: u32,
}

/// Lines churned in one path.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hotspot {
    /// Path.
    pub path: String,
    /// Lines added over the history.
    pub added: u64,
    /// Lines removed over the history.
    pub removed: u64,
    /// Number of commits touching the path.
    pub commits: u32,
}

impl Hotspot {
    /// Total churn (added + removed).
    #[must_use]
    pub fn churn(&self) -> u64 {
        self.added + self.removed
    }
}

/// Age and merge state of a branch.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BranchAge {
    /// Branch short name.
    pub name: String,
    /// Tip commit.
    pub tip: Oid,
    /// Committer date of the tip.
    pub last_commit: Timestamp,
    /// Whether the branch is fully merged into the default branch.
    pub merged: bool,
}

/// Incremental statistics produced from a range of new commits.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatsDelta {
    /// Activity counts to add.
    pub activity: Vec<ActivityBucket>,
    /// Churn to add.
    pub churn: Vec<Hotspot>,
}

impl StatsDelta {
    /// Whether the delta adds nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.activity.is_empty() && self.churn.is_empty()
    }
}
