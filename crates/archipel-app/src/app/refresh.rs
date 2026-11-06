//! Keeping the dashboard rows fresh: summary refreshes on the scheduler
//! (coalesced per repo), file-watcher invalidations and focus regains.
//!
//! Hidden repos are never refreshed nor watched (spec §1). Repos whose
//! worktree cannot be watched are refreshed when the window regains focus.

use std::sync::Arc;

use archipel_model::ports::{Coverage, Invalidation, InvalidationSink, Scope, WatchHandle};
use archipel_model::{GitError, ProjectId, RepoId, RepoPath, RepoSummary};
use archipel_ops::{OpClass, OpSpec};

use super::{App, AppEvent, lock};
use crate::dashboard::RepoStatus;

impl App {
    /// Recomputes every summary in parallel, then caches them.
    pub fn refresh_all(&self) {
        let ids: Vec<RepoId> = lock(&self.session)
            .as_ref()
            .map(|s| s.rows.iter().filter(|r| !r.hidden).map(|r| r.id).collect())
            .unwrap_or_default();
        for id in ids {
            self.refresh_repo(id);
        }
    }

    /// Recomputes one summary (coalesced: at most one pending refresh per repo).
    pub fn refresh_repo(&self, repo: RepoId) {
        let path = {
            let mut guard = lock(&self.session);
            let Some(s) = guard.as_mut() else { return };
            // Hidden repos are never refreshed (spec §1: saves CPU and RAM).
            if !s.rows.iter().any(|r| r.id == repo && !r.hidden) || !s.pending.insert(repo) {
                return;
            }
            // Stale while the refresh is queued (the UI can show a spinner).
            let Some(row) = s.rows.iter_mut().find(|r| r.id == repo) else {
                return;
            };
            row.stale = true;
            row.path.clone()
        };
        let status = self.services.git.status.clone();
        let me = self.me.clone();
        let spec = OpSpec::new(
            Some(repo),
            OpClass::Read,
            format!("Refresh {}", path.name()),
        );
        let _handle = self.scheduler.submit(spec, move |_| {
            let result = status.summary(&path);
            if let Some(app) = me.upgrade() {
                app.apply_summary(repo, &path, result);
            }
            Ok(())
        });
    }

    /// Stores a refreshed summary in its row; caches every summary once no
    /// refresh is pending.
    fn apply_summary(&self, repo: RepoId, path: &RepoPath, result: Result<RepoSummary, GitError>) {
        let finished_all = {
            let mut guard = lock(&self.session);
            let Some(s) = guard.as_mut() else { return };
            s.pending.remove(&repo);
            if let Some(row) = s.rows.iter_mut().find(|r| r.id == repo) {
                row.stale = false;
                row.status = match result {
                    _ if !path.as_path().exists() => RepoStatus::Missing,
                    Ok(summary) => RepoStatus::Ready(Box::new(summary)),
                    Err(GitError::NotARepository { .. }) => RepoStatus::Missing,
                    Err(e) => RepoStatus::Error(e.to_string()),
                };
            }
            s.pending.is_empty().then(|| (s.id.clone(), s.rows.clone()))
        };
        self.emit(AppEvent::RowsChanged(vec![repo]));
        self.load_tree(repo);
        self.load_forge(repo, false);
        if let Some((id, rows)) = finished_all {
            self.persist_rows(&id, &rows);
        }
    }

    /// (Re)starts watching the shown repos of the open project; tells the
    /// UI which ones are only partly watched.
    pub(super) fn start_watching(&self) {
        // Watch handles unwatch on drop (syscalls): they are dropped after
        // the session lock is released.
        let (project, targets, old): (ProjectId, Vec<(RepoId, RepoPath)>, Vec<WatchHandle>) = {
            let mut guard = lock(&self.session);
            let Some(s) = guard.as_mut() else { return };
            let targets = s
                .rows
                .iter()
                .filter(|r| !r.hidden && r.path.as_path().exists())
                .map(|r| (r.id, r.path.clone()))
                .collect();
            (s.id.clone(), targets, std::mem::take(&mut s.watches))
        };
        drop(old);
        let me = self.me.clone();
        let sink: Arc<dyn InvalidationSink> = Arc::new(move |inv: Invalidation| {
            let Some(app) = me.upgrade() else { return };
            // Only `FETCH_HEAD` written: a fetch that moved no ref.
            if inv.scope == Scope::FETCH {
                app.note_fetched(inv.repo);
            } else {
                app.refresh_repo(inv.repo);
            }
        });
        let mut handles = Vec::new();
        let mut degraded = Vec::new();
        for (id, path) in targets {
            match self.services.watcher.watch(id, &path, sink.clone()) {
                Ok(h) => {
                    if h.coverage == Coverage::GitDirOnly {
                        degraded.push(id);
                    }
                    handles.push(h);
                }
                Err(e) => {
                    tracing::warn!(error = %e, repo = %path.as_path().display(), "cannot watch repository");
                    degraded.push(id);
                }
            }
        }
        // The project may have been switched while watching: the handles
        // then belong to no session and are dropped (outside the lock).
        let stale = match lock(&self.session).as_mut() {
            Some(s) if s.id == project => std::mem::replace(&mut s.watches, handles),
            _ => return,
        };
        drop(stale);
        if !degraded.is_empty() {
            self.emit(AppEvent::DegradedWatching(degraded));
        }
    }

    /// Called when the window regains focus: when the worktree of some
    /// shown repo is not fully watched (degraded mode), refreshes every
    /// shown repo; then refreshes stale forge statuses.
    pub fn on_focus(&self) {
        let degraded: Vec<RepoId> = lock(&self.session)
            .as_ref()
            .map(|s| {
                let watched = s
                    .watches
                    .iter()
                    .filter(|w| w.coverage == Coverage::Full)
                    .count();
                let shown = s.rows.iter().filter(|r| !r.hidden);
                if watched == shown.clone().count() {
                    Vec::new()
                } else {
                    shown.map(|r| r.id).collect()
                }
            })
            .unwrap_or_default();
        for id in degraded {
            self.refresh_repo(id);
        }
        self.refresh_forge_stale();
    }
}
