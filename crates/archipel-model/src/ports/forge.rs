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
    CiRun, ForgeAccount, ForgeKind, ForgeRepo, ForgeToken, MergeOptions, MergeReport, Namespace,
    PullRequestDraft, PullRequestRef, PullRequestStatus, Release, RemoteRepo,
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
}

/// Reads the CI of a branch head (dashboard "CI" column).
pub trait CiReader: Send + Sync {
    /// The latest CI run of `branch` on the forge with its jobs; `None` when
    /// the branch is not on the forge or has no CI.
    fn ci_for_branch(&self, repo: &ForgeRepo, branch: &str) -> Result<Option<CiRun>, ForgeError>;
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
}

/// Lists what an account can see (clone from an account / org / group).
pub trait ForgeDirectory: Send + Sync {
    /// The user's own namespace first, then its organizations / groups.
    fn namespaces(&self, account: &ForgeAccount) -> Result<Vec<Namespace>, ForgeError>;
    /// Repositories of a namespace (bounded number of pages).
    fn repositories(
        &self,
        account: &ForgeAccount,
        namespace: &Namespace,
    ) -> Result<Vec<RemoteRepo>, ForgeError>;
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
}
