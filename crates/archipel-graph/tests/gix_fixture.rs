#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! `HistoryView` on real repositories read with `GixReader`.
//!
//! The `#[ignore]` test is the Phase 4 benchmark on the 100k-commit fixture:
//! `CARGO_TARGET_DIR=target/agent-graph cargo test -p archipel-graph --release -- --ignored --nocapture`

use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use archipel_cache::CacheRegistry;
use archipel_fixtures::git;
use archipel_fixtures::history::{HistoryShape, generate_history};
use archipel_git::GixReader;
use archipel_graph::{
    CacheStatus, EdgeKind, GraphOptions, GraphRow, HistoryView, RefreshOutcome, SearchQuery,
};
use archipel_model::history::WalkOrder;
use archipel_model::refs::RefKind;
use archipel_model::{CancelToken, NoProgress, Oid, RepoPath};

/// Rows visible on one screen.
const SCREEN: u32 = 60;

fn fixture(commits: usize) -> (tempfile::TempDir, RepoPath) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("big");
    generate_history(
        &path,
        HistoryShape {
            commits,
            ..HistoryShape::default()
        },
    )
    .unwrap();
    git(&path, &["commit-graph", "write", "--reachable"]).unwrap();
    (dir, RepoPath::new(path))
}

fn view(cache_dir: &Path, caches: &Arc<CacheRegistry>, options: GraphOptions) -> HistoryView {
    let gix = Arc::new(GixReader::default());
    HistoryView::new(gix.clone(), gix, caches.clone(), cache_dir, options)
}

fn open(v: &mut HistoryView, repo: &RepoPath) -> CacheStatus {
    let cache = v
        .open(repo, &NoProgress, &CancelToken::new())
        .unwrap()
        .cache;
    v.wait_complete(&NoProgress).unwrap();
    cache
}

fn options(interval: usize) -> GraphOptions {
    GraphOptions {
        checkpoint_interval: interval,
        ..GraphOptions::default()
    }
}

#[test]
fn real_history_round_trip() {
    let (dir, repo) = fixture(1500);
    let caches = Arc::new(CacheRegistry::new());
    let cache_dir = dir.path().join("cache");
    let mut full = view(&cache_dir, &caches, options(usize::MAX));
    assert_eq!(open(&mut full, &repo), CacheStatus::BuiltMissing);
    let n = full.row_count();
    let count: u32 = git(repo.as_path(), &["rev-list", "--all", "--count"])
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(n, count);
    let all = full.rows(0..n).unwrap();

    // Same commits as git, each exactly once.
    let listed: HashSet<String> = git(repo.as_path(), &["rev-list", "--all"])
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect();
    let ours: HashSet<String> = all.iter().map(|r| r.oid.to_hex()).collect();
    assert_eq!(listed, ours);
    assert!(all.iter().any(|r| {
        r.edges
            .iter()
            .any(|e| e.kind == EdgeKind::Outgoing && e.from != e.to)
    }));

    // Every window from checkpoints equals the full layout.
    let mut windowed = view(&cache_dir, &caches, options(50));
    assert_eq!(open(&mut windowed, &repo), CacheStatus::Reused);
    let mut start = 0;
    while start < n {
        let w = windowed.rows(start..start + SCREEN).unwrap();
        let end = (start + SCREEN).min(n);
        assert_eq!(w, all[start as usize..end as usize]);
        start += 17;
    }

    // Badges.
    assert!(
        all.iter()
            .any(|r| r.refs.iter().any(|t| t.kind == RefKind::Head))
    );
    assert!(all.iter().any(|r| r.refs.iter().any(|t| t.name == "main")));

    // Metadata of a window matches git.
    let metas = windowed.meta(100..110).unwrap();
    for (m, r) in metas.iter().zip(&all[100..110]) {
        assert_eq!(m.oid, r.oid);
        let subject = git(
            repo.as_path(),
            &["log", "-1", "--format=%s", &r.oid.to_hex()],
        )
        .unwrap();
        assert_eq!(m.summary, subject);
    }

    // Search.
    let cancel = CancelToken::new();
    let hits = windowed
        .search(&SearchQuery::new("Dev 3"), &cancel)
        .unwrap();
    assert!(!hits.rows.is_empty());
    let target = all[700].oid.to_hex();
    let hits = windowed
        .search(&SearchQuery::new(&target[..12]), &cancel)
        .unwrap();
    assert!(hits.rows.contains(&700));
    let hits = windowed
        .search(&SearchQuery::new("feature/3 step 2"), &cancel)
        .unwrap();
    assert_eq!(hits.rows.len(), 1);

    // A new commit on main moves the tips: refresh rebuilds.
    git(
        repo.as_path(),
        &["commit", "-q", "--allow-empty", "-m", "on top"],
    )
    .unwrap();
    assert_eq!(
        windowed.refresh(&NoProgress, &cancel).unwrap(),
        RefreshOutcome::Rebuilt { rows: n + 1 }
    );
    let top = windowed.rows(0..1).unwrap();
    assert_eq!(windowed.meta(0..1).unwrap()[0].summary, "on top");
    assert!(top[0].refs.iter().any(|t| t.name == "main"));
    // A new branch on an existing tip only changes badges.
    git(repo.as_path(), &["branch", "topic", "main"]).unwrap();
    assert_eq!(
        windowed.refresh(&NoProgress, &cancel).unwrap(),
        RefreshOutcome::BadgesUpdated
    );
}

// ---------------------------------------------------------------- benchmark

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1e3
}

fn stats(mut v: Vec<Duration>) -> String {
    v.sort();
    format!(
        "median {:.3} ms, min {:.3} ms, max {:.3} ms (n={})",
        ms(v[v.len() / 2]),
        ms(v[0]),
        ms(v[v.len() - 1]),
        v.len()
    )
}

fn time<T>(f: impl FnOnce() -> T) -> (T, Duration) {
    let t = Instant::now();
    let r = f();
    (r, t.elapsed())
}

/// Deterministic positions spread over the history.
fn positions(n: u32, count: u32) -> Vec<u32> {
    let mut x = 0x2545_f491_u64;
    (0..count)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            u32::try_from(x % u64::from(n - SCREEN)).unwrap()
        })
        .collect()
}

fn first_screen(v: &HistoryView) -> (Vec<GraphRow>, usize) {
    let rows = v.rows(0..SCREEN).unwrap();
    let metas = v.meta(0..SCREEN).unwrap();
    (rows, metas.len())
}

#[test]
#[ignore = "benchmark: 100k-commit fixture, run in release with --ignored --nocapture"]
#[allow(clippy::too_many_lines, clippy::cast_precision_loss)] // linear benchmark script
fn bench_100k() {
    // The fixture is kept in cargo's target tmp dir and reused across runs.
    let path = Path::new(env!("CARGO_TARGET_TMPDIR")).join("graph-bench-100k");
    if !path.join(".git").exists() {
        let _ = std::fs::remove_dir_all(&path);
        let ((), t) = time(|| {
            generate_history(&path, HistoryShape::default()).unwrap();
            git(&path, &["commit-graph", "write", "--reachable"]).unwrap();
        });
        println!("fixture generation: {:.0} ms", ms(t));
    }
    let repo = RepoPath::new(&path);
    let dir = tempfile::tempdir().unwrap();
    let cache_dir = dir.path().join("cache");
    let caches = Arc::new(CacheRegistry::new());
    let gix = GixReader::default();

    // Raw backend costs for reference.
    let walk_once = || {
        let mut n = 0u32;
        archipel_model::ports::HistoryReader::walk_topology(
            &gix,
            &repo,
            WalkOrder::Topological,
            &mut |_| {
                n += 1;
                std::ops::ControlFlow::Continue(())
            },
            &CancelToken::new(),
        )
        .unwrap();
        n
    };
    let count = walk_once();
    let walks: Vec<Duration> = (0..3).map(|_| time(walk_once).1).collect();
    println!("raw gix topology walk ({count} commits): {}", stats(walks));
    let tips: Vec<Duration> = (0..5)
        .map(|_| time(|| archipel_model::ports::RefReader::ref_tips(&gix, &repo).unwrap()).1)
        .collect();
    println!("raw gix ref_tips: {}", stats(tips));

    // Cold open: no cache file.
    let mut cold = Vec::new();
    for _ in 0..3 {
        let _ = std::fs::remove_dir_all(&cache_dir);
        let mut v = view(&cache_dir, &caches, options(1000));
        let ((status, screen), t) = time(|| {
            let s = open(&mut v, &repo);
            (s, first_screen(&v).1)
        });
        assert_eq!(status, CacheStatus::BuiltMissing);
        assert_eq!(screen, SCREEN as usize);
        cold.push(t);
    }
    println!(
        "time to first rows, cold (walk + build cache + checkpoints + first screen): {}",
        stats(cold)
    );
    let file_len = std::fs::metadata(view(&cache_dir, &caches, options(1000)).cache_path(&repo))
        .unwrap()
        .len();
    println!(
        "cache file: {file_len} bytes ({:.1} bytes/commit)",
        file_len as f64 / f64::from(count)
    );

    // Warm open: cache reused.
    let mut warm = Vec::new();
    let mut open_only = Vec::new();
    for _ in 0..5 {
        let mut v = view(&cache_dir, &caches, options(1000));
        let (status, t_open) = time(|| open(&mut v, &repo));
        let (_, t_screen) = time(|| first_screen(&v));
        assert_eq!(status, CacheStatus::Reused);
        open_only.push(t_open);
        warm.push(t_open + t_screen);
    }
    println!(
        "open, warm (ref tips + verify file + checkpoints): {}",
        stats(open_only)
    );
    println!("time to first rows, warm: {}", stats(warm));

    let mut v = view(&cache_dir, &caches, options(1000));
    open(&mut v, &repo);
    let n = v.row_count();
    assert_eq!(n, count);

    // Deep windows: layout only, then layout + metadata (cold LRU).
    let pos = positions(n, 50);
    let mut layout_times = Vec::new();
    let mut meta_times = Vec::new();
    let mut oid_reads = Vec::new();
    for &p in &pos {
        layout_times.push(time(|| v.rows(p..p + SCREEN).unwrap()).1);
        meta_times.push(time(|| v.meta(p..p + SCREEN).unwrap()).1);
        oid_reads.push(time(|| v.oids(p..p + 200).unwrap()).1);
    }
    println!(
        "deep window layout ({SCREEN} rows, replay from checkpoint): {}",
        stats(layout_times)
    );
    println!(
        "deep window metadata ({SCREEN} rows ± overscan, LRU miss): {}",
        stats(meta_times)
    );
    println!(
        "positioned read of a random 200-row id window: {}",
        stats(oid_reads)
    );
    let (_, scan) = time(|| {
        v.row_of(Oid::from_bytes(&[0xab; 20]).unwrap(), &CancelToken::new())
            .unwrap()
    });
    println!(
        "full sequential id scan with positioned reads ({n} rows): {:.2} ms",
        ms(scan)
    );

    // Scroll the whole history, screen by screen: memory must stay flat.
    v.unload();
    open(&mut v, &repo);
    let base = v.memory_usage();
    let mut samples = Vec::new();
    let mut frame_times = Vec::new();
    let mut start = 0;
    let t = Instant::now();
    while start < n {
        let ((), f) = time(|| {
            v.rows(start..start + SCREEN).unwrap();
            v.meta(start..start + SCREEN).unwrap();
        });
        frame_times.push(f);
        if (start / SCREEN).is_multiple_of(200) {
            samples.push((start, v.memory_usage()));
        }
        start += SCREEN;
    }
    println!(
        "scroll whole history ({} screens): {:.0} ms total; per screen {}",
        frame_times.len(),
        ms(t.elapsed()),
        stats(frame_times)
    );
    let end = v.memory_usage();
    println!("memory after open: {base:?}");
    for (row, m) in samples.iter().step_by(4) {
        println!(
            "  at row {row:>6}: total {} bytes (checkpoints {}, badges {}, meta {})",
            m.total(),
            m.checkpoints,
            m.badges,
            m.meta_cache
        );
    }
    println!(
        "memory after full scroll: total {} bytes (meta {} / budget {})",
        end.total(),
        end.meta_cache,
        end.meta_budget
    );
    assert!(end.meta_cache <= end.meta_budget);

    // Checkpoint interval trade-off.
    for interval in [250, 1000, 4000] {
        let mut v = view(&cache_dir, &caches, options(interval));
        let (_, t_open) = time(|| open(&mut v, &repo));
        let usage = v.memory_usage();
        let windows: Vec<Duration> = pos
            .iter()
            .map(|&p| time(|| v.rows(p..p + SCREEN).unwrap()).1)
            .collect();
        println!(
            "interval {interval:>4}: open {:.1} ms, {} checkpoints = {} bytes, deep window {}",
            ms(t_open),
            usage.checkpoint_count,
            usage.checkpoints,
            stats(windows)
        );
    }

    // Refresh.
    let cancel = CancelToken::new();
    let (outcome, t) = time(|| v.refresh(&NoProgress, &cancel).unwrap());
    assert_eq!(outcome, RefreshOutcome::Unchanged);
    println!("refresh, refs unchanged: {:.2} ms", ms(t));
    git(
        repo.as_path(),
        &["commit", "-q", "--allow-empty", "-m", "new tip"],
    )
    .unwrap();
    let (outcome, t) = time(|| v.refresh(&NoProgress, &cancel).unwrap());
    assert_eq!(outcome, RefreshOutcome::Rebuilt { rows: n + 1 });
    println!("refresh after a new commit (full rebuild): {:.1} ms", ms(t));
    // Restore the shared fixture.
    git(repo.as_path(), &["reset", "-q", "--hard", "HEAD~1"]).unwrap();

    // Search.
    let searcher = v.searcher().unwrap();
    let (hits, t) = time(|| {
        searcher
            .search(&SearchQuery::new("feature/1234 step 2"), &cancel)
            .unwrap()
    });
    println!(
        "search message over {} rows: {:.0} ms ({} hits)",
        n + 1,
        ms(t),
        hits.rows.len()
    );
    let target = v.oids(n / 2..n / 2 + 1).unwrap()[0].to_hex();
    let (hits, t) = time(|| {
        searcher
            .search(&SearchQuery::new(&target[..10]), &cancel)
            .unwrap()
    });
    println!(
        "search hash prefix: {:.0} ms ({} hits)",
        ms(t),
        hits.rows.len()
    );
}
