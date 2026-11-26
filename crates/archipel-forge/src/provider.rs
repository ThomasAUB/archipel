//! The provider extension point: one implementation per forge kind
//! (GitHub, GitLab; a fork adds Gitea, Bitbucket…), split by role and
//! registered in [`crate::ForgeClient`].

use archipel_model::ForgeError;
use archipel_model::forge::{
    AutoMergeReport, CiJobLog, CiRun, CloseReason, ForgeKind, ForgeToken, Issue, IssueChoices,
    IssueDraft, IssueRef, MergeOptions, MergeReport, Namespace, NewRemoteRepo, PullRequestDraft,
    PullRequestRef, PullRequestStatus, RemoteRepo,
};

use crate::http::HttpTransport;

/// Where a call goes: transport, host, API base URL and token.
pub struct Endpoint<'a> {
    /// Transport.
    pub http: &'a dyn HttpTransport,
    /// Host name (error messages).
    pub host: &'a str,
    /// API base URL, without trailing slash.
    pub api_url: &'a str,
    /// Token.
    pub token: &'a ForgeToken,
}

/// Identity of a provider and sign-in.
pub trait ForgeApi: Send + Sync {
    /// Provider id (`github`).
    fn kind(&self) -> ForgeKind;
    /// Display name (`GitHub`).
    fn label(&self) -> &str;
    /// Whether a host name looks like this forge (auto-detection before
    /// asking the APIs).
    fn recognizes(&self, host: &str) -> bool;
    /// API base URL for a host (`https://api.github.com`).
    fn api_url(&self, host: &str) -> String;
    /// Login of the token's user (verifies the token).
    fn current_user(&self, at: &Endpoint) -> Result<String, ForgeError>;
}

/// Pull / merge requests.
pub trait PullRequestApi: Send + Sync {
    /// The open pull request of `branch` with CI and review state.
    fn for_branch(
        &self,
        at: &Endpoint,
        repo: &str,
        branch: &str,
    ) -> Result<Option<PullRequestStatus>, ForgeError>;
    /// Every open pull request with CI, review and merge state.
    fn list_open(&self, at: &Endpoint, repo: &str) -> Result<Vec<PullRequestStatus>, ForgeError>;
    /// Opens a pull request.
    fn create(
        &self,
        at: &Endpoint,
        repo: &str,
        draft: &PullRequestDraft,
    ) -> Result<PullRequestRef, ForgeError>;
    /// Replaces a pull request description.
    fn set_description(
        &self,
        at: &Endpoint,
        repo: &str,
        number: u64,
        description: &str,
    ) -> Result<(), ForgeError>;
    /// Merges a pull request.
    fn merge(
        &self,
        at: &Endpoint,
        repo: &str,
        number: u64,
        options: &MergeOptions,
    ) -> Result<MergeReport, ForgeError>;
    /// Arms the forge's auto-merge of a pull request. Unsupported by
    /// default (the app then watches the CI itself).
    fn enable_auto_merge(
        &self,
        at: &Endpoint,
        repo: &str,
        number: u64,
        options: &MergeOptions,
    ) -> Result<AutoMergeReport, ForgeError> {
        let _ = (at, repo, number, options);
        Err(ForgeError::Unsupported(
            "this forge has no auto-merge".into(),
        ))
    }
    /// Disarms the forge's auto-merge of a pull request. Unsupported by
    /// default.
    fn disable_auto_merge(&self, at: &Endpoint, repo: &str, number: u64) -> Result<(), ForgeError> {
        let _ = (at, repo, number);
        Err(ForgeError::Unsupported(
            "this forge has no auto-merge".into(),
        ))
    }
    /// The logins a pull request can be assigned to, sorted. Unsupported
    /// by default.
    fn assignable_users(&self, at: &Endpoint, repo: &str) -> Result<Vec<String>, ForgeError> {
        let _ = (at, repo);
        Err(no_assignees())
    }
    /// Replaces the assignees of a pull request; returns them as the forge
    /// kept them. Unsupported by default.
    fn set_assignees(
        &self,
        at: &Endpoint,
        repo: &str,
        number: u64,
        assignees: &[String],
    ) -> Result<Vec<String>, ForgeError> {
        let _ = (at, repo, number, assignees);
        Err(no_assignees())
    }
}

fn no_assignees() -> ForgeError {
    ForgeError::Unsupported("this forge cannot assign pull requests".into())
}

/// CI of branches.
pub trait CiApi: Send + Sync {
    /// The latest CI run of `branch` with its jobs; `None` when the branch
    /// is not on the forge or has no CI.
    fn branch_ci(
        &self,
        at: &Endpoint,
        repo: &str,
        branch: &str,
    ) -> Result<Option<CiRun>, ForgeError>;

    /// The raw log of job `job` (a [`archipel_model::forge::CiJob`] id);
    /// the caller keeps its end only. Unsupported by default.
    fn job_log(&self, at: &Endpoint, repo: &str, job: u64) -> Result<String, ForgeError> {
        let _ = (at, repo, job);
        Err(ForgeError::Unsupported(
            "this forge does not serve CI logs".into(),
        ))
    }

    /// The excerpt of the end of a log: the default error rules of
    /// [`crate::ci_log::excerpt`], for a provider whose logs need others.
    fn log_excerpt(&self, log: &str) -> CiJobLog {
        crate::ci_log::excerpt(log)
    }

    /// Reruns the failed jobs of `run`; returns how many runs restarted.
    /// Unsupported by default.
    fn rerun_failed(&self, at: &Endpoint, repo: &str, run: &CiRun) -> Result<usize, ForgeError> {
        let _ = (at, repo, run);
        Err(ForgeError::Unsupported("this forge cannot rerun CI".into()))
    }
}

/// Issues. Every call is unsupported by default: a provider without issues
/// implements nothing (`impl IssueApi for Gitea {}`), and the app says so
/// instead of offering a form that cannot be sent.
pub trait IssueApi: Send + Sync {
    /// Every open issue (pull requests left out), oldest first.
    fn open_issues(&self, at: &Endpoint, repo: &str) -> Result<Vec<Issue>, ForgeError> {
        let _ = (at, repo);
        Err(no_issues())
    }

    /// The repo's labels and assignable users; [`ForgeError::Unsupported`]
    /// when the repo has issues disabled.
    fn issue_choices(&self, at: &Endpoint, repo: &str) -> Result<IssueChoices, ForgeError> {
        let _ = (at, repo);
        Err(no_issues())
    }

    /// Opens an issue.
    fn create_issue(
        &self,
        at: &Endpoint,
        repo: &str,
        draft: &IssueDraft,
    ) -> Result<IssueRef, ForgeError> {
        let _ = (at, repo, draft);
        Err(no_issues())
    }

    /// Closes an issue, posting `comment` first when there is one.
    fn close_issue(
        &self,
        at: &Endpoint,
        repo: &str,
        number: u64,
        reason: CloseReason,
        comment: Option<&str>,
    ) -> Result<(), ForgeError> {
        let _ = (at, repo, number, reason, comment);
        Err(no_issues())
    }

    /// Reopens an issue.
    fn reopen_issue(&self, at: &Endpoint, repo: &str, number: u64) -> Result<(), ForgeError> {
        let _ = (at, repo, number);
        Err(no_issues())
    }
}

fn no_issues() -> ForgeError {
    ForgeError::Unsupported("this forge does not serve issues here".into())
}

/// Account browsing (clone from an account / org / group) and repository
/// creation (publish a local repository).
pub trait DirectoryApi: Send + Sync {
    /// `user`'s own namespace first, then its organizations / groups.
    fn namespaces(&self, at: &Endpoint, user: &str) -> Result<Vec<Namespace>, ForgeError>;
    /// Repositories of a namespace.
    fn repositories(
        &self,
        at: &Endpoint,
        namespace: &Namespace,
    ) -> Result<Vec<RemoteRepo>, ForgeError>;
    /// Creates an empty repository in `namespace`. Unsupported by default.
    fn create_repository(
        &self,
        at: &Endpoint,
        namespace: &Namespace,
        repo: &NewRemoteRepo,
    ) -> Result<RemoteRepo, ForgeError> {
        let _ = (at, namespace, repo);
        Err(ForgeError::Unsupported(
            "this forge cannot create repositories".into(),
        ))
    }
}

/// A complete provider: what [`crate::ForgeClient::register`] takes.
pub trait Provider: ForgeApi + PullRequestApi + CiApi + IssueApi + DirectoryApi {}

impl<T: ForgeApi + PullRequestApi + CiApi + IssueApi + DirectoryApi> Provider for T {}
