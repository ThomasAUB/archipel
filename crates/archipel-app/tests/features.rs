//! App behavior of the v1 additions against fakes: launch from a directory,
//! hidden repos, sibling sessions (one per window), window identity and the
//! repo dependency tree.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use archipel_app::{App, Command, Extensions, ProjectIcon, Services, file_project_id};
use archipel_model::ports::{ProjectSource, Scope};
use archipel_model::project::{
    DiscoveredKind, DiscoveredRepo, ProjectDef, ProjectLocation, RepoEntry,
};
use archipel_model::settings::Settings;
use archipel_model::submodule::SubmoduleInfo;
use archipel_model::testing::{
    FakeChangeSource, FakeDiscovery, FakeGit, FakeProjectSource, InMemoryStore, oid,
};
use archipel_model::{HeadState, ProjectId, RepoPath};
use archipel_ops::GitPorts;

struct Env {
    _rt: tokio::runtime::Runtime,
    git: Arc<FakeGit>,
    watcher: Arc<FakeChangeSource>,
    app: Arc<App>,
    repos: Vec<RepoPath>,
    dir: tempfile::TempDir,
}

/// Id of the project `env` creates.
fn p1() -> ProjectId {
    file_project_id(Path::new(P1_FILE))
}

/// Where `env` stores its project (in memory: the fake source).
const P1_FILE: &str = "/p1/archipel.toml";

/// Three repos `r0..r2` in a temp dir, a project `p1` listing them, and a
/// discovery that finds them (plus `extra`) when scanning.
fn env(extra: &[&str]) -> Env {
    let e = env_with(extra, Arc::new(FakeProjectSource::default()));
    let def = ProjectDef {
        name: "My product".into(),
        repos: e
            .repos
            .iter()
            .map(|p| RepoEntry {
                path: p.clone(),
                alias: None,
                category: None,
            })
            .collect(),
        settings: archipel_model::project::ProjectSettings::default(),
    };
    e.app
        .create_project(ProjectLocation::SharedFile(P1_FILE.into()), &def)
        .unwrap();
    e
}

/// The repos and discovery of [`env`], with no project, storing project
/// definitions in `projects`.
fn env_with(extra: &[&str], projects: Arc<dyn ProjectSource>) -> Env {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let git = Arc::new(FakeGit::new());
    let store = Arc::new(InMemoryStore::new());
    let watcher = Arc::new(FakeChangeSource::default());
    let mut names: Vec<String> = (0..3).map(|i| format!("r{i}")).collect();
    names.extend(extra.iter().map(|s| (*s).to_owned()));
    let all: Vec<RepoPath> = names
        .iter()
        .enumerate()
        .map(|(i, name)| {
            let p = dir.path().join(name);
            std::fs::create_dir_all(&p).unwrap();
            let path = RepoPath::new(p);
            git.with_repo(&path, |r| {
                r.summary.head = HeadState::Branch("main".into());
                r.refs.insert("refs/heads/main".into(), oid(i as u64));
            });
            path
        })
        .collect();
    let discovery = FakeDiscovery {
        repos: all
            .iter()
            .map(|p| DiscoveredRepo {
                path: p.clone(),
                kind: DiscoveredKind::GitDir,
            })
            .collect(),
    };
    let services = Services {
        git: GitPorts::from_backend(git.clone()),
        projects,
        discovery: Arc::new(discovery),
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
    };
    let app = App::new(
        services,
        Settings::default(),
        Extensions::default(),
        rt.handle().clone(),
    );
    let repos: Vec<RepoPath> = all.into_iter().take(3).collect();
    Env {
        _rt: rt,
        git,
        watcher,
        app,
        repos,
        dir,
    }
}

fn wait_until(what: &str, cond: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn open_fresh(e: &Env) {
    e.app.open_project(&p1()).unwrap();
    wait_until("refresh", || e.app.rows().iter().all(|r| !r.stale));
}

#[test]
fn hidden_repos_leave_dashboard_batches_and_refreshes() {
    let e = env(&[]);
    open_fresh(&e);
    let hidden = e.repos[1].id();
    e.app.dispatch(Command::Select(Some(hidden))).unwrap();
    e.app.dispatch(Command::ToggleHidden(vec![hidden])).unwrap();
    assert_eq!(e.app.rows().len(), 2);
    assert_eq!(e.app.visible_rows().len(), 2);
    assert_eq!(e.app.hidden_count(), 1);
    assert_eq!(e.app.ui_state().unwrap().selected_repo, None);

    // Not refreshed on watcher events.
    e.git
        .with_repo(&e.repos[1], |r| r.summary.changes.modified = 9);
    e.watcher.trigger(hidden, Scope::STATUS);
    std::thread::sleep(Duration::from_millis(50));
    let row = e
        .app
        .all_rows()
        .into_iter()
        .find(|r| r.id == hidden)
        .unwrap();
    assert_ne!(row.summary().unwrap().changes.modified, 9);

    // Not targeted by batch actions ("all" = every non-hidden repo).
    e.app.dispatch(Command::Fetch(Vec::new())).unwrap();
    wait_until("fetch", || {
        e.git.calls().iter().filter(|c| c.op == "fetch").count() == 2
    });
    std::thread::sleep(Duration::from_millis(30));
    let fetched: Vec<String> = e
        .git
        .calls()
        .into_iter()
        .filter(|c| c.op == "fetch")
        .map(|c| c.repo)
        .collect();
    assert_eq!(fetched.len(), 2);
    assert!(!fetched.contains(&e.repos[1].as_path().display().to_string()));

    // "Show hidden" lists it, flagged.
    e.app.dispatch(Command::ToggleShowHidden).unwrap();
    let visible = e.app.visible_rows();
    assert_eq!(visible.len(), 3);
    assert!(visible.iter().any(|r| r.id == hidden && r.hidden));

    // Hiding is remembered in the private state.
    e.app.close_project();
    open_fresh(&e);
    assert_eq!(e.app.rows().len(), 2);

    // Unhide: back everywhere, and refreshed.
    e.app.dispatch(Command::UnhideRepos(Vec::new())).unwrap();
    assert_eq!(e.app.rows().len(), 3);
    wait_until("refresh after unhide", || {
        e.app
            .rows()
            .iter()
            .any(|r| r.id == hidden && r.summary().is_some_and(|s| s.changes.modified == 9))
    });
}

#[test]
fn multi_selection_drives_batch_actions() {
    let e = env(&[]);
    open_fresh(&e);
    let (a, b) = (e.repos[0].id(), e.repos[2].id());
    e.app.dispatch(Command::Select(Some(a))).unwrap();
    e.app.dispatch(Command::ToggleSelect(b)).unwrap();
    assert_eq!(e.app.action_context().selected, vec![a, b]);
    e.app.dispatch(Command::ToggleSelect(a)).unwrap();
    assert_eq!(e.app.action_context().selected, vec![b]);
    e.app.dispatch(Command::Select(Some(a))).unwrap();
    assert_eq!(e.app.action_context().selected, vec![a]);
}

#[test]
fn categories_group_the_sidebar_and_are_saved_in_the_project() {
    let e = env(&[]);
    open_fresh(&e);
    let ids: Vec<_> = e.repos.iter().map(RepoPath::id).collect();
    let groups = e.app.sidebar_groups();
    assert_eq!(groups.len(), 1, "no category: flat list");
    assert_eq!(groups[0].category, None);

    let set = |repos: Vec<_>, c: Option<&str>| {
        e.app
            .dispatch(Command::SetCategory(repos, c.map(str::to_owned)))
            .unwrap();
    };
    set(vec![ids[0], ids[2]], Some(" Libs "));
    set(vec![ids[1]], Some("apps"));
    // Same name but for case: the existing spelling is reused.
    set(vec![ids[2]], Some("libs"));
    let summary = |app: &App| -> Vec<(Option<String>, Vec<String>)> {
        app.sidebar_groups()
            .into_iter()
            .map(|g| (g.category, g.rows.into_iter().map(|r| r.name).collect()))
            .collect()
    };
    assert_eq!(
        summary(&e.app),
        [
            (Some("apps".into()), vec!["r1".to_owned()]),
            (Some("Libs".into()), vec!["r0".to_owned(), "r2".to_owned()]),
        ]
    );
    assert_eq!(e.app.categories(), ["apps", "Libs"]);

    // Stored in the project definition, so it survives a reopen.
    let (_, def) = e.app.project().unwrap();
    assert_eq!(def.repos[0].category.as_deref(), Some("Libs"));
    open_fresh(&e);
    assert_eq!(e.app.categories(), ["apps", "Libs"]);

    // Collapsing is personal UI state.
    e.app
        .dispatch(Command::ToggleCategory("Libs".into()))
        .unwrap();
    let groups = e.app.sidebar_groups();
    assert!(groups[1].collapsed && !groups[0].collapsed);
    assert_eq!(e.app.ui_state().unwrap().collapsed_categories, ["Libs"]);

    // Clearing: back under "Uncategorized", listed last.
    set(vec![ids[1]], None);
    assert_eq!(
        summary(&e.app),
        [
            (Some("Libs".into()), vec!["r0".to_owned(), "r2".to_owned()]),
            (None, vec!["r1".to_owned()]),
        ]
    );
    // Hidden repos are not listed.
    e.app.dispatch(Command::ToggleHidden(vec![ids[1]])).unwrap();
    assert_eq!(e.app.sidebar_groups().len(), 1);
}

#[test]
fn dashboard_filters_on_categories() {
    let e = env(&[]);
    open_fresh(&e);
    let ids: Vec<_> = e.repos.iter().map(RepoPath::id).collect();
    let cmd = |c: Command| e.app.dispatch(c).unwrap();
    let names = |app: &App| -> Vec<String> {
        app.visible_rows().into_iter().map(|r| r.name).collect()
    };
    assert!(e.app.category_filter_keys().is_empty(), "no category: no chip");

    cmd(Command::SetCategory(vec![ids[0]], Some("libs".into())));
    cmd(Command::SetCategory(vec![ids[1]], Some("apps".into())));
    assert_eq!(e.app.category_filter_keys(), ["apps", "libs", ""]);

    cmd(Command::ToggleCategoryFilter("libs".into()));
    assert_eq!(names(&e.app), ["r0"]);
    // Several categories: any of them.
    cmd(Command::ToggleCategoryFilter(String::new()));
    assert_eq!(names(&e.app), ["r0", "r2"]);
    // Combined with the other filters (AND).
    cmd(Command::ToggleFilter("problems".into()));
    assert!(names(&e.app).is_empty());
    cmd(Command::ToggleFilter("problems".into()));

    // The filter follows a rename; a removed category becomes "Uncategorized".
    cmd(Command::RenameCategory("libs".into(), Some("core".into())));
    assert_eq!(e.app.ui_state().unwrap().category_filters, ["", "core"]);
    assert_eq!(names(&e.app), ["r0", "r2"]);
    cmd(Command::RenameCategory("core".into(), None));
    assert_eq!(e.app.ui_state().unwrap().category_filters, [""]);
    assert_eq!(names(&e.app), ["r0", "r2"]);

    // A category that no longer exists does not filter.
    cmd(Command::ToggleCategoryFilter(String::new()));
    cmd(Command::ToggleCategoryFilter("gone".into()));
    assert_eq!(names(&e.app), ["r0", "r1", "r2"]);
}

#[test]
fn categories_are_renamed_merged_and_removed() {
    let e = env(&[]);
    open_fresh(&e);
    let ids: Vec<_> = e.repos.iter().map(RepoPath::id).collect();
    let cmd = |c: Command| e.app.dispatch(c).unwrap();
    let rename = |old: &str, new: Option<&str>| {
        cmd(Command::RenameCategory(old.into(), new.map(str::to_owned)));
    };
    cmd(Command::SetCategory(
        vec![ids[0], ids[1]],
        Some("libs".into()),
    ));
    cmd(Command::SetCategory(vec![ids[2]], Some("apps".into())));
    cmd(Command::ToggleCategory("libs".into()));

    // A change of case only is kept; the collapsed state follows.
    rename("libs", Some("Libs"));
    assert_eq!(e.app.categories(), ["apps", "Libs"]);
    assert_eq!(e.app.ui_state().unwrap().collapsed_categories, ["Libs"]);

    // Onto another existing category (any case): merged into it.
    rename("Libs", Some(" APPS "));
    assert_eq!(e.app.categories(), ["apps"]);
    let groups = e.app.sidebar_groups();
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].rows.len(), 3);
    assert_eq!(e.app.ui_state().unwrap().collapsed_categories, ["apps"]);

    // Removed: every repo uncategorized, still in the project; flat list.
    rename("apps", None);
    assert!(e.app.categories().is_empty());
    let (_, def) = e.app.project().unwrap();
    assert_eq!(def.repos.len(), 3);
    assert!(def.repos.iter().all(|r| r.category.is_none()));
    assert!(e.app.ui_state().unwrap().collapsed_categories.is_empty());
    let groups = e.app.sidebar_groups();
    assert_eq!((groups.len(), groups[0].category.clone()), (1, None));
}

#[test]
fn open_directory_writes_a_project_file_then_reopens_it() {
    let e = env_with(
        &["r3"],
        Arc::new(archipel_project::FileProjectSource::new()),
    );
    let target = e.app.directory_target(e.dir.path()).unwrap();
    assert!(target.project_file.is_none());
    let file = target.scan_root.join("archipel.toml");
    assert_eq!(target.id, file_project_id(&file));

    let id = e.app.open_directory(e.dir.path()).unwrap();
    assert_eq!(id, target.id);
    assert_eq!(
        e.app.project().unwrap().1.name,
        e.dir.path().file_name().unwrap().to_string_lossy()
    );
    wait_until("scan", || e.app.all_rows().len() == 4 && !e.app.scanning());
    // The definition, scan result included, is in the project file.
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(text.contains("\"r3\""), "{text}");

    let r3 = e.dir.path().join("r3");
    let hidden = RepoPath::new(&r3).id();
    e.app.dispatch(Command::HideRepos(vec![hidden])).unwrap();
    e.app.close_project();

    // Same directory again: the file opens, no rescan, the repo stays hidden.
    let again = e.app.open_directory(e.dir.path()).unwrap();
    assert_eq!(again, id);
    assert!(!e.app.scanning());
    assert_eq!(e.app.all_rows().len(), 4);
    assert_eq!(e.app.rows().len(), 3);

    // Launching from inside a repo targets the enclosing repo's project file.
    std::fs::create_dir_all(r3.join(".git")).unwrap();
    std::fs::create_dir_all(r3.join("src")).unwrap();
    let target = e.app.directory_target(&r3.join("src")).unwrap();
    assert_eq!(target.scan_root, r3);
    assert!(target.project_file.is_none());
    assert_eq!(target.id, file_project_id(&r3.join("archipel.toml")));
}

#[test]
fn sibling_sessions_are_independent() {
    let e = env(&[]);
    open_fresh(&e);
    let other = e.app.sibling();
    let dir_id = other.open_directory(e.repos[0].as_path()).unwrap();
    assert_eq!(e.app.project().unwrap().0, p1());
    assert_eq!(other.project().unwrap().0, dir_id);
    e.app
        .dispatch(Command::Select(Some(e.repos[1].id())))
        .unwrap();
    assert_eq!(other.ui_state().unwrap().selected_repo, None);
    // Both see the shared registry.
    assert_eq!(other.projects().unwrap().len(), 2);
}

#[test]
fn identity_is_automatic_then_saved_in_the_project() {
    let e = env(&[]);
    open_fresh(&e);
    let auto = e.app.identity().unwrap();
    assert!(auto.automatic_accent);
    assert_eq!(auto.initials, "MP");
    assert_eq!(auto.window_title(), "My product — Archipel");
    assert_eq!(e.app.identity().unwrap().accent, auto.accent);

    e.app
        .dispatch(Command::SetAccent(Some("ff8800".into())))
        .unwrap();
    e.app
        .dispatch(Command::SetIcon(Some(" 🚀 ".into())))
        .unwrap();
    let mine = e.app.identity().unwrap();
    assert!(!mine.automatic_accent);
    assert_eq!(mine.accent.to_hex(), "#ff8800");
    assert_eq!(mine.window_title(), "🚀 My product — Archipel");
    let settings = e.app.project().unwrap().1.settings;
    assert_eq!(settings.accent.as_deref(), Some("#ff8800"));
    assert_eq!(settings.icon.as_deref(), Some("🚀"));
    assert!(
        e.app
            .dispatch(Command::SetAccent(Some("nope".into())))
            .is_err()
    );
    e.app.dispatch(Command::SetAccent(None)).unwrap();
    assert!(e.app.identity().unwrap().automatic_accent);

    assert_eq!(e.app.identity().unwrap().background_tint, None);
    e.app
        .dispatch(Command::SetBackgroundTint(Some("15%".into())))
        .unwrap();
    assert_eq!(e.app.identity().unwrap().background_tint, Some(15));
    let settings = e.app.project().unwrap().1.settings;
    assert_eq!(settings.background_tint, Some(15));
    for bad in ["101", "-5", "half"] {
        assert!(
            e.app
                .dispatch(Command::SetBackgroundTint(Some(bad.into())))
                .is_err()
        );
    }
    e.app.dispatch(Command::SetBackgroundTint(None)).unwrap();
    assert_eq!(e.app.identity().unwrap().background_tint, None);
}

#[test]
fn image_icon_is_stored_relative_to_the_project_file() {
    let e = env(&[]);
    open_fresh(&e);
    let def = e.app.project().unwrap().1;
    let file = e.dir.path().join("archipel.toml");
    let id = e
        .app
        .create_project(ProjectLocation::SharedFile(file), &def)
        .unwrap();
    e.app.open_project(&id).unwrap();

    let logo = e.dir.path().join("assets").join("logo.svg");
    e.app
        .dispatch(Command::SetIcon(Some(logo.display().to_string())))
        .unwrap();
    let settings = e.app.project().unwrap().1.settings;
    assert_eq!(settings.icon.as_deref(), Some("assets/logo.svg"));
    let identity = e.app.identity().unwrap();
    assert_eq!(identity.icon, Some(ProjectIcon::Image(logo.clone())));
    assert_eq!(identity.window_title(), "My product — Archipel");
    assert_eq!(e.app.project_icon(&id), Some(ProjectIcon::Image(logo)));

    // An absolute image path is kept; text stays text.
    let elsewhere = e.dir.path().join("Icon.PNG");
    assert_eq!(
        ProjectIcon::parse(&elsewhere.display().to_string(), None),
        Some(ProjectIcon::Image(elsewhere))
    );
    assert_eq!(ProjectIcon::parse("logo.png", None), None);
    assert_eq!(
        ProjectIcon::parse(" 🚀 ", None),
        Some(ProjectIcon::Text("🚀".into()))
    );
}

#[test]
fn dependency_tree_nests_submodules_and_updates_them() {
    let e = env(&[]);
    // r0 has two submodules: r1 (also a project repo) and an uninitialized one.
    let vendor = RepoPath::new(e.repos[0].as_path().join("vendor"));
    e.git.with_repo(&e.repos[0], |r| {
        r.submodules = vec![
            SubmoduleInfo {
                name: "r1".into(),
                path: e.repos[1].clone(),
                initialized: true,
                pinned: Some(oid(1)),
                checked_out: Some(oid(1)),
            },
            SubmoduleInfo {
                name: "vendor".into(),
                path: vendor.clone(),
                initialized: false,
                pinned: Some(oid(7)),
                checked_out: None,
            },
        ];
    });
    open_fresh(&e);
    assert!(e.app.dependency_tree().is_empty(), "off by default");
    e.app.dispatch(Command::ToggleDependencyTree).unwrap();
    wait_until("tree", || e.app.dependency_tree().len() == 4);
    let tree = e.app.dependency_tree();
    let lines: Vec<String> = tree
        .iter()
        .map(|r| format!("{}{}", r.prefix, r.name))
        .collect();
    assert_eq!(lines, ["r0", "├─ r1", "└─ vendor", "r2"]);
    assert_eq!(tree[1].repo, Some(e.repos[1].id()));
    let state = tree[2].state.clone().unwrap();
    assert!(!state.initialized);

    // Collapse r0.
    e.app
        .dispatch(Command::ToggleTreeNode(tree[0].key.clone()))
        .unwrap();
    assert_eq!(e.app.dependency_tree().len(), 2);
    e.app
        .dispatch(Command::ToggleTreeNode(tree[0].key.clone()))
        .unwrap();

    // Init the uninitialized one.
    e.app
        .dispatch(Command::UpdateSubmodules {
            parent: state.parent,
            paths: vec![state.rel_path],
            init: true,
            recursive: false,
        })
        .unwrap();
    wait_until("init", || {
        e.app
            .dependency_tree()
            .iter()
            .any(|r| r.path == vendor && r.state.as_ref().is_some_and(|s| s.initialized))
    });

    // Open the submodule as a repo: added to the project, tab opened.
    e.app
        .dispatch(Command::OpenSubmodule(vendor.clone()))
        .unwrap();
    assert!(e.app.rows().iter().any(|r| r.path == vendor));
    let ui = e.app.ui_state().unwrap();
    assert_eq!(
        ui.tabs[ui.active_tab],
        archipel_model::ui_state::TabState::Repo(vendor.id())
    );
}

#[test]
fn new_project_from_folder_is_a_shared_project_file() {
    let e = env(&[]);
    let id = e.app.new_project_from_folder(e.dir.path()).unwrap();
    let file = e.dir.path().join("archipel.toml");
    assert_eq!(id, archipel_app::file_project_id(&file));
    let entry = e
        .app
        .projects()
        .unwrap()
        .into_iter()
        .find(|p| p.id == id)
        .unwrap();
    assert_eq!(entry.location, ProjectLocation::SharedFile(file));
    let (open, def) = e.app.project().unwrap();
    assert_eq!(open, id);
    assert_eq!(def.repos.len(), 3);
    // The automatic color is written to the file, shared with the team.
    let identity = e.app.identity().unwrap();
    assert_eq!(def.settings.accent, Some(identity.accent.to_hex()));
    assert!(!identity.automatic_accent);
}

#[test]
fn refresh_reloads_the_project_definition() {
    let e = env(&[]);
    open_fresh(&e);
    let before = e.app.identity().unwrap().accent;

    // Another window (or a hand edit of the file) changes the project.
    let other = e.app.sibling();
    other.open_project(&p1()).unwrap();
    other
        .dispatch(Command::SetAccent(Some("#22c55e".into())))
        .unwrap();
    other.remove_repo(e.repos[2].id()).unwrap();
    assert_eq!(e.app.identity().unwrap().accent, before);

    e.app.dispatch(Command::RefreshAll).unwrap();
    assert_eq!(e.app.identity().unwrap().accent.to_hex(), "#22c55e");
    let rows: Vec<_> = e.app.all_rows().iter().map(|r| r.id).collect();
    assert_eq!(rows, [e.repos[0].id(), e.repos[1].id()]);
}
