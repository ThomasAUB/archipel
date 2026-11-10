//! Fakes of the forge ports: an in-memory forge, and an in-memory keychain
//! with its account list.

use std::collections::HashMap;
use std::sync::{Condvar, Mutex};

use super::lock;
use crate::error::ForgeError;
use crate::forge::{
    CiRun, ForgeAccount, ForgeKind, ForgeRepo, ForgeToken, MergeOptions, MergeReport, MergeState,
    Namespace, NamespaceKind, PullRequestDraft, PullRequestRef, PullRequestStatus, Release,
    RemoteRepo, ReviewState,
};
use crate::ports::{
    CiReader, CredentialStore, ForgeAccountStore, ForgeDirectory, ForgeSignIn, PullRequestReader,
    PullRequestWriter, ReleaseFeed,
};

/// A pull request created through the fake.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CreatedPullRequest {
    /// Repository path on the forge.
    pub repo: String,
    /// The draft it was created from.
    pub draft: PullRequestDraft,
    /// Number given.
    pub number: u64,
    /// Description, after any later edit.
    pub description: String,
}

/// A pull request merged through the fake.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MergedPullRequest {
    /// Repository path on the forge.
    pub repo: String,
    /// Number.
    pub number: u64,
    /// Options it was merged with.
    pub options: MergeOptions,
}

/// A host known to the fake: provider, accepted token, user.
type FakeHost = (ForgeKind, String, String);

#[derive(Default)]
struct ForgeState {
    /// `(repo path, branch)` → open pull request.
    open: HashMap<(String, String), PullRequestStatus>,
    /// `(repo path, branch)` → CI run of the branch head.
    ci: HashMap<(String, String), CiRun>,
    created: Vec<CreatedPullRequest>,
    merged: Vec<MergedPullRequest>,
    repos: HashMap<String, Vec<RemoteRepo>>,
    hosts: HashMap<String, FakeHost>,
    /// Repo paths whose calls fail.
    failing: HashMap<String, ForgeError>,
    /// Repo paths whose pull request reads alone fail.
    failing_pull_requests: HashMap<String, ForgeError>,
    reads: usize,
}

/// An in-memory forge serving every host it was told about.
#[derive(Default)]
pub struct FakeForge {
    state: Mutex<ForgeState>,
    /// Repo paths whose status reads wait, with how many are waiting.
    held: Mutex<HashMap<String, usize>>,
    released: Condvar,
}

impl FakeForge {
    /// An empty forge.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Declares a host with its provider, the token it accepts and its user.
    pub fn with_host(&self, host: &str, kind: ForgeKind, token: &str, user: &str) {
        lock(&self.state)
            .hosts
            .insert(host.into(), (kind, token.into(), user.into()));
    }

    /// Declares an open pull request for `branch` of repo `path`.
    pub fn with_open(&self, path: &str, branch: &str, pr: PullRequestStatus) {
        lock(&self.state)
            .open
            .insert((path.into(), branch.into()), pr);
    }

    /// Declares the CI run of `branch` of repo `path`.
    pub fn with_ci(&self, path: &str, branch: &str, run: CiRun) {
        lock(&self.state)
            .ci
            .insert((path.into(), branch.into()), run);
    }

    /// Declares the repositories of a namespace.
    pub fn with_repos(&self, namespace: &str, repos: Vec<RemoteRepo>) {
        lock(&self.state).repos.insert(namespace.into(), repos);
    }

    /// Makes every call on repo `path` fail with `error`.
    pub fn fail(&self, path: &str, error: ForgeError) {
        lock(&self.state).failing.insert(path.into(), error);
    }

    /// Makes the pull request reads of repo `path` fail with `error`, or
    /// succeed again with `None`; its CI reads still answer.
    pub fn fail_pull_requests(&self, path: &str, error: Option<ForgeError>) {
        let mut s = lock(&self.state);
        match error {
            Some(e) => s.failing_pull_requests.insert(path.into(), e),
            None => s.failing_pull_requests.remove(path),
        };
    }

    /// Pull requests created so far.
    #[must_use]
    pub fn created(&self) -> Vec<CreatedPullRequest> {
        lock(&self.state).created.clone()
    }

    /// Pull requests merged so far.
    #[must_use]
    pub fn merged(&self) -> Vec<MergedPullRequest> {
        lock(&self.state).merged.clone()
    }

    /// Makes the status reads of repo `path` wait until [`Self::release`]
    /// (a slow forge).
    pub fn hold(&self, path: &str) {
        lock(&self.held).insert(path.into(), 0);
    }

    /// Number of status reads of repo `path` waiting on [`Self::hold`].
    #[must_use]
    pub fn waiting(&self, path: &str) -> usize {
        lock(&self.held).get(path).copied().unwrap_or(0)
    }

    /// Lets the held status reads of repo `path` go on.
    pub fn release(&self, path: &str) {
        lock(&self.held).remove(path);
        self.released.notify_all();
    }

    /// Number of status reads served.
    #[must_use]
    pub fn reads(&self) -> usize {
        lock(&self.state).reads
    }

    fn check(&self, repo: &ForgeRepo) -> Result<(), ForgeError> {
        match lock(&self.state).failing.get(&repo.path) {
            Some(e) => Err(e.clone()),
            None => Ok(()),
        }
    }
}

impl PullRequestReader for FakeForge {
    fn pull_request_for_branch(
        &self,
        repo: &ForgeRepo,
        branch: &str,
    ) -> Result<Option<PullRequestStatus>, ForgeError> {
        self.check(repo)?;
        lock(&self.state).reads += 1;
        let mut held = lock(&self.held);
        if let Some(n) = held.get_mut(&repo.path) {
            *n += 1;
        }
        while held.contains_key(&repo.path) {
            held = self
                .released
                .wait(held)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        drop(held);
        let s = lock(&self.state);
        if let Some(e) = s.failing_pull_requests.get(&repo.path) {
            return Err(e.clone());
        }
        Ok(s.open.get(&(repo.path.clone(), branch.to_owned())).cloned())
    }

    fn open_pull_requests(&self, repo: &ForgeRepo) -> Result<Vec<PullRequestStatus>, ForgeError> {
        self.check(repo)?;
        let mut open: Vec<PullRequestStatus> = lock(&self.state)
            .open
            .iter()
            .filter(|((path, _), _)| *path == repo.path)
            .map(|(_, pr)| pr.clone())
            .collect();
        open.sort_by_key(|pr| pr.number);
        Ok(open)
    }
}

impl CiReader for FakeForge {
    fn ci_for_branch(&self, repo: &ForgeRepo, branch: &str) -> Result<Option<CiRun>, ForgeError> {
        self.check(repo)?;
        Ok(lock(&self.state)
            .ci
            .get(&(repo.path.clone(), branch.to_owned()))
            .cloned())
    }
}

impl PullRequestWriter for FakeForge {
    fn create_pull_request(
        &self,
        repo: &ForgeRepo,
        draft: &PullRequestDraft,
    ) -> Result<PullRequestRef, ForgeError> {
        self.check(repo)?;
        let mut s = lock(&self.state);
        let number = u64::try_from(s.created.len()).unwrap_or(0) + 1;
        let url = format!("https://{}/{}/pull/{number}", repo.account.host, repo.path);
        s.created.push(CreatedPullRequest {
            repo: repo.path.clone(),
            draft: draft.clone(),
            number,
            description: draft.description.clone(),
        });
        s.open.insert(
            (repo.path.clone(), draft.source.clone()),
            PullRequestStatus {
                number,
                title: draft.title.clone(),
                url: url.clone(),
                draft: draft.draft,
                source: draft.source.clone(),
                target: draft.target.clone(),
                head_sha: String::new(),
                author: String::new(),
                ci: None,
                review: ReviewState::Pending,
                merge: MergeState::Unknown,
            },
        );
        Ok(PullRequestRef { number, url })
    }

    fn set_pull_request_description(
        &self,
        repo: &ForgeRepo,
        number: u64,
        description: &str,
    ) -> Result<(), ForgeError> {
        self.check(repo)?;
        let mut s = lock(&self.state);
        let pr = s
            .created
            .iter_mut()
            .find(|c| c.repo == repo.path && c.number == number)
            .ok_or_else(|| ForgeError::NotFound {
                host: repo.account.host.clone(),
                what: format!("pull request {number}"),
            })?;
        description.clone_into(&mut pr.description);
        Ok(())
    }

    /// Refuses a draft, or a head that moved since `expected_head`, as the
    /// forges do.
    fn merge_pull_request(
        &self,
        repo: &ForgeRepo,
        number: u64,
        options: &MergeOptions,
    ) -> Result<MergeReport, ForgeError> {
        self.check(repo)?;
        let mut s = lock(&self.state);
        let key = s
            .open
            .iter()
            .find(|((path, _), pr)| *path == repo.path && pr.number == number)
            .map(|(k, pr)| (k.clone(), pr.clone()));
        let Some((key, pr)) = key else {
            return Err(ForgeError::NotFound {
                host: repo.account.host.clone(),
                what: format!("pull request {number}"),
            });
        };
        let refuse = |message: &str| ForgeError::Http {
            host: repo.account.host.clone(),
            status: 405,
            message: message.into(),
        };
        if pr.draft {
            return Err(refuse("draft"));
        }
        if options
            .expected_head
            .as_ref()
            .is_some_and(|sha| *sha != pr.head_sha)
        {
            return Err(refuse("head branch was modified"));
        }
        s.open.remove(&key);
        s.merged.push(MergedPullRequest {
            repo: repo.path.clone(),
            number,
            options: options.clone(),
        });
        Ok(MergeReport::default())
    }
}

impl ForgeDirectory for FakeForge {
    fn namespaces(&self, account: &ForgeAccount) -> Result<Vec<Namespace>, ForgeError> {
        let s = lock(&self.state);
        let mut out = vec![Namespace {
            path: account.user.clone(),
            kind: NamespaceKind::User,
        }];
        let mut others: Vec<&String> = s.repos.keys().filter(|k| **k != account.user).collect();
        others.sort();
        out.extend(others.into_iter().map(|p| Namespace {
            path: p.clone(),
            kind: NamespaceKind::Group,
        }));
        Ok(out)
    }

    fn repositories(
        &self,
        _account: &ForgeAccount,
        namespace: &Namespace,
    ) -> Result<Vec<RemoteRepo>, ForgeError> {
        Ok(lock(&self.state)
            .repos
            .get(&namespace.path)
            .cloned()
            .unwrap_or_default())
    }
}

impl ForgeSignIn for FakeForge {
    fn kinds(&self) -> Vec<ForgeKind> {
        vec![ForgeKind::github(), ForgeKind::gitlab()]
    }

    fn verify(
        &self,
        host: &str,
        kind: Option<&ForgeKind>,
        token: &ForgeToken,
    ) -> Result<ForgeAccount, ForgeError> {
        let s = lock(&self.state);
        let (real, valid, user) = s.hosts.get(host).ok_or_else(|| ForgeError::Network {
            host: host.into(),
            message: "unknown host".into(),
        })?;
        if kind.is_some_and(|k| k != real) {
            return Err(ForgeError::UnknownForge(host.into()));
        }
        if token.expose() != valid {
            return Err(ForgeError::Unauthorized {
                host: host.into(),
                status: 401,
            });
        }
        Ok(ForgeAccount {
            host: host.into(),
            kind: real.clone(),
            api_url: format!("https://{host}/api"),
            user: user.clone(),
        })
    }
}

/// An in-memory keychain and account list.
#[derive(Default)]
pub struct InMemoryCredentials {
    tokens: Mutex<HashMap<String, ForgeToken>>,
    accounts: Mutex<Vec<ForgeAccount>>,
}

impl InMemoryCredentials {
    /// Empty.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl CredentialStore for InMemoryCredentials {
    fn token(&self, host: &str) -> Result<Option<ForgeToken>, ForgeError> {
        Ok(lock(&self.tokens).get(host).cloned())
    }
    fn set_token(&self, host: &str, token: &ForgeToken) -> Result<(), ForgeError> {
        lock(&self.tokens).insert(host.into(), token.clone());
        Ok(())
    }
    fn delete_token(&self, host: &str) -> Result<(), ForgeError> {
        lock(&self.tokens).remove(host);
        Ok(())
    }
}

impl ForgeAccountStore for InMemoryCredentials {
    fn accounts(&self) -> Result<Vec<ForgeAccount>, ForgeError> {
        Ok(lock(&self.accounts).clone())
    }
    fn save_accounts(&self, accounts: &[ForgeAccount]) -> Result<(), ForgeError> {
        *lock(&self.accounts) = accounts.to_vec();
        Ok(())
    }
}

/// A release feed answering with whatever release is set.
#[derive(Default)]
pub struct FakeReleaseFeed {
    latest: Mutex<Option<Release>>,
}

impl FakeReleaseFeed {
    /// Publishes `release` as the latest one.
    pub fn publish(&self, release: Release) {
        *lock(&self.latest) = Some(release);
    }
}

impl ReleaseFeed for FakeReleaseFeed {
    fn latest_release(&self) -> Result<Option<Release>, ForgeError> {
        Ok(lock(&self.latest).clone())
    }
}
