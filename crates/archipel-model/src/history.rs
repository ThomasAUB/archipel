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

/// What a squash would take: the commits, oldest first, and the message
/// proposed for the squashed commit (theirs, oldest first).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SquashPreview {
    /// The squashed commits, oldest first.
    pub commits: Vec<Oid>,
    /// The proposed message.
    pub message: String,
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

/// What history search reads of a commit: its metadata and its full
/// message. Read per search chunk and dropped, never cached: messages can
/// be long.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitText {
    /// Display metadata.
    pub meta: CommitMeta,
    /// Full message, summary included, trailing whitespace trimmed.
    pub message: String,
}

/// Order of the topology walk.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WalkOrder {
    /// `--topo-order`: keeps each line of history together, so an older commit
    /// of one branch can be listed above newer commits of another.
    Topological,
    /// Committer date, newest first, never a parent above its child
    /// (`--date-order`); what graph views use, as rows read chronologically.
    #[default]
    Date,
}
