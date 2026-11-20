//! Forge integration (spec §8) against fakes: per-repo status column,
//! sign-in / sign-out, pull requests across repos (open, list, merge),
//! clone from an account or from URLs.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use archipel_app::forge::{BranchCi, ForgePorts, ForgeStatus};
use archipel_app::{
    App, AppError, AutoMergeOutcome, CloneProtocol, Command, Extensions, PublishTarget, Services,
};
use archipel_model::forge::{
    CiJob, CiJobLog, CiJobState, CiRun, CiState, CloseReason, ForgeKind, Issue, IssueChoices,
    IssueDraft, MergeMethod, MergeState, Namespace, NamespaceKind, NewRemoteRepo,
    PullRequestStatus, RemoteRepo, ReviewState,
};
use archipel_model::ports::{CredentialStore, ForgeAccountStore, ForgeDirectory, RemoteInfo};
use archipel_model::project::{ProjectDef, ProjectLocation, RepoEntry};
use archipel_model::refs::BranchInfo;
use archipel_model::settings::Settings;
use archipel_model::testing::{
    FakeChangeSource, FakeDiscovery, FakeForge, FakeGit, FakeProjectSource, InMemoryCredentials,
    InMemoryStore, oid,
};
use archipel_model::{ForgeError, HeadState, Oid, ProjectId, RepoPath, Timestamp, Upstream};
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
    env_with(Settings::default())
}

/// [`env`] with `settings`.
#[allow(clippy::too_many_lines)] // the fixture, declared in one place
fn env_with(settings: Settings) -> Env {
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
            from_fork: false,
            auto_merge: false,
        },
    );
    forge.with_ci(
        "acme/r1",
        "feat/x",
        CiRun {
            id: None,
            state: CiState::Pending,
            sha: "abc".into(),
            url: Some("https://github.com/acme/r1/commit/abc".into()),
            jobs: vec![
                CiJob {
                    id: None,
                    name: "build".into(),
                    stage: None,
                    state: CiJobState::Success,
                    url: None,
                },
                CiJob {
                    id: None,
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
        settings,
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
                copy_to_worktrees: Vec::new(),
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

fn ci_of(e: &Env, path: &RepoPath) -> BranchCi {
    let id = path.id();
    e.app
        .rows()
        .into_iter()
        .find(|r| r.id == id)
        .map(|r| r.ci)
        .unwrap()
}

fn ci_run(state: CiState) -> CiRun {
    CiRun {
        id: None,
        state,
        sha: "abc".into(),
        url: None,
        jobs: vec![CiJob {
            id: None,
            name: "build".into(),
            stage: None,
            state: match state {
                CiState::Pending => CiJobState::Running,
                CiState::Success => CiJobState::Success,
                CiState::Failure => CiJobState::Failed,
            },
            url: None,
        }],
    }
}

fn forge_of_path(e: &Env, path: &RepoPath) -> ForgeStatus {
    let id = path.id();
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
    // host, keeps its status, marked too. A read of r1 started before r0
    // hit the limit may still call the host; once r1 is marked, every later
    // read of it (reads of one repo run one at a time) knows the limit.
    e.app.dispatch(Command::RefreshForge).unwrap();
    wait_until("r1 backed off", || limited(&e, 1));
    assert!(matches!(forge_of(&e, 1), ForgeStatus::NoPullRequest { .. }));
    let reads = e.forge.reads();
    e.app.dispatch(Command::RefreshForge).unwrap();
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(e.forge.reads(), reads, "no call to a rate-limited host");
    assert!(limited(&e, 1));
}

#[test]
fn passing_pull_request_failure_keeps_the_last_status() {
    let e = env();
    sign_in(&e);
    wait_until("statuses", || {
        matches!(forge_of(&e, 1), ForgeStatus::NoPullRequest { .. })
    });
    // The CI read answers, the pull request read hits a forge-side error
    // (as a 502 on one of its calls does).
    e.forge.fail_pull_requests(
        "acme/r1",
        Some(ForgeError::Http {
            host: "github.com".into(),
            status: 502,
            message: "bad gateway".into(),
        }),
    );
    e.app.dispatch(Command::RefreshForge).unwrap();
    wait_until("marked stale", || limited(&e, 1));
    assert!(
        matches!(forge_of(&e, 1), ForgeStatus::NoPullRequest { .. }),
        "the last status stays rather than \"error\""
    );
    let ext = e.app.extensions();
    let col = ext.columns.get("forge").unwrap();
    let row = e.app.rows().into_iter().nth(1).unwrap();
    let cell = col.cell(&row, Timestamp::now());
    assert_eq!(cell.text, "none ?");
    assert!(cell.detail.unwrap().contains("Last forge read failed"));

    // Once the forge answers again, the mark goes.
    e.forge.fail_pull_requests("acme/r1", None);
    e.app.dispatch(Command::RefreshForge).unwrap();
    wait_until("fresh again", || !limited(&e, 1));
    assert!(matches!(forge_of(&e, 1), ForgeStatus::NoPullRequest { .. }));

    // A lasting failure still shows as an error.
    e.forge.fail_pull_requests(
        "acme/r1",
        Some(ForgeError::Decode {
            host: "github.com".into(),
            message: "bad json".into(),
        }),
    );
    e.app.dispatch(Command::RefreshForge).unwrap();
    wait_until("error", || matches!(forge_of(&e, 1), ForgeStatus::Error(_)));
}

#[test]
fn passing_failure_of_a_first_read_is_retried_before_showing_an_error() {
    let e = env_with(Settings {
        forge_refresh_secs: 1,
        ..Settings::default()
    });
    let unreachable = ForgeError::Network {
        host: "github.com".into(),
        message: "connection reset".into(),
    };
    e.forge.fail("acme/r0", unreachable.clone());
    e.forge.fail("acme/r1", unreachable);
    sign_in(&e);
    // "sign in" goes once the first read lands: it shows nothing.
    let r1 = e.repos[1].clone();
    wait_until("first read", || forge_of(&e, 1) == ForgeStatus::Unknown);
    assert_eq!(ci_of(&e, &r1), BranchCi::Unknown, "no error shown yet");

    // No focus, no refresh: the retry alone shows r1 once it answers…
    e.forge.recover("acme/r1");
    wait_until(
        "r1 read again",
        || matches!(ci_of(&e, &r1), BranchCi::Run(r) if r.state == CiState::Pending),
    );
    assert!(matches!(forge_of(&e, 1), ForgeStatus::NoPullRequest { .. }));
    // …and the error of r0, which keeps failing.
    let r0 = e.repos[0].clone();
    wait_until("r0 error", || {
        matches!(ci_of(&e, &r0), BranchCi::Error(_))
            && matches!(forge_of(&e, 0), ForgeStatus::Error(_))
    });
}

#[test]
#[allow(clippy::too_many_lines)] // one story, from listing to reopening
fn issues_are_listed_created_closed_and_reopened() {
    let e = env();
    sign_in(&e);
    e.forge.with_issue(
        "acme/r0",
        Issue {
            number: 3,
            title: "Crash".into(),
            url: "https://github.com/acme/r0/issues/3".into(),
            author: "ann".into(),
            labels: vec!["bug".into()],
            assignees: Vec::new(),
            created_at: None,
        },
    );
    e.forge.with_issue_choices(
        "acme/r0",
        IssueChoices {
            labels: vec!["bug".into(), "ui".into()],
            assignees: vec!["ann".into()],
        },
    );
    e.forge.without_issues("acme/r1");

    let app = e.app.clone();
    let targets =
        e.rt.block_on(async move { app.issue_targets(&[]).await })
            .unwrap();
    let choices: Vec<_> = targets.iter().map(|t| t.choices.clone()).collect();
    assert_eq!(choices[0].as_ref().unwrap().labels, ["bug", "ui"]);
    // Issues disabled on r1, r2's forge not signed in: said before typing.
    assert!(
        choices[1]
            .as_ref()
            .unwrap_err()
            .contains("issues are disabled on acme/r1")
    );
    assert!(choices[2].as_ref().unwrap_err().contains("not signed in"));

    let r0 = targets[0].clone();
    let forge_repo = r0.forge.clone().unwrap();
    assert_eq!(forge_repo.path, "acme/r0");
    let app = e.app.clone();
    let fr = forge_repo.clone();
    let err =
        e.rt.block_on(async move { app.create_issue(r0.repo, &fr, IssueDraft::default()).await })
            .unwrap_err();
    assert!(matches!(err, AppError::InvalidInput(_)), "{err}");
    let app = e.app.clone();
    let fr = forge_repo.clone();
    let created =
        e.rt.block_on(async move {
            let draft = IssueDraft {
                title: "  Slow start ".into(),
                description: "Takes 10 s".into(),
                labels: vec!["ui".into()],
                assignees: vec!["ann".into()],
            };
            app.create_issue(r0.repo, &fr, draft).await
        })
        .unwrap();
    assert_eq!(created.number, 4);
    assert_eq!(created.url, "https://github.com/acme/r0/issues/4");

    let app = e.app.clone();
    let listed =
        e.rt.block_on(async move { app.open_issues(&[]).await })
            .unwrap();
    let numbers: Vec<_> = listed[0]
        .result
        .as_ref()
        .unwrap()
        .iter()
        .map(|i| (i.number, i.title.clone()))
        .collect();
    assert_eq!(numbers, [(3, "Crash".into()), (4, "Slow start".into())]);
    assert!(listed[1].result.is_err());

    let crash = listed[0].result.as_ref().unwrap()[0].clone();
    let app = e.app.clone();
    let fr = forge_repo.clone();
    e.rt.block_on(async move {
        app.close_issue(
            r0.repo,
            &fr,
            &crash,
            CloseReason::NotPlanned,
            Some("Duplicate of #1".into()),
        )
        .await
    })
    .unwrap();
    let closed = e
        .forge
        .issues()
        .into_iter()
        .find(|i| i.issue.number == 3)
        .unwrap();
    assert!(!closed.open);
    assert_eq!(closed.reason, Some(CloseReason::NotPlanned));
    assert_eq!(closed.comments, ["Duplicate of #1"]);
    let session: Vec<u64> = e
        .app
        .closed_issues()
        .iter()
        .map(|c| c.issue.number)
        .collect();
    assert_eq!(session, [3]);

    let app = e.app.clone();
    e.rt.block_on(async move { app.reopen_issue(r0.repo, &forge_repo, 3).await })
        .unwrap();
    assert!(
        e.forge
            .issues()
            .iter()
            .find(|i| i.issue.number == 3)
            .unwrap()
            .open
    );
    assert!(e.app.closed_issues().is_empty());
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
    // The deleted source branch leaves the local remote-tracking refs too.
    wait_until("pruning fetch after the merge", || {
        e.git
            .calls()
            .iter()
            .any(|c| c.op == "fetch" && c.detail.ends_with("--prune"))
    });
}

#[test]
fn pull_requests_across_repos_are_linked() {
    let e = env();
    sign_in(&e);
    wait_until("statuses", || {
        matches!(forge_of(&e, 0), ForgeStatus::Open(_))
            && matches!(forge_of(&e, 1), ForgeStatus::NoPullRequest { .. })
            // The plan skips repos whose summary is not loaded yet.
            && e.app.rows().iter().filter(|r| r.summary().is_some()).count() == 3
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

/// Git calls of `op` on `repo`, their details.
fn calls_of(e: &Env, op: &str, repo: &RepoPath) -> Vec<String> {
    e.git
        .calls()
        .into_iter()
        .filter(|c| c.op == op && c.repo == repo.as_path().display().to_string())
        .map(|c| c.detail)
        .collect()
}

/// Publishes row `i` to `target`.
fn publish(e: &Env, i: usize, target: PublishTarget) -> Result<String, AppError> {
    let app = e.app.clone();
    let id = e.repos[i].id();
    e.rt.block_on(async move { app.publish_repo(id, target).await })
        .map(|o| o.url)
}

fn new_repo(e: &Env, namespace: &str, name: &str) -> PublishTarget {
    PublishTarget::Forge {
        account: e.app.forge_accounts().remove(0),
        namespace: Namespace {
            path: namespace.into(),
            kind: if namespace == "me" {
                NamespaceKind::User
            } else {
                NamespaceKind::Organization
            },
        },
        repo: NewRemoteRepo {
            name: name.into(),
            description: Some("A tool".into()),
            private: true,
        },
        protocol: CloneProtocol::Https,
    }
}

#[test]
fn publish_creates_the_repo_adds_origin_and_pushes() {
    let e = env();
    sign_in(&e);
    let path = e.repos[1].clone();
    e.git.with_repo(&path, |r| r.remotes.clear());

    let url = publish(&e, 1, new_repo(&e, "acme", " tool ")).unwrap();
    assert_eq!(url, "https://github.com/acme/tool.git");
    let account = e.app.forge_accounts().remove(0);
    let listed = e
        .forge
        .repositories(
            &account,
            &Namespace {
                path: "acme".into(),
                kind: NamespaceKind::Organization,
            },
        )
        .unwrap();
    assert_eq!(listed[0].path, "acme/tool");
    assert_eq!(
        calls_of(&e, "set_remote_url", &path),
        ["origin->https://github.com/acme/tool.git"]
    );
    assert_eq!(calls_of(&e, "push", &path), ["feat/x"]);

    // It has a remote now: publishing again is refused, nothing created.
    let err = publish(&e, 1, new_repo(&e, "acme", "other")).unwrap_err();
    assert!(
        err.to_string().contains("already has the remote origin"),
        "{err}"
    );
    assert_eq!(calls_of(&e, "push", &path).len(), 1);
}

#[test]
fn publish_refuses_what_cannot_be_pushed_before_creating_anything() {
    let e = env();
    sign_in(&e);
    let path = e.repos[1].clone();
    e.git.with_repo(&path, |r| {
        r.remotes.clear();
        r.summary.head = HeadState::Unborn("main".into());
    });
    let err = publish(&e, 1, new_repo(&e, "me", "tool")).unwrap_err();
    assert!(err.to_string().contains("no commit yet"), "{err}");
    let account = e.app.forge_accounts().remove(0);
    let mine = Namespace {
        path: "me".into(),
        kind: NamespaceKind::User,
    };
    assert!(e.forge.repositories(&account, &mine).unwrap().is_empty());
    assert!(calls_of(&e, "set_remote_url", &path).is_empty());

    // A name taken on the forge: refused, no remote added.
    e.git.with_repo(&path, |r| {
        r.summary.head = HeadState::Branch("feat/x".into());
    });
    publish(&e, 1, new_repo(&e, "me", "tool")).unwrap();
    let other = e.repos[2].clone();
    e.git.with_repo(&other, |r| r.remotes.clear());
    let err = publish(&e, 2, new_repo(&e, "me", "tool")).unwrap_err();
    assert!(err.to_string().contains("already exists"), "{err}");
    assert!(calls_of(&e, "set_remote_url", &other).is_empty());
    assert!(calls_of(&e, "push", &other).is_empty());
}

#[test]
fn publish_to_a_url_needs_no_account() {
    let e = env();
    let path = e.repos[2].clone();
    e.git.with_repo(&path, |r| r.remotes.clear());
    let err = publish(&e, 2, PublishTarget::Url("  ".into())).unwrap_err();
    assert!(matches!(err, AppError::InvalidInput(_)));
    let url = publish(
        &e,
        2,
        PublishTarget::Url(" git@example.com:me/tool.git ".into()),
    )
    .unwrap();
    assert_eq!(url, "git@example.com:me/tool.git");
    assert_eq!(
        calls_of(&e, "set_remote_url", &path),
        ["origin->git@example.com:me/tool.git"]
    );
    assert_eq!(calls_of(&e, "push", &path), ["feat/x"]);
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

#[test]
fn running_ci_is_followed_until_it_ends() {
    let e = env_with(Settings {
        ci_poll_secs: 1,
        ..Settings::default()
    });
    sign_in(&e);
    let r1 = e.repos[1].clone();
    wait_until(
        "running CI",
        || matches!(ci_of(&e, &r1), BranchCi::Run(r) if r.state == CiState::Pending),
    );
    e.forge
        .with_ci("acme/r1", "feat/x", ci_run(CiState::Success));
    // No focus, no refresh: the follow-up read alone shows the end.
    wait_until(
        "finished CI",
        || matches!(ci_of(&e, &r1), BranchCi::Run(r) if r.state == CiState::Success),
    );
}

#[test]
fn ci_registered_after_a_pull_request_is_opened_shows() {
    let e = env_with(Settings {
        ci_poll_secs: 1,
        ..Settings::default()
    });
    sign_in(&e);
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
    wait_until("r3 without CI", || {
        e.app
            .rows()
            .iter()
            .any(|r| r.id == extra.id() && r.summary().is_some())
            && ci_of(&e, &extra) == BranchCi::None
    });
    let app = e.app.clone();
    let ids = [extra.id()];
    let plan =
        e.rt.block_on(async move { app.prepare_pull_requests(&ids).await })
            .unwrap();
    let app = e.app.clone();
    let outcomes =
        e.rt.block_on(async move {
            app.create_pull_requests(&plan, "Add x", "", None, false)
                .await
        })
        .unwrap();
    assert!(outcomes.iter().all(|o| o.result.is_ok()));
    wait_until("pull request shown", || {
        matches!(forge_of_path(&e, &extra), ForgeStatus::Open(_))
    });
    assert_eq!(ci_of(&e, &extra), BranchCi::None, "runs not registered yet");
    // The forge registers the runs a moment later.
    e.forge
        .with_ci("acme/r3", "feat/x", ci_run(CiState::Pending));
    wait_until(
        "CI shown",
        || matches!(ci_of(&e, &extra), BranchCi::Run(r) if r.state == CiState::Pending),
    );
}

/// A run of `state` for commit `sha`.
fn ci_run_of(state: CiState, sha: Oid) -> CiRun {
    CiRun {
        sha: sha.to_hex(),
        ..ci_run(state)
    }
}

#[test]
fn ci_of_another_commit_than_the_upstream_is_outdated_until_the_forge_catches_up() {
    let e = env_with(Settings {
        ci_poll_secs: 1,
        ..Settings::default()
    });
    let r0 = e.repos[0].clone();
    let at = |e: &Env, n: u64| {
        e.git.with_repo(&r0, |r| {
            r.summary.head_oid = Some(oid(n));
            r.refs.insert("refs/heads/feat/x".into(), oid(n));
            r.refs.insert("refs/remotes/origin/feat/x".into(), oid(n));
        });
    };
    at(&e, 1);
    e.forge
        .with_ci("acme/r0", "feat/x", ci_run_of(CiState::Success, oid(1)));
    sign_in(&e);
    wait_until(
        "CI of the upstream's tip",
        || matches!(ci_of(&e, &r0), BranchCi::Run(r) if r.state == CiState::Success),
    );

    // A push: the branch and its upstream move, the forge has not
    // registered the new run yet. Read again at once, well within
    // `forge_refresh_secs`, and shown as out of date.
    at(&e, 2);
    e.app.refresh_repo(r0.id());
    wait_until(
        "outdated CI",
        || matches!(ci_of(&e, &r0), BranchCi::Outdated { ref tip, .. } if *tip == oid(2).to_hex()),
    );
    let cell =
        archipel_app::forge::ci_cell(&ci_of(&e, &r0), &forge_of(&e, 0), None, Timestamp::now());
    assert!(
        cell.text.ends_with(archipel_app::forge::OUTDATED_MARK),
        "{}",
        cell.text
    );

    // No focus, no refresh: followed until the forge shows the new run.
    e.forge
        .with_ci("acme/r0", "feat/x", ci_run_of(CiState::Pending, oid(2)));
    wait_until(
        "CI of the new tip",
        || matches!(ci_of(&e, &r0), BranchCi::Run(r) if r.state == CiState::Pending),
    );
}

#[test]
fn refresh_fetches_and_reads_the_forge_again() {
    let e = env();
    sign_in(&e);
    let r0 = e.repos[0].clone();
    wait_until("pull request shown", || {
        matches!(forge_of_path(&e, &r0), ForgeStatus::Open(_))
    });
    let reads = e.forge.reads();
    e.app.dispatch(Command::RefreshAll).unwrap();
    // Only r0 has an upstream (nor was any repo fetched before).
    wait_until("fetch", || {
        let fetched: Vec<_> = e
            .git
            .calls()
            .into_iter()
            .filter(|c| c.op == "fetch")
            .map(|c| c.repo)
            .collect();
        fetched == [r0.as_path().display().to_string()]
    });
    // Read again though fresh (`forge_refresh_secs` not elapsed).
    wait_until("forge read again", || e.forge.reads() > reads);
}

/// Releases the held reads of a repo when dropped.
struct Release<'a>(&'a FakeForge, &'static str);

impl Drop for Release<'_> {
    fn drop(&mut self) {
        self.0.release(self.1);
    }
}

#[test]
fn checkout_during_a_forge_read_shows_the_new_branch() {
    let e = env();
    sign_in(&e);
    let r0 = e.repos[0].clone();
    wait_until("pull request shown", || {
        matches!(forge_of_path(&e, &r0), ForgeStatus::Open(_))
    });
    e.forge
        .with_ci("acme/r0", "feat/y", ci_run(CiState::Success));
    // A slow forge: a read of `feat/x` is still going when the branch
    // changes.
    e.forge.hold("acme/r0");
    // Released even when an assertion fails: a held read would hang the
    // runtime shutdown.
    let release = Release(&e.forge, "acme/r0");
    e.app.refresh_forge_all();
    wait_until("read of feat/x started", || e.forge.waiting("acme/r0") == 1);
    e.git.with_repo(&r0, |r| {
        r.summary.head = HeadState::Branch("feat/y".into());
    });
    e.app.refresh_repo(r0.id());
    // The status of `feat/x` is not shown for `feat/y`.
    wait_until("previous branch status cleared", || {
        forge_of_path(&e, &r0) == ForgeStatus::Unknown && ci_of(&e, &r0) == BranchCi::Unknown
    });
    drop(release);
    // No refresh period waited for: `feat/y` is read once `feat/x` lands.
    wait_until(
        "CI of feat/y",
        || matches!(ci_of(&e, &r0), BranchCi::Run(r) if r.state == CiState::Success),
    );
    assert_eq!(
        forge_of_path(&e, &r0),
        ForgeStatus::NoPullRequest {
            branch: "feat/y".into()
        }
    );
}

#[test]
fn failed_ci_log_is_read_once_and_failing_ci_reruns() {
    let e = env();
    let failed = CiJob {
        id: Some(41),
        name: "test".into(),
        stage: None,
        state: CiJobState::Failed,
        url: Some("https://github.com/acme/r0/actions/runs/1/job/41".into()),
    };
    e.forge.with_ci(
        "acme/r0",
        "feat/x",
        CiRun {
            id: None,
            state: CiState::Failure,
            sha: "f00".into(),
            url: None,
            jobs: vec![failed.clone()],
        },
    );
    e.forge.with_ci_log(
        "acme/r0",
        41,
        CiJobLog {
            excerpt: vec!["error: boom".into()],
            matched: true,
            url: failed.url.clone(),
        },
    );
    sign_in(&e);
    wait_until(
        "failing CI",
        || matches!(ci_of(&e, &e.repos[0]), BranchCi::Run(r) if r.state == CiState::Failure),
    );
    let r0 = e.repos[0].id();

    // The excerpt is read once, then served from the cache.
    assert_eq!(e.app.cached_ci_log(r0, 41), None);
    for _ in 0..2 {
        let app = e.app.clone();
        let job = failed.clone();
        let log =
            e.rt.block_on(async move { app.ci_job_log(r0, &job).await })
                .unwrap();
        assert_eq!(log.excerpt, ["error: boom"]);
    }
    assert_eq!(e.forge.log_reads(), 1);
    assert!(e.app.cached_ci_log(r0, 41).is_some());
    // A check posted by another CI has no log to read.
    let app = e.app.clone();
    let other = CiJob { id: None, ..failed };
    assert!(
        e.rt.block_on(async move { app.ci_job_log(r0, &other).await })
            .is_err()
    );

    // Rerun on every repo: only r0 fails, the others are skipped.
    e.app.dispatch(Command::RerunFailedCi(Vec::new())).unwrap();
    wait_until("rerun", || !e.forge.reruns().is_empty());
    assert_eq!(e.forge.reruns(), [("acme/r0".to_owned(), "f00".to_owned())]);
}

/// The listed pull request #7 of `r0`, with its forge repository.
fn listed_pr(
    e: &Env,
) -> (
    archipel_model::RepoId,
    archipel_model::forge::ForgeRepo,
    PullRequestStatus,
) {
    let app = e.app.clone();
    let listed =
        e.rt.block_on(async move { app.open_pull_requests(&[]).await })
            .unwrap();
    let r0 = listed[0].clone();
    (r0.repo, r0.forge.unwrap(), r0.result.unwrap()[0].clone())
}

fn row_of(e: &Env, i: usize) -> archipel_app::dashboard::DashboardRow {
    e.app.row(e.repos[i].id()).unwrap()
}

/// `r0`'s pull request #7 as the forge now serves it, with `ci`.
fn reopen_r0(e: &Env, ci: CiState, auto_merge: bool) {
    e.forge.with_open(
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
            ci: Some(ci),
            review: ReviewState::Approved,
            merge: MergeState::Mergeable,
            from_fork: false,
            auto_merge,
        },
    );
}

#[test]
fn auto_merge_is_armed_on_the_forge_and_cancelled() {
    let e = env();
    sign_in(&e);
    wait_until("status", || matches!(forge_of(&e, 0), ForgeStatus::Open(_)));
    let (repo, forge_repo, pr) = listed_pr(&e);
    let app = e.app.clone();
    let fr = forge_repo.clone();
    let outcome =
        e.rt.block_on(async move {
            app.arm_auto_merge(repo, &fr, &pr, MergeMethod::Squash, true)
                .await
        })
        .unwrap();
    assert_eq!(outcome, AutoMergeOutcome::Forge { warning: None });
    let armed = e.forge.auto_merges();
    assert_eq!(armed.len(), 1);
    assert_eq!((armed[0].0.as_str(), armed[0].1), ("acme/r0", 7));
    assert_eq!(armed[0].2.method, MergeMethod::Squash);
    assert_eq!(armed[0].2.expected_head.as_deref(), Some("h7"));
    // Read back from the forge, and marked in the "Pull request" column.
    wait_until(
        "armed state read back",
        || matches!(forge_of(&e, 0), ForgeStatus::Open(p) if p.auto_merge),
    );
    let column = archipel_app::forge::ForgeColumn;
    let cell =
        archipel_app::dashboard::DashboardColumn::cell(&column, &row_of(&e, 0), Timestamp(0));
    assert!(cell.text.ends_with('⏲'), "{}", cell.text);
    assert!(
        cell.detail
            .as_deref()
            .is_some_and(|d| d.contains("auto-merge on the forge")),
        "{:?}",
        cell.detail
    );

    // Nothing was merged by Archipel: the forge does it.
    assert!(e.forge.merged().is_empty());
    e.app
        .dispatch(Command::CancelAutoMerge(vec![repo]))
        .unwrap();
    wait_until("auto-merge cancelled", || e.forge.auto_merges().is_empty());
    wait_until(
        "cancelled state read back",
        || matches!(forge_of(&e, 0), ForgeStatus::Open(p) if !p.auto_merge),
    );
}

#[test]
fn refused_auto_merge_is_watched_and_merged_once_ci_passes() {
    let e = env_with(Settings {
        ci_poll_secs: 1,
        ..Settings::default()
    });
    sign_in(&e);
    e.forge.refuse_auto_merge(
        "acme/r0",
        ForgeError::Unsupported("Auto merge is not allowed for this repository".into()),
    );
    wait_until("status", || matches!(forge_of(&e, 0), ForgeStatus::Open(_)));
    let (repo, forge_repo, pr) = listed_pr(&e);
    let app = e.app.clone();
    let outcome =
        e.rt.block_on(async move {
            app.arm_auto_merge(repo, &forge_repo, &pr, MergeMethod::Merge, false)
                .await
        })
        .unwrap();
    assert!(
        matches!(&outcome, AutoMergeOutcome::Watched { refusal } if refusal.contains("not allowed")),
        "{outcome:?}"
    );
    // The CI of #7 fails: it stays armed, saying why it waits.
    wait_until("waiting on the failed CI", || {
        row_of(&e, 0)
            .auto_merge
            .and_then(|w| w.waiting)
            .is_some_and(|w| w.contains("CI failed"))
    });
    assert!(e.forge.merged().is_empty());

    reopen_r0(&e, CiState::Success, false);
    wait_until("merged once the CI passed", || !e.forge.merged().is_empty());
    let merged = e.forge.merged();
    assert_eq!(merged[0].number, 7);
    assert_eq!(merged[0].options.method, MergeMethod::Merge);
    assert_eq!(merged[0].options.expected_head.as_deref(), Some("h7"));
    wait_until("watch dropped", || {
        e.app.auto_merge_watch(repo, 7).is_none() && row_of(&e, 0).auto_merge.is_none()
    });
}

#[test]
fn cancelled_watch_does_not_merge() {
    let e = env_with(Settings {
        ci_poll_secs: 1,
        ..Settings::default()
    });
    sign_in(&e);
    e.forge.refuse_auto_merge(
        "acme/r0",
        ForgeError::Unsupported("Auto merge is not allowed for this repository".into()),
    );
    wait_until("status", || matches!(forge_of(&e, 0), ForgeStatus::Open(_)));
    let (repo, forge_repo, pr) = listed_pr(&e);
    let app = e.app.clone();
    let (fr, p) = (forge_repo.clone(), pr.clone());
    e.rt.block_on(async move {
        app.arm_auto_merge(repo, &fr, &p, MergeMethod::Merge, false)
            .await
    })
    .unwrap();
    assert!(e.app.auto_merge_watch(repo, 7).is_some());
    let app = e.app.clone();
    e.rt.block_on(async move { app.cancel_auto_merge(repo, &forge_repo, &pr).await })
        .unwrap();
    assert!(e.app.auto_merge_watch(repo, 7).is_none());
    reopen_r0(&e, CiState::Success, false);
    std::thread::sleep(Duration::from_millis(2500));
    assert!(e.forge.merged().is_empty());
}
