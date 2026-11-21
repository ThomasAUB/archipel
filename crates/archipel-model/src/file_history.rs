//! The history of one file: the commits that touched it, and who last
//! changed each of its lines (blame).

use crate::history::CommitMeta;
use crate::ids::Oid;
use crate::time::Timestamp;

/// A commit that touched a file, and the file's path in it (renames are
/// followed, so older commits may name it differently).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileRevision {
    /// The commit.
    pub meta: CommitMeta,
    /// The file's path in that commit.
    pub path: String,
}

/// How lines are matched to the commit that last changed them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct BlameOptions {
    /// Whitespace-only changes do not count (`git blame -w`).
    pub ignore_whitespace: bool,
    /// Lines moved or copied, within the file or from other files of the
    /// same commit, keep their original commit (`git blame -M -C`).
    pub detect_moves: bool,
}

/// Where a commit of a blame came from: the commit before it and the
/// file's path there, to blame the lines as they were before it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlameParent {
    /// The parent commit.
    pub oid: Oid,
    /// The file's path in the parent.
    pub path: String,
}

/// A commit named in a blame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlameCommit {
    /// The commit; `None` for lines not committed yet (working copy).
    pub oid: Option<Oid>,
    /// First line of the message.
    pub summary: String,
    /// Author name.
    pub author_name: String,
    /// Author email.
    pub author_email: String,
    /// Author date.
    pub author_time: Timestamp,
    /// The commit before it, when the file existed there.
    pub parent: Option<BlameParent>,
}

/// Consecutive lines last changed by the same commit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlameHunk {
    /// Index of the commit in [`Blame::commits`].
    pub commit: usize,
    /// First line, 0-based, in [`Blame::lines`].
    pub start: usize,
    /// Number of lines.
    pub len: usize,
    /// The file's path in that commit (it may have been renamed since, or
    /// the lines moved from another file).
    pub path: String,
}

/// Who last changed each line of a file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Blame {
    /// The commits named, each once.
    pub commits: Vec<BlameCommit>,
    /// The hunks, in line order, covering every line.
    pub hunks: Vec<BlameHunk>,
    /// The file's lines, without their line ending.
    pub lines: Vec<String>,
}

impl Blame {
    /// The hunk holding line `line` (0-based).
    #[must_use]
    pub fn hunk_at(&self, line: usize) -> Option<&BlameHunk> {
        let i = self.hunks.partition_point(|h| h.start + h.len <= line);
        self.hunks.get(i).filter(|h| h.start <= line)
    }

    /// The oldest and newest author dates of the committed lines, for
    /// shading lines by age.
    #[must_use]
    pub fn time_span(&self) -> Option<(Timestamp, Timestamp)> {
        let mut times = self
            .commits
            .iter()
            .filter(|c| c.oid.is_some())
            .map(|c| c.author_time);
        let first = times.next()?;
        Some(times.fold((first, first), |(lo, hi), t| (lo.min(t), hi.max(t))))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commit(time: i64, committed: bool) -> BlameCommit {
        BlameCommit {
            oid: committed.then(|| Oid::from_hex(&format!("{time:040x}")).unwrap()),
            summary: String::new(),
            author_name: String::new(),
            author_email: String::new(),
            author_time: Timestamp(time),
            parent: None,
        }
    }

    fn hunk(commit: usize, start: usize, len: usize) -> BlameHunk {
        BlameHunk {
            commit,
            start,
            len,
            path: "f".into(),
        }
    }

    #[test]
    fn finds_the_hunk_of_a_line() {
        let blame = Blame {
            commits: vec![commit(1, true), commit(2, true)],
            hunks: vec![hunk(0, 0, 2), hunk(1, 2, 1), hunk(0, 3, 3)],
            lines: vec![String::new(); 6],
        };
        let at = |l| blame.hunk_at(l).map(|h| h.start);
        assert_eq!(
            [at(0), at(1), at(2), at(3), at(5)],
            [Some(0), Some(0), Some(2), Some(3), Some(3)]
        );
        assert_eq!(at(6), None);
    }

    #[test]
    fn the_time_span_skips_uncommitted_lines() {
        let blame = Blame {
            commits: vec![commit(5, true), commit(99, false), commit(2, true)],
            ..Blame::default()
        };
        assert_eq!(blame.time_span(), Some((Timestamp(2), Timestamp(5))));
        assert_eq!(Blame::default().time_span(), None);
    }
}
