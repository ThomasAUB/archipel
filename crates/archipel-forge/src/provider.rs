//! The provider extension point: one implementation per forge kind
//! (GitHub, GitLab; a fork adds Gitea, Bitbucket…), split by role and
//! registered in [`crate::ForgeClient`].

use archipel_model::ForgeError;
use archipel_model::forge::{
    ForgeKind, ForgeToken, Namespace, PullRequestDraft, PullRequestRef, PullRequestStatus,
    RemoteRepo,
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
}

/// Account browsing (clone from an account / org / group).
pub trait DirectoryApi: Send + Sync {
    /// `user`'s own namespace first, then its organizations / groups.
    fn namespaces(&self, at: &Endpoint, user: &str) -> Result<Vec<Namespace>, ForgeError>;
    /// Repositories of a namespace.
    fn repositories(
        &self,
        at: &Endpoint,
        namespace: &Namespace,
    ) -> Result<Vec<RemoteRepo>, ForgeError>;
}

/// A complete provider: what [`crate::ForgeClient::register`] takes.
pub trait Provider: ForgeApi + PullRequestApi + DirectoryApi {}

impl<T: ForgeApi + PullRequestApi + DirectoryApi> Provider for T {}
