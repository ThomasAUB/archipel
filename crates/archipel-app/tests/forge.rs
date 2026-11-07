//! Forge integration (spec §8) against fakes: per-repo status column,
//! sign-in / sign-out, pull requests across repos (open, list, merge),
//! clone from an account or from URLs.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use archipel_app::forge::{ForgePorts, ForgeStatus};
use archipel_app::{App, AppError, CloneProtocol, Command, Extensions, Services};
use archipel_model::forge::{
    CiJob, CiJobState, CiRun, CiState, ForgeKind, MergeMethod, MergeState, NamespaceKind,
    PullRequestStatus, RemoteRepo, ReviewState,
};
use archipel_model::ports::{CredentialStore, ForgeAccountStore, RemoteInfo};
use archipel_model::project::{ProjectDef, ProjectLocation, RepoEntry};
use archipel_model::refs::BranchInfo;
use archipel_model::settings::Settings;
use archipel_model::testing::{
    FakeChangeSource, FakeDiscovery, FakeForge, FakeGit, FakeProjectSource, InMemoryCredentials,
    InMemoryStore, oid,
};
use archipel_model::{ForgeError, HeadState, ProjectId, RepoPath, Timestamp, Upstream};
use archipel_ops::GitPorts;

/// Where the test project is stored (in memory: the fake source).
const P_FILE: &str = "/p/archipel.toml";

/// Id of the test project.
fn p_id() -> ProjectId {
    archipel_app::file_project_id(std::path::Path::new(P_FILE))
}

struct Env {
    rt: tokio::runtime::Runtime,
    git: Arc<FakeGit>,
    forge: Arc<FakeForge>,
    creds: Arc<InMemoryCredentials>,
    app: Arc<App>,
    repos: Vec<RepoPath>,
    dir: tempfile::TempDir,
}

/// Three repos on branch `feat/x`:
/// * `r0` on github.com (`acme/r0`), pushed, with an open pull request;
/// * `r1` on github.com (`acme/r1`), no pull request, CI running;
/// * `r2` on a GitLab host nobody signed in to.
fn env() -> Env {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let git = Arc::new(FakeGit::new());
    let forge = Arc::new(FakeForge::new());
    let creds = Arc::new(InMemoryCredentials::new());
    let store = Arc::new(InMemoryStore::new());
    let repos = fake_repos(dir.path(), &git);
    forge.with_host("github.com", ForgeKind::github(), "tok", "me");
    forge.with_open(
        "acme/r0",
        "feat/x",
        PullRequestStatus {
            number: 7,
            title: "Add x".into(),
            url: "https://github.com/acme/r0/pull/7".into(),
            draft: false,
            source: "feat/x".into(),
            target: "main".into(),
            head_sha: "h7".into(),
            author: "ann".into(),
            ci: Some(CiState::Failure),
            review: ReviewState::Approved,
            merge: MergeState::Mergeable,
        },
    );
    forge.with_ci(
        "acme/r1",
        "feat/x",
        CiRun {
            state: CiState::Pending,
            sha: "abc".into(),
            url: Some("https://github.com/acme/r1/commit/abc".into()),
            jobs: vec![
                CiJob {
                    name: "build".into(),
                    stage: None,
                    state: CiJobState::Success,
                    url: None,
                },
                CiJob {
                    name: "test".into(),
                    stage: None,
                    state: CiJobState::Running,
                    url: None,
                },
            ],
        },
    );
    let services = Services {
        git: GitPorts::from_backend(git.clone()),
        projects: Arc::new(FakeProjectSource::default()),
        discovery: Arc::new(FakeDiscovery::default()),
        registry: store.clone(),
        summaries: store.clone(),
        ui_state: store.clone(),
        journal: store.clone(),
        watcher: Arc::new(FakeChangeSource::default()),
        diffs: git.clone(),
        churn: git.clone(),
        stats: store,
        caches: Arc::new(archipel_cache::CacheRegistry::new()),
        cache_dir: dir.path().join("cache"),
        forge: ForgePorts::from_backend(forge.clone(), creds.clone()),
        updates: None,
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
    app.create_project(ProjectLocation::SharedFile(P_FILE.into()), &def)
        .unwrap();
    app.open_project(&p_id()).unwrap();
    Env {
        rt,
        git,
        forge,
        creds,
        app,
        repos,
        dir,
    }
}

/// The three fake repos of [`env`].
fn fake_repos(dir: &std::path::Path, git: &FakeGit) -> Vec<RepoPath> {
    let urls = [
        "git@github.com:acme/r0.git",
        "https://github.com/acme/r1.git",
        "git@gitlab.other.example:grp/r2.git",
    ];
    urls.iter()
        .enumerate()
        .map(|(i, url)| {
            let p = dir.join(format!("r{i}"));
            std::fs::create_dir_all(&p).unwrap();
            let path = RepoPath::new(p);
            git.with_repo(&path, |r| {
                r.summary.head = HeadState::Branch("feat/x".into());
                r.summary.upstream = (i == 0).then(|| Upstream {
                    name: "origin/feat/x".into(),
                    ahead: 0,
                    behind: 0,
                    gone: false,
                });
                r.refs.insert("refs/heads/feat/x".into(), oid(i as u64));
                r.remotes = vec![RemoteInfo {
                    name: "origin".into(),
                    url: (*url).into(),
                }];
                r.branches = vec![BranchInfo {
                    name: if i == 1 {
                        "origin/master"
                    } else {
                        "origin/main"
                    }
                    .into(),
                    is_remote: true,
                    target: oid(100),
                    is_head: false,
                    upstream: None,
                    tip_time: Timestamp(0),
                }];
            });
            path
        })
        .collect()
}

fn wait_until(what: &str, cond: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn forge_of(e: &Env, i: usize) -> ForgeStatus {
    let id = e.repos[i].id();
    e.app
        .rows()
        .into_iter()
        .find(|r| r.id == id)
        .map(|r| r.forge)
        .unwrap()
}

fn sign_in(e: &Env) {
    let app = e.app.clone();
    let account =
        e.rt.block_on(async move { app.sign_in("https://github.com/", None, "tok").await })
            .unwrap();
    assert_eq!(account.kind, ForgeKind::github());
    assert_eq!(account.user, "me");
}

#[test]
fn status_column_follows_sign_in() {
    let e = env();
    wait_until("statuses", || {
        (0..3).all(|i| forge_of(&e, i) != ForgeStatus::Unknown)
    });
    for i in 0..2 {
        assert_eq!(
            forge_of(&e, i),
            ForgeStatus::SignedOut {
                host: "github.com".into()
            }
        );
    }
    assert_eq!(e.forge.reads(), 0, "no API call without an account");

    // A wrong token is refused and nothing is stored.
    let app = e.app.clone();
    let err =
        e.rt.block_on(async move { app.sign_in("github.com", None, "bad").await })
            .unwrap_err();
    assert!(matches!(err, AppError::Op(_)), "{err:?}");
    assert!(e.creds.token("github.com").unwrap().is_none());

    sign_in(&e);
    assert!(e.creds.token("github.com").unwrap().is_some());
    assert_eq!(e.app.forge_accounts().len(), 1);
    wait_until("pull request status", || {
        matches!(forge_of(&e, 0), ForgeStatus::Open(_))
    });
    let ForgeStatus::Open(pr) = forge_of(&e, 0) else {
        unreachable!()
    };
    assert_eq!((pr.number, pr.ci), (7, Some(CiState::Failure)));
    wait_until("no pull request", || {
        forge_of(&e, 1)
            == ForgeStatus::NoPullRequest {
                branch: "feat/x".into(),
            }
    });
    assert_eq!(
        forge_of(&e, 2),
        ForgeStatus::SignedOut {
            host: "gitlab.other.example".into()
        }
    );

    // The column and filters see it.
    let ext = e.app.extensions();
    let col = ext.columns.get("forge").unwrap();
    let row = e.app.rows().into_iter().next().unwrap();
    assert_eq!(col.cell(&row, Timestamp(0)).text, "#7 ✗ approved");

    // The CI column shows the branch CI, with or without a pull request.
    let ci = ext.columns.get("ci").unwrap();
    let rows = e.app.rows();
    assert_eq!(ci.cell(&rows[0], Timestamp(0)).text, "—", "no CI run");
    assert_eq!(ci.cell(&rows[1], Timestamp(0)).text, "● 1/2");
    assert_eq!(
        ci.tooltip(&rows[1]).unwrap()[1],
        "● test · running",
        "running jobs before passed ones"
    );
    assert_eq!(ci.cell(&rows[2], Timestamp(0)).text, "sign in");
    e.app
        .dispatch(Command::ToggleFilter("ci-failing".into()))
        .unwrap();
    assert_eq!(e.app.visible_rows().len(), 1);

    // Fresh statuses are not re-read on focus; an explicit refresh is.
    let reads = e.forge.reads();
    e.app.on_focus();
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(e.forge.reads(), reads);
    e.app.dispatch(Command::RefreshForge).unwrap();
    wait_until("refresh", || e.forge.reads() >= reads + 2);

    // Sign out: statuses go back to "sign in", the token is gone.
    let app = e.app.clone();
    e.rt.block_on(async move { app.sign_out("github.com").await })
        .unwrap();
    assert!(e.creds.token("github.com").unwrap().is_none());
    assert!(e.creds.accounts().unwrap().is_empty());
    wait_until("signed out", || {
        matches!(forge_of(&e, 0), ForgeStatus::SignedOut { .. })
    });
}

fn limited(e: &Env, i: usize) -> bool {
    let id = e.repos[i].id();
    e.app
        .rows()
        .into_iter()
        .any(|r| r.id == id && r.forge_limit.is_some())
}

#[test]
fn rate_limit_keeps_the_last_status_marked() {
    let e = env();
    sign_in(&e);
    wait_until("statuses", || {
        matches!(forge_of(&e, 0), ForgeStatus::Open(_))
            && matches!(forge_of(&e, 1), ForgeStatus::NoPullRequest { .. })
    });
    let reads = e.forge.reads();
    let retry_at = Timestamp(Timestamp::now().0 + 600);
    e.forge.fail(
        "acme/r0",
        ForgeError::RateLimited {
            host: "github.com".into(),
            retry_at: Some(retry_at),
        },
    );
    e.app.dispatch(Command::RefreshForge).unwrap();
    wait_until("rate limited", || limited(&e, 0));
    // The pull request read before stays, marked as maybe out of date.
    assert!(matches!(forge_of(&e, 0), ForgeStatus::Open(_)));
    let ext = e.app.extensions();
    let col = ext.columns.get("forge").unwrap();
    let row = e.app.rows().into_iter().next().unwrap();
    let cell = col.cell(&row, Timestamp::now());
    assert_eq!(cell.text, "#7 ✗ approved ?");
    assert!(cell.detail.unwrap().contains("Rate limited"));
    let ci = ext.columns.get("ci").unwrap();
    assert!(ci.tooltip(&row).unwrap()[0].contains("next read in 10 min"));

    // The host is left alone until it accepts calls again: r1, on the same
    // host, is not read and keeps its status, marked too.
    wait_until("r1 read", || e.forge.reads() > reads);
    let reads = e.forge.reads();
    e.app.dispatch(Command::RefreshForge).unwrap();
    wait_until("r1 backed off", || limited(&e, 1));
    assert_eq!(e.forge.reads(), reads, "no call to a rate-limited host");
    assert!(matches!(forge_of(&e, 1), ForgeStatus::NoPullRequest { .. }));
}

#[test]
fn open_pull_requests_are_listed_and_merged() {
    let e = env();
    sign_in(&e);
    wait_until("status", || matches!(forge_of(&e, 0), ForgeStatus::Open(_)));
    let app = e.app.clone();
    let listed =
        e.rt.block_on(async move { app.open_pull_requests(&[]).await })
            .unwrap();
    let results: Vec<_> = listed
        .iter()
        .map(|r| {
            r.result
                .as_ref()
                .map(|prs| prs.iter().map(|p| p.number).collect::<Vec<_>>())
        })
        .collect();
    assert_eq!(
        results,
        [
            Ok(vec![7]),
            Ok(vec![]),
            Err(&"not signed in to gitlab.other.example".to_owned())
        ]
    );
    let r0 = listed[0].clone();
    let (forge_repo, pr) = (r0.forge.unwrap(), r0.result.unwrap()[0].clone());

    // Someone pushed since the list was read: the forge refuses.
    let mut stale = pr.clone();
    stale.head_sha = "older".into();
    let app = e.app.clone();
    let fr = forge_repo.clone();
    let err =
        e.rt.block_on(async move {
            app.merge_pull_request(r0.repo, &fr, &stale, MergeMethod::Merge, false)
                .await
        })
        .unwrap_err();
    assert!(err.to_string().contains("modified"), "{err}");
    assert!(e.forge.merged().is_empty());

    let app = e.app.clone();
    let report =
        e.rt.block_on(async move {
            app.merge_pull_request(r0.repo, &forge_repo, &pr, MergeMethod::Squash, true)
                .await
        })
        .unwrap();
    assert_eq!(report.warning, None);
    let merged = e.forge.merged();
    assert_eq!(merged.len(), 1);
    assert_eq!(merged[0].number, 7);
    assert_eq!(merged[0].options.method, MergeMethod::Squash);
    assert!(merged[0].options.delete_source_branch);
    assert_eq!(merged[0].options.expected_head.as_deref(), Some("h7"));
    wait_until("status re-read after the merge", || {
        matches!(forge_of(&e, 0), ForgeStatus::NoPullRequest { .. })
    });
}

#[test]
fn pull_requests_across_repos_are_linked() {
    let e = env();
    sign_in(&e);
    wait_until("statuses", || {
        matches!(forge_of(&e, 0), ForgeStatus::Open(_))
            && matches!(forge_of(&e, 1), ForgeStatus::NoPullRequest { .. })
    });
    let app = e.app.clone();
    let mut plan =
        e.rt.block_on(async move { app.prepare_pull_requests(&[]).await })
            .unwrap();
    let t: Vec<_> = plan.targets.iter().map(|t| t.problem.clone()).collect();
    assert_eq!(
        t,
        [
            Some("pull request #7 already open".to_owned()),
            None,
            Some("not signed in to gitlab.other.example".to_owned()),
        ]
    );
    let r1 = &plan.targets[1];
    assert!(r1.selected && r1.needs_push);
    assert_eq!(r1.target, "master", "the remote's default branch");
    assert_eq!(plan.common_target().as_deref(), Some("master"));

    // A second repo on the same branch, also on GitHub.
    let extra = e.dir.path().join("r3");
    std::fs::create_dir_all(&extra).unwrap();
    let extra = RepoPath::new(extra);
    e.git.with_repo(&extra, |r| {
        r.summary.head = HeadState::Branch("feat/x".into());
        r.remotes = vec![RemoteInfo {
            name: "origin".into(),
            url: "git@github.com:acme/r3.git".into(),
        }];
    });
    e.app.add_repos(std::slice::from_ref(&extra)).unwrap();
    wait_until("r3 loaded", || {
        e.app
            .rows()
            .iter()
            .any(|r| r.id == extra.id() && r.summary().is_some())
    });
    let app = e.app.clone();
    plan =
        e.rt.block_on(async move { app.prepare_pull_requests(&[]).await })
            .unwrap();
    assert_eq!(plan.selected().count(), 2);
    assert_eq!(plan.common_target(), None, "master vs main");
    plan.select(e.repos[0].id(), true);
    assert_eq!(
        plan.selected().count(),
        2,
        "a repo with a problem stays out"
    );

    let app = e.app.clone();
    let outcomes =
        e.rt.block_on(async move {
            app.create_pull_requests(&plan, " Add x ", "Why.", None, true)
                .await
        })
        .unwrap();
    assert!(
        outcomes
            .iter()
            .all(|o| o.result.is_ok() && o.link_error.is_none())
    );
    let created = e.forge.created();
    assert_eq!(created.len(), 2);
    let r1 = created.iter().find(|c| c.repo == "acme/r1").unwrap();
    assert_eq!(r1.draft.target, "master");
    assert_eq!(r1.draft.title, "Add x");
    assert!(r1.draft.draft);
    for c in &created {
        assert!(
            c.description.starts_with("Why.\n\n---\n"),
            "{}",
            c.description
        );
        assert!(c.description.contains("acme/r1 #") && c.description.contains("acme/r3 #"));
    }
    // r1 had no upstream: pushed with -u first.
    let pushes: Vec<_> = e
        .git
        .calls()
        .into_iter()
        .filter(|c| c.op == "push")
        .collect();
    assert_eq!(pushes.len(), 2, "r3 has no upstream either");
    wait_until("new pull request shown", || {
        matches!(forge_of(&e, 1), ForgeStatus::Open(_))
    });
}

#[test]
fn clone_from_an_account_adds_repos() {
    let e = env();
    sign_in(&e);
    let repo = |path: &str| RemoteRepo {
        path: path.into(),
        name: path.rsplit('/').next().unwrap().into(),
        description: None,
        https_url: format!("https://github.com/{path}.git"),
        ssh_url: format!("git@github.com:{path}.git"),
        default_branch: Some("main".into()),
        archived: false,
    };
    e.forge
        .with_repos("acme", vec![repo("acme/lib"), repo("acme/tools")]);
    let account = e.app.forge_accounts().remove(0);
    let app = e.app.clone();
    let acct = account.clone();
    let namespaces =
        e.rt.block_on(async move { app.forge_namespaces(&acct).await })
            .unwrap();
    assert_eq!(namespaces[0].kind, NamespaceKind::User);
    let acme = namespaces
        .iter()
        .find(|n| n.path == "acme")
        .unwrap()
        .clone();
    let app = e.app.clone();
    let listed =
        e.rt.block_on(async move { app.forge_repositories(&account, &acme).await })
            .unwrap();
    assert_eq!(listed.len(), 2);

    // `lib` already exists as a plain folder: refused; `tools` is cloned.
    let folder = e.dir.path().join("clones");
    std::fs::create_dir_all(folder.join("lib")).unwrap();
    let app = e.app.clone();
    let f = folder.clone();
    let outcomes =
        e.rt.block_on(async move {
            app.clone_into_project(&listed, &f, CloneProtocol::Ssh)
                .await
        })
        .unwrap();
    assert!(
        outcomes[0]
            .result
            .as_ref()
            .unwrap_err()
            .contains("already exists")
    );
    assert_eq!(
        outcomes[1].result.as_ref().unwrap(),
        &RepoPath::new(folder.join("tools"))
    );
    let clones: Vec<_> = e
        .git
        .calls()
        .into_iter()
        .filter(|c| c.op == "clone")
        .collect();
    assert_eq!(clones.len(), 1);
    assert_eq!(clones[0].detail, "git@github.com:acme/tools.git");
    let id = RepoPath::new(folder.join("tools")).id();
    assert!(e.app.all_rows().iter().any(|r| r.id == id));
}

#[test]
fn clone_from_urls_adds_repos_without_an_account() {
    let e = env();
    // Defaults next to the project file.
    assert_eq!(
        e.app.default_clone_folder(),
        Some(std::path::PathBuf::from("/p"))
    );
    let folder = e.dir.path().join("clones");
    // `lib` already holds a repository: added as is, not cloned again.
    std::fs::create_dir_all(folder.join("lib").join(".git")).unwrap();
    // The group the clones land in is collapsed: expanded so they show up.
    e.app
        .dispatch(Command::ToggleCategory(String::new()))
        .unwrap();
    let urls = vec![
        "https://github.com/acme/lib.git".to_owned(),
        "git@gitlab.com:acme/tools.git".to_owned(),
        "https://example.com/other/tools".to_owned(),
    ];
    let app = e.app.clone();
    let f = folder.clone();
    let outcomes =
        e.rt.block_on(async move { app.clone_urls_into_project(&urls, &f).await })
            .unwrap();
    let paths: Vec<RepoPath> = outcomes.iter().map(|o| o.result.clone().unwrap()).collect();
    assert_eq!(
        paths,
        vec![
            RepoPath::new(folder.join("lib")),
            RepoPath::new(folder.join("tools")),
            RepoPath::new(folder.join("tools-2")),
        ]
    );
    assert_eq!(outcomes[2].repo, "https://example.com/other/tools");
    let clones: Vec<String> = e
        .git
        .calls()
        .into_iter()
        .filter(|c| c.op == "clone")
        .map(|c| c.detail)
        .collect();
    assert_eq!(clones.len(), 2, "{clones:?}");
    for p in &paths {
        assert!(e.app.all_rows().iter().any(|r| r.id == p.id()));
    }
    let ui = e.app.ui_state().unwrap();
    assert!(!ui.collapsed_categories.contains(&String::new()));
    assert_eq!(ui.selected_repo, Some(paths[0].id()));
    assert_eq!(ui.selection, vec![paths[1].id(), paths[2].id()]);

    // A URL with no usable name is refused before anything runs.
    let app = e.app.clone();
    let err =
        e.rt.block_on(async move {
            app.clone_urls_into_project(&["https://host/..".to_owned()], &folder)
                .await
        })
        .unwrap_err();
    assert!(matches!(err, AppError::InvalidInput(_)), "{err}");
}
