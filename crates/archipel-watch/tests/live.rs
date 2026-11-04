#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Integration tests: real repositories, real OS watcher.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use archipel_fixtures::TestRepo;
use archipel_model::ports::{ChangeSource, Coverage, Invalidation, Scope, WatchHandle};
use archipel_model::{RepoId, RepoPath};
use archipel_watch::{DebounceConfig, NotifyChangeSource};

/// Generous: CI file systems (and Windows) can be slow to report.
const TIMEOUT: Duration = Duration::from_secs(15);
/// How long to wait when asserting that nothing arrives.
const SILENCE: Duration = Duration::from_millis(1500);
/// Lets events from setup drain before a scenario starts.
const SETTLE: Duration = Duration::from_millis(500);
const POLL: Duration = Duration::from_millis(20);
const DEBOUNCE: Duration = Duration::from_millis(50);

#[derive(Clone, Default)]
struct Collected(Arc<Mutex<Vec<Invalidation>>>);

impl Collected {
    fn scope(&self) -> Scope {
        let mut s = Scope::NONE;
        for i in self.0.lock().unwrap().iter() {
            s |= i.scope;
        }
        s
    }

    fn clear(&self) {
        self.0.lock().unwrap().clear();
    }

    fn wait_for(&self, wanted: Scope) {
        let start = Instant::now();
        while start.elapsed() < TIMEOUT {
            if self.scope().contains(wanted) {
                return;
            }
            std::thread::sleep(POLL);
        }
        panic!("expected {wanted:?}, got {:?}", self.0.lock().unwrap());
    }

    fn assert_silent(&self) {
        std::thread::sleep(SILENCE);
        assert!(
            self.0.lock().unwrap().is_empty(),
            "unexpected {:?}",
            self.0.lock().unwrap()
        );
    }
}

fn source() -> NotifyChangeSource {
    NotifyChangeSource::new(DebounceConfig::with_quiet(DEBOUNCE)).unwrap()
}

fn watch(src: &NotifyChangeSource, path: &std::path::Path) -> (WatchHandle, Collected) {
    let got = Collected::default();
    let sink_got = got.clone();
    let repo = RepoId(1);
    let handle = src
        .watch(
            repo,
            &RepoPath::new(path),
            Arc::new(move |inv: Invalidation| sink_got.0.lock().unwrap().push(inv)),
        )
        .unwrap();
    assert_eq!(handle.coverage, Coverage::Full);
    std::thread::sleep(SETTLE);
    got.clear();
    (handle, got)
}

fn committed_repo() -> TestRepo {
    let repo = TestRepo::new().unwrap();
    repo.write("tracked.txt", "one\n").unwrap();
    repo.write(".gitignore", "*.log\nbuild/\n").unwrap();
    repo.commit_all("init").unwrap();
    repo
}

#[test]
fn worktree_write_yields_status() {
    let repo = committed_repo();
    let src = source();
    let (_h, got) = watch(&src, repo.path());
    repo.write("src/new.rs", "fn main() {}\n").unwrap();
    got.wait_for(Scope::STATUS);
}

#[test]
fn commit_yields_refs() {
    let repo = committed_repo();
    let src = source();
    let (_h, got) = watch(&src, repo.path());
    repo.git(&["commit", "-q", "--allow-empty", "-m", "second"])
        .unwrap();
    got.wait_for(Scope::REFS);
}

#[test]
fn stash_yields_stash() {
    let repo = committed_repo();
    repo.write("tracked.txt", "two\n").unwrap();
    let src = source();
    let (_h, got) = watch(&src, repo.path());
    repo.git(&["stash", "-q"]).unwrap();
    got.wait_for(Scope::STASH | Scope::STATUS);
}

#[test]
fn ignored_files_yield_nothing() {
    let repo = committed_repo();
    repo.write("app.log", "start\n").unwrap();
    repo.write("build/out.bin", "x").unwrap();
    let src = source();
    let (_h, got) = watch(&src, repo.path());
    for i in 0..5 {
        repo.write("app.log", &format!("line {i}\n")).unwrap();
        repo.write("build/out.bin", &format!("{i}")).unwrap();
    }
    got.assert_silent();
    // The watch is live: a tracked file still reports.
    repo.write("tracked.txt", "changed\n").unwrap();
    got.wait_for(Scope::STATUS);
}

#[test]
fn nothing_after_handle_is_dropped() {
    let repo = committed_repo();
    let src = source();
    let (h, got) = watch(&src, repo.path());
    drop(h);
    std::thread::sleep(SETTLE);
    got.clear();
    repo.write("tracked.txt", "changed\n").unwrap();
    repo.git(&["commit", "-q", "-am", "x"]).unwrap();
    got.assert_silent();
}

#[test]
fn linked_worktree_sees_shared_refs_and_own_files() {
    let repo = committed_repo();
    let tmp = tempfile::tempdir().unwrap();
    let wt = tmp.path().join("wt");
    repo.git(&["worktree", "add", "-q", wt.to_str().unwrap(), "-b", "side"])
        .unwrap();
    let src = source();
    let (_h, got) = watch(&src, &wt);
    // A commit in the main worktree moves a shared ref.
    repo.git(&["commit", "-q", "--allow-empty", "-m", "main moves"])
        .unwrap();
    got.wait_for(Scope::REFS);
    got.clear();
    std::fs::write(wt.join("tracked.txt"), "wt change\n").unwrap();
    got.wait_for(Scope::STATUS);
}
