//! Commit topology and metadata.

use serde::{Deserialize, Serialize};

use crate::ids::Oid;
use crate::time::Timestamp;

/// One commit of the topology walk: id and parent ids, nothing else.
///
/// The walk is streamed in display order (children before parents) so the
/// graph can lay out lanes without holding the full history in memory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TopologyNode {
    /// Commit id.
    pub oid: Oid,
    /// Parent ids, first parent first.
    pub parents: Vec<Oid>,
}

/// Display metadata of a commit, loaded only for visible rows.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitMeta {
    /// Commit id.
    pub oid: Oid,
    /// First line of the message.
    pub summary: String,
    /// Author name.
    pub author_name: String,
    /// Author email.
    pub author_email: String,
    /// Author date.
    pub author_time: Timestamp,
    /// Committer date.
    pub commit_time: Timestamp,
}

impl CommitMeta {
    /// Approximate heap + inline size, used by byte-budgeted caches.
    #[must_use]
    pub fn approx_size(&self) -> usize {
        std::mem::size_of::<Self>()
            + self.summary.len()
            + self.author_name.len()
            + self.author_email.len()
    }
}

/// Order of the topology walk.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WalkOrder {
    /// `--topo-order` with committer date tie breaks (what graph views use).
    #[default]
    Topological,
    /// Committer date, newest first (`--date-order`).
    Date,
}
