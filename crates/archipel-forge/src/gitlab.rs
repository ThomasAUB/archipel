//! GitLab, gitlab.com or self-hosted (REST API v4). Merge requests are
//! reported as pull requests.

use archipel_model::ForgeError;
use archipel_model::forge::{
    CiJob, CiJobState, CiRun, CiState, ForgeKind, MergeMethod, MergeOptions, MergeReport,
    MergeState, Namespace, NamespaceKind, PullRequestDraft, PullRequestRef, PullRequestStatus,
    RemoteRepo, ReviewState,
};
use serde::Deserialize;
use serde_json::json;

use crate::http::{Api, Auth, Method, encode, optional, permission};
use crate::provider::{CiApi, DirectoryApi, Endpoint, ForgeApi, PullRequestApi};

/// The GitLab provider.
pub struct GitLab;

fn api<'a>(at: &'a Endpoint<'a>) -> Api<'a> {
    Api {
        http: at.http,
        host: at.host,
        base: at.api_url,
        token: at.token,
        auth: Auth::PrivateToken,
        extra_headers: &[],
    }
}

#[derive(Deserialize)]
struct User {
    username: String,
}

#[derive(Deserialize)]
struct MergeRequest {
    iid: u64,
    title: String,
    web_url: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    source_branch: String,
    target_branch: String,
    #[serde(default)]
    sha: Option<String>,
    #[serde(default)]
    author: Option<User>,
    #[serde(default)]
    has_conflicts: bool,
    #[serde(default)]
    detailed_merge_status: Option<String>,
    #[serde(default)]
    head_pipeline: Option<Pipeline>,
    #[serde(default)]
    source_project_id: Option<u64>,
    #[serde(default)]
    target_project_id: Option<u64>,
}

#[derive(Deserialize)]
struct Pipeline {
    status: String,
}

#[derive(Deserialize)]
struct BranchPipeline {
    id: u64,
    status: String,
    sha: String,
    web_url: Option<String>,
}

#[derive(Deserialize)]
struct Job {
    name: String,
    stage: Option<String>,
    status: String,
    web_url: Option<String>,
    #[serde(default)]
    allow_failure: bool,
}

#[derive(Deserialize)]
struct Approvals {
    #[serde(default)]
    approved_by: Vec<serde_json::Value>,
}

#[derive(Deserialize)]
struct Created {
    iid: u64,
    web_url: String,
}

#[derive(Deserialize)]
struct Group {
    full_path: String,
}

#[derive(Deserialize)]
struct Project {
    path_with_namespace: String,
    path: String,
    description: Option<String>,
    http_url_to_repo: String,
    ssh_url_to_repo: String,
    default_branch: Option<String>,
    #[serde(default)]
    archived: bool,
}

fn ci_state(pipeline: Option<&Pipeline>) -> Option<CiState> {
    match pipeline?.status.as_str() {
        "success" => Some(CiState::Success),
        "failed" | "canceled" => Some(CiState::Failure),
        "skipped" => None,
        _ => Some(CiState::Pending),
    }
}

/// State of one job. An allowed failure does not fail the pipeline, so it
/// is not reported as one.
fn job_state(job: &Job) -> CiJobState {
    match job.status.as_str() {
        "success" => CiJobState::Success,
        "failed" if job.allow_failure => CiJobState::Skipped,
        "failed" => CiJobState::Failed,
        "canceled" | "canceling" => CiJobState::Canceled,
        "skipped" => CiJobState::Skipped,
        "manual" => CiJobState::Manual,
        "running" => CiJobState::Running,
        _ => CiJobState::Queued,
    }
}

fn project(repo: &str) -> String {
    format!("/projects/{}", encode(repo))
}

/// Mergeability from `detailed_merge_status` (computed asynchronously:
/// `checking` / `unchecked` until then).
fn merge_state(mr: &MergeRequest) -> MergeState {
    if mr.has_conflicts {
        return MergeState::Conflicts;
    }
    let blocked = |reason: &str| MergeState::Blocked(reason.into());
    match mr.detailed_merge_status.as_deref() {
        None | Some("checking" | "unchecked" | "approvals_syncing" | "preparing") => {
            MergeState::Unknown
        }
        Some("mergeable") => MergeState::Mergeable,
        Some("conflict" | "broken_status") => MergeState::Conflicts,
        Some("draft_status") => blocked("draft"),
        Some("ci_must_pass") => blocked("CI must pass"),
        Some("ci_still_running") => blocked("CI still running"),
        Some("not_approved") => blocked("not approved"),
        Some("discussions_not_resolved") => blocked("unresolved discussions"),
        Some("need_rebase") => blocked("needs a rebase"),
        Some("requested_changes") => blocked("changes requested"),
        Some(other) => blocked(&other.replace('_', " ")),
    }
}

/// The merge request `iid` with its head pipeline and review state (the
/// single-MR endpoint carries the head pipeline, lists do not).
fn status(api: &Api<'_>, base: &str, iid: u64) -> Result<PullRequestStatus, ForgeError> {
    let mr: MergeRequest = api.get(&format!("{base}/merge_requests/{iid}"), "merge request")?;
    // Approvals may be unavailable (permissions): no approval then.
    let approved = api
        .get::<Approvals>(
            &format!("{base}/merge_requests/{}/approvals", mr.iid),
            "approvals",
        )
        .is_ok_and(|a| !a.approved_by.is_empty());
    let review = if mr.detailed_merge_status.as_deref() == Some("requested_changes") {
        ReviewState::ChangesRequested
    } else if approved {
        ReviewState::Approved
    } else {
        ReviewState::Pending
    };
    Ok(PullRequestStatus {
        number: mr.iid,
        ci: ci_state(mr.head_pipeline.as_ref()),
        merge: merge_state(&mr),
        title: mr.title,
        url: mr.web_url,
        draft: mr.draft,
        source: mr.source_branch,
        target: mr.target_branch,
        head_sha: mr.sha.unwrap_or_default(),
        author: mr.author.map(|u| u.username).unwrap_or_default(),
        review,
        from_fork: matches!(
            (mr.source_project_id, mr.target_project_id),
            (Some(s), Some(t)) if s != t
        ),
    })
}

impl ForgeApi for GitLab {
    fn kind(&self) -> ForgeKind {
        ForgeKind::gitlab()
    }

    fn label(&self) -> &str {
        "GitLab"
    }

    fn recognizes(&self, host: &str) -> bool {
        host.split('.').any(|part| part == "gitlab")
    }

    fn api_url(&self, host: &str) -> String {
        format!("https://{host}/api/v4")
    }

    fn current_user(&self, at: &Endpoint) -> Result<String, ForgeError> {
        Ok(api(at).get::<User>("/user", "user")?.username)
    }
}

impl PullRequestApi for GitLab {
    fn for_branch(
        &self,
        at: &Endpoint,
        repo: &str,
        branch: &str,
    ) -> Result<Option<PullRequestStatus>, ForgeError> {
        let api = api(at);
        let base = project(repo);
        let list: Vec<MergeRequest> = api.get(
            &format!(
                "{base}/merge_requests?state=opened&per_page=1&source_branch={}",
                encode(branch)
            ),
            &format!("project {repo}"),
        )?;
        let Some(listed) = list.into_iter().next() else {
            return Ok(None);
        };
        status(&api, &base, listed.iid).map(Some)
    }

    fn list_open(&self, at: &Endpoint, repo: &str) -> Result<Vec<PullRequestStatus>, ForgeError> {
        let api = api(at);
        let base = project(repo);
        let list: Vec<MergeRequest> = api.get_pages(
            &format!(
                "{base}/merge_requests?state=opened&per_page=100&order_by=created_at&sort=asc"
            ),
            &format!("project {repo}"),
        )?;
        list.iter().map(|mr| status(&api, &base, mr.iid)).collect()
    }

    fn create(
        &self,
        at: &Endpoint,
        repo: &str,
        draft: &PullRequestDraft,
    ) -> Result<PullRequestRef, ForgeError> {
        let title = if draft.draft {
            format!("Draft: {}", draft.title)
        } else {
            draft.title.clone()
        };
        let created: Created = api(at).write(
            Method::Post,
            &format!("{}/merge_requests", project(repo)),
            json!({
                "source_branch": draft.source,
                "target_branch": draft.target,
                "title": title,
                "description": draft.description,
            }),
            &format!("project {repo}"),
        )?;
        Ok(PullRequestRef {
            number: created.iid,
            url: created.web_url,
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
                Method::Put,
                &format!("{}/merge_requests/{number}", project(repo)),
                json!({ "description": description }),
                &format!("merge request !{number}"),
            )
            .map(drop)
    }

    fn merge(
        &self,
        at: &Endpoint,
        repo: &str,
        number: u64,
        options: &MergeOptions,
    ) -> Result<MergeReport, ForgeError> {
        let squash = match options.method {
            MergeMethod::Merge => false,
            MergeMethod::Squash => true,
            MergeMethod::Rebase => {
                return Err(ForgeError::Unsupported(
                    "GitLab rebases according to the project's merge method: choose Merge \
                     or Squash"
                        .into(),
                ));
            }
        };
        let mut body = json!({
            "squash": squash,
            "should_remove_source_branch": options.delete_source_branch,
        });
        if let Some(sha) = &options.expected_head {
            body["sha"] = json!(sha);
        }
        let what = format!("merge request !{number}");
        permission(
            api(at).write::<serde_json::Value>(
                Method::Put,
                &format!("{}/merge_requests/{number}/merge", project(repo)),
                body,
                &what,
            ),
            &format!("merge {what}"),
        )?;
        Ok(MergeReport::default())
    }
}

impl CiApi for GitLab {
    fn branch_ci(
        &self,
        at: &Endpoint,
        repo: &str,
        branch: &str,
    ) -> Result<Option<CiRun>, ForgeError> {
        let api = api(at);
        let base = project(repo);
        // Newest first: the first pipeline is the latest of the branch.
        let pipelines: Vec<BranchPipeline> = match api.get(
            &format!("{base}/pipelines?ref={}&per_page=1", encode(branch)),
            &format!("project {repo}"),
        ) {
            Ok(p) => p,
            // CI disabled on the project.
            Err(ForgeError::NotFound { .. }) => return Ok(None),
            Err(e) => return Err(e),
        };
        let Some(pipeline) = pipelines.into_iter().next() else {
            return Ok(None);
        };
        let Some(state) = ci_state(Some(&Pipeline {
            status: pipeline.status.clone(),
        })) else {
            return Ok(None);
        };
        // Newest job first; retried jobs are left out by the API.
        let mut jobs: Vec<Job> = optional(api.get(
            &format!("{base}/pipelines/{}/jobs?per_page=100", pipeline.id),
            "jobs",
        ))?
        .unwrap_or_default();
        jobs.reverse();
        Ok(Some(CiRun {
            state,
            sha: pipeline.sha,
            url: pipeline.web_url,
            jobs: jobs
                .into_iter()
                .map(|j| CiJob {
                    state: job_state(&j),
                    name: j.name,
                    stage: j.stage,
                    url: j.web_url,
                })
                .collect(),
        }))
    }
}

impl DirectoryApi for GitLab {
    fn namespaces(&self, at: &Endpoint, user: &str) -> Result<Vec<Namespace>, ForgeError> {
        let groups: Vec<Group> = api(at).get_pages(
            "/groups?min_access_level=10&per_page=100&order_by=path",
            "groups",
        )?;
        let mut out = vec![Namespace {
            path: user.to_owned(),
            kind: NamespaceKind::User,
        }];
        out.extend(groups.into_iter().map(|g| Namespace {
            path: g.full_path,
            kind: NamespaceKind::Group,
        }));
        Ok(out)
    }

    fn repositories(
        &self,
        at: &Endpoint,
        namespace: &Namespace,
    ) -> Result<Vec<RemoteRepo>, ForgeError> {
        let path = match namespace.kind {
            NamespaceKind::User => {
                "/projects?owned=true&per_page=100&order_by=path&sort=asc".into()
            }
            NamespaceKind::Organization | NamespaceKind::Group => format!(
                "/groups/{}/projects?include_subgroups=true&per_page=100&order_by=path&sort=asc",
                encode(&namespace.path)
            ),
        };
        let projects: Vec<Project> = api(at).get_pages(&path, &namespace.path)?;
        Ok(projects
            .into_iter()
            .map(|p| RemoteRepo {
                path: p.path_with_namespace,
                name: p.path,
                description: p.description.filter(|d| !d.is_empty()),
                https_url: p.http_url_to_repo,
                ssh_url: p.ssh_url_to_repo,
                default_branch: p.default_branch,
                archived: p.archived,
            })
            .collect())
    }
}
