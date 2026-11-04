//! Projects: named groups of repositories (spec §1).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::ids::{ProjectId, RepoPath};

/// One repository entry of a project definition.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoEntry {
    /// Absolute path of the repository.
    pub path: RepoPath,
    /// Optional display name overriding the folder name.
    pub alias: Option<String>,
    /// Optional category grouping repos in the sidebar (spec §1).
    #[serde(default)]
    pub category: Option<String>,
}

impl RepoEntry {
    /// Name shown in the UI.
    #[must_use]
    pub fn display_name(&self) -> String {
        self.alias.clone().unwrap_or_else(|| self.path.name())
    }
}

/// Per-project settings that travel with the shared file.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProjectSettings {
    /// Refuse a multi-repo commit when repos are on different branches.
    pub require_same_branch: bool,
    /// Fixed ticket prefix, e.g. `JIRA-123`.
    pub ticket_prefix: Option<String>,
    /// Regex extracting a ticket prefix from the branch name.
    pub ticket_from_branch: Option<String>,
    /// Commit message templates.
    pub message_templates: Vec<MessageTemplateDef>,
    /// Accent color of the project's windows (`#rrggbb`; automatic when
    /// `None`), shared through the project file (spec §2).
    pub accent: Option<String>,
    /// Percentage (0–100) of the accent's hue mixed into the background of
    /// the project's windows, keeping the theme's lightness so it suits light
    /// and dark themes alike (`None`: untinted); shared through the project
    /// file (spec §2).
    pub background_tint: Option<u8>,
    /// Project icon / emoji, shared through the project file.
    pub icon: Option<String>,
}

/// A named commit message template.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageTemplateDef {
    /// Template name.
    pub name: String,
    /// Template body; `{ticket}` and `{branch}` are substituted.
    pub body: String,
}

/// A project definition: a name and a list of repositories.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectDef {
    /// Project name.
    pub name: String,
    /// Repositories, in display order.
    pub repos: Vec<RepoEntry>,
    /// Project settings.
    pub settings: ProjectSettings,
}

/// Where a project definition is stored. A definition always lives in a
/// project file, never in the user data dir: only personal state (UI state,
/// hidden repos) is kept there.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ProjectLocation {
    /// A shareable `archipel.toml` file.
    SharedFile(PathBuf),
}

/// A repository found by a folder scan.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoveredRepo {
    /// Working tree path.
    pub path: RepoPath,
    /// How the repository was detected.
    pub kind: DiscoveredKind,
}

/// How a scanned repository was detected.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DiscoveredKind {
    /// A regular `.git` directory.
    GitDir,
    /// A `.git` file (linked worktree or submodule).
    GitFile,
}

/// Options for a folder scan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScanOptions {
    /// Maximum depth below the root.
    pub max_depth: usize,
    /// Directory names never descended into.
    pub skip_dirs: Vec<String>,
    /// Skip hidden directories (starting with `.`).
    pub skip_hidden: bool,
    /// Keep descending inside a found repository (to find nested repos).
    pub descend_into_repos: bool,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            max_depth: 6,
            skip_dirs: [
                "node_modules",
                "target",
                "build",
                "dist",
                "vendor",
                "__pycache__",
            ]
            .into_iter()
            .map(String::from)
            .collect(),
            skip_hidden: true,
            descend_into_repos: false,
        }
    }
}

/// An entry of the user registry of known projects.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegistryEntry {
    /// Registry id.
    pub id: ProjectId,
    /// Display name (cached from the definition).
    pub name: String,
    /// Where the definition lives.
    pub location: ProjectLocation,
    /// Last time the project was opened (for the recent list).
    pub last_opened: Option<crate::time::Timestamp>,
}
