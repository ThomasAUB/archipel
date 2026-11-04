#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! `HistoryView` against the in-memory `FakeGit`.

use std::sync::Arc;

use archipel_cache::CacheRegistry;
use archipel_graph::{
    CacheStatus, EdgeKind, GraphError, GraphOptions, GraphRow, HistoryView, LaneState, ParentLink,
    RefreshOutcome, SearchQuery,
};
use archipel_model::history::{CommitMeta, TopologyNode};
use archipel_model::testing::{FakeGit, FakeRepo, oid};
use archipel_model::{CancelToken, GitError, NoProgress, RepoPath, Timestamp};

/// A history given as `parents[row]` = parent rows (all greater than `row`).
fn history(parents: &[Vec<u64>]) -> Vec<(TopologyNode, CommitMeta)> {
    parents
        .iter()
        .zip(0u64..)
        .map(|(ps, row)| {
            let node = TopologyNode {
                oid: oid(row),
                parents: ps.iter().map(|p| oid(*p)).collect(),
            };
            let meta = CommitMeta {
                oid: oid(row),
                summary: format!("commit number {row}"),
                author_name: format!("Author {}", row % 5),
                author_email: format!("a{}@example.com", row % 5),
                author_time: Timestamp(i64::try_from(row).unwrap()),
                commit_time: Timestamp(i64::try_from(row).unwrap()),
            };
            (node, meta)
        })
        .collect()
}

/// A deterministic pseudo-random DAG in display order with merges, octopus
/// merges, several tips and several roots.
fn random_dag(n: u64, seed: u64) -> Vec<Vec<u64>> {
    let mut state = seed;
    let mut next = move || {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        state >> 33
    };
    (0..n)
        .map(|row| {
            let left = n - row - 1;
            if left == 0 || next() % 97 == 0 {
                return Vec::new(); // a root
            }
            let mut ps = vec![row + 1 + next() % left.min(3)];
            let merges = match next() % 20 {
                0..=3 => 1,
                4 => 3,
                _ => 0,
            };
            for _ in 0..merges {
                let p = row + 1 + next() % left.min(60);
                if !ps.contains(&p) {
                    ps.push(p);
                }
            }
            ps
        })
        .collect()
}

struct Env {
    git: Arc<FakeGit>,
    repo: RepoPath,
    dir: tempfile::TempDir,
    caches: Arc<CacheRegistry>,
}

impl Env {
    fn new(parents: &[Vec<u64>]) -> Self {
        let git = Arc::new(FakeGit::new());
        let repo = RepoPath::new("/fake/repo");
        let mut state = FakeRepo {
            history: history(parents),
            ..FakeRepo::default()
        };
        if !parents.is_empty() {
            state.refs.insert("refs/heads/main".into(), oid(0));
        }
        git.insert(&repo, state);
        Self {
            git,
            repo,
            dir: tempfile::tempdir().unwrap(),
            caches: Arc::new(CacheRegistry::new()),
        }
    }

    fn view(&self, options: GraphOptions) -> HistoryView {
        HistoryView::new(
            self.git.clone(),
            self.git.clone(),
            self.caches.clone(),
            self.dir.path(),
            options,
        )
    }

    fn opened(&self, options: GraphOptions) -> HistoryView {
        let mut v = self.view(options);
        v.open(&self.repo, &NoProgress, &CancelToken::new())
            .unwrap();
        v.wait_complete(&NoProgress).unwrap();
        v
    }
}

fn options(interval: usize) -> GraphOptions {
    GraphOptions {
        checkpoint_interval: interval,
        ..GraphOptions::default()
    }
}

fn reference_layout(parents: &[Vec<u64>]) -> Vec<(u32, u32, Vec<archipel_graph::Edge>)> {
    let mut state = LaneState::new();
    parents
        .iter()
        .zip(0u32..)
        .map(|(ps, row)| {
            let mut links: Vec<ParentLink> = Vec::new();
            for p in ps {
                let link = ParentLink::Row(u32::try_from(*p).unwrap());
                if !links.contains(&link) {
                    links.push(link);
                }
            }
            let l = state.step(row, &links);
            (l.column, l.color, l.edges)
        })
        .collect()
}

fn shape(rows: &[GraphRow]) -> Vec<(u32, u32, Vec<archipel_graph::Edge>)> {
    rows.iter()
        .map(|r| (r.column, r.color, r.edges.clone()))
        .collect()
}

#[test]
fn rows_match_direct_layout_including_octopus() {
    let parents = vec![
        vec![1, 2, 3, 4], // octopus
        vec![5],
        vec![5],
        vec![5],
        vec![5],
        vec![6, 7],
        vec![8],
        vec![8],
        vec![],
    ];
    let env = Env::new(&parents);
    let view = env.opened(options(1000));
    assert_eq!(view.row_count(), 9);
    let rows = view.rows(0..9).unwrap();
    assert_eq!(shape(&rows), reference_layout(&parents));
    assert_eq!(rows[0].edges.len(), 4);
    assert!(rows[0].edges.iter().all(|e| e.kind == EdgeKind::Outgoing));
    assert_eq!(rows[0].oid, oid(0));
    assert_eq!(rows[0].refs.len(), 1);
    assert_eq!(rows[0].refs[0].name, "refs/heads/main");
    assert!(rows[1].refs.is_empty());
    assert_eq!(view.max_width(), 4);
}

#[test]
fn first_window_is_served_while_the_topology_builds() {
    let parents = random_dag(5000, 9);
    let env = Env::new(&parents);
    let expected = reference_layout(&parents);
    let mut view = env.view(GraphOptions {
        stream_rows: 100,
        ..options(64)
    });
    let report = view
        .open(&env.repo, &NoProgress, &CancelToken::new())
        .unwrap();
    assert_eq!(report.cache, CacheStatus::BuiltMissing);
    // Either the first window only, or everything if the build already ended.
    assert!(report.rows == 100 || (report.complete && report.rows == 5000));
    let completion = view.completion().unwrap();
    // The streamed rows are laid out like the finished file lays them out.
    let first = view.rows(0..100).unwrap();
    assert_eq!(shape(&first), expected[..100]);
    assert_eq!(view.meta(0..100).unwrap().len(), 100);
    assert!(view.rows(0..view.row_count()).unwrap().len() >= 100);

    let size = completion.wait(&NoProgress).unwrap();
    assert_eq!(size.rows, 5000);
    assert!(view.is_complete() && completion.is_complete());
    assert_eq!(view.row_count(), 5000);
    assert_eq!(size.max_width, view.max_width());
    assert_eq!(view.rows(0..100).unwrap(), first);
    assert_eq!(shape(&view.rows(0..5000).unwrap()), expected);

    // Unloading during a build stops it; the next open starts over cleanly.
    std::fs::remove_file(view.cache_path(&env.repo)).unwrap();
    view.open(&env.repo, &NoProgress, &CancelToken::new())
        .unwrap();
    let pending = view.completion().unwrap();
    view.unload();
    match pending.wait(&NoProgress) {
        Ok(size) => assert_eq!(size.rows, 5000),
        Err(e) => assert!(e.is_cancelled()),
    }
    let view = env.opened(options(64));
    assert_eq!(shape(&view.rows(0..5000).unwrap()), expected);
}

#[test]
fn every_window_replays_like_the_full_layout() {
    let parents = random_dag(3000, 7);
    let env = Env::new(&parents);
    let full_view = env.opened(options(usize::MAX));
    let full = full_view.rows(0..3000).unwrap();
    assert_eq!(shape(&full), reference_layout(&parents));
    drop(full_view);
    for interval in [1, 7, 64, 1000] {
        let view = env.opened(options(interval));
        for size in [1u32, 13, 200] {
            let mut start = 0u32;
            while start < 3000 {
                let window = view.rows(start..start + size).unwrap();
                let end = (start + size).min(3000) as usize;
                assert_eq!(
                    window,
                    full[start as usize..end],
                    "interval {interval} start {start}"
                );
                start += 37;
            }
        }
    }
}

#[test]
fn cache_round_trip_and_reuse() {
    let parents = random_dag(500, 3);
    let env = Env::new(&parents);
    let mut view = env.view(options(50));
    let report = view
        .open(&env.repo, &NoProgress, &CancelToken::new())
        .unwrap();
    assert_eq!(report.cache, CacheStatus::BuiltMissing);
    assert_eq!(report.rows, 500);
    let first = view.rows(0..500).unwrap();
    assert!(view.cache_path(&env.repo).exists());

    let mut again = env.view(options(50));
    let report = again
        .open(&env.repo, &NoProgress, &CancelToken::new())
        .unwrap();
    assert_eq!(report.cache, CacheStatus::Reused);
    assert_eq!(again.rows(0..500).unwrap(), first);
    assert_eq!(again.oids(10..12).unwrap(), vec![oid(10), oid(11)]);
}

#[test]
fn moved_refs_make_the_cache_stale() {
    let parents = vec![vec![1], vec![2], vec![]];
    let env = Env::new(&parents);
    let mut view = env.opened(options(2));
    // Checking out / renaming without moving tips only updates badges.
    env.git.with_repo(&env.repo, |r| {
        r.refs.insert("refs/heads/topic".into(), oid(0));
    });
    let cancel = CancelToken::new();
    assert_eq!(
        view.refresh(&NoProgress, &cancel).unwrap(),
        RefreshOutcome::BadgesUpdated
    );
    assert_eq!(view.rows(0..1).unwrap()[0].refs.len(), 2);
    assert_eq!(
        view.refresh(&NoProgress, &cancel).unwrap(),
        RefreshOutcome::Unchanged
    );

    // A new commit on top moves `main`.
    env.git.with_repo(&env.repo, |r| {
        // New commit 100 on top of the old row 0.
        let mut h = vec![(
            TopologyNode {
                oid: oid(100),
                parents: vec![oid(0)],
            },
            CommitMeta {
                oid: oid(100),
                summary: "new".into(),
                author_name: "N".into(),
                author_email: "n@example.com".into(),
                author_time: Timestamp(1),
                commit_time: Timestamp(1),
            },
        )];
        h.extend(history(&parents));
        r.history = h;
        r.refs.insert("refs/heads/main".into(), oid(100));
    });
    // Keep a searcher on the old snapshot while the file is replaced.
    let old = view.searcher().unwrap();
    assert_eq!(
        view.refresh(&NoProgress, &cancel).unwrap(),
        RefreshOutcome::Rebuilt { rows: 4 }
    );
    assert_eq!(old.row_count(), 3);
    let rows = view.rows(0..4).unwrap();
    assert_eq!(rows[0].oid, oid(100));
    assert_eq!(rows[0].refs[0].name, "refs/heads/main");
    assert_eq!(rows[1].refs[0].name, "refs/heads/topic");
    assert!(rows.iter().all(|r| r.column == 0));

    let mut fresh = env.view(options(2));
    let report = fresh.open(&env.repo, &NoProgress, &cancel).unwrap();
    assert_eq!(report.cache, CacheStatus::Reused);
}

#[test]
fn stale_file_from_other_tips_is_rebuilt_on_open() {
    let parents = random_dag(100, 5);
    let env = Env::new(&parents);
    drop(env.opened(options(10)));
    env.git.with_repo(&env.repo, |r| {
        r.refs.insert("refs/tags/v1".into(), oid(50));
    });
    let mut view = env.view(options(10));
    let report = view
        .open(&env.repo, &NoProgress, &CancelToken::new())
        .unwrap();
    assert_eq!(report.cache, CacheStatus::BuiltStale);
}

fn corrupt_and_reopen(
    env: &Env,
    damage: impl FnOnce(&mut Vec<u8>),
) -> (CacheStatus, Vec<GraphRow>) {
    let path = {
        let v = env.opened(options(16));
        v.cache_path(&env.repo)
    };
    let mut bytes = std::fs::read(&path).unwrap();
    damage(&mut bytes);
    std::fs::write(&path, &bytes).unwrap();
    let mut view = env.view(options(16));
    let report = view
        .open(&env.repo, &NoProgress, &CancelToken::new())
        .unwrap();
    let rows = view.rows(0..view.row_count()).unwrap();
    (report.cache, rows)
}

type Damage = Box<dyn FnOnce(&mut Vec<u8>)>;

#[test]
fn corrupted_files_are_rebuilt() {
    let parents = random_dag(400, 11);
    let env = Env::new(&parents);
    let expected = reference_layout(&parents);
    let cases: Vec<(&str, Damage)> = vec![
        ("flip in oids", Box::new(|b| b[64 + 100] ^= 0xff)),
        (
            "flip in parents",
            Box::new(|b| {
                let at = 64 + 400 * 20 + 13;
                b[at] ^= 0x01;
            }),
        ),
        ("truncated", Box::new(|b| b.truncate(b.len() - 3))),
        ("bad magic", Box::new(|b| b[0] = b'X')),
        ("header field", Box::new(|b| b[16] ^= 0x04)),
        ("empty", Box::new(Vec::clear)),
    ];
    for (name, damage) in cases {
        let (status, rows) = corrupt_and_reopen(&env, damage);
        assert!(
            matches!(status, CacheStatus::BuiltCorrupt(_)),
            "{name}: {status:?}"
        );
        assert_eq!(shape(&rows), expected, "{name}");
    }
}

#[test]
fn meta_lru_stays_within_budget_while_scrolling() {
    let parents = random_dag(5000, 9);
    let env = Env::new(&parents);
    let budget = 64 * 1024;
    let view = env.opened(GraphOptions {
        meta_cache_bytes: budget,
        overscan: 50,
        ..options(100)
    });
    let mut peak = 0;
    let mut start = 0;
    while start < 5000 {
        let metas = view.meta(start..start + 60).unwrap();
        let end = (start + 60).min(5000);
        assert_eq!(metas.len(), (end - start) as usize);
        for (m, row) in metas.iter().zip(start..) {
            assert_eq!(m.oid, oid(u64::from(row)));
            assert_eq!(m.summary, format!("commit number {row}"));
        }
        let usage = view.memory_usage();
        assert!(usage.meta_cache <= budget);
        peak = peak.max(usage.total());
        start += 60;
    }
    let report = env.caches.report();
    assert_eq!(report.len(), 1);
    assert!(report[0].name.starts_with("commit-meta"));
    assert!(report[0].stats.evictions > 0);
    // Checkpoints are tiny next to the LRU budget.
    assert!(view.memory_usage().checkpoints < budget, "{peak}");
}

#[test]
fn meta_window_is_served_from_cache_with_overscan() {
    let parents = random_dag(1000, 2);
    let env = Env::new(&parents);
    let view = env.opened(GraphOptions {
        overscan: 100,
        ..options(100)
    });
    view.meta(300..350).unwrap();
    // The overscan already loaded the rows around the window.
    env.git.fail(
        &env.repo,
        "commit_meta",
        GitError::Backend("offline".into()),
    );
    assert_eq!(view.meta(220..250).unwrap().len(), 30);
    assert_eq!(view.meta(400..449).unwrap().len(), 49);
    assert!(matches!(view.meta(600..610), Err(GraphError::Git(_))));
}

#[test]
fn search_by_hash_author_and_message() {
    let parents = random_dag(3000, 4);
    let env = Env::new(&parents);
    let view = env.opened(GraphOptions {
        search_chunk_rows: 256,
        ..options(100)
    });
    let cancel = CancelToken::new();
    // Hash prefix: oid(n) is 12 zero bytes then n big-endian.
    let target = oid(2345).to_hex();
    let hit = view
        .search(&SearchQuery::new(&target[..38]), &cancel)
        .unwrap();
    assert!(hit.rows.contains(&2345));
    let hit = view.search(&SearchQuery::new(&target), &cancel).unwrap();
    assert_eq!(hit.rows, vec![2345]);
    // Message (case-insensitive).
    let hit = view
        .search(&SearchQuery::new("COMMIT NUMBER 1234"), &cancel)
        .unwrap();
    assert_eq!(hit.rows, vec![1234]);
    // Author email.
    let hit = view
        .search(&SearchQuery::new("a3@example"), &cancel)
        .unwrap();
    assert_eq!(hit.rows.len(), 600);
    assert!(hit.rows.iter().all(|r| r % 5 == 3));
    // Limit.
    let mut q = SearchQuery::new("author");
    q.max_hits = 10;
    let hit = view.search(&q, &cancel).unwrap();
    assert!(hit.truncated && hit.rows == (0..10).collect::<Vec<_>>());
    assert!(
        view.search(&SearchQuery::new("  "), &cancel)
            .unwrap()
            .rows
            .is_empty()
    );
    // Background thread + cancellation.
    let searcher = view.searcher().unwrap();
    let handle = std::thread::spawn(move || {
        let cancel = CancelToken::new();
        cancel.cancel();
        searcher.search(&SearchQuery::new("x"), &cancel)
    });
    assert!(handle.join().unwrap().unwrap_err().is_cancelled());
    assert_eq!(view.row_of(oid(1999), &cancel).unwrap(), Some(1999));
    assert_eq!(view.row_of(oid(99_999), &cancel).unwrap(), None);
    let rows = view
        .searcher()
        .unwrap()
        .rows_of(&[oid(7), oid(1999), oid(99_999), oid(7)], &cancel)
        .unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!((rows[&oid(7)], rows[&oid(1999)]), (7, 1999));
}

#[test]
fn unload_releases_everything() {
    let env = Env::new(&random_dag(2000, 8));
    let mut view = env.opened(options(100));
    view.meta(0..100).unwrap();
    let usage = view.memory_usage();
    assert_eq!(usage.checkpoint_count, 20);
    assert!(usage.meta_cache > 0 && usage.checkpoints > 0);
    assert_eq!(env.caches.report().len(), 1);
    view.unload();
    assert!(!view.is_open());
    assert_eq!(view.memory_usage().total(), 0);
    assert_eq!(view.row_count(), 0);
    assert!(env.caches.report().is_empty());
    assert!(matches!(view.rows(0..10), Err(GraphError::NotOpen)));
    assert!(matches!(view.meta(0..10), Err(GraphError::NotOpen)));
}

#[test]
fn shallow_parents_and_empty_history() {
    // Row 1's parent is not in the walk.
    let env = Env::new(&[vec![1], vec![]]);
    env.git.with_repo(&env.repo, |r| {
        r.history[1].0.parents = vec![oid(77)];
    });
    let view = env.opened(options(10));
    let rows = view.rows(0..2).unwrap();
    assert!(!rows[0].hidden_parents && rows[1].hidden_parents);

    let empty = Env::new(&[]);
    let view = empty.opened(options(10));
    assert_eq!(view.row_count(), 0);
    assert!(view.rows(0..10).unwrap().is_empty());
    assert!(view.meta(0..10).unwrap().is_empty());
}

#[test]
fn out_of_range_windows_are_clamped() {
    let env = Env::new(&random_dag(50, 1));
    let view = env.opened(options(8));
    assert_eq!(view.rows(40..100).unwrap().len(), 10);
    assert!(view.rows(60..100).unwrap().is_empty());
    #[allow(clippy::reversed_empty_ranges)]
    let reversed = view.rows(30..20).unwrap();
    assert!(reversed.is_empty());
}

#[test]
fn open_reports_progress_and_honors_cancel() {
    let env = Env::new(&random_dag(10_000, 6));
    let mut view = env.view(options(100));
    let cancel = CancelToken::new();
    cancel.cancel();
    let err = view.open(&env.repo, &NoProgress, &cancel).unwrap_err();
    assert!(err.is_cancelled());
    assert!(!view.is_open());
    assert!(!view.cache_path(&env.repo).exists());

    let seen = std::sync::Mutex::new(Vec::new());
    let sink = |p: archipel_model::Progress| seen.lock().unwrap().push(p.phase);
    view.open(&env.repo, &sink, &CancelToken::new()).unwrap();
    view.wait_complete(&sink).unwrap();
    let phases = seen.into_inner().unwrap();
    assert!(phases.iter().any(|p| p == "Reading history"));
    assert!(phases.iter().any(|p| p == "Laying out graph"));
}

#[test]
fn backend_errors_are_typed() {
    let env = Env::new(&random_dag(10, 1));
    env.git
        .fail(&env.repo, "walk_topology", GitError::Backend("boom".into()));
    let mut view = env.view(options(4));
    let err = view
        .open(&env.repo, &NoProgress, &CancelToken::new())
        .unwrap_err();
    assert!(matches!(err, GraphError::Git(GitError::Backend(_))));
}
