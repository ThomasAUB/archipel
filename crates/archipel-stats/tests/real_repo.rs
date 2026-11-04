#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! `StatsEngine` and providers on real temporary repositories (`GitCli`).

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use archipel_fixtures::TestRepo;
use archipel_fixtures::history::{HistoryShape, generate_history};
use archipel_git::GitCli;
use archipel_model::ports::StatsStore;
use archipel_model::testing::InMemoryStore;
use archipel_model::{CancelToken, RepoPath, Timestamp};
use archipel_stats::{
    BRANCHES_ID, PauseToken, RepoTarget, StatSources, StatView, StatsConfig, StatsEngine,
    StatsRegistry,
};

/// PLAN Phase 7 exit criterion.
const INCREMENTAL_BUDGET: Duration = Duration::from_secs(1);

struct Setup {
    store: Arc<InMemoryStore>,
    engine: StatsEngine,
    sources: StatSources,
    target: RepoTarget,
}

fn setup(path: &std::path::Path) -> Setup {
    let git = Arc::new(GitCli::default());
    let store = Arc::new(InMemoryStore::new());
    Setup {
        engine: StatsEngine::new(git.clone(), store.clone(), git.clone(), git.clone()),
        sources: StatSources {
            store: store.clone(),
            refs: git.clone(),
            history: git,
        },
        store,
        target: RepoTarget::from_path(RepoPath::new(path)),
    }
}

impl Setup {
    fn update(&self) -> archipel_stats::UpdateOutcome {
        self.engine
            .update(
                self.target.id,
                &self.target.path,
                &CancelToken::new(),
                &PauseToken::new(),
            )
            .unwrap()
    }

    /// Activity and churn as stored, in a comparable form.
    fn stored(&self) -> Expected {
        let mut activity = BTreeMap::new();
        for b in self.store.activity(self.target.id).unwrap() {
            activity.insert((b.week, b.author), b.commits);
        }
        let mut churn = BTreeMap::new();
        for h in self.store.hotspots(self.target.id, usize::MAX).unwrap() {
            churn.insert(h.path, (h.added, h.removed, h.commits));
        }
        Expected { activity, churn }
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Expected {
    activity: BTreeMap<(i64, String), u32>,
    churn: BTreeMap<String, (u64, u64, u32)>,
}

/// Statistics derived independently from plain `git log` output.
fn expected_from_git_log(repo: &TestRepo) -> Expected {
    let mut activity = BTreeMap::new();
    for line in repo
        .git(&["log", "--format=%ae %at", "HEAD"])
        .unwrap()
        .lines()
    {
        let (email, at) = line.rsplit_once(' ').unwrap();
        let week = Timestamp(at.parse().unwrap()).week_index();
        *activity.entry((week, email.to_lowercase())).or_insert(0) += 1;
    }
    let mut churn: BTreeMap<String, (u64, u64, u32)> = BTreeMap::new();
    let numstat = repo
        .git(&["log", "--numstat", "--no-renames", "--format=", "HEAD"])
        .unwrap();
    for line in numstat.lines().filter(|l| !l.is_empty()) {
        let mut parts = line.splitn(3, '\t');
        let added = parts.next().unwrap().parse().unwrap_or(0);
        let removed = parts.next().unwrap().parse().unwrap_or(0);
        let e = churn.entry(parts.next().unwrap().to_owned()).or_default();
        e.0 += added;
        e.1 += removed;
        e.2 += 1;
    }
    Expected { activity, churn }
}

fn commit_as(repo: &TestRepo, author: &str, date: &str, message: &str) {
    repo.git(&["add", "-A"]).unwrap();
    repo.git(&[
        "commit",
        "-q",
        "--allow-empty",
        &format!("--author={author}"),
        &format!("--date={date}"),
        "-m",
        message,
    ])
    .unwrap();
}

fn sample_repo() -> TestRepo {
    let repo = TestRepo::new().unwrap();
    repo.write("src/a.rs", "one\ntwo\nthree\n").unwrap();
    commit_as(
        &repo,
        "Alice <alice@example.com>",
        "2024-01-01T10:00:00Z",
        "a",
    );
    repo.write("src/a.rs", "one\n2\nthree\nfour\n").unwrap();
    repo.write("b.bin", "\0\x01\x02binary").unwrap();
    commit_as(&repo, "Bob <BOB@Example.com>", "2024-01-03T10:00:00Z", "b");
    repo.git(&["checkout", "-q", "-b", "feature"]).unwrap();
    repo.write("docs/readme.md", "hello\n").unwrap();
    commit_as(
        &repo,
        "Alice <Alice@example.com>",
        "2024-01-09T10:00:00Z",
        "f",
    );
    repo.git(&["checkout", "-q", "main"]).unwrap();
    repo.write("src/a.rs", "one\n").unwrap();
    commit_as(&repo, "Bob <bob@example.com>", "2024-01-10T10:00:00Z", "c");
    repo.git(&["merge", "-q", "--no-ff", "--no-edit", "feature"])
        .unwrap();
    repo
}

#[test]
fn stats_match_git_log_and_incremental_update_is_fast() {
    let repo = sample_repo();
    let s = setup(repo.path());

    let full = s.update();
    assert_eq!(full.commits, 5, "4 commits + 1 merge");
    assert_eq!(s.stored(), expected_from_git_log(&repo));
    assert_eq!(s.update().commits, 0, "up to date");

    repo.write("src/new.rs", "x\ny\n").unwrap();
    commit_as(
        &repo,
        "Carol <carol@example.com>",
        "2024-02-01T10:00:00Z",
        "n",
    );
    let started = Instant::now();
    let inc = s.update();
    let elapsed = started.elapsed();
    assert_eq!(inc.commits, 1);
    assert!(!inc.reset);
    assert!(
        elapsed < INCREMENTAL_BUDGET,
        "incremental update took {elapsed:?}"
    );
    assert_eq!(s.stored(), expected_from_git_log(&repo));

    // Rewriting HEAD (amend) resets and recomputes.
    repo.git(&["commit", "-q", "--amend", "-m", "amended"])
        .unwrap();
    let rewritten = s.update();
    assert!(rewritten.reset);
    assert_eq!(s.stored(), expected_from_git_log(&repo));

    // A rewritten cursor that was garbage-collected is detected too.
    repo.git(&["commit", "-q", "--amend", "-m", "again"])
        .unwrap();
    repo.git(&["reflog", "expire", "--expire=now", "--all"])
        .unwrap();
    repo.git(&["gc", "-q", "--prune=now"]).unwrap();
    assert!(s.update().reset);
    assert_eq!(s.stored(), expected_from_git_log(&repo));
}

#[test]
fn branches_view_on_a_real_repository() {
    let repo = sample_repo();
    repo.git(&["branch", "old-merged", "HEAD~1"]).unwrap();
    repo.git(&["checkout", "-q", "-b", "open"]).unwrap();
    repo.write("open.txt", "o\n").unwrap();
    repo.commit_all("open work").unwrap();
    repo.git(&["checkout", "-q", "main"]).unwrap();

    let s = setup(repo.path());
    let registry = StatsRegistry::with_builtins(&StatsConfig::default());
    let stats = registry.compute_repo(&s.sources, &s.target);
    let Some(StatView::Branches(view)) = stats.view(BRANCHES_ID) else {
        panic!("{stats:?}");
    };
    let mut merged: Vec<&str> = view
        .merged_not_deleted()
        .map(|r| r.age.name.as_str())
        .collect();
    merged.sort_unstable();
    assert_eq!(merged, vec!["feature", "old-merged"]);
    assert!(
        view.branches
            .iter()
            .any(|r| r.age.name == "open" && !r.age.merged)
    );
    assert!(view.branches.iter().all(|r| r.age.name != "main"));
}

/// Full computation over a 100k-commit history. Run in release:
/// `cargo test -p archipel-stats --release -- --ignored --nocapture`.
#[test]
#[ignore = "large fixture; run in release with --ignored"]
fn large_history_full_and_incremental() {
    let dir = tempfile::tempdir().unwrap();
    let started = Instant::now();
    generate_history(dir.path(), HistoryShape::default()).unwrap();
    println!("fixture generated in {:?}", started.elapsed());

    let s = setup(dir.path());
    let started = Instant::now();
    let full = s.update();
    let full_time = started.elapsed();
    println!(
        "full: {} commits in {full_time:?}; peak delta: {} activity keys, {} churn keys, ~{} bytes",
        full.commits, full.delta.activity_keys, full.delta.churn_keys, full.delta.approx_bytes
    );
    let total: u64 = s
        .store
        .activity(s.target.id)
        .unwrap()
        .iter()
        .map(|b| u64::from(b.commits))
        .sum();
    assert_eq!(total, full.commits);

    archipel_fixtures::write_file(dir.path(), "extra.txt", "extra\n").unwrap();
    archipel_fixtures::git(dir.path(), &["add", "-A"]).unwrap();
    archipel_fixtures::git(dir.path(), &["commit", "-q", "-m", "extra"]).unwrap();
    let started = Instant::now();
    let inc = s.update();
    let inc_time = started.elapsed();
    println!("incremental: {} commit in {inc_time:?}", inc.commits);
    assert_eq!(inc.commits, 1);
    assert!(inc_time < INCREMENTAL_BUDGET);
}
