//! Headless commands.

use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use archipel_app::dashboard::RepoStatus;
use archipel_app::{App, AppEvent, Command};
use archipel_compose::{AppDirs, compose};
use archipel_graph::EdgeKind;
use archipel_model::project::ScanOptions;
use archipel_model::{CancelToken, NoProgress, ProjectId, RepoPath, Timestamp};
use archipel_stats::{PauseToken, RepoTarget, StatsConfig, StatsRegistry};

/// How long headless commands wait for background work.
const WAIT: Duration = Duration::from_secs(600);

/// Registers `target` (a project file, or a folder scanned into a new
/// `archipel.toml`) and opens it. Returns the project id.
fn open_target(app: &App, target: &Path) -> Result<ProjectId> {
    let target = std::path::absolute(target)?;
    if !target.is_file() {
        return Ok(app.new_project_from_folder(&target)?);
    }
    let id = app.import_project_file(&target)?;
    app.open_project(&id)?;
    Ok(id)
}

/// Waits until every row has a fresh summary.
pub fn wait_fresh(app: &App) -> Result<()> {
    let deadline = Instant::now() + WAIT;
    while app.rows().iter().any(|r| r.stale) {
        if Instant::now() > deadline {
            bail!("timed out waiting for summaries");
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    Ok(())
}

/// `tree`: prints the repo dependency tree (spec §9) of a project.
pub fn tree(dirs: AppDirs, target: &Path) -> Result<()> {
    use archipel_app::deptree::{DependencyReaders, TreeInput, build_tree};
    let c = compose(dirs)?;
    open_target(&c.app, target)?;
    let rows = c.app.rows();
    let ports = DependencyReaders::from_ports(c.app.repo_actions().ports());
    let source = c.app.extensions().dependencies.clone();
    let mut dependencies = std::collections::HashMap::new();
    for row in &rows {
        match source.dependencies(&ports, &row.path) {
            Ok(nodes) => {
                dependencies.insert(row.id, nodes);
            }
            Err(e) => eprintln!("{}: {e}", row.name),
        }
    }
    let tree = build_tree(&TreeInput {
        rows: &rows,
        dependencies: &dependencies,
        collapsed: &[],
        skip: &std::collections::HashSet::new(),
    });
    let short = |o: Option<archipel_model::Oid>| o.map_or_else(|| "?".into(), |o| o.short(8));
    for node in tree {
        let mut notes = Vec::new();
        if node.state.is_some() && node.repo.is_some() {
            notes.push("in project".to_owned());
        }
        if let Some(s) = &node.state {
            if !s.initialized {
                notes.push(format!("not initialized, pins {}", short(s.pinned)));
            } else if s.out_of_sync {
                notes.push(format!(
                    "checked out {} != pinned {}",
                    short(s.checked_out),
                    short(s.pinned)
                ));
            }
            if s.dirty {
                notes.push("dirty".into());
            }
        }
        let notes = if notes.is_empty() {
            String::new()
        } else {
            format!("  [{}]", notes.join(", "))
        };
        println!("{}{}{notes}", node.prefix, node.name);
    }
    Ok(())
}

/// `dashboard`: prints the six columns of every repo.
pub fn dashboard(dirs: AppDirs, target: &Path, json: bool) -> Result<()> {
    let c = compose(dirs)?;
    open_target(&c.app, target)?;
    wait_fresh(&c.app)?;
    let rows = c.app.rows();
    let columns = c.app.extensions().columns.clone();
    let now = Timestamp::now();
    if json {
        let out: Vec<serde_json::Value> = rows
            .iter()
            .map(|r| {
                let mut obj = serde_json::Map::new();
                for col in columns.columns() {
                    obj.insert(
                        col.id().to_owned(),
                        serde_json::Value::String(col.cell(r, now).text),
                    );
                }
                serde_json::Value::Object(obj)
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }
    let table: Vec<Vec<String>> = rows
        .iter()
        .map(|r| {
            columns
                .columns()
                .iter()
                .map(|c| c.cell(r, now).text)
                .collect()
        })
        .collect();
    let headers: Vec<&str> = columns.columns().iter().map(|c| c.header()).collect();
    let widths: Vec<usize> = (0..headers.len())
        .map(|i| {
            table
                .iter()
                .map(|row| row[i].chars().count())
                .chain([headers[i].len()])
                .max()
                .unwrap_or(0)
        })
        .collect();
    let line = |cells: &[&str]| {
        cells
            .iter()
            .zip(&widths)
            .map(|(c, w)| format!("{c:<w$}"))
            .collect::<Vec<_>>()
            .join("  ")
    };
    println!("{}", line(&headers));
    for row in &table {
        println!(
            "{}",
            line(&row.iter().map(String::as_str).collect::<Vec<_>>())
        );
    }
    for r in &rows {
        if let RepoStatus::Error(e) = &r.status {
            eprintln!("{}: {e}", r.name);
        }
    }
    Ok(())
}

/// `scan`: prints the repositories found.
pub fn scan(root: &Path) -> Result<()> {
    use archipel_model::ports::RepoDiscovery;
    let started = Instant::now();
    let found = archipel_project::FsRepoDiscovery::new().scan(
        root,
        &ScanOptions::default(),
        &CancelToken::new(),
    )?;
    for repo in &found {
        println!("{}", repo.path.as_path().display());
    }
    eprintln!("{} repositories in {:?}", found.len(), started.elapsed());
    Ok(())
}

/// `fetch`: fetches every repo and prints the batch report.
pub fn fetch(dirs: AppDirs, target: &Path) -> Result<()> {
    let c = compose(dirs)?;
    let mut events = c.app.subscribe();
    open_target(&c.app, target)?;
    wait_fresh(&c.app)?;
    c.app.dispatch(Command::Fetch(Vec::new()))?;
    let (label, done, failed, skipped) = c.runtime.block_on(async {
        loop {
            match events.recv().await {
                Ok(AppEvent::BatchFinished(label, done, failed, skipped)) => {
                    return Ok((label, done, failed, skipped));
                }
                Ok(AppEvent::Op(e)) => {
                    if let archipel_ops::OpEventKind::Finished(Err(msg)) = e.kind {
                        eprintln!("{}: {msg}", e.label);
                    }
                }
                Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(e) => return Err(anyhow::anyhow!(e)),
            }
        }
    })?;
    println!("{label}: {done} done, {failed} failed, {skipped} skipped");
    Ok(())
}

/// `graph`: prints a window of the graph with simple ASCII lanes.
pub fn graph(dirs: AppDirs, repo: &Path, start: u32, count: u32) -> Result<()> {
    let c = compose(dirs)?;
    let repo = RepoPath::new(std::path::absolute(repo)?);
    let mut view = c.app.history_view();
    let started = Instant::now();
    let report = view.open(&repo, &NoProgress, &CancelToken::new())?;
    let first_window = started.elapsed();
    let size = view.wait_complete(&NoProgress)?;
    eprintln!(
        "{} commits, cache {:?}, first rows in {first_window:?}, complete in {:?}",
        size.rows,
        report.cache,
        started.elapsed()
    );
    let range = start..start.saturating_add(count);
    let rows = view.rows(range.clone())?;
    let metas = view.meta(range)?;
    for (row, meta) in rows.iter().zip(&metas) {
        let width = usize::try_from(row.width.max(1)).unwrap_or(1);
        let mut lanes = vec![' '; width * 2];
        for e in &row.edges {
            let col = usize::try_from(if e.kind == EdgeKind::Incoming {
                e.to
            } else {
                e.from
            })
            .unwrap_or(0)
                * 2;
            if col < lanes.len() {
                lanes[col] = '│';
            }
        }
        let col = usize::try_from(row.column).unwrap_or(0) * 2;
        if col < lanes.len() {
            lanes[col] = '●';
        }
        let refs: Vec<&str> = row.refs.iter().map(|r| r.name.as_str()).collect();
        let badges = if refs.is_empty() {
            String::new()
        } else {
            format!(" ({})", refs.join(", "))
        };
        println!(
            "{} {} {}{badges} — {}",
            lanes.iter().collect::<String>(),
            row.oid.short(8),
            meta.summary,
            meta.author_name
        );
    }
    Ok(())
}

/// `stats`: updates the statistics of a repo and prints the three views.
pub fn stats(dirs: AppDirs, repo: &Path) -> Result<()> {
    let c = compose(dirs)?;
    let path = RepoPath::new(std::path::absolute(repo)?);
    let started = Instant::now();
    let outcome = c
        .app
        .stats_engine()
        .update(path.id(), &path, &CancelToken::new(), &PauseToken::new())
        .context("updating statistics")?;
    eprintln!(
        "{} new commit(s) processed in {:?}",
        outcome.commits,
        started.elapsed()
    );
    let registry = StatsRegistry::with_builtins(&StatsConfig::default());
    let stats = registry.compute_repo(&c.app.stat_sources(), &RepoTarget::from_path(path));
    for result in &stats.results {
        println!("{result:#?}");
    }
    Ok(())
}
