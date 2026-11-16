//! Worktree list and writes through the CLI (`git worktree …`).

use std::path::Path;

use archipel_model::ports::{WorktreeReader, WorktreeWriter};
use archipel_model::worktree::{WorktreeAddRequest, WorktreeInfo, WorktreeTarget};
use archipel_model::{GitError, RepoPath};

use super::GitCli;
use super::runner::{Cmd, CmdKind};
use crate::parse::worktree::parse_worktree_list;

impl WorktreeReader for GitCli {
    fn worktrees(&self, repo: &RepoPath) -> Result<Vec<WorktreeInfo>, GitError> {
        let out = self.runner.run(Cmd::new(
            repo.as_path(),
            CmdKind::Read,
            &["worktree", "list", "--porcelain", "-z"],
        ))?;
        parse_worktree_list(&out.stdout)
    }
}

impl GitCli {
    fn worktree_cmd(
        &self,
        repo: &RepoPath,
        args: &[&str],
        paths: &[&Path],
    ) -> Result<(), GitError> {
        let cmd = Cmd::new(repo.as_path(), CmdKind::Write, args)
            .args(paths.iter().map(|p| p.as_os_str()));
        self.runner.run(cmd).map(drop)
    }
}

impl WorktreeWriter for GitCli {
    fn add_worktree(&self, repo: &RepoPath, request: &WorktreeAddRequest) -> Result<(), GitError> {
        let path = request.path.as_os_str();
        let cmd = Cmd::new(repo.as_path(), CmdKind::Write, &["worktree", "add"]);
        let cmd = match &request.target {
            WorktreeTarget::Branch(b) => cmd.arg("--").arg(path).arg(b),
            WorktreeTarget::TrackRemote(remote) => {
                let local = request.target.branch().unwrap_or(remote);
                cmd.arg("--track")
                    .arg(format!("-b{local}"))
                    .arg("--")
                    .arg(path)
                    .arg(remote)
            }
            WorktreeTarget::NewBranch { name, base } => {
                let cmd = cmd.arg(format!("-b{name}")).arg("--").arg(path);
                match base {
                    Some(b) => cmd.arg(b),
                    None => cmd,
                }
            }
            WorktreeTarget::Detached(rev) => cmd.arg("--detach").arg("--").arg(path).arg(rev),
        };
        self.runner.run(cmd).map(drop)
    }

    fn remove_worktree(&self, repo: &RepoPath, path: &Path, force: bool) -> Result<(), GitError> {
        // `--force` twice also removes a locked worktree.
        let args: &[&str] = if force {
            &["worktree", "remove", "--force", "--force", "--"]
        } else {
            &["worktree", "remove", "--"]
        };
        self.worktree_cmd(repo, args, &[path])
    }

    fn lock_worktree(
        &self,
        repo: &RepoPath,
        path: &Path,
        reason: Option<&str>,
    ) -> Result<(), GitError> {
        match reason {
            Some(reason) => {
                let reason = format!("--reason={reason}");
                self.worktree_cmd(repo, &["worktree", "lock", &reason, "--"], &[path])
            }
            None => self.worktree_cmd(repo, &["worktree", "unlock", "--"], &[path]),
        }
    }

    fn prune_worktrees(&self, repo: &RepoPath) -> Result<(), GitError> {
        self.worktree_cmd(repo, &["worktree", "prune"], &[])
    }

    fn move_worktree(&self, repo: &RepoPath, from: &Path, to: &Path) -> Result<(), GitError> {
        self.worktree_cmd(repo, &["worktree", "move", "--"], &[from, to])
    }
}
