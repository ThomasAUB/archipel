//! Project definition storage and repository discovery.

use std::path::Path;

use crate::cancel::CancelToken;
use crate::error::ProjectError;
use crate::project::{DiscoveredRepo, ProjectDef, ProjectLocation, ScanOptions};

/// Loads and saves project definitions.
///
/// Missing repositories are **not** an error: they load as entries whose path
/// does not exist, and the UI shows them as "missing".
pub trait ProjectSource: Send + Sync {
    /// Loads the definition at `location`.
    fn load(&self, location: &ProjectLocation) -> Result<ProjectDef, ProjectError>;
    /// Saves the definition at `location`, creating it if needed.
    fn save(&self, location: &ProjectLocation, project: &ProjectDef) -> Result<(), ProjectError>;
}

/// Finds repositories below a folder.
pub trait RepoDiscovery: Send + Sync {
    /// Scans `root` and returns the repositories found, sorted by path.
    /// Unreadable folders are skipped, not reported as errors.
    fn scan(
        &self,
        root: &Path,
        options: &ScanOptions,
        cancel: &CancelToken,
    ) -> Result<Vec<DiscoveredRepo>, ProjectError>;

    /// Like [`scan`](Self::scan), but reports each repository as soon as it
    /// is found, in no particular order, so a list can fill in while the scan
    /// runs. `on_found` may be called from several threads. The default
    /// implementation scans, then reports everything.
    fn scan_each(
        &self,
        root: &Path,
        options: &ScanOptions,
        cancel: &CancelToken,
        on_found: &(dyn Fn(DiscoveredRepo) + Sync),
    ) -> Result<(), ProjectError> {
        for repo in self.scan(root, options, cancel)? {
            on_found(repo);
        }
        Ok(())
    }
}
