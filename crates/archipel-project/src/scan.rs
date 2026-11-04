//! [`FsRepoDiscovery`]: depth-limited folder scan detecting Git repositories.

use std::fs;
use std::path::{Path, PathBuf};

use archipel_model::cancel::CancelToken;
use archipel_model::error::ProjectError;
use archipel_model::ids::RepoPath;
use archipel_model::ports::project::RepoDiscovery;
use archipel_model::project::{DiscoveredKind, DiscoveredRepo, ScanOptions};
use rayon::prelude::*;

use crate::presence::DOT_GIT;

/// Prefix of hidden directory names.
const HIDDEN_PREFIX: char = '.';

/// How the folder tree is walked.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ScanStrategy {
    /// One thread, depth-first.
    Sequential,
    /// Sibling folders are scanned in parallel on the rayon pool (default:
    /// fastest on the benchmark of `tests/scan_bench.rs`).
    #[default]
    Parallel,
}

/// Scans the file system for repositories.
///
/// * The root is depth 0; folders down to `max_depth` are inspected.
/// * A folder is a repository when it holds a `.git` directory
///   ([`DiscoveredKind::GitDir`]) or a `.git` file ([`DiscoveredKind::GitFile`],
///   linked worktrees and submodules).
/// * `skip_dirs` and (optionally) hidden folders are never entered; symbolic
///   links are not followed; unreadable folders are skipped.
/// * On cancellation the scan stops early and returns what was found so far.
#[derive(Clone, Copy, Debug, Default)]
pub struct FsRepoDiscovery {
    strategy: ScanStrategy,
}

impl FsRepoDiscovery {
    /// A discovery using the default strategy.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A discovery using `strategy`.
    #[must_use]
    pub fn with_strategy(strategy: ScanStrategy) -> Self {
        Self { strategy }
    }
}

impl RepoDiscovery for FsRepoDiscovery {
    fn scan(
        &self,
        root: &Path,
        options: &ScanOptions,
        cancel: &CancelToken,
    ) -> Result<Vec<DiscoveredRepo>, ProjectError> {
        check_dir(root)?;
        let walker = Walker {
            options,
            cancel,
            on_found: None,
        };
        let mut found = match self.strategy {
            ScanStrategy::Sequential => {
                let mut out = Vec::new();
                walker.visit_sequential(root.to_path_buf(), 0, &mut out);
                out
            }
            ScanStrategy::Parallel => walker.visit_parallel(root.to_path_buf(), 0),
        };
        found.sort_by(|a, b| a.path.as_path().cmp(b.path.as_path()));
        Ok(found)
    }

    fn scan_each(
        &self,
        root: &Path,
        options: &ScanOptions,
        cancel: &CancelToken,
        on_found: &(dyn Fn(DiscoveredRepo) + Sync),
    ) -> Result<(), ProjectError> {
        check_dir(root)?;
        let walker = Walker {
            options,
            cancel,
            on_found: Some(on_found),
        };
        match self.strategy {
            ScanStrategy::Sequential => {
                walker.visit_sequential(root.to_path_buf(), 0, &mut Vec::new());
            }
            ScanStrategy::Parallel => drop(walker.visit_parallel(root.to_path_buf(), 0)),
        }
        Ok(())
    }
}

fn check_dir(root: &Path) -> Result<(), ProjectError> {
    let metadata = fs::metadata(root).map_err(|e| ProjectError::Io {
        path: root.to_path_buf(),
        message: e.to_string(),
    })?;
    if metadata.is_dir() {
        Ok(())
    } else {
        Err(ProjectError::Io {
            path: root.to_path_buf(),
            message: "not a directory".to_owned(),
        })
    }
}

struct Walker<'a> {
    options: &'a ScanOptions,
    cancel: &'a CancelToken,
    /// Streaming mode: found repos go here instead of the output list.
    on_found: Option<&'a (dyn Fn(DiscoveredRepo) + Sync)>,
}

/// What a single folder contains, as far as the scan cares.
struct Inspection {
    kind: Option<DiscoveredKind>,
    subdirs: Vec<PathBuf>,
}

impl Walker<'_> {
    fn visit_sequential(&self, dir: PathBuf, depth: usize, out: &mut Vec<DiscoveredRepo>) {
        let Some(found) = self.inspect(&dir, depth) else {
            return;
        };
        let subdirs = self.record(dir, found, out);
        for sub in subdirs {
            self.visit_sequential(sub, depth + 1, out);
        }
    }

    fn visit_parallel(&self, dir: PathBuf, depth: usize) -> Vec<DiscoveredRepo> {
        let Some(found) = self.inspect(&dir, depth) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let subdirs = self.record(dir, found, &mut out);
        let nested: Vec<Vec<DiscoveredRepo>> = subdirs
            .into_par_iter()
            .map(|sub| self.visit_parallel(sub, depth + 1))
            .collect();
        out.extend(nested.into_iter().flatten());
        out
    }

    /// Records `dir` when it is a repository and returns the folders to visit next.
    fn record(
        &self,
        dir: PathBuf,
        found: Inspection,
        out: &mut Vec<DiscoveredRepo>,
    ) -> Vec<PathBuf> {
        match found.kind {
            Some(kind) => {
                let repo = DiscoveredRepo {
                    path: RepoPath::new(dir),
                    kind,
                };
                match self.on_found {
                    Some(report) => report(repo),
                    None => out.push(repo),
                }
                if self.options.descend_into_repos {
                    found.subdirs
                } else {
                    Vec::new()
                }
            }
            None => found.subdirs,
        }
    }

    /// Reads `dir`; `None` when cancelled or unreadable.
    fn inspect(&self, dir: &Path, depth: usize) -> Option<Inspection> {
        if self.cancel.is_cancelled() {
            return None;
        }
        let entries = match fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(error) => {
                tracing::debug!(dir = %dir.display(), %error, "skipping unreadable folder");
                return None;
            }
        };
        let descend = depth < self.options.max_depth;
        let mut kind = None;
        let mut subdirs = Vec::new();
        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            let name = entry.file_name();
            if name == DOT_GIT {
                kind = dot_git_kind(&entry.path(), file_type);
            } else if descend && file_type.is_dir() && self.may_enter(&name.to_string_lossy()) {
                subdirs.push(entry.path());
            }
        }
        Some(Inspection { kind, subdirs })
    }

    fn may_enter(&self, name: &str) -> bool {
        let hidden = self.options.skip_hidden && name.starts_with(HIDDEN_PREFIX);
        let skipped = self.options.skip_dirs.iter().any(|skip| skip == name);
        !hidden && !skipped
    }
}

/// Kind of a `.git` entry; symbolic links are resolved.
fn dot_git_kind(path: &Path, file_type: fs::FileType) -> Option<DiscoveredKind> {
    let (is_dir, is_file) = if file_type.is_symlink() {
        let meta = fs::metadata(path).ok()?;
        (meta.is_dir(), meta.is_file())
    } else {
        (file_type.is_dir(), file_type.is_file())
    };
    if is_dir {
        Some(DiscoveredKind::GitDir)
    } else if is_file {
        Some(DiscoveredKind::GitFile)
    } else {
        None
    }
}
