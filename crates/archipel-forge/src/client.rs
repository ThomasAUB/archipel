//! [`ForgeClient`]: serves the forge ports of `archipel-model` by
//! dispatching to the registered provider of each account's kind, with the
//! token of the account's host from the [`CredentialStore`].

use std::sync::Arc;

use archipel_model::ForgeError;
use archipel_model::forge::{
    CiRun, ForgeAccount, ForgeKind, ForgeRepo, ForgeToken, MergeOptions, MergeReport, Namespace,
    PullRequestDraft, PullRequestRef, PullRequestStatus, RemoteRepo, normalize_host,
};
use archipel_model::ports::{
    CiReader, CredentialStore, ForgeDirectory, ForgeSignIn, PullRequestReader, PullRequestWriter,
};

use crate::github::GitHub;
use crate::gitlab::GitLab;
use crate::http::HttpTransport;
use crate::provider::{Endpoint, Provider};

/// Forge client over a set of providers.
pub struct ForgeClient {
    http: Arc<dyn HttpTransport>,
    credentials: Arc<dyn CredentialStore>,
    providers: Vec<Arc<dyn Provider>>,
}

impl ForgeClient {
    /// A client without providers.
    #[must_use]
    pub fn new(http: Arc<dyn HttpTransport>, credentials: Arc<dyn CredentialStore>) -> Self {
        Self {
            http,
            credentials,
            providers: Vec::new(),
        }
    }

    /// A client with the GitHub and GitLab providers.
    #[must_use]
    pub fn with_builtins(
        http: Arc<dyn HttpTransport>,
        credentials: Arc<dyn CredentialStore>,
    ) -> Self {
        let mut client = Self::new(http, credentials);
        client.register(Arc::new(GitHub));
        client.register(Arc::new(GitLab));
        client
    }

    /// Adds a provider, or replaces the one of the same kind.
    pub fn register(&mut self, provider: Arc<dyn Provider>) {
        let kind = provider.kind();
        match self.providers.iter().position(|p| p.kind() == kind) {
            Some(i) => self.providers[i] = provider,
            None => self.providers.push(provider),
        }
    }

    fn provider(&self, kind: &ForgeKind) -> Result<&Arc<dyn Provider>, ForgeError> {
        self.providers
            .iter()
            .find(|p| p.kind() == *kind)
            .ok_or_else(|| ForgeError::UnknownForge(kind.to_string()))
    }

    fn token(&self, host: &str) -> Result<ForgeToken, ForgeError> {
        self.credentials
            .token(host)?
            .filter(|t| !t.is_empty())
            .ok_or_else(|| ForgeError::NotSignedIn { host: host.into() })
    }

    /// Runs `f` with the provider and endpoint of an account.
    fn with<T>(
        &self,
        account: &ForgeAccount,
        f: impl FnOnce(&dyn Provider, &Endpoint) -> Result<T, ForgeError>,
    ) -> Result<T, ForgeError> {
        let provider = self.provider(&account.kind)?;
        let token = self.token(&account.host)?;
        let at = Endpoint {
            http: self.http.as_ref(),
            host: &account.host,
            api_url: account.api_url.trim_end_matches('/'),
            token: &token,
        };
        f(provider.as_ref(), &at)
    }
}

impl PullRequestReader for ForgeClient {
    fn pull_request_for_branch(
        &self,
        repo: &ForgeRepo,
        branch: &str,
    ) -> Result<Option<PullRequestStatus>, ForgeError> {
        self.with(&repo.account, |p, at| p.for_branch(at, &repo.path, branch))
    }

    fn open_pull_requests(&self, repo: &ForgeRepo) -> Result<Vec<PullRequestStatus>, ForgeError> {
        self.with(&repo.account, |p, at| p.list_open(at, &repo.path))
    }
}

impl CiReader for ForgeClient {
    fn ci_for_branch(&self, repo: &ForgeRepo, branch: &str) -> Result<Option<CiRun>, ForgeError> {
        self.with(&repo.account, |p, at| p.branch_ci(at, &repo.path, branch))
    }
}

impl PullRequestWriter for ForgeClient {
    fn create_pull_request(
        &self,
        repo: &ForgeRepo,
        draft: &PullRequestDraft,
    ) -> Result<PullRequestRef, ForgeError> {
        self.with(&repo.account, |p, at| p.create(at, &repo.path, draft))
    }

    fn set_pull_request_description(
        &self,
        repo: &ForgeRepo,
        number: u64,
        description: &str,
    ) -> Result<(), ForgeError> {
        self.with(&repo.account, |p, at| {
            p.set_description(at, &repo.path, number, description)
        })
    }

    fn merge_pull_request(
        &self,
        repo: &ForgeRepo,
        number: u64,
        options: &MergeOptions,
    ) -> Result<MergeReport, ForgeError> {
        self.with(&repo.account, |p, at| {
            p.merge(at, &repo.path, number, options)
        })
    }
}

impl ForgeDirectory for ForgeClient {
    fn namespaces(&self, account: &ForgeAccount) -> Result<Vec<Namespace>, ForgeError> {
        self.with(account, |p, at| p.namespaces(at, &account.user))
    }

    fn repositories(
        &self,
        account: &ForgeAccount,
        namespace: &Namespace,
    ) -> Result<Vec<RemoteRepo>, ForgeError> {
        self.with(account, |p, at| p.repositories(at, namespace))
    }
}

impl ForgeSignIn for ForgeClient {
    fn kinds(&self) -> Vec<ForgeKind> {
        self.providers.iter().map(|p| p.kind()).collect()
    }

    fn verify(
        &self,
        host: &str,
        kind: Option<&ForgeKind>,
        token: &ForgeToken,
    ) -> Result<ForgeAccount, ForgeError> {
        let host = normalize_host(host);
        if host.is_empty() {
            return Err(ForgeError::UnknownForge("an empty host".into()));
        }
        // An explicit kind, else providers recognizing the host name first,
        // then every other one (self-hosted forge on a neutral host name).
        let mut candidates: Vec<&Arc<dyn Provider>> = match kind {
            Some(k) => vec![self.provider(k)?],
            None => self
                .providers
                .iter()
                .filter(|p| p.recognizes(&host))
                .collect(),
        };
        if kind.is_none() {
            candidates.extend(self.providers.iter().filter(|p| !p.recognizes(&host)));
        }
        let mut last = ForgeError::UnknownForge(host.clone());
        for provider in candidates {
            let api_url = provider.api_url(&host);
            let at = Endpoint {
                http: self.http.as_ref(),
                host: &host,
                api_url: &api_url,
                token,
            };
            match provider.current_user(&at) {
                Ok(user) => {
                    return Ok(ForgeAccount {
                        host: host.clone(),
                        kind: provider.kind(),
                        api_url,
                        user,
                    });
                }
                // The forge answered and refused the token, or is down:
                // asking another API at the same host will not help.
                Err(e @ (ForgeError::Unauthorized { .. } | ForgeError::Network { .. })) => {
                    return Err(e);
                }
                Err(e) => last = e,
            }
        }
        Err(match last {
            ForgeError::NotFound { .. } | ForgeError::Decode { .. } => {
                ForgeError::UnknownForge(host)
            }
            other => other,
        })
    }
}
