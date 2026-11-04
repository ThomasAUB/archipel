//! The registry of statistics providers.

use std::sync::Arc;

use archipel_model::RepoId;

use crate::builtin::{ActivityProvider, BranchesProvider, HotspotsProvider};
use crate::config::StatsConfig;
use crate::error::StatsError;
use crate::provider::{RepoTarget, StatProvider, StatSources};
use crate::view::StatView;

/// Result of one provider on one repository.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderResult {
    /// Provider id.
    pub id: String,
    /// The view, or why it could not be computed.
    pub result: Result<StatView, StatsError>,
}

/// Every provider's result for one repository.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepoStats {
    /// Repository id.
    pub repo: RepoId,
    /// Repository display name.
    pub name: String,
    /// One result per provider, in registration order.
    pub results: Vec<ProviderResult>,
}

impl RepoStats {
    /// The view of a provider, if it succeeded.
    #[must_use]
    pub fn view(&self, id: &str) -> Option<&StatView> {
        self.results
            .iter()
            .find(|r| r.id == id)
            .and_then(|r| r.result.as_ref().ok())
    }
}

/// A provider's view aggregated across a project.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderView {
    /// Provider id.
    pub id: String,
    /// Provider label.
    pub label: String,
    /// Aggregated view (repos whose computation failed are left out).
    pub view: StatView,
}

/// Statistics of a project: per repository and aggregated.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectStats {
    /// Per-repo results, in the order of the given repositories.
    pub repos: Vec<RepoStats>,
    /// Aggregated views, in registration order.
    pub aggregated: Vec<ProviderView>,
}

impl ProjectStats {
    /// The aggregated view of a provider.
    #[must_use]
    pub fn aggregated(&self, id: &str) -> Option<&StatView> {
        self.aggregated.iter().find(|v| v.id == id).map(|v| &v.view)
    }
}

/// Ordered set of [`StatProvider`]s, keyed by id.
#[derive(Clone, Default)]
pub struct StatsRegistry {
    providers: Vec<Arc<dyn StatProvider>>,
}

impl StatsRegistry {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A registry holding the three built-in providers.
    #[must_use]
    pub fn with_builtins(config: &StatsConfig) -> Self {
        let mut registry = Self::new();
        registry.register(Arc::new(ActivityProvider));
        registry.register(Arc::new(BranchesProvider));
        registry.register(Arc::new(HotspotsProvider::new(config.hotspot_limit)));
        registry
    }

    /// Registers a provider. A provider with the same id is replaced (keeping
    /// its position) and returned, so a fork can override a built-in.
    pub fn register(&mut self, provider: Arc<dyn StatProvider>) -> Option<Arc<dyn StatProvider>> {
        if let Some(slot) = self.providers.iter_mut().find(|p| p.id() == provider.id()) {
            return Some(std::mem::replace(slot, provider));
        }
        self.providers.push(provider);
        None
    }

    /// Removes a provider by id.
    pub fn unregister(&mut self, id: &str) -> Option<Arc<dyn StatProvider>> {
        let index = self.providers.iter().position(|p| p.id() == id)?;
        Some(self.providers.remove(index))
    }

    /// A provider by id.
    #[must_use]
    pub fn get(&self, id: &str) -> Option<&Arc<dyn StatProvider>> {
        self.providers.iter().find(|p| p.id() == id)
    }

    /// Ids of the registered providers, in order.
    #[must_use]
    pub fn ids(&self) -> Vec<&str> {
        self.providers.iter().map(|p| p.id()).collect()
    }

    /// Every provider, in order.
    #[must_use]
    pub fn providers(&self) -> &[Arc<dyn StatProvider>] {
        &self.providers
    }

    /// Runs every provider on one repository. One failing provider does not
    /// prevent the others.
    #[must_use]
    pub fn compute_repo(&self, sources: &StatSources, repo: &RepoTarget) -> RepoStats {
        RepoStats {
            repo: repo.id,
            name: repo.name.clone(),
            results: self
                .providers
                .iter()
                .map(|p| ProviderResult {
                    id: p.id().to_owned(),
                    result: p.compute(sources, repo),
                })
                .collect(),
        }
    }

    /// Runs every provider on every repository, then aggregates per provider.
    #[must_use]
    pub fn compute_project(&self, sources: &StatSources, repos: &[RepoTarget]) -> ProjectStats {
        let per_repo: Vec<RepoStats> = repos
            .iter()
            .map(|r| self.compute_repo(sources, r))
            .collect();
        let aggregated = self
            .providers
            .iter()
            .enumerate()
            .map(|(index, p)| {
                let views: Vec<StatView> = per_repo
                    .iter()
                    .filter_map(|r| r.results.get(index)?.result.as_ref().ok().cloned())
                    .collect();
                ProviderView {
                    id: p.id().to_owned(),
                    label: p.label().to_owned(),
                    view: p.aggregate(&views),
                }
            })
            .collect();
        ProjectStats {
            repos: per_repo,
            aggregated,
        }
    }
}
