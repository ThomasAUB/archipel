//! Copying a worktree's local files (`.env`, local configs) into another
//! worktree of the same repository (spec §10, `copy_to_worktrees`).

use std::fs;
use std::path::{Path, PathBuf};

use archipel_model::ports::LocalFilesCopier;
use archipel_model::{GitError, RepoPath};

use super::GitCli;

/// Name of the Git directory (never copied, never walked into).
const DOT_GIT: &str = ".git";

impl LocalFilesCopier for GitCli {
    fn copy_local_files(
        &self,
        from: &RepoPath,
        to: &RepoPath,
        patterns: &[String],
    ) -> Result<Vec<String>, GitError> {
        let mut found = Vec::new();
        for pattern in patterns {
            let parts: Vec<&str> = pattern
                .split(['/', '\\'])
                .filter(|p| !p.is_empty() && *p != ".")
                .collect();
            // A pattern leaving the worktree matches nothing.
            if parts.is_empty() || parts.contains(&"..") {
                continue;
            }
            walk(from.as_path(), &mut Vec::new(), &parts, &mut found);
        }
        found.sort();
        found.dedup();
        let mut copied = Vec::new();
        for rel in found {
            let source = join(from.as_path(), &rel);
            let target = join(to.as_path(), &rel);
            if fs::symlink_metadata(&target).is_ok() {
                continue;
            }
            let io = |e: std::io::Error| GitError::Io {
                path: target.clone(),
                message: e.to_string(),
            };
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent).map_err(io)?;
            }
            fs::copy(&source, &target).map_err(io)?;
            copied.push(rel);
        }
        Ok(copied)
    }
}

/// `base` joined with the `/`-separated relative path `rel`.
fn join(base: &Path, rel: &str) -> PathBuf {
    let mut p = base.to_path_buf();
    p.extend(rel.split('/'));
    p
}

/// Collects into `found` the regular files below `dir` (at relative
/// components `prefix`) matching the pattern components `parts`. Links are
/// never followed, `.git` never entered.
fn walk(dir: &Path, prefix: &mut Vec<String>, parts: &[&str], found: &mut Vec<String>) {
    let Some((first, rest)) = parts.split_first() else {
        return;
    };
    if *first == "**" {
        // Zero directories…
        walk(dir, prefix, rest, found);
        // …or one more, keeping `**`.
        for (name, path, is_dir) in entries(dir) {
            if is_dir {
                prefix.push(name);
                walk(&path, prefix, parts, found);
                prefix.pop();
            }
        }
        return;
    }
    for (name, path, is_dir) in entries(dir) {
        if !glob_match(first, &name) {
            continue;
        }
        prefix.push(name);
        if rest.is_empty() {
            if !is_dir {
                found.push(prefix.join("/"));
            }
        } else if is_dir {
            walk(&path, prefix, rest, found);
        }
        prefix.pop();
    }
}

/// The entries of `dir` that are regular files or directories (no links),
/// `.git` left out: (name, path, is a directory).
fn entries(dir: &Path) -> Vec<(String, PathBuf, bool)> {
    let Ok(read) = fs::read_dir(dir) else {
        return Vec::new();
    };
    read.filter_map(Result::ok)
        .filter_map(|e| {
            let name = e.file_name().to_str()?.to_owned();
            let meta = fs::symlink_metadata(e.path()).ok()?;
            let kind = meta.file_type();
            (name != DOT_GIT && (kind.is_file() || kind.is_dir()))
                .then(|| (name, e.path(), kind.is_dir()))
        })
        .collect()
}

/// Whether `name` matches the one-component glob `pattern` (`*`, `?`).
fn glob_match(pattern: &str, name: &str) -> bool {
    let (p, n): (Vec<char>, Vec<char>) = (pattern.chars().collect(), name.chars().collect());
    let (mut pi, mut ni) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while ni < n.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == n[ni]) {
            pi += 1;
            ni += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some((pi, ni));
            pi += 1;
        } else if let Some((sp, sn)) = star {
            pi = sp + 1;
            ni = sn + 1;
            star = Some((sp, sn + 1));
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|c| *c == '*')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs() {
        assert!(glob_match(".env", ".env"));
        assert!(glob_match("*.toml", "local.toml"));
        assert!(!glob_match("*.toml", "local.json"));
        assert!(glob_match(".env*", ".env.local"));
        assert!(glob_match("a?c", "abc"));
        assert!(!glob_match("a?c", "ac"));
    }

    #[test]
    fn copies_matching_files_without_overwriting() {
        let dir = tempfile::tempdir().unwrap();
        let (from, to) = (dir.path().join("main"), dir.path().join("wt"));
        for f in [
            ".env",
            "config/local.toml",
            "config/other.json",
            "deep/a/b/.env",
        ] {
            let p = join(&from, f);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(&p, f).unwrap();
        }
        fs::create_dir_all(from.join(".git")).unwrap();
        fs::write(from.join(".git/config"), "x").unwrap();
        fs::create_dir_all(to.join("config")).unwrap();
        fs::write(to.join("config/local.toml"), "kept").unwrap();
        let cli = GitCli::default();
        let copied = cli
            .copy_local_files(
                &RepoPath::new(&from),
                &RepoPath::new(&to),
                &[
                    ".env".into(),
                    "config/*.toml".into(),
                    "**/.env".into(),
                    "../outside".into(),
                    ".git/config".into(),
                    "missing".into(),
                ],
            )
            .unwrap();
        assert_eq!(copied, [".env", "deep/a/b/.env"]);
        assert_eq!(
            fs::read_to_string(to.join("config/local.toml")).unwrap(),
            "kept"
        );
        assert_eq!(
            fs::read_to_string(join(&to, "deep/a/b/.env")).unwrap(),
            "deep/a/b/.env"
        );
        assert!(!to.join(".git").exists());
    }
}
