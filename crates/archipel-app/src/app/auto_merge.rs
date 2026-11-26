//! Merge a pull request once its CI passes (spec §8).
//!
//! The forge's own auto-merge is preferred: it merges even with Archipel
//! closed, and its state is read back with the pull request
//! ([`PullRequestStatus::auto_merge`]). When the forge refuses it (the
//! setting is off on the repository, a token scope is missing, nothing is
//! left to wait for), Archipel watches the pull request itself and merges
//! it once its CI passes, only while it runs: a [`LocalAutoMerge`].
//!
//! A watched pull request stays armed when its CI fails or it gets a
//! conflict, as the forges' own auto-merge does: a rerun or a new push can
//! still make it pass. It is dropped when the pull request is no longer
//! open (merged or closed elsewhere).

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use archipel_model::forge::{
    AutoMergeReport, CiState, ForgeKind, ForgeRepo, MergeMethod, MergeOptions, MergeState,
    PullRequestStatus,
};
use archipel_model::{ForgeError, RepoId};
use archipel_ops::{OpClass, OpSpec};

use super::forge_flow::{forge_repo, upstream_remote};
use super::session::Session;
use super::{App, AppError, AppEvent, lock};
use crate::forge::AutoMergeWatch;

/// How often a watched pull request is checked when CI polling is off.
const WATCH_FALLBACK_SECS: u64 = 30;

/// Fewest seconds between two checks of a watched pull request.
const WATCH_MIN_SECS: u64 = 1;

/// Tells the armings of a pull request apart: the loop of an earlier one
/// (cancelled, then armed again) stops.
static GENERATION: AtomicU64 = AtomicU64::new(1);

/// What [`App::arm_auto_merge`] did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AutoMergeOutcome {
    /// Armed on the forge: merged once the required checks pass, even with
    /// Archipel closed.
    Forge {
        /// What will not be done as asked (the source branch kept).
        warning: Option<String>,
    },
    /// The forge merged at once, nothing being left to wait for.
    Merged,
    /// The forge refused its own auto-merge: Archipel watches the CI and
    /// merges, only while it runs.
    Watched {
        /// The forge's refusal.
        refusal: String,
    },
}

/// An auto-merge Archipel watches itself.
#[derive(Clone, Debug)]
pub(super) struct LocalAutoMerge {
    forge: ForgeRepo,
    /// Source branch of the pull request.
    source: String,
    /// From a fork: read from the list of open pull requests, as its branch
    /// is not one of the repository's.
    from_fork: bool,
    method: MergeMethod,
    delete_source_branch: bool,
    watch: AutoMergeWatch,
    generation: u64,
}

/// Whether a refusal of the forge's auto-merge leaves the local watch to
/// try: the forge or the repository does not allow it, or the token lacks
/// a scope. Not a pull request that is gone, a network failure or a rate
/// limit.
fn watch_instead(error: &ForgeError) -> bool {
    match error {
        ForgeError::Unsupported(_) | ForgeError::Unauthorized { .. } => true,
        ForgeError::Http { status, .. } => (400..500).contains(status) && *status != 429,
        _ => false,
    }
}

/// Why a watched pull request cannot be merged yet; `None` when it can.
fn waiting_for(pr: &PullRequestStatus) -> Option<String> {
    if pr.draft {
        return Some("it is a draft".into());
    }
    if pr.merge == MergeState::Conflicts {
        return Some("it conflicts with its target branch".into());
    }
    match pr.ci {
        None => Some("no CI run on its head yet".into()),
        Some(CiState::Pending) => Some("its CI is running".into()),
        Some(CiState::Failure) => Some("its CI failed (a rerun or a new push can pass)".into()),
        Some(CiState::Success) => None,
    }
}

/// `!12` on GitLab, `#12` elsewhere.
fn pr_ref(forge: &ForgeRepo, number: u64) -> String {
    let sign = if forge.account.kind == ForgeKind::gitlab() {
        '!'
    } else {
        '#'
    };
    format!("{} {sign}{number}", forge.path)
}

/// Copies the watch of `repo`'s current pull request onto its row (the
/// "Pull request" column marks it). Returns whether the row changed.
pub(super) fn sync_auto_merge_row(s: &mut Session, repo: RepoId) -> bool {
    let Some(row) = s.rows.iter_mut().find(|r| r.id == repo) else {
        return false;
    };
    let watch = row.forge.pull_request().and_then(|pr| {
        s.auto_merges
            .get(&(repo, pr.number))
            .map(|l| l.watch.clone())
    });
    let changed = row.auto_merge != watch;
    row.auto_merge = watch;
    changed
}

impl App {
    /// Arms the auto-merge of `pr`, with the same choices as a merge now.
    /// The forge's own when it allows it, else a watch by Archipel (see the
    /// module documentation).
    pub async fn arm_auto_merge(
        &self,
        repo: RepoId,
        forge_repo: &ForgeRepo,
        pr: &PullRequestStatus,
        method: MergeMethod,
        delete_source_branch: bool,
    ) -> Result<AutoMergeOutcome, AppError> {
        let label = pr_ref(forge_repo, pr.number);
        if pr.draft {
            return Err(AppError::InvalidInput(format!(
                "{label} is a draft: mark it ready on the forge first"
            )));
        }
        if method == MergeMethod::Rebase && forge_repo.account.kind == ForgeKind::gitlab() {
            return Err(AppError::InvalidInput(
                "GitLab rebases according to the project's merge method: choose Merge or Squash"
                    .into(),
            ));
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
            format!("Arm auto-merge of {label}"),
        );
        let armed: Result<AutoMergeReport, ForgeError> = self
            .scheduler
            .submit(spec, move |_| {
                Ok(forge
                    .pull_request_writer
                    .enable_auto_merge(&target, number, &options))
            })
            .wait()
            .await?;
        match armed {
            Ok(report) if report.merged_now => {
                self.fetch(&[repo], delete_source_branch);
                self.load_forge(repo, true);
                self.emit(AppEvent::Notice(format!(
                    "Merged {label}: nothing left to wait for"
                )));
                Ok(AutoMergeOutcome::Merged)
            }
            Ok(report) => {
                // Shown once the forge's state is read back.
                self.load_forge(repo, true);
                self.emit(AppEvent::Notice(match &report.warning {
                    Some(w) => format!("Auto-merge armed on {label} ({w})"),
                    None => format!("Auto-merge armed on {label}: merged once its CI passes"),
                }));
                Ok(AutoMergeOutcome::Forge {
                    warning: report.warning,
                })
            }
            Err(e) if watch_instead(&e) => {
                let refusal = e.to_string();
                self.watch_auto_merge(
                    repo,
                    forge_repo,
                    pr,
                    method,
                    delete_source_branch,
                    refusal.clone(),
                );
                self.emit(AppEvent::Notice(format!(
                    "Auto-merge of {label} watched by Archipel, only while it runs (the forge \
                     refused its own: {refusal})"
                )));
                Ok(AutoMergeOutcome::Watched { refusal })
            }
            Err(e) => Err(e.into()),
        }
    }

    /// Cancels the auto-merge of `pr`: Archipel's watch and the forge's own.
    pub async fn cancel_auto_merge(
        &self,
        repo: RepoId,
        forge_repo: &ForgeRepo,
        pr: &PullRequestStatus,
    ) -> Result<(), AppError> {
        let label = pr_ref(forge_repo, pr.number);
        let watched = self.drop_watch(repo, pr.number, None);
        if pr.auto_merge {
            let forge = self.services.forge.clone();
            let (target, number) = (forge_repo.clone(), pr.number);
            let spec = OpSpec::new(
                Some(repo),
                OpClass::Network,
                format!("Cancel auto-merge of {label}"),
            );
            self.scheduler
                .submit(spec, move |_| {
                    Ok(forge
                        .pull_request_writer
                        .disable_auto_merge(&target, number)?)
                })
                .wait()
                .await?;
            self.load_forge(repo, true);
        } else if !watched {
            return Err(AppError::InvalidInput(format!(
                "{label} has no auto-merge armed"
            )));
        }
        self.emit(AppEvent::Notice(format!("Auto-merge of {label} cancelled")));
        Ok(())
    }

    /// Archipel's watch of pull request `number` of `repo`, if any.
    #[must_use]
    pub fn auto_merge_watch(&self, repo: RepoId, number: u64) -> Option<AutoMergeWatch> {
        lock(&self.session)
            .as_ref()
            .and_then(|s| s.auto_merges.get(&(repo, number)))
            .map(|l| l.watch.clone())
    }

    /// Cancels the auto-merge of the current branch's pull request of `ids`
    /// (every shown repo when empty), in the background; repos without one
    /// armed are skipped.
    pub(super) fn cancel_auto_merges(&self, ids: &[RepoId]) {
        let armed: Vec<_> = self
            .target_rows(ids)
            .into_iter()
            .filter_map(|row| {
                let pr = row.forge.pull_request()?.clone();
                let watched = row
                    .auto_merge
                    .as_ref()
                    .is_some_and(|w| w.number == pr.number);
                (pr.auto_merge || watched).then_some((row, pr))
            })
            .collect();
        if armed.is_empty() {
            self.emit(AppEvent::Notice("No auto-merge armed".into()));
            return;
        }
        for (row, pr) in armed {
            let Some(app) = self.me.upgrade() else { return };
            let git = self.services.git.clone();
            let forge = self.services.forge.clone();
            let links = self.ext.forges.clone();
            let (path, preferred) = (row.path.clone(), upstream_remote(&row));
            let spec = OpSpec::new(
                Some(row.id),
                OpClass::Network,
                format!("Locate {} on its forge", row.name),
            );
            let located = self.scheduler.submit(spec, move |_| {
                forge_repo(&git, &forge, &links, &path, preferred.as_deref())
            });
            self.scheduler.runtime().spawn(async move {
                let result = match located.wait().await {
                    Ok(target) => app.cancel_auto_merge(row.id, &target, &pr).await,
                    Err(e) => Err(e.into()),
                };
                if let Err(e) = result {
                    app.emit(AppEvent::Notice(format!(
                        "{}: auto-merge not cancelled: {e}",
                        row.name
                    )));
                }
            });
        }
    }

    /// Starts watching `pr` (replacing an earlier watch of it).
    fn watch_auto_merge(
        &self,
        repo: RepoId,
        forge_repo: &ForgeRepo,
        pr: &PullRequestStatus,
        method: MergeMethod,
        delete_source_branch: bool,
        refusal: String,
    ) {
        let generation = GENERATION.fetch_add(1, Ordering::Relaxed);
        let number = pr.number;
        {
            let mut guard = lock(&self.session);
            let Some(s) = guard.as_mut() else { return };
            s.auto_merges.insert(
                (repo, number),
                LocalAutoMerge {
                    forge: forge_repo.clone(),
                    source: pr.source.clone(),
                    from_fork: pr.from_fork,
                    method,
                    delete_source_branch,
                    watch: AutoMergeWatch {
                        number,
                        refusal,
                        waiting: None,
                    },
                    generation,
                },
            );
            sync_auto_merge_row(s, repo);
        }
        self.emit(AppEvent::RowsChanged(vec![repo]));
        let every = match self.settings.ci_poll_secs {
            0 => WATCH_FALLBACK_SECS,
            secs => secs.max(WATCH_MIN_SECS),
        };
        let me = self.me.clone();
        self.scheduler.runtime().spawn(async move {
            loop {
                let Some(app) = me.upgrade() else { return };
                if !app.check_auto_merge(repo, number, generation).await {
                    return;
                }
                drop(app);
                tokio::time::sleep(Duration::from_secs(every)).await;
            }
        });
    }

    /// The watch of `(repo, number)`, if it is still the one of
    /// `generation`.
    fn watched(&self, repo: RepoId, number: u64, generation: u64) -> Option<LocalAutoMerge> {
        lock(&self.session)
            .as_ref()
            .and_then(|s| s.auto_merges.get(&(repo, number)))
            .filter(|l| l.generation == generation)
            .cloned()
    }

    /// Ends the watch of `(repo, number)` (only the one of `generation`,
    /// when given). Returns whether there was one.
    fn drop_watch(&self, repo: RepoId, number: u64, generation: Option<u64>) -> bool {
        let removed = {
            let mut guard = lock(&self.session);
            let Some(s) = guard.as_mut() else {
                return false;
            };
            let current = s
                .auto_merges
                .get(&(repo, number))
                .is_some_and(|l| generation.is_none_or(|g| l.generation == g));
            if current {
                s.auto_merges.remove(&(repo, number));
                sync_auto_merge_row(s, repo);
            }
            current
        };
        if removed {
            self.emit(AppEvent::RowsChanged(vec![repo]));
        }
        removed
    }

    /// Notes why a watched pull request is not merged yet.
    fn set_waiting(&self, repo: RepoId, number: u64, generation: u64, waiting: Option<String>) {
        let changed = {
            let mut guard = lock(&self.session);
            let Some(s) = guard.as_mut() else { return };
            match s
                .auto_merges
                .get_mut(&(repo, number))
                .filter(|l| l.generation == generation)
            {
                Some(l) if l.watch.waiting != waiting => {
                    l.watch.waiting = waiting;
                    sync_auto_merge_row(s, repo);
                    true
                }
                _ => false,
            }
        };
        if changed {
            self.emit(AppEvent::RowsChanged(vec![repo]));
        }
    }

    /// One check of a watched pull request: merges it when its CI passed.
    /// Returns whether to keep watching.
    async fn check_auto_merge(&self, repo: RepoId, number: u64, generation: u64) -> bool {
        let Some(watch) = self.watched(repo, number, generation) else {
            return false;
        };
        let label = pr_ref(&watch.forge, number);
        let forge = self.services.forge.clone();
        let (target, source, from_fork) =
            (watch.forge.clone(), watch.source.clone(), watch.from_fork);
        let spec = OpSpec::new(
            Some(repo),
            OpClass::Network,
            format!("Auto-merge: check {label}"),
        );
        let read = self
            .scheduler
            .submit(spec, move |_| {
                let found = if from_fork {
                    forge
                        .pull_requests
                        .open_pull_requests(&target)?
                        .into_iter()
                        .find(|p| p.number == number)
                } else {
                    forge
                        .pull_requests
                        .pull_request_for_branch(&target, &source)?
                        .filter(|p| p.number == number)
                };
                Ok(found)
            })
            .wait()
            .await;
        let pr = match read {
            Ok(Some(pr)) => pr,
            Ok(None) => {
                if self.drop_watch(repo, number, Some(generation)) {
                    self.load_forge(repo, true);
                    self.emit(AppEvent::Notice(format!(
                        "{label} is no longer open: its auto-merge is dropped"
                    )));
                }
                return false;
            }
            Err(e) => {
                self.set_waiting(
                    repo,
                    number,
                    generation,
                    Some(format!("cannot read it: {e}")),
                );
                return true;
            }
        };
        if let Some(waiting) = waiting_for(&pr) {
            self.set_waiting(repo, number, generation, Some(waiting));
            return true;
        }
        // Cancelled while it was read.
        if self.watched(repo, number, generation).is_none() {
            return false;
        }
        let options = MergeOptions {
            method: watch.method,
            delete_source_branch: watch.delete_source_branch,
            // Only the commit whose CI passed.
            expected_head: (!pr.head_sha.is_empty()).then(|| pr.head_sha.clone()),
        };
        let forge = self.services.forge.clone();
        let target = watch.forge.clone();
        let spec = OpSpec::new(Some(repo), OpClass::Network, format!("Auto-merge {label}"));
        let merged = self
            .scheduler
            .submit(spec, move |_| {
                Ok(forge
                    .pull_request_writer
                    .merge_pull_request(&target, number, &options)?)
            })
            .wait()
            .await;
        match merged {
            Ok(report) => {
                self.drop_watch(repo, number, Some(generation));
                self.fetch(&[repo], watch.delete_source_branch);
                self.load_forge(repo, true);
                self.emit(AppEvent::Notice(match &report.warning {
                    Some(w) => format!("Merged {label}: its CI passed ({w})"),
                    None => format!("Merged {label}: its CI passed"),
                }));
                false
            }
            Err(e) => {
                self.set_waiting(
                    repo,
                    number,
                    generation,
                    Some(format!("the merge was refused: {e}")),
                );
                true
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use archipel_model::forge::ReviewState;

    use super::*;

    fn pr(ci: Option<CiState>) -> PullRequestStatus {
        PullRequestStatus {
            number: 1,
            title: String::new(),
            url: String::new(),
            draft: false,
            source: "feat".into(),
            target: "main".into(),
            head_sha: "abc".into(),
            author: String::new(),
            ci,
            review: ReviewState::Pending,
            merge: MergeState::Unknown,
            from_fork: false,
            auto_merge: false,
            assignees: Vec::new(),
        }
    }

    #[test]
    fn merges_only_a_ready_pull_request_whose_ci_passed() {
        assert_eq!(waiting_for(&pr(Some(CiState::Success))), None);
        assert!(waiting_for(&pr(Some(CiState::Pending))).is_some());
        assert!(waiting_for(&pr(Some(CiState::Failure))).is_some());
        assert!(waiting_for(&pr(None)).is_some());
        let conflicted = PullRequestStatus {
            merge: MergeState::Conflicts,
            ..pr(Some(CiState::Success))
        };
        assert!(waiting_for(&conflicted).is_some());
        let draft = PullRequestStatus {
            draft: true,
            ..pr(Some(CiState::Success))
        };
        assert!(waiting_for(&draft).is_some());
    }

    #[test]
    fn only_refusals_of_the_feature_fall_back_to_a_watch() {
        let host = || "h".to_owned();
        assert!(watch_instead(&ForgeError::Unsupported("off".into())));
        assert!(watch_instead(&ForgeError::Unauthorized {
            host: host(),
            status: 403
        }));
        assert!(watch_instead(&ForgeError::Http {
            host: host(),
            status: 422,
            message: String::new()
        }));
        assert!(!watch_instead(&ForgeError::Http {
            host: host(),
            status: 502,
            message: String::new()
        }));
        assert!(!watch_instead(&ForgeError::Network {
            host: host(),
            message: String::new()
        }));
        assert!(!watch_instead(&ForgeError::NotFound {
            host: host(),
            what: String::new()
        }));
    }
}
