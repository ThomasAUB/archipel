//! (Test file: unwrap/expect/panic are the expected failure mode in tests.)
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration tests of every write operation of the CLI adapter, on real
//! temporary repositories.

use std::ops::ControlFlow;

use archipel_fixtures::{TestRepo, git};
use archipel_git::GitCli;
use archipel_model::changes::ChangeKind;
use archipel_model::ports::{
    BranchWriter, ChurnReader, CommitWriter, DiffReader, DiffSide, HistoryRewriter, IndexWriter,
    RefReader, RefWriter, RemoteOps, RemoteReader, StashReader, StashWriter, StatusReader,
    TagWriter,
};
use archipel_model::requests::{
    CheckoutTarget, CommitMode, CommitRequest, DeleteMode, FetchRequest, ForceMode, MergeMode,
    PatchDirection, PullMode, PushRequest, ResetMode, SequenceAction, StashApplyMode, StashRequest,
    TagRequest, UntrackedMode,
};
use archipel_model::{CancelToken, GitError, NoProgress, Oid, Progress, RepoOp, RepoPath};

fn setup() -> (TestRepo, RepoPath, GitCli) {
    let repo = TestRepo::new().unwrap();
    repo.write("a.txt", "one\ntwo\nthree\n").unwrap();
    repo.commit_all("initial").unwrap();
    let path = RepoPath::new(repo.path());
    (repo, path, GitCli::default())
}

fn commit(cli: &GitCli, path: &RepoPath, message: &str) -> Result<Oid, GitError> {
    cli.commit(
        path,
        &CommitRequest {
            message: message.into(),
            mode: CommitMode::New,
        },
    )
}

#[test]
fn commit_and_amend() {
    let (repo, path, cli) = setup();
    repo.write("b.txt", "b\n").unwrap();
    cli.stage(&path, &["b.txt".into()]).unwrap();
    let oid = commit(&cli, &path, "add b\n\nbody line").unwrap();
    assert_eq!(cli.resolve(&path, "HEAD").unwrap(), Some(oid));
    assert_eq!(
        repo.git(&["log", "-1", "--format=%B"]).unwrap(),
        "add b\n\nbody line"
    );
    let amended = cli
        .commit(
            &path,
            &CommitRequest {
                message: "add b (amended)".into(),
                mode: CommitMode::Amend,
            },
        )
        .unwrap();
    assert_ne!(amended, oid);
    assert_eq!(repo.git(&["rev-list", "--count", "HEAD"]).unwrap(), "2");
}

#[test]
fn failing_hook_reports_its_output() {
    let (repo, path, cli) = setup();
    let hook = repo.path().join(".git/hooks/pre-commit");
    std::fs::write(
        &hook,
        "#!/bin/sh\necho 'lint failed: missing semicolon' >&2\nexit 1\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    repo.write("b.txt", "b\n").unwrap();
    cli.stage(&path, &["b.txt".into()]).unwrap();
    match commit(&cli, &path, "blocked") {
        Err(GitError::CommandFailed {
            stderr, command, ..
        }) => {
            assert!(stderr.contains("lint failed"), "{stderr}");
            assert!(command.starts_with("git commit"));
        }
        other => panic!("expected hook failure, got {other:?}"),
    }
}

#[test]
fn stage_unstage_discard() {
    let (repo, path, cli) = setup();
    repo.write("a.txt", "changed\n").unwrap();
    repo.write("new.txt", "new\n").unwrap();
    cli.stage(&path, &["a.txt".into()]).unwrap();
    let s = cli.summary(&path).unwrap();
    assert_eq!((s.changes.staged, s.changes.untracked), (1, 1));
    cli.unstage(&path, &["a.txt".into()]).unwrap();
    assert_eq!(cli.summary(&path).unwrap().changes.modified, 1);
    cli.discard(&path, &["a.txt".into(), "new.txt".into()])
        .unwrap();
    assert!(!cli.summary(&path).unwrap().changes.is_dirty());
    assert_eq!(
        std::fs::read_to_string(repo.path().join("a.txt")).unwrap(),
        "one\ntwo\nthree\n"
    );
    assert!(!repo.path().join("new.txt").exists());
}

#[test]
fn unstage_on_unborn_head() {
    let repo = TestRepo::new().unwrap();
    let path = RepoPath::new(repo.path());
    let cli = GitCli::default();
    repo.write("x.txt", "x\n").unwrap();
    cli.stage(&path, &["x.txt".into()]).unwrap();
    cli.unstage(&path, &["x.txt".into()]).unwrap();
    assert_eq!(cli.summary(&path).unwrap().changes.untracked, 1);
}

#[test]
fn partial_staging_with_patches() {
    let (repo, path, cli) = setup();
    repo.write("a.txt", "ONE\ntwo\nTHREE\n").unwrap();
    // Stage only the first change: a patch with a single hunk.
    let first_hunk =
        "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1,2 +1,2 @@\n-one\n+ONE\n two\n";
    cli.apply_patch(&path, first_hunk, PatchDirection::Forward)
        .unwrap();
    let staged = cli.worktree_diff(&path, DiffSide::Staged, None).unwrap();
    assert_eq!(staged[0].line_stats(), (1, 1));
    let unstaged = cli
        .worktree_diff(&path, DiffSide::Unstaged, Some("a.txt"))
        .unwrap();
    assert_eq!(unstaged[0].line_stats(), (1, 1));
    assert!(
        unstaged[0].hunks[0]
            .lines
            .iter()
            .any(|l| l.content == "THREE")
    );
    cli.apply_patch(&path, first_hunk, PatchDirection::Reverse)
        .unwrap();
    assert!(
        cli.worktree_diff(&path, DiffSide::Staged, None)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn branches_and_checkout() {
    let (repo, path, cli) = setup();
    cli.create_branch(&path, "feature", None).unwrap();
    cli.checkout(&path, &CheckoutTarget::Branch("feature".into()))
        .unwrap();
    assert_eq!(cli.summary(&path).unwrap().head.branch(), Some("feature"));
    cli.rename_branch(&path, "feature", "feat").unwrap();
    assert_eq!(cli.summary(&path).unwrap().head.branch(), Some("feat"));
    cli.checkout(&path, &CheckoutTarget::Branch("main".into()))
        .unwrap();
    repo.write("f.txt", "f\n").unwrap();
    repo.git(&["checkout", "-q", "feat"]).unwrap();
    repo.commit_all("on feat").unwrap();
    repo.git(&["checkout", "-q", "main"]).unwrap();
    let err = cli
        .delete_branch(&path, "feat", DeleteMode::Safe)
        .unwrap_err();
    assert!(matches!(err, GitError::CommandFailed { .. }));
    cli.delete_branch(&path, "feat", DeleteMode::Force).unwrap();
    assert!(
        cli.branches(&path)
            .unwrap()
            .iter()
            .all(|b| b.name != "feat")
    );
    let head = cli.resolve(&path, "HEAD").unwrap().unwrap();
    cli.checkout(&path, &CheckoutTarget::Detached(head))
        .unwrap();
    assert!(cli.summary(&path).unwrap().head.branch().is_none());
}

#[test]
fn tags_refs_and_reset() {
    let (repo, path, cli) = setup();
    let first = cli.resolve(&path, "HEAD").unwrap().unwrap();
    cli.create_tag(
        &path,
        &TagRequest {
            name: "v1".into(),
            target: None,
            message: Some("release".into()),
        },
    )
    .unwrap();
    cli.create_tag(
        &path,
        &TagRequest {
            name: "light".into(),
            target: None,
            message: None,
        },
    )
    .unwrap();
    let tags = cli.tags(&path).unwrap();
    assert!(
        tags.iter()
            .any(|t| t.name == "v1" && t.annotated && t.target == first)
    );
    assert!(tags.iter().any(|t| t.name == "light" && !t.annotated));
    cli.delete_tag(&path, "light").unwrap();
    assert_eq!(cli.tags(&path).unwrap().len(), 1);

    repo.write("b.txt", "b\n").unwrap();
    let second = Oid::from_hex(&repo.commit_all("second").unwrap()).unwrap();
    cli.reset(&path, &first.to_hex(), ResetMode::Keep).unwrap();
    assert_eq!(cli.resolve(&path, "HEAD").unwrap(), Some(first));
    cli.update_ref(&path, "refs/heads/main", Some(second), Some(first))
        .unwrap();
    assert_eq!(cli.resolve(&path, "main").unwrap(), Some(second));
    // Wrong expected old value is refused.
    assert!(
        cli.update_ref(&path, "refs/heads/main", Some(first), Some(first))
            .is_err()
    );
    cli.update_ref(&path, "refs/heads/tmp", Some(first), None)
        .unwrap();
    cli.update_ref(&path, "refs/heads/tmp", None, None).unwrap();
    assert_eq!(cli.resolve(&path, "tmp").unwrap(), None);
    cli.set_symbolic_head(&path, "refs/heads/main").unwrap();
    let snap = cli.snapshot(&path, &["refs/heads/main".into()]).unwrap();
    assert_eq!(snap.target("refs/heads/main"), Some(second));
}

#[test]
fn stash_lifecycle() {
    let (repo, path, cli) = setup();
    assert!(
        !cli.stash_push(&path, &StashRequest::default()).unwrap(),
        "nothing to stash"
    );
    repo.write("a.txt", "wip\n").unwrap();
    repo.write("u.txt", "untracked\n").unwrap();
    assert!(
        cli.stash_push(
            &path,
            &StashRequest {
                message: Some("wip".into()),
                untracked: UntrackedMode::Include
            }
        )
        .unwrap()
    );
    let stashes = cli.stashes(&path).unwrap();
    assert_eq!(stashes.len(), 1);
    assert!(stashes[0].message.contains("wip"));
    assert_eq!(Some(stashes[0].base), cli.summary(&path).unwrap().head_oid);
    assert!(!cli.summary(&path).unwrap().changes.is_dirty());
    cli.stash_apply(&path, 0, StashApplyMode::Keep).unwrap();
    assert_eq!(cli.stashes(&path).unwrap().len(), 1);
    cli.discard(&path, &["a.txt".into(), "u.txt".into()])
        .unwrap();
    let dropped = cli.stashes(&path).unwrap()[0].clone();
    cli.drop_stash(&path, 0).unwrap();
    assert!(cli.stashes(&path).unwrap().is_empty());
    cli.stash_store(&path, dropped.oid, "restored").unwrap();
    assert_eq!(cli.stashes(&path).unwrap()[0].oid, dropped.oid);
    cli.stash_apply(&path, 0, StashApplyMode::Pop).unwrap();
    assert!(cli.stashes(&path).unwrap().is_empty());
    // Snapshot does not touch the stash list.
    let snap = cli.snapshot_worktree(&path).unwrap();
    assert!(snap.is_some());
    assert!(cli.stashes(&path).unwrap().is_empty());
}

#[test]
fn remote_operations_with_progress() {
    let (repo, path, cli) = setup();
    let remote_dir = tempfile::tempdir().unwrap();
    let remote = remote_dir.path().join("remote.git");
    git(
        remote_dir.path(),
        &[
            "init",
            "-q",
            "--bare",
            "-b",
            "main",
            remote.to_str().unwrap(),
        ],
    )
    .unwrap();
    repo.git(&["remote", "add", "origin", remote.to_str().unwrap()])
        .unwrap();
    assert_eq!(cli.remotes(&path).unwrap()[0].name, "origin");

    let none = CancelToken::new();
    cli.push(
        &path,
        &PushRequest {
            refspec: Some("main".into()),
            set_upstream: true,
            ..Default::default()
        },
        &NoProgress,
        &none,
    )
    .unwrap();
    assert_eq!(
        cli.summary(&path).unwrap().upstream.unwrap().name,
        "origin/main"
    );

    // A teammate clone pushes a commit.
    let other_dir = tempfile::tempdir().unwrap();
    let other = other_dir.path().join("other");
    let events = std::sync::Mutex::new(Vec::<Progress>::new());
    let sink = |p: Progress| events.lock().unwrap().push(p);
    let cloned = cli
        .clone_repo(remote.to_str().unwrap(), &other, &sink, &none)
        .unwrap();
    archipel_fixtures::configure(cloned.as_path()).unwrap();
    std::fs::write(other.join("t.txt"), "teammate\n").unwrap();
    git(&other, &["add", "-A"]).unwrap();
    git(&other, &["commit", "-q", "-m", "teammate"]).unwrap();
    git(&other, &["push", "-q"]).unwrap();

    cli.fetch(
        &path,
        &FetchRequest {
            remote: Some("origin".into()),
            prune: true,
        },
        &NoProgress,
        &none,
    )
    .unwrap();
    let s = cli.summary(&path).unwrap();
    assert_eq!(s.behind(), 1);
    assert!(s.last_fetch.is_some());
    cli.pull(&path, PullMode::FastForwardOnly, &NoProgress, &none)
        .unwrap();
    assert_eq!(cli.summary(&path).unwrap().behind(), 0);

    // Rewrite local history, then force-with-lease: refused with a stale
    // expectation, accepted with the actual remote value.
    let remote_tip = cli.resolve(&path, "origin/main").unwrap();
    let stale = cli.resolve(&path, "HEAD~1").unwrap();
    repo.git(&["reset", "-q", "--hard", "HEAD~1"]).unwrap();
    repo.write("rewrite.txt", "x\n").unwrap();
    repo.commit_all("rewritten").unwrap();
    let push = |expected| PushRequest {
        refspec: Some("main".into()),
        force: ForceMode::WithLease(expected),
        ..Default::default()
    };
    assert!(cli.push(&path, &push(stale), &NoProgress, &none).is_err());
    cli.push(&path, &push(remote_tip), &NoProgress, &none)
        .unwrap();

    // A cancelled fetch reports cancellation.
    let cancelled = CancelToken::new();
    cancelled.cancel();
    assert!(matches!(
        cli.fetch(&path, &FetchRequest::default(), &NoProgress, &cancelled),
        Err(GitError::Cancelled)
    ));
}

#[test]
fn merge_rebase_cherry_pick_revert() {
    let (repo, path, cli) = setup();
    repo.git(&["checkout", "-q", "-b", "topic"]).unwrap();
    repo.write("t.txt", "t\n").unwrap();
    let topic = Oid::from_hex(&repo.commit_all("topic").unwrap()).unwrap();
    repo.git(&["checkout", "-q", "main"]).unwrap();
    cli.merge(&path, "topic", MergeMode::FastForwardOnly)
        .unwrap();
    assert_eq!(cli.resolve(&path, "HEAD").unwrap(), Some(topic));

    cli.revert(&path, topic).unwrap();
    assert!(!repo.path().join("t.txt").exists());
    cli.cherry_pick(&path, &[topic]).unwrap();
    assert!(repo.path().join("t.txt").exists());

    // Conflict, then abort.
    repo.git(&["checkout", "-q", "-b", "side", "HEAD~2"])
        .unwrap();
    repo.write("a.txt", "side\n").unwrap();
    repo.commit_all("side").unwrap();
    repo.git(&["checkout", "-q", "main"]).unwrap();
    repo.write("a.txt", "main\n").unwrap();
    repo.commit_all("main change").unwrap();
    assert!(cli.merge(&path, "side", MergeMode::NoFastForward).is_err());
    assert_eq!(cli.summary(&path).unwrap().in_progress, Some(RepoOp::Merge));
    assert!(cli.sequence(&path, SequenceAction::Skip).is_err());
    cli.sequence(&path, SequenceAction::Abort).unwrap();
    assert_eq!(cli.summary(&path).unwrap().in_progress, None);

    assert!(cli.rebase(&path, "side").is_err());
    assert_eq!(
        cli.summary(&path).unwrap().in_progress,
        Some(RepoOp::Rebase)
    );
    cli.sequence(&path, SequenceAction::Abort).unwrap();
    assert_eq!(cli.summary(&path).unwrap().in_progress, None);
}

#[test]
fn diffs_and_churn() {
    let (repo, path, cli) = setup();
    repo.write("a.txt", "one\n2\nthree\nfour\n").unwrap();
    repo.git(&["mv", "a.txt", "b.txt"]).unwrap();
    let head = Oid::from_hex(&repo.commit_all("edit and rename").unwrap()).unwrap();
    let diff = cli.commit_diff(&path, head).unwrap();
    assert_eq!(diff.len(), 1);
    assert_eq!(diff[0].kind, ChangeKind::Renamed);
    assert_eq!(diff[0].line_stats(), (2, 1));

    let mut commits = Vec::new();
    cli.walk_churn(
        &path,
        None,
        &mut |c| {
            commits.push(c);
            ControlFlow::Continue(())
        },
        &CancelToken::new(),
    )
    .unwrap();
    assert_eq!(commits.len(), 2);
    assert_eq!(commits[0].oid, head);
    assert!(commits[0].files.iter().any(|f| f.path == "b.txt"));
    let mut newer = 0;
    cli.walk_churn(
        &path,
        Some(head),
        &mut |_| {
            newer += 1;
            ControlFlow::Continue(())
        },
        &CancelToken::new(),
    )
    .unwrap();
    assert_eq!(newer, 0);
}

#[test]
fn branch_upstream_configuration() {
    let (repo, path, cli) = setup();
    repo.git(&["update-ref", "refs/remotes/origin/main", "HEAD"])
        .unwrap();
    repo.git(&["config", "remote.origin.url", "https://example.com/x.git"])
        .unwrap();
    repo.git(&[
        "config",
        "remote.origin.fetch",
        "+refs/heads/*:refs/remotes/origin/*",
    ])
    .unwrap();
    cli.set_upstream(&path, "main", Some("origin/main"))
        .unwrap();
    assert_eq!(
        cli.summary(&path).unwrap().upstream.unwrap().name,
        "origin/main"
    );
    cli.set_upstream(&path, "main", None).unwrap();
    assert!(cli.summary(&path).unwrap().upstream.is_none());
}
