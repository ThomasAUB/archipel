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

/// One commit of a history walk for the repository statistics: its parents,
/// author and line changes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitRecord {
    /// Commit id.
    pub oid: Oid,
    /// Parent ids, first parent first.
    pub parents: Vec<Oid>,
    /// Author name.
    pub author_name: String,
    /// Author email (lowercased).
    pub author_email: String,
    /// Author date.
    pub author_time: Timestamp,
    /// Committer date.
    pub commit_time: Timestamp,
    /// Subject line.
    pub subject: String,
    /// Lines changed per path (empty for merges).
    pub files: Vec<crate::diff::NumStat>,
}

/// One large blob of the history.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlobSize {
    /// Blob id.
    pub oid: Oid,
    /// A path it was stored at (empty when unreachable).
    pub path: String,
    /// Whether a ref reaches it (else only `git gc --prune` would drop it).
    pub reachable: bool,
    /// Uncompressed size in bytes.
    pub bytes: u64,
}

/// What a repository weighs on disk (`git count-objects -v`, the size of its
/// Git directory, its largest blobs).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RepoStorage {
    /// Size of the Git directory on disk, in bytes.
    pub git_dir_bytes: u64,
    /// Loose objects.
    pub loose_objects: u64,
    /// Disk space of the loose objects, in bytes.
    pub loose_bytes: u64,
    /// Objects in packs.
    pub packed_objects: u64,
    /// Number of packs.
    pub packs: u64,
    /// Disk space of the packs, in bytes.
    pub pack_bytes: u64,
    /// Largest blobs, largest first: the biggest reachable from a ref, and
    /// a few unreachable ones left for `git gc` to prune.
    pub largest_blobs: Vec<BlobSize>,
    /// Whether the clone is shallow.
    pub shallow: bool,
    /// Whether Git LFS is set up (an `lfs` folder or `filter=lfs`).
    pub lfs: bool,
}
