//! Refs and stashes with `gix`.

use archipel_model::journal::{HeadRef, RefSnapshot};
use archipel_model::ports::{RefReader, StashReader};
use archipel_model::refs::{BranchInfo, RefKind, RefTip, StashEntry, TagInfo};
use archipel_model::{GitError, Oid, RepoPath, Timestamp, Upstream};
use gix::bstr::ByteSlice;

use super::{GixReader, backend, count_exclusive, to_oid};

fn commit_time(repo: &gix::Repository, id: gix::ObjectId) -> Timestamp {
    repo.find_commit(id)
        .ok()
        .and_then(|c| c.time().ok())
        .map_or(Timestamp(0), |t| Timestamp(t.seconds))
}

/// The upstream of the branch `full` pointing at `local` (`None`: unborn),
/// with ahead / behind counts.
pub(super) fn tracking(
    repo: &gix::Repository,
    full: &gix::refs::FullNameRef,
    local: Option<gix::ObjectId>,
) -> Result<Option<Upstream>, GitError> {
    let Some(name) = repo.branch_remote_tracking_ref_name(full, gix::remote::Direction::Fetch)
    else {
        return Ok(None);
    };
    let name = name.map_err(backend)?;
    let short = name.as_ref().shorten().to_str_lossy().into_owned();
    let Some(mut up) = repo.try_find_reference(name.as_ref()).map_err(backend)? else {
        return Ok(Some(Upstream {
            name: short,
            ahead: 0,
            behind: 0,
            gone: true,
        }));
    };
    let up = up.peel_to_id().map_err(backend)?.detach();
    let (ahead, behind) = match local {
        Some(local) if local != up => (
            count_exclusive(repo, local, up)?,
            count_exclusive(repo, up, local)?,
        ),
        _ => (0, 0),
    };
    Ok(Some(Upstream {
        name: short,
        ahead,
        behind,
        gone: false,
    }))
}

impl RefReader for GixReader {
    fn branches(&self, path: &RepoPath) -> Result<Vec<BranchInfo>, GitError> {
        let repo = self.open(path)?;
        let head = repo.head_name().map_err(backend)?;
        let platform = repo.references().map_err(backend)?;
        let mut out = Vec::new();
        for (iter, is_remote) in [
            (platform.local_branches().map_err(backend)?, false),
            (platform.remote_branches().map_err(backend)?, true),
        ] {
            for r in iter {
                let mut r = r.map_err(backend)?;
                let full = r.name().to_owned();
                if is_remote && full.as_bstr().ends_with(b"/HEAD") {
                    continue;
                }
                let id = r.peel_to_id().map_err(backend)?.detach();
                out.push(BranchInfo {
                    name: full.shorten().to_str_lossy().into_owned(),
                    is_remote,
                    target: to_oid(id)?,
                    is_head: head.as_ref() == Some(&full),
                    upstream: if is_remote {
                        None
                    } else {
                        tracking(&repo, full.as_ref(), Some(id))?
                    },
                    tip_time: commit_time(&repo, id),
                });
            }
        }
        Ok(out)
    }

    fn tags(&self, path: &RepoPath) -> Result<Vec<TagInfo>, GitError> {
        let repo = self.open(path)?;
        let platform = repo.references().map_err(backend)?;
        let mut out = Vec::new();
        for r in platform.tags().map_err(backend)? {
            let mut r = r.map_err(backend)?;
            let direct = r.target().try_id().map(ToOwned::to_owned);
            let annotated = direct.is_some_and(|id| {
                repo.find_object(id)
                    .is_ok_and(|o| o.kind == gix::object::Kind::Tag)
            });
            let id = r.peel_to_id().map_err(backend)?.detach();
            out.push(TagInfo {
                name: r.name().shorten().to_str_lossy().into_owned(),
                target: to_oid(id)?,
                annotated,
            });
        }
        Ok(out)
    }

    fn ref_tips(&self, path: &RepoPath) -> Result<Vec<RefTip>, GitError> {
        let repo = self.open(path)?;
        let mut tips = Vec::new();
        if let Ok(head) = repo.head_id() {
            tips.push(RefTip {
                name: "HEAD".into(),
                kind: RefKind::Head,
                target: to_oid(head)?,
            });
        }
        for r in repo.references().map_err(backend)?.all().map_err(backend)? {
            let mut r = r.map_err(backend)?;
            let full = r.name().as_bstr().to_str_lossy().into_owned();
            let (kind, name) = if let Some(n) = full.strip_prefix("refs/heads/") {
                (RefKind::LocalBranch, n.to_owned())
            } else if let Some(n) = full.strip_prefix("refs/remotes/") {
                if n.ends_with("/HEAD") {
                    continue;
                }
                (RefKind::RemoteBranch, n.to_owned())
            } else if let Some(n) = full.strip_prefix("refs/tags/") {
                (RefKind::Tag, n.to_owned())
            } else if full == "refs/stash" {
                (RefKind::Stash, "stash".to_owned())
            } else {
                continue;
            };
            let Ok(id) = r.peel_to_id() else { continue };
            tips.push(RefTip {
                name,
                kind,
                target: to_oid(id)?,
            });
        }
        Ok(tips)
    }

    fn resolve(&self, path: &RepoPath, rev: &str) -> Result<Option<Oid>, GitError> {
        let repo = self.open(path)?;
        let Ok(id) = repo.rev_parse_single(rev) else {
            return Ok(None);
        };
        let Ok(object) = id.object() else {
            return Ok(None);
        };
        match object.peel_to_kind(gix::object::Kind::Commit) {
            Ok(commit) => to_oid(commit.id).map(Some),
            Err(_) => Ok(None),
        }
    }

    fn snapshot(&self, path: &RepoPath, refs: &[String]) -> Result<RefSnapshot, GitError> {
        let repo = self.open(path)?;
        let head = match repo.head_name().map_err(backend)? {
            Some(name) => HeadRef::Symbolic(name.as_bstr().to_str_lossy().into_owned()),
            None => HeadRef::Detached(to_oid(repo.head_id().map_err(backend)?)?),
        };
        let mut values = Vec::with_capacity(refs.len());
        for name in refs {
            let target = match repo.try_find_reference(name.as_str()).map_err(backend)? {
                // Record the direct target (an annotated tag's object, not
                // its commit), as the CLI does, so undo restores it exactly.
                Some(mut r) => match r.target().try_id().map(ToOwned::to_owned) {
                    Some(id) => Some(to_oid(id)?),
                    None => Some(to_oid(r.peel_to_id().map_err(backend)?)?),
                },
                None => None,
            };
            values.push((name.clone(), target));
        }
        Ok(RefSnapshot { head, refs: values })
    }
}

impl StashReader for GixReader {
    fn stashes(&self, path: &RepoPath) -> Result<Vec<StashEntry>, GitError> {
        let repo = self.open(path)?;
        let Some(stash) = repo.try_find_reference("refs/stash").map_err(backend)? else {
            return Ok(Vec::new());
        };
        let mut platform = stash.log_iter();
        let Some(log) = platform.rev().map_err(backend)? else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        for (i, line) in log.enumerate() {
            let line = line.map_err(backend)?;
            let commit = repo.find_commit(line.new_oid).map_err(backend)?;
            let base = commit
                .parent_ids()
                .next()
                .ok_or_else(|| backend(format!("stash {} has no parent", line.new_oid)))?;
            out.push(StashEntry {
                index: u32::try_from(i).unwrap_or(u32::MAX),
                oid: to_oid(line.new_oid)?,
                base: to_oid(base)?,
                time: commit_time(&repo, line.new_oid),
                message: line.message.to_str_lossy().into_owned(),
            });
        }
        Ok(out)
    }
}
