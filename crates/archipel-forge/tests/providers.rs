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
    CiState, ForgeAccount, ForgeKind, ForgeRepo, ForgeToken, Namespace, NamespaceKind,
    PullRequestDraft, ReviewState,
};
use archipel_model::ports::{
    CredentialStore, ForgeDirectory, ForgeSignIn, PullRequestReader, PullRequestWriter,
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
