//! Conflict forecast with `git merge-tree --write-tree` (git ≥ 2.40): the
//! merge runs in memory, without touching the worktree, the index or any
//! ref. Only objects are written (trees, and the commits of a replayed
//! rebase), as dangling ones that `git gc` prunes.

use archipel_model::forecast::{MergeForecast, MergeStrategy};
use archipel_model::ports::MergeForecaster;
use archipel_model::{CancelToken, GitError, Oid, RepoPath};

use super::GitCli;
use super::runner::{Cmd, CmdKind};

/// Most commits a rebase forecast replays; past that, a merge is checked
/// instead (one `merge-tree` per commit would cost too much).
const MAX_REPLAYED: usize = 100;

/// Identity of the commits written while replaying a rebase: they are
/// never referenced, so it only has to keep `commit-tree` from failing
/// where no identity is configured.
const REPLAY_IDENTITY: [(&str, &str); 4] = [
    ("GIT_AUTHOR_NAME", "archipel"),
    ("GIT_AUTHOR_EMAIL", "archipel@localhost"),
    ("GIT_COMMITTER_NAME", "archipel"),
    ("GIT_COMMITTER_EMAIL", "archipel@localhost"),
];

/// Outcome of one `merge-tree`.
enum Merged {
    /// Clean: the id of the merged tree.
    Clean(String),
    /// The conflicting paths.
    Conflicts(Vec<String>),
}

/// Whether a `branch.<name>.rebase` / `pull.rebase` value asks to rebase
/// (`true`, `merges`, `interactive`…), rather than to merge.
fn asks_rebase(value: &str) -> bool {
    !matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "false" | "no" | "off" | "0"
    )
}

fn parse_error(what: &'static str, detail: &str) -> GitError {
    GitError::Parse {
        what,
        detail: detail.to_owned(),
    }
}

impl GitCli {
    /// A config value, `None` when unset.
    fn config_value(&self, repo: &RepoPath, key: &str) -> Result<Option<String>, GitError> {
        let out = self.runner.run(
            Cmd::new(repo.as_path(), CmdKind::Read, &["config", "--get", key]).ok_codes(&[0, 1]),
        )?;
        let value = out.stdout_text();
        Ok((out.code == Some(0) && !value.is_empty()).then_some(value))
    }

    /// Merges `theirs` into `ours` in memory, with `base` as the merge base
    /// when given (git computes it otherwise).
    fn merge_tree(
        &self,
        repo: &RepoPath,
        ours: &str,
        theirs: &str,
        base: Option<&str>,
        cancel: &CancelToken,
    ) -> Result<Merged, GitError> {
        let mut cmd = Cmd::new(
            repo.as_path(),
            CmdKind::Read,
            &[
                "merge-tree",
                "--write-tree",
                "-z",
                "--name-only",
                "--no-messages",
            ],
        )
        .cancel(cancel)
        // 1: the merge has conflicts.
        .ok_codes(&[0, 1]);
        if let Some(base) = base {
            cmd = cmd.arg(format!("--merge-base={base}"));
        }
        let out = self.runner.run(cmd.arg(ours).arg(theirs))?;
        let text = String::from_utf8_lossy(&out.stdout);
        // `<tree> NUL (<path> NUL)*`, then the (disabled) messages.
        let mut fields = text.split('\0');
        let tree = fields
            .next()
            .filter(|t| !t.is_empty())
            .ok_or_else(|| parse_error("git merge-tree", &text))?
            .to_owned();
        if out.code == Some(0) {
            return Ok(Merged::Clean(tree));
        }
        let mut paths: Vec<String> = fields
            .take_while(|p| !p.is_empty())
            .map(str::to_owned)
            .collect();
        paths.sort();
        paths.dedup();
        Ok(Merged::Conflicts(paths))
    }

    /// The commits a rebase of `ours` onto `theirs` would replay, oldest
    /// first: those not on `theirs`, merges and commits already applied
    /// there left out, as `git rebase` does.
    fn replayed(
        &self,
        repo: &RepoPath,
        ours: Oid,
        theirs: Oid,
        cancel: &CancelToken,
    ) -> Result<Vec<Oid>, GitError> {
        let text = self
            .runner
            .run(
                Cmd::new(
                    repo.as_path(),
                    CmdKind::Read,
                    &[
                        "rev-list",
                        "--reverse",
                        "--topo-order",
                        "--no-merges",
                        "--right-only",
                        "--cherry-pick",
                    ],
                )
                .arg(format!("{}...{}", theirs.to_hex(), ours.to_hex()))
                .cancel(cancel),
            )?
            .stdout_text();
        text.lines()
            .filter(|l| !l.is_empty())
            .map(|l| Oid::from_hex(l).map_err(|_| parse_error("git rev-list", l)))
            .collect()
    }

    /// Writes a dangling commit of `tree` on `parent`, standing for a
    /// replayed commit.
    fn replay_commit(
        &self,
        repo: &RepoPath,
        tree: &str,
        parent: &str,
        cancel: &CancelToken,
    ) -> Result<String, GitError> {
        let mut cmd = Cmd::new(
            repo.as_path(),
            CmdKind::Write,
            &["commit-tree", "--no-gpg-sign", "-m", "archipel forecast"],
        )
        .arg(tree)
        .arg("-p")
        .arg(parent)
        .cancel(cancel);
        for (key, value) in REPLAY_IDENTITY {
            cmd = cmd.env(key, value);
        }
        let text = self.runner.run(cmd)?.stdout_text();
        Oid::from_hex(&text)
            .map(|oid| oid.to_hex())
            .map_err(|_| parse_error("git commit-tree", &text))
    }

    /// Replays `commits` onto `onto` one at a time, as a rebase does; stops
    /// at the first that conflicts.
    fn forecast_rebase(
        &self,
        repo: &RepoPath,
        commits: &[Oid],
        onto: Oid,
        cancel: &CancelToken,
    ) -> Result<Vec<String>, GitError> {
        let mut onto = onto.to_hex();
        for (i, commit) in commits.iter().enumerate() {
            cancel.check()?;
            let commit = commit.to_hex();
            let base = format!("{commit}^");
            match self.merge_tree(repo, &onto, &commit, Some(&base), cancel)? {
                Merged::Conflicts(paths) => return Ok(paths),
                Merged::Clean(tree) if i + 1 < commits.len() => {
                    onto = self.replay_commit(repo, &tree, &onto, cancel)?;
                }
                Merged::Clean(_) => {}
            }
        }
        Ok(Vec::new())
    }
}

impl MergeForecaster for GitCli {
    fn pull_strategy(&self, repo: &RepoPath, branch: &str) -> Result<MergeStrategy, GitError> {
        let value = match self.config_value(repo, &format!("branch.{branch}.rebase"))? {
            Some(v) => Some(v),
            None => self.config_value(repo, "pull.rebase")?,
        };
        Ok(match value {
            Some(v) if asks_rebase(&v) => MergeStrategy::Rebase,
            _ => MergeStrategy::Merge,
        })
    }

    fn forecast(
        &self,
        repo: &RepoPath,
        ours: Oid,
        theirs: Oid,
        strategy: MergeStrategy,
        cancel: &CancelToken,
    ) -> Result<MergeForecast, GitError> {
        if strategy == MergeStrategy::Rebase {
            let commits = self.replayed(repo, ours, theirs, cancel)?;
            if commits.len() <= MAX_REPLAYED {
                return Ok(MergeForecast {
                    strategy,
                    conflicts: self.forecast_rebase(repo, &commits, theirs, cancel)?,
                });
            }
        }
        let conflicts =
            match self.merge_tree(repo, &ours.to_hex(), &theirs.to_hex(), None, cancel)? {
                Merged::Clean(_) => Vec::new(),
                Merged::Conflicts(paths) => paths,
            };
        Ok(MergeForecast {
            strategy: MergeStrategy::Merge,
            conflicts,
        })
    }

    fn incoming_paths(
        &self,
        repo: &RepoPath,
        ours: Oid,
        theirs: Oid,
    ) -> Result<Vec<String>, GitError> {
        // `A...B`: from the merge base of both to `B`. Without rename
        // detection, a renamed file lists both its old and new paths.
        let out = self.runner.run(
            Cmd::new(
                repo.as_path(),
                CmdKind::Read,
                &["diff", "--name-only", "-z", "--no-renames", "--no-ext-diff"],
            )
            .arg(format!("{}...{}", ours.to_hex(), theirs.to_hex())),
        )?;
        let mut paths: Vec<String> = String::from_utf8_lossy(&out.stdout)
            .split('\0')
            .filter(|p| !p.is_empty())
            .map(str::to_owned)
            .collect();
        paths.sort();
        Ok(paths)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rebase_values() {
        for v in ["true", "merges", "interactive", "i", "preserve", "TRUE"] {
            assert!(asks_rebase(v), "{v}");
        }
        for v in ["false", "no", "off", "0", " False\n"] {
            assert!(!asks_rebase(v), "{v}");
        }
    }
}
