#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Rough timings of the SQLite store, to catch pathological slowness.
//!
//! Run with `cargo test -p archipel-cache --release --test sqlite_bench -- --ignored --nocapture`.

use std::time::Instant;

use archipel_cache::SqliteStore;
use archipel_model::journal::{
    EntryState, HeadRef, JournalEntry, JournalId, OperationKind, RefSnapshot,
};
use archipel_model::ports::store::{JournalStore, SummaryStore};
use archipel_model::summary::{HeadState, RepoSummary, Upstream};
use archipel_model::testing::oid;
use archipel_model::{ProjectId, RepoId, Timestamp};

/// Repositories in the benchmarked project.
const SUMMARIES: u64 = 20;
/// Save/load rounds of the summaries.
const SUMMARY_ROUNDS: u32 = 100;
/// Journal entries appended.
const JOURNAL_APPENDS: u64 = 1_000;
/// Journal bound per repo (PLAN Phase 6).
const JOURNAL_LIMIT: usize = 200;
/// Repositories the journal entries are spread over.
const JOURNAL_REPOS: u64 = 4;

fn summaries() -> Vec<(RepoId, RepoSummary)> {
    (0..SUMMARIES)
        .map(|n| {
            let mut s = RepoSummary::empty("main");
            s.head = HeadState::Branch(format!("feature/{n}"));
            s.head_oid = Some(oid(n));
            s.upstream = Some(Upstream {
                name: "origin/main".into(),
                ahead: 1,
                behind: 2,
                gone: false,
            });
            s.computed_at = Timestamp(1_700_000_000);
            (RepoId(n.wrapping_mul(0x9e37_79b9_7f4a_7c15)), s)
        })
        .collect()
}

fn entry(n: u64) -> JournalEntry {
    let snapshot = |o| RefSnapshot {
        head: HeadRef::Symbolic("refs/heads/main".into()),
        refs: vec![("refs/heads/main".into(), Some(oid(o)))],
    };
    JournalEntry {
        id: JournalId(0),
        repo: RepoId(n % JOURNAL_REPOS),
        batch: None,
        kind: OperationKind::Commit,
        label: format!("Commit {n}"),
        before: snapshot(n),
        after: snapshot(n + 1),
        worktree_snapshot: None,
        stash: None,
        time: Timestamp(0),
        state: EntryState::Done,
    }
}

fn bench(name: &str, store: &SqliteStore) {
    let project = ProjectId("bench".into());
    let data = summaries();

    let start = Instant::now();
    for _ in 0..SUMMARY_ROUNDS {
        store.save_summaries(&project, &data).unwrap();
    }
    let save = start.elapsed() / SUMMARY_ROUNDS;

    let start = Instant::now();
    for _ in 0..SUMMARY_ROUNDS {
        assert_eq!(store.load_summaries(&project).unwrap().len(), data.len());
    }
    let load = start.elapsed() / SUMMARY_ROUNDS;

    let start = Instant::now();
    for n in 0..JOURNAL_APPENDS {
        store.append(&entry(n), JOURNAL_LIMIT).unwrap();
    }
    let append = start.elapsed();

    println!(
        "[{name}] save {SUMMARIES} summaries: {save:?}/call, load: {load:?}/call, \
         {JOURNAL_APPENDS} journal appends: {append:?} total ({:?}/append)",
        append / u32::try_from(JOURNAL_APPENDS).unwrap()
    );
}

#[test]
#[ignore = "benchmark; run with --release -- --ignored --nocapture"]
fn store_timings() {
    let dir = tempfile::tempdir().unwrap();
    bench(
        "file",
        &SqliteStore::open(dir.path().join("bench.sqlite")).unwrap(),
    );
    bench("memory", &SqliteStore::open_in_memory().unwrap());
}
