//! Read-backend benchmark (PLAN Phase 0, step 3): `gix` vs the `git` CLI.
//!
//! Run with `cargo bench -p archipel-git --bench read_backends`.
//! Fixtures are generated once in a temporary folder (a 20-repo project, a
//! 100k-commit history and a 20k-file worktree), then every scenario is
//! timed several times; the median and the minimum are printed as JSON lines.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, missing_docs)]

use std::ops::ControlFlow;
use std::path::Path;
use std::time::{Duration, Instant};

use archipel_fixtures::history::{HistoryShape, generate_history};
use archipel_fixtures::project::generate_project;
use archipel_fixtures::{git, write_file};
use archipel_git::{GitCli, GixReader};
use archipel_model::history::WalkOrder;
use archipel_model::ports::{HistoryReader, StatusReader};
use archipel_model::{CancelToken, RepoPath};
use rayon::prelude::*;

fn measure(name: &str, backend: &str, runs: usize, mut f: impl FnMut()) {
    f(); // warm-up (OS file cache)
    let mut times: Vec<Duration> = (0..runs)
        .map(|_| {
            let t = Instant::now();
            f();
            t.elapsed()
        })
        .collect();
    times.sort();
    println!(
        "{{\"scenario\":\"{name}\",\"backend\":\"{backend}\",\"median_ms\":{:.2},\"min_ms\":{:.2},\"runs\":{runs}}}",
        times[runs / 2].as_secs_f64() * 1e3,
        times[0].as_secs_f64() * 1e3
    );
}

fn big_worktree(dir: &Path, files: usize) {
    git(
        dir.parent().unwrap(),
        &["init", "-q", "-b", "main", dir.to_str().unwrap()],
    )
    .unwrap();
    archipel_fixtures::configure(dir).unwrap();
    for i in 0..files {
        write_file(
            dir,
            &format!("d{}/e{}/f{i}.txt", i % 50, i % 7),
            &format!("{i}\n"),
        )
        .unwrap();
    }
    git(dir, &["add", "-A"]).unwrap();
    git(dir, &["commit", "-q", "-m", "big"]).unwrap();
    for i in 0..10 {
        write_file(
            dir,
            &format!("d{}/e{}/f{i}.txt", i % 50, i % 7),
            "changed\n",
        )
        .unwrap();
    }
}

fn walk_count(reader: &dyn HistoryReader, repo: &RepoPath, limit: usize) -> usize {
    let mut n = 0;
    reader
        .walk_topology(
            repo,
            WalkOrder::Topological,
            &mut |_| {
                n += 1;
                if n >= limit {
                    ControlFlow::Break(())
                } else {
                    ControlFlow::Continue(())
                }
            },
            &CancelToken::new(),
        )
        .unwrap();
    n
}

fn main() {
    let filter = std::env::args().skip(1).find(|a| !a.starts_with('-'));
    let enabled = |s: &str| filter.as_deref().is_none_or(|f| s.contains(f));
    let tmp = tempfile::tempdir().unwrap();
    let cli = GitCli::default();
    let gix = GixReader::default();
    let gix_uncached = GixReader::uncached(4 * 1024 * 1024);
    let backends: [(&str, &dyn StatusReader); 3] = [
        ("cli", &cli),
        ("gix", &gix),
        ("gix-uncached", &gix_uncached),
    ];

    if enabled("summary") {
        eprintln!("generating 20-repo project…");
        let repos: Vec<RepoPath> = generate_project(&tmp.path().join("project"), 20, 30)
            .unwrap()
            .into_iter()
            .map(|r| RepoPath::new(r.path))
            .collect();
        for (name, reader) in backends {
            measure("summary_20_repos_parallel", name, 15, || {
                let ok = repos
                    .par_iter()
                    .filter(|r| reader.summary(r).is_ok())
                    .count();
                assert_eq!(ok, 20);
            });
            measure("summary_20_repos_sequential", name, 7, || {
                for r in &repos {
                    reader.summary(r).unwrap();
                }
            });
            measure("summary_single_repo", name, 30, || {
                reader.summary(&repos[1]).unwrap();
            });
        }
    }

    if enabled("status") {
        eprintln!("generating 20k-file worktree…");
        let wt = tmp.path().join("bigwt");
        big_worktree(&wt, 20_000);
        let wt = RepoPath::new(wt);
        for (name, reader) in backends {
            measure("changes_20k_files", name, 9, || {
                assert_eq!(reader.changes(&wt).unwrap().len(), 10);
            });
            measure("summary_20k_files", name, 9, || {
                reader.summary(&wt).unwrap();
            });
        }
    }

    if enabled("history") {
        eprintln!("generating 100k-commit history…");
        let big = tmp.path().join("big");
        generate_history(&big, HistoryShape::default()).unwrap();
        git(&big, &["commit-graph", "write", "--reachable"]).unwrap();
        let big = RepoPath::new(big);
        let readers: [(&str, &dyn HistoryReader); 3] = [
            ("cli", &cli),
            ("gix", &gix),
            ("gix-uncached", &gix_uncached),
        ];
        let total = walk_count(&cli, &big, usize::MAX);
        for (name, reader) in readers {
            measure("walk_100k_full", name, 5, || {
                assert_eq!(walk_count(reader, &big, usize::MAX), total);
            });
            measure("walk_100k_first_1000", name, 9, || {
                walk_count(reader, &big, 1000);
            });
            let mut oids = Vec::new();
            reader
                .walk_topology(
                    &big,
                    WalkOrder::Topological,
                    &mut |n| {
                        oids.push(n.oid);
                        if oids.len() >= 50_200 {
                            ControlFlow::Break(())
                        } else {
                            ControlFlow::Continue(())
                        }
                    },
                    &CancelToken::new(),
                )
                .unwrap();
            let window = &oids[50_000..50_200];
            measure("commit_meta_200", name, 15, || {
                assert_eq!(reader.commit_meta(&big, window).unwrap().len(), 200);
            });
        }
    }
}
