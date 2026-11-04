//! Branches, tags, stashes and ref tips.

use serde::{Deserialize, Serialize};

use crate::ids::Oid;
use crate::summary::Upstream;
use crate::time::Timestamp;

/// A local or remote-tracking branch.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BranchInfo {
    /// Short name (`main`, `origin/main`).
    pub name: String,
    /// Whether it is a remote-tracking branch.
    pub is_remote: bool,
    /// Commit the branch points to.
    pub target: Oid,
    /// Whether `HEAD` is on this branch.
    pub is_head: bool,
    /// Upstream info (local branches only).
    pub upstream: Option<Upstream>,
    /// Committer date of the tip commit.
    pub tip_time: Timestamp,
}

/// A tag.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TagInfo {
    /// Tag name.
    pub name: String,
    /// Commit the tag ultimately points to (peeled).
    pub target: Oid,
    /// Whether it is an annotated tag object.
    pub annotated: bool,
}

/// A stash entry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StashEntry {
    /// Position in the stash list (`stash@{index}`).
    pub index: u32,
    /// Stash commit id (stable across list changes, used by undo).
    pub oid: Oid,
    /// Stash message.
    pub message: String,
}

/// Kind of a ref pointing into the history, for graph badges.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RefKind {
    /// `HEAD` itself (detached or not).
    Head,
    /// Local branch.
    LocalBranch,
    /// Remote-tracking branch.
    RemoteBranch,
    /// Tag.
    Tag,
    /// Stash.
    Stash,
}

/// A named ref and the commit it points to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefTip {
    /// Short name.
    pub name: String,
    /// Kind of ref.
    pub kind: RefKind,
    /// Commit it points to (tags are peeled).
    pub target: Oid,
}
