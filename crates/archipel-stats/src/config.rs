//! Tunable defaults of the statistics views.

/// Number of hotspots shown by default (per repo and per project).
pub const DEFAULT_HOTSPOT_LIMIT: usize = 20;

/// Local branch names tried, in order, when `origin/HEAD` does not tell the
/// default branch.
pub const FALLBACK_DEFAULT_BRANCHES: [&str; 2] = ["main", "master"];

/// Configuration of the built-in providers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatsConfig {
    /// Maximum number of hotspots per view.
    pub hotspot_limit: usize,
}

impl Default for StatsConfig {
    fn default() -> Self {
        Self {
            hotspot_limit: DEFAULT_HOTSPOT_LIMIT,
        }
    }
}
