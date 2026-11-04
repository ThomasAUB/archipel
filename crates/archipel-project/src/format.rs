//! The `archipel.toml` file format (schema v1).
//!
//! ```toml
//! version = 1
//! name = "My product"
//!
//! [[repo]]
//! path = "app"
//!
//! [[repo]]
//! path = "../shared/libfoo"
//! alias = "libfoo"
//! category = "libs"     # sidebar group (spec §1)
//!
//! [settings]
//! require_same_branch = true
//! ticket_prefix = "JIRA-123"
//! ticket_from_branch = "([A-Z]+-[0-9]+)"
//!
//! accent = "#3b82f6"   # window color (spec §2)
//! background_tint = 15  # % of the accent tinting the window background
//! icon = "🚀"           # project icon: an emoji, or an image file
//!                       # relative to this file ("assets/logo.svg")
//!
//! [[settings.template]]
//! name = "fix"
//! body = "{ticket} fix: "
//! ```
//!
//! Repository paths are stored relative to the file's folder with `/`
//! separators; they are resolved to absolute, lexically normalized paths on
//! load. A path with no relative form (another Windows drive) is stored
//! absolute.

use std::collections::HashSet;
use std::path::Path;

use archipel_model::error::ProjectError;
use archipel_model::ids::RepoPath;
use archipel_model::project::{MessageTemplateDef, ProjectDef, ProjectSettings, RepoEntry};
use serde::{Deserialize, Serialize};

use crate::paths;

/// Highest schema version this build reads and the version it writes.
pub const CURRENT_VERSION: u32 = 1;

/// Key holding the schema version.
const VERSION_KEY: &str = "version";

/// On-disk shape of a v1 file.
#[derive(Debug, Serialize, Deserialize)]
struct FileV1 {
    version: u32,
    name: String,
    #[serde(default, rename = "repo", skip_serializing_if = "Vec::is_empty")]
    repos: Vec<RepoV1>,
    #[serde(default, skip_serializing_if = "SettingsV1::is_default")]
    settings: SettingsV1,
}

#[derive(Debug, Serialize, Deserialize)]
struct RepoV1 {
    path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    alias: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    category: Option<String>,
}

#[derive(Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct SettingsV1 {
    #[serde(default, skip_serializing_if = "is_false")]
    require_same_branch: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ticket_prefix: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ticket_from_branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    accent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    background_tint: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    icon: Option<String>,
    #[serde(default, rename = "template", skip_serializing_if = "Vec::is_empty")]
    templates: Vec<TemplateV1>,
}

impl SettingsV1 {
    fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
struct TemplateV1 {
    name: String,
    body: String,
}

#[allow(clippy::trivially_copy_pass_by_ref)] // signature imposed by serde
fn is_false(value: &bool) -> bool {
    !*value
}

/// Parses the text of a project file located at `file` (absolute).
pub(crate) fn parse(file: &Path, text: &str) -> Result<ProjectDef, ProjectError> {
    let invalid = |message: String| ProjectError::Invalid {
        path: file.to_path_buf(),
        message,
    };
    let table: toml::Table = toml::from_str(text).map_err(|e| invalid(e.to_string()))?;
    check_version(&table, &invalid)?;
    let raw: FileV1 = table
        .try_into()
        .map_err(|e: toml::de::Error| invalid(e.message().to_owned()))?;
    if raw.name.trim().is_empty() {
        return Err(invalid("`name` must not be empty".to_owned()));
    }
    let base = file.parent().unwrap_or(file);
    Ok(ProjectDef {
        name: raw.name,
        repos: resolve_repos(file, base, raw.repos),
        settings: raw.settings.into(),
    })
}

/// Renders `project` as the text of a project file located at `file` (absolute).
pub(crate) fn render(file: &Path, project: &ProjectDef) -> Result<String, ProjectError> {
    let base = file.parent().unwrap_or(file);
    let repos = project
        .repos
        .iter()
        .map(|entry| {
            let target = paths::normalize(entry.path.as_path());
            let path = paths::relative(base, &target)
                .or_else(|| target.to_str().map(str::to_owned))
                .ok_or_else(|| ProjectError::Invalid {
                    path: file.to_path_buf(),
                    message: format!("repository path is not valid UTF-8: {}", target.display()),
                })?;
            Ok(RepoV1 {
                path,
                alias: entry.alias.clone(),
                category: entry.category.clone(),
            })
        })
        .collect::<Result<Vec<_>, ProjectError>>()?;
    let raw = FileV1 {
        version: CURRENT_VERSION,
        name: project.name.clone(),
        repos,
        settings: SettingsV1::from(&project.settings),
    };
    toml::to_string_pretty(&raw).map_err(|e| ProjectError::Invalid {
        path: file.to_path_buf(),
        message: e.to_string(),
    })
}

fn check_version(
    table: &toml::Table,
    invalid: &impl Fn(String) -> ProjectError,
) -> Result<(), ProjectError> {
    let value = table
        .get(VERSION_KEY)
        .ok_or_else(|| invalid("missing `version`".to_owned()))?;
    let version = value
        .as_integer()
        .and_then(|v| u32::try_from(v).ok())
        .filter(|v| *v > 0)
        .ok_or_else(|| invalid(format!("`version` must be a positive integer, got {value}")))?;
    if version > CURRENT_VERSION {
        return Err(ProjectError::UnsupportedVersion {
            found: version,
            supported: CURRENT_VERSION,
        });
    }
    Ok(())
}

fn resolve_repos(file: &Path, base: &Path, raw: Vec<RepoV1>) -> Vec<RepoEntry> {
    let mut seen = HashSet::new();
    let mut repos = Vec::with_capacity(raw.len());
    for repo in raw {
        let path = paths::resolve(base, &repo.path);
        if !seen.insert(paths::dedup_key(&path)) {
            tracing::warn!(
                file = %file.display(),
                repo = %repo.path,
                "duplicate repository in project file, ignored"
            );
            continue;
        }
        repos.push(RepoEntry {
            path: RepoPath::new(path),
            alias: repo.alias,
            category: repo
                .category
                .map(|c| c.trim().to_owned())
                .filter(|c| !c.is_empty()),
        });
    }
    repos
}

impl From<SettingsV1> for ProjectSettings {
    fn from(raw: SettingsV1) -> Self {
        Self {
            require_same_branch: raw.require_same_branch,
            ticket_prefix: raw.ticket_prefix,
            ticket_from_branch: raw.ticket_from_branch,
            message_templates: raw
                .templates
                .into_iter()
                .map(|t| MessageTemplateDef {
                    name: t.name,
                    body: t.body,
                })
                .collect(),
            accent: raw.accent,
            background_tint: raw.background_tint,
            icon: raw.icon,
        }
    }
}

impl From<&ProjectSettings> for SettingsV1 {
    fn from(settings: &ProjectSettings) -> Self {
        Self {
            require_same_branch: settings.require_same_branch,
            ticket_prefix: settings.ticket_prefix.clone(),
            ticket_from_branch: settings.ticket_from_branch.clone(),
            accent: settings.accent.clone(),
            background_tint: settings.background_tint,
            icon: settings.icon.clone(),
            templates: settings
                .message_templates
                .iter()
                .map(|t| TemplateV1 {
                    name: t.name.clone(),
                    body: t.body.clone(),
                })
                .collect(),
        }
    }
}
