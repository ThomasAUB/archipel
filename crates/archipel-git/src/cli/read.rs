//! Reader traits implemented with the CLI (also the fallback for `gix`).

use std::ops::ControlFlow;
use std::thread;

use archipel_model::changes::FileChange;
use archipel_model::diff::FileDiff;
use archipel_model::history::{CommitMeta, TopologyNode, WalkOrder};
use archipel_model::journal::{HeadRef, RefSnapshot};
use archipel_model::ports::{
    ChurnReader, CommitChurn, DiffReader, DiffSide, HistoryReader, RefReader, RemoteInfo,
    RemoteReader, StashReader, StatusReader,
};
use archipel_model::refs::{BranchInfo, RefKind, RefTip, StashEntry, TagInfo};
use archipel_model::{
    CancelToken, GitError, HeadState, Oid, RepoPath, RepoSummary, Timestamp, Upstream,
};

use super::GitCli;
use super::runner::{Cmd, CmdKind};
use crate::gitdir::{GitDirs, has_submodules};
use crate::parse::diff::parse_unified_diff;
use crate::parse::numstat::{CHURN_FORMAT, parse_churn_record};
use crate::parse::refs::{
    BRANCH_FORMAT, TAG_FORMAT, TIP_FORMAT, parse_branches, parse_tags, parse_tips,
};
use crate::parse::status::{StatusReport, parse_status};

const META_FORMAT: &str = "--format=%H%x1f%s%x1f%an%x1f%ae%x1f%at%x1f%ct";

impl GitCli {
    fn read(&self, repo: &RepoPath, args: &[&str]) -> Result<String, GitError> {
        Ok(self
            .runner
            .run(Cmd::new(repo.as_path(), CmdKind::Read, args))?
            .stdout_text())
    }

    fn status_report(
        &self,
        repo: &RepoPath,
        untracked_all: bool,
    ) -> Result<StatusReport, GitError> {
        let untracked = if untracked_all {
            "--untracked-files=all"
        } else {
            "--untracked-files=normal"
        };
        let out = self.runner.run(Cmd::new(
            repo.as_path(),
            CmdKind::Read,
            &[
                "status",
                "--porcelain=v2",
                "--branch",
                "--show-stash",
                "-z",
                untracked,
            ],
        ))?;
        parse_status(&out.stdout)
    }

    /// `git describe --tags`; `None` when there is no tag (or no commit).
    pub(crate) fn describe(&self, repo: &RepoPath) -> Result<Option<String>, GitError> {
        let out = self.runner.run(
            Cmd::new(repo.as_path(), CmdKind::Read, &["describe", "--tags"]).ok_codes(&[0, 128]),
        )?;
        let text = out.stdout_text();
        Ok((out.code == Some(0) && !text.is_empty()).then_some(text))
    }
}

fn oid(text: &str, what: &'static str) -> Result<Oid, GitError> {
    Oid::from_hex(text).map_err(|_| GitError::Parse {
        what,
        detail: text.to_owned(),
    })
}

impl StatusReader for GitCli {
    fn summary(&self, repo: &RepoPath) -> Result<RepoSummary, GitError> {
        let dirs = GitDirs::locate(repo.as_path())?;
        // `describe` and `status` are independent: run them concurrently.
        let (status, describe) = thread::scope(|s| {
            let describe = s.spawn(|| self.describe(repo));
            let status = self.status_report(repo, false);
            (status, describe.join())
        });
        let status = status?;
        let describe =
            describe.map_err(|_| GitError::Backend("describe thread panicked".into()))??;
        let head = match (&status.branch, status.head_oid) {
            (Some(b), Some(_)) => HeadState::Branch(b.clone()),
            (Some(b), None) => HeadState::Unborn(b.clone()),
            (None, Some(o)) => HeadState::Detached(o),
            (None, None) => {
                return Err(GitError::Parse {
                    what: "git status",
                    detail: "no HEAD".into(),
                });
            }
        };
        let upstream = status.upstream.as_ref().map(|name| {
            let (ahead, behind) = status.ahead_behind.unwrap_or((0, 0));
            Upstream {
                name: name.clone(),
                ahead,
                behind,
                gone: status.ahead_behind.is_none(),
            }
        });
        Ok(RepoSummary {
            head,
            head_oid: status.head_oid,
            upstream,
            changes: status.counts(),
            stash_count: status.stash_count,
            describe,
            last_fetch: dirs.last_fetch(),
            in_progress: dirs.in_progress(),
            has_submodules: has_submodules(repo.as_path()),
            computed_at: Timestamp::now(),
        })
    }

    fn changes(&self, repo: &RepoPath) -> Result<Vec<FileChange>, GitError> {
        Ok(self.status_report(repo, true)?.changes)
    }
}

impl RefReader for GitCli {
    fn branches(&self, repo: &RepoPath) -> Result<Vec<BranchInfo>, GitError> {
        let format = format!("--format={BRANCH_FORMAT}");
        parse_branches(&self.read(
            repo,
            &["for-each-ref", &format, "refs/heads", "refs/remotes"],
        )?)
    }

    fn tags(&self, repo: &RepoPath) -> Result<Vec<TagInfo>, GitError> {
        let format = format!("--format={TAG_FORMAT}");
        parse_tags(&self.read(repo, &["for-each-ref", &format, "refs/tags"])?)
    }

    fn ref_tips(&self, repo: &RepoPath) -> Result<Vec<RefTip>, GitError> {
        let format = format!("--format={TIP_FORMAT}");
        let mut tips = parse_tips(&self.read(repo, &["for-each-ref", &format])?)?;
        if let Some(head) = self.resolve(repo, "HEAD")? {
            tips.insert(
                0,
                RefTip {
                    name: "HEAD".into(),
                    kind: RefKind::Head,
                    target: head,
                },
            );
        }
        Ok(tips)
    }

    fn resolve(&self, repo: &RepoPath, rev: &str) -> Result<Option<Oid>, GitError> {
        let spec = format!("{rev}^{{commit}}");
        let out = self.runner.run(
            Cmd::new(
                repo.as_path(),
                CmdKind::Read,
                &["rev-parse", "-q", "--verify", "--end-of-options", &spec],
            )
            .ok_codes(&[0, 1, 128]),
        )?;
        let text = out.stdout_text();
        if out.code == Some(0) && !text.is_empty() {
            oid(&text, "git rev-parse").map(Some)
        } else {
            Ok(None)
        }
    }

    fn snapshot(&self, repo: &RepoPath, refs: &[String]) -> Result<RefSnapshot, GitError> {
        let sym = self.runner.run(
            Cmd::new(
                repo.as_path(),
                CmdKind::Read,
                &["symbolic-ref", "-q", "HEAD"],
            )
            .ok_codes(&[0, 1]),
        )?;
        let head = if sym.code == Some(0) {
            HeadRef::Symbolic(sym.stdout_text())
        } else {
            let target = self
                .resolve(repo, "HEAD")?
                .ok_or_else(|| GitError::InvalidRequest("HEAD is invalid".into()))?;
            HeadRef::Detached(target)
        };
        let mut values = Vec::with_capacity(refs.len());
        if !refs.is_empty() {
            let out = self.runner.run(
                Cmd::new(
                    repo.as_path(),
                    CmdKind::Read,
                    &["for-each-ref", "--format=%(refname)%00%(objectname)"],
                )
                .args(refs.iter().map(String::as_str)),
            )?;
            let text = out.stdout_text();
            let found: Vec<(&str, &str)> =
                text.lines().filter_map(|l| l.split_once('\0')).collect();
            for name in refs {
                let target = found
                    .iter()
                    .find(|(n, _)| n == name)
                    .map(|(_, o)| oid(o, "for-each-ref"))
                    .transpose()?;
                values.push((name.clone(), target));
            }
        }
        Ok(RefSnapshot { head, refs: values })
    }
}

impl RemoteReader for GitCli {
    fn remotes(&self, repo: &RepoPath) -> Result<Vec<RemoteInfo>, GitError> {
        let out = self.runner.run(
            Cmd::new(
                repo.as_path(),
                CmdKind::Read,
                &["config", "--get-regexp", r"^remote\..*\.url$"],
            )
            .ok_codes(&[0, 1]),
        )?;
        Ok(out
            .stdout_text()
            .lines()
            .filter_map(|l| {
                let (key, url) = l.split_once(' ')?;
                let name = key.strip_prefix("remote.")?.strip_suffix(".url")?;
                Some(RemoteInfo {
                    name: name.to_owned(),
                    url: url.to_owned(),
                })
            })
            .collect())
    }
}

impl HistoryReader for GitCli {
    fn walk_topology(
        &self,
        repo: &RepoPath,
        order: WalkOrder,
        sink: &mut dyn FnMut(TopologyNode) -> ControlFlow<()>,
        cancel: &CancelToken,
    ) -> Result<(), GitError> {
        if self.resolve(repo, "HEAD")?.is_none()
            && self.read(repo, &["for-each-ref", "--count=1"])?.is_empty()
        {
            return Ok(()); // empty repository
        }
        let order = match order {
            WalkOrder::Topological => "--topo-order",
            WalkOrder::Date => "--date-order",
        };
        let cmd = Cmd::new(
            repo.as_path(),
            CmdKind::Read,
            &[
                "rev-list",
                "--exclude=refs/stash",
                "--all",
                "--parents",
                order,
            ],
        )
        .cancel(cancel);
        self.runner.stream(cmd, b'\n', &mut |line| {
            let text = std::str::from_utf8(line).map_err(|e| GitError::Backend(e.to_string()))?;
            let mut ids = text.split(' ').filter(|s| !s.is_empty());
            let Some(first) = ids.next() else {
                return Ok(ControlFlow::Continue(()));
            };
            let node = TopologyNode {
                oid: oid(first, "git rev-list")?,
                parents: ids
                    .map(|p| oid(p, "git rev-list"))
                    .collect::<Result<_, _>>()?,
            };
            Ok(sink(node))
        })
    }

    fn commit_meta(&self, repo: &RepoPath, oids: &[Oid]) -> Result<Vec<CommitMeta>, GitError> {
        if oids.is_empty() {
            return Ok(Vec::new());
        }
        let mut input = String::with_capacity(oids.len() * 65);
        for o in oids {
            input.push_str(&o.to_hex());
            input.push('\n');
        }
        let out = self.runner.run(
            Cmd::new(
                repo.as_path(),
                CmdKind::Read,
                &["log", "--no-walk=unsorted", "--stdin", META_FORMAT],
            )
            .stdin(input),
        )?;
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|l| !l.is_empty())
            .map(parse_meta)
            .collect()
    }

    fn is_ancestor(
        &self,
        repo: &RepoPath,
        ancestor: Oid,
        descendant: Oid,
    ) -> Result<bool, GitError> {
        let out = self.runner.run(
            Cmd::new(
                repo.as_path(),
                CmdKind::Read,
                &["merge-base", "--is-ancestor"],
            )
            .arg(ancestor.to_hex())
            .arg(descendant.to_hex())
            .ok_codes(&[0, 1]),
        )?;
        Ok(out.code == Some(0))
    }
}

fn parse_meta(line: &str) -> Result<CommitMeta, GitError> {
    let f: Vec<&str> = line.split('\x1f').collect();
    let [hash, summary, name, email, at, ct] = f[..] else {
        return Err(GitError::Parse {
            what: "git log meta",
            detail: line.to_owned(),
        });
    };
    Ok(CommitMeta {
        oid: oid(hash, "git log")?,
        summary: summary.to_owned(),
        author_name: name.to_owned(),
        author_email: email.to_owned(),
        author_time: Timestamp(at.parse().unwrap_or(0)),
        commit_time: Timestamp(ct.parse().unwrap_or(0)),
    })
}

impl ChurnReader for GitCli {
    fn walk_churn(
        &self,
        repo: &RepoPath,
        since: Option<Oid>,
        sink: &mut dyn FnMut(CommitChurn) -> ControlFlow<()>,
        cancel: &CancelToken,
    ) -> Result<(), GitError> {
        if self.resolve(repo, "HEAD")?.is_none() {
            return Ok(());
        }
        let mut cmd = Cmd::new(
            repo.as_path(),
            CmdKind::Read,
            &[
                "log",
                "-z",
                "--numstat",
                "--no-renames",
                CHURN_FORMAT,
                "HEAD",
            ],
        )
        .cancel(cancel);
        if let Some(since) = since {
            cmd = cmd.arg(format!("^{since}"));
        }
        self.runner.stream(cmd, 0x1e, &mut |record| {
            if record.is_empty() {
                return Ok(ControlFlow::Continue(()));
            }
            let churn = parse_churn_record(&String::from_utf8_lossy(record))?;
            Ok(sink(churn))
        })
    }
}

const DIFF_ARGS: [&str; 5] = [
    "-M",
    "--no-ext-diff",
    "--src-prefix=a/",
    "--dst-prefix=b/",
    "--no-color",
];

impl DiffReader for GitCli {
    fn commit_diff(&self, repo: &RepoPath, commit: Oid) -> Result<Vec<FileDiff>, GitError> {
        let out = self.runner.run(
            Cmd::new(
                repo.as_path(),
                CmdKind::Read,
                &["show", "--format=", "--diff-merges=first-parent"],
            )
            .args(DIFF_ARGS)
            .arg(commit.to_hex()),
        )?;
        Ok(parse_unified_diff(
            &String::from_utf8_lossy(&out.stdout),
            self.max_diff_bytes,
        ))
    }

    fn worktree_diff(
        &self,
        repo: &RepoPath,
        side: DiffSide,
        path: Option<&str>,
    ) -> Result<Vec<FileDiff>, GitError> {
        let mut cmd = Cmd::new(repo.as_path(), CmdKind::Read, &["diff"]).args(DIFF_ARGS);
        if side == DiffSide::Staged {
            cmd = cmd.arg("--cached");
        }
        if let Some(path) = path {
            cmd = cmd.arg("--").arg(path);
        }
        let out = self.runner.run(cmd)?;
        Ok(parse_unified_diff(
            &String::from_utf8_lossy(&out.stdout),
            self.max_diff_bytes,
        ))
    }
}

impl StashReader for GitCli {
    fn stashes(&self, repo: &RepoPath) -> Result<Vec<StashEntry>, GitError> {
        let text = self.read(repo, &["stash", "list", "--format=%H%x00%P%x00%ct%x00%gs"])?;
        text.lines()
            .filter(|l| !l.is_empty())
            .enumerate()
            .map(|(i, line)| {
                let mut fields = line.splitn(4, '\0');
                let mut next = || fields.next().unwrap_or_default();
                let (hash, parents, time, message) = (next(), next(), next(), next());
                let base = parents.split(' ').next().unwrap_or_default();
                Ok(StashEntry {
                    index: u32::try_from(i).unwrap_or(u32::MAX),
                    oid: oid(hash, "git stash list")?,
                    base: oid(base, "git stash list")?,
                    time: Timestamp(time.parse().unwrap_or(0)),
                    message: message.to_owned(),
                })
            })
            .collect()
    }
}
