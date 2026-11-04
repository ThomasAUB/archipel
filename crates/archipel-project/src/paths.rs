//! Lexical path helpers: normalization, relative paths and portable rendering.
//!
//! Everything here is purely lexical: no file system access, no symlink
//! resolution. Paths stored in `.gitproject.toml` use `/` separators.

use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};

/// Separator used for relative paths written to a project file.
const FILE_SEPARATOR: &str = "/";

/// Removes `.` components and folds `..` into the preceding component.
///
/// A `..` that would climb above the root is dropped; a leading `..` of a
/// relative path is kept.
#[must_use]
pub(crate) fn normalize(path: &Path) -> PathBuf {
    let mut parts: Vec<Component<'_>> = Vec::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => match parts.last() {
                Some(Component::Normal(_)) => {
                    parts.pop();
                }
                Some(Component::RootDir | Component::Prefix(_)) => {}
                _ => parts.push(component),
            },
            _ => parts.push(component),
        }
    }
    parts.iter().collect()
}

/// Resolves a path read from a project file against `base` (the file's folder).
///
/// Both `/` and `\` are accepted as separators so files written on any OS load
/// everywhere. The result is absolute (when `base` is) and normalized.
#[must_use]
pub(crate) fn resolve(base: &Path, stored: &str) -> PathBuf {
    let stored_path = Path::new(stored);
    if stored_path.is_absolute() {
        return normalize(stored_path);
    }
    let portable = if cfg!(windows) {
        stored.to_owned()
    } else {
        stored.replace('\\', FILE_SEPARATOR)
    };
    normalize(&base.join(portable))
}

/// Path of `target` relative to `base`, both absolute and normalized.
///
/// Returns `None` when no relative path exists (e.g. different Windows drives)
/// or when a component is not valid UTF-8.
#[must_use]
pub(crate) fn relative(base: &Path, target: &Path) -> Option<String> {
    let base: Vec<Component<'_>> = base.components().collect();
    let target: Vec<Component<'_>> = target.components().collect();
    let common = base
        .iter()
        .zip(&target)
        .take_while(|(a, b)| same_component(**a, **b))
        .count();
    let anchored = |c: &Component<'_>| matches!(c, Component::Prefix(_) | Component::RootDir);
    if base.iter().take(common).filter(|c| anchored(c)).count()
        != base.iter().filter(|c| anchored(c)).count()
    {
        return None;
    }
    let mut parts: Vec<&str> = vec![".."; base.len() - common];
    for component in &target[common..] {
        match component {
            Component::Normal(name) => parts.push(name.to_str()?),
            Component::ParentDir => parts.push(".."),
            Component::CurDir => {}
            Component::Prefix(_) | Component::RootDir => return None,
        }
    }
    if parts.is_empty() {
        Some(".".to_owned())
    } else {
        Some(parts.join(FILE_SEPARATOR))
    }
}

/// Key used to detect duplicate paths (case-insensitive on Windows).
#[must_use]
pub(crate) fn dedup_key(path: &Path) -> String {
    let text = path.to_string_lossy();
    if cfg!(windows) {
        text.replace('/', "\\").to_lowercase()
    } else {
        text.into_owned()
    }
}

fn same_component(a: Component<'_>, b: Component<'_>) -> bool {
    match (a, b) {
        (Component::Normal(x), Component::Normal(y)) => same_name(x, y),
        _ => a == b,
    }
}

fn same_name(a: &OsStr, b: &OsStr) -> bool {
    if cfg!(windows) {
        a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase()
    } else {
        a == b
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> PathBuf {
        if cfg!(windows) {
            PathBuf::from(r"C:\work")
        } else {
            PathBuf::from("/work")
        }
    }

    #[test]
    fn normalize_folds_dots() {
        let p = root().join("a/./b/../c");
        assert_eq!(normalize(&p), root().join("a").join("c"));
    }

    #[test]
    fn normalize_does_not_climb_above_root() {
        let p = root().join("../../x");
        let expected = normalize(&root()).parent().unwrap().join("x");
        assert_eq!(normalize(&p), expected);
    }

    #[test]
    fn normalize_keeps_leading_parent_of_relative_path() {
        assert_eq!(normalize(Path::new("../a/./b")), PathBuf::from("../a/b"));
    }

    #[test]
    fn resolve_accepts_both_separators() {
        let base = root().join("proj");
        let expected = root().join("shared").join("libfoo");
        assert_eq!(resolve(&base, "../shared/libfoo"), expected);
        assert_eq!(resolve(&base, r"..\shared\libfoo"), expected);
    }

    #[test]
    fn relative_round_trips() {
        let base = root().join("proj");
        let target = root().join("shared").join("libfoo");
        let rel = relative(&base, &target).unwrap();
        assert_eq!(rel, "../shared/libfoo");
        assert_eq!(resolve(&base, &rel), target);
        assert_eq!(relative(&base, &base).unwrap(), ".");
        assert_eq!(relative(&base, &base.join("app")).unwrap(), "app");
    }

    #[cfg(windows)]
    #[test]
    fn relative_fails_across_drives() {
        assert_eq!(relative(Path::new(r"C:\a"), Path::new(r"D:\b")), None);
    }

    #[cfg(windows)]
    #[test]
    fn relative_ignores_case_on_windows() {
        let rel = relative(Path::new(r"C:\Work\Proj"), Path::new(r"c:\work\proj\app"));
        assert_eq!(rel.as_deref(), Some("app"));
    }
}
