//! The `git` CLI adapter: implements every reader and writer trait.

mod files;
mod read;
mod runner;
mod submodule;
mod write;

use std::path::PathBuf;
use std::time::Duration;

use archipel_model::settings::Settings;

use runner::Runner;

/// Configuration of the CLI adapter.
#[derive(Clone, Debug)]
pub struct CliConfig {
    /// The `git` executable (looked up in `PATH` when relative).
    pub program: PathBuf,
    /// Timeout of local commands.
    pub local_timeout: Duration,
    /// Timeout of network commands.
    pub network_timeout: Duration,
    /// Askpass helper forwarding credential prompts to the UI.
    pub askpass: Option<PathBuf>,
    /// Credential helper consulted after the user's own ones (a
    /// `credential.helper` value: the app serving its forge tokens).
    pub credential_helper: Option<String>,
    /// Diffs above this size per file are truncated.
    pub max_diff_bytes: usize,
}

impl CliConfig {
    /// Builds the configuration from the application settings.
    #[must_use]
    pub fn from_settings(settings: &Settings) -> Self {
        Self {
            program: PathBuf::from("git"),
            local_timeout: Duration::from_secs(settings.local_timeout_secs),
            network_timeout: Duration::from_secs(settings.network_timeout_secs),
            askpass: None,
            credential_helper: None,
            max_diff_bytes: settings.max_diff_bytes,
        }
    }
}

impl Default for CliConfig {
    fn default() -> Self {
        Self::from_settings(&Settings::default())
    }
}

/// Git adapter running the `git` executable.
///
/// Credential prompts never block: `GIT_TERMINAL_PROMPT=0` is always set, and
/// the optional askpass helper receives prompts instead. The user's config
/// (hooks, signing, credential helpers, LFS) is never overridden: the
/// optional app credential helper is only appended after the user's ones.
#[derive(Clone, Debug)]
pub struct GitCli {
    runner: Runner,
    max_diff_bytes: usize,
}

impl GitCli {
    /// Creates the adapter.
    #[must_use]
    pub fn new(config: CliConfig) -> Self {
        Self {
            runner: Runner {
                program: config.program,
                local_timeout: config.local_timeout,
                network_timeout: config.network_timeout,
                askpass: config.askpass,
                credential_helper: config.credential_helper,
            },
            max_diff_bytes: config.max_diff_bytes,
        }
    }
}

impl Default for GitCli {
    fn default() -> Self {
        Self::new(CliConfig::default())
    }
}
