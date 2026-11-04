//! The [`StatProvider`] extension point.

use std::sync::Arc;

use archipel_model::ports::{HistoryReader, RefReader, StatsStore};
use archipel_model::{RepoId, RepoPath};

use crate::error::StatsError;
use crate::view::StatView;

/// The ports a provider may read from.
#[derive(Clone)]
pub struct StatSources {
    /// Incrementally maintained statistics (filled by `StatsEngine`).
    pub store: Arc<dyn StatsStore>,
    /// Branches and revisions.
    pub refs: Arc<dyn RefReader>,
    /// Ancestry queries.
    pub history: Arc<dyn HistoryReader>,
}

/// A repository whose statistics are computed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepoTarget {
    /// Store key of the repository.
    pub id: RepoId,
    /// Working tree path.
    pub path: RepoPath,
    /// Display name, copied into per-repo rows.
    pub name: String,
    /// Configured default branch; auto-detected when `None`.
    pub default_branch: Option<String>,
}

impl RepoTarget {
    /// A target with its id and name derived from the path and an
    /// auto-detected default branch.
    #[must_use]
    pub fn from_path(path: RepoPath) -> Self {
        Self {
            id: path.id(),
            name: path.name(),
            path,
            default_branch: None,
        }
    }
}

/// One statistic: computes a view for a repository and merges per-repo views
/// into a project view.
///
/// Built-in providers implement this trait and are registered in a
/// `StatsRegistry` exactly like a fork's provider would be. Implementations
/// block and are called off the UI thread.
pub trait StatProvider: Send + Sync {
    /// Stable identifier (`"activity"`), unique within a registry.
    fn id(&self) -> &str;
    /// Human readable title.
    fn label(&self) -> &str;
    /// Computes the view of one repository.
    fn compute(&self, sources: &StatSources, repo: &RepoTarget) -> Result<StatView, StatsError>;
    /// Merges the views of several repositories (all produced by `compute`).
    fn aggregate(&self, views: &[StatView]) -> StatView;
}
