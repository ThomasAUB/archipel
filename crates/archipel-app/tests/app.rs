//! App behavior against fakes: project switching paints from cache, refreshes
//! in the background, reacts to watcher invalidations, runs batches and undo.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use archipel_app::{App, AppEvent, Command, Extensions, Services};
use archipel_model::changes::FileChange;
use archipel_model::journal::OperationKind;
use archipel_model::ports::{
    JournalStore, ProjectSource, Scope, StatusReader, SummaryStore, UiStateStore,
};
use archipel_model::project::{ProjectDef, ProjectLocation, RepoEntry};
use archipel_model::requests::{CommitMode, CommitRequest};
use archipel_model::settings::Settings;
use archipel_model::testing::{
    FakeChangeSource, FakeDiscovery, FakeGit, FakeProjectSource, FakeReleaseFeed, InMemoryStore,
    oid,
};
use archipel_model::ui_state::TabState;
use archipel_model::{GitError, HeadState, ProjectId, RepoPath, RepoSummary};
use archipel_ops::GitPorts;

/// Where the test project is stored (in memory: the fake source).
const P1_FILE: &str = "/p1/archipel.toml";

/// Id of the test project.
fn p1_id() -> ProjectId {
    archipel_app::file_project_id(std::path::Path::new(P1_FILE))
}

struct Env {
    rt: tokio::runtime::Runtime,
    git: Arc<FakeGit>,
    store: Arc<InMemoryStore>,
    watcher: Arc<FakeChangeSource>,
    app: Arc<App>,
    releases: Arc<FakeReleaseFeed>,
    repos: Vec<RepoPath>,
    dir: tempfile::TempDir,
}

fn env() -> Env {
    env_with(|_, _| {})
}

/// The test environment, with `ports` adjusted before the app is built.
fn env_with(adjust: impl FnOnce(&mut GitPorts, &Arc<FakeGit>)) -> Env {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let git = Arc::new(FakeGit::new());
    let store = Arc::new(InMemoryStore::new());
    let watcher = Arc::new(FakeChangeSource::default());
    let projects = Arc::new(FakeProjectSource::default());
    let releases = Arc::new(FakeReleaseFeed::default());
    let repos: Vec<RepoPath> = (0..3)
        .map(|i| {
            let p = dir.path().join(format!("r{i}"));
            std::fs::create_dir_all(&p).unwrap();
            let path = RepoPath::new(p);
            git.with_repo(&path, |r| {
                r.summary.head = HeadState::Branch("main".into());
                r.refs.insert("refs/heads/main".into(), oid(i));
            });
            path
        })
        .collect();
    let mut ports = GitPorts::from_backend(git.clone());
    adjust(&mut ports, &git);
    let services = Services {
        git: ports,
        projects: projects.clone(),
        discovery: Arc::new(FakeDiscovery::default()),
        registry: store.clone(),
        summaries: store.clone(),
        ui_state: store.clone(),
        journal: store.clone(),
        watcher: watcher.clone(),
        diffs: git.clone(),
        churn: git.clone(),
        stats: store.clone(),
        caches: Arc::new(archipel_cache::CacheRegistry::new()),
        cache_dir: dir.path().join("cache"),
        forge: archipel_app::forge::ForgePorts::disabled(),
        updates: Some(releases.clone()),
    };
    let app = App::new(
        services,
        Settings::default(),
        Extensions::default(),
        rt.handle().clone(),
    );
    let def = ProjectDef {
        name: "Product".into(),
        repos: repos
            .iter()
            .map(|p| RepoEntry {
                path: p.clone(),
                alias: None,
                category: None,
            })
            .collect(),
        settings: archipel_model::project::ProjectSettings::default(),
    };
    app.create_project(ProjectLocation::SharedFile(P1_FILE.into()), &def)
        .unwrap();
    assert!(
        projects
            .load(&ProjectLocation::SharedFile(P1_FILE.into()))
            .is_ok()
    );
    Env {
        rt,
        git,
        store,
        watcher,
        app,
        releases,
        repos,
        dir,
    }
}

fn wait_until(app: &App, what: &str, cond: impl Fn(&App) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !cond(app) {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Waits until the 3 repos are watched (watches are registered in the
/// background).
fn wait_watched(e: &Env) {
    wait_until(&e.app, "watching", |_| e.watcher.watch_count() == 3);
}

fn all_fresh(app: &App) -> bool {
    let rows = app.rows();
    !rows.is_empty() && rows.iter().all(|r| !r.stale)
}

#[test]
fn opening_paints_from_cache_then_refreshes() {
    let e = env();
    let id = p1_id();
    // A cached summary from a previous session.
    let mut cached = RepoSummary::empty("main");
    cached.head = HeadState::Branch("cached-branch".into());
    e.store
        .save_summaries(&id, &[(e.repos[0].id(), cached)])
        .unwrap();
    // Slow down nothing: the fake answers instantly, but the first paint
    // must already contain the cached row before any refresh.
    let mut events = e.app.subscribe();
    let started = Instant::now();
    e.app.open_project(&id).unwrap();
    assert!(
        started.elapsed() < Duration::from_millis(200),
        "switch must paint in < 200 ms"
    );
    assert!(matches!(
        e.rt.block_on(events.recv()).unwrap(),
        AppEvent::ProjectOpened(_)
    ));
    wait_until(&e.app, "refresh", all_fresh);
    let rows = e.app.rows();
    assert_eq!(rows.len(), 3);
    assert_eq!(
        rows[0].summary().unwrap().head,
        HeadState::Branch("main".into())
    );
    wait_watched(&e);
    // Summaries were cached for the next session.
    wait_until(&e.app, "persisted", |_| {
        e.store.load_summaries(&id).unwrap().len() == 3
    });
    assert!(e.app.projects().unwrap()[0].last_opened.is_some());
}

#[test]
fn watcher_invalidation_refreshes_one_repo() {
    let e = env();
    e.app.open_project(&p1_id()).unwrap();
    wait_until(&e.app, "refresh", all_fresh);
    wait_watched(&e);
    e.git
        .with_repo(&e.repos[1], |r| r.summary.changes.modified = 4);
    e.watcher.trigger(e.repos[1].id(), Scope::STATUS);
    wait_until(&e.app, "invalidation", |app| {
        app.rows()[1]
            .summary()
            .is_some_and(|s| s.changes.modified == 4)
    });
}

/// A status reader whose next summary read, once armed, reads the repo
/// then waits to be released before answering.
struct HeldSummary {
    inner: Arc<FakeGit>,
    armed: Mutex<Option<(mpsc::Sender<()>, mpsc::Receiver<()>)>>,
}

impl StatusReader for HeldSummary {
    fn summary(&self, repo: &RepoPath) -> Result<RepoSummary, GitError> {
        let summary = self.inner.summary(repo);
        let held = self.armed.lock().unwrap().take();
        if let Some((entered, release)) = held {
            entered.send(()).unwrap();
            release.recv().unwrap();
        }
        summary
    }
    fn changes(&self, repo: &RepoPath) -> Result<Vec<FileChange>, GitError> {
        self.inner.changes(repo)
    }
}

/// The regression: a change made while a refresh was reading (a file saved
/// right after the watcher saw its temporary copy) was lost, as the refresh
/// asked for it was dropped in favor of the pending one.
#[test]
fn a_refresh_asked_while_one_is_reading_runs_after_it() {
    let held = Arc::new(Mutex::new(None::<Arc<HeldSummary>>));
    let slot = held.clone();
    let e = env_with(move |ports, git| {
        let reader = Arc::new(HeldSummary {
            inner: git.clone(),
            armed: Mutex::new(None),
        });
        *slot.lock().unwrap() = Some(reader.clone());
        ports.status = reader;
    });
    let reader = held.lock().unwrap().clone().unwrap();
    e.app.open_project(&p1_id()).unwrap();
    wait_until(&e.app, "refresh", all_fresh);
    wait_watched(&e);
    let (entered_tx, entered) = mpsc::channel();
    let (release, release_rx) = mpsc::channel();
    *reader.armed.lock().unwrap() = Some((entered_tx, release_rx));
    e.watcher.trigger(e.repos[1].id(), Scope::STATUS);
    entered.recv_timeout(Duration::from_secs(5)).unwrap();
    // The read in progress already saw the repo: this change is newer.
    e.git
        .with_repo(&e.repos[1], |r| r.summary.changes.modified = 4);
    e.watcher.trigger(e.repos[1].id(), Scope::STATUS);
    release.send(()).unwrap();
    wait_until(&e.app, "the second refresh", |app| {
        app.rows()[1]
            .summary()
            .is_some_and(|s| s.changes.modified == 4)
    });
}

#[test]
fn filters_sorting_tabs_and_missing_repos() {
    let e = env();
    e.git
        .with_repo(&e.repos[2], |r| r.summary.changes.untracked = 1);
    std::fs::remove_dir_all(&e.repos[0]).unwrap();
    e.app.open_project(&p1_id()).unwrap();
    wait_until(&e.app, "refresh", all_fresh);
    assert_eq!(
        e.app.rows()[0].status,
        archipel_app::dashboard::RepoStatus::Missing
    );
    e.app
        .dispatch(Command::ToggleFilter("dirty".into()))
        .unwrap();
    let visible = e.app.visible_rows();
    assert_eq!(visible.len(), 1);
    assert_eq!(visible[0].path, e.repos[2]);
    e.app
        .dispatch(Command::ToggleFilter("dirty".into()))
        .unwrap();
    e.app
        .dispatch(Command::ToggleFilter("problems".into()))
        .unwrap();
    assert_eq!(e.app.visible_rows()[0].path, e.repos[0]);
    e.app
        .dispatch(Command::ToggleFilter("problems".into()))
        .unwrap();
    e.app.dispatch(Command::SortBy("repo".into())).unwrap();
    e.app.dispatch(Command::SortBy("repo".into())).unwrap();
    assert_eq!(e.app.visible_rows()[0].name, "r2");

    e.app
        .dispatch(Command::OpenRepoTab(e.repos[1].id()))
        .unwrap();
    e.app
        .dispatch(Command::OpenRepoTab(e.repos[1].id()))
        .unwrap();
    let ui = e.app.ui_state().unwrap();
    assert_eq!(ui.tabs.len(), 2);
    assert_eq!(ui.active_tab, 1);
    assert_eq!(e.app.action_context().active_repo, Some(e.repos[1].id()));
    e.app.dispatch(Command::CycleTab(true)).unwrap();
    assert_eq!(e.app.ui_state().unwrap().active_tab, 0);
    e.app.dispatch(Command::CloseTab(0)).unwrap(); // the dashboard cannot be closed
    e.app.dispatch(Command::CloseTab(1)).unwrap();
    assert_eq!(e.app.ui_state().unwrap().tabs.len(), 1);

    // UI state survives a project switch.
    e.app
        .dispatch(Command::ToggleFilter("ahead".into()))
        .unwrap();
    e.app.close_project();
    e.app.open_project(&p1_id()).unwrap();
    assert_eq!(e.app.ui_state().unwrap().filters, ["ahead"]);

    // …and is saved as soon as a command changes it, without a close (the
    // app may be killed).
    e.app
        .dispatch(Command::ToggleFilter("dirty".into()))
        .unwrap();
    let saved = e.store.load_ui_state(&p1_id()).unwrap().unwrap();
    assert_eq!(saved.filters, ["ahead", "dirty"]);
    drop(e.dir);
}

#[test]
fn closing_several_tabs() {
    let e = env();
    e.app.open_project(&p1_id()).unwrap();
    let open_all = || {
        for r in &e.repos {
            e.app.dispatch(Command::OpenRepoTab(r.id())).unwrap();
        }
    };
    let repo_tabs = || {
        let ui = e.app.ui_state().unwrap();
        let ids: Vec<_> = ui
            .tabs
            .iter()
            .filter_map(|t| match t {
                TabState::Repo(id) => Some(*id),
                TabState::Dashboard => None,
            })
            .collect();
        (ids, ui.active_tab)
    };
    let id = |i: usize| e.repos[i].id();

    // Tabs: dashboard, r0, r1, r2 (active).
    open_all();
    e.app.dispatch(Command::CloseOtherTabs(2)).unwrap();
    assert_eq!(repo_tabs(), (vec![id(1)], 1));

    open_all();
    e.app.dispatch(Command::CloseTabsToRight(1)).unwrap();
    // The active tab (r2) closed: the clicked tab becomes active.
    assert_eq!(repo_tabs(), (vec![id(1)], 1));

    open_all();
    e.app.dispatch(Command::CycleTab(true)).unwrap(); // the dashboard
    e.app.dispatch(Command::CloseTabsToRight(2)).unwrap();
    assert_eq!(repo_tabs(), (vec![id(1), id(0)], 0));

    e.app.dispatch(Command::CloseOtherTabs(9)).unwrap(); // out of range: no-op
    assert_eq!(repo_tabs().0.len(), 2);
    e.app.dispatch(Command::CloseAllTabs).unwrap();
    assert_eq!(repo_tabs(), (vec![], 0));
    drop(e.dir);
}

#[test]
fn moving_tabs() {
    let e = env();
    e.app.open_project(&p1_id()).unwrap();
    for r in &e.repos {
        e.app.dispatch(Command::OpenRepoTab(r.id())).unwrap();
    }
    let tabs = || {
        let ui = e.app.ui_state().unwrap();
        (ui.tabs, ui.active_tab)
    };
    let repo = |i: usize| TabState::Repo(e.repos[i].id());

    // Tabs: dashboard, r0, r1, r2 (active). The active tab follows its tab.
    e.app.dispatch(Command::MoveTab { from: 3, to: 1 }).unwrap();
    assert_eq!(
        tabs(),
        (vec![TabState::Dashboard, repo(2), repo(0), repo(1)], 1)
    );
    e.app.dispatch(Command::MoveTab { from: 2, to: 3 }).unwrap();
    assert_eq!(
        tabs(),
        (vec![TabState::Dashboard, repo(2), repo(1), repo(0)], 1)
    );
    // The dashboard stays first: it cannot move nor be displaced.
    e.app.dispatch(Command::MoveTab { from: 0, to: 2 }).unwrap();
    e.app.dispatch(Command::MoveTab { from: 3, to: 0 }).unwrap();
    assert_eq!(
        tabs(),
        (vec![TabState::Dashboard, repo(0), repo(2), repo(1)], 2)
    );
    e.app.dispatch(Command::MoveTab { from: 9, to: 1 }).unwrap(); // out of range: no-op
    assert_eq!(tabs().0.len(), 4);
    drop(e.dir);
}

#[test]
fn batch_fetch_reports_and_refreshes() {
    let e = env();
    e.app.open_project(&p1_id()).unwrap();
    wait_until(&e.app, "refresh", all_fresh);
    let mut events = e.app.subscribe();
    e.app.dispatch(Command::Fetch(vec![])).unwrap();
    let finished = e.rt.block_on(async {
        loop {
            if let AppEvent::BatchFinished(label, d, f, s) = events.recv().await.unwrap() {
                return (label, d, f, s);
            }
        }
    });
    assert_eq!(finished, ("Fetch".into(), 3, 0, 0));
    assert_eq!(e.git.calls().iter().filter(|c| c.op == "fetch").count(), 3);
}

#[test]
fn background_fetch_starts_with_the_selected_repo() {
    let e = env();
    e.app.open_project(&p1_id()).unwrap();
    wait_until(&e.app, "refresh", all_fresh);
    e.app
        .dispatch(Command::Select(Some(e.repos[2].id())))
        .unwrap();
    let mut events = e.app.subscribe();
    e.app.start_auto_fetch();
    wait_until(&e.app, "background fetch", |app| {
        app.rows()[2]
            .summary()
            .is_some_and(|s| s.last_fetch.is_some())
    });
    let fetches: Vec<_> = e
        .git
        .calls()
        .into_iter()
        .filter(|c| c.op == "fetch")
        .collect();
    assert_eq!(fetches.len(), 1, "one repo at a time");
    assert_eq!(fetches[0].repo, e.repos[2].as_path().display().to_string());
    // The fetch itself stays out of the operations drawer.
    while let Ok(ev) = events.try_recv() {
        if let AppEvent::Op(op) = ev {
            assert!(!op.label.starts_with("Auto fetch"), "{op:?}");
        }
    }
}

#[test]
fn fetch_head_alone_only_updates_the_last_fetch_time() {
    let e = env();
    e.app.open_project(&p1_id()).unwrap();
    wait_until(&e.app, "refresh", all_fresh);
    wait_watched(&e);
    e.git
        .with_repo(&e.repos[1], |r| r.summary.changes.modified = 4);
    e.watcher.trigger(e.repos[1].id(), Scope::FETCH);
    wait_until(&e.app, "last fetch", |app| {
        app.rows()[1]
            .summary()
            .is_some_and(|s| s.last_fetch.is_some())
    });
    // No summary recomputed.
    assert_eq!(e.app.rows()[1].summary().unwrap().changes.modified, 0);
}

#[test]
fn undo_through_the_command_bus() {
    let e = env();
    e.app.open_project(&p1_id()).unwrap();
    wait_until(&e.app, "refresh", all_fresh);
    let repo = &e.repos[0];
    e.app
        .repo_actions()
        .commit(
            repo,
            &CommitRequest {
                message: "x".into(),
                mode: CommitMode::New,
            },
            None,
        )
        .unwrap();
    assert_eq!(
        e.store.recent(repo.id(), 1).unwrap()[0].kind,
        OperationKind::Commit
    );
    let mut events = e.app.subscribe();
    e.app.dispatch(Command::Undo(repo.id())).unwrap();
    let notice = e.rt.block_on(async {
        loop {
            if let AppEvent::Notice(n) = events.recv().await.unwrap() {
                return n;
            }
        }
    });
    assert!(notice.starts_with("Undo: Commit"), "{notice}");
    assert!(e.git.calls().iter().any(|c| c.op == "reset"));
}

#[test]
fn repo_jobs_run_through_the_scheduler() {
    let e = env();
    e.app.open_project(&p1_id()).unwrap();
    let mut events = e.app.subscribe();
    let handle = e.app.submit_repo_job(
        &e.repos[0],
        archipel_ops::OpClass::Network,
        "Fetch",
        |_, path, op| {
            assert!(!op.cancel().is_cancelled());
            Ok(path.clone())
        },
    );
    assert_eq!(handle.wait_blocking().unwrap(), e.repos[0]);
    // The operations drawer saw it end.
    let finished = e.rt.block_on(async {
        loop {
            if let AppEvent::Op(op) = events.recv().await.unwrap()
                && op.label == "Fetch"
                && let archipel_ops::OpEventKind::Finished(result) = op.kind
            {
                return (op.repo, result);
            }
        }
    });
    assert_eq!(finished, (Some(e.repos[0].id()), Ok(())));
}

#[test]
fn custom_commands_run_against_the_app() {
    let e = env();
    e.app.open_project(&p1_id()).unwrap();
    let seen = Arc::new(std::sync::Mutex::new(0));
    let s = seen.clone();
    let command = archipel_app::CustomCommand::new("count rows", move |app| {
        *s.lock().unwrap() = app.rows().len();
        Ok(())
    });
    e.app.dispatch(Command::Custom(command)).unwrap();
    assert_eq!(*seen.lock().unwrap(), 3);
}

#[test]
fn refused_undo_fails_the_operation() {
    let e = env();
    e.app.open_project(&p1_id()).unwrap();
    wait_until(&e.app, "refresh", all_fresh);
    let repo = &e.repos[0];
    e.app
        .repo_actions()
        .commit(
            repo,
            &CommitRequest {
                message: "x".into(),
                mode: CommitMode::New,
            },
            None,
        )
        .unwrap();
    // The branch moves after the commit: undoing it would lose work.
    e.git.with_repo(repo, |r| {
        r.refs.insert("refs/heads/main".into(), oid(99));
    });
    let mut events = e.app.subscribe();
    e.app.dispatch(Command::Undo(repo.id())).unwrap();
    let finished = e.rt.block_on(async {
        loop {
            // Refreshes spawned by the commit and the undo finish too.
            if let AppEvent::Op(op) = events.recv().await.unwrap()
                && op.label == "Undo"
                && let archipel_ops::OpEventKind::Finished(result) = op.kind
            {
                return result;
            }
        }
    });
    assert!(finished.is_err(), "{finished:?}");
}

#[test]
fn closing_unloads_watchers_and_keeps_summaries() {
    let e = env();
    let id = p1_id();
    e.app.open_project(&id).unwrap();
    wait_until(&e.app, "refresh", all_fresh);
    e.app.close_project();
    assert!(e.app.rows().is_empty());
    assert_eq!(e.store.load_summaries(&id).unwrap().len(), 3);
}

fn release(version: &str) -> archipel_model::forge::Release {
    archipel_model::forge::Release {
        version: version.into(),
        url: format!("https://example.com/{version}"),
    }
}

#[test]
fn update_check_reports_a_newer_release_to_every_window() {
    let e = env();
    let sibling = e.app.sibling();
    let mut events = sibling.subscribe();
    e.releases.publish(release("999.0.0"));
    e.app.start_update_check(Duration::ZERO);
    // One check loop per process, whichever window starts it.
    sibling.start_update_check(Duration::ZERO);
    wait_until(&sibling, "the update", |a| a.available_update().is_some());
    assert_eq!(e.app.available_update(), Some(release("999.0.0")));
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match events.try_recv() {
            Ok(AppEvent::UpdateAvailable(r)) => {
                assert_eq!(r, release("999.0.0"));
                break;
            }
            Ok(_) => {}
            Err(_) => {
                assert!(Instant::now() < deadline, "no UpdateAvailable event");
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }
}

#[test]
fn update_check_ignores_older_releases() {
    let e = env();
    e.releases.publish(release("0.0.1"));
    e.app.start_update_check(Duration::ZERO);
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(e.app.available_update(), None);
}

fn forecast_calls(e: &Env) -> usize {
    e.git.calls().iter().filter(|c| c.op == "forecast").count()
}

#[test]
fn conflict_forecast_checks_upstream_and_default_branch_once_per_commits() {
    use archipel_app::forecast::ForecastBase;
    use archipel_model::Timestamp;
    use archipel_model::Upstream;
    use archipel_model::forecast::MergeStrategy;
    use archipel_model::refs::BranchInfo;

    let e = env();
    let upstream = |behind| Upstream {
        name: "origin/main".into(),
        ahead: 1,
        behind,
        gone: false,
    };
    // r0: on main, diverged from origin/main; pull rebases.
    e.git.with_repo(&e.repos[0], |r| {
        r.summary.upstream = Some(upstream(2));
        r.refs.insert("origin/main".into(), oid(50));
        r.pull_strategy = MergeStrategy::Rebase;
        r.conflicts.insert((oid(0), oid(50)), vec!["a.rs".into()]);
        r.incoming
            .insert((oid(0), oid(50)), vec!["a.rs".into(), "b.rs".into()]);
    });
    // r1: on a feature branch without upstream: checked against the
    // remote's default branch.
    e.git.with_repo(&e.repos[1], |r| {
        r.summary.head = HeadState::Branch("feat".into());
        r.refs.insert("origin/main".into(), oid(60));
        r.branches.push(BranchInfo {
            name: "origin/main".into(),
            is_remote: true,
            target: oid(60),
            is_head: false,
            upstream: None,
            tip_time: Timestamp(0),
        });
        r.conflicts.insert((oid(1), oid(60)), vec!["x.rs".into()]);
    });
    e.app.open_project(&p1_id()).unwrap();
    wait_until(&e.app, "forecasts", |app| {
        let rows = app.rows();
        rows[0].forecast.will_conflict() && rows[1].forecast.will_conflict()
    });
    let rows = e.app.rows();
    let up = rows[0].forecast.upstream.as_ref().unwrap();
    assert_eq!(up.against, "origin/main");
    assert_eq!(up.strategy, MergeStrategy::Rebase);
    assert_eq!(up.conflicts, ["a.rs"]);
    assert_eq!(up.incoming, ["a.rs", "b.rs"]);
    // On main: no separate check against the default branch.
    assert_eq!(rows[0].forecast.target, None);
    let target = rows[1].forecast.target.as_ref().unwrap();
    assert_eq!(target.base, ForecastBase::DefaultBranch);
    assert_eq!(target.against, "origin/main");
    assert_eq!(target.strategy, MergeStrategy::Merge);
    assert!(!rows[2].forecast.will_conflict());
    let filters = &e.app.extensions().filters;
    let shown =
        archipel_app::dashboard::visible_rows(&rows, filters, &["will-conflict".into()], None);
    assert_eq!(shown.len(), 2);
    let calls = forecast_calls(&e);
    assert_eq!(calls, 2);

    // A working-copy change does not recompute anything.
    e.git
        .with_repo(&e.repos[0], |r| r.summary.changes.modified = 3);
    e.watcher.trigger(e.repos[0].id(), Scope::STATUS);
    wait_until(&e.app, "refresh", |app| {
        app.rows()[0]
            .summary()
            .is_some_and(|s| s.changes.modified == 3)
    });
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(forecast_calls(&e), calls);

    // A fetch moving the upstream does: now clean.
    e.git.with_repo(&e.repos[0], |r| {
        r.refs.insert("origin/main".into(), oid(51));
        r.summary.upstream = Some(upstream(3));
        r.summary.last_fetch = Some(Timestamp(1));
    });
    e.watcher.trigger(e.repos[0].id(), Scope::REFS);
    wait_until(&e.app, "new forecast", |app| {
        app.rows()[0]
            .forecast
            .upstream
            .as_ref()
            .is_some_and(|f| f.theirs == oid(51))
    });
    assert!(!e.app.rows()[0].forecast.will_conflict());
    assert_eq!(forecast_calls(&e), calls + 1);
}
