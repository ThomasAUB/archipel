//! Worktrees of a repository (spec §10, WORKTREES.md): the main worktree and
//! the linked ones (`git worktree add`), as Git reports them, and the pure
//! rules for where new ones go (the location template).
//!
//! Worktrees are discovered, never stored in the project file: only how to
//! make them (the template, the files to copy) is configuration.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::ids::{Oid, RepoPath};
use crate::journal::HeadRef;

/// One worktree of a repository, as `git worktree list` reports it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeInfo {
    /// Folder of the worktree (for a bare main worktree: the repository).
    pub path: RepoPath,
    /// Where its `HEAD` points (`None` for a bare repository).
    pub head: Option<HeadRef>,
    /// Commit its `HEAD` resolves to (`None` when unborn or bare).
    pub head_oid: Option<Oid>,
    /// The repository's own checkout (listed first by Git).
    pub is_main: bool,
    /// A bare repository (main worktree only).
    pub is_bare: bool,
    /// Locked (`git worktree lock`), with the reason (may be empty).
    pub locked: Option<String>,
    /// Its folder is gone: `git worktree prune` would remove it; the
    /// reason Git gives.
    pub prunable: Option<String>,
}

impl WorktreeInfo {
    /// The short name of the branch checked out, if any.
    #[must_use]
    pub fn branch(&self) -> Option<&str> {
        match &self.head {
            Some(HeadRef::Symbolic(r)) => Some(r.strip_prefix("refs/heads/").unwrap_or(r)),
            _ => None,
        }
    }

    /// Its label: the branch, or `detached @ abc1234`.
    #[must_use]
    pub fn label(&self) -> String {
        match (&self.head, self.branch()) {
            (_, Some(b)) => b.to_owned(),
            (Some(HeadRef::Detached(oid)), None) => format!("detached @ {}", oid.short(7)),
            _ if self.is_bare => "bare".to_owned(),
            _ => "?".to_owned(),
        }
    }

    /// Whether its folder is missing (prunable).
    #[must_use]
    pub fn is_missing(&self) -> bool {
        self.prunable.is_some()
    }
}

/// What a new worktree checks out.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorktreeTarget {
    /// An existing local branch.
    Branch(String),
    /// A remote-tracking branch (`origin/x`): creates the local branch `x`
    /// tracking it.
    TrackRemote(String),
    /// A new branch `name` started at `base` (a branch, tag or commit;
    /// `HEAD` when `None`).
    NewBranch {
        /// New branch name.
        name: String,
        /// Start point.
        base: Option<String>,
    },
    /// Detached `HEAD` at a revision.
    Detached(String),
}

impl WorktreeTarget {
    /// The local branch the worktree ends up on, if any.
    #[must_use]
    pub fn branch(&self) -> Option<&str> {
        match self {
            Self::Branch(b) => Some(b),
            Self::TrackRemote(r) => Some(r.split_once('/').map_or(r.as_str(), |(_, b)| b)),
            Self::NewBranch { name, .. } => Some(name),
            Self::Detached(_) => None,
        }
    }
}

/// A request to create a worktree.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeAddRequest {
    /// Folder of the new worktree (must not exist, or be empty).
    pub path: PathBuf,
    /// What it checks out.
    pub target: WorktreeTarget,
}

/// A Worktree Set (WORKTREES.md §5): the same branch checked out as a
/// worktree in several repos of the project, under one folder. Personal
/// state, keyed by the worktree paths.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeSet {
    /// Name, unique in the project (usually the branch).
    pub name: String,
    /// Branch checked out in every member.
    pub branch: String,
    /// The Set Folder holding the members.
    pub folder: PathBuf,
    /// One worktree per repo.
    pub members: Vec<SetMember>,
}

/// A worktree of a [`WorktreeSet`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetMember {
    /// The repo (its main worktree) the member belongs to.
    pub repo: crate::ids::RepoId,
    /// Folder of the member worktree.
    pub path: RepoPath,
}

/// What the dashboard shows of worktrees (the Set Selector, §5.3).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SetView {
    /// Repos with their linked worktrees nested.
    #[default]
    All,
    /// Main worktrees only.
    MainOnly,
    /// The members of the named set, in place of their repos.
    Set(String),
}

/// Default location template of a new worktree, relative to the main
/// worktree of its repository (WORKTREES.md §8).
pub const DEFAULT_WORKTREE_LOCATION: &str = "../{repo}.worktrees/{branch}";

/// Default location template of a Worktree Set member, relative to the
/// project file's folder.
pub const DEFAULT_SET_LOCATION: &str = "../{project}.worktrees/{set}/{repo}";

/// Values substituted in a location template.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LocationVars<'a> {
    /// Project name (`{project}`).
    pub project: &'a str,
    /// Repo display name (`{repo}`).
    pub repo: &'a str,
    /// Branch name (`{branch}`), slugged.
    pub branch: &'a str,
    /// Set name (`{set}`), slugged.
    pub set: &'a str,
}

/// A name made safe as one path component on every OS: `/` and `\` become
/// `-`, characters forbidden on Windows and control characters are
/// dropped, trailing dots and spaces trimmed (Windows), and names reserved
/// on Windows (`con`, `nul`…) get a `_` suffix. Never empty.
#[must_use]
pub fn path_slug(name: &str) -> String {
    let mut out: String = name
        .chars()
        .filter_map(|c| match c {
            '/' | '\\' => Some('-'),
            '<' | '>' | ':' | '"' | '|' | '?' | '*' => None,
            c if c.is_control() => None,
            c => Some(c),
        })
        .collect();
    while out.ends_with(['.', ' ']) {
        out.pop();
    }
    let out = out.trim_start().to_owned();
    let stem = out
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let reserved = matches!(stem.as_str(), "con" | "prn" | "aux" | "nul")
        || ((stem.starts_with("com") || stem.starts_with("lpt"))
            && stem.len() == 4
            && stem.as_bytes()[3].is_ascii_digit());
    match out {
        o if o.is_empty() || o == "." || o == ".." => "worktree".to_owned(),
        o if reserved => format!("{o}_"),
        o => o,
    }
}

/// The folder a location template gives: placeholders substituted (the
/// branch and set slugged with [`path_slug`]), `/` as the separator on
/// every OS, made absolute against `base` when relative, and `.` / `..`
/// resolved lexically.
#[must_use]
pub fn expand_location(template: &str, vars: &LocationVars<'_>, base: &Path) -> PathBuf {
    let text = template
        .replace("{project}", &path_slug(vars.project))
        .replace("{repo}", &path_slug(vars.repo))
        .replace("{branch}", &path_slug(vars.branch))
        .replace("{set}", &path_slug(vars.set));
    let relative = Path::new(&text);
    let joined = if relative.is_absolute() || text.starts_with('~') {
        PathBuf::from(&text)
    } else {
        let mut p = base.to_path_buf();
        p.extend(text.split(['/', '\\']).filter(|s| !s.is_empty()));
        p
    };
    normalize_lexically(&joined)
}

/// Resolves `.` and `..` without touching the disk.
#[must_use]
pub fn normalize_lexically(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_are_safe_path_components() {
        assert_eq!(path_slug("feat/parser"), "feat-parser");
        assert_eq!(path_slug("a\\b"), "a-b");
        assert_eq!(path_slug("what?:*"), "what");
        assert_eq!(path_slug("trailing. "), "trailing");
        assert_eq!(path_slug("con"), "con_");
        assert_eq!(path_slug("COM1.txt"), "COM1.txt_");
        assert_eq!(path_slug("component"), "component");
        assert_eq!(path_slug(""), "worktree");
        assert_eq!(path_slug(".."), "worktree");
    }

    #[test]
    fn template_expands_the_same_on_every_os() {
        let vars = LocationVars {
            project: "My project",
            repo: "libfoo",
            branch: "feat/parser",
            set: "",
        };
        let base = Path::new("/work/libfoo");
        let got = expand_location(DEFAULT_WORKTREE_LOCATION, &vars, base);
        let mut want = PathBuf::from("/work");
        want.push("libfoo.worktrees");
        want.push("feat-parser");
        assert_eq!(got, want);
        let set = LocationVars {
            set: "agent/one",
            ..vars
        };
        let got = expand_location(DEFAULT_SET_LOCATION, &set, Path::new("/work"));
        let mut want = PathBuf::from("/");
        want.push("My project.worktrees");
        want.push("agent-one");
        want.push("libfoo");
        assert_eq!(got, want);
    }

    #[test]
    fn labels() {
        let oid = Oid::from_bytes(&[0xab; 20]).unwrap();
        let mut wt = WorktreeInfo {
            path: RepoPath::new("/w"),
            head: Some(HeadRef::Symbolic("refs/heads/feat/x".into())),
            head_oid: Some(oid),
            is_main: false,
            is_bare: false,
            locked: None,
            prunable: None,
        };
        assert_eq!(wt.branch(), Some("feat/x"));
        assert_eq!(wt.label(), "feat/x");
        wt.head = Some(HeadRef::Detached(oid));
        assert_eq!(wt.label(), "detached @ abababa");
        assert_eq!(
            WorktreeTarget::TrackRemote("origin/feat/y".into()).branch(),
            Some("feat/y")
        );
    }
}
