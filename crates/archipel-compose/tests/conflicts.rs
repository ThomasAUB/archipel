//! Conflicts left by real Git operations open the merge view by
//! themselves (the app tests drive a fake Git: these check what Git
//! really leaves).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use archipel_app::{App, AppEvent};
use archipel_compose::{AppDirs, Composition, compose};
use archipel_model::RepoPath;
use archipel_model::requests::{MergeMode, SequenceAction};
use archipel_ops::{OpClass, OpError, RepoActions};
use tokio::sync::broadcast::Receiver;

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .current_dir(dir)
        .args(args)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

/// Commits `f.txt` holding `text` on the current branch.
fn commit(repo: &Path, text: &str, message: &str) {
    std::fs::write(repo.join("f.txt"), text).unwrap();
    git(repo, &["commit", "-qam", message]);
}

/// A repo with `f.txt` on `main`, and the app.
struct Env {
    _home: tempfile::TempDir,
    repo: PathBuf,
    c: Composition,
}

impl Env {
    fn new() -> Self {
        let home = tempfile::tempdir().unwrap();
        let repo = home.path().join("ws").join("r");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["config", "user.email", "a@example.com"]);
        git(&repo, &["config", "user.name", "a"]);
        std::fs::write(repo.join("f.txt"), "a\nb\nc\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-qm", "base"]);
        let dirs = AppDirs::under(&home.path().join("home"));
        std::fs::create_dir_all(&dirs.config).unwrap();
        let c = compose(dirs).unwrap();
        Self {
            _home: home,
            repo,
            c,
        }
    }

    fn app(&self) -> &App {
        &self.c.app
    }

    /// Opens a project holding the repo; the repo once read.
    fn open(&self) -> RepoPath {
        let app = self.app();
        let id = app
            .new_project_from_folder(self.repo.parent().unwrap())
            .unwrap();
        app.open_project(&id).unwrap();
        let start = Instant::now();
        while app.rows().iter().any(|r| r.summary().is_none()) {
            assert!(start.elapsed() < Duration::from_secs(10), "rows not read");
            std::thread::sleep(Duration::from_millis(20));
        }
        app.rows()[0].path.clone()
    }

    /// Runs `job` as the repo view does: a write on the repo.
    fn write(
        &self,
        repo: &RepoPath,
        job: impl FnOnce(&RepoActions, &RepoPath) -> Result<(), OpError> + Send + 'static,
    ) {
        // Stopping on conflicts fails the operation: expected here.
        let _ = self
            .app()
            .submit_repo_job(repo, OpClass::Write, "Write", move |a, p, _| job(a, p))
            .wait_blocking();
    }

    /// The file the next `ConflictsToResolve` opens, `None` without one.
    fn conflict_opened(&self, events: &mut Receiver<AppEvent>) -> Option<String> {
        self.c.runtime.block_on(async {
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    if let Ok(AppEvent::ConflictsToResolve { file, .. }) = events.recv().await {
                        return file;
                    }
                }
            })
            .await
            .ok()
        })
    }
}

#[test]
fn a_merge_stopping_on_conflicts_opens_them() {
    let e = Env::new();
    git(&e.repo, &["checkout", "-qb", "other"]);
    commit(&e.repo, "a\nTHEIRS\nc\n", "theirs");
    git(&e.repo, &["checkout", "-q", "main"]);
    commit(&e.repo, "a\nOURS\nc\n", "ours");
    let repo = e.open();
    let mut events = e.app().subscribe();
    e.write(&repo, |a, p| a.merge(p, "other", MergeMode::Auto));
    assert_eq!(e.conflict_opened(&mut events).as_deref(), Some("f.txt"));
}

#[test]
fn a_rebase_continued_onto_its_next_conflict_opens_it() {
    let e = Env::new();
    git(&e.repo, &["checkout", "-qb", "topic"]);
    commit(&e.repo, "a\nT1\nc\n", "t1");
    commit(&e.repo, "a\nT2\nc\n", "t2");
    git(&e.repo, &["checkout", "-q", "main"]);
    commit(&e.repo, "a\nM\nc\n", "m");
    git(&e.repo, &["checkout", "-q", "topic"]);
    let repo = e.open();

    let mut events = e.app().subscribe();
    e.write(&repo, |a, p| a.rebase(p, "main"));
    assert_eq!(e.conflict_opened(&mut events).as_deref(), Some("f.txt"));

    // Resolved then continued at once: the dashboard may still count the
    // first conflict, the next one opens all the same.
    e.write(&repo, |a, p| {
        a.resolve_conflict(p, "f.txt", None, Some(b"a\nX\nc\n"))
    });
    let mut events = e.app().subscribe();
    e.write(&repo, |a, p| a.sequence(p, SequenceAction::Continue));
    assert_eq!(e.conflict_opened(&mut events).as_deref(), Some("f.txt"));
}
