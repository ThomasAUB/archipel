//! Submodule reads and updates through the CLI (spec §9).

use std::collections::HashMap;

use archipel_model::ports::{SubmoduleReader, SubmoduleWriter};
use archipel_model::requests::SubmoduleUpdate;
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
}
