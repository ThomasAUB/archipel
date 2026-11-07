//! (Test file: unwrap/expect/panic are the expected failure mode in tests.)
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Differential tests: the `gix` reader must agree with the `git` CLI on every
//! fixture state (PLAN §5.3).

use std::collections::HashSet;
use std::ops::ControlFlow;
use std::sync::OnceLock;

use archipel_fixtures::project::{GeneratedRepo, RepoState, generate_project};
use archipel_git::{GitCli, GixReader};
use archipel_model::history::WalkOrder;
use archipel_model::ports::{HistoryReader, RefReader, StashReader, StatusReader};
use archipel_model::{CancelToken, ContainingBranch, RepoOp, RepoPath, RepoSummary};

struct Fixture {
    _dir: tempfile::TempDir,
    repos: Vec<GeneratedRepo>,
}

fn fixture() -> &'static Fixture {
    static FIXTURE: OnceLock<Fixture> = OnceLock::new();
    FIXTURE.get_or_init(|| {
        let dir = tempfile::tempdir().unwrap();
        let repos = generate_project(dir.path(), RepoState::CYCLE.len(), 6).unwrap();
        Fixture { _dir: dir, repos }
    })
}

/// `describe` hash abbreviations may differ in length: compare the tag and distance.
fn describe_key(s: &RepoSummary) -> Option<String> {
    s.describe
        .as_ref()
        .map(|d| d.split("-g").next().unwrap_or(d).to_owned())
}

#[test]
fn summaries_agree() {
    let (cli, gix) = (GitCli::default(), GixReader::default());
    for repo in &fixture().repos {
        let path = RepoPath::new(&repo.path);
        let a = cli.summary(&path).unwrap();
        let b = gix.summary(&path).unwrap();
        let ctx = format!("{:?}", repo.state);
        assert_eq!(a.head, b.head, "{ctx}");
        assert_eq!(a.head_oid, b.head_oid, "{ctx}");
        assert_eq!(a.upstream, b.upstream, "{ctx}");
        assert_eq!(a.changes, b.changes, "{ctx}");
        assert_eq!(a.stash_count, b.stash_count, "{ctx}");
        assert_eq!(describe_key(&a), describe_key(&b), "{ctx}");
        assert_eq!(a.in_progress, b.in_progress, "{ctx}");
        assert_eq!(a.containing_branches, b.containing_branches, "{ctx}");
    }
}

#[test]
fn summaries_reflect_states() {
    let cli = GitCli::default();
    for repo in &fixture().repos {
        let s = cli.summary(&RepoPath::new(&repo.path)).unwrap();
        match repo.state {
            RepoState::Clean => assert!(!s.changes.is_dirty() && s.ahead() == 0 && s.behind() == 0),
            RepoState::Dirty => assert_eq!(s.changes.modified, 1),
            RepoState::Staged => assert_eq!(s.changes.staged, 1),
            RepoState::Untracked => assert_eq!(s.changes.untracked, 1),
            RepoState::Ahead => assert_eq!((s.ahead(), s.behind()), (1, 0)),
            RepoState::Behind => assert_eq!((s.ahead(), s.behind()), (0, 1)),
            RepoState::Diverged => assert_eq!((s.ahead(), s.behind()), (1, 1)),
            RepoState::Detached => {
                assert!(s.head.branch().is_none());
                // Checked out at `main~1`: `main` and `origin/main` contain it.
                let labels: Vec<String> = s
                    .containing_branches
                    .iter()
                    .map(ContainingBranch::label)
                    .collect();
                assert_eq!(labels, ["main~1", "origin/main~1"]);
            }
            RepoState::Stashed => assert_eq!(s.stash_count, 1),
            RepoState::MergeConflict => {
                assert_eq!(s.in_progress, Some(RepoOp::Merge));
                assert_eq!(s.changes.conflicted, 1);
            }
        }
        assert!(
            s.describe
                .as_deref()
                .unwrap_or_default()
                .starts_with("v1.0.0"),
            "{:?}",
            s.describe
        );
    }
}

#[test]
fn changes_agree() {
    let (cli, gix) = (GitCli::default(), GixReader::default());
    for repo in &fixture().repos {
        let path = RepoPath::new(&repo.path);
        let mut a = cli.changes(&path).unwrap();
        let mut b = gix.changes(&path).unwrap();
        a.sort_by(|x, y| x.path.cmp(&y.path));
        b.sort_by(|x, y| x.path.cmp(&y.path));
        assert_eq!(a, b, "{:?}", repo.state);
    }
}

#[test]
fn refs_agree() {
    let (cli, gix) = (GitCli::default(), GixReader::default());
    for repo in &fixture().repos {
        let path = RepoPath::new(&repo.path);
        let mut a = cli.branches(&path).unwrap();
        let mut b = gix.branches(&path).unwrap();
        a.sort_by(|x, y| x.name.cmp(&y.name));
        b.sort_by(|x, y| x.name.cmp(&y.name));
        assert_eq!(a, b, "{:?}", repo.state);
        assert_eq!(cli.tags(&path).unwrap(), gix.tags(&path).unwrap());
        assert_eq!(cli.stashes(&path).unwrap(), gix.stashes(&path).unwrap());
        let mut ta = cli.ref_tips(&path).unwrap();
        let mut tb = gix.ref_tips(&path).unwrap();
        ta.sort_by(|x, y| x.name.cmp(&y.name));
        tb.sort_by(|x, y| x.name.cmp(&y.name));
        assert_eq!(ta, tb);
        for rev in ["HEAD", "main", "v1.0.0", "does-not-exist"] {
            assert_eq!(
                cli.resolve(&path, rev).unwrap(),
                gix.resolve(&path, rev).unwrap(),
                "{rev}"
            );
        }
        let names = vec![
            "refs/heads/main".to_owned(),
            "refs/heads/missing".to_owned(),
        ];
        assert_eq!(
            cli.snapshot(&path, &names).unwrap(),
            gix.snapshot(&path, &names).unwrap()
        );
    }
}

fn walk(reader: &dyn HistoryReader, path: &RepoPath) -> Vec<archipel_model::history::TopologyNode> {
    let mut nodes = Vec::new();
    reader
        .walk_topology(
            path,
            WalkOrder::Topological,
            &mut |n| {
                nodes.push(n);
                ControlFlow::Continue(())
            },
            &CancelToken::new(),
        )
        .unwrap();
    nodes
}

#[test]
fn history_agrees_and_is_topological() {
    let (cli, gix) = (GitCli::default(), GixReader::default());
    for repo in &fixture().repos {
        let path = RepoPath::new(&repo.path);
        let a = walk(&cli, &path);
        let b = walk(&gix, &path);
        let set_a: HashSet<_> = a.iter().map(|n| (n.oid, n.parents.clone())).collect();
        let set_b: HashSet<_> = b.iter().map(|n| (n.oid, n.parents.clone())).collect();
        assert_eq!(set_a, set_b, "{:?}", repo.state);
        // Children always come before their parents.
        let mut seen = HashSet::new();
        for node in &b {
            for p in &node.parents {
                assert!(!seen.contains(p), "parent emitted before child");
            }
            seen.insert(node.oid);
        }
        let oids: Vec<_> = b.iter().take(5).map(|n| n.oid).collect();
        assert_eq!(
            cli.commit_meta(&path, &oids).unwrap(),
            gix.commit_meta(&path, &oids).unwrap()
        );
        let (first, last) = (b[0].oid, b[b.len() - 1].oid);
        assert_eq!(
            cli.is_ancestor(&path, last, first).unwrap(),
            gix.is_ancestor(&path, last, first).unwrap()
        );
        assert_eq!(
            cli.is_ancestor(&path, first, last).unwrap(),
            gix.is_ancestor(&path, first, last).unwrap()
        );
    }
}

#[test]
fn walk_skips_unlabelled_refs() {
    let repo = archipel_fixtures::TestRepo::new().unwrap();
    repo.write("a.txt", "a").unwrap();
    let base = repo.commit_all("base").unwrap();
    repo.write("a.txt", "rewritten away").unwrap();
    let old = repo.commit_all("old").unwrap();
    // A `filter-branch` backup, a note and a stash on the old commit.
    repo.git(&["update-ref", "refs/original/refs/heads/main", &old])
        .unwrap();
    repo.git(&["notes", "add", "-m", "n", &old]).unwrap();
    repo.write("a.txt", "dirty").unwrap();
    repo.git(&["stash"]).unwrap();
    repo.git(&["reset", "-q", "--hard", &base]).unwrap();
    let path = RepoPath::new(repo.path());
    let base = archipel_model::Oid::from_hex(&base).unwrap();
    for reader in [
        &GitCli::default() as &dyn HistoryReader,
        &GixReader::default(),
    ] {
        let oids: Vec<_> = walk(reader, &path).iter().map(|n| n.oid).collect();
        assert_eq!(oids, vec![base]);
    }
}

#[test]
fn walk_can_stop_early_and_be_cancelled() {
    let path = RepoPath::new(&fixture().repos[0].path);
    for reader in [
        &GitCli::default() as &dyn HistoryReader,
        &GixReader::default(),
    ] {
        let mut count = 0;
        reader
            .walk_topology(
                &path,
                WalkOrder::Topological,
                &mut |_| {
                    count += 1;
                    if count == 2 {
                        ControlFlow::Break(())
                    } else {
                        ControlFlow::Continue(())
                    }
                },
                &CancelToken::new(),
            )
            .unwrap();
        assert_eq!(count, 2);
        let cancel = CancelToken::new();
        cancel.cancel();
        let r = reader.walk_topology(
            &path,
            WalkOrder::Topological,
            &mut |_| ControlFlow::Continue(()),
            &cancel,
        );
        assert!(matches!(r, Err(archipel_model::GitError::Cancelled)));
    }
}

#[test]
fn missing_repo_is_reported() {
    let dir = tempfile::tempdir().unwrap();
    let path = RepoPath::new(dir.path());
    assert!(matches!(
        GitCli::default().summary(&path),
        Err(archipel_model::GitError::NotARepository { .. })
    ));
    assert!(matches!(
        GixReader::default().summary(&path),
        Err(archipel_model::GitError::NotARepository { .. })
    ));
}

#[test]
fn cached_gix_handles_see_new_commits_refs_and_config() {
    let repo = archipel_fixtures::TestRepo::new().unwrap();
    repo.write("a.txt", "a").unwrap();
    repo.commit_all("one").unwrap();
    let path = RepoPath::new(repo.path());
    let gix = GixReader::default();
    let first = gix.summary(&path).unwrap();
    assert!(first.upstream.is_none());
    // New commit and new branch, written by the CLI behind the cached handle.
    repo.write("b.txt", "b").unwrap();
    let new = archipel_model::Oid::from_hex(&repo.commit_all("two").unwrap()).unwrap();
    repo.git(&["branch", "side"]).unwrap();
    assert_eq!(gix.summary(&path).unwrap().head_oid, Some(new));
    assert_eq!(gix.commit_meta(&path, &[new]).unwrap()[0].summary, "two");
    assert!(
        gix.branches(&path)
            .unwrap()
            .iter()
            .any(|b| b.name == "side")
    );
    // Repacking moves objects into a new pack.
    repo.git(&["gc", "-q"]).unwrap();
    assert_eq!(gix.commit_meta(&path, &[new]).unwrap()[0].summary, "two");
    // Config edit (new upstream) is picked up.
    repo.git(&["update-ref", "refs/remotes/origin/main", "HEAD"])
        .unwrap();
    repo.git(&["config", "remote.origin.url", "https://example.com/r.git"])
        .unwrap();
    repo.git(&[
        "config",
        "remote.origin.fetch",
        "+refs/heads/*:refs/remotes/origin/*",
    ])
    .unwrap();
    repo.git(&["branch", "--set-upstream-to=origin/main"])
        .unwrap();
    assert_eq!(
        gix.summary(&path).unwrap().upstream.unwrap().name,
        "origin/main"
    );
}
