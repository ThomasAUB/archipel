//! # archipel-project
//!
//! Projects: named groups of repositories (spec §1). This crate owns the
//! project definition storage, the folder scan and submodule detection.
//!
//! * [`FileProjectSource`] implements [`ProjectSource`]: shared
//!   `.gitproject.toml` files (schema v1, see [`format`](crate::format)
//!   for the layout) written atomically, and private projects delegated to an
//!   injected [`PrivateProjectStore`].
//! * [`FsRepoDiscovery`] implements [`RepoDiscovery`]: a depth-limited walk
//!   detecting `.git` directories and `.git` files, sequential or parallel
//!   ([`ScanStrategy`]).
//! * [`classify_repos`] / [`repo_presence`] tell present repositories from
//!   missing ones; a missing repository never prevents loading a project.
//! * [`submodule_paths`] lists the submodules declared in `.gitmodules`.
//! * [`outermost_repo_root`] / [`normalize_dir`] resolve the folder a
//!   launch from a directory scans (spec §1).
//!
//! Dependencies: `archipel-model` (types and port traits), `serde`/`toml`
//! for the file format, `rayon` for the parallel scan, `tracing`. No adapter
//! crate is used.
//!
//! [`ProjectSource`]: archipel_model::ports::project::ProjectSource
//! [`RepoDiscovery`]: archipel_model::ports::project::RepoDiscovery
//! [`PrivateProjectStore`]: archipel_model::ports::store::PrivateProjectStore

pub mod format;
mod paths;
mod presence;
mod scan;
mod source;
mod submodules;

pub use format::CURRENT_VERSION;
pub use presence::{RepoPresence, classify_repos, outermost_repo_root, repo_presence};
pub use scan::{FsRepoDiscovery, ScanStrategy};
pub use source::FileProjectSource;
pub use submodules::submodule_paths;

/// File name of a shared project definition.
pub const PROJECT_FILE_NAME: &str = ".gitproject.toml";

/// Absolute, lexically normalized form of a directory given on the command
/// line (relative paths are resolved against the current directory).
pub fn normalize_dir(dir: &std::path::Path) -> std::io::Result<std::path::PathBuf> {
    std::path::absolute(dir).map(|p| paths::normalize(&p))
}
