//! Submodule reads and updates through the CLI (spec §9).

use std::collections::HashMap;

use archipel_model::ports::{SubmoduleReader, SubmoduleWriter};
use archipel_model::requests::{SubmoduleAdd, SubmoduleUpdate};
use archipel_model::submodule::SubmoduleInfo;
use archipel_model::{CancelToken, GitError, ProgressSink, RepoPath};

use super::GitCli;
use super::runner::{Cmd, CmdKind};
use crate::gitdir::has_submodules;
use crate::parse::submodule::{SubmoduleFlag, parse_submodule_status};

impl SubmoduleReader for GitCli {
    fn submodules(&self, repo: &RepoPath) -> Result<Vec<SubmoduleInfo>, GitError> {
        // Fast path: no `.gitmodules`, no process spawned.
        if !has_submodules(repo.as_path()) {
            return Ok(Vec::new());
        }
        let status = |cached: bool| -> Result<_, GitError> {
            let mut args = vec!["submodule", "status"];
            if cached {
                args.push("--cached");
            }
            let out = self
                .runner
                .run(Cmd::new(repo.as_path(), CmdKind::Read, &args))?;
            parse_submodule_status(&out.stdout_text())
        };
        let lines = status(false)?;
        // `+` lines print the checked-out commit: the pinned one needs `--cached`.
        let pinned: HashMap<String, _> = if lines.iter().any(|l| l.flag == SubmoduleFlag::OutOfSync)
        {
            status(true)?.into_iter().map(|l| (l.path, l.oid)).collect()
        } else {
            HashMap::new()
        };
        let mut out: Vec<SubmoduleInfo> = lines
            .into_iter()
            .map(|l| {
                let initialized = l.flag != SubmoduleFlag::Uninitialized;
                let (pinned, checked_out) = match l.flag {
                    SubmoduleFlag::InSync => (l.oid, l.oid),
                    SubmoduleFlag::Uninitialized => (l.oid, None),
                    SubmoduleFlag::OutOfSync => (pinned.get(&l.path).copied().flatten(), l.oid),
                    SubmoduleFlag::Conflict => (None, None),
                };
                // Git prints `/`-separated paths: join component-wise so the
                // path uses the platform separator.
                let mut path = repo.as_path().to_path_buf();
                path.extend(l.path.split('/'));
                SubmoduleInfo {
                    path: RepoPath::new(path),
                    name: l.path,
                    initialized,
                    pinned,
                    checked_out,
                }
            })
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }
}

impl SubmoduleWriter for GitCli {
    fn update_submodules(
        &self,
        repo: &RepoPath,
        request: &SubmoduleUpdate,
        progress: &dyn ProgressSink,
        cancel: &CancelToken,
    ) -> Result<(), GitError> {
        let mut cmd = Cmd::new(
            repo.as_path(),
            CmdKind::Network,
            &["submodule", "update", "--progress"],
        )
        .cancel(cancel)
        .progress(progress);
        if request.init {
            cmd = cmd.arg("--init");
        }
        if request.recursive {
            cmd = cmd.arg("--recursive");
        }
        if !request.paths.is_empty() {
            cmd = cmd.arg("--").args(request.paths.iter().map(String::as_str));
        }
        self.runner.run(cmd).map(drop)
    }

    fn add_submodule(
        &self,
        repo: &RepoPath,
        request: &SubmoduleAdd,
        progress: &dyn ProgressSink,
        cancel: &CancelToken,
    ) -> Result<(), GitError> {
        // The URL is the one the user typed: a local repository is allowed
        // (Git refuses the file transport for submodules by default, against
        // URLs planted in a cloned `.gitmodules`; this clone does not recurse).
        let cmd = Cmd::new(
            repo.as_path(),
            CmdKind::Network,
            &[
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "add",
                "--progress",
                "--",
            ],
        )
        .arg(&request.url)
        .arg(&request.path)
        .cancel(cancel)
        .progress(progress);
        self.runner.run(cmd).map(drop)
    }

    fn remove_submodule(
        &self,
        repo: &RepoPath,
        path: &str,
        cancel: &CancelToken,
    ) -> Result<(), GitError> {
        // Its repository under `$GIT_DIR/modules` is named after the
        // submodule's name, which `git rm` drops from `.gitmodules`: look it
        // up first.
        let module_dir = match self.submodule_name(repo, path)? {
            Some(name) => {
                let out = self.runner.run(
                    Cmd::new(repo.as_path(), CmdKind::Read, &["rev-parse", "--git-path"])
                        .arg(format!("modules/{name}")),
                )?;
                Some(repo.as_path().join(out.stdout_text().trim()))
            }
            None => None,
        };
        // Without `--force`: Git refuses when the submodule has local changes.
        self.runner.run(
            Cmd::new(
                repo.as_path(),
                CmdKind::Write,
                &["submodule", "deinit", "--"],
            )
            .arg(path)
            .cancel(cancel),
        )?;
        self.runner.run(
            Cmd::new(repo.as_path(), CmdKind::Write, &["rm", "-q", "-f", "--"])
                .arg(path)
                .cancel(cancel),
        )?;
        if let Some(dir) = module_dir.filter(|d| d.is_dir()) {
            std::fs::remove_dir_all(&dir).map_err(|e| GitError::Io {
                path: dir.clone(),
                message: e.to_string(),
            })?;
        }
        Ok(())
    }
}

impl GitCli {
    /// The name, in `.gitmodules`, of the submodule at `path`. `None` when
    /// none is declared there, or its name could step out of
    /// `$GIT_DIR/modules` (Git refuses such names too).
    fn submodule_name(&self, repo: &RepoPath, path: &str) -> Result<Option<String>, GitError> {
        let out = self.runner.run(
            Cmd::new(
                repo.as_path(),
                CmdKind::Read,
                &[
                    "config",
                    "-z",
                    "-f",
                    ".gitmodules",
                    "--get-regexp",
                    r"^submodule\..*\.path$",
                ],
            )
            // 1: no match (or no `.gitmodules`).
            .ok_codes(&[0, 1]),
        )?;
        Ok(out
            .stdout_text()
            .split('\0')
            .filter_map(|entry| entry.split_once('\n'))
            .find(|(_, value)| value.trim_end_matches('/') == path)
            .and_then(|(key, _)| key.strip_prefix("submodule.")?.strip_suffix(".path"))
            .filter(|name| {
                !name.is_empty() && !name.split(['/', '\\']).any(|c| c.is_empty() || c == "..")
            })
            .map(str::to_owned))
    }
}
