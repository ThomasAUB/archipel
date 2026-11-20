//! Issues (spec §8): the open issues of repos, a new issue for one repo
//! (with the repo's own labels and assignees), closing (with a reason and
//! a comment) and reopening.
//!
//! Each repo's issues are those of the forge repository its forge status
//! uses (the remote of its branch's upstream first), so a fork's issues go
//! to the fork, never silently to its upstream. Every call runs on the
//! scheduler's network pool.

use archipel_model::RepoId;
use archipel_model::forge::{CloseReason, ForgeRepo, Issue, IssueChoices, IssueDraft, IssueRef};
use archipel_ops::{OpClass, OpSpec};

use super::{App, AppError, lock};

/// The open issues of one repo.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepoIssues {
    /// Repository.
    pub repo: RepoId,
    /// Display name.
    pub name: String,
    /// Forge repository, when found.
    pub forge: Option<ForgeRepo>,
    /// The open issues (oldest first), or why they cannot be listed.
    pub result: Result<Vec<Issue>, String>,
}

/// A repo a new issue can be filed on, with what the issue can carry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IssueTarget {
    /// Repository.
    pub repo: RepoId,
    /// Display name.
    pub name: String,
    /// Forge repository, when found.
    pub forge: Option<ForgeRepo>,
    /// Its labels and assignees, or why no issue can be filed there
    /// (issues disabled, not signed in, no forge).
    pub choices: Result<IssueChoices, String>,
}

/// An issue closed from Archipel in this session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClosedIssue {
    /// Repository.
    pub repo: RepoId,
    /// Forge repository it was closed on.
    pub forge: ForgeRepo,
    /// The issue, as listed before closing.
    pub issue: Issue,
}

/// Issues remembered by [`App::closed_issues`].
const MAX_CLOSED_ISSUES: usize = 50;

impl App {
    /// The open issues of `ids` (all when empty, see
    /// [`App::resolve_targets`]), one network read per repo, in parallel; in
    /// project order.
    pub async fn open_issues(&self, ids: &[RepoId]) -> Result<Vec<RepoIssues>, AppError> {
        Ok(self
            .per_forge_repo(ids, "List issues", |forge, repo| {
                forge.issues.open_issues(repo)
            })
            .await?
            .into_iter()
            .map(|r| RepoIssues {
                repo: r.repo,
                name: r.name,
                forge: r.forge,
                result: r.result,
            })
            .collect())
    }

    /// The repos of `ids` (all when empty) a new issue can go to, each with
    /// its labels and assignees, read in parallel; in project order. A repo
    /// with issues disabled says so here, before anything is typed.
    pub async fn issue_targets(&self, ids: &[RepoId]) -> Result<Vec<IssueTarget>, AppError> {
        Ok(self
            .per_forge_repo(ids, "Read issue labels", |forge, repo| {
                forge.issues.issue_choices(repo)
            })
            .await?
            .into_iter()
            .map(|r| IssueTarget {
                repo: r.repo,
                name: r.name,
                forge: r.forge,
                choices: r.result,
            })
            .collect())
    }

    /// Opens an issue on `forge_repo` (the forge repository of `repo`).
    pub async fn create_issue(
        &self,
        repo: RepoId,
        forge_repo: &ForgeRepo,
        draft: IssueDraft,
    ) -> Result<IssueRef, AppError> {
        let draft = IssueDraft {
            title: draft.title.trim().to_owned(),
            ..draft
        };
        if draft.title.is_empty() {
            return Err(AppError::InvalidInput("an issue needs a title".into()));
        }
        let forge = self.services.forge.clone();
        let target = forge_repo.clone();
        let spec = OpSpec::new(
            Some(repo),
            OpClass::Network,
            format!("Create an issue on {}", forge_repo.path),
        );
        Ok(self
            .scheduler
            .submit(spec, move |_| {
                Ok(forge.issue_writer.create_issue(&target, &draft)?)
            })
            .wait()
            .await?)
    }

    /// Closes `issue` of `forge_repo`, after posting `comment` on it when
    /// it is not blank. `reason` is dropped on GitLab. The issue is then
    /// listed by [`App::closed_issues`] until reopened.
    pub async fn close_issue(
        &self,
        repo: RepoId,
        forge_repo: &ForgeRepo,
        issue: &Issue,
        reason: CloseReason,
        comment: Option<String>,
    ) -> Result<(), AppError> {
        let comment = comment.filter(|c| !c.trim().is_empty());
        let forge = self.services.forge.clone();
        let (target, number) = (forge_repo.clone(), issue.number);
        let spec = OpSpec::new(
            Some(repo),
            OpClass::Network,
            format!("Close issue {} #{number}", forge_repo.path),
        );
        self.scheduler
            .submit(spec, move |_| {
                Ok(forge
                    .issue_writer
                    .close_issue(&target, number, reason, comment.as_deref())?)
            })
            .wait()
            .await?;
        if let Some(s) = lock(&self.session).as_mut() {
            s.closed_issues
                .retain(|c| !(c.forge == *forge_repo && c.issue.number == number));
            if s.closed_issues.len() >= MAX_CLOSED_ISSUES {
                s.closed_issues.remove(0);
            }
            s.closed_issues.push(ClosedIssue {
                repo,
                forge: forge_repo.clone(),
                issue: issue.clone(),
            });
        }
        Ok(())
    }

    /// Reopens issue `number` of `forge_repo` (undoes a close).
    pub async fn reopen_issue(
        &self,
        repo: RepoId,
        forge_repo: &ForgeRepo,
        number: u64,
    ) -> Result<(), AppError> {
        let forge = self.services.forge.clone();
        let target = forge_repo.clone();
        let spec = OpSpec::new(
            Some(repo),
            OpClass::Network,
            format!("Reopen issue {} #{number}", forge_repo.path),
        );
        self.scheduler
            .submit(spec, move |_| {
                Ok(forge.issue_writer.reopen_issue(&target, number)?)
            })
            .wait()
            .await?;
        if let Some(s) = lock(&self.session).as_mut() {
            s.closed_issues
                .retain(|c| !(c.forge == *forge_repo && c.issue.number == number));
        }
        Ok(())
    }

    /// The issues closed from Archipel in this project session and not
    /// reopened since, oldest first.
    #[must_use]
    pub fn closed_issues(&self) -> Vec<ClosedIssue> {
        lock(&self.session)
            .as_ref()
            .map(|s| s.closed_issues.clone())
            .unwrap_or_default()
    }
}
