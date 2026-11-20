//! Repository statistics (issue #93): contribution, weight, commit sizes,
//! churn against age, tag cadence, lead time to merge and (issue #94) the
//! language breakdown, per repository and across the project.

use std::ops::ControlFlow;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, PoisonError};
use std::thread;

use archipel_model::{CancelToken, GitError, RepoId, RepoPath, Timestamp};
use archipel_stats::{
    HistoryBuilder, Insights, InsightsConfig, RepoHistory, detect_default_branch, latest_tag,
};

use super::App;

/// How many repositories are read at once.
const PARALLEL: usize = 4;
/// Largest blobs listed per repository.
const LARGEST_BLOBS: usize = 8;

/// The statistics of one repository.
#[derive(Clone, Debug)]
pub struct RepoInsights {
    /// The repository.
    pub id: RepoId,
    /// Display name.
    pub name: String,
    /// Its statistics.
    pub insights: Insights,
}

/// The statistics of the repositories asked for, and of all of them
/// together.
#[derive(Clone, Debug, Default)]
pub struct InsightsReport {
    /// Every repository together.
    pub project: Insights,
    /// Per repository, in dashboard order.
    pub repos: Vec<RepoInsights>,
    /// Repositories that could not be read ("name: error").
    pub errors: Vec<String>,
}

impl App {
    /// The statistics of the given repos (all shown when empty; a worktree
    /// stands for its repository), read in parallel; `progress` hears of
    /// each repository done (done, total). Blocking: run it off the UI
    /// thread. Stops early, with what it has, when `cancel` fires.
    #[must_use]
    pub fn insights(
        &self,
        ids: &[RepoId],
        cancel: &CancelToken,
        progress: &(dyn Fn(usize, usize) + Sync),
    ) -> InsightsReport {
        let mut mains: Vec<RepoId> = Vec::new();
        for id in self.resolve_targets(ids) {
            let main = self.main_repo_of(id);
            if !mains.contains(&main) {
                mains.push(main);
            }
        }
        let targets: Vec<(RepoId, String, RepoPath)> = self
            .rows()
            .into_iter()
            .filter(|r| r.summary().is_some() && r.parent().is_none() && mains.contains(&r.id))
            .map(|r| (r.id, r.name, r.path))
            .collect();
        let config = InsightsConfig::at(Timestamp::now());
        let next = AtomicUsize::new(0);
        let done = AtomicUsize::new(0);
        let results: Mutex<Vec<Option<Result<RepoHistory, String>>>> =
            Mutex::new((0..targets.len()).map(|_| None).collect());
        thread::scope(|scope| {
            for _ in 0..PARALLEL.min(targets.len()) {
                scope.spawn(|| {
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        let Some((_, name, path)) = targets.get(i) else {
                            break;
                        };
                        if cancel.is_cancelled() {
                            break;
                        }
                        let result = self
                            .repo_history(name, path, config, cancel)
                            .map_err(|e| format!("{name}: {e}"));
                        results.lock().unwrap_or_else(PoisonError::into_inner)[i] = Some(result);
                        progress(done.fetch_add(1, Ordering::Relaxed) + 1, targets.len());
                    }
                });
            }
        });
        let results = results.into_inner().unwrap_or_else(PoisonError::into_inner);
        let mut report = InsightsReport::default();
        let mut histories = Vec::new();
        for ((id, name, _), result) in targets.into_iter().zip(results) {
            match result {
                Some(Ok(history)) => histories.push((id, name, history)),
                Some(Err(e)) => report.errors.push(e),
                None => {}
            }
        }
        let all: Vec<&RepoHistory> = histories.iter().map(|(_, _, h)| h).collect();
        report.project = Insights::build(&all, config);
        report.repos = histories
            .iter()
            .map(|(id, name, h)| RepoInsights {
                id: *id,
                name: name.clone(),
                insights: Insights::build(&[h], config),
            })
            .collect();
        report
    }

    /// Walks the default branch of one repository (`HEAD` when there is
    /// none), measures its weight and counts the languages of its tree.
    fn repo_history(
        &self,
        name: &str,
        path: &RepoPath,
        config: InsightsConfig,
        cancel: &CancelToken,
    ) -> Result<RepoHistory, GitError> {
        let git = &self.services.git;
        let branches = git.refs.branches(path)?;
        let default = detect_default_branch(git.refs.as_ref(), path, &branches, None)?;
        let rev = default.map_or_else(|| "HEAD".to_owned(), |d| d.name);
        let mut builder = HistoryBuilder::new(name, &rev, config);
        self.services.churn.walk_records(
            path,
            &rev,
            &mut |record| {
                builder.push(record);
                ControlFlow::Continue(())
            },
            cancel,
        )?;
        let tags = git.refs.tags(path)?;
        // Work after the latest tag, even when it is on another branch.
        let unreleased = match latest_tag(&tags) {
            Some(tag) => self
                .services
                .churn
                .count_commits(path, &rev, tag.target)
                .unwrap_or_else(|e| {
                    tracing::debug!("commits after {} in {name}: {e}", tag.name);
                    None
                }),
            None => None,
        };
        let storage = match self.services.churn.storage(path, LARGEST_BLOBS, cancel) {
            Ok(s) => Some(s),
            Err(GitError::Cancelled) => return Err(GitError::Cancelled),
            Err(e) => {
                tracing::debug!("repo weight of {name}: {e}");
                None
            }
        };
        let files = match self.services.churn.tree_files(path, &rev, cancel) {
            Ok(files) => Some(files),
            Err(GitError::Cancelled) => return Err(GitError::Cancelled),
            Err(e) => {
                tracing::debug!("languages of {name}: {e}");
                None
            }
        };
        let mut history = builder.finish(&tags, unreleased, storage);
        if let Some(files) = files {
            history.set_languages(&files);
        }
        Ok(history)
    }
}
