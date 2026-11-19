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

/// What resolves conflicts (setting `merge_tool`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum MergeToolChoice {
    /// The built-in merge view (default).
    #[default]
    BuiltIn,
    /// The external tool Git's configuration names (`merge.tool`).
    GitConfig,
    /// This external tool (`git mergetool --tool=<name>`).
    External(String),
}

impl MergeToolChoice {
    /// Setting value of [`Self::BuiltIn`].
    pub const BUILT_IN: &'static str = "built-in";
    /// Setting value of [`Self::GitConfig`].
    pub const GIT_CONFIG: &'static str = "git";

    /// The choice a `merge_tool` setting value names (blank: built-in).
    #[must_use]
    pub fn from_setting(value: Option<&str>) -> Self {
        match value.map(str::trim) {
            None | Some("" | Self::BUILT_IN) => Self::BuiltIn,
            Some(Self::GIT_CONFIG) => Self::GitConfig,
            Some(name) => Self::External(name.to_owned()),
        }
    }

    /// The `merge_tool` setting value naming it (`None`: the default).
    #[must_use]
    pub fn setting(&self) -> Option<&str> {
        match self {
            Self::BuiltIn => None,
            Self::GitConfig => Some(Self::GIT_CONFIG),
            Self::External(name) => Some(name),
        }
    }

    /// The tool to pass to `git mergetool --tool` (`None`: Git's
    /// `merge.tool`).
    #[must_use]
    pub fn external_tool(&self) -> Option<&str> {
        match self {
            Self::External(name) => Some(name),
            Self::BuiltIn | Self::GitConfig => None,
        }
    }
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
    /// What opens when an operation leaves conflicts, read with
    /// [`MergeToolChoice::from_setting`]: the built-in merge view when
    /// absent (or `"built-in"`), Git's `merge.tool` for `"git"`, else the
    /// tool of that name (`git mergetool --tool`).
    pub merge_tool: Option<String>,
    /// Timeout of one forge API request (GitHub, GitLab, …), in seconds. Default 20.
    pub forge_timeout_secs: u64,
    /// Forge status (pull request, CI, review) older than this is re-read
    /// when the window gets focus, in seconds. Default 300.
    pub forge_refresh_secs: u64,
    /// While a repo's CI is unsettled (running, or just after a pull request
    /// was opened and its runs may not be registered yet), its forge status
    /// is re-read this often, in seconds; 0 disables it. Default 20.
    pub ci_poll_secs: u64,
    /// Base interval of the background fetch, in seconds; 0 disables it.
    /// Repos are fetched one at a time, spread over the interval; a repo
    /// whose fetches bring nothing is fetched less and less often. Default 600.
    pub auto_fetch_secs: u64,
    /// Interval between two checks for a new Archipel release, in hours;
    /// 0 disables the check. Default 24.
    pub update_check_hours: u64,
    /// Whether each repo's current branch is checked, in the background after
    /// every refresh, for conflicts with its upstream and with the target of
    /// its pull request (the default branch without one). Default on.
    pub conflict_forecast: bool,
    /// Where a new worktree goes (spec §10): `{project}`, `{repo}`,
    /// `{branch}` and `{set}` are substituted; relative to the repo's main
    /// worktree. Default `../{repo}.worktrees/{branch}`.
    pub worktree_location: String,
    /// Where the members of a new Worktree Set go, relative to the project
    /// file's folder. Default `../{project}.worktrees/{set}/{repo}`.
    pub worktree_set_location: String,
    /// List a repo's worktrees under it when a new one appears. Default on.
    pub worktree_auto_expand: bool,
    /// Command opening a terminal in a folder (the folder is appended; the
    /// platform's terminal when `None`).
    pub terminal_command: Option<String>,
    /// Command opening a folder in an external editor (the folder is
    /// appended; `code` when `None`).
    pub editor_command: Option<String>,
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
            ci_poll_secs: 20,
            auto_fetch_secs: 600,
            update_check_hours: 24,
            conflict_forecast: true,
            worktree_location: crate::worktree::DEFAULT_WORKTREE_LOCATION.to_owned(),
            worktree_set_location: crate::worktree::DEFAULT_SET_LOCATION.to_owned(),
            worktree_auto_expand: true,
            terminal_command: None,
            editor_command: None,
        }
    }
}

#[cfg(test)]
mod merge_tool_tests {
    use super::MergeToolChoice as C;

    #[test]
    fn merge_tool_setting_round_trip() {
        for c in [C::BuiltIn, C::GitConfig, C::External("meld".into())] {
            assert_eq!(C::from_setting(c.setting()), c);
        }
        assert_eq!(C::from_setting(Some(" built-in ")), C::BuiltIn);
        assert_eq!(C::from_setting(Some("")), C::BuiltIn);
        assert_eq!(C::External("x".into()).external_tool(), Some("x"));
        assert_eq!(C::GitConfig.external_tool(), None);
    }
}
