//! Provider logic (GitHub, GitLab, detection, dispatch) against a fake HTTP
//! transport: no network.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// Route bodies read best as `json!` literals passed by value.
#![allow(clippy::needless_pass_by_value)]

use std::sync::{Arc, Mutex};

use archipel_forge::ForgeClient;
use archipel_forge::http::{HttpRequest, HttpResponse, HttpTransport, Method};
use archipel_model::ForgeError;
use archipel_model::forge::{
    CiJob, CiJobState, CiRun, CiState, CloseReason, ForgeAccount, ForgeKind, ForgeRepo, ForgeToken,
    IssueDraft, MergeMethod, MergeOptions, MergeState, Namespace, NamespaceKind, NewRemoteRepo,
    PullRequestDraft, ReviewState,
};
use archipel_model::ports::{
    CiReader, CiWriter, CredentialStore, ForgeDirectory, ForgeSignIn, IssueReader, IssueWriter,
    PullRequestReader, PullRequestWriter,
};
use archipel_model::testing::InMemoryCredentials;
use serde_json::json;

/// Answers by (method, URL substring), first match wins; 404 otherwise.
#[derive(Default)]
struct FakeHttp {
    routes: Mutex<Vec<(Method, String, HttpResponse)>>,
    seen: Mutex<Vec<HttpRequest>>,
}

impl FakeHttp {
    fn on(&self, method: Method, url_part: &str, status: u16, body: serde_json::Value) {
        self.on_with(method, url_part, status, body, &[]);
    }

    fn on_with(
        &self,
        method: Method,
        url_part: &str,
        status: u16,
        body: serde_json::Value,
        headers: &[(&str, &str)],
    ) {
        self.routes.lock().unwrap().push((
            method,
            url_part.into(),
            HttpResponse {
                status,
                headers: headers
                    .iter()
                    .map(|(k, v)| ((*k).into(), (*v).into()))
                    .collect(),
                body: body.to_string(),
            },
        ));
    }

    /// A plain-text answer (a log).
    fn on_text(&self, method: Method, url_part: &str, status: u16, text: &str) {
        self.routes.lock().unwrap().push((
            method,
            url_part.into(),
            HttpResponse {
                status,
                headers: Vec::new(),
                body: text.into(),
            },
        ));
    }

    fn seen(&self) -> Vec<HttpRequest> {
        self.seen.lock().unwrap().clone()
    }
}

impl HttpTransport for FakeHttp {
    fn send(&self, request: &HttpRequest) -> Result<HttpResponse, String> {
        self.seen.lock().unwrap().push(request.clone());
        if request.url.contains("unreachable") {
            return Err("connection refused".into());
        }
        Ok(self
            .routes
            .lock()
            .unwrap()
            .iter()
            .find(|(m, part, _)| *m == request.method && request.url.contains(part.as_str()))
            .map_or_else(
                || HttpResponse {
                    status: 404,
                    body: "{\"message\":\"Not Found\"}".into(),
                    ..HttpResponse::default()
                },
                |(_, _, r)| r.clone(),
            ))
    }
}

fn client(http: &Arc<FakeHttp>, hosts: &[&str]) -> ForgeClient {
    let creds = Arc::new(InMemoryCredentials::new());
    for h in hosts {
        creds.set_token(h, &ForgeToken::new("tok")).unwrap();
    }
    ForgeClient::with_builtins(http.clone(), creds)
}

fn account(host: &str, kind: ForgeKind, api: &str) -> ForgeAccount {
    ForgeAccount {
        host: host.into(),
        kind,
        api_url: api.into(),
        user: "me".into(),
    }
}

fn github_repo() -> ForgeRepo {
    ForgeRepo {
        account: account("github.com", ForgeKind::github(), "https://api.github.com"),
        path: "octo/app".into(),
    }
}

fn gitlab_repo() -> ForgeRepo {
    ForgeRepo {
        account: account(
            "git.corp.example",
            ForgeKind::gitlab(),
            "https://git.corp.example/api/v4",
        ),
        path: "team/sub/app".into(),
    }
}

#[test]
fn github_pull_request_with_ci_and_reviews() {
    let http = Arc::new(FakeHttp::default());
    http.on(
        Method::Get,
        "/repos/octo/app/pulls?state=open&per_page=1&head=octo%3Afeat%2Fx",
        200,
        json!([{
            "number": 42, "title": "Add x", "html_url": "https://github.com/octo/app/pull/42",
            "draft": false, "base": {"ref": "main", "sha": "b"}, "head": {"ref": "feat/x", "sha": "abc"}
        }]),
    );
    http.on(
        Method::Get,
        "/commits/abc/check-runs",
        200,
        json!({"check_runs": [
            {"status": "completed", "conclusion": "success"},
            {"status": "in_progress", "conclusion": null}
        ]}),
    );
    http.on(
        Method::Get,
        "/commits/abc/status",
        200,
        json!({"state": "success", "total_count": 1}),
    );
    http.on(
        Method::Get,
        "/pulls/42/reviews",
        200,
        json!([
            {"state": "CHANGES_REQUESTED", "user": {"login": "ann"}},
            {"state": "COMMENTED", "user": {"login": "bob"}},
            {"state": "APPROVED", "user": {"login": "ann"}}
        ]),
    );
    let c = client(&http, &["github.com"]);
    let pr = c
        .pull_request_for_branch(&github_repo(), "feat/x")
        .unwrap()
        .unwrap();
    assert_eq!(pr.number, 42);
    assert_eq!(pr.target, "main");
    assert_eq!(pr.ci, Some(CiState::Pending), "one check still running");
    assert_eq!(pr.review, ReviewState::Approved, "ann's latest review wins");
    assert!(!pr.auto_merge);
    let first = &http.seen()[0];
    assert!(
        first
            .headers
            .contains(&("Authorization".into(), "Bearer tok".into()))
    );
    assert!(
        first
            .url
            .starts_with("https://api.github.com/repos/octo/app/")
    );
}

#[test]
fn github_ci_failure_and_missing_scopes() {
    let http = Arc::new(FakeHttp::default());
    http.on(
        Method::Get,
        "/pulls?state=open",
        200,
        json!([{
            "number": 7, "title": "t", "html_url": "u", "base": {"ref": "dev", "sha": "b"},
            "head": {"ref": "f", "sha": "s1"}
        }]),
    );
    http.on(
        Method::Get,
        "/check-runs",
        200,
        json!({"check_runs": [{"status": "completed", "conclusion": "failure"}]}),
    );
    // Status and reviews forbidden for this token: still a result.
    http.on(Method::Get, "/status", 403, json!({"message": "no"}));
    http.on(Method::Get, "/reviews", 403, json!({"message": "no"}));
    let c = client(&http, &["github.com"]);
    let pr = c
        .pull_request_for_branch(&github_repo(), "f")
        .unwrap()
        .unwrap();
    assert_eq!(pr.ci, Some(CiState::Failure));
    assert_eq!(pr.review, ReviewState::Pending);
}

#[test]
fn github_no_pull_request_and_errors() {
    let http = Arc::new(FakeHttp::default());
    http.on(Method::Get, "/pulls?state=open", 200, json!([]));
    let c = client(&http, &["github.com"]);
    assert_eq!(
        c.pull_request_for_branch(&github_repo(), "f").unwrap(),
        None
    );

    let http = Arc::new(FakeHttp::default());
    http.on(
        Method::Get,
        "/pulls",
        401,
        json!({"message": "Bad credentials"}),
    );
    let c = client(&http, &["github.com"]);
    assert!(matches!(
        c.pull_request_for_branch(&github_repo(), "f"),
        Err(ForgeError::Unauthorized { status: 401, .. })
    ));

    let http = Arc::new(FakeHttp::default());
    let c = client(&http, &[]);
    assert!(matches!(
        c.pull_request_for_branch(&github_repo(), "f"),
        Err(ForgeError::NotSignedIn { .. })
    ));
    assert!(http.seen().is_empty(), "no request without a token");
}

#[test]
fn github_create_and_link() {
    let http = Arc::new(FakeHttp::default());
    http.on(
        Method::Post,
        "/repos/octo/app/pulls",
        201,
        json!({"number": 5, "html_url": "https://github.com/octo/app/pull/5"}),
    );
    http.on(
        Method::Patch,
        "/repos/octo/app/pulls/5",
        200,
        json!({"number": 5}),
    );
    let c = client(&http, &["github.com"]);
    let draft = PullRequestDraft {
        source: "feat/x".into(),
        target: "main".into(),
        title: "Add x".into(),
        description: "Body".into(),
        draft: true,
    };
    let created = c.create_pull_request(&github_repo(), &draft).unwrap();
    assert_eq!(created.number, 5);
    c.set_pull_request_description(&github_repo(), 5, "Body\n\nlinks")
        .unwrap();
    let seen = http.seen();
    let body: serde_json::Value = serde_json::from_str(seen[0].body.as_deref().unwrap()).unwrap();
    assert_eq!(body["head"], "feat/x");
    assert_eq!(body["base"], "main");
    assert_eq!(body["draft"], true);
    let body: serde_json::Value = serde_json::from_str(seen[1].body.as_deref().unwrap()).unwrap();
    assert_eq!(body["body"], "Body\n\nlinks");
}

#[test]
fn github_directory_follows_pages() {
    let http = Arc::new(FakeHttp::default());
    http.on(Method::Get, "/user/orgs", 200, json!([{"login": "acme"}]));
    let repo = |n: &str| {
        json!({"full_name": format!("acme/{n}"), "name": n, "description": "",
               "clone_url": format!("https://github.com/acme/{n}.git"),
               "ssh_url": format!("git@github.com:acme/{n}.git"), "default_branch": "main"})
    };
    http.on(
        Method::Get,
        "/orgs/acme/repos?per_page=100&page=2",
        200,
        json!([repo("b")]),
    );
    http.on_with(
        Method::Get,
        "/orgs/acme/repos?per_page=100",
        200,
        json!([repo("a")]),
        &[(
            "link",
            "<https://api.github.com/orgs/acme/repos?per_page=100&page=2>; rel=\"next\"",
        )],
    );
    let c = client(&http, &["github.com"]);
    let acct = github_repo().account;
    let ns = c.namespaces(&acct).unwrap();
    assert_eq!(ns[0].kind, NamespaceKind::User);
    assert_eq!(ns[1].path, "acme");
    let repos = c.repositories(&acct, &ns[1]).unwrap();
    assert_eq!(
        repos.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
        ["a", "b"]
    );
    assert_eq!(repos[0].description, None, "empty description dropped");
    assert_eq!(repos[1].ssh_url, "git@github.com:acme/b.git");
}

#[test]
fn gitlab_merge_request_status() {
    let http = Arc::new(FakeHttp::default());
    let base = "/api/v4/projects/team%2Fsub%2Fapp/merge_requests";
    http.on(
        Method::Get,
        &format!("{base}?state=opened&per_page=1&source_branch=feat%2Fx"),
        200,
        json!([{"iid": 3, "title": "MR", "web_url": "w", "target_branch": "main"}]),
    );
    http.on(
        Method::Get,
        &format!("{base}/3/approvals"),
        200,
        json!({"approved_by": [{"user": {"username": "ann"}}]}),
    );
    http.on(
        Method::Get,
        &format!("{base}/3"),
        200,
        json!({"iid": 3, "title": "MR", "web_url": "w", "target_branch": "main", "draft": true,
               "detailed_merge_status": "mergeable", "head_pipeline": {"status": "failed"}}),
    );
    let c = client(&http, &["git.corp.example"]);
    let pr = c
        .pull_request_for_branch(&gitlab_repo(), "feat/x")
        .unwrap()
        .unwrap();
    assert_eq!(pr.number, 3);
    assert!(pr.draft);
    assert_eq!(pr.ci, Some(CiState::Failure));
    assert_eq!(pr.review, ReviewState::Approved);
    assert!(
        http.seen()[0]
            .headers
            .contains(&("PRIVATE-TOKEN".into(), "tok".into()))
    );
}

fn body(request: &HttpRequest) -> serde_json::Value {
    serde_json::from_str(request.body.as_deref().unwrap()).unwrap()
}

#[test]
fn github_lists_open_pull_requests_with_mergeability() {
    let http = Arc::new(FakeHttp::default());
    let pull = |n: u64, state: Option<&str>| {
        json!({
            "number": n, "title": format!("PR {n}"), "html_url": format!("u{n}"),
            "base": {"ref": "main", "sha": "b"},
            "head": {"ref": format!("f{n}"), "sha": format!("h{n}")},
            "user": {"login": "ann"}, "mergeable_state": state
        })
    };
    // Specific routes first: the fake matches URL substrings.
    http.on(Method::Get, "/pulls/1/reviews", 200, json!([]));
    http.on(Method::Get, "/pulls/2/reviews", 200, json!([]));
    http.on(Method::Get, "/pulls/1", 200, pull(1, Some("dirty")));
    http.on(Method::Get, "/pulls/2", 200, pull(2, Some("clean")));
    http.on(
        Method::Get,
        "/pulls?state=open",
        200,
        json!([pull(1, None), pull(2, None)]),
    );
    let c = client(&http, &["github.com"]);
    let prs = c.open_pull_requests(&github_repo()).unwrap();
    let got: Vec<_> = prs
        .iter()
        .map(|p| {
            (
                p.number,
                p.source.as_str(),
                p.head_sha.as_str(),
                p.merge.clone(),
            )
        })
        .collect();
    assert_eq!(
        got,
        [
            (1, "f1", "h1", MergeState::Conflicts),
            (2, "f2", "h2", MergeState::Mergeable)
        ]
    );
    assert_eq!(prs[0].author, "ann");
}

#[test]
fn github_merge_checks_the_head_and_deletes_the_branch() {
    let http = Arc::new(FakeHttp::default());
    http.on(
        Method::Get,
        "/pulls/42",
        200,
        json!({
            "number": 42, "title": "t", "html_url": "u",
            "base": {"ref": "main", "sha": "b"},
            "head": {"ref": "feat/x", "sha": "abc", "repo": {"full_name": "Octo/App"}}
        }),
    );
    http.on(Method::Put, "/pulls/42/merge", 200, json!({"merged": true}));
    http.on(Method::Delete, "/git/refs/heads/feat/x", 204, json!(null));
    let c = client(&http, &["github.com"]);
    let options = MergeOptions {
        method: MergeMethod::Squash,
        delete_source_branch: true,
        expected_head: Some("abc".into()),
    };
    let report = c.merge_pull_request(&github_repo(), 42, &options).unwrap();
    assert_eq!(report.warning, None);
    let seen = http.seen();
    let merge = seen.iter().find(|r| r.method == Method::Put).unwrap();
    assert_eq!(body(merge), json!({"merge_method": "squash", "sha": "abc"}));
    assert!(seen.iter().any(|r| r.method == Method::Delete
        && r.url == "https://api.github.com/repos/octo/app/git/refs/heads/feat/x"));
}

#[test]
fn github_merge_failures() {
    // The branch of a fork is left alone.
    let http = Arc::new(FakeHttp::default());
    http.on(
        Method::Get,
        "/pulls/42",
        200,
        json!({
            "number": 42, "title": "t", "html_url": "u",
            "base": {"ref": "main", "sha": "b"},
            "head": {"ref": "feat/x", "sha": "abc", "repo": {"full_name": "fork/app"}}
        }),
    );
    http.on(Method::Put, "/pulls/42/merge", 200, json!({"merged": true}));
    let c = client(&http, &["github.com"]);
    let options = MergeOptions {
        delete_source_branch: true,
        ..MergeOptions::default()
    };
    c.merge_pull_request(&github_repo(), 42, &options).unwrap();
    assert!(!http.seen().iter().any(|r| r.method == Method::Delete));

    // A refused merge is a permission problem, not a bad token; a failed
    // branch deletion is a warning, the merge happened.
    let http = Arc::new(FakeHttp::default());
    http.on(
        Method::Get,
        "/pulls/",
        200,
        json!({
            "number": 42, "title": "t", "html_url": "u",
            "base": {"ref": "main", "sha": "b"},
            "head": {"ref": "feat/x", "sha": "abc", "repo": {"full_name": "octo/app"}}
        }),
    );
    http.on(
        Method::Put,
        "/pulls/43/merge",
        403,
        json!({"message": "no"}),
    );
    http.on(Method::Put, "/pulls/42/merge", 200, json!({"merged": true}));
    http.on(
        Method::Delete,
        "/git/refs/",
        422,
        json!({"message": "protected"}),
    );
    let c = client(&http, &["github.com"]);
    let err = c
        .merge_pull_request(&github_repo(), 43, &MergeOptions::default())
        .unwrap_err();
    assert!(
        matches!(&err, ForgeError::Http { status: 403, message, .. } if message.contains("not allowed")),
        "{err:?}"
    );
    let report = c.merge_pull_request(&github_repo(), 42, &options).unwrap();
    assert!(report.warning.unwrap().contains("protected"));
}

#[test]
fn gitlab_lists_and_merges_merge_requests() {
    let http = Arc::new(FakeHttp::default());
    let base = "/api/v4/projects/team%2Fsub%2Fapp/merge_requests";
    http.on(
        Method::Put,
        &format!("{base}/3/merge"),
        200,
        json!({"state": "merged"}),
    );
    http.on(
        Method::Get,
        &format!("{base}/3/approvals"),
        200,
        json!({"approved_by": []}),
    );
    http.on(
        Method::Get,
        &format!("{base}/3"),
        200,
        json!({"iid": 3, "title": "MR", "web_url": "w", "source_branch": "feat/x",
               "target_branch": "main", "sha": "abc", "author": {"username": "ann"},
               "detailed_merge_status": "ci_must_pass", "head_pipeline": {"status": "running"}}),
    );
    http.on(
        Method::Get,
        &format!("{base}?state=opened"),
        200,
        json!([{"iid": 3, "title": "MR", "web_url": "w", "target_branch": "main"}]),
    );
    let c = client(&http, &["git.corp.example"]);
    let prs = c.open_pull_requests(&gitlab_repo()).unwrap();
    assert_eq!(prs.len(), 1);
    assert_eq!(
        (
            prs[0].source.as_str(),
            prs[0].head_sha.as_str(),
            prs[0].author.as_str()
        ),
        ("feat/x", "abc", "ann")
    );
    assert_eq!(prs[0].merge, MergeState::Blocked("CI must pass".into()));
    assert_eq!(prs[0].ci, Some(CiState::Pending));

    let options = MergeOptions {
        method: MergeMethod::Squash,
        delete_source_branch: true,
        expected_head: Some("abc".into()),
    };
    c.merge_pull_request(&gitlab_repo(), 3, &options).unwrap();
    let merge = http
        .seen()
        .into_iter()
        .find(|r| r.method == Method::Put)
        .unwrap();
    assert_eq!(
        body(&merge),
        json!({"squash": true, "should_remove_source_branch": true, "sha": "abc"})
    );

    // GitLab rebases per project, not per merge.
    let rebase = MergeOptions {
        method: MergeMethod::Rebase,
        ..MergeOptions::default()
    };
    let err = c
        .merge_pull_request(&gitlab_repo(), 3, &rebase)
        .unwrap_err();
    assert!(matches!(err, ForgeError::Unsupported(_)), "{err:?}");
}

#[test]
fn gitlab_create_draft_and_groups() {
    let http = Arc::new(FakeHttp::default());
    http.on(
        Method::Post,
        "/projects/team%2Fsub%2Fapp/merge_requests",
        201,
        json!({"iid": 9, "web_url": "w9"}),
    );
    http.on(
        Method::Get,
        "/groups?",
        200,
        json!([{"full_path": "team/sub"}]),
    );
    http.on(
        Method::Get,
        "/groups/team%2Fsub/projects",
        200,
        json!([{"path_with_namespace": "team/sub/app", "path": "app", "description": "An app",
                "http_url_to_repo": "https://h/team/sub/app.git", "ssh_url_to_repo": "git@h:team/sub/app.git",
                "default_branch": "main", "archived": true}]),
    );
    let c = client(&http, &["git.corp.example"]);
    let created = c
        .create_pull_request(
            &gitlab_repo(),
            &PullRequestDraft {
                source: "f".into(),
                target: "main".into(),
                title: "T".into(),
                description: String::new(),
                draft: true,
            },
        )
        .unwrap();
    assert_eq!(created.number, 9);
    let body: serde_json::Value =
        serde_json::from_str(http.seen()[0].body.as_deref().unwrap()).unwrap();
    assert_eq!(body["title"], "Draft: T");
    assert_eq!(body["source_branch"], "f");
    let acct = gitlab_repo().account;
    let ns = c.namespaces(&acct).unwrap();
    assert_eq!(ns[1].kind, NamespaceKind::Group);
    let repos = c
        .repositories(
            &acct,
            &Namespace {
                path: "team/sub".into(),
                kind: NamespaceKind::Group,
            },
        )
        .unwrap();
    assert!(repos[0].archived);
    assert_eq!(repos[0].path, "team/sub/app");
}

#[test]
fn sign_in_detects_the_provider() {
    // Neutral host name: GitHub's API path is tried, then GitLab's answers.
    let http = Arc::new(FakeHttp::default());
    http.on(
        Method::Get,
        "https://git.corp.example/api/v4/user",
        200,
        json!({"username": "me"}),
    );
    let c = client(&http, &[]);
    let account = c
        .verify("https://Git.Corp.Example/", None, &ForgeToken::new(" t "))
        .unwrap();
    assert_eq!(account.kind, ForgeKind::gitlab());
    assert_eq!(account.host, "git.corp.example");
    assert_eq!(account.api_url, "https://git.corp.example/api/v4");
    assert_eq!(account.user, "me");

    // github.com is recognized by name: one request.
    let http = Arc::new(FakeHttp::default());
    http.on(
        Method::Get,
        "https://api.github.com/user",
        200,
        json!({"login": "octo"}),
    );
    let c = client(&http, &[]);
    let account = c.verify("github.com", None, &ForgeToken::new("t")).unwrap();
    assert_eq!(account.kind, ForgeKind::github());
    assert_eq!(http.seen().len(), 1);

    // A refused token stops detection.
    let http = Arc::new(FakeHttp::default());
    http.on(Method::Get, "api.github.com/user", 401, json!({}));
    let c = client(&http, &[]);
    assert!(matches!(
        c.verify("github.com", None, &ForgeToken::new("bad")),
        Err(ForgeError::Unauthorized { .. })
    ));
    assert_eq!(http.seen().len(), 1);

    // Nothing answers: unknown forge; unreachable: network error.
    let c = client(&Arc::new(FakeHttp::default()), &[]);
    assert!(matches!(
        c.verify("example.org", None, &ForgeToken::new("t")),
        Err(ForgeError::UnknownForge(_))
    ));
    assert!(matches!(
        c.verify("unreachable.example", None, &ForgeToken::new("t")),
        Err(ForgeError::Network { .. })
    ));
    assert_eq!(c.kinds(), [ForgeKind::github(), ForgeKind::gitlab()]);
}

#[test]
fn errors_never_contain_the_token() {
    let http = Arc::new(FakeHttp::default());
    http.on(Method::Get, "/pulls", 500, json!({"message": "boom"}));
    let c = client(&http, &["github.com"]);
    let err = c
        .pull_request_for_branch(&github_repo(), "f")
        .unwrap_err()
        .to_string();
    assert!(err.contains("boom") && !err.contains("tok"), "{err}");
}

#[test]
fn github_branch_ci_lists_check_runs_and_statuses() {
    let http = Arc::new(FakeHttp::default());
    http.on(
        Method::Get,
        "/repos/octo/app/branches/feat%2Fx",
        200,
        json!({"name": "feat/x", "commit": {"sha": "abc123"}}),
    );
    http.on(
        Method::Get,
        "/commits/abc123/check-runs",
        200,
        json!({"check_runs": [
            {"name": "build", "status": "completed", "conclusion": "success", "html_url": "hb"},
            {"name": "test", "status": "in_progress", "conclusion": null}
        ]}),
    );
    http.on(
        Method::Get,
        "/commits/abc123/status",
        200,
        json!({"state": "failure", "total_count": 1,
               "statuses": [{"context": "ci/legacy", "state": "error", "target_url": "hl"}]}),
    );
    let c = client(&http, &["github.com"]);
    let run = c.ci_for_branch(&github_repo(), "feat/x").unwrap().unwrap();
    assert_eq!(run.state, CiState::Failure, "a failed status wins");
    assert_eq!(run.sha, "abc123");
    assert_eq!(
        run.url.as_deref(),
        Some("https://github.com/octo/app/commit/abc123")
    );
    let jobs: Vec<_> = run
        .jobs
        .iter()
        .map(|j| (j.name.as_str(), j.state))
        .collect();
    assert_eq!(
        jobs,
        [
            ("build", CiJobState::Success),
            ("test", CiJobState::Running),
            ("ci/legacy", CiJobState::Failed)
        ]
    );
}

#[test]
fn github_branch_ci_absent() {
    let http = Arc::new(FakeHttp::default());
    let c = client(&http, &["github.com"]);
    // Branch not pushed: 404.
    assert_eq!(c.ci_for_branch(&github_repo(), "local").unwrap(), None);
    // Pushed, no checks at all.
    http.on(
        Method::Get,
        "/branches/main",
        200,
        json!({"commit": {"sha": "s"}}),
    );
    http.on(Method::Get, "/check-runs", 200, json!({"check_runs": []}));
    http.on(
        Method::Get,
        "/status",
        200,
        json!({"state": "pending", "total_count": 0, "statuses": []}),
    );
    assert_eq!(c.ci_for_branch(&github_repo(), "main").unwrap(), None);
}

#[test]
fn gitlab_branch_ci_lists_pipeline_jobs() {
    let http = Arc::new(FakeHttp::default());
    let base = "/projects/team%2Fsub%2Fapp/pipelines";
    http.on(
        Method::Get,
        &format!("{base}?ref=feat%2Fx&per_page=1"),
        200,
        json!([{"id": 9, "status": "running", "sha": "def", "web_url": "pw"}]),
    );
    // Newest job first, as GitLab returns them.
    http.on(
        Method::Get,
        &format!("{base}/9/jobs"),
        200,
        json!([
            {"name": "deploy", "stage": "deploy", "status": "manual", "web_url": "j3"},
            {"name": "lint", "stage": "test", "status": "failed", "allow_failure": true, "web_url": "j2"},
            {"name": "build", "stage": "build", "status": "success", "web_url": "j1"}
        ]),
    );
    let c = client(&http, &["git.corp.example"]);
    let run = c.ci_for_branch(&gitlab_repo(), "feat/x").unwrap().unwrap();
    assert_eq!(run.state, CiState::Pending);
    assert_eq!((run.sha.as_str(), run.url.as_deref()), ("def", Some("pw")));
    let jobs: Vec<_> = run
        .jobs
        .iter()
        .map(|j| (j.name.as_str(), j.stage.as_deref(), j.state))
        .collect();
    assert_eq!(
        jobs,
        [
            ("build", Some("build"), CiJobState::Success),
            ("lint", Some("test"), CiJobState::Skipped),
            ("deploy", Some("deploy"), CiJobState::Manual)
        ]
    );
    // No pipeline on the branch.
    http.on(Method::Get, &format!("{base}?ref=other"), 200, json!([]));
    assert_eq!(c.ci_for_branch(&gitlab_repo(), "other").unwrap(), None);
}

#[test]
fn github_pull_request_ci_matches_the_branch_ci() {
    let http = Arc::new(FakeHttp::default());
    http.on(
        Method::Get,
        "/pulls?state=open",
        200,
        json!([{
            "number": 7, "title": "t", "html_url": "u", "base": {"ref": "dev", "sha": "b"},
            "head": {"ref": "f", "sha": "s1"}
        }]),
    );
    http.on(
        Method::Get,
        "/branches/f",
        200,
        json!({"commit": {"sha": "s1"}}),
    );
    // A stale run is skipped, not failed, in both columns.
    http.on(
        Method::Get,
        "/check-runs",
        200,
        json!({"check_runs": [
            {"name": "build", "status": "completed", "conclusion": "success"},
            {"name": "old", "status": "completed", "conclusion": "stale"}
        ]}),
    );
    let c = client(&http, &["github.com"]);
    let pr = c
        .pull_request_for_branch(&github_repo(), "f")
        .unwrap()
        .unwrap();
    let ci = c.ci_for_branch(&github_repo(), "f").unwrap().unwrap();
    assert_eq!(pr.ci, Some(ci.state));
    assert_eq!(ci.state, CiState::Success);
}

#[test]
fn github_server_errors_are_not_hidden_as_missing_scopes() {
    let http = Arc::new(FakeHttp::default());
    http.on(
        Method::Get,
        "/pulls?state=open",
        200,
        json!([{
            "number": 7, "title": "t", "html_url": "u", "base": {"ref": "dev", "sha": "b"},
            "head": {"ref": "f", "sha": "s1"}
        }]),
    );
    http.on(
        Method::Get,
        "/check-runs",
        502,
        json!({"message": "bad gateway"}),
    );
    let c = client(&http, &["github.com"]);
    assert!(matches!(
        c.pull_request_for_branch(&github_repo(), "f"),
        Err(ForgeError::Http { status: 502, .. })
    ));
}

#[test]
fn github_rate_limit_is_not_a_bad_token() {
    let http = Arc::new(FakeHttp::default());
    http.on_with(
        Method::Get,
        "/pulls",
        403,
        json!({"message": "API rate limit exceeded"}),
        &[("x-ratelimit-remaining", "0")],
    );
    let c = client(&http, &["github.com"]);
    assert!(matches!(
        c.pull_request_for_branch(&github_repo(), "f"),
        Err(ForgeError::RateLimited { .. })
    ));
}

#[test]
fn github_secondary_rate_limit_without_headers_is_not_a_bad_token() {
    let http = Arc::new(FakeHttp::default());
    http.on(
        Method::Get,
        "/pulls",
        403,
        json!({"message": "You have exceeded a secondary rate limit. Please wait a few minutes before you try again."}),
    );
    let c = client(&http, &["github.com"]);
    assert!(matches!(
        c.pull_request_for_branch(&github_repo(), "f"),
        Err(ForgeError::RateLimited { .. })
    ));
}

#[test]
fn pages_on_another_host_are_not_followed() {
    let http = Arc::new(FakeHttp::default());
    http.on(Method::Get, "/user/orgs", 200, json!([]));
    http.on_with(
        Method::Get,
        "/user/repos",
        200,
        json!([]),
        &[("link", "<https://evil.example/steal?page=2>; rel=\"next\"")],
    );
    let c = client(&http, &["github.com"]);
    let acct = github_repo().account;
    let ns = c.namespaces(&acct).unwrap();
    c.repositories(&acct, &ns[0]).unwrap();
    assert!(http.seen().iter().all(|r| !r.url.contains("evil.example")));
}

fn failed_job(id: Option<u64>, name: &str) -> CiJob {
    CiJob {
        id,
        name: name.into(),
        stage: None,
        state: CiJobState::Failed,
        url: Some(format!("web/{name}")),
    }
}

#[test]
fn github_actions_jobs_carry_their_id() {
    let http = Arc::new(FakeHttp::default());
    http.on(
        Method::Get,
        "/branches/main",
        200,
        json!({"commit": {"sha": "abc"}}),
    );
    http.on(
        Method::Get,
        "/commits/abc/check-runs",
        200,
        json!({"check_runs": [
            {"id": 11, "app": {"slug": "github-actions"}, "name": "test",
             "status": "completed", "conclusion": "failure"},
            {"id": 12, "app": {"slug": "circleci-checks"}, "name": "circle",
             "status": "completed", "conclusion": "failure"}
        ]}),
    );
    let c = client(&http, &["github.com"]);
    let run = c.ci_for_branch(&github_repo(), "main").unwrap().unwrap();
    let ids: Vec<_> = run.jobs.iter().map(|j| j.id).collect();
    assert_eq!(ids, [Some(11), None], "only Actions jobs have a log here");
    assert_eq!(run.id, None);
}

#[test]
fn github_job_log_excerpt() {
    let http = Arc::new(FakeHttp::default());
    http.on_text(
        Method::Get,
        "/repos/octo/app/actions/jobs/11/logs",
        200,
        "2024-05-01T10:00:00.0000000Z ##[group]Run cargo test\n\
         2024-05-01T10:00:01.0000000Z running 1 test\n\
         2024-05-01T10:00:02.0000000Z test it_works ... FAILED\n\
         2024-05-01T10:00:03.0000000Z ##[error]Process completed with exit code 101.\n",
    );
    let c = client(&http, &["github.com"]);
    let log = c
        .ci_job_log(&github_repo(), &failed_job(Some(11), "test"))
        .unwrap();
    assert!(log.matched);
    assert_eq!(
        log.excerpt,
        [
            "Run cargo test",
            "running 1 test",
            "test it_works ... FAILED",
            "Error: Process completed with exit code 101."
        ]
    );
    assert_eq!(log.url.as_deref(), Some("web/test"));
    // A check posted by another CI has no log here.
    assert!(matches!(
        c.ci_job_log(&github_repo(), &failed_job(None, "circle")),
        Err(ForgeError::Unsupported(_))
    ));
    // A token without Actions access is told what to add.
    http.on(
        Method::Get,
        "/actions/jobs/12/logs",
        403,
        json!({"message": "Resource not accessible by personal access token"}),
    );
    let e = c
        .ci_job_log(&github_repo(), &failed_job(Some(12), "test"))
        .unwrap_err()
        .to_string();
    assert!(e.contains("Actions: read"), "{e}");
}

#[test]
fn github_reruns_each_failed_workflow_run_once() {
    let http = Arc::new(FakeHttp::default());
    http.on(Method::Get, "/actions/jobs/11", 200, json!({"run_id": 7}));
    http.on(Method::Get, "/actions/jobs/12", 200, json!({"run_id": 7}));
    http.on(Method::Get, "/actions/jobs/13", 200, json!({"run_id": 8}));
    http.on_text(Method::Post, "/rerun-failed-jobs", 201, "");
    let c = client(&http, &["github.com"]);
    let run = CiRun {
        id: None,
        state: CiState::Failure,
        sha: "abc".into(),
        url: None,
        jobs: vec![
            failed_job(Some(11), "a"),
            failed_job(Some(12), "b"),
            failed_job(Some(13), "c"),
            failed_job(None, "legacy"),
        ],
    };
    assert_eq!(c.rerun_failed_ci(&github_repo(), &run).unwrap(), 2);
    let posts: Vec<String> = http
        .seen()
        .into_iter()
        .filter(|r| r.method == Method::Post)
        .map(|r| r.url)
        .collect();
    assert_eq!(
        posts,
        [
            "https://api.github.com/repos/octo/app/actions/runs/7/rerun-failed-jobs",
            "https://api.github.com/repos/octo/app/actions/runs/8/rerun-failed-jobs"
        ]
    );
    // Nothing GitHub Actions can rerun.
    let other = CiRun {
        jobs: vec![failed_job(None, "legacy")],
        ..run
    };
    assert!(matches!(
        c.rerun_failed_ci(&github_repo(), &other),
        Err(ForgeError::Unsupported(_))
    ));
}

#[test]
fn github_rerun_without_write_access_names_the_scope() {
    let http = Arc::new(FakeHttp::default());
    http.on(Method::Get, "/actions/jobs/11", 200, json!({"run_id": 7}));
    http.on(
        Method::Post,
        "/rerun-failed-jobs",
        403,
        json!({"message": "Resource not accessible by personal access token"}),
    );
    let c = client(&http, &["github.com"]);
    let run = CiRun {
        id: None,
        state: CiState::Failure,
        sha: "abc".into(),
        url: None,
        jobs: vec![failed_job(Some(11), "a")],
    };
    let e = c
        .rerun_failed_ci(&github_repo(), &run)
        .unwrap_err()
        .to_string();
    assert!(e.contains("not allowed to rerun the CI"), "{e}");
    assert!(e.contains("Actions: read and write"), "{e}");
}

#[test]
fn gitlab_job_log_and_pipeline_retry() {
    let http = Arc::new(FakeHttp::default());
    let base = "/projects/team%2Fsub%2Fapp";
    http.on(
        Method::Get,
        &format!("{base}/pipelines?ref=main"),
        200,
        json!([{"id": 9, "status": "failed", "sha": "def", "web_url": "pw"}]),
    );
    http.on(
        Method::Get,
        &format!("{base}/pipelines/9/jobs"),
        200,
        json!([{"id": 31, "name": "test", "stage": "test", "status": "failed", "web_url": "j"}]),
    );
    http.on_text(
        Method::Get,
        &format!("{base}/jobs/31/trace"),
        200,
        "section_start:1700000000:script\r\u{1b}[0K\u{1b}[32;1m$ make test\u{1b}[0;m\n\
         ok 1\n\
         \u{1b}[31mERROR: Job failed: exit code 2\u{1b}[0m\n\
         section_end:1700000000:script\r\u{1b}[0K\n",
    );
    http.on(
        Method::Post,
        &format!("{base}/pipelines/9/retry"),
        201,
        json!({"id": 9}),
    );
    let c = client(&http, &["git.corp.example"]);
    let run = c.ci_for_branch(&gitlab_repo(), "main").unwrap().unwrap();
    assert_eq!((run.id, run.jobs[0].id), (Some(9), Some(31)));
    let log = c.ci_job_log(&gitlab_repo(), &run.jobs[0]).unwrap();
    assert_eq!(
        log.excerpt,
        ["$ make test", "ok 1", "ERROR: Job failed: exit code 2"]
    );
    assert_eq!(c.rerun_failed_ci(&gitlab_repo(), &run).unwrap(), 1);
    // A read-only token is told what to add.
    http.routes.lock().unwrap().insert(
        0,
        (
            Method::Post,
            "/retry".into(),
            HttpResponse {
                status: 403,
                body: "{\"message\":\"403 Forbidden\"}".into(),
                ..HttpResponse::default()
            },
        ),
    );
    let e = c
        .rerun_failed_ci(&gitlab_repo(), &run)
        .unwrap_err()
        .to_string();
    assert!(e.contains("api scope"), "{e}");
}

fn new_repo(private: bool) -> NewRemoteRepo {
    NewRemoteRepo {
        name: "tool".into(),
        description: Some("A tool".into()),
        private,
    }
}

#[test]
fn github_creates_repositories_for_the_user_or_an_organization() {
    let http = Arc::new(FakeHttp::default());
    let created = |full: &str| {
        json!({"full_name": full, "name": "tool", "description": "A tool",
               "clone_url": format!("https://github.com/{full}.git"),
               "ssh_url": format!("git@github.com:{full}.git")})
    };
    http.on(Method::Post, "/user/repos", 201, created("me/tool"));
    http.on(Method::Post, "/orgs/acme/repos", 201, created("acme/tool"));
    let c = client(&http, &["github.com"]);
    let acct = account("github.com", ForgeKind::github(), "https://api.github.com");
    let user = Namespace {
        path: "me".into(),
        kind: NamespaceKind::User,
    };
    let repo = c.create_repository(&acct, &user, &new_repo(true)).unwrap();
    assert_eq!(repo.path, "me/tool");
    assert_eq!(repo.https_url, "https://github.com/me/tool.git");
    let sent = http.seen();
    assert_eq!(sent[0].url, "https://api.github.com/user/repos");
    assert_eq!(
        body(&sent[0]),
        json!({"name": "tool", "description": "A tool", "private": true, "auto_init": false})
    );

    let org = Namespace {
        path: "acme".into(),
        kind: NamespaceKind::Organization,
    };
    let repo = c.create_repository(&acct, &org, &new_repo(false)).unwrap();
    assert_eq!(repo.ssh_url, "git@github.com:acme/tool.git");
    assert_eq!(body(&http.seen()[1])["private"], json!(false));
}

#[test]
fn github_repository_creation_failures_say_why() {
    let http = Arc::new(FakeHttp::default());
    http.on(
        Method::Post,
        "/user/repos",
        422,
        json!({"message": "Repository creation failed.",
               "errors": [{"resource": "Repository", "code": "custom", "field": "name",
                           "message": "name already exists on this account"}]}),
    );
    http.on(
        Method::Post,
        "/orgs/acme/repos",
        403,
        json!({"message": "Forbidden"}),
    );
    let c = client(&http, &["github.com"]);
    let acct = account("github.com", ForgeKind::github(), "https://api.github.com");
    let user = Namespace {
        path: "me".into(),
        kind: NamespaceKind::User,
    };
    let err = c
        .create_repository(&acct, &user, &new_repo(true))
        .unwrap_err();
    assert!(err.to_string().contains("name already exists"), "{err}");
    let org = Namespace {
        path: "acme".into(),
        kind: NamespaceKind::Organization,
    };
    let err = c
        .create_repository(&acct, &org, &new_repo(true))
        .unwrap_err();
    assert!(err.to_string().contains("Administration"), "{err}");
}

#[test]
fn gitlab_creates_projects_in_the_user_namespace_or_a_group() {
    let http = Arc::new(FakeHttp::default());
    http.on(
        Method::Get,
        "/namespaces/grp%2Fsub",
        200,
        json!({"id": 42, "full_path": "grp/sub"}),
    );
    http.on(
        Method::Post,
        "/projects",
        201,
        json!({"path_with_namespace": "grp/sub/tool", "path": "tool",
               "http_url_to_repo": "https://gitlab.com/grp/sub/tool.git",
               "ssh_url_to_repo": "git@gitlab.com:grp/sub/tool.git"}),
    );
    let c = client(&http, &["gitlab.com"]);
    let acct = account(
        "gitlab.com",
        ForgeKind::gitlab(),
        "https://gitlab.com/api/v4",
    );
    let user = Namespace {
        path: "me".into(),
        kind: NamespaceKind::User,
    };
    c.create_repository(&acct, &user, &new_repo(true)).unwrap();
    let sent = http.seen();
    assert_eq!(sent.len(), 1, "no namespace lookup for the user's own");
    assert_eq!(
        body(&sent[0]),
        json!({"name": "tool", "path": "tool", "description": "A tool",
               "visibility": "private", "initialize_with_readme": false})
    );

    let group = Namespace {
        path: "grp/sub".into(),
        kind: NamespaceKind::Group,
    };
    let repo = c
        .create_repository(&acct, &group, &new_repo(false))
        .unwrap();
    assert_eq!(repo.path, "grp/sub/tool");
    let sent = http.seen();
    let created = body(&sent[2]);
    assert_eq!(created["namespace_id"], json!(42));
    assert_eq!(created["visibility"], json!("public"));
}

#[test]
fn github_arms_and_disarms_auto_merge_through_graphql() {
    let http = Arc::new(FakeHttp::default());
    http.on(
        Method::Get,
        "/pulls/42",
        200,
        json!({
            "number": 42, "title": "t", "html_url": "u", "node_id": "PR_42",
            "base": {"ref": "main", "sha": "b"},
            "head": {"ref": "feat/x", "sha": "abc", "repo": {"full_name": "octo/app"}},
            "auto_merge": {"merge_method": "squash"}
        }),
    );
    http.on(
        Method::Get,
        "/repos/octo/app",
        200,
        json!({"delete_branch_on_merge": false}),
    );
    http.on(Method::Post, "/graphql", 200, json!({"data": {}}));
    let c = client(&http, &["github.com"]);
    let options = MergeOptions {
        method: MergeMethod::Squash,
        delete_source_branch: true,
        expected_head: Some("abc".into()),
    };
    let report = c.enable_auto_merge(&github_repo(), 42, &options).unwrap();
    assert!(!report.merged_now);
    // The repository does not delete merged branches: said, not hidden.
    assert!(
        report
            .warning
            .as_deref()
            .is_some_and(|w| w.contains("feat/x")),
        "{report:?}"
    );
    let seen = http.seen();
    let arm = seen.iter().find(|r| r.method == Method::Post).unwrap();
    assert_eq!(arm.url, "https://api.github.com/graphql");
    let sent = body(arm);
    assert!(
        sent["query"]
            .as_str()
            .unwrap()
            .contains("enablePullRequestAutoMerge")
    );
    assert_eq!(
        sent["variables"],
        json!({"id": "PR_42", "method": "SQUASH", "head": "abc"})
    );

    c.disable_auto_merge(&github_repo(), 42).unwrap();
    let disarm = http
        .seen()
        .into_iter()
        .filter(|r| r.method == Method::Post)
        .nth(1)
        .unwrap();
    assert!(
        body(&disarm)["query"]
            .as_str()
            .unwrap()
            .contains("disablePullRequestAutoMerge")
    );
}

#[test]
fn github_pull_request_reads_its_auto_merge() {
    let http = Arc::new(FakeHttp::default());
    http.on(
        Method::Get,
        "/pulls?state=open",
        200,
        json!([{
            "number": 42, "title": "t", "html_url": "u",
            "base": {"ref": "main", "sha": "b"}, "head": {"ref": "feat/x", "sha": "abc"},
            "auto_merge": {"merge_method": "merge", "enabled_by": {"login": "me"}}
        }]),
    );
    let c = client(&http, &["github.com"]);
    let pr = c
        .pull_request_for_branch(&github_repo(), "feat/x")
        .unwrap()
        .unwrap();
    assert!(pr.auto_merge);
}

#[test]
fn github_auto_merge_refusals_are_unsupported() {
    let http = Arc::new(FakeHttp::default());
    http.on(
        Method::Get,
        "/pulls/42",
        200,
        json!({
            "number": 42, "title": "t", "html_url": "u", "node_id": "PR_42",
            "base": {"ref": "main", "sha": "b"},
            "head": {"ref": "feat/x", "sha": "abc", "repo": {"full_name": "octo/app"}}
        }),
    );
    http.on(
        Method::Post,
        "/api/graphql",
        200,
        json!({"errors": [{"type": "UNPROCESSABLE",
                           "message": "Auto merge is not allowed for this repository"}]}),
    );
    let c = client(&http, &["ghe.corp.example"]);
    let repo = ForgeRepo {
        account: account(
            "ghe.corp.example",
            ForgeKind::github(),
            "https://ghe.corp.example/api/v3",
        ),
        path: "octo/app".into(),
    };
    let err = c
        .enable_auto_merge(&repo, 42, &MergeOptions::default())
        .unwrap_err();
    assert_eq!(
        err,
        ForgeError::Unsupported("Auto merge is not allowed for this repository".into())
    );
    // GitHub Enterprise's GraphQL endpoint is next to its REST API.
    assert!(
        http.seen()
            .iter()
            .any(|r| r.url == "https://ghe.corp.example/api/graphql")
    );
}

#[test]
fn gitlab_arms_and_cancels_merge_when_pipeline_succeeds() {
    let http = Arc::new(FakeHttp::default());
    let base = "/api/v4/projects/team%2Fsub%2Fapp/merge_requests";
    http.on(
        Method::Put,
        &format!("{base}/3/merge"),
        200,
        json!({"iid": 3, "title": "MR", "web_url": "w", "target_branch": "main",
               "state": "opened", "merge_when_pipeline_succeeds": true}),
    );
    http.on(
        Method::Post,
        &format!("{base}/3/cancel_merge_when_pipeline_succeeds"),
        200,
        json!({"iid": 3}),
    );
    let c = client(&http, &["git.corp.example"]);
    let options = MergeOptions {
        method: MergeMethod::Merge,
        delete_source_branch: true,
        expected_head: Some("abc".into()),
    };
    let report = c.enable_auto_merge(&gitlab_repo(), 3, &options).unwrap();
    assert!(!report.merged_now);
    let arm = http
        .seen()
        .into_iter()
        .find(|r| r.method == Method::Put)
        .unwrap();
    assert_eq!(
        body(&arm),
        json!({"squash": false, "should_remove_source_branch": true, "sha": "abc",
               "merge_when_pipeline_succeeds": true, "auto_merge": true})
    );
    c.disable_auto_merge(&gitlab_repo(), 3).unwrap();
    assert!(http.seen().iter().any(|r| {
        r.method == Method::Post
            && r.url
                .ends_with("/merge_requests/3/cancel_merge_when_pipeline_succeeds")
    }));
}

#[test]
fn gitlab_auto_merge_with_a_passed_pipeline_merges_at_once() {
    let http = Arc::new(FakeHttp::default());
    http.on(
        Method::Put,
        "/merge_requests/3/merge",
        200,
        json!({"iid": 3, "title": "MR", "web_url": "w", "target_branch": "main",
               "state": "merged"}),
    );
    let c = client(&http, &["git.corp.example"]);
    let report = c
        .enable_auto_merge(&gitlab_repo(), 3, &MergeOptions::default())
        .unwrap();
    assert!(report.merged_now);
}

#[test]
fn gitlab_merge_request_reads_its_auto_merge() {
    let http = Arc::new(FakeHttp::default());
    let base = "/api/v4/projects/team%2Fsub%2Fapp/merge_requests";
    http.on(
        Method::Get,
        &format!("{base}?state=opened"),
        200,
        json!([{"iid": 3, "title": "MR", "web_url": "w", "target_branch": "main"}]),
    );
    http.on(
        Method::Get,
        &format!("{base}/3"),
        200,
        json!({"iid": 3, "title": "MR", "web_url": "w", "source_branch": "feat/x",
               "target_branch": "main", "merge_when_pipeline_succeeds": true}),
    );
    let c = client(&http, &["git.corp.example"]);
    let prs = c.open_pull_requests(&gitlab_repo()).unwrap();
    assert!(prs[0].auto_merge);
}

fn issue_draft() -> IssueDraft {
    IssueDraft {
        title: "Crash on start".into(),
        description: "Steps…".into(),
        labels: vec!["bug".into(), "ui".into()],
        assignees: vec!["ann".into()],
    }
}

#[test]
fn github_lists_open_issues_without_pull_requests() {
    let http = Arc::new(FakeHttp::default());
    http.on(
        Method::Get,
        "/repos/octo/app/issues?state=open",
        200,
        json!([
            {"number": 3, "title": "Bug", "html_url": "https://github.com/octo/app/issues/3",
             "user": {"login": "ann"}, "labels": [{"name": "bug"}],
             "assignees": [{"login": "bob"}], "created_at": "1970-01-02T00:00:00Z"},
            {"number": 4, "title": "A PR", "html_url": "https://github.com/octo/app/pull/4",
             "pull_request": {"url": "x"}}
        ]),
    );
    let c = client(&http, &["github.com"]);
    let issues = c.open_issues(&github_repo()).unwrap();
    assert_eq!(issues.len(), 1);
    let i = &issues[0];
    assert_eq!(
        (i.number, i.title.as_str(), i.author.as_str()),
        (3, "Bug", "ann")
    );
    assert_eq!(i.labels, vec!["bug".to_owned()]);
    assert_eq!(i.assignees, vec!["bob".to_owned()]);
    assert_eq!(i.created_at, Some(archipel_model::Timestamp(86_400)));
}

#[test]
fn github_issue_choices_and_disabled_issues() {
    let http = Arc::new(FakeHttp::default());
    http.on(
        Method::Get,
        "/repos/octo/app/labels",
        200,
        json!([{"name": "ui"}, {"name": "Bug"}]),
    );
    // A token without push access cannot list assignees: none then.
    http.on(Method::Get, "/repos/octo/app/assignees", 403, json!({}));
    http.on(
        Method::Get,
        "/repos/octo/app",
        200,
        json!({"has_issues": true}),
    );
    http.on(
        Method::Get,
        "/repos/octo/off",
        200,
        json!({"has_issues": false}),
    );
    let c = client(&http, &["github.com"]);
    let choices = c.issue_choices(&github_repo()).unwrap();
    assert_eq!(choices.labels, vec!["Bug".to_owned(), "ui".to_owned()]);
    assert!(choices.assignees.is_empty());
    let off = ForgeRepo {
        path: "octo/off".into(),
        ..github_repo()
    };
    let e = c.issue_choices(&off).unwrap_err();
    assert!(matches!(e, ForgeError::Unsupported(_)), "{e:?}");
    assert!(
        e.to_string().contains("issues are disabled on octo/off"),
        "{e}"
    );
}

#[test]
fn github_creates_closes_and_reopens_issues() {
    let http = Arc::new(FakeHttp::default());
    http.on(
        Method::Post,
        "/repos/octo/app/issues/7/comments",
        201,
        json!({}),
    );
    http.on(
        Method::Post,
        "/repos/octo/app/issues",
        201,
        json!({"number": 7, "html_url": "https://github.com/octo/app/issues/7"}),
    );
    http.on(Method::Patch, "/repos/octo/app/issues/7", 200, json!({}));
    let c = client(&http, &["github.com"]);
    let created = c.create_issue(&github_repo(), &issue_draft()).unwrap();
    assert_eq!(created.number, 7);
    assert_eq!(created.url, "https://github.com/octo/app/issues/7");
    c.close_issue(
        &github_repo(),
        7,
        CloseReason::NotPlanned,
        Some("Duplicate of #2"),
    )
    .unwrap();
    c.reopen_issue(&github_repo(), 7).unwrap();
    let seen = http.seen();
    let sent: Vec<_> = seen.iter().filter(|r| r.method != Method::Get).collect();
    assert_eq!(sent.len(), 4);
    assert_eq!(
        body(sent[0]),
        json!({"title": "Crash on start", "body": "Steps…", "labels": ["bug", "ui"],
               "assignees": ["ann"]})
    );
    // The comment goes first, then the state change.
    assert!(sent[1].url.ends_with("/issues/7/comments"));
    assert_eq!(body(sent[1]), json!({"body": "Duplicate of #2"}));
    assert_eq!(
        body(sent[2]),
        json!({"state": "closed", "state_reason": "not_planned"})
    );
    assert_eq!(
        body(sent[3]),
        json!({"state": "open", "state_reason": "reopened"})
    );
}

#[test]
fn github_issue_failures_name_the_scope_or_the_disabled_issues() {
    let http = Arc::new(FakeHttp::default());
    http.on(
        Method::Patch,
        "/repos/octo/app/issues/7",
        403,
        json!({"message": "Resource not accessible by personal access token"}),
    );
    http.on(
        Method::Post,
        "/repos/octo/app/issues",
        410,
        json!({"message": "Issues are disabled for this repo"}),
    );
    let c = client(&http, &["github.com"]);
    let e = c
        .close_issue(&github_repo(), 7, CloseReason::Completed, None)
        .unwrap_err()
        .to_string();
    assert!(e.contains("not allowed to close the issue"), "{e}");
    assert!(e.contains("Issues: read and write"), "{e}");
    // No comment, no comment request.
    assert!(http.seen().iter().all(|r| !r.url.contains("/comments")));
    let e = c.create_issue(&github_repo(), &issue_draft()).unwrap_err();
    assert!(matches!(e, ForgeError::Unsupported(_)), "{e:?}");
}

#[test]
fn gitlab_issues_list_create_close_reopen() {
    let http = Arc::new(FakeHttp::default());
    let base = "/projects/team%2Fsub%2Fapp";
    http.on(
        Method::Get,
        &format!("{base}/issues?state=opened"),
        200,
        json!([{"iid": 5, "title": "Slow", "web_url": "https://git.corp.example/team/sub/app/-/issues/5",
                "author": {"username": "ann"}, "labels": ["perf"],
                "assignees": [{"username": "bob"}], "created_at": "1970-01-01T01:00:00.000Z"}]),
    );
    http.on(
        Method::Get,
        &format!("{base}/labels"),
        200,
        json!([{"name": "perf"}]),
    );
    http.on(
        Method::Get,
        &format!("{base}/members/all"),
        200,
        json!([{"id": 9, "username": "bob"}, {"id": 4, "username": "ann"}]),
    );
    http.on(
        Method::Get,
        "/users?username=ann",
        200,
        json!([{"id": 4, "username": "ann"}]),
    );
    http.on(
        Method::Get,
        base,
        200,
        json!({"issues_access_level": "enabled"}),
    );
    http.on(
        Method::Post,
        &format!("{base}/issues/6/notes"),
        201,
        json!({}),
    );
    http.on(
        Method::Post,
        &format!("{base}/issues"),
        201,
        json!({"iid": 6, "web_url": "https://git.corp.example/team/sub/app/-/issues/6"}),
    );
    http.on(Method::Put, &format!("{base}/issues/6"), 200, json!({}));
    let c = client(&http, &["git.corp.example"]);
    let repo = gitlab_repo();
    let issues = c.open_issues(&repo).unwrap();
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].number, 5);
    assert_eq!(issues[0].assignees, vec!["bob".to_owned()]);
    assert_eq!(issues[0].created_at, Some(archipel_model::Timestamp(3_600)));
    let choices = c.issue_choices(&repo).unwrap();
    assert_eq!(choices.labels, vec!["perf".to_owned()]);
    assert_eq!(choices.assignees, vec!["ann".to_owned(), "bob".to_owned()]);
    let created = c.create_issue(&repo, &issue_draft()).unwrap();
    assert_eq!(created.number, 6);
    c.close_issue(&repo, 6, CloseReason::NotPlanned, Some("Fixed in !3"))
        .unwrap();
    c.reopen_issue(&repo, 6).unwrap();
    let seen = http.seen();
    let sent: Vec<_> = seen.iter().filter(|r| r.method != Method::Get).collect();
    assert_eq!(
        body(sent[0]),
        json!({"title": "Crash on start", "description": "Steps…", "labels": "bug,ui",
               "assignee_ids": [4]})
    );
    assert!(sent[1].url.ends_with("/issues/6/notes"));
    assert_eq!(body(sent[2]), json!({"state_event": "close"}));
    assert_eq!(body(sent[3]), json!({"state_event": "reopen"}));
}

#[test]
fn gitlab_disabled_issues_say_so() {
    let http = Arc::new(FakeHttp::default());
    let base = "/projects/team%2Fsub%2Fapp";
    http.on(Method::Get, &format!("{base}/issues"), 403, json!({}));
    http.on(
        Method::Get,
        base,
        200,
        json!({"issues_access_level": "disabled"}),
    );
    let c = client(&http, &["git.corp.example"]);
    for e in [
        c.open_issues(&gitlab_repo()).unwrap_err(),
        c.issue_choices(&gitlab_repo()).unwrap_err(),
    ] {
        assert!(matches!(e, ForgeError::Unsupported(_)), "{e:?}");
    }
}

#[test]
fn github_lists_assignable_users_and_assigns_pull_requests() {
    let http = Arc::new(FakeHttp::default());
    http.on(
        Method::Get,
        "/repos/octo/app/assignees",
        200,
        json!([{"login": "zed"}, {"login": "Ann"}]),
    );
    // GitHub drops a login it cannot assign: the answer says what it kept.
    http.on(
        Method::Patch,
        "/repos/octo/app/issues/12",
        200,
        json!({"number": 12, "assignees": [{"login": "Ann"}]}),
    );
    let c = client(&http, &["github.com"]);
    let repo = github_repo();
    assert_eq!(
        c.assignable_users(&repo).unwrap(),
        vec!["Ann".to_owned(), "zed".to_owned()]
    );
    let kept = c
        .set_pull_request_assignees(&repo, 12, &["Ann".into(), "ghost".into()])
        .unwrap();
    assert_eq!(kept, vec!["Ann".to_owned()]);
    let seen = http.seen();
    let patch = seen.iter().find(|r| r.method == Method::Patch).unwrap();
    assert_eq!(body(patch), json!({"assignees": ["Ann", "ghost"]}));
}

#[test]
fn github_assigning_without_push_access_names_the_scope() {
    let http = Arc::new(FakeHttp::default());
    http.on(Method::Patch, "/repos/octo/app/issues/12", 403, json!({}));
    let c = client(&http, &["github.com"]);
    let e = c
        .set_pull_request_assignees(&github_repo(), 12, &["ann".into()])
        .unwrap_err();
    assert!(e.to_string().contains("assign the pull request"), "{e}");
    assert!(
        e.to_string().contains("Pull requests: read and write"),
        "{e}"
    );
}

#[test]
fn gitlab_assigns_merge_requests_by_user_id() {
    let http = Arc::new(FakeHttp::default());
    let base = "/projects/team%2Fsub%2Fapp";
    http.on(
        Method::Get,
        &format!("{base}/members/all"),
        200,
        json!([{"id": 9, "username": "bob"}, {"id": 4, "username": "ann"},
               {"id": 4, "username": "ann"}]),
    );
    http.on(
        Method::Get,
        "/users?username=ann",
        200,
        json!([{"id": 4, "username": "ann"}]),
    );
    http.on(
        Method::Put,
        &format!("{base}/merge_requests/3"),
        200,
        json!({"iid": 3, "title": "x", "web_url": "u", "target_branch": "main",
               "assignees": [{"username": "ann"}]}),
    );
    let c = client(&http, &["git.corp.example"]);
    let repo = gitlab_repo();
    assert_eq!(
        c.assignable_users(&repo).unwrap(),
        vec!["ann".to_owned(), "bob".to_owned()]
    );
    let kept = c
        .set_pull_request_assignees(&repo, 3, &["ann".into()])
        .unwrap();
    assert_eq!(kept, vec!["ann".to_owned()]);
    // Unassigning everyone sends an empty list.
    c.set_pull_request_assignees(&repo, 3, &[]).unwrap();
    let seen = http.seen();
    let puts: Vec<_> = seen.iter().filter(|r| r.method == Method::Put).collect();
    assert_eq!(body(puts[0]), json!({"assignee_ids": [4]}));
    assert_eq!(body(puts[1]), json!({"assignee_ids": []}));
}

#[test]
fn pull_requests_read_their_assignees() {
    let http = Arc::new(FakeHttp::default());
    let base = "/projects/team%2Fsub%2Fapp";
    http.on(
        Method::Get,
        &format!("{base}/merge_requests/3"),
        200,
        json!({"iid": 3, "title": "x", "web_url": "u", "target_branch": "main",
               "assignees": [{"username": "ann"}, {"username": "bob"}]}),
    );
    http.on(
        Method::Get,
        &format!("{base}/merge_requests?state=opened"),
        200,
        json!([{"iid": 3, "title": "x", "web_url": "u", "target_branch": "main"}]),
    );
    let c = client(&http, &["git.corp.example"]);
    let open = c.open_pull_requests(&gitlab_repo()).unwrap();
    assert_eq!(open[0].assignees, vec!["ann".to_owned(), "bob".to_owned()]);
}
