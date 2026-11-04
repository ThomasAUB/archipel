//! Parser of `git status --porcelain=v2 --branch --show-stash -z`.

use archipel_model::changes::{ChangeKind, FileChange, FileState};
use archipel_model::{ChangeCounts, GitError, Oid};

/// Parsed porcelain v2 output.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StatusReport {
    /// `HEAD` commit (`None` when unborn).
    pub head_oid: Option<Oid>,
    /// Current branch (`None` when detached).
    pub branch: Option<String>,
    /// Upstream short name.
    pub upstream: Option<String>,
    /// Ahead/behind counts (`None` with an upstream means the upstream is gone).
    pub ahead_behind: Option<(u32, u32)>,
    /// Stash entries.
    pub stash_count: u32,
    /// Changed paths.
    pub changes: Vec<FileChange>,
}

impl StatusReport {
    /// Counts of modified, staged, untracked and conflicted files.
    #[must_use]
    pub fn counts(&self) -> ChangeCounts {
        count_changes(&self.changes)
    }
}

/// Counts changes per category (a file can be both staged and modified).
#[must_use]
pub fn count_changes(changes: &[FileChange]) -> ChangeCounts {
    let mut counts = ChangeCounts::default();
    for change in changes {
        match change.state {
            FileState::Untracked => counts.untracked += 1,
            FileState::Conflicted => counts.conflicted += 1,
            FileState::Tracked { staged, unstaged } => {
                if staged != ChangeKind::Unmodified {
                    counts.staged += 1;
                }
                if unstaged != ChangeKind::Unmodified {
                    counts.modified += 1;
                }
            }
        }
    }
    counts
}

fn parse_err(detail: &str) -> GitError {
    GitError::Parse {
        what: "git status --porcelain=v2",
        detail: detail.chars().take(200).collect(),
    }
}

fn kind(code: u8) -> ChangeKind {
    match code {
        b'M' => ChangeKind::Modified,
        b'A' => ChangeKind::Added,
        b'D' => ChangeKind::Deleted,
        b'R' => ChangeKind::Renamed,
        b'C' => ChangeKind::Copied,
        b'T' => ChangeKind::TypeChanged,
        _ => ChangeKind::Unmodified,
    }
}

/// Parses the NUL-separated porcelain v2 output.
pub fn parse_status(output: &[u8]) -> Result<StatusReport, GitError> {
    let text = String::from_utf8_lossy(output);
    let mut report = StatusReport::default();
    let mut records = text.split('\0').filter(|r| !r.is_empty());
    while let Some(record) = records.next() {
        let bytes = record.as_bytes();
        match bytes.first() {
            Some(b'#') => parse_header(record, &mut report)?,
            Some(b'1') => {
                let (xy, path) = split_entry(record, 8)?;
                report.changes.push(tracked(xy, path, None));
            }
            Some(b'2') => {
                let (xy, path) = split_entry(record, 9)?;
                let original = records.next().map(str::to_owned);
                report.changes.push(tracked(xy, path, original));
            }
            Some(b'u') => {
                let (_, path) = split_entry(record, 10)?;
                report.changes.push(FileChange {
                    path: path.to_owned(),
                    original_path: None,
                    state: FileState::Conflicted,
                });
            }
            Some(b'?') => report.changes.push(FileChange {
                path: record.get(2..).ok_or_else(|| parse_err(record))?.to_owned(),
                original_path: None,
                state: FileState::Untracked,
            }),
            Some(b'!') => {} // ignored files are not reported
            _ => return Err(parse_err(record)),
        }
    }
    Ok(report)
}

/// Splits an entry with `fields` space-separated fields before the path.
fn split_entry(record: &str, fields: usize) -> Result<(&[u8], &str), GitError> {
    let mut parts = record.splitn(fields + 1, ' ');
    let xy = parts.nth(1).ok_or_else(|| parse_err(record))?.as_bytes();
    let path = parts.nth(fields - 2).ok_or_else(|| parse_err(record))?;
    if xy.len() != 2 {
        return Err(parse_err(record));
    }
    Ok((xy, path))
}

fn tracked(xy: &[u8], path: &str, original_path: Option<String>) -> FileChange {
    FileChange {
        path: path.to_owned(),
        original_path,
        state: FileState::Tracked {
            staged: kind(xy[0]),
            unstaged: kind(xy[1]),
        },
    }
}

fn parse_header(record: &str, report: &mut StatusReport) -> Result<(), GitError> {
    let mut parts = record.splitn(3, ' ');
    let key = parts.nth(1).unwrap_or_default();
    let value = parts.next().unwrap_or_default();
    match key {
        "branch.oid" => {
            report.head_oid = if value == "(initial)" {
                None
            } else {
                Some(Oid::from_hex(value).map_err(|_| parse_err(record))?)
            };
        }
        "branch.head" => {
            report.branch = (value != "(detached)").then(|| value.to_owned());
        }
        "branch.upstream" => report.upstream = Some(value.to_owned()),
        "branch.ab" => {
            let mut nums = value
                .split(' ')
                .map(|n| n.trim_start_matches(['+', '-']).parse::<u32>());
            let ahead = nums
                .next()
                .and_then(Result::ok)
                .ok_or_else(|| parse_err(record))?;
            let behind = nums
                .next()
                .and_then(Result::ok)
                .ok_or_else(|| parse_err(record))?;
            report.ahead_behind = Some((ahead, behind));
        }
        "stash" => report.stash_count = value.parse().map_err(|_| parse_err(record))?,
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const OID: &str = "0123456789abcdef0123456789abcdef01234567";

    fn sample() -> Vec<u8> {
        let records = [
            format!("# branch.oid {OID}"),
            "# branch.head main".into(),
            "# branch.upstream origin/main".into(),
            "# branch.ab +2 -3".into(),
            "# stash 4".into(),
            format!("1 .M N... 100644 100644 100644 {OID} {OID} src/a b.txt"),
            format!("1 A. N... 000000 100644 100644 {OID} {OID} new.txt"),
            format!("1 MM N... 100644 100644 100644 {OID} {OID} both.txt"),
            format!("2 R. N... 100644 100644 100644 {OID} {OID} R100 renamed.txt"),
            "orig.txt".into(),
            format!("u UU N... 100644 100644 100644 100644 {OID} {OID} {OID} conflict.txt"),
            "? untracked file.txt".into(),
            "! ignored.txt".into(),
        ];
        records.join("\0").into_bytes()
    }

    #[test]
    fn parses_headers() {
        let r = parse_status(&sample()).unwrap();
        assert_eq!(r.head_oid.unwrap().to_hex(), OID);
        assert_eq!(r.branch.as_deref(), Some("main"));
        assert_eq!(r.upstream.as_deref(), Some("origin/main"));
        assert_eq!(r.ahead_behind, Some((2, 3)));
        assert_eq!(r.stash_count, 4);
    }

    #[test]
    fn parses_entries_with_spaces_and_renames() {
        let r = parse_status(&sample()).unwrap();
        assert_eq!(r.changes.len(), 6);
        assert_eq!(r.changes[0].path, "src/a b.txt");
        assert!(r.changes[0].is_unstaged() && !r.changes[0].is_staged());
        assert_eq!(r.changes[3].path, "renamed.txt");
        assert_eq!(r.changes[3].original_path.as_deref(), Some("orig.txt"));
        assert_eq!(r.changes[4].state, FileState::Conflicted);
        assert_eq!(r.changes[5].path, "untracked file.txt");
        let c = r.counts();
        assert_eq!(
            (c.modified, c.staged, c.untracked, c.conflicted),
            (2, 3, 1, 1)
        );
    }

    #[test]
    fn unborn_and_detached() {
        let r = parse_status(b"# branch.oid (initial)\0# branch.head main\0").unwrap();
        assert_eq!(r.head_oid, None);
        assert_eq!(r.branch.as_deref(), Some("main"));
        let r = parse_status(format!("# branch.oid {OID}\0# branch.head (detached)\0").as_bytes())
            .unwrap();
        assert_eq!(r.branch, None);
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_status(b"zzz\0").is_err());
        assert!(parse_status(b"# branch.ab +x -y\0").is_err());
    }
}
