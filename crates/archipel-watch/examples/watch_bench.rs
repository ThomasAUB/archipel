#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Measures the cost of watching repositories.
//!
//! `cargo run --release -p archipel-watch --example watch_bench -- [repos] [files]`
//!
//! * time of `watch()` per repo (shared source) and for the whole project;
//! * the alternative: one `notify` watcher per repo (setup time, threads);
//! * idle thread count and CPU time;
//! * an event storm (rewriting every file of one repo): flushes delivered.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use archipel_model::ports::{ChangeSource, Invalidation};
use archipel_model::{RepoId, RepoPath};
use archipel_watch::{DebounceConfig, NotifyChangeSource};
use notify::{RecursiveMode, Watcher};

const DEFAULT_REPOS: usize = 20;
const DEFAULT_FILES: usize = 20_000;
const FILES_PER_DIR: usize = 100;
const IDLE: Duration = Duration::from_secs(5);
const STORM_WAIT: Duration = Duration::from_secs(5);

fn make_repo(root: &Path, name: &str, files: usize) -> PathBuf {
    let dir = root.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    archipel_fixtures::git(&dir, &["init", "-q", "-b", "main"]).unwrap();
    for i in 0..files {
        let sub = dir.join(format!("d{:03}", i / FILES_PER_DIR));
        if i % FILES_PER_DIR == 0 {
            std::fs::create_dir_all(&sub).unwrap();
        }
        std::fs::write(sub.join(format!("f{i}.txt")), "x").unwrap();
    }
    dir
}

fn rewrite_all(repo: &Path, files: usize, content: &str) {
    for i in 0..files {
        let sub = repo.join(format!("d{:03}", i / FILES_PER_DIR));
        std::fs::write(sub.join(format!("f{i}.txt")), content).unwrap();
    }
}

/// (threads, cpu ms) of this process.
fn process_stats() -> (String, String) {
    #[cfg(windows)]
    {
        let pid = std::process::id();
        let out = std::process::Command::new("powershell")
            .args([
                "-NoProfile",
                "-Command",
                &format!(
                    "$p=Get-Process -Id {pid}; \"$($p.Threads.Count) $([int]$p.TotalProcessorTime.TotalMilliseconds)\""
                ),
            ])
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout).trim().to_owned();
        let mut it = text.split(' ');
        (
            it.next().unwrap_or("?").to_owned(),
            it.next().unwrap_or("?").to_owned(),
        )
    }
    #[cfg(not(windows))]
    {
        let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
        let threads = status
            .lines()
            .find_map(|l| l.strip_prefix("Threads:"))
            .map_or("?".to_owned(), |s| s.trim().to_owned());
        (threads, "?".to_owned())
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let repos: usize = args.get(1).map_or(DEFAULT_REPOS, |s| s.parse().unwrap());
    let files: usize = args.get(2).map_or(DEFAULT_FILES, |s| s.parse().unwrap());

    let tmp = tempfile::tempdir().unwrap();
    let start = Instant::now();
    let paths: Vec<PathBuf> = (0..repos)
        .map(|i| make_repo(tmp.path(), &format!("r{i}"), files))
        .collect();
    println!(
        "fixture: {repos} repos x {files} files in {:.1?}",
        start.elapsed()
    );
    let (t0, base0) = process_stats();
    rewrite_all(&paths[0], files, "z");
    let (_, base1) = process_stats();
    println!(
        "threads before watching: {t0}; unwatched rewrite of repo 0: process cpu ms {base0} -> {base1}"
    );

    let layout = archipel_watch::RepoLayout::locate(&paths[0]).unwrap();
    let root = layout.worktree.clone().unwrap();
    let mut classifier = archipel_watch::RepoClassifier::new(layout);
    let t = Instant::now();
    for i in 0..files {
        let p = root
            .join(format!("d{:03}", i / FILES_PER_DIR))
            .join(format!("f{i}.txt"));
        let _ = classifier.classify(&p);
    }
    println!("classify: {files} worktree paths in {:.2?}", t.elapsed());

    // --- Shared source (the implementation) ---
    let count = Arc::new(AtomicUsize::new(0));
    let src = NotifyChangeSource::new(DebounceConfig::default()).unwrap();
    let mut handles = Vec::new();
    let mut per_repo = Vec::new();
    let all = Instant::now();
    for (i, p) in paths.iter().enumerate() {
        let c = Arc::clone(&count);
        let t = Instant::now();
        let h = src
            .watch(
                RepoId(i as u64),
                &RepoPath::new(p),
                Arc::new(move |_inv: Invalidation| {
                    c.fetch_add(1, Ordering::Relaxed);
                }),
            )
            .unwrap();
        per_repo.push(t.elapsed());
        handles.push(h);
    }
    let total = all.elapsed();
    per_repo.sort();
    println!(
        "shared source: watch() total {total:.2?}, per repo min {:.2?} median {:.2?} max {:.2?}, coverage {:?}",
        per_repo[0],
        per_repo[per_repo.len() / 2],
        per_repo[per_repo.len() - 1],
        handles[0].coverage
    );
    let (threads, cpu0) = process_stats();
    println!("shared source: threads while watching {threads}");
    std::thread::sleep(IDLE);
    let (_, cpu1) = process_stats();
    println!("shared source: process cpu ms before/after {IDLE:?} idle: {cpu0} / {cpu1}");

    // Event storm in one repo: rewrite every file.
    count.store(0, Ordering::Relaxed);
    let storm = Instant::now();
    rewrite_all(&paths[0], files, "y");
    let write_time = storm.elapsed();
    std::thread::sleep(STORM_WAIT);
    let (_, cpu2) = process_stats();
    println!("storm: process cpu ms before {cpu1}");
    println!(
        "storm: rewrote {files} files in {write_time:.2?}; {} invalidations delivered; process cpu ms now {cpu2}",
        count.load(Ordering::Relaxed)
    );
    let drop_t = Instant::now();
    drop(handles);
    println!(
        "shared source: dropping all handles {:.2?}",
        drop_t.elapsed()
    );
    drop(src);

    // --- Alternative: one notify watcher per repo ---
    let all = Instant::now();
    let mut watchers = Vec::new();
    for p in &paths {
        let mut w = notify::recommended_watcher(|_res| {}).unwrap();
        w.watch(p, RecursiveMode::Recursive).unwrap();
        watchers.push(w);
    }
    let total = all.elapsed();
    let (threads, _) = process_stats();
    println!("per-repo watchers: setup total {total:.2?}, threads while watching {threads}");
    let (_, raw0) = process_stats();
    rewrite_all(&paths[0], files, "w");
    std::thread::sleep(STORM_WAIT);
    let (_, raw1) = process_stats();
    println!(
        "per-repo watchers (no-op handler): rewrite of repo 0: process cpu ms {raw0} -> {raw1}"
    );
    drop(watchers);
}
