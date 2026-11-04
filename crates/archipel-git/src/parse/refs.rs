//! Parsers of `git for-each-ref` outputs.

use archipel_model::refs::{BranchInfo, RefKind, RefTip, TagInfo};
use archipel_model::{GitError, Oid, Timestamp, Upstream};

/// `--format` used for branches (fields separated by NUL, one ref per line).
pub const BRANCH_FORMAT: &str = "%(refname)%00%(objectname)%00%(HEAD)%00%(upstream:short)%00%(upstream:track,nobracket)%00%(committerdate:unix)";

/// `--format` used for tags.
pub const TAG_FORMAT: &str = "%(refname:short)%00%(objectname)%00%(objecttype)%00%(*objectname)";

/// `--format` used for ref tips.
pub const TIP_FORMAT: &str = "%(refname)%00%(objectname)%00%(*objectname)";

fn err(line: &str) -> GitError {
    GitError::Parse {
        what: "git for-each-ref",
        detail: line.to_owned(),
    }
}

fn oid(s: &str, line: &str) -> Result<Oid, GitError> {
    Oid::from_hex(s).map_err(|_| err(line))
}

/// Parses `ahead 1, behind 2` / `gone` / empty.
#[must_use]
pub fn parse_track(track: &str) -> (u32, u32, bool) {
    if track.trim() == "gone" {
        return (0, 0, true);
    }
    let mut ahead = 0;
    let mut behind = 0;
    for part in track.split(',') {
        let mut words = part.split_whitespace();
        match (words.next(), words.next().and_then(|n| n.parse().ok())) {
            (Some("ahead"), Some(n)) => ahead = n,
            (Some("behind"), Some(n)) => behind = n,
            _ => {}
        }
    }
    (ahead, behind, false)
}

/// Parses branches printed with [`BRANCH_FORMAT`] for `refs/heads` and `refs/remotes`.
pub fn parse_branches(output: &str) -> Result<Vec<BranchInfo>, GitError> {
    let mut branches = Vec::new();
    for line in output.lines().filter(|l| !l.is_empty()) {
        let f: Vec<&str> = line.split('\0').collect();
        let [refname, target, head, upstream, track, time] = f[..] else {
            return Err(err(line));
        };
        let (name, is_remote) = if let Some(n) = refname.strip_prefix("refs/heads/") {
            (n, false)
        } else if let Some(n) = refname.strip_prefix("refs/remotes/") {
            if n.ends_with("/HEAD") {
                continue; // symbolic `origin/HEAD`
            }
            (n, true)
        } else {
            continue;
        };
        let upstream = (!upstream.is_empty()).then(|| {
            let (ahead, behind, gone) = parse_track(track);
            Upstream {
                name: upstream.to_owned(),
                ahead,
                behind,
                gone,
            }
        });
        branches.push(BranchInfo {
            name: name.to_owned(),
            is_remote,
            target: oid(target, line)?,
            is_head: head == "*",
            upstream,
            tip_time: Timestamp(time.parse().unwrap_or(0)),
        });
    }
    Ok(branches)
}

/// Parses tags printed with [`TAG_FORMAT`].
pub fn parse_tags(output: &str) -> Result<Vec<TagInfo>, GitError> {
    output
        .lines()
        .filter(|l| !l.is_empty())
        .map(|line| {
            let f: Vec<&str> = line.split('\0').collect();
            let [name, target, kind, peeled] = f[..] else {
                return Err(err(line));
            };
            let annotated = kind == "tag";
            let target = if annotated && !peeled.is_empty() {
                peeled
            } else {
                target
            };
            Ok(TagInfo {
                name: name.to_owned(),
                target: oid(target, line)?,
                annotated,
            })
        })
        .collect()
}

/// Parses ref tips printed with [`TIP_FORMAT`] (all refs).
pub fn parse_tips(output: &str) -> Result<Vec<RefTip>, GitError> {
    let mut tips = Vec::new();
    for line in output.lines().filter(|l| !l.is_empty()) {
        let f: Vec<&str> = line.split('\0').collect();
        let [refname, target, peeled] = f[..] else {
            return Err(err(line));
        };
        let (kind, name) = if let Some(n) = refname.strip_prefix("refs/heads/") {
            (RefKind::LocalBranch, n)
        } else if let Some(n) = refname.strip_prefix("refs/remotes/") {
            if n.ends_with("/HEAD") {
                continue;
            }
            (RefKind::RemoteBranch, n)
        } else if let Some(n) = refname.strip_prefix("refs/tags/") {
            (RefKind::Tag, n)
        } else if refname == "refs/stash" {
            (RefKind::Stash, "stash")
        } else {
            continue;
        };
        let target = if peeled.is_empty() { target } else { peeled };
        tips.push(RefTip {
            name: name.to_owned(),
            kind,
            target: oid(target, line)?,
        });
    }
    Ok(tips)
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "1111111111111111111111111111111111111111";
    const B: &str = "2222222222222222222222222222222222222222";

    #[test]
    fn track_parsing() {
        assert_eq!(parse_track("ahead 1, behind 2"), (1, 2, false));
        assert_eq!(parse_track("behind 7"), (0, 7, false));
        assert_eq!(parse_track("gone"), (0, 0, true));
        assert_eq!(parse_track(""), (0, 0, false));
    }

    #[test]
    fn branch_parsing() {
        let out = format!(
            "refs/heads/main\0{A}\0*\0origin/main\0ahead 1\01700000000\n\
             refs/remotes/origin/HEAD\0{A}\0 \0\0\00\n\
             refs/remotes/origin/main\0{B}\0 \0\0\01600000000\n"
        );
        let b = parse_branches(&out).unwrap();
        assert_eq!(b.len(), 2);
        assert!(b[0].is_head && !b[0].is_remote);
        assert_eq!(b[0].upstream.as_ref().unwrap().ahead, 1);
        assert_eq!(b[1].name, "origin/main");
        assert!(b[1].is_remote && b[1].upstream.is_none());
    }

    #[test]
    fn tag_parsing_peels_annotated() {
        let out = format!("v1\0{A}\0tag\0{B}\nlight\0{A}\0commit\0\n");
        let t = parse_tags(&out).unwrap();
        assert_eq!(t[0].target.to_hex(), B);
        assert!(t[0].annotated);
        assert_eq!(t[1].target.to_hex(), A);
    }

    #[test]
    fn tips_parsing() {
        let out = format!(
            "refs/heads/x\0{A}\0\nrefs/tags/v\0{A}\0{B}\nrefs/stash\0{A}\0\nrefs/notes/c\0{A}\0\n"
        );
        let t = parse_tips(&out).unwrap();
        assert_eq!(t.len(), 3);
        assert_eq!(t[1].kind, RefKind::Tag);
        assert_eq!(t[1].target.to_hex(), B);
        assert_eq!(t[2].kind, RefKind::Stash);
    }
}
