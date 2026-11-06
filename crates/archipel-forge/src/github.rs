//! GitHub and GitHub Enterprise Server (REST API v3).

use archipel_model::ForgeError;
use archipel_model::forge::{
    CiJob, CiJobState, CiRun, CiState, ForgeKind, Namespace, NamespaceKind, PullRequestDraft,
    PullRequestRef, PullRequestStatus, RemoteRepo, ReviewState, overall_ci_state,
};
use serde::Deserialize;
use serde_json::json;

use crate::http::{Api, Auth, Method, encode, encode_path};
use crate::provider::{CiApi, DirectoryApi, Endpoint, ForgeApi, PullRequestApi};

/// The GitHub provider.
pub struct GitHub;

const HEADERS: &[(&str, &str)] = &[
    ("Accept", "application/vnd.github+json"),
    ("X-GitHub-Api-Version", "2022-11-28"),
];

fn api<'a>(at: &'a Endpoint<'a>) -> Api<'a> {
    Api {
        http: at.http,
        host: at.host,
        base: at.api_url,
        token: at.token,
        auth: Auth::Bearer,
        extra_headers: HEADERS,
    }
}

#[derive(Deserialize)]
struct User {
    login: String,
}

#[derive(Deserialize)]
struct Ref {
    #[serde(rename = "ref")]
    name: String,
    sha: String,
}

#[derive(Deserialize)]
struct Pull {
    number: u64,
    title: String,
    html_url: String,
    #[serde(default)]
    draft: bool,
    base: Ref,
    head: Ref,
}

#[derive(Deserialize)]
struct CheckRun {
    #[serde(default)]
    name: String,
    status: String,
    conclusion: Option<String>,
    #[serde(default)]
    html_url: Option<String>,
}

#[derive(Deserialize)]
struct CheckRuns {
    check_runs: Vec<CheckRun>,
}

#[derive(Deserialize)]
struct CombinedStatus {
    state: String,
    total_count: u64,
    #[serde(default)]
    statuses: Vec<CommitStatus>,
}

#[derive(Deserialize)]
struct CommitStatus {
    context: String,
    state: String,
    target_url: Option<String>,
}

#[derive(Deserialize)]
struct Branch {
    commit: BranchCommit,
}

#[derive(Deserialize)]
struct BranchCommit {
    sha: String,
}

#[derive(Deserialize)]
struct Review {
    state: String,
    user: Option<User>,
}

#[derive(Deserialize)]
struct Created {
    number: u64,
    html_url: String,
}

#[derive(Deserialize)]
struct Repo {
    full_name: String,
    name: String,
    description: Option<String>,
    clone_url: String,
    ssh_url: String,
    default_branch: Option<String>,
    #[serde(default)]
    archived: bool,
}

/// CI state from check runs and the legacy combined status: any failure
/// wins, then anything still running, then success.
fn ci_state(runs: &[CheckRun], combined: Option<&CombinedStatus>) -> Option<CiState> {
    let mut states = Vec::new();
    for run in runs {
        states.push(if run.status == "completed" {
            match run.conclusion.as_deref() {
                Some("success" | "neutral" | "skipped") => CiState::Success,
                _ => CiState::Failure,
            }
        } else {
            CiState::Pending
        });
    }
    if let Some(c) = combined.filter(|c| c.total_count > 0) {
        states.push(match c.state.as_str() {
            "success" => CiState::Success,
            "pending" => CiState::Pending,
            _ => CiState::Failure,
        });
    }
    [CiState::Failure, CiState::Pending, CiState::Success]
        .into_iter()
        .find(|s| states.contains(s))
}

/// State of one check run.
fn check_run_state(run: &CheckRun) -> CiJobState {
    match (run.status.as_str(), run.conclusion.as_deref()) {
        ("completed", Some("success")) => CiJobState::Success,
        ("completed", Some("neutral" | "skipped" | "stale")) => CiJobState::Skipped,
        ("completed", Some("cancelled")) => CiJobState::Canceled,
        ("completed", Some("action_required")) => CiJobState::Manual,
        ("completed", _) => CiJobState::Failed,
        ("in_progress", _) => CiJobState::Running,
        _ => CiJobState::Queued,
    }
}

/// State of one legacy commit status.
fn commit_status_state(state: &str) -> CiJobState {
    match state {
        "success" => CiJobState::Success,
        "pending" => CiJobState::Running,
        _ => CiJobState::Failed,
    }
}

/// The jobs of a commit: its check runs, then its commit statuses.
fn ci_jobs(runs: Vec<CheckRun>, combined: Option<CombinedStatus>) -> Vec<CiJob> {
    let mut jobs: Vec<CiJob> = runs
        .into_iter()
        .map(|r| CiJob {
            state: check_run_state(&r),
            name: r.name,
            stage: None,
            url: r.html_url,
        })
        .collect();
    jobs.extend(
        combined
            .into_iter()
            .flat_map(|c| c.statuses)
            .map(|s| CiJob {
                state: commit_status_state(&s.state),
                name: s.context,
                stage: None,
                url: s.target_url,
            }),
    );
    jobs
}

/// Review state from the reviews (chronological): each reviewer's latest
/// decisive review counts; comments do not, a dismissal clears.
fn review_state(reviews: &[Review]) -> ReviewState {
    let mut latest: Vec<(String, &str)> = Vec::new();
    for r in reviews {
        let who = r.user.as_ref().map_or("", |u| u.login.as_str()).to_owned();
        let state = r.state.as_str();
        if !matches!(state, "APPROVED" | "CHANGES_REQUESTED" | "DISMISSED") {
            continue;
        }
        latest.retain(|(w, _)| *w != who);
        latest.push((who, state));
    }
    if latest.iter().any(|(_, s)| *s == "CHANGES_REQUESTED") {
        ReviewState::ChangesRequested
    } else if latest.iter().any(|(_, s)| *s == "APPROVED") {
        ReviewState::Approved
    } else {
        ReviewState::Pending
    }
}

impl ForgeApi for GitHub {
    fn kind(&self) -> ForgeKind {
        ForgeKind::github()
    }

    fn label(&self) -> &str {
        "GitHub"
    }

    fn recognizes(&self, host: &str) -> bool {
        host.split('.').any(|part| part == "github")
    }

    fn api_url(&self, host: &str) -> String {
        if host == "github.com" {
            "https://api.github.com".into()
        } else {
            format!("https://{host}/api/v3")
        }
    }

    fn current_user(&self, at: &Endpoint) -> Result<String, ForgeError> {
        Ok(api(at).get::<User>("/user", "user")?.login)
    }
}

impl PullRequestApi for GitHub {
    fn for_branch(
        &self,
        at: &Endpoint,
        repo: &str,
        branch: &str,
    ) -> Result<Option<PullRequestStatus>, ForgeError> {
        let api = api(at);
        let owner = repo.split('/').next().unwrap_or_default();
        let base = format!("/repos/{}", encode_path(repo));
        let pulls: Vec<Pull> = api.get(
            &format!(
                "{base}/pulls?state=open&per_page=1&head={}",
                encode(&format!("{owner}:{branch}"))
            ),
            &format!("repository {repo}"),
        )?;
        let Some(pull) = pulls.into_iter().next() else {
            return Ok(None);
        };
        let sha = encode(&pull.head.sha);
        // CI and reviews need extra scopes some tokens lack: a pull request
        // without them still shows, with no CI state.
        let runs = api
            .get::<CheckRuns>(
                &format!("{base}/commits/{sha}/check-runs?per_page=100"),
                "check runs",
            )
            .map(|r| r.check_runs)
            .unwrap_or_default();
        let combined = api
            .get::<CombinedStatus>(&format!("{base}/commits/{sha}/status"), "commit status")
            .ok();
        let reviews: Vec<Review> = api
            .get(
                &format!("{base}/pulls/{}/reviews?per_page=100", pull.number),
                "reviews",
            )
            .unwrap_or_default();
        Ok(Some(PullRequestStatus {
            number: pull.number,
            title: pull.title,
            url: pull.html_url,
            draft: pull.draft,
            target: pull.base.name,
            ci: ci_state(&runs, combined.as_ref()),
            review: review_state(&reviews),
        }))
    }

    fn create(
        &self,
        at: &Endpoint,
        repo: &str,
        draft: &PullRequestDraft,
    ) -> Result<PullRequestRef, ForgeError> {
        let created: Created = api(at).write(
            Method::Post,
            &format!("/repos/{}/pulls", encode_path(repo)),
            json!({
                "title": draft.title,
                "head": draft.source,
                "base": draft.target,
                "body": draft.description,
                "draft": draft.draft,
            }),
            &format!("repository {repo}"),
        )?;
        Ok(PullRequestRef {
            number: created.number,
            url: created.html_url,
        })
    }

    fn set_description(
        &self,
        at: &Endpoint,
        repo: &str,
        number: u64,
        description: &str,
    ) -> Result<(), ForgeError> {
        api(at)
            .write::<serde_json::Value>(
                Method::Patch,
                &format!("/repos/{}/pulls/{number}", encode_path(repo)),
                json!({ "body": description }),
                &format!("pull request #{number}"),
            )
            .map(drop)
    }
}

impl CiApi for GitHub {
    fn branch_ci(
        &self,
        at: &Endpoint,
        repo: &str,
        branch: &str,
    ) -> Result<Option<CiRun>, ForgeError> {
        let api = api(at);
        let base = format!("/repos/{}", encode_path(repo));
        let head = match api.get::<Branch>(
            &format!("{base}/branches/{}", encode(branch)),
            &format!("branch {branch}"),
        ) {
            Ok(b) => b.commit.sha,
            // Not pushed yet.
            Err(ForgeError::NotFound { .. }) => return Ok(None),
            Err(e) => return Err(e),
        };
        let sha = encode(&head);
        let runs = api
            .get::<CheckRuns>(
                &format!("{base}/commits/{sha}/check-runs?per_page=100"),
                "check runs",
            )
            .map(|r| r.check_runs)
            .unwrap_or_default();
        let combined = api
            .get::<CombinedStatus>(&format!("{base}/commits/{sha}/status"), "commit status")
            .ok();
        let jobs = ci_jobs(runs, combined);
        let Some(state) = overall_ci_state(&jobs) else {
            return Ok(None);
        };
        Ok(Some(CiRun {
            state,
            url: Some(format!("https://{}/{repo}/commit/{head}", at.host)),
            sha: head,
            jobs,
        }))
    }
}

impl DirectoryApi for GitHub {
    fn namespaces(&self, at: &Endpoint, user: &str) -> Result<Vec<Namespace>, ForgeError> {
        let orgs: Vec<User> = api(at).get_pages("/user/orgs?per_page=100", "organizations")?;
        let mut out = vec![Namespace {
            path: user.to_owned(),
            kind: NamespaceKind::User,
        }];
        out.extend(orgs.into_iter().map(|o| Namespace {
            path: o.login,
            kind: NamespaceKind::Organization,
        }));
        Ok(out)
    }

    fn repositories(
        &self,
        at: &Endpoint,
        namespace: &Namespace,
    ) -> Result<Vec<RemoteRepo>, ForgeError> {
        let path = match namespace.kind {
            NamespaceKind::User => "/user/repos?affiliation=owner&per_page=100".to_owned(),
            NamespaceKind::Organization | NamespaceKind::Group => {
                format!("/orgs/{}/repos?per_page=100", encode(&namespace.path))
            }
        };
        let repos: Vec<Repo> = api(at).get_pages(&path, &namespace.path)?;
        Ok(repos
            .into_iter()
            .map(|r| RemoteRepo {
                path: r.full_name,
                name: r.name,
                description: r.description.filter(|d| !d.is_empty()),
                https_url: r.clone_url,
                ssh_url: r.ssh_url,
                default_branch: r.default_branch,
                archived: r.archived,
            })
            .collect())
    }
}
