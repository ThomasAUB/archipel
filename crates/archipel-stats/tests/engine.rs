#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! `StatsEngine` against `FakeGit` + `InMemoryStore`.

#[allow(dead_code)]
mod common;

use std::ops::ControlFlow;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use archipel_model::ports::{ChurnReader, CommitChurn, StatsStore};
use archipel_model::testing::{FakeGit, oid};
use archipel_model::{CancelToken, GitError, Oid, RepoPath};
use archipel_stats::{PauseToken, StatsEngine, StatsError};
use common::{Fixture, MONDAY, WEEK, activity_triples, churn};

fn first_three() -> Vec<CommitChurn> {
    vec![
        churn(1, "alice@x.org", MONDAY, &[("a.rs", 10, 0)]),
        churn(
            2,
            "Alice@X.org",
            MONDAY + 3600,
            &[("a.rs", 2, 1), ("b.rs", 5, 0)],
        ),
        churn(3, "bob@x.org", MONDAY + WEEK, &[("b.rs", 1, 1)]),
    ]
}

fn two_more() -> Vec<CommitChurn> {
    vec![
        churn(4, "bob@x.org", MONDAY + WEEK + 60, &[("a.rs", 1, 0)]),
        churn(5, "carol@x.org", MONDAY + 2 * WEEK, &[("c.rs", 3, 0)]),
    ]
}

fn run(fx: &Fixture) -> Result<archipel_stats::UpdateOutcome, StatsError> {
    fx.engine().update(
        fx.target.id,
        &fx.target.path,
        &CancelToken::new(),
        &PauseToken::new(),
    )
}

fn churn_of(fx: &Fixture, path: &str) -> (u64, u64, u32) {
    let h = fx.store.hotspots(fx.target.id, usize::MAX).unwrap();
    let h = h.iter().find(|h| h.path == path).unwrap();
    (h.added, h.removed, h.commits)
}

#[test]
fn full_then_incremental_update_processes_only_new_commits() {
    let fx = Fixture::new("r");
    let mut history = first_three();
    fx.set_linear(&history);

    let first = run(&fx).unwrap();
    assert_eq!(first.commits, 3);
    assert!(!first.reset);
    assert_eq!(first.head, Some(oid(3)));
    assert_eq!(first.delta.activity_keys, 2);
    assert_eq!(first.delta.churn_keys, 2);
    assert!(first.delta.approx_bytes > 0);
    assert_eq!(
        activity_triples(&fx.store.activity(fx.target.id).unwrap()),
        vec![(0, "alice@x.org".into(), 2), (1, "bob@x.org".into(), 1)]
    );
    assert_eq!(churn_of(&fx, "a.rs"), (12, 1, 2));

    // Nothing new: no walk, nothing changes.
    assert_eq!(run(&fx).unwrap().commits, 0);

    history.extend(two_more());
    fx.set_linear(&history);
    let second = run(&fx).unwrap();
    assert_eq!(second.commits, 2, "only the new commits are read");
    assert_eq!(fx.store.cursor(fx.target.id).unwrap(), Some(oid(5)));
    assert_eq!(
        activity_triples(&fx.store.activity(fx.target.id).unwrap()),
        vec![
            (0, "alice@x.org".into(), 2),
            (1, "bob@x.org".into(), 2),
            (2, "carol@x.org".into(), 1)
        ]
    );
    assert_eq!(churn_of(&fx, "a.rs"), (13, 1, 3));
    assert_eq!(churn_of(&fx, "c.rs"), (3, 0, 1));
}

#[test]
fn rewritten_history_resets_and_recomputes() {
    let fx = Fixture::new("r");
    fx.set_linear(&first_three());
    run(&fx).unwrap();

    // Force-pushed history: the old cursor (3) is not an ancestor any more.
    let rewritten = vec![
        churn(10, "dave@x.org", MONDAY, &[("z.rs", 4, 0)]),
        churn(11, "dave@x.org", MONDAY + 60, &[("z.rs", 1, 1)]),
    ];
    fx.set_linear(&rewritten);
    let out = run(&fx).unwrap();
    assert!(out.reset);
    assert_eq!(out.commits, 2);
    assert_eq!(
        activity_triples(&fx.store.activity(fx.target.id).unwrap()),
        vec![(0, "dave@x.org".into(), 2)]
    );
    let hotspots = fx.store.hotspots(fx.target.id, usize::MAX).unwrap();
    assert_eq!(hotspots.len(), 1, "old paths are gone: {hotspots:?}");
    assert_eq!(fx.store.cursor(fx.target.id).unwrap(), Some(oid(11)));
}

/// Cancels the token after `after` commits were delivered.
struct CancelAfter {
    inner: Arc<FakeGit>,
    after: usize,
}

impl ChurnReader for CancelAfter {
    fn walk_churn(
        &self,
        repo: &RepoPath,
        since: Option<Oid>,
        sink: &mut dyn FnMut(CommitChurn) -> ControlFlow<()>,
        cancel: &CancelToken,
    ) -> Result<(), GitError> {
        let mut seen = 0;
        self.inner.walk_churn(
            repo,
            since,
            &mut |c| {
                seen += 1;
                if seen > self.after {
                    cancel.cancel();
                }
                sink(c)
            },
            cancel,
        )
    }
}

#[test]
fn cancellation_keeps_cursor_and_adds_nothing() {
    let fx = Fixture::new("r");
    let mut history = first_three();
    fx.set_linear(&history);
    run(&fx).unwrap();
    let before = fx.store.activity(fx.target.id).unwrap();

    history.extend(two_more());
    fx.set_linear(&history);
    let cancelling = StatsEngine::new(
        Arc::new(CancelAfter {
            inner: fx.git.clone(),
            after: 1,
        }),
        fx.store.clone(),
        fx.git.clone(),
        fx.git.clone(),
    );
    let err = cancelling
        .update(
            fx.target.id,
            &fx.target.path,
            &CancelToken::new(),
            &PauseToken::new(),
        )
        .unwrap_err();
    assert_eq!(err, StatsError::Cancelled);
    assert_eq!(fx.store.cursor(fx.target.id).unwrap(), Some(oid(3)));
    assert_eq!(fx.store.activity(fx.target.id).unwrap(), before);

    // The next run resumes from the old cursor without double counting.
    assert_eq!(run(&fx).unwrap().commits, 2);
    assert_eq!(churn_of(&fx, "a.rs"), (13, 1, 3));
}

#[test]
fn cancelled_before_start_touches_nothing() {
    let fx = Fixture::new("r");
    fx.set_linear(&first_three());
    let cancel = CancelToken::new();
    cancel.cancel();
    let err = fx
        .engine()
        .update(fx.target.id, &fx.target.path, &cancel, &PauseToken::new())
        .unwrap_err();
    assert_eq!(err, StatsError::Cancelled);
    assert_eq!(fx.store.cursor(fx.target.id).unwrap(), None);
}

/// Requests a pause once `at` commits were delivered.
struct PauseAt {
    inner: Arc<FakeGit>,
    pause: PauseToken,
    at: usize,
    seen: Arc<AtomicUsize>,
}

impl ChurnReader for PauseAt {
    fn walk_churn(
        &self,
        repo: &RepoPath,
        since: Option<Oid>,
        sink: &mut dyn FnMut(CommitChurn) -> ControlFlow<()>,
        cancel: &CancelToken,
    ) -> Result<(), GitError> {
        self.inner.walk_churn(
            repo,
            since,
            &mut |c| {
                if self.seen.fetch_add(1, Ordering::SeqCst) + 1 == self.at {
                    self.pause.pause();
                }
                sink(c)
            },
            cancel,
        )
    }
}

#[test]
fn pause_blocks_the_walk_until_resumed() {
    let fx = Fixture::new("r");
    let mut history = first_three();
    history.extend(two_more());
    fx.set_linear(&history);
    let pause = PauseToken::new();
    let seen = Arc::new(AtomicUsize::new(0));
    let engine = StatsEngine::new(
        Arc::new(PauseAt {
            inner: fx.git.clone(),
            pause: pause.clone(),
            at: 2,
            seen: seen.clone(),
        }),
        fx.store.clone(),
        fx.git.clone(),
        fx.git.clone(),
    );
    std::thread::scope(|s| {
        let worker =
            s.spawn(|| engine.update(fx.target.id, &fx.target.path, &CancelToken::new(), &pause));
        std::thread::sleep(Duration::from_millis(150));
        assert!(!worker.is_finished(), "paused update must wait");
        assert_eq!(seen.load(Ordering::SeqCst), 2);
        assert_eq!(fx.store.cursor(fx.target.id).unwrap(), None);
        pause.resume();
        let out = worker.join().unwrap().unwrap();
        assert_eq!(out.commits, 5);
    });
    assert_eq!(fx.store.cursor(fx.target.id).unwrap(), Some(oid(5)));
}

#[test]
fn cancel_while_paused_returns_promptly() {
    let fx = Fixture::new("r");
    fx.set_linear(&first_three());
    let pause = PauseToken::new();
    pause.pause();
    let cancel = CancelToken::new();
    let engine = fx.engine();
    std::thread::scope(|s| {
        let worker = s.spawn(|| engine.update(fx.target.id, &fx.target.path, &cancel, &pause));
        std::thread::sleep(Duration::from_millis(50));
        cancel.cancel();
        assert_eq!(worker.join().unwrap().unwrap_err(), StatsError::Cancelled);
    });
    assert_eq!(fx.store.cursor(fx.target.id).unwrap(), None);
}

#[test]
fn head_not_first_in_walk_is_reported_as_moving() {
    let fx = Fixture::new("r");
    fx.set_linear(&first_three());
    fx.git.with_repo(&fx.target.path, |r| {
        r.refs.insert("refs/heads/main".into(), oid(99));
    });
    let err = run(&fx).unwrap_err();
    assert!(matches!(err, StatsError::HeadMoved { .. }), "{err:?}");
    assert_eq!(fx.store.cursor(fx.target.id).unwrap(), None);
}

#[test]
fn empty_repository_is_a_no_op() {
    let fx = Fixture::new("r");
    let out = run(&fx).unwrap();
    assert_eq!(out.head, None);
    assert_eq!(out.commits, 0);
}

#[test]
fn git_errors_are_typed() {
    let fx = Fixture::new("r");
    fx.set_linear(&first_three());
    fx.git.fail(
        &fx.target.path,
        "walk_churn",
        GitError::Backend("boom".into()),
    );
    let err = run(&fx).unwrap_err();
    assert_eq!(err, StatsError::Git(GitError::Backend("boom".into())));
    assert_eq!(fx.store.cursor(fx.target.id).unwrap(), None);
}

#[test]
fn update_all_covers_every_repo() {
    let a = Fixture::new("a");
    let b = Fixture::with(a.git.clone(), a.store.clone(), "b");
    a.set_linear(&first_three());
    b.set_linear(&two_more());
    let results = a.engine().update_all(
        &[a.target.clone(), b.target.clone()],
        &CancelToken::new(),
        &PauseToken::new(),
    );
    let commits: Vec<u64> = results
        .iter()
        .map(|(_, r)| r.as_ref().unwrap().commits)
        .collect();
    assert_eq!(commits, vec![3, 2]);
}

#[test]
fn ancestry_errors_on_an_existing_cursor_are_propagated() {
    let fx = Fixture::new("r");
    let mut history = first_three();
    fx.set_linear(&history);
    run(&fx).unwrap();
    history.extend(two_more());
    fx.set_linear(&history);
    fx.git.fail(
        &fx.target.path,
        "is_ancestor",
        GitError::Backend("x".into()),
    );
    assert_eq!(
        run(&fx).unwrap_err(),
        StatsError::Git(GitError::Backend("x".into()))
    );
    assert_eq!(fx.store.cursor(fx.target.id).unwrap(), Some(oid(3)));
}
