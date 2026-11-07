//! The three built-in providers (spec §7), registered like any fork provider.

use std::collections::BTreeMap;

use archipel_model::stats::{ActivityBucket, BranchAge};

use crate::default_branch::detect_default_branch;
use crate::error::StatsError;
use crate::provider::{RepoTarget, StatProvider, StatSources};
use crate::view::{ActivityView, BranchRow, BranchesView, HotspotRow, HotspotsView, StatView};

/// Id of [`ActivityProvider`].
pub const ACTIVITY_ID: &str = "activity";
/// Id of [`BranchesProvider`].
pub const BRANCHES_ID: &str = "branches";
/// Id of [`HotspotsProvider`].
pub const HOTSPOTS_ID: &str = "hotspots";

/// Commits per week and author.
#[derive(Clone, Copy, Debug, Default)]
pub struct ActivityProvider;

impl StatProvider for ActivityProvider {
    fn id(&self) -> &'static str {
        ACTIVITY_ID
    }

    fn label(&self) -> &'static str {
        "Activity per author"
    }

    fn compute(&self, sources: &StatSources, repo: &RepoTarget) -> Result<StatView, StatsError> {
        Ok(StatView::Activity(sum_activity(
            sources.store.activity(repo.id)?,
        )))
    }

    fn aggregate(&self, views: &[StatView]) -> StatView {
        let all = views.iter().flat_map(|v| match v {
            StatView::Activity(a) => a.buckets.clone(),
            _ => Vec::new(),
        });
        StatView::Activity(sum_activity(all))
    }
}

/// Sums buckets by (week, author) and sorts them.
fn sum_activity(buckets: impl IntoIterator<Item = ActivityBucket>) -> ActivityView {
    let mut sums: BTreeMap<(i64, String), u32> = BTreeMap::new();
    for b in buckets {
        let slot = sums.entry((b.week, b.author)).or_default();
        *slot = slot.saturating_add(b.commits);
    }
    ActivityView {
        buckets: sums
            .into_iter()
            .map(|((week, author), commits)| ActivityBucket {
                week,
                author,
                commits,
            })
            .collect(),
    }
}

/// Age of local branches and branches merged into the default branch but not
/// deleted. The default branch is listed nowhere; the current branch is listed
/// but never reported as merged.
#[derive(Clone, Copy, Debug, Default)]
pub struct BranchesProvider;

impl StatProvider for BranchesProvider {
    fn id(&self) -> &'static str {
        BRANCHES_ID
    }

    fn label(&self) -> &'static str {
        "Branch age"
    }

    fn compute(&self, sources: &StatSources, repo: &RepoTarget) -> Result<StatView, StatsError> {
        let branches = sources.refs.branches(&repo.path)?;
        let default = detect_default_branch(
            sources.refs.as_ref(),
            &repo.path,
            &branches,
            repo.default_branch.as_deref(),
        )?;
        let mut rows = Vec::new();
        for b in branches.iter().filter(|b| !b.is_remote) {
            if default.as_ref().is_some_and(|d| d.name == b.name) {
                continue;
            }
            let merged = match &default {
                Some(d) if !b.is_head => {
                    sources.history.is_ancestor(&repo.path, b.target, d.tip)?
                }
                _ => false,
            };
            rows.push(BranchRow {
                repo_id: repo.id,
                repo: repo.name.clone(),
                age: BranchAge {
                    name: b.name.clone(),
                    tip: b.target,
                    last_commit: b.tip_time,
                    merged,
                },
                current: b.is_head,
            });
        }
        sort_branches(&mut rows);
        Ok(StatView::Branches(BranchesView { branches: rows }))
    }

    fn aggregate(&self, views: &[StatView]) -> StatView {
        let mut rows: Vec<BranchRow> = views
            .iter()
            .flat_map(|v| match v {
                StatView::Branches(b) => b.branches.clone(),
                _ => Vec::new(),
            })
            .collect();
        sort_branches(&mut rows);
        StatView::Branches(BranchesView { branches: rows })
    }
}

fn sort_branches(rows: &mut [BranchRow]) {
    rows.sort_by(|a, b| {
        a.age
            .last_commit
            .cmp(&b.age.last_commit)
            .then_with(|| a.repo.cmp(&b.repo))
            .then_with(|| a.age.name.cmp(&b.age.name))
    });
}

/// Paths with the highest churn (lines added + removed).
#[derive(Clone, Copy, Debug)]
pub struct HotspotsProvider {
    limit: usize,
}

impl HotspotsProvider {
    /// Shows at most `limit` paths per repository and per project.
    #[must_use]
    pub fn new(limit: usize) -> Self {
        Self { limit }
    }
}

impl StatProvider for HotspotsProvider {
    fn id(&self) -> &'static str {
        HOTSPOTS_ID
    }

    fn label(&self) -> &'static str {
        "File hotspots"
    }

    fn compute(&self, sources: &StatSources, repo: &RepoTarget) -> Result<StatView, StatsError> {
        let rows = sources
            .store
            .hotspots(repo.id, self.limit)?
            .into_iter()
            .map(|hotspot| HotspotRow {
                repo: repo.name.clone(),
                hotspot,
            })
            .collect();
        Ok(StatView::Hotspots(self.top(rows)))
    }

    fn aggregate(&self, views: &[StatView]) -> StatView {
        let rows = views
            .iter()
            .flat_map(|v| match v {
                StatView::Hotspots(h) => h.hotspots.clone(),
                _ => Vec::new(),
            })
            .collect();
        StatView::Hotspots(self.top(rows))
    }
}

impl HotspotsProvider {
    fn top(self, mut rows: Vec<HotspotRow>) -> HotspotsView {
        rows.sort_by(|a, b| {
            b.hotspot
                .churn()
                .cmp(&a.hotspot.churn())
                .then_with(|| a.repo.cmp(&b.repo))
                .then_with(|| a.hotspot.path.cmp(&b.hotspot.path))
        });
        rows.truncate(self.limit);
        HotspotsView { hotspots: rows }
    }
}
