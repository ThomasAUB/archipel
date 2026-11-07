//! Background folder scan of a launch from a directory (spec §1): fills the
//! freshly created project in as repositories are found, cancellable.

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use archipel_model::project::ScanOptions;
use archipel_model::{CancelToken, ProjectId, RepoPath};
use archipel_ops::{OpClass, OpError, OpSpec};

use super::{App, AppEvent, lock};

/// Found repos are added in batches: at most this many…
const SCAN_BATCH: usize = 16;
/// …or after this long, so the list fills in smoothly without re-watching
/// every repo for each one found.
const SCAN_FLUSH: Duration = Duration::from_millis(150);

impl App {
    /// Scans `root` in the background, adding the repos found to `project`
    /// while it stays open. Cancels the scan already running, if any.
    pub(super) fn start_scan(&self, project: &ProjectId, root: PathBuf) {
        let cancel = CancelToken::new();
        {
            let mut guard = lock(&self.session);
            let Some(s) = guard.as_mut().filter(|s| &s.id == project) else {
                return;
            };
            if let Some(old) = s.scan.replace(cancel.clone()) {
                old.cancel();
            }
        }
        self.emit(AppEvent::ScanProgress {
            found: 0,
            running: true,
        });
        let discovery = self.services.discovery.clone();
        let me = self.me.clone();
        let project = project.clone();
        let spec = OpSpec::new(None, OpClass::Read, format!("Scan {}", root.display()));
        let _handle = self.scheduler.submit(spec, move |_| {
            let pending = Mutex::new((Vec::new(), Instant::now(), 0_usize));
            let flush = |paths: Vec<RepoPath>, found: usize| {
                if let Some(app) = me.upgrade() {
                    app.add_found(&project, &paths, found);
                }
            };
            let result = discovery.scan_each(&root, &ScanOptions::default(), &cancel, &|repo| {
                let batch = {
                    let mut p = lock(&pending);
                    p.0.push(repo.path);
                    p.2 += 1;
                    (p.0.len() >= SCAN_BATCH || p.1.elapsed() >= SCAN_FLUSH).then(|| {
                        p.1 = Instant::now();
                        (std::mem::take(&mut p.0), p.2)
                    })
                };
                if let Some((paths, found)) = batch {
                    flush(paths, found);
                }
            });
            let (rest, found) = {
                let mut p = lock(&pending);
                (std::mem::take(&mut p.0), p.2)
            };
            flush(rest, found);
            if let Some(app) = me.upgrade() {
                app.finish_scan(&project, &cancel, found);
            }
            result.map_err(|e| OpError::Worker(e.to_string()))
        });
    }

    /// Adds repos found by the scan that the project does not list yet.
    fn add_found(&self, project: &ProjectId, paths: &[RepoPath], found: usize) {
        let mut new: Vec<RepoPath> = {
            let guard = lock(&self.session);
            let Some(s) = guard.as_ref().filter(|s| &s.id == project) else {
                return;
            };
            paths
                .iter()
                .filter(|p| s.def.repos.iter().all(|r| r.path.id() != p.id()))
                .cloned()
                .collect()
        };
        // The scan reports in no particular order; keep each batch tidy.
        new.sort_by(|a, b| a.as_path().cmp(b.as_path()));
        if !new.is_empty()
            && let Err(e) = self.add_repos(&new)
        {
            tracing::warn!(error = %e, "cannot add scanned repositories");
        }
        self.emit(AppEvent::ScanProgress {
            found,
            running: true,
        });
    }

    fn finish_scan(&self, project: &ProjectId, cancel: &CancelToken, found: usize) {
        if let Some(s) = lock(&self.session).as_mut().filter(|s| &s.id == project) {
            // Only clear our own scan, not one started after it.
            if s.scan.as_ref().is_some_and(|c| c.same_as(cancel)) {
                s.scan = None;
            }
        }
        self.emit(AppEvent::ScanProgress {
            found,
            running: false,
        });
    }

    /// Whether a folder scan is running for the open project.
    #[must_use]
    pub fn scanning(&self) -> bool {
        lock(&self.session)
            .as_ref()
            .is_some_and(|s| s.scan.is_some())
    }

    /// Cancels the running folder scan, if any.
    pub(super) fn cancel_scan(&self) {
        let scan = lock(&self.session).as_ref().and_then(|s| s.scan.clone());
        if let Some(scan) = scan {
            scan.cancel();
        }
    }
}
