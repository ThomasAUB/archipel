//! The history of one file (the file browser's History and Blame):
//! `git log --follow` and `git blame --porcelain`.

use std::collections::HashMap;

use archipel_model::file_history::{
    Blame, BlameCommit, BlameHunk, BlameOptions, BlameParent, FileRevision,
};
use archipel_model::ports::FileHistoryReader;
use archipel_model::{CancelToken, GitError, Oid, RepoPath, Timestamp};

use super::GitCli;
use super::read::{META_FORMAT, oid, parse_meta};
use super::runner::{Cmd, CmdKind};

/// Starts each commit of `git log --name-only` output, before its metadata.
const RECORD: char = '\x1e';

impl FileHistoryReader for GitCli {
    fn file_log(
        &self,
        repo: &RepoPath,
        path: &str,
        from: Option<Oid>,
        skip: usize,
        limit: usize,
        cancel: &CancelToken,
    ) -> Result<Vec<FileRevision>, GitError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let fields = META_FORMAT.trim_start_matches("--format=");
        let format = format!("--format=%x1e{fields}");
        let rev = from.map_or_else(|| "HEAD".to_owned(), |o| o.to_hex());
        let (skip, limit) = (format!("--skip={skip}"), format!("--max-count={limit}"));
        let args = [
            "log",
            "--follow",
            "--name-only",
            &format,
            &skip,
            &limit,
            &rev,
            "--",
            path,
        ];
        let out = self
            .runner
            .run(Cmd::new(repo.as_path(), CmdKind::Read, &args).cancel(cancel))?;
        parse_file_log(&out.stdout_text(), path)
    }

    fn blame(
        &self,
        repo: &RepoPath,
        path: &str,
        at: Option<Oid>,
        options: BlameOptions,
        cancel: &CancelToken,
    ) -> Result<Blame, GitError> {
        let mut args = vec!["blame".to_owned(), "--porcelain".to_owned()];
        if options.ignore_whitespace {
            args.push("-w".into());
        }
        if options.detect_moves {
            args.extend(["-M".into(), "-C".into()]);
        }
        args.extend(at.map(|o| o.to_hex()));
        args.extend(["--".into(), path.to_owned()]);
        let out = self.runner.run(
            Cmd::new(repo.as_path(), CmdKind::Read, &[])
                .args(args)
                .cancel(cancel),
        )?;
        parse_blame(&String::from_utf8_lossy(&out.stdout))
    }
}

/// `git log --follow --name-only` with [`RECORD`] before the metadata: per
/// commit, its metadata line, a blank line, then the file's path in it.
/// A commit listing no path (a merge) keeps the path of the newer one.
fn parse_file_log(text: &str, path: &str) -> Result<Vec<FileRevision>, GitError> {
    let mut current = path.to_owned();
    text.split(RECORD)
        .filter(|r| !r.trim().is_empty())
        .map(|record| {
            let mut lines = record.lines();
            let meta = parse_meta(lines.next().unwrap_or_default())?;
            if let Some(p) = lines.find(|l| !l.is_empty()) {
                p.clone_into(&mut current);
            }
            Ok(FileRevision {
                meta,
                path: current.clone(),
            })
        })
        .collect()
}

/// The header of a group of lines in `git blame --porcelain`: the commit,
/// then the line numbers in it and in the file (and, at a group's start,
/// the number of lines of the group). `None` for any other line.
fn blame_header(line: &str) -> Option<&str> {
    let mut parts = line.split(' ');
    let hash = parts.next()?;
    let numbers = parts.next()?;
    (hash.len() >= 40
        && hash.bytes().all(|b| b.is_ascii_hexdigit())
        && numbers.bytes().all(|b| b.is_ascii_digit()))
    .then_some(hash)
}

/// `git blame --porcelain`: a header per line (see [`blame_header`]); the
/// first time a commit shows up, its details follow (author, summary,
/// `previous`, `filename`); then the line itself, after a tab. A commit
/// seen again names its file only when it has several.
fn parse_blame(text: &str) -> Result<Blame, GitError> {
    let mut blame = Blame::default();
    let mut by_hash: HashMap<String, usize> = HashMap::new();
    // Last path named for each commit.
    let mut paths: HashMap<usize, String> = HashMap::new();
    let mut current: Option<usize> = None;
    for line in text.split('\n') {
        if let Some(content) = line.strip_prefix('\t') {
            let Some(c) = current else {
                return Err(parse_error(line));
            };
            let path = paths.get(&c).cloned().unwrap_or_default();
            let at = blame.lines.len();
            blame
                .lines
                .push(content.strip_suffix('\r').unwrap_or(content).to_owned());
            match blame.hunks.last_mut() {
                Some(h) if h.commit == c && h.path == path && h.start + h.len == at => h.len += 1,
                _ => blame.hunks.push(BlameHunk {
                    commit: c,
                    start: at,
                    len: 1,
                    path,
                }),
            }
        } else if let Some(hash) = blame_header(line) {
            let next = blame.commits.len();
            let c = *by_hash.entry(hash.to_owned()).or_insert(next);
            if c == next {
                let committed = hash.bytes().any(|b| b != b'0');
                blame.commits.push(BlameCommit {
                    oid: committed.then(|| oid(hash, "git blame")).transpose()?,
                    summary: String::new(),
                    author_name: String::new(),
                    author_email: String::new(),
                    author_time: Timestamp::default(),
                    parent: None,
                });
            }
            current = Some(c);
        } else if let Some((key, value)) = line.split_once(' ')
            && let Some(c) = current
        {
            let commit = &mut blame.commits[c];
            match key {
                "author" => value.clone_into(&mut commit.author_name),
                "author-mail" => {
                    let mail = value.trim_start_matches('<').trim_end_matches('>');
                    mail.clone_into(&mut commit.author_email);
                }
                "author-time" => commit.author_time = Timestamp(value.parse().unwrap_or(0)),
                "summary" => value.clone_into(&mut commit.summary),
                "previous" => {
                    let (hash, path) = value.split_once(' ').ok_or_else(|| parse_error(line))?;
                    commit.parent = Some(BlameParent {
                        oid: oid(hash, "git blame")?,
                        path: path.to_owned(),
                    });
                }
                "filename" => {
                    paths.insert(c, value.to_owned());
                }
                _ => {}
            }
        }
    }
    Ok(blame)
}

fn parse_error(line: &str) -> GitError {
    GitError::Parse {
        what: "git blame",
        detail: line.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "ab38f7c831a9669c78d35bede9db4ee6f34f1b4a";
    const B: &str = "d0085ca35cf2c983e8b7f757e939537ca85f80bc";
    const ZERO: &str = "0000000000000000000000000000000000000000";

    fn details(
        hash: &str,
        author: &str,
        time: u32,
        summary: &str,
        previous: Option<&str>,
    ) -> String {
        let previous = previous
            .map(|p| format!("previous {p}\n"))
            .unwrap_or_default();
        format!(
            "{hash} 1 1 1\nauthor {author}\nauthor-mail <{author}@x>\nauthor-time {time}\n\
             author-tz +0100\ncommitter C\ncommitter-mail <c@x>\ncommitter-time {time}\n\
             committer-tz +0100\nsummary {summary}\n{previous}"
        )
    }

    #[test]
    fn blame_groups_lines_into_hunks_and_reads_each_commit_once() {
        let text = format!(
            "{}boundary\nfilename a.txt\n\tone\n{A} 2 2\n\ttwo\r\n\
             {}filename b c.txt\n\tTHREE\n\
             {A} 3 4 1\n\tfour\n\
             {}filename b c.txt\n\tdirty\n",
            details(A, "Ann", 100, "first", None),
            details(B, "Bob", 200, "second", Some(&format!("{A} a.txt"))),
            details(
                ZERO,
                "Not Committed Yet",
                300,
                "Version of b c.txt",
                Some(&format!("{B} b c.txt"))
            ),
        );
        let blame = parse_blame(&text).unwrap();
        assert_eq!(blame.lines, ["one", "two", "THREE", "four", "dirty"]);
        assert_eq!(blame.commits.len(), 3);
        let first = &blame.commits[0];
        assert_eq!(first.oid.map(|o| o.to_hex()).as_deref(), Some(A));
        assert_eq!(
            (
                first.author_name.as_str(),
                first.author_email.as_str(),
                first.author_time
            ),
            ("Ann", "Ann@x", Timestamp(100))
        );
        assert_eq!(first.summary, "first");
        assert_eq!(first.parent, None);
        let second = &blame.commits[1];
        assert_eq!(
            second.parent,
            Some(BlameParent {
                oid: Oid::from_hex(A).unwrap(),
                path: "a.txt".into()
            })
        );
        assert_eq!(blame.commits[2].oid, None, "uncommitted lines");
        let hunks: Vec<(usize, usize, usize, &str)> = blame
            .hunks
            .iter()
            .map(|h| (h.commit, h.start, h.len, h.path.as_str()))
            .collect();
        assert_eq!(
            hunks,
            [
                (0, 0, 2, "a.txt"),
                (1, 2, 1, "b c.txt"),
                (0, 3, 1, "a.txt"),
                (2, 4, 1, "b c.txt"),
            ]
        );
    }

    #[test]
    fn blame_headers_are_told_from_details() {
        assert_eq!(blame_header(&format!("{A} 3 4 1")), Some(A));
        assert_eq!(blame_header(&format!("{A} 3 4")), Some(A));
        assert_eq!(blame_header("author 12"), None);
        assert_eq!(blame_header("boundary"), None);
        assert_eq!(blame_header("deadbeef 1 1"), None);
    }

    #[test]
    fn an_empty_file_has_no_lines() {
        assert_eq!(parse_blame("").unwrap(), Blame::default());
    }

    #[test]
    fn file_log_follows_renames_and_keeps_the_path_through_merges() {
        let meta = |hash: &str, summary: &str| {
            format!("\x1e{hash}\x1f{summary}\x1fAnn\x1fa@x\x1f10\x1f20\n")
        };
        let text = format!(
            "{}\nb c.txt\n{}{}\na.txt\n",
            meta(B, "rename"),
            meta(ZERO.replace('0', "1").as_str(), "merge"),
            meta(A, "first"),
        );
        let log = parse_file_log(&text, "b c.txt").unwrap();
        let got: Vec<(&str, &str)> = log
            .iter()
            .map(|r| (r.meta.summary.as_str(), r.path.as_str()))
            .collect();
        assert_eq!(
            got,
            [
                ("rename", "b c.txt"),
                ("merge", "b c.txt"),
                ("first", "a.txt")
            ]
        );
        assert_eq!(log[0].meta.author_time, Timestamp(10));
        assert!(parse_file_log("", "x").unwrap().is_empty());
    }
}
