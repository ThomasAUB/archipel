//! Forge access (spec §8), split by role (ARCHITECTURE §3.1).
//!
//! Contract shared by every trait in this module:
//! * implementations are `Send + Sync` and may be called concurrently;
//! * calls are **blocking network calls** bounded by the adapter's timeout:
//!   callers run them on the scheduler's network pool, never on the UI thread;
//! * the token of a host is looked up by the implementation (from a
//!   [`CredentialStore`]); it never travels through consumers, and errors
//!   never contain it.

use crate::error::ForgeError;
use crate::forge::{
    AutoMergeReport, CiJob, CiJobLog, CiRun, CloseReason, ForgeAccount, ForgeKind, ForgeRepo,
    ForgeToken, Issue, IssueChoices, IssueDraft, IssueRef, MergeOptions, MergeReport, Namespace,
    NewRemoteRepo, PullRequestDraft, PullRequestRef, PullRequestStatus, Release, RemoteRepo,
};

/// Reads open pull / merge requests (dashboard column, review list).
pub trait PullRequestReader: Send + Sync {
    /// The open pull request whose source is `branch`, with its CI and
    /// review state; `None` when there is none.
    fn pull_request_for_branch(
        &self,
        repo: &ForgeRepo,
        branch: &str,
    ) -> Result<Option<PullRequestStatus>, ForgeError>;
    /// Every open pull request of the repo (bounded number of pages), with
    /// CI, review and merge state.
    fn open_pull_requests(&self, repo: &ForgeRepo) -> Result<Vec<PullRequestStatus>, ForgeError>;
    /// The logins a pull request of the repo can be assigned to, sorted.
    /// Empty when the token may not list them (no push access).
    fn assignable_users(&self, repo: &ForgeRepo) -> Result<Vec<String>, ForgeError>;
}

/// Reads the CI of a branch head (dashboard "CI" column).
pub trait CiReader: Send + Sync {
    /// The latest CI run of `branch` on the forge with its jobs; `None` when
    /// the branch is not on the forge or has no CI.
    fn ci_for_branch(&self, repo: &ForgeRepo, branch: &str) -> Result<Option<CiRun>, ForgeError>;
    /// The end of the log of a job (one with an `id`), reduced to its error
    /// lines. Only the end of the log is kept.
    fn ci_job_log(&self, repo: &ForgeRepo, job: &CiJob) -> Result<CiJobLog, ForgeError>;
}

/// Runs CI again.
pub trait CiWriter: Send + Sync {
    /// Reruns the failed (and canceled) jobs of `run`. Returns how many
    /// runs were restarted. A token without write access to the CI fails
    /// with a message naming the scope to add.
    fn rerun_failed_ci(&self, repo: &ForgeRepo, run: &CiRun) -> Result<usize, ForgeError>;
}

/// Creates and merges pull / merge requests.
pub trait PullRequestWriter: Send + Sync {
    /// Opens a pull request. The source branch must already be on the forge.
    fn create_pull_request(
        &self,
        repo: &ForgeRepo,
        draft: &PullRequestDraft,
    ) -> Result<PullRequestRef, ForgeError>;
    /// Replaces the description of a pull request (links between the pull
    /// requests of a multi-repo change).
    fn set_pull_request_description(
        &self,
        repo: &ForgeRepo,
        number: u64,
        description: &str,
    ) -> Result<(), ForgeError>;
    /// Merges a pull request (and deletes its source branch when asked).
    fn merge_pull_request(
        &self,
        repo: &ForgeRepo,
        number: u64,
        options: &MergeOptions,
    ) -> Result<MergeReport, ForgeError>;
    /// Arms the forge's auto-merge: the forge merges the pull request
    /// itself once its required checks pass, even with Archipel closed.
    /// [`ForgeError::Unsupported`] when the forge or the repository does not
    /// allow it (setting off, nothing left to wait for); the caller may then
    /// watch the CI itself.
    fn enable_auto_merge(
        &self,
        repo: &ForgeRepo,
        number: u64,
        options: &MergeOptions,
    ) -> Result<AutoMergeReport, ForgeError>;
    /// Disarms the forge's auto-merge of a pull request.
    fn disable_auto_merge(&self, repo: &ForgeRepo, number: u64) -> Result<(), ForgeError>;
    /// Replaces the assignees of a pull request (empty: unassigns all).
    /// Returns the assignees as the forge kept them: GitHub silently drops
    /// a login that cannot be assigned. A token without write access fails
    /// with a message naming the scope to add.
    fn set_pull_request_assignees(
        &self,
        repo: &ForgeRepo,
        number: u64,
        assignees: &[String],
    ) -> Result<Vec<String>, ForgeError>;
}

/// Reads issues. A forge or a repo without issues (disabled on the repo,
/// or a provider without them) fails with [`ForgeError::Unsupported`],
/// saying so.
pub trait IssueReader: Send + Sync {
    /// Every open issue of the repo (bounded number of pages; pull
    /// requests left out), oldest first.
    fn open_issues(&self, repo: &ForgeRepo) -> Result<Vec<Issue>, ForgeError>;
    /// The labels and assignees a new issue of the repo can carry; fails
    /// with [`ForgeError::Unsupported`] when the repo has no issues, so a
    /// form can say so before anything is typed.
    fn issue_choices(&self, repo: &ForgeRepo) -> Result<IssueChoices, ForgeError>;
}

/// Creates, closes and reopens issues. A token without write access to
/// issues fails with a message naming the scope to add.
pub trait IssueWriter: Send + Sync {
    /// Opens an issue.
    fn create_issue(&self, repo: &ForgeRepo, draft: &IssueDraft) -> Result<IssueRef, ForgeError>;
    /// Closes an issue, after posting `comment` on it when there is one.
    /// `reason` is dropped by forges without one (GitLab).
    fn close_issue(
        &self,
        repo: &ForgeRepo,
        number: u64,
        reason: CloseReason,
        comment: Option<&str>,
    ) -> Result<(), ForgeError>;
    /// Reopens a closed issue.
    fn reopen_issue(&self, repo: &ForgeRepo, number: u64) -> Result<(), ForgeError>;
}

/// Lists what an account can see (clone from an account / org / group),
/// and creates repositories there (publish a local repository).
pub trait ForgeDirectory: Send + Sync {
    /// The user's own namespace first, then its organizations / groups.
    fn namespaces(&self, account: &ForgeAccount) -> Result<Vec<Namespace>, ForgeError>;
    /// Repositories of a namespace (bounded number of pages).
    fn repositories(
        &self,
        account: &ForgeAccount,
        namespace: &Namespace,
    ) -> Result<Vec<RemoteRepo>, ForgeError>;
    /// Creates an empty repository (no README, no first commit, so a local
    /// history can be pushed to it) in `namespace`.
    fn create_repository(
        &self,
        account: &ForgeAccount,
        namespace: &Namespace,
        repo: &NewRemoteRepo,
    ) -> Result<RemoteRepo, ForgeError>;
}

/// Checks a token and identifies the forge behind a host.
pub trait ForgeSignIn: Send + Sync {
    /// Provider kinds available, for the sign-in form.
    fn kinds(&self) -> Vec<ForgeKind>;
    /// Verifies `token` against `host` and returns the account. With `kind`
    /// `None`, the provider is detected (host name first, then by asking
    /// each provider's API). Stores nothing.
    fn verify(
        &self,
        host: &str,
        kind: Option<&ForgeKind>,
        token: &ForgeToken,
    ) -> Result<ForgeAccount, ForgeError>;
}

/// Secret storage (the OS keychain): one token per host.
pub trait CredentialStore: Send + Sync {
    /// The token of a host, if any.
    fn token(&self, host: &str) -> Result<Option<ForgeToken>, ForgeError>;
    /// Stores (or replaces) the token of a host.
    fn set_token(&self, host: &str, token: &ForgeToken) -> Result<(), ForgeError>;
    /// Removes the token of a host (no error when there is none).
    fn delete_token(&self, host: &str) -> Result<(), ForgeError>;
}

/// The list of signed-in hosts (non-secret; never in `archipel.toml`).
pub trait ForgeAccountStore: Send + Sync {
    /// Every account.
    fn accounts(&self) -> Result<Vec<ForgeAccount>, ForgeError>;
    /// Replaces the list.
    fn save_accounts(&self, accounts: &[ForgeAccount]) -> Result<(), ForgeError>;
}

/// Where Archipel's own releases are published (update check). Unlike the
/// other forge ports, it needs no account.
pub trait ReleaseFeed: Send + Sync {
    /// The latest stable release; `None` when nothing is published yet.
    fn latest_release(&self) -> Result<Option<Release>, ForgeError>;

    /// Installs `release` in place of the running binary: downloads its
    /// archive for this platform, checks it against the release's
    /// checksums, then swaps the binary. The running process is untouched;
    /// the new version runs from the next launch. `progress` gets the
    /// downloaded share, 0 to 100. Returns the installed binary's path.
    ///
    /// Blocking and long (a download): callers run it off the UI thread.
    fn install(
        &self,
        release: &Release,
        progress: &dyn Fn(u8),
    ) -> Result<std::path::PathBuf, ForgeError>;

    /// Removes what an earlier [`install`](Self::install) left behind (the
    /// replaced binary, which Windows cannot delete while it runs). Best
    /// effort, called once at start.
    fn clean_up_install(&self) {}
}
