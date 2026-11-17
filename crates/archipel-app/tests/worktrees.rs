//! Worktrees in the app (spec §10) against fakes: discovered and nested
//! under their repo, followed live, created and removed, Worktree Sets.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use archipel_app::dashboard::DashboardRow;
use archipel_app::{App, AppEvent, Command, Extensions, NewSetRequest, Services, SetMemberPlan};
use archipel_model::journal::HeadRef;
use archipel_model::ports::{ProjectSource, Scope};
use archipel_model::project::{ProjectDef, ProjectLocation, RepoEntry};
use archipel_model::settings::Settings;
use archipel_model::testing::{
    FakeChangeSource, FakeDiscovery, FakeGit, FakeProjectSource, FakeReleaseFeed, InMemoryStore,
    oid,
};
use archipel_model::ui_state::TabState;
use archipel_model::worktree::{SetView, WorktreeAddRequest, WorktreeInfo, WorktreeTarget};
use archipel_model::{HeadState, ProjectId, RepoId, RepoPath};
use archipel_ops::GitPorts;

const FILE: &str = "/p/archipel.toml";

struct Env {
    rt: tokio::runtime::Runtime,
    git: Arc<FakeGit>,
    watcher: Arc<FakeChangeSource>,
    app: Arc<App>,
    repos: Vec<RepoPath>,
    dir: tempfile::TempDir,
}

fn linked(path: &RepoPath, branch: &str) -> WorktreeInfo {
    WorktreeInfo {
        path: path.clone(),
        head: Some(HeadRef::Symbolic(format!("refs/heads/{branch}"))),
        head_oid: Some(oid(9)),
        is_main: false,
        is_bare: false,
        locked: None,
        prunable: None,
    }
}

/// Two repos; `r0` has two linked worktrees (one locked), `r1` none.
fn env() -> Env {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let git = Arc::new(FakeGit::new());
    let mk = |name: &str| {
        let p = dir.path().join(name);
        std::fs::create_dir_all(&p).unwrap();
        RepoPath::new(p)
    };
    let repos = vec![mk("r0"), mk("r1")];
    let wts = [mk("r0.worktrees/feat"), mk("r0.worktrees/fix")];
    for (i, r) in repos.iter().enumerate() {
        git.with_repo(r, |s| {
            s.summary.head = HeadState::Branch("main".into());
            s.refs.insert("refs/heads/main".into(), oid(i as u64));
        });
    }
    for (w, b) in wts.iter().zip(["feat", "fix"]) {
        git.with_repo(w, |s| s.summary.head = HeadState::Branch(b.into()));
    }
    git.with_repo(&repos[0], |s| {
        s.worktrees = vec![linked(&wts[0], "feat"), linked(&wts[1], "fix")];
        s.worktrees[1].locked = Some("usb".into());
    });
    let store = Arc::new(InMemoryStore::new());
    let watcher = Arc::new(FakeChangeSource::default());
    let projects = Arc::new(FakeProjectSource::default());
    let services = Services {
        git: GitPorts::from_backend(git.clone()),
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
        updates: Some(Arc::new(FakeReleaseFeed::default())),
    };
    let app = App::new(
        services,
        Settings::default(),
        Extensions::default(),
        rt.handle().clone(),
    );
    let def = ProjectDef {
        name: "P".into(),
        repos: repos
            .iter()
            .map(|p| RepoEntry {
                path: p.clone(),
                alias: None,
                category: None,
                copy_to_worktrees: vec![".env".into()],
            })
            .collect(),
        settings: archipel_model::project::ProjectSettings::default(),
    };
    let location = ProjectLocation::SharedFile(FILE.into());
    app.create_project(location.clone(), &def).unwrap();
    assert!(projects.load(&location).is_ok());
    app.open_project(&archipel_app::file_project_id(std::path::Path::new(FILE)))
        .unwrap();
    Env {
        rt,
        git,
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

fn labels(app: &App) -> Vec<String> {
    app.visible_rows().iter().map(DashboardRow::label).collect()
}

fn loaded(e: &Env) {
    wait_until(&e.app, "worktrees", |app| app.all_rows().len() == 4);
    wait_until(&e.app, "fresh", |app| {
        app.all_rows().iter().all(|r| !r.stale)
    });
}

#[test]
fn worktrees_are_nested_under_their_repo_and_collapsed_by_default() {
    let e = env();
    loaded(&e);
    assert_eq!(labels(&e.app), ["r0", "r1"]);
    let r0 = e.app.visible_rows()[0].clone();
    assert_eq!(r0.linked_worktrees, 2);
    // Not in the sidebar groups nor the dependency tree: under their repo.
    let grouped: usize = e.app.sidebar_groups().iter().map(|g| g.rows.len()).sum();
    assert_eq!(grouped, 2);
    e.app.dispatch(Command::ToggleWorktrees(r0.id)).unwrap();
    assert_eq!(labels(&e.app), ["r0", "r0 · feat", "r0 · fix", "r1"]);
    let fix = &e.app.visible_rows()[2];
    assert_eq!(
        fix.worktree.as_ref().unwrap().locked.as_deref(),
        Some("usb")
    );
    // Never written to the project file.
    let (_, def) = e.app.project().unwrap();
    assert_eq!(def.repos.len(), 2);
    // Switcher: every worktree of the project.
    let all: Vec<String> = e
        .app
        .project_worktrees()
        .iter()
        .map(|w| format!("{} · {}", w.name, w.label))
        .collect();
    assert_eq!(all, ["r0 · main", "r0 · feat", "r0 · fix", "r1 · main"]);
}

#[test]
fn collapsed_worktrees_are_not_watched_until_shown() {
    let e = env();
    loaded(&e);
    let r0 = e.repos[0].id();
    let feat = e
        .app
        .all_rows()
        .into_iter()
        .find(|r| r.label() == "r0 · feat")
        .unwrap();
    wait_until(&e.app, "watching repos", |_| e.watcher.watch_count() == 2);
    assert!(!e.watcher.watched().contains(&feat.id));
    e.app.dispatch(Command::ToggleWorktrees(r0)).unwrap();
    wait_until(&e.app, "watching worktrees", |_| {
        e.watcher.watch_count() == 4
    });
    assert!(e.watcher.watched().contains(&feat.id));
}

#[test]
fn worktrees_created_from_a_terminal_show_up_live() {
    let e = env();
    loaded(&e);
    let new = RepoPath::new(e.dir.path().join("r1.worktrees/agent"));
    std::fs::create_dir_all(new.as_path()).unwrap();
    e.git
        .with_repo(&new, |s| s.summary.head = HeadState::Branch("agent".into()));
    e.git
        .with_repo(&e.repos[1], |s| s.worktrees.push(linked(&new, "agent")));
    e.watcher.trigger(e.repos[1].id(), Scope::WORKTREES);
    wait_until(&e.app, "new row", |app| {
        app.all_rows().iter().any(|r| r.id == new.id())
    });
    // A new worktree expands its repo (worktree_auto_expand).
    wait_until(&e.app, "expanded", |app| {
        labels(app).contains(&"r1 · agent".to_owned())
    });
    e.git.with_repo(&e.repos[1], |s| s.worktrees.clear());
    e.watcher.trigger(e.repos[1].id(), Scope::WORKTREES);
    wait_until(&e.app, "row gone", |app| {
        !app.all_rows().iter().any(|r| r.id == new.id())
    });
}

#[test]
fn fetch_runs_once_per_repo_and_all_means_repos() {
    let e = env();
    loaded(&e);
    let feat = e
        .app
        .all_rows()
        .into_iter()
        .find(DashboardRow::is_worktree)
        .unwrap();
    e.app.dispatch(Command::Fetch(vec![feat.id])).unwrap();
    wait_until(&e.app, "fetched", |_| {
        e.git.calls().iter().any(|c| c.op == "fetch")
    });
    std::thread::sleep(Duration::from_millis(100));
    let fetched: Vec<String> = e
        .git
        .calls()
        .into_iter()
        .filter(|c| c.op == "fetch")
        .map(|c| c.repo)
        .collect();
    assert_eq!(fetched, [e.repos[0].as_path().display().to_string()]);
}

#[test]
fn a_remote_url_set_from_a_worktree_row_changes_its_repo() {
    let e = env();
    loaded(&e);
    let feat = e
        .app
        .all_rows()
        .into_iter()
        .find(DashboardRow::is_worktree)
        .unwrap();
    assert!(e.app.remotes(feat.id).unwrap().is_empty());
    e.app
        .dispatch(Command::SetRemoteUrl {
            repo: feat.id,
            remote: "origin".into(),
            url: " https://example.com/r0.git ".into(),
        })
        .unwrap();
    wait_until(&e.app, "set", |_| {
        e.git.calls().iter().any(|c| c.op == "set_remote_url")
    });
    let call = e
        .git
        .calls()
        .into_iter()
        .find(|c| c.op == "set_remote_url")
        .unwrap();
    // The shared configuration: the main repo's, the URL trimmed.
    assert_eq!(call.repo, e.repos[0].as_path().display().to_string());
    assert_eq!(call.detail, "origin->https://example.com/r0.git");
    let remotes = e.app.remotes(feat.id).unwrap();
    assert_eq!(remotes.len(), 1);
    assert_eq!(remotes[0].url, "https://example.com/r0.git");
}

#[test]
fn all_means_every_shown_repo_and_hidden_repos_are_never_targets() {
    let e = env();
    loaded(&e);
    let (r0, r1) = (e.repos[0].id(), e.repos[1].id());
    let feat = e
        .app
        .all_rows()
        .into_iter()
        .find(DashboardRow::is_worktree)
        .unwrap()
        .id;
    // All: the repos, not their worktrees; a worktree picked is itself.
    assert_eq!(e.app.resolve_targets(&[]), [r0, r1]);
    assert_eq!(e.app.resolve_targets(&[feat]), [feat]);
    e.app.dispatch(Command::HideRepos(vec![r1])).unwrap();
    assert_eq!(e.app.resolve_targets(&[]), [r0]);
    assert!(e.app.resolve_targets(&[r1]).is_empty());
}

#[test]
fn create_open_and_remove_a_worktree() {
    let e = env();
    loaded(&e);
    let mut events = e.app.subscribe();
    let path: PathBuf = e
        .app
        .default_worktree_folder(e.repos[1].id(), "feat/new")
        .unwrap();
    assert!(path.ends_with("r1.worktrees/feat-new") || path.ends_with("r1.worktrees\\feat-new"));
    e.app
        .dispatch(Command::AddWorktree {
            repo: e.repos[1].id(),
            request: WorktreeAddRequest {
                path: path.clone(),
                target: WorktreeTarget::NewBranch {
                    name: "feat/new".into(),
                    base: None,
                },
            },
            init_submodules: false,
            copy_files: true,
            open: true,
        })
        .unwrap();
    let id = RepoPath::new(path.clone()).id();
    wait_until(&e.app, "opened", |app| {
        app.ui_state()
            .is_some_and(|ui| ui.tabs.contains(&TabState::Repo(id)))
    });
    // The local files were asked to be copied.
    assert!(
        e.git
            .calls()
            .iter()
            .any(|c| c.op == "copy_local_files" && c.detail == ".env")
    );
    let row = e.app.all_rows().into_iter().find(|r| r.id == id).unwrap();
    assert_eq!(row.label(), "r1 · feat/new");
    while let Ok(ev) = events.try_recv() {
        drop(ev);
    }
    e.app
        .dispatch(Command::RemoveWorktree {
            worktree: id,
            force: false,
            delete_branch: false,
        })
        .unwrap();
    wait_until(&e.app, "removed", |app| {
        !app.all_rows().iter().any(|r| r.id == id)
    });
    // Its tab closed with it.
    assert!(!e.app.ui_state().unwrap().tabs.contains(&TabState::Repo(id)));
    // A locked worktree is refused.
    let fix = e
        .app
        .all_rows()
        .into_iter()
        .find(|r| r.label() == "r0 · fix")
        .unwrap();
    assert!(
        e.app
            .dispatch(Command::RemoveWorktree {
                worktree: fix.id,
                force: false,
                delete_branch: false,
            })
            .is_err()
    );
}

#[test]
fn a_worktree_set_shows_its_members_in_place_of_their_repos() {
    let e = env();
    loaded(&e);
    let mut events = e.app.subscribe();
    let folder = e.app.default_set_folder("agent/one").unwrap();
    assert!(
        folder.ends_with("P.worktrees/agent-one") || folder.ends_with("P.worktrees\\agent-one")
    );
    let folder = e.dir.path().join("sets/agent-one");
    // The fake writes no files: the members' folders exist beforehand.
    for r in ["r0", "r1"] {
        std::fs::create_dir_all(folder.join(r)).unwrap();
    }
    e.app
        .dispatch(Command::CreateWorktreeSet(NewSetRequest {
            name: "agent/one".into(),
            branch: "agent/one".into(),
            folder: folder.clone(),
            members: e
                .repos
                .iter()
                .map(|r| SetMemberPlan {
                    repo: r.id(),
                    base: Some("main".into()),
                })
                .collect(),
            init_submodules: false,
            copy_files: false,
        }))
        .unwrap();
    let report = loop {
        if let AppEvent::WorktreeSetCreated(report) = e.rt.block_on(events.recv()).unwrap() {
            break report;
        }
    };
    assert!(report.failed().is_empty(), "{report:?}");
    let ui = e.app.ui_state().unwrap();
    assert_eq!(ui.set_view, SetView::Set("agent/one".into()));
    assert_eq!(labels(&e.app), ["r0 · agent/one", "r1 · agent/one"]);
    assert!(
        e.app
            .identity()
            .unwrap()
            .window_title()
            .contains("[agent/one]")
    );
    // Batch actions on "all" target the members.
    let shown: Vec<RepoId> = e.app.visible_rows().iter().map(|r| r.id).collect();
    assert_eq!(e.app.resolve_targets(&[]), shown);
    wait_until(&e.app, "members read", |app| {
        app.visible_rows().iter().all(|r| r.summary().is_some())
    });
    e.app
        .dispatch(Command::CreateTagAll("t1".into(), Vec::new()))
        .unwrap();
    wait_until(&e.app, "tagged", |_| {
        e.git
            .calls()
            .iter()
            .filter(|c| c.op == "create_tag")
            .count()
            == 2
    });
    let tagged: Vec<String> = e
        .git
        .calls()
        .into_iter()
        .filter(|c| c.op == "create_tag")
        .map(|c| c.repo)
        .collect();
    assert!(tagged.iter().all(|p| p.contains("agent-one")), "{tagged:?}");
    // Finish: every member removed, the set gone, back to all.
    let members: Vec<RepoId> = e.app.visible_rows().iter().map(|r| r.id).collect();
    e.app
        .dispatch(Command::FinishWorktreeSet {
            name: "agent/one".into(),
            removals: members
                .iter()
                .map(|id| (*id, archipel_ops::RemoveOptions::default()))
                .collect(),
        })
        .unwrap();
    loop {
        if let AppEvent::WorktreeSetFinished(report) = e.rt.block_on(events.recv()).unwrap() {
            assert!(report.failed().is_empty(), "{report:?}");
            break;
        }
    }
    assert!(e.app.worktree_sets().is_empty());
    assert_eq!(e.app.ui_state().unwrap().set_view, SetView::All);
    // The members are gone; r0 stays expanded (a worktree appeared in it).
    assert_eq!(labels(&e.app), ["r0", "r0 · feat", "r0 · fix", "r1"]);
}

#[test]
fn a_project_entry_that_is_a_worktree_of_a_listed_repo_is_folded() {
    let e = env();
    loaded(&e);
    let feat = RepoPath::new(e.dir.path().join("r0.worktrees/feat"));
    e.app
        .dispatch(Command::AddRepos(vec![feat.clone()]))
        .unwrap();
    wait_until(&e.app, "folded", |app| {
        app.all_rows()
            .iter()
            .any(|r| r.id == feat.id() && r.worktree.as_ref().is_some_and(|w| w.listed))
    });
    // Once, under its repo.
    assert_eq!(
        e.app
            .all_rows()
            .iter()
            .filter(|r| r.id == feat.id())
            .count(),
        1
    );
    assert_eq!(labels(&e.app), ["r0", "r1"]);
    let _ = ProjectId("unused".into());
}
