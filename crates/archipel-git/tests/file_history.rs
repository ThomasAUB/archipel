//! (Test file: unwrap/expect/panic are the expected failure mode in tests.)
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The history of one file (`FileHistoryReader`) on real temporary
//! repositories: renames followed, paging, blame at a commit and in the
//! working copy.

use archipel_fixtures::TestRepo;
use archipel_git::GitCli;
use archipel_model::file_history::{Blame, BlameOptions};
use archipel_model::ports::{FileHistoryReader, RefReader};
use archipel_model::{CancelToken, GitError, Oid, RepoPath};

/// Four commits: `a.txt` created, edited, renamed to `b c.txt`, edited;
/// `other.txt` touched in between.
fn setup() -> (TestRepo, RepoPath, GitCli, Vec<Oid>) {
    let repo = TestRepo::new().unwrap();
    let path = RepoPath::new(repo.path());
    let cli = GitCli::default();
    let head = |cli: &GitCli| cli.resolve(&path, "HEAD").unwrap().unwrap();
    let mut commits = Vec::new();
    repo.write("a.txt", "one\ntwo\nthree\n").unwrap();
    repo.commit_all("create").unwrap();
    commits.push(head(&cli));
    repo.write("a.txt", "one\nTWO\nthree\nfour\n").unwrap();
    repo.commit_all("edit").unwrap();
    commits.push(head(&cli));
    repo.write("other.txt", "x\n").unwrap();
    repo.commit_all("unrelated").unwrap();
    repo.git(&["mv", "a.txt", "b c.txt"]).unwrap();
    repo.commit_all("rename").unwrap();
    commits.push(head(&cli));
    repo.write("b c.txt", "one\nTWO\nthree\nfour\nfive\n")
        .unwrap();
    repo.commit_all("append").unwrap();
    commits.push(head(&cli));
    (repo, path, cli, commits)
}

fn summaries(cli: &GitCli, path: &RepoPath, skip: usize, limit: usize) -> Vec<(String, String)> {
    cli.file_log(path, "b c.txt", None, skip, limit, &CancelToken::new())
        .unwrap()
        .into_iter()
        .map(|r| (r.meta.summary, r.path))
        .collect()
}

fn pairs(list: &[(&str, &str)]) -> Vec<(String, String)> {
    list.iter()
        .map(|(a, b)| ((*a).to_owned(), (*b).to_owned()))
        .collect()
}

#[test]
fn file_log_follows_renames_newest_first() {
    let (_repo, path, cli, _) = setup();
    assert_eq!(
        summaries(&cli, &path, 0, 100),
        pairs(&[
            ("append", "b c.txt"),
            ("rename", "b c.txt"),
            ("edit", "a.txt"),
            ("create", "a.txt"),
        ])
    );
}

#[test]
fn file_log_pages() {
    let (_repo, path, cli, _) = setup();
    assert_eq!(
        summaries(&cli, &path, 1, 2),
        pairs(&[("rename", "b c.txt"), ("edit", "a.txt")])
    );
    assert!(summaries(&cli, &path, 4, 2).is_empty());
    assert!(summaries(&cli, &path, 0, 0).is_empty());
}

#[test]
fn file_log_starts_from_a_commit() {
    let (_repo, path, cli, commits) = setup();
    let log = cli
        .file_log(&path, "a.txt", Some(commits[1]), 0, 10, &CancelToken::new())
        .unwrap();
    let got: Vec<Oid> = log.iter().map(|r| r.meta.oid).collect();
    assert_eq!(got, [commits[1], commits[0]]);
}

fn line_commits(blame: &Blame) -> Vec<Option<Oid>> {
    (0..blame.lines.len())
        .map(|l| blame.commits[blame.hunk_at(l).unwrap().commit].oid)
        .collect()
}

#[test]
fn blame_at_head_names_the_commit_of_each_line() {
    let (_repo, path, cli, c) = setup();
    let blame = cli
        .blame(
            &path,
            "b c.txt",
            Some(c[3]),
            BlameOptions::default(),
            &CancelToken::new(),
        )
        .unwrap();
    assert_eq!(blame.lines, ["one", "TWO", "three", "four", "five"]);
    assert_eq!(
        line_commits(&blame),
        [Some(c[0]), Some(c[1]), Some(c[0]), Some(c[1]), Some(c[3])]
    );
    // Lines from before the rename are named by their old path, and the
    // last commit's parent is where to blame before it.
    let first = blame.hunk_at(0).unwrap();
    assert_eq!(first.path, "a.txt");
    let last = &blame.commits[blame.hunk_at(4).unwrap().commit];
    assert_eq!(last.summary, "append");
    let parent = last.parent.as_ref().unwrap();
    assert_eq!(parent.path, "b c.txt");
    assert_eq!(parent.oid, c[2]);
}

#[test]
fn blame_of_the_working_copy_shows_uncommitted_lines() {
    let (repo, path, cli, c) = setup();
    repo.write("b c.txt", "one\nTWO\nthree\nfour\nfive\nsix\n")
        .unwrap();
    let blame = cli
        .blame(
            &path,
            "b c.txt",
            None,
            BlameOptions::default(),
            &CancelToken::new(),
        )
        .unwrap();
    assert_eq!(line_commits(&blame)[4..], [Some(c[3]), None]);
    assert_eq!(blame.lines[5], "six");
}

#[test]
fn blame_can_ignore_whitespace_changes() {
    let (repo, path, cli, c) = setup();
    repo.write("b c.txt", "one\n  TWO\nthree\nfour\nfive\n")
        .unwrap();
    repo.commit_all("indent").unwrap();
    let blame = |options| {
        cli.blame(&path, "b c.txt", None, options, &CancelToken::new())
            .unwrap()
    };
    let plain = blame(BlameOptions::default());
    assert_eq!(
        plain.commits[plain.hunk_at(1).unwrap().commit].summary,
        "indent"
    );
    let ignoring = blame(BlameOptions {
        ignore_whitespace: true,
        ..BlameOptions::default()
    });
    assert_eq!(line_commits(&ignoring)[1], Some(c[1]));
}

#[test]
fn cancelled_calls_stop() {
    let (_repo, path, cli, _) = setup();
    let cancel = CancelToken::new();
    cancel.cancel();
    let result = cli.blame(&path, "b c.txt", None, BlameOptions::default(), &cancel);
    assert!(matches!(result, Err(GitError::Cancelled)), "{result:?}");
}
