//! Application settings with documented defaults.
//!
//! Every tunable value (budgets, timeouts, concurrency) lives here, so there
//! are no magic numbers scattered in the code. The settings file overrides any
//! field; missing fields keep their default.

use serde::{Deserialize, Serialize};

use crate::requests::PullMode;

/// Which implementation serves Git reads.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReadBackend {
    /// In-process `gix` (default, see ADR 0002).
    #[default]
    Gix,
    /// `git` CLI subprocesses.
    Cli,
}

/// Application settings.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Backend serving status/ref/history reads.
    pub read_backend: ReadBackend,
    /// Maximum concurrent network operations (fetch/pull/push/clone). Default 6.
    pub network_concurrency: usize,
    /// Maximum concurrent read computations; 0 = number of CPUs. Default 0.
    pub read_concurrency: usize,
    /// Timeout for network operations, in seconds. Default 300.
    pub network_timeout_secs: u64,
    /// Timeout for local Git commands, in seconds. Default 120.
    pub local_timeout_secs: u64,
    /// Debounce delay for file system events, in milliseconds. Default 150.
    pub watch_debounce_ms: u64,
    /// Byte budget of the commit metadata cache. Default 16 MiB.
    pub commit_cache_bytes: usize,
    /// Byte budget of the diff cache. Default 32 MiB.
    pub diff_cache_bytes: usize,
    /// Rows between two lane-layout checkpoints in the graph. Default 1000.
    pub graph_checkpoint_interval: usize,
    /// Rows loaded above and below the visible window. Default 100.
    pub graph_overscan: usize,
    /// Diffs larger than this many bytes are truncated. Default 2 MiB.
    pub max_diff_bytes: usize,
    /// Journal entries kept per repository. Default 200.
    pub journal_limit: usize,
    /// Pull strategy for "pull all". Default fast-forward only.
    pub pull_mode: PullMode,
    /// Recent projects listed in the switcher. Default 10.
    pub recent_projects: usize,
    /// Minimum supported `git` version (major, minor). Default 2.40.
    pub min_git_version: (u32, u32),
    /// External merge tool name passed to `git mergetool --tool` (Git config when `None`).
    pub merge_tool: Option<String>,
    /// Timeout of one forge API request (GitHub, GitLab, …), in seconds. Default 20.
    pub forge_timeout_secs: u64,
    /// Forge status (pull request, CI, review) older than this is re-read
    /// when the window gets focus, in seconds. Default 300.
    pub forge_refresh_secs: u64,
    /// Base interval of the background fetch, in seconds; 0 disables it.
    /// Repos are fetched one at a time, spread over the interval; a repo
    /// whose fetches bring nothing is fetched less and less often. Default 600.
    pub auto_fetch_secs: u64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            read_backend: ReadBackend::Gix,
            network_concurrency: 6,
            read_concurrency: 0,
            network_timeout_secs: 300,
            local_timeout_secs: 120,
            watch_debounce_ms: 150,
            commit_cache_bytes: 16 * 1024 * 1024,
            diff_cache_bytes: 32 * 1024 * 1024,
            graph_checkpoint_interval: 1000,
            graph_overscan: 100,
            max_diff_bytes: 2 * 1024 * 1024,
            journal_limit: 200,
            pull_mode: PullMode::FastForwardOnly,
            recent_projects: 10,
            min_git_version: (2, 40),
            merge_tool: None,
            forge_timeout_secs: 20,
            forge_refresh_secs: 300,
            auto_fetch_secs: 600,
        }
    }
}
