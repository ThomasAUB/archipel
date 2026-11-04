//! Detecting the installed `git` version.

use std::fmt;
use std::path::Path;
use std::process::Command;

use archipel_model::GitError;

/// A parsed `git --version`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct GitVersion {
    /// Major version.
    pub major: u32,
    /// Minor version.
    pub minor: u32,
    /// Patch version.
    pub patch: u32,
}

impl GitVersion {
    /// Parses the output of `git --version` (`git version 2.43.0.windows.1`).
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let rest = text.trim().strip_prefix("git version ")?;
        let mut parts = rest
            .split(|c: char| !c.is_ascii_digit())
            .filter(|s| !s.is_empty());
        let major = parts.next()?.parse().ok()?;
        let minor = parts.next()?.parse().ok()?;
        let patch = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
        Some(Self {
            major,
            minor,
            patch,
        })
    }

    /// Whether this version is at least `major.minor`.
    #[must_use]
    pub fn at_least(&self, (major, minor): (u32, u32)) -> bool {
        (self.major, self.minor) >= (major, minor)
    }
}

impl fmt::Display for GitVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// Runs `git --version` and checks it against the minimum supported version.
pub fn check_git_version(program: &Path, minimum: (u32, u32)) -> Result<GitVersion, GitError> {
    let out = Command::new(program)
        .arg("--version")
        .output()
        .map_err(|e| GitError::Io {
            path: program.to_path_buf(),
            message: format!("cannot run git: {e}"),
        })?;
    let text = String::from_utf8_lossy(&out.stdout);
    let version = GitVersion::parse(&text).ok_or_else(|| GitError::Parse {
        what: "git --version",
        detail: text.trim().to_owned(),
    })?;
    if version.at_least(minimum) {
        Ok(version)
    } else {
        Err(GitError::InvalidRequest(format!(
            "git {version} is too old: Archipel needs git {}.{} or newer",
            minimum.0, minimum.1
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_versions() {
        let v = GitVersion::parse("git version 2.43.0.windows.1\n").unwrap();
        assert_eq!(
            v,
            GitVersion {
                major: 2,
                minor: 43,
                patch: 0
            }
        );
        assert!(v.at_least((2, 40)));
        assert!(!v.at_least((2, 44)));
        assert_eq!(GitVersion::parse("git version 2.39").unwrap().patch, 0);
        assert!(GitVersion::parse("hello").is_none());
    }

    #[test]
    fn installed_git_is_supported() {
        assert!(check_git_version(Path::new("git"), (2, 40)).is_ok());
    }
}
