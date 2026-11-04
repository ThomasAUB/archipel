//! App behavior against fakes: project switching paints from cache, refreshes
//! in the background, reacts to watcher invalidations, runs batches and undo.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use archipel_app::{App, AppEvent, Command, Extensions, Services};
use archipel_model::journal::OperationKind;
use archipel_model::ports::{JournalStore, ProjectSource, Scope, SummaryStore};
use archipel_model::project::{ProjectDef, ProjectLocation, RepoEntry};
use archipel_model::requests::{CommitMode, CommitRequest};
use archipel_model::settings::Settings;
use archipel_model::testing::{
    FakeChangeSource, FakeDiscovery, FakeGit, FakeProjectSource, InMemoryStore, oid,
};
use archipel_model::{HeadState, ProjectId, RepoPath, RepoSummary};
use archipel_ops::GitPorts;

struct Env {
    rt: tokio::runtime::Runtime,
    git: Arc<FakeGit>,
    store: Arc<InMemoryStore>,
    watcher: Arc<FakeChangeSource>,
    app: Arc<App>,
    repos: Vec<RepoPath>,
    dir: tempfile::TempDir,
}

fn env() -> Env {
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
    let services = Services {
        git: GitPorts::from_backend(git.clone()),
        projects: projects.clone(),
        discovery: Arc::new(FakeDiscovery::default()),
        registry: store.clone(),
        summaries: store.clone(),
        ui_state: store.clone(),
        journal: store.clone(),
        watcher: watcher.clone(),
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
            })
            .collect(),
        settings: archipel_model::project::ProjectSettings::default(),
    };
    app.create_project(ProjectLocation::Private(ProjectId("p1".into())), &def)
        .unwrap();
    assert!(
        projects
            .load(&ProjectLocation::Private(ProjectId("p1".into())))
            .is_ok()
    );
    Env {
        rt,
        git,
        store,
        watcher,
        app,
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

fn all_fresh(app: &App) -> bool {
    let rows = app.rows();
    !rows.is_empty() && rows.iter().all(|r| !r.stale)
}

#[test]
fn opening_paints_from_cache_then_refreshes() {
    let e = env();
    let id = ProjectId("p1".into());
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
    assert_eq!(e.watcher.watch_count(), 3);
    // Summaries were cached for the next session.
    wait_until(&e.app, "persisted", |_| {
        e.store.load_summaries(&id).unwrap().len() == 3
    });
    assert!(e.app.projects().unwrap()[0].last_opened.is_some());
}

#[test]
fn watcher_invalidation_refreshes_one_repo() {
    let e = env();
    e.app.open_project(&ProjectId("p1".into())).unwrap();
    wait_until(&e.app, "refresh", all_fresh);
    e.git
        .with_repo(&e.repos[1], |r| r.summary.changes.modified = 4);
    e.watcher.trigger(e.repos[1].id(), Scope::STATUS);
    wait_until(&e.app, "invalidation", |app| {
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
    e.app.open_project(&ProjectId("p1".into())).unwrap();
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
    e.app.open_project(&ProjectId("p1".into())).unwrap();
    assert_eq!(e.app.ui_state().unwrap().filters, ["ahead"]);
    drop(e.dir);
}

#[test]
fn batch_fetch_reports_and_refreshes() {
    let e = env();
    e.app.open_project(&ProjectId("p1".into())).unwrap();
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
fn undo_through_the_command_bus() {
    let e = env();
    e.app.open_project(&ProjectId("p1".into())).unwrap();
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
fn closing_unloads_watchers_and_keeps_summaries() {
    let e = env();
    let id = ProjectId("p1".into());
    e.app.open_project(&id).unwrap();
    wait_until(&e.app, "refresh", all_fresh);
    e.app.close_project();
    assert!(e.app.rows().is_empty());
    assert_eq!(e.store.load_summaries(&id).unwrap().len(), 3);
}
