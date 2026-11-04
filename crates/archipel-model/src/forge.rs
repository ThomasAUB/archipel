//! Forge (GitHub, GitLab, …) vocabulary (spec §8): accounts, pull / merge
//! requests, CI and review state, repositories of an account.
//!
//! "Pull request" is used for both GitHub pull requests and GitLab merge
//! requests. Secrets never live in these types except [`ForgeToken`], whose
//! `Debug` output is redacted.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Id of a forge provider (`github`, `gitlab`). Open-ended so a fork can add
/// its own provider without touching this crate.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ForgeKind(pub String);

impl ForgeKind {
    /// GitHub and GitHub Enterprise Server.
    #[must_use]
    pub fn github() -> Self {
        Self("github".into())
    }

    /// GitLab (gitlab.com or self-hosted).
    #[must_use]
    pub fn gitlab() -> Self {
        Self("gitlab".into())
    }

    /// The id.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ForgeKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Normalizes a host typed by the user: `https://Host.example/` →
/// `host.example` (the form remote URLs are matched against).
#[must_use]
pub fn normalize_host(host: &str) -> String {
    let host = host.trim();
    let host = host.split_once("://").map_or(host, |(_, rest)| rest);
    let host = host.split('/').next().unwrap_or(host);
    let host = host.rsplit('@').next().unwrap_or(host);
    host.to_ascii_lowercase()
}

/// A forge host the user signed in to. Only the non-secret part: the token
/// lives in the OS keychain, keyed by `host`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForgeAccount {
    /// Host name as it appears in remote URLs (`github.com`, `gitlab.example.com`).
    pub host: String,
    /// Provider serving this host.
    pub kind: ForgeKind,
    /// API base URL (`https://api.github.com`, `https://gitlab.example.com/api/v4`).
    pub api_url: String,
    /// Login of the signed-in user.
    pub user: String,
}

/// A personal access token. Never printed by `Debug`.
#[derive(Clone, PartialEq, Eq)]
pub struct ForgeToken(String);

impl ForgeToken {
    /// Wraps a token (surrounding whitespace removed).
    #[must_use]
    pub fn new(token: impl Into<String>) -> Self {
        Self(token.into().trim().to_owned())
    }

    /// The secret, for the HTTP adapter and the keychain only.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// Whether the token is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for ForgeToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ForgeToken(***)")
    }
}

/// A repository on a signed-in forge.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForgeRepo {
    /// The account (host and provider) it lives on.
    pub account: ForgeAccount,
    /// Full path on the forge, without `.git` (`owner/repo`, `group/sub/project`).
    pub path: String,
}

/// State of the CI of a pull request's head commit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CiState {
    /// Queued or running.
    Pending,
    /// Every check passed.
    Success,
    /// At least one check failed.
    Failure,
}

/// Review state of a pull request.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReviewState {
    /// No decisive review yet.
    Pending,
    /// Approved.
    Approved,
    /// A reviewer requested changes.
    ChangesRequested,
}

/// An open pull / merge request with its CI and review state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequestStatus {
    /// Number (GitHub) or iid (GitLab).
    pub number: u64,
    /// Title.
    pub title: String,
    /// Web page.
    pub url: String,
    /// Draft / work in progress.
    pub draft: bool,
    /// Target branch.
    pub target: String,
    /// CI of the head commit, `None` when there is no CI.
    pub ci: Option<CiState>,
    /// Review state.
    pub review: ReviewState,
}

/// A pull / merge request to create.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PullRequestDraft {
    /// Branch with the changes (must exist on the forge).
    pub source: String,
    /// Branch to merge into.
    pub target: String,
    /// Title.
    pub title: String,
    /// Description (Markdown).
    pub description: String,
    /// Open as a draft.
    pub draft: bool,
}

/// A created pull / merge request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PullRequestRef {
    /// Number (GitHub) or iid (GitLab).
    pub number: u64,
    /// Web page.
    pub url: String,
}

/// Kind of an account namespace.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NamespaceKind {
    /// The signed-in user.
    User,
    /// A GitHub organization.
    Organization,
    /// A GitLab group (or subgroup).
    Group,
}

/// Where repositories can be listed from: the user, an organization, a group.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Namespace {
    /// Full path (`octocat`, `my-org`, `group/subgroup`).
    pub path: String,
    /// Kind.
    pub kind: NamespaceKind,
}

/// A repository listed from a namespace (clone from an account).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteRepo {
    /// Full path (`owner/repo`).
    pub path: String,
    /// Short name (folder name of a clone).
    pub name: String,
    /// Description.
    pub description: Option<String>,
    /// HTTPS clone URL.
    pub https_url: String,
    /// SSH clone URL.
    pub ssh_url: String,
    /// Default branch.
    pub default_branch: Option<String>,
    /// Archived (read-only).
    pub archived: bool,
}
