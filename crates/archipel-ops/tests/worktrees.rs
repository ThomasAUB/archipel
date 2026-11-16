//! Worktree actions on real repositories (spec §10): create with local files
//! copied, remove saying what would be lost, undo bringing the files back
//! byte for byte, and the guard rails of undo.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use archipel_fixtures::TestRepo;
use archipel_git::GitCli;
use archipel_model::journal::OperationKind;
use archipel_model::ports::{JournalStore, RefReader, WorktreeReader};
use archipel_model::testing::InMemoryStore;
use archipel_model::worktree::{WorktreeAddRequest, WorktreeTarget};
use archipel_model::{CancelToken, NoProgress, RepoPath};
use archipel_ops::{
    Direction, GitPorts, Journal, OpError, RemoveOptions, RepoActions, UndoService, WorktreeSetup,
};

struct Env {
    repo: TestRepo,
    path: RepoPath,
    cli: Arc<GitCli>,
    store: Arc<InMemoryStore>,
    actions: RepoActions,
    undo: UndoService,
    /// Holds the worktrees, beside the repository.
    _dir: tempfile::TempDir,
    wt_root: PathBuf,
}

fn env() -> Env {
    let repo = TestRepo::new().unwrap();
    repo.write("a.txt", "one\n").unwrap();
    repo.write(".gitignore", ".env\n").unwrap();
    repo.commit_all("initial").unwrap();
    repo.write(".env", "SECRET=1\n").unwrap();
    let path = RepoPath::new(repo.path());
    let cli = Arc::new(GitCli::default());
    let store = Arc::new(InMemoryStore::new());
    let ports = GitPorts::from_backend(cli.clone());
    let journal = Journal::new(store.clone(), cli.clone(), cli.clone(), 200);
    let dir = tempfile::tempdir().unwrap();
    // A path with a space, as user folders often have.
    let wt_root = dir.path().join("my worktrees");
    Env {
        actions: RepoActions::new(ports.clone(), journal.clone()),
        undo: UndoService::new(ports, journal),
        repo,
        path,
        cli,
        store,
        _dir: dir,
        wt_root,
    }
}

impl Env {
    fn add(&self, name: &str, target: WorktreeTarget) -> RepoPath {
        let path = self.wt_root.join(name);
        self.actions
            .add_worktree(
                &self.path,
                &WorktreeAddRequest {
                    path: path.clone(),
                    target,
                },
                &WorktreeSetup {
                    copy: vec![".env".into()],
                    init_submodules: false,
                    batch: None,
                },
                &NoProgress,
                &CancelToken::new(),
            )
            .unwrap();
        RepoPath::new(path)
    }

    fn undo_last(&self) -> Result<Option<String>, OpError> {
        let e = self.undo.next_undo(self.path.id(), 50).unwrap().unwrap();
        self.undo.execute(&self.path, &e, Direction::Undo)
    }

    fn listed(&self) -> Vec<String> {
        self.cli
            .worktrees(&self.path)
            .unwrap()
            .iter()
            .map(archipel_model::worktree::WorktreeInfo::label)
            .collect()
    }
}

#[test]
fn create_copies_local_files_and_undo_removes_it_with_its_branch() {
    let e = env();
    let wt = e.add(
        "feat",
        WorktreeTarget::NewBranch {
            name: "feat/x".into(),
            base: None,
        },
    );
    assert_eq!(
        fs::read_to_string(wt.as_path().join(".env")).unwrap(),
        "SECRET=1\n"
    );
    assert_eq!(e.listed(), ["main", "feat/x"]);
    let entry = e.store.recent(e.path.id(), 1).unwrap().remove(0);
    assert_eq!(entry.kind, OperationKind::WorktreeAdd);
    // The copied `.env` is ignored: the worktree is clean, undo removes it.
    e.undo_last().unwrap();
    assert_eq!(e.listed(), ["main"]);
    assert!(
        e.cli
            .resolve(&e.path, "refs/heads/feat/x")
            .unwrap()
            .is_none()
    );
}

#[test]
fn remove_with_changes_then_undo_restores_files_byte_for_byte() {
    let e = env();
    let wt = e.add(
        "work",
        WorktreeTarget::NewBranch {
            name: "work".into(),
            base: None,
        },
    );
    let dir = wt.as_path().to_path_buf();
    fs::write(dir.join("a.txt"), b"edited\r\nwith crlf\r\n").unwrap();
    fs::create_dir_all(dir.join("notes")).unwrap();
    fs::write(dir.join("notes/new.md"), b"untracked\n").unwrap();
    let loss = e.actions.worktree_loss(&wt).unwrap();
    assert_eq!(loss.changes.len(), 2, "{loss:?}");
    assert!(!loss.is_safe());
    // Refused without force: it has changes.
    assert!(matches!(
        e.actions
            .remove_worktree(&e.path, &wt, RemoveOptions::default()),
        Err(OpError::Refused(_))
    ));
    e.actions
        .remove_worktree(
            &e.path,
            &wt,
            RemoveOptions {
                force: true,
                delete_branch: true,
                batch: None,
            },
        )
        .unwrap();
    assert!(!dir.exists());
    assert!(e.cli.resolve(&e.path, "refs/heads/work").unwrap().is_none());
    e.undo_last().unwrap();
    assert_eq!(e.listed(), ["main", "work"]);
    assert_eq!(
        fs::read(dir.join("a.txt")).unwrap(),
        b"edited\r\nwith crlf\r\n"
    );
    assert_eq!(fs::read(dir.join("notes/new.md")).unwrap(), b"untracked\n");
}

#[test]
fn undo_of_a_removal_is_refused_when_the_branch_is_checked_out_elsewhere() {
    let e = env();
    let wt = e.add(
        "w",
        WorktreeTarget::NewBranch {
            name: "w".into(),
            base: None,
        },
    );
    e.actions
        .remove_worktree(&e.path, &wt, RemoveOptions::default())
        .unwrap();
    e.repo.git(&["checkout", "w"]).unwrap();
    let err = e.undo_last().unwrap_err();
    assert!(err.to_string().contains("checked out"), "{err}");
}

#[test]
fn remove_is_refused_while_a_rebase_is_in_progress() {
    let e = env();
    let wt = e.add(
        "r",
        WorktreeTarget::NewBranch {
            name: "r".into(),
            base: None,
        },
    );
    let dir = wt.as_path();
    let git = |args: &[&str]| archipel_fixtures::git(dir, args).unwrap();
    fs::write(dir.join("a.txt"), "theirs\n").unwrap();
    git(&["commit", "-qam", "r side"]);
    e.repo.write("a.txt", "ours\n").unwrap();
    e.repo.commit_all("main side").unwrap();
    // Conflicts: the rebase stops.
    assert!(archipel_fixtures::git(dir, &["rebase", "main"]).is_err());
    let loss = e.actions.worktree_loss(&wt).unwrap();
    assert!(loss.in_progress.is_some());
    let err = e
        .actions
        .remove_worktree(
            &e.path,
            &wt,
            RemoveOptions {
                force: true,
                delete_branch: false,
                batch: None,
            },
        )
        .unwrap_err();
    assert!(err.to_string().contains("abort it first"), "{err}");
}

#[test]
fn undo_refuses_moving_a_branch_checked_out_in_another_worktree() {
    let e = env();
    // A branch created, then checked out in a worktree.
    e.actions
        .create_branch(&e.path, "side", None, None)
        .unwrap();
    let _wt = e.add("side", WorktreeTarget::Branch("side".into()));
    // Undoing the branch creation would delete the branch under it. The
    // newest entry is the worktree creation: skip to the branch one.
    let entries = e.store.recent(e.path.id(), 10).unwrap();
    let create = entries
        .iter()
        .find(|x| x.kind == OperationKind::BranchCreate)
        .unwrap();
    let err = e
        .undo
        .execute(&e.path, create, Direction::Undo)
        .unwrap_err();
    assert!(err.to_string().contains("checked out in"), "{err}");
}
