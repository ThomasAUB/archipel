//! Writer traits implemented with the CLI (the only write path, by design).

use std::path::Path;

use archipel_model::ports::{
    BranchWriter, CommitWriter, HistoryRewriter, IndexWriter, MergeToolLauncher, RefReader,
    RefWriter, RemoteOps, StashWriter, TagWriter, WorktreeSnapshots,
};
use archipel_model::requests::{
    CheckoutTarget, CommitMode, CommitRequest, DeleteMode, FetchRequest, ForceMode, MergeMode,
    PatchDirection, PullMode, PushRequest, ResetMode, SequenceAction, StashApplyMode, StashRequest,
    TagRequest, UntrackedMode,
};
use archipel_model::{CancelToken, GitError, Oid, ProgressSink, RepoOp, RepoPath};

use super::GitCli;
use super::runner::{Cmd, CmdKind};
use crate::gitdir::GitDirs;

impl GitCli {
    fn write(&self, repo: &RepoPath, args: &[&str]) -> Result<String, GitError> {
        Ok(self
            .runner
            .run(Cmd::new(repo.as_path(), CmdKind::Write, args))?
            .stdout_text())
    }

    fn head(&self, repo: &RepoPath) -> Result<Oid, GitError> {
        self.resolve(repo, "HEAD")?
            .ok_or_else(|| GitError::InvalidRequest("HEAD has no commit".into()))
    }

    /// Recovers from `failed` (a `git branch <name>` or `git switch <name>`)
    /// when `HEAD` is on a branch with no commit yet: a fresh repository or
    /// an orphan branch. Git refuses both there (no commit to point the new
    /// branch at; no such branch to switch to), yet the unborn branch only
    /// exists as `HEAD`, so pointing `HEAD` at `name` loses nothing: the
    /// first commit then lands on `name`, and the index is kept. Anything
    /// else (HEAD has a commit, `name` is an existing branch) returns
    /// `failed` unchanged.
    fn move_unborn_head(
        &self,
        repo: &RepoPath,
        name: &str,
        failed: GitError,
    ) -> Result<String, GitError> {
        if self.resolve(repo, "HEAD")?.is_some()
            || self.resolve(repo, &format!("refs/heads/{name}"))?.is_some()
        {
            return Err(failed);
        }
        // `--create=` keeps a name starting with `-` from reading as an
        // option; Git validates the name.
        self.write(repo, &["switch", &format!("--create={name}")])
    }

    /// Splits paths into tracked and untracked ones.
    fn partition_tracked(
        &self,
        repo: &RepoPath,
        paths: &[String],
    ) -> Result<(Vec<String>, Vec<String>), GitError> {
        let out = self.runner.run(
            Cmd::new(repo.as_path(), CmdKind::Read, &["ls-files", "-z", "--"])
                .args(paths.iter().map(String::as_str)),
        )?;
        let text = String::from_utf8_lossy(&out.stdout);
        let tracked: Vec<&str> = text.split('\0').filter(|s| !s.is_empty()).collect();
        Ok(paths.iter().cloned().partition(|p| {
            tracked.iter().any(|t| {
                t == p
                    || t.strip_prefix(p.as_str())
                        .is_some_and(|r| r.starts_with('/'))
            })
        }))
    }

    /// The submodules (gitlink entries) among `paths` in `tree` (the index
    /// when `None`), with the commit each one records.
    fn gitlinks(
        &self,
        repo: &RepoPath,
        tree: Option<&str>,
        paths: &[String],
    ) -> Result<Vec<(String, String)>, GitError> {
        let cmd = match tree {
            Some(t) => Cmd::new(repo.as_path(), CmdKind::Read, &["ls-tree", "-r", "-z", t]),
            None => Cmd::new(repo.as_path(), CmdKind::Read, &["ls-files", "-s", "-z"]),
        };
        let out = self
            .runner
            .run(cmd.arg("--").args(paths.iter().map(String::as_str)))?;
        // `ls-tree`: `160000 commit <oid>\t<path>`; `ls-files -s`:
        // `160000 <oid> <stage>\t<path>`.
        Ok(nul_separated(&out.stdout)
            .into_iter()
            .filter_map(|e| {
                let (meta, path) = e.split_once('\t')?;
                let mut fields = meta.split(' ');
                if fields.next()? != "160000" {
                    return None;
                }
                let oid = fields.find(|f| f.len() >= 40)?;
                Some((path.to_owned(), oid.to_owned()))
            })
            .collect())
    }
}

impl CommitWriter for GitCli {
    fn commit(&self, repo: &RepoPath, request: &CommitRequest) -> Result<Oid, GitError> {
        // The message goes through stdin (`-F -`) so hooks and signing run normally.
        let mut cmd = Cmd::new(repo.as_path(), CmdKind::Write, &["commit", "-F", "-"])
            .stdin(request.message.clone());
        if request.mode == CommitMode::Amend {
            cmd = cmd.arg("--amend");
        }
        self.runner.run(cmd)?;
        self.head(repo)
    }
}

impl IndexWriter for GitCli {
    fn stage(&self, repo: &RepoPath, paths: &[String]) -> Result<(), GitError> {
        self.runner.run(
            Cmd::new(repo.as_path(), CmdKind::Write, &["add", "-A", "--"])
                .args(paths.iter().map(String::as_str)),
        )?;
        Ok(())
    }

    fn unstage(&self, repo: &RepoPath, paths: &[String]) -> Result<(), GitError> {
        let args: &[&str] = if self.resolve(repo, "HEAD")?.is_some() {
            &["restore", "--staged", "--"]
        } else {
            // No commit yet: there is nothing to restore from.
            &["rm", "--cached", "-r", "-q", "--"]
        };
        self.runner.run(
            Cmd::new(repo.as_path(), CmdKind::Write, args).args(paths.iter().map(String::as_str)),
        )?;
        Ok(())
    }

    fn apply_patch(
        &self,
        repo: &RepoPath,
        patch: &str,
        direction: PatchDirection,
    ) -> Result<(), GitError> {
        let mut cmd = Cmd::new(
            repo.as_path(),
            CmdKind::Write,
            &["apply", "--cached", "--whitespace=nowarn", "-"],
        )
        .stdin(patch.to_owned());
        if direction == PatchDirection::Reverse {
            cmd = cmd.arg("--reverse");
        }
        self.runner.run(cmd)?;
        Ok(())
    }

    fn discard_patch(&self, repo: &RepoPath, patch: &str) -> Result<(), GitError> {
        self.runner.run(
            Cmd::new(
                repo.as_path(),
                CmdKind::Write,
                &["apply", "--reverse", "--whitespace=nowarn", "-"],
            )
            .stdin(patch.to_owned()),
        )?;
        Ok(())
    }

    fn discard(&self, repo: &RepoPath, paths: &[String]) -> Result<(), GitError> {
        let (tracked, untracked) = self.partition_tracked(repo, paths)?;
        // `restore` never touches a submodule's checkout: put it back on the
        // commit the index records, and drop the changes made inside it.
        let submodules = self.gitlinks(repo, None, &tracked)?;
        let tracked: Vec<String> = tracked
            .into_iter()
            .filter(|p| !submodules.iter().any(|(s, _)| s == p))
            .collect();
        for (path, oid) in &submodules {
            let sub = RepoPath::new(repo.as_path().join(path));
            let Some(head) = self.resolve(&sub, "HEAD")? else {
                continue; // not initialized: nothing checked out to discard
            };
            // Same commit: keep the branch checked out there, if any.
            let reset: &[&str] = if head.to_hex() == *oid {
                &["reset", "-q", "--hard"]
            } else {
                &["checkout", "-q", "-f", "--detach", oid]
            };
            self.write(&sub, reset)?;
            self.write(&sub, &["clean", "-f", "-d", "-q"])?;
        }
        if !tracked.is_empty() {
            self.runner.run(
                Cmd::new(
                    repo.as_path(),
                    CmdKind::Write,
                    &["restore", "--worktree", "--"],
                )
                .args(tracked.iter().map(String::as_str)),
            )?;
        }
        if !untracked.is_empty() {
            self.runner.run(
                Cmd::new(
                    repo.as_path(),
                    CmdKind::Write,
                    &["clean", "-f", "-d", "-q", "--"],
                )
                .args(untracked.iter().map(String::as_str)),
            )?;
        }
        Ok(())
    }
}

impl BranchWriter for GitCli {
    fn checkout(&self, repo: &RepoPath, target: &CheckoutTarget) -> Result<(), GitError> {
        match target {
            CheckoutTarget::Branch(name) => self
                .write(repo, &["switch", "--no-guess", "--end-of-options", name])
                .or_else(|e| self.move_unborn_head(repo, name, e)),
            CheckoutTarget::TrackRemote(remote) => {
                self.write(repo, &["switch", "--track", "--end-of-options", remote])
            }
            CheckoutTarget::Detached(oid) => {
                self.write(repo, &["switch", "--detach", &oid.to_hex()])
            }
        }
        .map(drop)
    }

    fn create_branch(
        &self,
        repo: &RepoPath,
        name: &str,
        start: Option<&str>,
    ) -> Result<(), GitError> {
        let mut args = vec!["branch", "--no-track", "--end-of-options", name];
        args.extend(start);
        let created = self.write(repo, &args);
        match start {
            None => created.or_else(|e| self.move_unborn_head(repo, name, e)),
            Some(_) => created,
        }
        .map(drop)
    }

    fn rename_branch(&self, repo: &RepoPath, old: &str, new: &str) -> Result<(), GitError> {
        self.write(repo, &["branch", "-m", "--end-of-options", old, new])
            .map(drop)
    }

    fn delete_branch(&self, repo: &RepoPath, name: &str, mode: DeleteMode) -> Result<(), GitError> {
        let flag = if mode == DeleteMode::Force {
            "-D"
        } else {
            "-d"
        };
        self.write(repo, &["branch", flag, "--end-of-options", name])
            .map(drop)
    }

    fn set_upstream(
        &self,
        repo: &RepoPath,
        branch: &str,
        upstream: Option<&str>,
    ) -> Result<(), GitError> {
        match upstream {
            Some(up) => self.write(
                repo,
                &[
                    "branch",
                    &format!("--set-upstream-to={up}"),
                    "--end-of-options",
                    branch,
                ],
            ),
            None => self.write(
                repo,
                &["branch", "--unset-upstream", "--end-of-options", branch],
            ),
        }
        .map(drop)
    }
}

impl TagWriter for GitCli {
    fn create_tag(&self, repo: &RepoPath, request: &TagRequest) -> Result<(), GitError> {
        let mut cmd = Cmd::new(repo.as_path(), CmdKind::Write, &["tag"]);
        if let Some(message) = &request.message {
            cmd = cmd.args(["-a", "-F", "-"]).stdin(message.clone());
        }
        cmd = cmd.args(["--end-of-options", request.name.as_str()]);
        if let Some(target) = &request.target {
            cmd = cmd.arg(target);
        }
        self.runner.run(cmd).map(drop)
    }

    fn delete_tag(&self, repo: &RepoPath, name: &str) -> Result<(), GitError> {
        self.write(repo, &["tag", "-d", "--end-of-options", name])
            .map(drop)
    }
}

impl RefWriter for GitCli {
    fn update_ref(
        &self,
        repo: &RepoPath,
        name: &str,
        new: Option<Oid>,
        expected_old: Option<Oid>,
    ) -> Result<(), GitError> {
        let old = expected_old.map(|o| o.to_hex());
        let mut args = match new {
            Some(_) => vec!["update-ref", "-m", "archipel: undo", name],
            None => vec!["update-ref", "-d", name],
        };
        let new_hex = new.map(|o| o.to_hex());
        args.extend(new_hex.as_deref());
        args.extend(old.as_deref());
        self.write(repo, &args).map(drop)
    }

    fn set_symbolic_head(&self, repo: &RepoPath, branch_ref: &str) -> Result<(), GitError> {
        self.write(
            repo,
            &["symbolic-ref", "--end-of-options", "HEAD", branch_ref],
        )
        .map(drop)
    }

    fn reset(&self, repo: &RepoPath, target: &str, mode: ResetMode) -> Result<(), GitError> {
        let flag = match mode {
            ResetMode::Soft => "--soft",
            ResetMode::Mixed => "--mixed",
            ResetMode::Hard => "--hard",
            ResetMode::Keep => "--keep",
        };
        self.write(
            repo,
            &["reset", "-q", flag, "--end-of-options", target, "--"],
        )
        .map(drop)
    }
}

impl StashWriter for GitCli {
    fn stash_push(&self, repo: &RepoPath, request: &StashRequest) -> Result<bool, GitError> {
        let before = self.resolve(repo, "refs/stash")?;
        let mut args = vec!["stash", "push", "-q"];
        if request.untracked == UntrackedMode::Include {
            args.push("--include-untracked");
        }
        if let Some(m) = &request.message {
            args.extend(["-m", m]);
        }
        self.write(repo, &args)?;
        Ok(self.resolve(repo, "refs/stash")? != before)
    }

    fn stash_apply(
        &self,
        repo: &RepoPath,
        index: u32,
        mode: StashApplyMode,
    ) -> Result<(), GitError> {
        let verb = if mode == StashApplyMode::Pop {
            "pop"
        } else {
            "apply"
        };
        self.write(repo, &["stash", verb, "-q", &format!("stash@{{{index}}}")])
            .map(drop)
    }

    fn stash_apply_index(&self, repo: &RepoPath, index: u32) -> Result<(), GitError> {
        self.write(
            repo,
            &[
                "stash",
                "apply",
                "-q",
                "--index",
                &format!("stash@{{{index}}}"),
            ],
        )
        .map(drop)
    }

    fn drop_stash(&self, repo: &RepoPath, index: u32) -> Result<(), GitError> {
        self.write(
            repo,
            &["stash", "drop", "-q", &format!("stash@{{{index}}}")],
        )
        .map(drop)
    }

    fn stash_store(&self, repo: &RepoPath, oid: Oid, message: &str) -> Result<(), GitError> {
        self.write(repo, &["stash", "store", "-m", message, &oid.to_hex()])
            .map(drop)
    }
}

impl WorktreeSnapshots for GitCli {
    fn snapshot_worktree(&self, repo: &RepoPath) -> Result<Option<Oid>, GitError> {
        let text = self.write(repo, &["stash", "create"])?;
        if text.is_empty() {
            Ok(None)
        } else {
            Oid::from_hex(&text).map(Some).map_err(|_| GitError::Parse {
                what: "git stash create",
                detail: text,
            })
        }
    }

    fn snapshot_paths(&self, repo: &RepoPath, paths: &[String]) -> Result<Option<Oid>, GitError> {
        if paths.is_empty() {
            return Ok(None);
        }
        let dirs = GitDirs::locate(repo.as_path())?;
        let index = dirs
            .git_dir
            .join(format!("archipel-snapshot-{}.index", std::process::id()));
        let dir = repo.as_path();
        // A private index seeded with `HEAD`, so the real one is never touched.
        let with_index =
            |args: &[&str]| Cmd::new(dir, CmdKind::Write, args).env("GIT_INDEX_FILE", &index);
        let head = self.resolve(repo, "HEAD")?;
        let tree = |cmd: Cmd<'_>| -> Result<String, GitError> {
            self.runner.run(cmd)?;
            Ok(self.runner.run(with_index(&["write-tree"]))?.stdout_text())
        };
        let result = (|| {
            let base = tree(match head {
                Some(_) => with_index(&["read-tree", "HEAD"]),
                None => with_index(&["read-tree", "--empty"]),
            })?;
            let snap = tree(
                with_index(&["add", "-A", "--pathspec-from-file=-", "--pathspec-file-nul"])
                    .stdin(pathspec_stdin(paths)),
            )?;
            // Internal objects: a fixed identity, never signed.
            let commit = |tree: &str, parent: Option<&str>, message: &str| {
                let mut cmd = Cmd::new(
                    dir,
                    CmdKind::Write,
                    &["commit-tree", "--no-gpg-sign", tree, "-m", message],
                );
                if let Some(p) = parent {
                    cmd = cmd.args(["-p", p]);
                }
                for (key, value) in [
                    ("GIT_AUTHOR_NAME", "Archipel"),
                    ("GIT_AUTHOR_EMAIL", "archipel@localhost"),
                    ("GIT_COMMITTER_NAME", "Archipel"),
                    ("GIT_COMMITTER_EMAIL", "archipel@localhost"),
                ] {
                    cmd = cmd.env(key, value);
                }
                Ok::<_, GitError>(self.runner.run(cmd)?.stdout_text())
            };
            let parent = commit(&base, None, "archipel: before discard")?;
            commit(&snap, Some(&parent), "archipel: discarded worktree changes")
        })();
        let _ = std::fs::remove_file(&index);
        let text = result?;
        Oid::from_hex(&text).map(Some).map_err(|_| GitError::Parse {
            what: "git commit-tree",
            detail: text,
        })
    }

    fn restore_paths(&self, repo: &RepoPath, snapshot: Oid) -> Result<(), GitError> {
        let paths = self.snapshot_changes(repo, snapshot)?;
        if paths.is_empty() {
            return Ok(());
        }
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
            .args(paths.iter().map(String::as_str)),
        )?;
        // `XY path`: a worktree-side change (Y) means it was edited since.
        let changed: Vec<String> = nul_separated(&status.stdout)
            .into_iter()
            .filter(|e| e.as_bytes().get(1).is_some_and(|y| *y != b' '))
            .filter_map(|e| e.get(3..).map(str::to_owned))
            .collect();
        if !changed.is_empty() {
            return Err(GitError::InvalidRequest(format!(
                "{} changed since the discard: restoring would overwrite it",
                changed.join(", ")
            )));
        }
        self.runner.run(
            Cmd::new(
                repo.as_path(),
                CmdKind::Write,
                &[
                    "restore",
                    "--worktree",
                    "--pathspec-from-file=-",
                    "--pathspec-file-nul",
                ],
            )
            .arg(format!("--source={}", snapshot.to_hex()))
            .stdin(pathspec_stdin(&paths)),
        )?;
        // Submodules: `restore` skips them, check their commit out directly.
        for (path, oid) in self.gitlinks(repo, Some(&snapshot.to_hex()), &paths)? {
            let dir = repo.as_path().join(&path);
            self.runner.run(Cmd::new(
                &dir,
                CmdKind::Write,
                &["checkout", "-q", "--detach", &oid],
            ))?;
        }
        Ok(())
    }
}

impl GitCli {
    /// The paths a [`WorktreeSnapshots::snapshot_paths`] snapshot holds.
    fn snapshot_changes(&self, repo: &RepoPath, snapshot: Oid) -> Result<Vec<String>, GitError> {
        let hex = snapshot.to_hex();
        let out = self.runner.run(Cmd::new(
            repo.as_path(),
            CmdKind::Read,
            &[
                "diff",
                "--name-only",
                "-z",
                "--no-renames",
                &format!("{hex}^"),
                &hex,
            ],
        ))?;
        Ok(nul_separated(&out.stdout))
    }
}

/// The non-empty records of NUL-separated command output.
fn nul_separated(bytes: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(bytes)
        .split('\0')
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Whether `git fetch` (without `--quiet`) reported a ref update: update
/// lines are the indented ones (` * [new branch]`, `   a..b  main -> …`,
/// ` - [deleted]`), whatever the locale; `From …`, `Fetching …` and
/// warnings are not.
fn fetch_updated_refs(stderr: &str) -> bool {
    stderr.lines().any(|l| l.starts_with(' '))
}

/// Paths as a NUL-separated pathspec file (`--pathspec-from-file=-`).
fn pathspec_stdin(paths: &[String]) -> Vec<u8> {
    paths.iter().flat_map(|p| p.bytes().chain([0])).collect()
}

impl RemoteOps for GitCli {
    fn fetch(
        &self,
        repo: &RepoPath,
        request: &FetchRequest,
        progress: &dyn ProgressSink,
        cancel: &CancelToken,
    ) -> Result<(), GitError> {
        let mut cmd = Cmd::new(repo.as_path(), CmdKind::Network, &["fetch", "--progress"])
            .cancel(cancel)
            .progress(progress);
        if request.prune {
            cmd = cmd.arg("--prune");
        }
        cmd = match &request.remote {
            Some(remote) => cmd.args(["--end-of-options", remote]),
            None => cmd.arg("--all"),
        };
        self.runner.run(cmd).map(drop)
    }

    fn fetch_background(&self, repo: &RepoPath, cancel: &CancelToken) -> Result<bool, GitError> {
        let cmd = Cmd::new(
            repo.as_path(),
            CmdKind::Network,
            &["fetch", "--all", "--no-auto-maintenance"],
        )
        .cancel(cancel);
        let out = self.runner.run(cmd)?;
        Ok(fetch_updated_refs(&out.stderr))
    }

    fn pull(
        &self,
        repo: &RepoPath,
        mode: PullMode,
        progress: &dyn ProgressSink,
        cancel: &CancelToken,
    ) -> Result<(), GitError> {
        let flag = match mode {
            PullMode::FastForwardOnly => "--ff-only",
            PullMode::Rebase => "--rebase",
            PullMode::Merge => "--no-rebase",
        };
        let cmd = Cmd::new(
            repo.as_path(),
            CmdKind::Network,
            &["pull", "--progress", flag],
        )
        .cancel(cancel)
        .progress(progress);
        self.runner.run(cmd).map(drop)
    }

    fn push(
        &self,
        repo: &RepoPath,
        request: &PushRequest,
        progress: &dyn ProgressSink,
        cancel: &CancelToken,
    ) -> Result<(), GitError> {
        let mut cmd = Cmd::new(repo.as_path(), CmdKind::Network, &["push", "--progress"])
            .cancel(cancel)
            .progress(progress);
        if request.set_upstream {
            cmd = cmd.arg("--set-upstream");
        }
        if request.tags {
            cmd = cmd.arg("--tags");
        }
        if let ForceMode::WithLease(expected) = &request.force {
            cmd = match (expected, &request.refspec) {
                (Some(oid), Some(refspec)) => {
                    let dst = refspec.rsplit(':').next().unwrap_or(refspec);
                    cmd.arg(format!("--force-with-lease={dst}:{oid}"))
                }
                _ => cmd.arg("--force-with-lease"),
            };
        }
        // Without remote nor refspec, git picks the push remote and branch
        // from the configuration (`pushRemote`, `pushDefault`, upstream).
        // A refspec needs a remote in front of it.
        match (&request.remote, &request.refspec) {
            (None, None) => {}
            (remote, refspec) => {
                cmd = cmd
                    .args(["--end-of-options", remote.as_deref().unwrap_or("origin")])
                    .args(refspec);
            }
        }
        self.runner.run(cmd).map(drop)
    }

    fn clone_repo(
        &self,
        url: &str,
        dest: &Path,
        progress: &dyn ProgressSink,
        cancel: &CancelToken,
    ) -> Result<RepoPath, GitError> {
        let parent = dest.parent().unwrap_or(Path::new("."));
        std::fs::create_dir_all(parent).map_err(|e| GitError::Io {
            path: parent.to_path_buf(),
            message: e.to_string(),
        })?;
        let cmd = Cmd::new(
            parent,
            CmdKind::Network,
            &["clone", "--progress", "--end-of-options", url],
        )
        .arg(dest.as_os_str())
        .cancel(cancel)
        .progress(progress);
        self.runner.run(cmd)?;
        Ok(RepoPath::new(dest))
    }
}

impl HistoryRewriter for GitCli {
    fn merge(&self, repo: &RepoPath, rev: &str, mode: MergeMode) -> Result<(), GitError> {
        let mut args = vec!["merge", "--no-edit"];
        match mode {
            MergeMode::Auto => {}
            MergeMode::FastForwardOnly => args.push("--ff-only"),
            MergeMode::NoFastForward => args.push("--no-ff"),
        }
        args.extend(["--end-of-options", rev]);
        self.write(repo, &args).map(drop)
    }

    fn rebase(&self, repo: &RepoPath, onto: &str) -> Result<(), GitError> {
        self.write(repo, &["rebase", "--end-of-options", onto])
            .map(drop)
    }

    fn cherry_pick(&self, repo: &RepoPath, commits: &[Oid]) -> Result<(), GitError> {
        let cmd = Cmd::new(repo.as_path(), CmdKind::Write, &["cherry-pick"])
            .args(commits.iter().map(Oid::to_hex));
        self.runner.run(cmd).map(drop)
    }

    fn revert(&self, repo: &RepoPath, commit: Oid) -> Result<(), GitError> {
        self.write(repo, &["revert", "--no-edit", &commit.to_hex()])
            .map(drop)
    }

    fn squash_preview(
        &self,
        repo: &RepoPath,
        oldest: Oid,
        newest: Oid,
    ) -> Result<archipel_model::history::SquashPreview, GitError> {
        self.squash_preview_impl(repo, oldest, newest)
    }

    fn squash(
        &self,
        repo: &RepoPath,
        oldest: Oid,
        newest: Oid,
        message: &str,
    ) -> Result<Oid, GitError> {
        self.squash_impl(repo, oldest, newest, message)
    }

    fn reword(&self, repo: &RepoPath, commit: Oid, message: &str) -> Result<Oid, GitError> {
        self.reword_impl(repo, commit, message)
    }

    fn sequence(&self, repo: &RepoPath, action: SequenceAction) -> Result<(), GitError> {
        let op = GitDirs::locate(repo.as_path())?
            .in_progress()
            .ok_or_else(|| GitError::InvalidRequest("no operation in progress".into()))?;
        let verb = match op {
            RepoOp::Merge => "merge",
            RepoOp::Rebase => "rebase",
            RepoOp::CherryPick => "cherry-pick",
            RepoOp::Revert => "revert",
            RepoOp::ApplyMailbox => "am",
            RepoOp::Bisect => return Err(GitError::Unsupported("continue/abort a bisect")),
        };
        let flag = match action {
            SequenceAction::Continue => "--continue",
            SequenceAction::Abort => "--abort",
            SequenceAction::Skip if op == RepoOp::Merge => {
                return Err(GitError::InvalidRequest("a merge cannot be skipped".into()));
            }
            SequenceAction::Skip => "--skip",
        };
        self.write(repo, &[verb, flag]).map(drop)
    }
}

impl MergeToolLauncher for GitCli {
    fn launch(
        &self,
        repo: &RepoPath,
        path: Option<&str>,
        tool: Option<&str>,
    ) -> Result<(), GitError> {
        // The user works in the tool for as long as they need.
        let mut cmd = Cmd::new(
            repo.as_path(),
            CmdKind::Interactive,
            &["mergetool", "--no-prompt"],
        );
        if let Some(tool) = tool {
            cmd = cmd.arg(format!("--tool={tool}"));
        }
        if let Some(path) = path {
            cmd = cmd.arg("--").arg(path);
        }
        self.runner.run(cmd).map(drop)
    }
}
