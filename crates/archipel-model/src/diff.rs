//! Diffs: files, hunks and lines.

use serde::{Deserialize, Serialize};

use crate::changes::ChangeKind;

/// Origin of one diff line.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum LineOrigin {
    /// Unchanged context line.
    Context,
    /// Added line.
    Added,
    /// Removed line.
    Removed,
}

/// One line of a hunk.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffLine {
    /// Whether the line is context, added or removed.
    pub origin: LineOrigin,
    /// Line content without the trailing newline.
    pub content: String,
    /// Line number in the old file (context and removed lines).
    pub old_lineno: Option<u32>,
    /// Line number in the new file (context and added lines).
    pub new_lineno: Option<u32>,
    /// `true` when the line is followed by "\ No newline at end of file".
    pub no_newline_at_eof: bool,
}

/// A hunk of a unified diff.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hunk {
    /// First line in the old file.
    pub old_start: u32,
    /// Number of old lines.
    pub old_lines: u32,
    /// First line in the new file.
    pub new_start: u32,
    /// Number of new lines.
    pub new_lines: u32,
    /// Text after the second `@@` (function context).
    pub header: String,
    /// The lines.
    pub lines: Vec<DiffLine>,
}

/// Diff of one file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileDiff {
    /// Old path (`None` for added files).
    pub old_path: Option<String>,
    /// New path (`None` for deleted files).
    pub new_path: Option<String>,
    /// Kind of change.
    pub kind: ChangeKind,
    /// Whether the file is binary (no hunks then).
    pub binary: bool,
    /// Hunks, empty for binary or too-large files.
    pub hunks: Vec<Hunk>,
    /// Set when hunks were omitted because the diff exceeded the size guard.
    pub truncated: bool,
}

impl FileDiff {
    /// The path to display (new path, or old path for deletions).
    #[must_use]
    pub fn display_path(&self) -> &str {
        self.new_path
            .as_deref()
            .or(self.old_path.as_deref())
            .unwrap_or("")
    }

    /// Number of added and removed lines.
    #[must_use]
    pub fn line_stats(&self) -> (u32, u32) {
        let mut added = 0;
        let mut removed = 0;
        for line in self.hunks.iter().flat_map(|h| &h.lines) {
            match line.origin {
                LineOrigin::Added => added += 1,
                LineOrigin::Removed => removed += 1,
                LineOrigin::Context => {}
            }
        }
        (added, removed)
    }
}

/// Lines added and removed in one path, as reported by `--numstat`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NumStat {
    /// Path (new path for renames).
    pub path: String,
    /// Added lines (`None` for binary files).
    pub added: Option<u32>,
    /// Removed lines (`None` for binary files).
    pub removed: Option<u32>,
}
