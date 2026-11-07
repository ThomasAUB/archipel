//! Undo / redo of every operation type of PLAN §2.4, on real repositories.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use archipel_fixtures::{TestRepo, git};
use archipel_git::GitCli;
use archipel_model::journal::{EntryState, OperationKind};
use archipel_model::ports::{JournalStore, RefReader, StashReader, StatusReader};
use archipel_model::requests::{
    CheckoutTarget, CommitMode, CommitRequest, DeleteMode, MergeMode, PullMode, PushRequest,
    ResetMode, StashApplyMode, StashRequest, TagRequest,
};
use archipel_model::testing::InMemoryStore;
use archipel_model::{CancelToken, NoProgress, Oid, RepoPath};
use archipel_ops::undo::UndoStep;
use archipel_ops::{
    CheckoutOutcome, Compensation, Direction, GitPorts, Journal, OpError, RepoActions, UndoPlan,
    UndoService,
};

struct Env {
    repo: TestRepo,
    path: RepoPath,
    cli: Arc<GitCli>,
    store: Arc<InMemoryStore>,
    actions: RepoActions,
    undo: UndoService,
}

fn env() -> Env {
    let repo = TestRepo::new().unwrap();
    repo.write("a.txt", "one\n").unwrap();
    repo.commit_all("initial").unwrap();
    let path = RepoPath::new(repo.path());
    let cli = Arc::new(GitCli::default());
    let store = Arc::new(InMemoryStore::new());
    let ports = GitPorts::from_backend(cli.clone());
    let journal = Journal::new(store.clone(), cli.clone(), cli.clone(), 200);
    Env {
        actions: RepoActions::new(ports.clone(), journal.clone()),
        undo: UndoService::new(ports, journal),
        repo,
        path,
        cli,
        store,
    }
}

impl Env {
    fn head(&self) -> Oid {
        self.cli.resolve(&self.path, "HEAD").unwrap().unwrap()
    }
    fn last(&self) -> archipel_model::journal::JournalEntry {
        self.store.recent(self.path.id(), 1).unwrap().remove(0)
    }
    fn undo_last(&self) -> Result<Option<String>, OpError> {
        let e = self.undo.next_undo(self.path.id(), 50).unwrap().unwrap();
        self.undo.execute(&self.path, &e, Direction::Undo)
    }
    fn redo_last(&self) -> Result<Option<String>, OpError> {
        let e = self.undo.next_redo(self.path.id(), 50).unwrap().unwrap();
        self.undo.execute(&self.path, &e, Direction::Redo)
    }
    fn commit(&self, file: &str, msg: &str) -> Oid {
        self.repo.write(file, msg).unwrap();
        self.actions.stage(&self.path, &[file.into()]).unwrap();
        self.actions
            .commit(
                &self.path,
                &CommitRequest {
                    message: msg.into(),
                    mode: CommitMode::New,
                },
                None,
            )
            .unwrap()
    }
}

#[test]
fn commit_undo_redo_keeps_changes() {
    let e = env();
    let before = e.head();
    let new = e.commit("b.txt", "add b");
    e.undo_last().unwrap();
    assert_eq!(e.head(), before);
    // Back to the state before the commit: the change is staged again.
    assert!(e.repo.path().join("b.txt").exists());
    assert_eq!(e.cli.summary(&e.path).unwrap().changes.staged, 1);
    assert_eq!(e.last().state, EntryState::Undone);
    e.redo_last().unwrap();
    assert_eq!(e.head(), new);
}

#[test]
fn amend_undo() {
    let e = env();
    let before = e.head();
    e.actions
        .commit(
            &e.path,
            &CommitRequest {
                message: "reworded".into(),
                mode: CommitMode::Amend,
            },
            None,
        )
        .unwrap();
    assert_ne!(e.head(), before);
    e.undo_last().unwrap();
    assert_eq!(e.head(), before);
}

#[test]
fn undo_is_refused_when_branch_moved() {
    let e = env();
    e.commit("b.txt", "b");
    let entry = e.last();
    // Someone commits outside of the app.
    e.repo.write("c.txt", "c").unwrap();
    e.repo.commit_all("external").unwrap();
    match e.undo.execute(&e.path, &entry, Direction::Undo) {
        Err(OpError::Refused(msg)) => assert!(msg.contains("moved"), "{msg}"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn merge_rebase_cherry_pick_revert_reset_undo() {
    let e = env();
    let base = e.head();
    e.repo.git(&["checkout", "-q", "-b", "topic"]).unwrap();
    let topic = e.commit("t.txt", "topic");
    e.repo.git(&["checkout", "-q", "main"]).unwrap();
    e.commit("m.txt", "main work");
    let main_tip = e.head();

    e.actions
        .merge(&e.path, "topic", MergeMode::NoFastForward)
        .unwrap();
    e.undo_last().unwrap();
    assert_eq!(e.head(), main_tip);

    e.actions.cherry_pick(&e.path, &[topic]).unwrap();
    e.undo_last().unwrap();
    assert_eq!(e.head(), main_tip);

    e.actions.revert(&e.path, main_tip).unwrap();
    e.undo_last().unwrap();
    assert_eq!(e.head(), main_tip);

    e.actions.rebase(&e.path, "topic").unwrap();
    assert_ne!(e.head(), main_tip);
    e.undo_last().unwrap();
    assert_eq!(e.head(), main_tip);

    // Hard reset loses a local change; undo brings back the commit and the change.
    e.repo.write("a.txt", "uncommitted\n").unwrap();
    e.actions
        .reset(&e.path, &base.to_hex(), ResetMode::Hard)
        .unwrap();
    assert_eq!(e.head(), base);
    e.undo_last().unwrap();
    assert_eq!(e.head(), main_tip);
    assert_eq!(
        std::fs::read_to_string(e.repo.path().join("a.txt")).unwrap(),
        "uncommitted\n"
    );
}

#[test]
fn checkout_and_branch_and_tag_undo() {
    let e = env();
    e.actions
        .create_branch(&e.path, "feature", None, None)
        .unwrap();
    e.actions
        .checkout(&e.path, &CheckoutTarget::Branch("feature".into()), None)
        .unwrap();
    e.undo_last().unwrap();
    assert_eq!(e.cli.summary(&e.path).unwrap().head.branch(), Some("main"));
    e.redo_last().unwrap();
    assert_eq!(
        e.cli.summary(&e.path).unwrap().head.branch(),
        Some("feature")
    );
    e.repo.git(&["checkout", "-q", "main"]).unwrap();

    e.actions.rename_branch(&e.path, "feature", "feat").unwrap();
    e.undo_last().unwrap();
    assert!(
        e.cli
            .resolve(&e.path, "refs/heads/feature")
            .unwrap()
            .is_some()
    );
    assert!(e.cli.resolve(&e.path, "refs/heads/feat").unwrap().is_none());

    let tip = e.cli.resolve(&e.path, "feature").unwrap();
    e.actions
        .delete_branch(&e.path, "feature", DeleteMode::Force, None)
        .unwrap();
    e.undo_last().unwrap();
    assert_eq!(e.cli.resolve(&e.path, "feature").unwrap(), tip);

    e.actions
        .create_tag(
            &e.path,
            &TagRequest {
                name: "v1".into(),
                target: None,
                message: Some("annotated".into()),
            },
            None,
        )
        .unwrap();
    let tag_object = e.repo.git(&["rev-parse", "refs/tags/v1"]).unwrap();
    e.actions.delete_tag(&e.path, "v1").unwrap();
    e.undo_last().unwrap();
    // The annotated tag object itself is restored, not just its commit.
    assert_eq!(
        e.repo.git(&["rev-parse", "refs/tags/v1"]).unwrap(),
        tag_object
    );
    e.undo_last().unwrap(); // undo the creation
    assert!(e.cli.tags(&e.path).unwrap().is_empty());
}

#[test]
fn track_remote_checkout_undo_deletes_created_branch() {
    let e = env();
    e.repo
        .git(&["remote", "add", "origin", "https://example.com/r.git"])
        .unwrap();
    e.repo
        .git(&["update-ref", "refs/remotes/origin/dev", "HEAD"])
        .unwrap();
    e.actions
        .checkout(
            &e.path,
            &CheckoutTarget::TrackRemote("origin/dev".into()),
            None,
        )
        .unwrap();
    assert_eq!(e.cli.summary(&e.path).unwrap().head.branch(), Some("dev"));
    e.undo_last().unwrap();
    assert_eq!(e.cli.summary(&e.path).unwrap().head.branch(), Some("main"));
    assert!(e.cli.resolve(&e.path, "refs/heads/dev").unwrap().is_none());
}

#[test]
fn stash_and_discard_undo() {
    let e = env();
    e.repo.write("a.txt", "work in progress\n").unwrap();
    assert!(
        e.actions
            .stash_push(&e.path, &StashRequest::default())
            .unwrap()
    );
    e.undo_last().unwrap(); // pops it back
    assert_eq!(
        std::fs::read_to_string(e.repo.path().join("a.txt")).unwrap(),
        "work in progress\n"
    );
    assert!(e.cli.stashes(&e.path).unwrap().is_empty());

    e.actions
        .stash_push(
            &e.path,
            &StashRequest {
                message: Some("keep".into()),
                ..Default::default()
            },
        )
        .unwrap();
    let oid = e.cli.stashes(&e.path).unwrap()[0].oid;
    e.actions.stash_drop(&e.path, 0).unwrap();
    e.undo_last().unwrap();
    assert_eq!(e.cli.stashes(&e.path).unwrap()[0].oid, oid);
    e.redo_last().unwrap();
    assert!(e.cli.stashes(&e.path).unwrap().is_empty());
    e.undo_last().unwrap(); // the drop again: back in the list

    e.actions
        .stash_apply(&e.path, 0, StashApplyMode::Pop)
        .unwrap();
    let note = e.undo_last().unwrap();
    assert!(note.is_some());
    assert_eq!(e.cli.stashes(&e.path).unwrap().len(), 1);

    e.actions.discard(&e.path, &["a.txt".into()]).unwrap();
    assert_eq!(
        std::fs::read_to_string(e.repo.path().join("a.txt")).unwrap(),
        "one\n"
    );
    e.undo_last().unwrap();
    assert_eq!(
        std::fs::read_to_string(e.repo.path().join("a.txt")).unwrap(),
        "work in progress\n"
    );
}

#[test]
fn discard_undo_restores_only_the_discarded_files() {
    let e = env();
    e.repo.write("b.txt", "bee\n").unwrap();
    e.repo.write("gone.txt", "gone\n").unwrap();
    e.repo.commit_all("more").unwrap();
    let read = |f: &str| std::fs::read_to_string(e.repo.path().join(f)).ok();
    // Another file stays modified: a whole-worktree snapshot cannot be
    // re-applied over it.
    e.repo.write("a.txt", "edited a\n").unwrap();
    e.repo.write("b.txt", "edited b\n").unwrap();
    e.repo.write("dir/new.txt", "untracked\n").unwrap();
    std::fs::remove_file(e.repo.path().join("gone.txt")).unwrap();
    let discarded: Vec<String> = vec!["a.txt".into(), "dir/".into(), "gone.txt".into()];
    e.actions.discard(&e.path, &discarded).unwrap();
    assert_eq!(read("a.txt").as_deref(), Some("one\n"));
    assert_eq!(read("dir/new.txt"), None);
    assert_eq!(read("gone.txt").as_deref(), Some("gone\n"));

    e.undo_last().unwrap();
    assert_eq!(read("a.txt").as_deref(), Some("edited a\n"));
    assert_eq!(read("b.txt").as_deref(), Some("edited b\n"));
    assert_eq!(read("dir/new.txt").as_deref(), Some("untracked\n"));
    assert_eq!(read("gone.txt"), None);
    assert!(e.cli.stashes(&e.path).unwrap().is_empty());

    // A file edited again after the discard is never overwritten.
    e.actions.discard(&e.path, &["a.txt".into()]).unwrap();
    e.repo.write("a.txt", "newer\n").unwrap();
    assert!(e.undo_last().is_err());
    assert_eq!(read("a.txt").as_deref(), Some("newer\n"));
}

#[test]
fn fast_forward_and_pull_undo_and_push_compensation() {
    let e = env();
    let remote_dir = tempfile::tempdir().unwrap();
    let remote = remote_dir.path().join("r.git");
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
    e.repo
        .git(&["remote", "add", "origin", remote.to_str().unwrap()])
        .unwrap();
    let none = CancelToken::new();
    e.actions
        .push(
            &e.path,
            &PushRequest {
                refspec: Some("main".into()),
                set_upstream: true,
                ..Default::default()
            },
            &NoProgress,
            &none,
            None,
        )
        .unwrap();
    let pushed_base = e.head();

    // Fast-forward a branch that is not checked out.
    e.repo.git(&["branch", "old", "HEAD"]).unwrap();
    let tip = e.commit("b.txt", "b");
    e.actions.fast_forward(&e.path, "old", tip).unwrap();
    assert_eq!(e.cli.resolve(&e.path, "old").unwrap(), Some(tip));
    e.undo_last().unwrap();
    assert_eq!(e.cli.resolve(&e.path, "old").unwrap(), Some(pushed_base));

    // Push, then compensate with force-with-lease.
    e.actions
        .push(
            &e.path,
            &PushRequest {
                refspec: Some("main".into()),
                ..Default::default()
            },
            &NoProgress,
            &none,
            None,
        )
        .unwrap();
    let entry = e.last();
    assert_eq!(entry.kind, OperationKind::Push);
    let UndoPlan::Compensate(options) = e.undo.plan(&e.path, &entry, Direction::Undo).unwrap()
    else {
        panic!()
    };
    assert!(matches!(options[0], Compensation::RevertCommits(ref c) if c == &vec![tip]));
    assert!(
        e.undo.execute(&e.path, &entry, Direction::Undo).is_err(),
        "push needs explicit compensation"
    );
    let lease = options
        .iter()
        .find(|o| matches!(o, Compensation::ForceWithLease { .. }))
        .unwrap();
    e.undo
        .compensate(&e.path, &entry, lease, &NoProgress, &none)
        .unwrap();
    assert_eq!(
        git(&remote, &["rev-parse", "main"]).unwrap(),
        pushed_base.to_hex()
    );

    // Pull brings the remote back down (fast-forward) and is undoable.
    e.repo
        .git(&["reset", "-q", "--hard", &pushed_base.to_hex()])
        .unwrap();
    e.repo
        .git(&[
            "push",
            "-q",
            "-f",
            "origin",
            &format!("{tip}:refs/heads/main"),
        ])
        .unwrap();
    e.actions
        .pull(&e.path, PullMode::FastForwardOnly, &NoProgress, &none, None)
        .unwrap();
    assert_eq!(e.head(), tip);
    e.undo_last().unwrap();
    assert_eq!(e.head(), pushed_base);
}

#[test]
fn commit_undo_plan_moves_branch_softly() {
    let e = env();
    let before = e.head();
    e.commit("x.txt", "x");
    let plan = e.undo.plan(&e.path, &e.last(), Direction::Undo).unwrap();
    assert_eq!(
        plan,
        UndoPlan::Steps {
            steps: vec![UndoStep::ResetSoft(before)],
            note: None
        }
    );
}

/// Delegates to the CLI, failing the `fail_at`-th ref update (1-based).
struct FlakyRefWriter {
    cli: Arc<GitCli>,
    fail_at: usize,
    calls: std::sync::atomic::AtomicUsize,
}

impl archipel_model::ports::RefWriter for FlakyRefWriter {
    fn update_ref(
        &self,
        repo: &RepoPath,
        name: &str,
        to: Option<Oid>,
        expected: Option<Oid>,
    ) -> Result<(), archipel_model::GitError> {
        let n = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        if n == self.fail_at {
            return Err(archipel_model::GitError::Unsupported("injected failure"));
        }
        self.cli.update_ref(repo, name, to, expected)
    }
    fn set_symbolic_head(
        &self,
        repo: &RepoPath,
        branch_ref: &str,
    ) -> Result<(), archipel_model::GitError> {
        self.cli.set_symbolic_head(repo, branch_ref)
    }
    fn reset(
        &self,
        repo: &RepoPath,
        target: &str,
        mode: ResetMode,
    ) -> Result<(), archipel_model::GitError> {
        self.cli.reset(repo, target, mode)
    }
}

#[test]
fn undo_failing_midway_restores_the_refs_and_can_be_retried() {
    let e = env();
    e.repo.git(&["branch", "feature"]).unwrap();
    e.actions.rename_branch(&e.path, "feature", "feat").unwrap();
    let tip = e.cli.resolve(&e.path, "refs/heads/feat").unwrap();

    let mut ports = GitPorts::from_backend(e.cli.clone());
    ports.ref_writer = Arc::new(FlakyRefWriter {
        cli: e.cli.clone(),
        fail_at: 2,
        calls: std::sync::atomic::AtomicUsize::new(0),
    });
    let journal = Journal::new(e.store.clone(), e.cli.clone(), e.cli.clone(), 200);
    let flaky = UndoService::new(ports, journal);
    let entry = flaky.next_undo(e.path.id(), 50).unwrap().unwrap();
    assert!(flaky.execute(&e.path, &entry, Direction::Undo).is_err());

    // Rolled back to the state after the rename, entry still in effect.
    assert_eq!(e.cli.resolve(&e.path, "refs/heads/feat").unwrap(), tip);
    assert!(
        e.cli
            .resolve(&e.path, "refs/heads/feature")
            .unwrap()
            .is_none()
    );
    assert_eq!(e.last().state, EntryState::Done);

    e.undo_last().unwrap();
    assert_eq!(e.cli.resolve(&e.path, "refs/heads/feature").unwrap(), tip);
    assert!(e.cli.resolve(&e.path, "refs/heads/feat").unwrap().is_none());
}

#[test]
fn remote_branch_of_a_remote_named_with_a_slash_is_deleted() {
    let e = env();
    let remote_dir = tempfile::tempdir().unwrap();
    let bare = remote_dir.path().join("fork.git");
    git(
        remote_dir.path(),
        &["init", "-q", "--bare", bare.to_str().unwrap()],
    )
    .unwrap();
    e.repo
        .git(&["remote", "add", "team/fork", bare.to_str().unwrap()])
        .unwrap();
    e.repo
        .git(&["push", "-q", "team/fork", "HEAD:refs/heads/feat"])
        .unwrap();
    e.repo.git(&["fetch", "-q", "team/fork"]).unwrap();
    e.actions
        .delete_remote_branch(&e.path, "team/fork/feat", &NoProgress, &CancelToken::new())
        .unwrap();
    let left = git(&bare, &["branch", "--list", "feat"]).unwrap();
    assert!(left.trim().is_empty(), "{left}");
}

impl Env {
    fn read(&self, file: &str) -> String {
        std::fs::read_to_string(self.repo.path().join(file)).unwrap()
    }
    /// A `feature` branch whose single commit writes `file`, back on `main`.
    fn feature_writing(&self, file: &str, contents: &str) {
        self.repo.git(&["checkout", "-q", "-b", "feature"]).unwrap();
        self.commit(file, contents);
        self.repo.git(&["checkout", "-q", "main"]).unwrap();
    }
    fn checkout_feature(&self) -> CheckoutOutcome {
        self.actions
            .checkout_autostash(&self.path, &CheckoutTarget::Branch("feature".into()))
            .unwrap()
    }
    fn branch(&self) -> Option<String> {
        self.cli
            .summary(&self.path)
            .unwrap()
            .head
            .branch()
            .map(str::to_owned)
    }
}

#[test]
fn autostash_checkout_lets_git_carry_changes_it_can() {
    let e = env();
    e.feature_writing("b.txt", "b\n");
    e.repo.write("a.txt", "local\n").unwrap();
    assert_eq!(e.checkout_feature(), CheckoutOutcome::Switched);
    assert_eq!(e.read("a.txt"), "local\n");
    assert!(e.cli.stashes(&e.path).unwrap().is_empty());
}

#[test]
fn autostash_checkout_reapplies_changes_keeping_the_index() {
    let e = env();
    e.commit("a.txt", "1\n2\n3\n4\n5\n");
    e.feature_writing("a.txt", "1 feature\n2\n3\n4\n5\n");
    e.repo.write("a.txt", "1\n2\n3\n4\n5 local\n").unwrap();
    e.actions.stage(&e.path, &["a.txt".into()]).unwrap();
    e.repo.write("new.txt", "untracked\n").unwrap();

    assert_eq!(e.checkout_feature(), CheckoutOutcome::Reapplied);
    assert_eq!(e.branch().as_deref(), Some("feature"));
    assert_eq!(e.read("a.txt"), "1 feature\n2\n3\n4\n5 local\n");
    assert_eq!(e.read("new.txt"), "untracked\n");
    let changes = e.cli.changes(&e.path).unwrap();
    let a = changes.iter().find(|c| c.path == "a.txt").unwrap();
    assert!(a.is_staged() && !a.is_unstaged());
    assert!(e.cli.stashes(&e.path).unwrap().is_empty());
    assert!(e.last().stash.is_none());
}

#[test]
fn autostash_checkout_keeps_conflicting_changes_stashed_until_undo() {
    let e = env();
    e.feature_writing("a.txt", "feature\n");
    e.repo.write("a.txt", "local\n").unwrap();
    e.repo.write("new.txt", "untracked\n").unwrap();

    let CheckoutOutcome::Stashed(record) = e.checkout_feature() else {
        panic!("the changes conflict");
    };
    assert_eq!(e.branch().as_deref(), Some("feature"));
    assert_eq!(e.read("a.txt"), "feature\n");
    assert!(e.cli.changes(&e.path).unwrap().is_empty(), "left clean");
    let stashes = e.cli.stashes(&e.path).unwrap();
    assert_eq!(stashes.len(), 1);
    assert_eq!(stashes[0].oid, record.oid);
    assert!(stashes[0].message.contains("autostash"));
    assert_eq!(e.last().stash.map(|s| s.oid), Some(record.oid));

    e.undo_last().unwrap();
    assert_eq!(e.branch().as_deref(), Some("main"));
    assert_eq!(e.read("a.txt"), "local\n");
    assert_eq!(e.read("new.txt"), "untracked\n");
    assert!(e.cli.stashes(&e.path).unwrap().is_empty());
}

#[test]
fn autostash_checkout_cleans_untracked_files_of_a_failed_apply() {
    let e = env();
    e.feature_writing("u.txt", "tracked on feature\n");
    // Git refuses: the untracked file would be overwritten.
    e.repo.write("u.txt", "untracked here\n").unwrap();

    assert!(matches!(e.checkout_feature(), CheckoutOutcome::Stashed(_)));
    assert_eq!(e.read("u.txt"), "tracked on feature\n");
    assert!(e.cli.changes(&e.path).unwrap().is_empty());
    e.undo_last().unwrap();
    assert_eq!(e.read("u.txt"), "untracked here\n");
}

#[test]
fn autostash_checkout_of_a_missing_branch_leaves_changes_alone() {
    let e = env();
    e.repo.write("a.txt", "local\n").unwrap();
    assert!(
        e.actions
            .checkout_autostash(&e.path, &CheckoutTarget::Branch("nope".into()))
            .is_err()
    );
    assert_eq!(e.read("a.txt"), "local\n");
    assert!(e.cli.stashes(&e.path).unwrap().is_empty());
}
