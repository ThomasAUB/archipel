//! # archipel-compose
//!
//! The composition root shared by the binaries (`archipel-cli`, `archipel-ui`):
//! the **only** place that knows which concrete adapter sits behind which
//! port trait (ARCHITECTURE §4), forge providers included ([`forge_ports`]).
//! Swapping an implementation is a one-line change in [`compose`], or a
//! setting (`read_backend`).
//!
//! It also sets up logging (PLAN §3.6) and resolves the user directories.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use archipel_app::forge::ForgePorts;
use archipel_app::settings::load_settings;
use archipel_app::{App, Extensions, Services};
use archipel_cache::{CacheRegistry, SqliteStore};
use archipel_forge::{
    CachingTransport, FileAccountStore, ForgeClient, GitHubReleases, KeychainCredentials,
    UreqTransport, git_credential,
};
use archipel_git::{CliConfig, GitCli, GixReader, check_git_version};
use archipel_model::ForgeError;
use archipel_model::ports::{HistoryReader, RefReader, ReleaseFeed, StashReader, StatusReader};
use archipel_model::settings::{ReadBackend, Settings};
use archipel_ops::GitPorts;
use archipel_project::{FileProjectSource, FsRepoDiscovery};
use archipel_watch::NotifyChangeSource;

/// Errors while starting the application.
#[derive(Debug, thiserror::Error)]
pub enum ComposeError {
    /// No usable data directory.
    #[error("cannot determine the user data directory; set ARCHIPEL_HOME")]
    NoDataDir,
    /// `git` missing or too old.
    #[error(transparent)]
    Git(#[from] archipel_model::GitError),
    /// The SQLite store could not be opened.
    #[error(transparent)]
    Store(#[from] archipel_model::StoreError),
    /// The watcher could not start.
    #[error(transparent)]
    Watch(#[from] archipel_model::WatchError),
    /// Async runtime creation failed.
    #[error("cannot start the async runtime: {0}")]
    Runtime(String),
}

/// User directories of the application.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppDirs {
    /// Settings, keymap and themes.
    pub config: PathBuf,
    /// SQLite store and logs.
    pub data: PathBuf,
    /// Disposable caches (graph topology files).
    pub cache: PathBuf,
}

impl AppDirs {
    /// `ARCHIPEL_HOME` (all three under it) or the platform directories.
    pub fn resolve() -> Result<Self, ComposeError> {
        if let Some(home) = std::env::var_os("ARCHIPEL_HOME") {
            return Ok(Self::under(Path::new(&home)));
        }
        let dirs = directories::ProjectDirs::from("dev", "Archipel", "Archipel")
            .ok_or(ComposeError::NoDataDir)?;
        Ok(Self {
            config: dirs.config_dir().to_path_buf(),
            data: dirs.data_dir().to_path_buf(),
            cache: dirs.cache_dir().to_path_buf(),
        })
    }

    /// Every directory under one root (tests, portable installs).
    #[must_use]
    pub fn under(root: &Path) -> Self {
        Self {
            config: root.join("config"),
            data: root.join("data"),
            cache: root.join("cache"),
        }
    }

    /// The settings file.
    #[must_use]
    pub fn settings_file(&self) -> PathBuf {
        self.config.join("settings.toml")
    }

    /// The keymap file.
    #[must_use]
    pub fn keymap_file(&self) -> PathBuf {
        self.config.join("keymap.toml")
    }

    /// The remembered theme choice (written by the app).
    #[must_use]
    pub fn appearance_file(&self) -> PathBuf {
        self.config.join("appearance.toml")
    }

    /// The user themes folder.
    #[must_use]
    pub fn themes_dir(&self) -> PathBuf {
        self.config.join("themes")
    }

    /// The signed-in forge accounts (hosts and users; tokens are in the
    /// OS keychain).
    #[must_use]
    pub fn forge_accounts_file(&self) -> PathBuf {
        self.config.join("forges.toml")
    }
}

/// Keychain service name of forge tokens (one entry per host).
pub const KEYCHAIN_SERVICE: &str = "dev.archipel.forge";

/// Forge ports: GitHub and GitLab over HTTPS, tokens in the OS keychain,
/// the account list in the config directory. A fork registers its own
/// provider on the client here (`client.register(...)`).
#[must_use]
pub fn forge_ports(settings: &Settings, dirs: &AppDirs) -> ForgePorts {
    let credentials = Arc::new(KeychainCredentials::new(KEYCHAIN_SERVICE));
    // Conditional GETs: the periodic status reads of unchanged repos do not
    // count against the forge's rate limit.
    let http = Arc::new(CachingTransport::new(Arc::new(UreqTransport::new(
        std::time::Duration::from_secs(settings.forge_timeout_secs),
    ))));
    let client = Arc::new(ForgeClient::with_builtins(http, credentials.clone()));
    ForgePorts {
        pull_requests: client.clone(),
        ci: client.clone(),
        pull_request_writer: client.clone(),
        directory: client.clone(),
        sign_in: client,
        credentials,
        accounts: Arc::new(FileAccountStore::new(dirs.forge_accounts_file())),
    }
}

/// The `credential.helper` value running `exe git-credential`: git passes
/// it to its shell, hence the quoting.
#[must_use]
pub fn credential_helper_command(exe: &Path) -> String {
    let path = exe.to_string_lossy();
    // Git for Windows runs helpers through its MSYS shell.
    #[cfg(windows)]
    let path = path.replace('\\', "/");
    format!("!'{}' git-credential", path.replace('\'', r"'\''"))
}

/// Answers a git credential helper call (`op` and stdin as git sent them)
/// with the token of a signed-in forge account
/// ([`archipel_forge::git_credential`]).
pub fn git_credential(dirs: &AppDirs, op: &str, input: &str) -> Result<String, ForgeError> {
    git_credential::answer(
        op,
        input,
        &FileAccountStore::new(dirs.forge_accounts_file()),
        &KeychainCredentials::new(KEYCHAIN_SERVICE),
    )
}

/// Options of [`compose_with`].
#[derive(Clone, Debug, Default)]
pub struct ComposeOptions {
    /// Credential helper handed to every `git` command (see
    /// [`credential_helper_command`]); only for a binary answering it.
    pub credential_helper: Option<String>,
}

/// Everything a binary needs.
pub struct Composition {
    /// The application.
    pub app: Arc<App>,
    /// The async runtime the app runs on (keep it alive).
    pub runtime: tokio::runtime::Runtime,
    /// Directories in use.
    pub dirs: AppDirs,
    /// Warnings to show (broken settings or keymap file…).
    pub warnings: Vec<String>,
    /// Duration of each composition step (startup diagnostics).
    pub timings: Vec<(&'static str, std::time::Duration)>,
}

/// Records how long each composition step takes.
struct Steps {
    last: std::time::Instant,
    timings: Vec<(&'static str, std::time::Duration)>,
}

impl Steps {
    fn new() -> Self {
        Self {
            last: std::time::Instant::now(),
            timings: Vec::new(),
        }
    }

    fn done(&mut self, step: &'static str) {
        let now = std::time::Instant::now();
        self.timings.push((step, now - self.last));
        self.last = now;
    }
}

/// Initializes logging: a daily rolling file in the data directory (level
/// from `ARCHIPEL_LOG`, default `info`). Returns the guard flushing the file.
#[must_use]
pub fn init_logging(dirs: &AppDirs) -> Option<tracing_appender::non_blocking::WorkerGuard> {
    use tracing_subscriber::EnvFilter;
    let logs = dirs.data.join("logs");
    std::fs::create_dir_all(&logs).ok()?;
    let (writer, guard) =
        tracing_appender::non_blocking(tracing_appender::rolling::daily(logs, "archipel.log"));
    let filter = EnvFilter::try_from_env("ARCHIPEL_LOG").unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_writer(writer)
        .with_ansi(false)
        .with_env_filter(filter)
        .try_init()
        .ok()?;
    Some(guard)
}

/// Git ports for the chosen read backend; writes always use the CLI.
#[must_use]
pub fn git_ports(settings: &Settings, cli: &Arc<GitCli>) -> GitPorts {
    let mut ports = GitPorts::from_backend(cli.clone());
    if settings.read_backend == ReadBackend::Gix {
        let gix = Arc::new(GixReader::default());
        ports.status = gix.clone() as Arc<dyn StatusReader>;
        ports.refs = gix.clone() as Arc<dyn RefReader>;
        ports.history = gix.clone() as Arc<dyn HistoryReader>;
        ports.stashes = gix as Arc<dyn StashReader>;
    }
    ports
}

/// Where the update check looks for releases: the GitHub releases of the
/// workspace's `repository`. Builds updated by a package manager (Homebrew,
/// winget, Flatpak…) set `ARCHIPEL_NO_UPDATE_CHECK` at compile time: no
/// check at all.
#[must_use]
pub fn release_feed(settings: &Settings) -> Option<Arc<dyn ReleaseFeed>> {
    if option_env!("ARCHIPEL_NO_UPDATE_CHECK").is_some() {
        return None;
    }
    let http = Arc::new(UreqTransport::new(std::time::Duration::from_secs(
        settings.forge_timeout_secs,
    )));
    let feed = GitHubReleases::from_repository_url(http, env!("CARGO_PKG_REPOSITORY"))?;
    Some(Arc::new(feed))
}

/// Builds the application from the user directories.
pub fn compose(dirs: AppDirs) -> Result<Composition, ComposeError> {
    compose_with(dirs, ComposeOptions::default())
}

/// [`compose`] with options.
pub fn compose_with(dirs: AppDirs, options: ComposeOptions) -> Result<Composition, ComposeError> {
    let mut steps = Steps::new();
    let mut warnings = Vec::new();
    let (settings, warning) = load_settings(&dirs.settings_file());
    warnings.extend(warning);
    steps.done("settings");
    let mut cli_config = CliConfig::from_settings(&settings);
    cli_config.credential_helper = options.credential_helper;
    let version = check_git_version(&cli_config.program, settings.min_git_version)?;
    tracing::info!(%version, "git found");
    steps.done("git version");

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .thread_name("archipel-worker")
        .enable_all()
        .build()
        .map_err(|e| ComposeError::Runtime(e.to_string()))?;
    steps.done("runtime");

    let store = Arc::new(SqliteStore::open(dirs.data.join("archipel.db"))?);
    steps.done("sqlite");
    let cli = Arc::new(GitCli::new(cli_config));
    let services = Services {
        git: git_ports(&settings, &cli),
        projects: Arc::new(FileProjectSource::new()),
        discovery: Arc::new(FsRepoDiscovery::new()),
        registry: store.clone(),
        summaries: store.clone(),
        ui_state: store.clone(),
        journal: store.clone(),
        watcher: Arc::new(NotifyChangeSource::from_settings(&settings)?),
        diffs: cli.clone(),
        churn: cli,
        stats: store,
        caches: Arc::new(CacheRegistry::new()),
        cache_dir: dirs.cache.clone(),
        forge: forge_ports(&settings, &dirs),
        updates: release_feed(&settings),
    };
    steps.done("adapters");

    let mut ext = Extensions::default();
    if let Ok(text) = std::fs::read_to_string(dirs.keymap_file()) {
        let (keymap, keymap_warnings) = ext.keymap.with_overrides(&text);
        ext.keymap = keymap;
        warnings.extend(keymap_warnings.into_iter().map(|w| w.0));
    }
    let app = App::new(services, settings, ext, runtime.handle().clone());
    steps.done("app");
    Ok(Composition {
        app,
        runtime,
        dirs,
        warnings,
        timings: steps.timings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn composes_in_a_fresh_home() {
        let home = tempfile::tempdir().unwrap();
        let dirs = AppDirs::under(home.path());
        std::fs::create_dir_all(&dirs.config).unwrap();
        std::fs::write(dirs.keymap_file(), "\"bad-key-x\" = \"y\"").unwrap();
        let c = compose(dirs).unwrap();
        assert!(c.app.projects().unwrap().is_empty());
        assert_eq!(c.warnings.len(), 1);
        assert!(home.path().join("data/archipel.db").exists());
    }

    #[test]
    fn credential_helper_command_is_shell_quoted() {
        assert_eq!(
            credential_helper_command(Path::new("/opt/it's here/archipel")),
            r"!'/opt/it'\''s here/archipel' git-credential"
        );
    }
}
