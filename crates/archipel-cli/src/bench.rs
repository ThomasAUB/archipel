//! `archipel-cli bench`: the performance and memory harness (PLAN §5.1).
//!
//! Every scenario prints one JSON line `{scenario, value, unit, target, ok}`.
//! With `--check`, a missed target makes the command fail (used by CI).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use archipel_compose::{AppDirs, Composition, compose};
use archipel_fixtures::history::{HistoryShape, generate_history};
use archipel_fixtures::project::generate_project;
use archipel_model::project::{ProjectDef, ProjectLocation, ProjectSettings, RepoEntry};
use archipel_model::{CancelToken, NoProgress, ProjectId, RepoPath};

use crate::commands::wait_fresh;

/// A measured value and its target.
struct Measure {
    scenario: &'static str,
    value: f64,
    unit: &'static str,
    /// Upper bound (`None`: informational).
    target: Option<f64>,
}

impl Measure {
    fn ok(&self) -> bool {
        self.target.is_none_or(|t| self.value <= t)
    }

    fn print(&self) {
        let target = self
            .target
            .map_or_else(|| "null".to_owned(), |t| format!("{t}"));
        println!(
            "{{\"scenario\":\"{}\",\"value\":{:.2},\"unit\":\"{}\",\"target\":{target},\"ok\":{}}}",
            self.scenario,
            self.value,
            self.unit,
            self.ok()
        );
    }
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1e3
}

/// Resident set size in MiB.
#[allow(clippy::cast_precision_loss)] // byte counts far below 2^52
fn rss_mib() -> f64 {
    memory_stats::memory_stats().map_or(0.0, |m| m.physical_mem as f64 / (1024.0 * 1024.0))
}

struct Fixtures {
    _temp: Option<tempfile::TempDir>,
    project_repos: Vec<RepoPath>,
    history: RepoPath,
}

fn fixtures(dir: Option<PathBuf>, quick: bool) -> Result<Fixtures> {
    let (root, temp) = if let Some(d) = dir {
        (d, None)
    } else {
        let t = tempfile::Builder::new()
            .prefix("archipel-bench-")
            .tempdir()?;
        (t.path().to_path_buf(), Some(t))
    };
    let (repos, commits) = if quick { (20, 5) } else { (20, 30) };
    let project_dir = root.join("project");
    if !project_dir.join("repos").exists() {
        eprintln!("generating the {repos}-repo project…");
        generate_project(&project_dir, repos, commits)?;
    }
    let mut project_repos: Vec<RepoPath> = std::fs::read_dir(project_dir.join("repos"))?
        .filter_map(Result::ok)
        .map(|e| RepoPath::new(e.path()))
        .collect();
    project_repos.sort_by(|a, b| a.as_path().cmp(b.as_path()));
    let history = root.join(if quick { "history-10k" } else { "history-100k" });
    if !history.join(".git").exists() {
        let shape = HistoryShape {
            commits: if quick { 10_000 } else { 100_000 },
            ..HistoryShape::default()
        };
        eprintln!("generating a {}-commit history…", shape.commits);
        generate_history(&history, shape)?;
        archipel_fixtures::git(&history, &["commit-graph", "write", "--reachable"])?;
    }
    Ok(Fixtures {
        _temp: temp,
        project_repos,
        history: RepoPath::new(history),
    })
}

fn project(name: &str, repos: &[RepoPath]) -> ProjectDef {
    ProjectDef {
        name: name.into(),
        repos: repos
            .iter()
            .map(|p| RepoEntry {
                path: p.clone(),
                alias: None,
            })
            .collect(),
        settings: ProjectSettings::default(),
    }
}

fn measure(scenario: &'static str, value: f64, unit: &'static str, target: Option<f64>) -> Measure {
    Measure {
        scenario,
        value,
        unit,
        target,
    }
}

const BIG: &str = "bench-20";
const SMALL: &str = "bench-5";

/// First session: creates the projects, then times a fresh dashboard.
fn first_session(home: &Path, fx: &Fixtures, out: &mut Vec<Measure>) -> Result<()> {
    let session = compose(AppDirs::under(home))?;
    let app = &session.app;
    let big = ProjectId(BIG.into());
    app.create_project(
        ProjectLocation::Private(big.clone()),
        &project("20 repos", &fx.project_repos),
    )?;
    app.create_project(
        ProjectLocation::Private(ProjectId(SMALL.into())),
        &project("5 repos", &fx.project_repos[..5]),
    )?;
    app.open_project(&big)?;
    wait_fresh(app)?;
    // Warm OS cache, recompute everything (spec < 500 ms).
    let started = Instant::now();
    app.refresh_all();
    wait_fresh(app)?;
    out.push(measure(
        "dashboard_20_repos_fresh",
        ms(started.elapsed()),
        "ms",
        Some(500.0),
    ));
    app.close_project();
    Ok(())
}

/// Second session: start → cached paint, project switches, idle RSS.
fn second_session(home: &Path, quick: bool, out: &mut Vec<Measure>) -> Result<Composition> {
    let rss_before = rss_mib();
    let started = Instant::now();
    let session = compose(AppDirs::under(home))?;
    let (big, small) = (ProjectId(BIG.into()), ProjectId(SMALL.into()));
    session.app.open_project(&big)?;
    // Fixture generation happens before: measure from the composition.
    out.push(measure(
        "start_to_cached_paint",
        ms(started.elapsed()),
        "ms",
        Some(1000.0),
    ));
    wait_fresh(&session.app)?;
    let mut worst = Duration::ZERO;
    for id in [&small, &big, &small, &big] {
        let switch = Instant::now();
        session.app.open_project(id)?;
        worst = worst.max(switch.elapsed());
        wait_fresh(&session.app)?;
    }
    out.push(measure(
        "project_switch_paint_worst",
        ms(worst),
        "ms",
        Some(200.0),
    ));
    std::thread::sleep(Duration::from_secs(if quick { 1 } else { 5 }));
    out.push(measure("idle_rss_20_repos", rss_mib(), "MiB", Some(150.0)));
    out.push(measure("rss_before_compose", rss_before, "MiB", None));
    Ok(session)
}

/// Graph: first rows, cached reopen, full scroll with flat memory.
fn graph(session: &Composition, history: &RepoPath, out: &mut Vec<Measure>) -> Result<()> {
    let mut view = session.app.history_view();
    let started = Instant::now();
    let report = view.open(history, &NoProgress, &CancelToken::new())?;
    let window = view.rows(0..SCREEN)?.len();
    view.meta(0..SCREEN)?;
    out.push(measure(
        "graph_first_rows_cold",
        ms(started.elapsed()),
        "ms",
        Some(500.0),
    ));
    if window == 0 {
        bail!("the graph returned no rows");
    }
    let started = Instant::now();
    view.open(history, &NoProgress, &CancelToken::new())?;
    view.rows(0..SCREEN)?;
    view.meta(0..SCREEN)?;
    out.push(measure(
        "graph_first_rows_cached",
        ms(started.elapsed()),
        "ms",
        Some(500.0),
    ));

    let rss_before = rss_mib();
    let budget_mib = to_mib(session.app.settings().commit_cache_bytes);
    let started = Instant::now();
    let mut worst = Duration::ZERO;
    for first in (0..report.rows).step_by(SCREEN as usize) {
        let screen = Instant::now();
        view.rows(first..first + SCREEN)?;
        view.meta(first..first + SCREEN)?;
        worst = worst.max(screen.elapsed());
    }
    out.push(measure(
        "graph_scroll_all_total",
        ms(started.elapsed()),
        "ms",
        None,
    ));
    out.push(measure(
        "graph_scroll_worst_screen",
        ms(worst),
        "ms",
        Some(50.0),
    ));
    out.push(measure(
        "graph_scroll_rss_growth",
        rss_mib() - rss_before,
        "MiB",
        Some(budget_mib + 32.0),
    ));
    view.unload();
    out.push(measure("rss_after_unload", rss_mib(), "MiB", Some(150.0)));
    Ok(())
}

/// Rows of one screen of the graph.
const SCREEN: u32 = 60;

/// Runs every scenario.
pub fn run(fixtures_dir: Option<PathBuf>, check: bool, quick: bool) -> Result<()> {
    let fx = fixtures(fixtures_dir, quick)?;
    let home = tempfile::Builder::new()
        .prefix("archipel-bench-home-")
        .tempdir()?;
    let mut results = Vec::new();
    first_session(home.path(), &fx, &mut results)?;
    let session = second_session(home.path(), quick, &mut results)?;
    graph(&session, &fx.history, &mut results)?;
    for m in &results {
        m.print();
    }
    let failed: Vec<&str> = results
        .iter()
        .filter(|m| !m.ok())
        .map(|m| m.scenario)
        .collect();
    if check && !failed.is_empty() {
        bail!("targets missed: {}", failed.join(", "));
    }
    Ok(())
}

#[allow(clippy::cast_precision_loss)] // budgets far below 2^52 bytes
fn to_mib(bytes: usize) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}
