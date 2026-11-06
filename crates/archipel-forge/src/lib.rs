//! # archipel-forge
//!
//! Forge adapters (spec §8): GitHub (and GitHub Enterprise Server) and
//! GitLab (gitlab.com and self-hosted) behind the forge ports of
//! `archipel-model`.
//!
//! * [`ForgeClient`] implements `PullRequestReader`, `PullRequestWriter`,
//!   `ForgeDirectory` and `ForgeSignIn` by dispatching to the registered
//!   [`provider::Provider`] of each account's kind. A fork adds a forge
//!   (Gitea, Bitbucket…) with [`ForgeClient::register`].
//! * Providers only build HTTP requests and parse answers through the
//!   [`http::HttpTransport`] trait; the real transport is [`UreqTransport`]
//!   (blocking, rustls with the OS trust store, ADR 0003). Tests use a fake
//!   transport: no network.
//! * Credentials: [`KeychainCredentials`] (OS keychain, `CredentialStore`)
//!   and [`FileAccountStore`] (non-secret account list, `ForgeAccountStore`).
//! * [`GitHubReleases`]: Archipel's own releases (update check).
//! * [`git_credential`]: the same tokens served to `git` as a credential
//!   helper, for HTTPS fetch and push.
//!
//! Dependencies: `archipel-model` only. Calls are blocking; callers run them
//! on the scheduler's network pool.

// Provider labels are literals behind the trait's `&str` signature.
#![allow(clippy::unnecessary_literal_bound)]

pub mod client;
pub mod credentials;
pub mod git_credential;
pub mod github;
pub mod gitlab;
pub mod http;
pub mod provider;
pub mod releases;

pub use client::ForgeClient;
pub use credentials::{FileAccountStore, KeychainCredentials};
pub use http::UreqTransport;
pub use releases::GitHubReleases;
