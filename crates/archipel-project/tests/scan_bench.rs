#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Scan benchmark: sequential vs parallel on a generated tree.
//!
//! ```text
//! cargo test -p archipel-project --release --test scan_bench -- --ignored --nocapture
//! ```
//!
//! The tree has `GROUPS × SUBGROUPS × LEAVES` leaf folders (2,000+ folders in
//! total), `REPOS` of which are repositories with their own content, plus a
//! few files per folder. The non-ignored test checks both strategies agree.

use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use archipel_model::cancel::CancelToken;
use archipel_model::ports::project::RepoDiscovery;
use archipel_model::project::ScanOptions;
use archipel_project::{FsRepoDiscovery, ScanStrategy};

/// Top-level folders.
const GROUPS: usize = 20;
/// Folders per group.
const SUBGROUPS: usize = 10;
/// Leaf folders per subgroup.
const LEAVES: usize = 10;
/// Number of leaves that are repositories.
const REPOS: usize = 50;
/// Plain files written in every non-repository folder.
const FILES_PER_DIR: usize = 3;
/// Folders created inside each repository (skipped unless descending).
const DIRS_PER_REPO: usize = 20;
/// Timed runs per strategy (after one warm-up run).
const RUNS: usize = 10;
/// Exit criterion of PLAN Phase 1.
const BUDGET: Duration = Duration::from_secs(1);

/// Builds the tree; returns the number of folders created.
fn build_tree(root: &Path) -> usize {
    let leaves = GROUPS * SUBGROUPS * LEAVES;
    let repo_every = leaves / REPOS;
    let mut dirs = 0;
    let mut leaf_index = 0;
    for g in 0..GROUPS {
        for s in 0..SUBGROUPS {
            let sub = root
                .join(format!("group-{g:02}"))
                .join(format!("sub-{s:02}"));
            for l in 0..LEAVES {
                let leaf = sub.join(format!("leaf-{l:02}"));
                fs::create_dir_all(&leaf).unwrap();
                dirs += 1;
                if leaf_index % repo_every == 0 {
                    fs::create_dir_all(leaf.join(".git").join("objects")).unwrap();
                    for d in 0..DIRS_PER_REPO {
                        fs::create_dir_all(leaf.join("src").join(format!("mod-{d:02}"))).unwrap();
                        dirs += 1;
                    }
                } else {
                    for f in 0..FILES_PER_DIR {
                        fs::write(leaf.join(format!("file-{f}.txt")), "x").unwrap();
                    }
                }
                leaf_index += 1;
            }
            dirs += 1;
        }
        dirs += 1;
    }
    dirs
}

fn time(strategy: ScanStrategy, root: &Path, options: &ScanOptions) -> (Duration, Duration, usize) {
    let discovery = FsRepoDiscovery::with_strategy(strategy);
    let cancel = CancelToken::new();
    let count = discovery.scan(root, options, &cancel).unwrap().len();
    let mut samples: Vec<Duration> = (0..RUNS)
        .map(|_| {
            let start = Instant::now();
            let found = discovery.scan(root, options, &cancel).unwrap();
            let elapsed = start.elapsed();
            assert_eq!(found.len(), count);
            elapsed
        })
        .collect();
    samples.sort();
    (samples[0], samples[RUNS / 2], count)
}

#[test]
fn strategies_agree_on_generated_tree() {
    let tmp = tempfile::tempdir().unwrap();
    build_tree(tmp.path());
    let options = ScanOptions::default();
    let cancel = CancelToken::new();
    let seq = FsRepoDiscovery::with_strategy(ScanStrategy::Sequential)
        .scan(tmp.path(), &options, &cancel)
        .unwrap();
    let par = FsRepoDiscovery::with_strategy(ScanStrategy::Parallel)
        .scan(tmp.path(), &options, &cancel)
        .unwrap();
    assert_eq!(seq.len(), REPOS);
    assert_eq!(seq, par);
}

#[test]
#[ignore = "benchmark: run with --release -- --ignored --nocapture"]
fn bench_scan_strategies() {
    let tmp = tempfile::tempdir().unwrap();
    let dirs = build_tree(tmp.path());
    println!("tree: {dirs} folders, {REPOS} repositories");
    for descend in [false, true] {
        let options = ScanOptions {
            descend_into_repos: descend,
            ..ScanOptions::default()
        };
        for strategy in [ScanStrategy::Sequential, ScanStrategy::Parallel] {
            let (min, median, count) = time(strategy, tmp.path(), &options);
            println!(
                "descend_into_repos={descend:<5} {strategy:?}: min {min:?}, median {median:?} ({count} repos)"
            );
            assert!(median < BUDGET, "{strategy:?} scan took {median:?}");
        }
    }
}
