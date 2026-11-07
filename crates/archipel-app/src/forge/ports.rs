//! The forge ports the application uses, injected by the composition root.

use std::sync::Arc;

use archipel_model::ForgeError;
use archipel_model::forge::{
    CiRun, ForgeAccount, ForgeKind, ForgeRepo, ForgeToken, MergeOptions, MergeReport, Namespace,
    PullRequestDraft, PullRequestRef, PullRequestStatus, RemoteRepo,
};
use archipel_model::ports::{
    CiReader, CredentialStore, ForgeAccountStore, ForgeDirectory, ForgeSignIn, PullRequestReader,
    PullRequestWriter,
};

/// Forge access, each a narrow trait object (spec §8).
#[derive(Clone)]
pub struct ForgePorts {
    /// Open pull requests (dashboard column, review list).
    pub pull_requests: Arc<dyn PullRequestReader>,
    /// CI of a branch head (dashboard "CI" column).
    pub ci: Arc<dyn CiReader>,
    /// Pull request creation, linking and merge.
    pub pull_request_writer: Arc<dyn PullRequestWriter>,
    /// Namespaces and repositories of an account (clone from an account).
    pub directory: Arc<dyn ForgeDirectory>,
    /// Token verification and provider detection.
    pub sign_in: Arc<dyn ForgeSignIn>,
    /// Tokens (OS keychain).
    pub credentials: Arc<dyn CredentialStore>,
    /// Signed-in hosts (non-secret list).
    pub accounts: Arc<dyn ForgeAccountStore>,
}

impl ForgePorts {
    /// One forge adapter for the API ports, one store for the credentials.
    pub fn from_backend<F, C>(forge: Arc<F>, credentials: Arc<C>) -> Self
    where
        F: PullRequestReader
            + CiReader
            + PullRequestWriter
            + ForgeDirectory
            + ForgeSignIn
            + 'static,
        C: CredentialStore + ForgeAccountStore + 'static,
    {
        Self {
            pull_requests: forge.clone(),
            ci: forge.clone(),
            pull_request_writer: forge.clone(),
            directory: forge.clone(),
            sign_in: forge,
            credentials: credentials.clone(),
            accounts: credentials,
        }
    }

    /// No forge integration: no account, every call refused. For builds or
    /// tests without network access.
    #[must_use]
    pub fn disabled() -> Self {
        let none = Arc::new(NoForge);
        Self::from_backend(none.clone(), none)
    }
}

/// The ports of [`ForgePorts::disabled`].
struct NoForge;

fn refused(what: &str) -> ForgeError {
    ForgeError::UnknownForge(format!("{what} (forge integration disabled)"))
}

impl PullRequestReader for NoForge {
    fn pull_request_for_branch(
        &self,
        repo: &ForgeRepo,
        _branch: &str,
    ) -> Result<Option<PullRequestStatus>, ForgeError> {
        Err(refused(&repo.account.host))
    }
    fn open_pull_requests(&self, repo: &ForgeRepo) -> Result<Vec<PullRequestStatus>, ForgeError> {
        Err(refused(&repo.account.host))
    }
}

impl CiReader for NoForge {
    fn ci_for_branch(&self, repo: &ForgeRepo, _branch: &str) -> Result<Option<CiRun>, ForgeError> {
        Err(refused(&repo.account.host))
    }
}

impl PullRequestWriter for NoForge {
    fn create_pull_request(
        &self,
        repo: &ForgeRepo,
        _draft: &PullRequestDraft,
    ) -> Result<PullRequestRef, ForgeError> {
        Err(refused(&repo.account.host))
    }
    fn set_pull_request_description(
        &self,
        repo: &ForgeRepo,
        _number: u64,
        _description: &str,
    ) -> Result<(), ForgeError> {
        Err(refused(&repo.account.host))
    }
    fn merge_pull_request(
        &self,
        repo: &ForgeRepo,
        _number: u64,
        _options: &MergeOptions,
    ) -> Result<MergeReport, ForgeError> {
        Err(refused(&repo.account.host))
    }
}

impl ForgeDirectory for NoForge {
    fn namespaces(&self, account: &ForgeAccount) -> Result<Vec<Namespace>, ForgeError> {
        Err(refused(&account.host))
    }
    fn repositories(
        &self,
        account: &ForgeAccount,
        _namespace: &Namespace,
    ) -> Result<Vec<RemoteRepo>, ForgeError> {
        Err(refused(&account.host))
    }
}

impl ForgeSignIn for NoForge {
    fn kinds(&self) -> Vec<ForgeKind> {
        Vec::new()
    }
    fn verify(
        &self,
        host: &str,
        _kind: Option<&ForgeKind>,
        _token: &ForgeToken,
    ) -> Result<ForgeAccount, ForgeError> {
        Err(refused(host))
    }
}

impl CredentialStore for NoForge {
    fn token(&self, _host: &str) -> Result<Option<ForgeToken>, ForgeError> {
        Ok(None)
    }
    fn set_token(&self, host: &str, _token: &ForgeToken) -> Result<(), ForgeError> {
        Err(refused(host))
    }
    fn delete_token(&self, _host: &str) -> Result<(), ForgeError> {
        Ok(())
    }
}

impl ForgeAccountStore for NoForge {
    fn accounts(&self) -> Result<Vec<ForgeAccount>, ForgeError> {
        Ok(Vec::new())
    }
    fn save_accounts(&self, _accounts: &[ForgeAccount]) -> Result<(), ForgeError> {
        Err(refused("accounts"))
    }
}
