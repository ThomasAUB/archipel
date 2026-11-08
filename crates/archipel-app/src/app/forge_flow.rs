//! Forge use cases (spec §8): per-repo status (pull request, CI, review),
//! sign-in, pull requests across repos (open, list, merge), listing what an
//! account can clone (the clone itself is in `clone`).
//!
//! Every network call runs on the scheduler's network pool; the async
//! methods only await operation handles, so frontends run them on the app
//! runtime and never block their UI thread.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use archipel_model::forge::{
    ForgeAccount, ForgeKind, ForgeRepo, ForgeToken, MergeMethod, MergeOptions, MergeReport,
    Namespace, PullRequestDraft, PullRequestRef, PullRequestStatus, RemoteRepo, normalize_host,
};
use archipel_model::ports::RemoteInfo;
use archipel_model::requests::PushRequest;
use archipel_model::{ForgeError, RepoId, RepoPath, Timestamp};
use archipel_ops::{GitPorts, OpClass, OpError, OpSpec};

use super::{App, AppError, AppEvent, lock};
use crate::dashboard::DashboardRow;
use crate::forge::{
    BranchCi, ForgeLimit, ForgePorts, ForgeRegistry, ForgeStatus, Located, linked_description,
    locate, pick_remote,
};

/// One repository of a pull request plan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PullRequestTarget {
    /// Repository.
    pub repo: RepoId,
    /// Display name.
    pub name: String,
    /// Local path.
    pub path: RepoPath,
    /// Source branch (the current branch).
    pub branch: Option<String>,
    /// Suggested target branch (the remote's `main` or `master`).
    pub target: String,
    /// Remote the branch is pushed to.
    pub remote: String,
    /// Forge repository, when found.
    pub forge: Option<ForgeRepo>,
    /// The branch must be pushed first (no upstream, or commits ahead).
    pub needs_push: bool,
    /// Why no pull request can be opened from this repo.
    pub problem: Option<String>,
    /// Included in the run.
    pub selected: bool,
}

/// What [`App::prepare_pull_requests`] found: one target per repo and a
/// prefilled title.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PullRequestPlan {
    /// Targets, in project order.
    pub targets: Vec<PullRequestTarget>,
    /// Title suggestion (subject of the last commit of the first target).
    pub title: String,
}

impl PullRequestPlan {
    /// Selects or deselects a repo (repos with a problem stay out).
    pub fn select(&mut self, repo: RepoId, selected: bool) {
        for t in &mut self.targets {
            if t.repo == repo && t.problem.is_none() {
                t.selected = selected;
            }
        }
    }

    /// Selected targets.
    pub fn selected(&self) -> impl Iterator<Item = &PullRequestTarget> {
        self.targets
            .iter()
            .filter(|t| t.selected && t.problem.is_none())
    }

    /// The target branch shared by every selected repo, if they agree.
    #[must_use]
    pub fn common_target(&self) -> Option<String> {
        let mut targets = self.selected().map(|t| t.target.as_str());
        let first = targets.next()?;
        targets.all(|t| t == first).then(|| first.to_owned())
    }

    /// Whether the selected repos are on different branches.
    #[must_use]
    pub fn branch_mismatch(&self) -> bool {
        let mut branches = self.selected().map(|t| t.branch.as_deref());
        let first = branches.next();
        branches.any(|b| Some(b) != first)
    }
}

/// Result of opening the pull request of one repo.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PullRequestOutcome {
    /// Repository.
    pub repo: RepoId,
    /// Display name.
    pub name: String,
    /// The pull request, or the error.
    pub result: Result<PullRequestRef, String>,
    /// Linking it to the other pull requests failed.
    pub link_error: Option<String>,
}

/// The open pull requests of one repo.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepoPullRequests {
    /// Repository.
    pub repo: RepoId,
    /// Display name.
    pub name: String,
    /// Forge repository, when found.
    pub forge: Option<ForgeRepo>,
    /// The open pull requests (oldest first), or why they cannot be listed.
    pub result: Result<Vec<PullRequestStatus>, String>,
}

/// What a forge read is for: a repo's branch.
struct ForgeTarget {
    path: RepoPath,
    branch: String,
    /// Remote of the branch's upstream, tried first.
    preferred: Option<String>,
}

/// What one forge read found for a repo.
struct ForgeRead {
    status: ForgeStatus,
    ci: BranchCi,
    /// The status read hit the rate limit: the last known status stays.
    status_limited: bool,
    /// The CI read hit the rate limit: the last known CI stays.
    ci_limited: bool,
    /// The rate limit met: host, and when it accepts calls again.
    limit: Option<(String, Option<Timestamp>)>,
    /// A real failure, for the drawer.
    error: Option<ForgeError>,
}

/// How long a rate-limited host is left alone when it did not say.
const DEFAULT_BACKOFF_SECS: i64 = 60;

/// The host and retry time of a rate limit error.
fn rate_limit(error: &ForgeError) -> Option<(String, Option<Timestamp>)> {
    match error {
        ForgeError::RateLimited { host, retry_at } => Some((host.clone(), *retry_at)),
        _ => None,
    }
}

/// Reads the forge status and branch CI of one repo (blocking; on a
/// scheduler worker). A host in `backoff` (rate limited until then) is not
/// called.
fn read_forge(
    git: &GitPorts,
    forge: &ForgePorts,
    links: &ForgeRegistry,
    target: &ForgeTarget,
    backoff: &HashMap<String, Timestamp>,
) -> ForgeRead {
    let (path, branch) = (&target.path, target.branch.as_str());
    let remotes = match git.remotes.remotes(path) {
        Ok(r) => r,
        Err(e) => {
            return ForgeRead {
                status: ForgeStatus::Error(e.to_string()),
                ci: BranchCi::Unknown,
                status_limited: false,
                ci_limited: false,
                limit: None,
                error: None,
            };
        }
    };
    let accounts = forge.accounts.accounts().unwrap_or_else(|e| {
        tracing::warn!(error = %e, "cannot read forge accounts");
        Vec::new()
    });
    let located = locate(&remotes, target.preferred.as_deref(), &accounts, links);
    if let Located::Repo { repo, .. } = &located {
        let host = &repo.account.host;
        if let Some(&until) = backoff.get(host).filter(|t| **t > Timestamp::now()) {
            let message = ForgeError::RateLimited {
                host: host.clone(),
                retry_at: Some(until),
            }
            .to_string();
            return ForgeRead {
                status: ForgeStatus::Error(message.clone()),
                ci: BranchCi::Error(message),
                status_limited: true,
                ci_limited: true,
                limit: Some((host.clone(), Some(until))),
                error: None,
            };
        }
    }
    let mut ci_limit = None;
    let ci = match &located {
        Located::Repo { repo, .. } => match forge.ci.ci_for_branch(repo, branch) {
            Ok(Some(run)) => BranchCi::Run(run),
            Ok(None) => BranchCi::None,
            Err(ForgeError::NotSignedIn { .. }) => BranchCi::Unknown,
            // Reported once, by the pull request read below.
            Err(e) => {
                ci_limit = rate_limit(&e);
                BranchCi::Error(e.to_string())
            }
        },
        _ => BranchCi::Unknown,
    };
    let (status, error) = pull_request_status(forge, located, branch);
    let status_limit = error.as_ref().and_then(rate_limit);
    ForgeRead {
        status,
        ci,
        status_limited: status_limit.is_some(),
        ci_limited: ci_limit.is_some(),
        limit: status_limit.or(ci_limit),
        error,
    }
}

/// The pull request status of a located repo, and the error for the drawer.
fn pull_request_status(
    forge: &ForgePorts,
    located: Located,
    branch: &str,
) -> (ForgeStatus, Option<ForgeError>) {
    match located {
        Located::NotOnForge => (ForgeStatus::NotOnForge, None),
        Located::SignedOut { host } => (ForgeStatus::SignedOut { host }, None),
        Located::Repo { repo, .. } => {
            match forge.pull_requests.pull_request_for_branch(&repo, branch) {
                Ok(Some(pr)) => (ForgeStatus::Open(pr), None),
                Ok(None) => (
                    ForgeStatus::NoPullRequest {
                        branch: branch.to_owned(),
                    },
                    None,
                ),
                Err(ForgeError::NotSignedIn { host }) => (ForgeStatus::SignedOut { host }, None),
                Err(e) => (ForgeStatus::Error(e.to_string()), Some(e)),
            }
        }
    }
}

/// The remote's usual default branch: `main`, else `master`, else `main`.
fn default_target(git: &GitPorts, path: &RepoPath, remote: &str) -> String {
    let branches = git.refs.branches(path).unwrap_or_default();
    ["main", "master"]
        .into_iter()
        .find(|b| {
            branches
                .iter()
                .any(|i| i.is_remote && i.name == format!("{remote}/{b}"))
        })
        .unwrap_or("main")
        .to_owned()
}

/// Remote name of an upstream like `origin/main`.
fn upstream_remote(row: &DashboardRow) -> Option<String> {
    let upstream = row.summary()?.upstream.as_ref()?;
    upstream.name.split_once('/').map(|(r, _)| r.to_owned())
}

fn plan_target(
    git: &GitPorts,
    accounts: &[ForgeAccount],
    links: &ForgeRegistry,
    row: &DashboardRow,
) -> PullRequestTarget {
    let summary = row.summary();
    let branch = summary.and_then(|s| s.head.branch()).map(str::to_owned);
    let preferred = upstream_remote(row);
    let remotes: Vec<RemoteInfo> = git.remotes.remotes(&row.path).unwrap_or_default();
    let remote = pick_remote(&remotes, preferred.as_deref())
        .map_or_else(|| "origin".to_owned(), |r| r.name.clone());
    let (forge, mut problem) = match locate(&remotes, preferred.as_deref(), accounts, links) {
        Located::Repo { repo, .. } => (Some(repo), None),
        Located::SignedOut { host } => (None, Some(format!("not signed in to {host}"))),
        Located::NotOnForge => (None, Some("no remote on a known forge".to_owned())),
    };
    let target = default_target(git, &row.path, &remote);
    if problem.is_none() {
        problem = match (&branch, &row.forge) {
            (None, _) => Some("no branch checked out".into()),
            (Some(b), _) if *b == target => Some(format!("already on {target}")),
            (_, ForgeStatus::Open(pr)) => Some(format!("pull request #{} already open", pr.number)),
            _ => None,
        };
    }
    let needs_push = summary
        .and_then(|s| s.upstream.as_ref())
        .is_none_or(|u| u.gone || u.ahead > 0);
    PullRequestTarget {
        repo: row.id,
        name: row.name.clone(),
        path: row.path.clone(),
        branch,
        target,
        remote,
        forge,
        needs_push,
        selected: problem.is_none(),
        problem,
    }
}

impl App {
    // ----- accounts -------------------------------------------------------

    /// Signed-in forge hosts (small local read).
    #[must_use]
    pub fn forge_accounts(&self) -> Vec<ForgeAccount> {
        self.services.forge.accounts.accounts().unwrap_or_else(|e| {
            tracing::warn!(error = %e, "cannot read forge accounts");
            Vec::new()
        })
    }

    /// Forge providers available for sign-in.
    #[must_use]
    pub fn forge_kinds(&self) -> Vec<ForgeKind> {
        self.services.forge.sign_in.kinds()
    }

    /// Verifies a personal access token for `host` (provider detected when
    /// `kind` is `None`), stores it in the keychain and remembers the
    /// account, then re-reads every forge status.
    pub async fn sign_in(
        &self,
        host: &str,
        kind: Option<ForgeKind>,
        token: &str,
    ) -> Result<ForgeAccount, AppError> {
        let token = ForgeToken::new(token);
        let host = normalize_host(host);
        if host.is_empty() {
            return Err(AppError::InvalidInput("type the forge host".into()));
        }
        if token.is_empty() {
            return Err(AppError::InvalidInput(
                "paste a personal access token".into(),
            ));
        }
        let forge = self.services.forge.clone();
        let spec = OpSpec::new(None, OpClass::Network, format!("Sign in to {host}"));
        let account = self
            .scheduler
            .submit(spec, move |_| {
                let account = forge.sign_in.verify(&host, kind.as_ref(), &token)?;
                forge.credentials.set_token(&account.host, &token)?;
                let mut accounts = forge.accounts.accounts()?;
                accounts.retain(|a| a.host != account.host);
                accounts.push(account.clone());
                forge.accounts.save_accounts(&accounts)?;
                Ok(account)
            })
            .wait()
            .await?;
        self.emit(AppEvent::ForgeAccountsChanged);
        self.refresh_forge_all();
        Ok(account)
    }

    /// Forgets the token and account of a host.
    pub async fn sign_out(&self, host: &str) -> Result<(), AppError> {
        let forge = self.services.forge.clone();
        let host = host.to_owned();
        let spec = OpSpec::new(None, OpClass::Write, format!("Sign out of {host}"));
        self.scheduler
            .submit(spec, move |_| {
                forge.credentials.delete_token(&host)?;
                let mut accounts = forge.accounts.accounts()?;
                accounts.retain(|a| a.host != host);
                forge.accounts.save_accounts(&accounts)?;
                Ok(())
            })
            .wait()
            .await?;
        self.emit(AppEvent::ForgeAccountsChanged);
        self.refresh_forge_all();
        Ok(())
    }

    // ----- status ---------------------------------------------------------

    /// Re-reads the forge status of every shown repo.
    pub fn refresh_forge_all(&self) {
        for id in self.shown_ids() {
            self.load_forge(id, true);
        }
    }

    /// Re-reads forge statuses older than `forge_refresh_secs`.
    pub(super) fn refresh_forge_stale(&self) {
        for id in self.shown_ids() {
            self.load_forge(id, false);
        }
    }

    fn shown_ids(&self) -> Vec<RepoId> {
        lock(&self.session)
            .as_ref()
            .map(|s| s.rows.iter().filter(|r| !r.hidden).map(|r| r.id).collect())
            .unwrap_or_default()
    }

    /// Reads the forge status of one repo in the background, unless it was
    /// read for the same branch less than `forge_refresh_secs` ago (or
    /// `even_if_fresh`). Coalesced: at most one pending read per repo.
    pub(super) fn load_forge(&self, repo: RepoId, even_if_fresh: bool) {
        let fresh = Duration::from_secs(self.settings.forge_refresh_secs);
        let (target, name, backoff) = {
            let mut guard = lock(&self.session);
            let Some(s) = guard.as_mut() else { return };
            let Some(row) = s.rows.iter_mut().find(|r| r.id == repo && !r.hidden) else {
                return;
            };
            let Some(summary) = row.summary() else { return };
            let Some(branch) = summary.head.branch().map(str::to_owned) else {
                // Detached or no commit: nothing to show on the forge.
                let changed = row.forge != ForgeStatus::NotOnForge || row.ci != BranchCi::Unknown;
                row.forge = ForgeStatus::NotOnForge;
                row.ci = BranchCi::Unknown;
                row.forge_limit = None;
                drop(guard);
                if changed {
                    self.emit(AppEvent::RowsChanged(vec![repo]));
                }
                return;
            };
            let preferred = upstream_remote(row);
            let recent = s
                .forge_checked
                .get(&repo)
                .is_some_and(|(b, at)| *b == branch && at.elapsed() < fresh);
            if recent && !even_if_fresh {
                return;
            }
            if !s.forge_pending.insert(repo) {
                if even_if_fresh {
                    s.forge_reread.insert(repo);
                }
                return;
            }
            let target = ForgeTarget {
                path: row.path.clone(),
                branch,
                preferred,
            };
            (target, row.name.clone(), s.forge_backoff.clone())
        };
        let git = self.services.git.clone();
        let forge = self.services.forge.clone();
        let links = self.ext.forges.clone();
        let me = self.me.clone();
        let spec = OpSpec::new(Some(repo), OpClass::Network, format!("Forge status {name}"));
        let _handle = self.scheduler.submit(spec, move |_| {
            let mut read = read_forge(&git, &forge, &links, &target, &backoff);
            let error = read.error.take();
            if let Some(app) = me.upgrade() {
                app.apply_forge(repo, target.branch, read);
            }
            error.map_or(Ok(()), |e| Err(OpError::Forge(e)))
        });
    }

    fn apply_forge(&self, repo: RepoId, branch: String, read: ForgeRead) {
        let (changed, reread) = {
            let mut guard = lock(&self.session);
            let Some(s) = guard.as_mut() else { return };
            s.forge_pending.remove(&repo);
            let reread = s.forge_reread.remove(&repo);
            let row = s.rows.iter_mut().find(|r| r.id == repo);
            // A status read for a branch the repo has left is dropped.
            let current = row
                .as_ref()
                .and_then(|r| r.summary())
                .and_then(|s| s.head.branch())
                == Some(branch.as_str());
            let now = Timestamp::now();
            let retry_at = read.limit.map(|(host, retry_at)| {
                let until = retry_at.unwrap_or(Timestamp(now.0 + DEFAULT_BACKOFF_SECS));
                s.forge_backoff.insert(host, until);
                until
            });
            // When the last read of this branch succeeded.
            let read_at = s
                .forge_checked
                .get(&repo)
                .filter(|(b, _)| *b == branch)
                .map(|(_, at)| {
                    Timestamp(now.0 - i64::try_from(at.elapsed().as_secs()).unwrap_or(0))
                });
            let changed = match row {
                Some(row) if current => {
                    // Rate limited: what an earlier read of this branch
                    // found stays shown, marked as maybe out of date,
                    // rather than an error.
                    let as_of = row.forge_limit.map(|l| l.as_of).or(read_at);
                    let keep_status = read.status_limited
                        && as_of.is_some()
                        && !matches!(row.forge, ForgeStatus::Unknown | ForgeStatus::Error(_));
                    let keep_ci = read.ci_limited
                        && as_of.is_some()
                        && !matches!(row.ci, BranchCi::Unknown | BranchCi::Error(_));
                    let status = if keep_status {
                        row.forge.clone()
                    } else {
                        read.status
                    };
                    let ci = if keep_ci { row.ci.clone() } else { read.ci };
                    let limit = as_of
                        .filter(|_| keep_status || keep_ci)
                        .map(|as_of| ForgeLimit { as_of, retry_at });
                    let changed = row.forge != status || row.ci != ci || row.forge_limit != limit;
                    row.forge = status;
                    row.ci = ci;
                    row.forge_limit = limit;
                    changed
                }
                _ => false,
            };
            if current {
                s.forge_checked.insert(repo, (branch, Instant::now()));
            }
            (changed, reread)
        };
        if changed {
            self.emit(AppEvent::RowsChanged(vec![repo]));
        }
        if reread {
            self.load_forge(repo, true);
        }
    }

    // ----- pull requests --------------------------------------------------

    /// Prepares pull requests for `ids` (every shown repo when empty): the
    /// forge repo, source and target branches of each, and a title.
    pub async fn prepare_pull_requests(&self, ids: &[RepoId]) -> Result<PullRequestPlan, AppError> {
        let rows: Vec<DashboardRow> = self
            .rows()
            .into_iter()
            .filter(|r| ids.is_empty() || ids.contains(&r.id))
            .filter(|r| r.summary().is_some())
            .collect();
        if rows.is_empty() {
            return Err(AppError::InvalidInput(
                "no repository to open a pull request from".into(),
            ));
        }
        let git = self.services.git.clone();
        let forge = self.services.forge.clone();
        let links = self.ext.forges.clone();
        let spec = OpSpec::new(None, OpClass::Read, "Prepare pull requests");
        let plan = self
            .scheduler
            .submit(spec, move |_| {
                let accounts = forge.accounts.accounts().unwrap_or_default();
                let targets: Vec<PullRequestTarget> = rows
                    .iter()
                    .map(|row| plan_target(&git, &accounts, &links, row))
                    .collect();
                let title = targets
                    .iter()
                    .find(|t| t.selected)
                    .and_then(|t| {
                        let head = git.refs.resolve(&t.path, "HEAD").ok()??;
                        git.history.commit_meta(&t.path, &[head]).ok()?.pop()
                    })
                    .map(|m| m.summary)
                    .unwrap_or_default();
                Ok(PullRequestPlan { targets, title })
            })
            .wait()
            .await?;
        Ok(plan)
    }

    /// Opens one pull request per selected repo of `plan` (pushing the
    /// branch first when needed), into `target` or each repo's suggested
    /// target. With several, each description links to all of them.
    pub async fn create_pull_requests(
        &self,
        plan: &PullRequestPlan,
        title: &str,
        description: &str,
        target: Option<&str>,
        draft: bool,
    ) -> Result<Vec<PullRequestOutcome>, AppError> {
        let title = title.trim();
        if title.is_empty() {
            return Err(AppError::InvalidInput("the title is empty".into()));
        }
        let chosen: Vec<PullRequestTarget> = plan.selected().cloned().collect();
        if chosen.is_empty() {
            return Err(AppError::InvalidInput("no repository selected".into()));
        }
        let target = target.map(str::trim).filter(|t| !t.is_empty());
        let mut jobs = Vec::new();
        for t in &chosen {
            let (Some(repo), Some(branch)) = (t.forge.clone(), t.branch.clone()) else {
                continue;
            };
            let draft = PullRequestDraft {
                source: branch.clone(),
                target: target.map_or_else(|| t.target.clone(), str::to_owned),
                title: title.to_owned(),
                description: description.to_owned(),
                draft,
            };
            let git = self.services.git.clone();
            let forge = self.services.forge.clone();
            let (path, remote, needs_push) = (t.path.clone(), t.remote.clone(), t.needs_push);
            let spec = OpSpec::new(
                Some(t.repo),
                OpClass::Network,
                format!("Open pull request for {}", t.name),
            );
            let handle = self.scheduler.submit(spec, move |ctx| {
                if needs_push {
                    let push = PushRequest {
                        remote: Some(remote),
                        refspec: Some(branch),
                        set_upstream: true,
                        ..PushRequest::default()
                    };
                    git.remote.push(&path, &push, ctx, ctx.cancel())?;
                }
                let created = forge
                    .pull_request_writer
                    .create_pull_request(&repo, &draft)?;
                Ok((repo, created))
            });
            jobs.push((t.repo, t.name.clone(), handle));
        }
        let mut outcomes = Vec::new();
        let mut created: Vec<(RepoId, ForgeRepo, PullRequestRef)> = Vec::new();
        for (repo, name, handle) in jobs {
            let result = match handle.wait().await {
                Ok((forge_repo, pr)) => {
                    created.push((repo, forge_repo, pr.clone()));
                    Ok(pr)
                }
                Err(e) => Err(e.to_string()),
            };
            outcomes.push(PullRequestOutcome {
                repo,
                name,
                result,
                link_error: None,
            });
        }
        if created.len() > 1 {
            let link_errors = self.link_pull_requests(&created, description).await;
            for o in &mut outcomes {
                o.link_error = link_errors.get(&o.repo).cloned();
            }
        }
        for o in &outcomes {
            self.refresh_repo(o.repo);
            self.load_forge(o.repo, true);
        }
        let ok = outcomes.iter().filter(|o| o.result.is_ok()).count();
        self.emit(AppEvent::Notice(format!(
            "Pull requests: {ok} opened, {} failed",
            outcomes.len() - ok
        )));
        Ok(outcomes)
    }

    /// Rewrites the description of each created pull request with links to
    /// all of them. Returns the failures per repo.
    async fn link_pull_requests(
        &self,
        created: &[(RepoId, ForgeRepo, PullRequestRef)],
        description: &str,
    ) -> HashMap<RepoId, String> {
        let all: Vec<(String, PullRequestRef)> = created
            .iter()
            .map(|(_, repo, pr)| (repo.path.clone(), pr.clone()))
            .collect();
        let body = linked_description(description, &all);
        let mut handles = Vec::new();
        for (id, repo, pr) in created {
            let forge = self.services.forge.clone();
            let (repo, number, body) = (repo.clone(), pr.number, body.clone());
            let spec = OpSpec::new(
                Some(*id),
                OpClass::Network,
                format!("Link pull request {} #{number}", repo.path),
            );
            handles.push((
                *id,
                self.scheduler.submit(spec, move |_| {
                    forge
                        .pull_request_writer
                        .set_pull_request_description(&repo, number, &body)
                        .map_err(OpError::from)
                }),
            ));
        }
        let mut errors = HashMap::new();
        for (id, handle) in handles {
            if let Err(e) = handle.wait().await {
                errors.insert(id, e.to_string());
            }
        }
        errors
    }

    /// The open pull requests of `ids` (every shown repo when empty), one
    /// network read per repo, in parallel; in project order.
    pub async fn open_pull_requests(
        &self,
        ids: &[RepoId],
    ) -> Result<Vec<RepoPullRequests>, AppError> {
        let rows: Vec<DashboardRow> = self
            .rows()
            .into_iter()
            .filter(|r| ids.is_empty() || ids.contains(&r.id))
            .collect();
        if rows.is_empty() {
            return Err(AppError::InvalidInput("no repository to list".into()));
        }
        let mut jobs = Vec::new();
        for row in rows {
            let git = self.services.git.clone();
            let forge = self.services.forge.clone();
            let links = self.ext.forges.clone();
            let (path, preferred) = (row.path.clone(), upstream_remote(&row));
            let spec = OpSpec::new(
                Some(row.id),
                OpClass::Network,
                format!("List pull requests of {}", row.name),
            );
            let handle = self.scheduler.submit(spec, move |_| {
                let remotes = git.remotes.remotes(&path)?;
                let accounts = forge.accounts.accounts()?;
                Ok(
                    match locate(&remotes, preferred.as_deref(), &accounts, &links) {
                        Located::Repo { repo, .. } => {
                            let open = forge.pull_requests.open_pull_requests(&repo);
                            (Some(repo), open.map_err(|e| e.to_string()))
                        }
                        Located::SignedOut { host } => {
                            (None, Err(format!("not signed in to {host}")))
                        }
                        Located::NotOnForge => (None, Err("no remote on a known forge".to_owned())),
                    },
                )
            });
            jobs.push((row.id, row.name, handle));
        }
        let mut out = Vec::new();
        for (repo, name, handle) in jobs {
            let (forge, result) = handle
                .wait()
                .await
                .unwrap_or_else(|e| (None, Err(e.to_string())));
            out.push(RepoPullRequests {
                repo,
                name,
                forge,
                result,
            });
        }
        Ok(out)
    }

    /// Merges `pr` on the forge, only if its head is still the commit it was
    /// listed with; then fetches the repo and re-reads its forge status.
    pub async fn merge_pull_request(
        &self,
        repo: RepoId,
        forge_repo: &ForgeRepo,
        pr: &PullRequestStatus,
        method: MergeMethod,
        delete_source_branch: bool,
    ) -> Result<MergeReport, AppError> {
        if pr.draft {
            return Err(AppError::InvalidInput(format!(
                "#{} is a draft: mark it ready on the forge first",
                pr.number
            )));
        }
        let options = MergeOptions {
            method,
            delete_source_branch,
            expected_head: (!pr.head_sha.is_empty()).then(|| pr.head_sha.clone()),
        };
        let forge = self.services.forge.clone();
        let (target, number) = (forge_repo.clone(), pr.number);
        let spec = OpSpec::new(
            Some(repo),
            OpClass::Network,
            format!("Merge pull request {} #{number}", forge_repo.path),
        );
        let report = self
            .scheduler
            .submit(spec, move |_| {
                Ok(forge
                    .pull_request_writer
                    .merge_pull_request(&target, number, &options)?)
            })
            .wait()
            .await?;
        // The target branch moved on the forge: show it locally, and drop
        // the remote-tracking branch of a source branch deleted there.
        self.fetch(&[repo], delete_source_branch);
        self.load_forge(repo, true);
        self.emit(AppEvent::Notice(match &report.warning {
            Some(w) => format!("Merged {} #{number} ({w})", forge_repo.path),
            None => format!("Merged {} #{number}", forge_repo.path),
        }));
        Ok(report)
    }

    // ----- browse an account to clone from ---------------------------------

    /// The user's namespace, organizations and groups on an account.
    pub async fn forge_namespaces(
        &self,
        account: &ForgeAccount,
    ) -> Result<Vec<Namespace>, AppError> {
        let forge = self.services.forge.clone();
        let account = account.clone();
        let spec = OpSpec::new(
            None,
            OpClass::Network,
            format!("List namespaces on {}", account.host),
        );
        Ok(self
            .scheduler
            .submit(spec, move |_| Ok(forge.directory.namespaces(&account)?))
            .wait()
            .await?)
    }

    /// Repositories of a namespace.
    pub async fn forge_repositories(
        &self,
        account: &ForgeAccount,
        namespace: &Namespace,
    ) -> Result<Vec<RemoteRepo>, AppError> {
        let forge = self.services.forge.clone();
        let (account, namespace) = (account.clone(), namespace.clone());
        let spec = OpSpec::new(
            None,
            OpClass::Network,
            format!("List repositories of {}", namespace.path),
        );
        Ok(self
            .scheduler
            .submit(spec, move |_| {
                Ok(forge.directory.repositories(&account, &namespace)?)
            })
            .wait()
            .await?)
    }
}
