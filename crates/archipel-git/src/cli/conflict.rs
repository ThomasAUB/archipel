//! Conflicted paths of the index: their three versions, resolving them and
//! bringing a resolved conflict back (undo), for the merge view.

use archipel_model::changes::ConflictSides;
use archipel_model::ports::{ConflictResolver, WorktreeFiles as _};
use archipel_model::{GitError, Oid, RepoPath};

use super::GitCli;
use super::files::worktree_path;
use super::runner::{Cmd, CmdKind};
use super::write::nul_separated;

/// A pathspec matching `path` exactly (no glob, even with `*` or `?`).
fn literal(path: &str) -> String {
    format!(":(literal){path}")
}

/// The unmerged entries of `git ls-files -u -z` naming `path`:
/// `<mode> <oid> <stage>\t<path>` → `(mode, oid, stage)`.
fn unmerged_entries<'a>(listing: &'a [String], path: &str) -> Vec<(&'a str, &'a str, u8)> {
    listing
        .iter()
        .filter_map(|r| {
            let (meta, p) = r.split_once('\t')?;
            if p != path {
                return None;
            }
            let mut fields = meta.split(' ');
            let mode = fields.next()?;
            let oid = fields.next()?;
            let stage = fields.next()?.parse().ok()?;
            Some((mode, oid, stage))
        })
        .collect()
}

/// Whether a `git status --porcelain` `XY` code is an unmerged one.
fn is_unmerged(xy: &[u8]) -> bool {
    matches!(xy, [b'U', _] | [_, b'U'] | [b'A', b'A'] | [b'D', b'D'])
}

impl GitCli {
    /// Writes `contents` over `path`, creating it (and its folders) when
    /// the worktree no longer has it.
    fn put_file(&self, repo: &RepoPath, path: &str, contents: &[u8]) -> Result<(), GitError> {
        let file = worktree_path(repo, path)?;
        if file.exists() {
            return self.write_file(repo, path, contents);
        }
        if let Some(dir) = file.parent() {
            std::fs::create_dir_all(dir).map_err(|e| GitError::Io {
                path: dir.to_owned(),
                message: e.to_string(),
            })?;
        }
        self.create_file(repo, path, contents)
    }
}

impl ConflictResolver for GitCli {
    fn conflict_sides(&self, repo: &RepoPath, path: &str) -> Result<ConflictSides, GitError> {
        let out = self.runner.run(
            Cmd::new(
                repo.as_path(),
                CmdKind::Read,
                &["ls-files", "-u", "-z", "--"],
            )
            .arg(literal(path)),
        )?;
        let listing = nul_separated(&out.stdout);
        let entries = unmerged_entries(&listing, path);
        if entries.is_empty() {
            return Err(GitError::InvalidRequest(format!(
                "{path} is not conflicted"
            )));
        }
        let mut sides = ConflictSides::default();
        for (mode, oid, stage) in entries {
            if mode == "160000" {
                return Err(GitError::InvalidRequest(format!(
                    "{path} is a submodule: pick its commit in a terminal"
                )));
            }
            let blob = self
                .runner
                .run(Cmd::new(
                    repo.as_path(),
                    CmdKind::Read,
                    &["cat-file", "blob", oid],
                ))?
                .stdout;
            match stage {
                1 => sides.base = Some(blob),
                2 => sides.ours = Some(blob),
                3 => sides.theirs = Some(blob),
                _ => {}
            }
        }
        Ok(sides)
    }

    fn resolve_conflict(
        &self,
        repo: &RepoPath,
        path: &str,
        contents: Option<&[u8]>,
    ) -> Result<(), GitError> {
        match contents {
            Some(contents) => {
                self.put_file(repo, path, contents)?;
                self.runner.run(
                    Cmd::new(repo.as_path(), CmdKind::Write, &["add", "--"]).arg(literal(path)),
                )?;
            }
            None => {
                self.runner.run(
                    Cmd::new(
                        repo.as_path(),
                        CmdKind::Write,
                        &["rm", "-q", "-f", "--ignore-unmatch", "--"],
                    )
                    .arg(literal(path)),
                )?;
            }
        }
        Ok(())
    }

    fn unresolve_conflicts(
        &self,
        repo: &RepoPath,
        paths: &[String],
        snapshot: Oid,
    ) -> Result<(), GitError> {
        if paths.is_empty() {
            return Ok(());
        }
        let pathspecs: Vec<String> = paths.iter().map(|p| literal(p)).collect();
        let status = self.runner.run(
            Cmd::new(
                repo.as_path(),
                CmdKind::Read,
                &[
                    "status",
                    "--porcelain=v1",
                    "-z",
                    "--untracked-files=all",
                    "--no-renames",
                    "--",
                ],
            )
            .args(pathspecs.iter().map(String::as_str)),
        )?;
        for entry in nul_separated(&status.stdout) {
            let xy = entry.as_bytes().get(..2).unwrap_or_default();
            let path = entry.get(3..).unwrap_or_default();
            if is_unmerged(xy) {
                return Err(GitError::InvalidRequest(format!(
                    "{path} is conflicted already"
                )));
            }
            if xy.get(1).is_some_and(|y| *y != b' ') {
                return Err(GitError::InvalidRequest(format!(
                    "{path} changed since it was resolved: bringing the conflict back would overwrite it"
                )));
            }
        }
        // The index keeps what `add` / `rm` replaced (resolve-undo):
        // `--unresolve` puts the conflict stages back from it, leaving the
        // worktree alone.
        self.runner.run(
            Cmd::new(
                repo.as_path(),
                CmdKind::Write,
                &["update-index", "--unresolve", "--"],
            )
            .args(paths.iter().map(String::as_str)),
        )?;
        let still: Vec<String> = paths
            .iter()
            .filter(|p| self.conflict_sides(repo, p).is_err())
            .cloned()
            .collect();
        if !still.is_empty() {
            return Err(GitError::InvalidRequest(format!(
                "Git no longer knows the conflict of {}",
                still.join(", ")
            )));
        }
        // Then the file as it was when resolved (edits made before
        // resolving included), or no file when there was none.
        let hex = snapshot.to_hex();
        for path in paths {
            let blob = self.runner.run(
                Cmd::new(
                    repo.as_path(),
                    CmdKind::Read,
                    &["cat-file", "blob", &format!("{hex}:{path}")],
                )
                .ok_codes(&[0, 128]),
            )?;
            if blob.code == Some(0) {
                self.put_file(repo, path, &blob.stdout)?;
            } else {
                let file = worktree_path(repo, path)?;
                if file.exists() {
                    std::fs::remove_file(&file).map_err(|e| GitError::Io {
                        path: file.clone(),
                        message: e.to_string(),
                    })?;
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_unmerged_entries_of_one_path() {
        let listing = vec![
            "100644 aaaa 1\tc.txt".to_owned(),
            "100644 bbbb 2\tc.txt".to_owned(),
            "100755 cccc 3\tc.txt".to_owned(),
            "100644 dddd 2\tc.txt.orig".to_owned(),
        ];
        assert_eq!(
            unmerged_entries(&listing, "c.txt"),
            [
                ("100644", "aaaa", 1),
                ("100644", "bbbb", 2),
                ("100755", "cccc", 3)
            ]
        );
        assert!(unmerged_entries(&listing, "other").is_empty());
    }

    #[test]
    fn tells_unmerged_status_codes() {
        for code in ["UU", "AU", "UA", "DU", "UD", "AA", "DD"] {
            assert!(is_unmerged(code.as_bytes()), "{code}");
        }
        for code in ["M ", " M", "A ", "D ", "??"] {
            assert!(!is_unmerged(code.as_bytes()), "{code}");
        }
    }
}
