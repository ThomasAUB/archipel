//! Status and summary with `gix`.

use std::collections::BTreeMap;

use archipel_model::changes::{ChangeKind, FileChange, FileState};
use archipel_model::ports::StatusReader;
use archipel_model::{
    ChangeCounts, GitError, HeadState, RepoPath, RepoSummary, Timestamp, Upstream,
};
use gix::bstr::ByteSlice;
use gix::status::UntrackedFiles;

use super::{GixReader, backend, to_oid};
use crate::gitdir::{GitDirs, has_submodules};
use crate::parse::status::count_changes;

#[derive(Default, Clone, Copy)]
struct PathState {
    staged: Option<ChangeKind>,
    unstaged: Option<ChangeKind>,
    untracked: bool,
    conflicted: bool,
}

struct Collected {
    paths: BTreeMap<String, PathState>,
    originals: BTreeMap<String, String>,
}

fn collect(repo: &gix::Repository, untracked: UntrackedFiles) -> Result<Collected, GitError> {
    use gix::status::index_worktree::Item as Wt;
    use gix::status::plumbing::index_as_worktree::{Change, EntryStatus};

    let iter = repo
        .status(gix::progress::Discard)
        .map_err(backend)?
        .untracked_files(untracked)
        .index_worktree_rewrites(None)
        .into_iter(None)
        .map_err(backend)?;
    let mut paths: BTreeMap<String, PathState> = BTreeMap::new();
    let mut originals = BTreeMap::new();
    for item in iter {
        match item.map_err(backend)? {
            gix::status::Item::TreeIndex(change) => {
                use gix::diff::index::ChangeRef;
                let kind = match &change {
                    ChangeRef::Addition { .. } => ChangeKind::Added,
                    ChangeRef::Deletion { .. } => ChangeKind::Deleted,
                    ChangeRef::Modification { .. } => ChangeKind::Modified,
                    ChangeRef::Rewrite {
                        source_location,
                        copy,
                        ..
                    } => {
                        originals.insert(
                            change.location().to_str_lossy().into_owned(),
                            source_location.to_str_lossy().into_owned(),
                        );
                        if *copy {
                            ChangeKind::Copied
                        } else {
                            ChangeKind::Renamed
                        }
                    }
                };
                paths
                    .entry(change.location().to_str_lossy().into_owned())
                    .or_default()
                    .staged = Some(kind);
            }
            gix::status::Item::IndexWorktree(item) => match item {
                Wt::Modification {
                    rela_path, status, ..
                } => {
                    let state = paths
                        .entry(rela_path.to_str_lossy().into_owned())
                        .or_default();
                    match status {
                        EntryStatus::Conflict { .. } => state.conflicted = true,
                        EntryStatus::Change(Change::Removed) => {
                            state.unstaged = Some(ChangeKind::Deleted);
                        }
                        EntryStatus::Change(Change::Type { .. }) => {
                            state.unstaged = Some(ChangeKind::TypeChanged);
                        }
                        EntryStatus::Change(_) => state.unstaged = Some(ChangeKind::Modified),
                        EntryStatus::IntentToAdd => state.unstaged = Some(ChangeKind::Added),
                        EntryStatus::NeedsUpdate(_) => {}
                    }
                }
                Wt::DirectoryContents { entry, .. } => {
                    if entry.status == gix::dir::entry::Status::Untracked {
                        paths
                            .entry(entry.rela_path.to_str_lossy().into_owned())
                            .or_default()
                            .untracked = true;
                    }
                }
                Wt::Rewrite {
                    source,
                    dirwalk_entry,
                    ..
                } => {
                    paths
                        .entry(source.rela_path().to_str_lossy().into_owned())
                        .or_default()
                        .unstaged = Some(ChangeKind::Deleted);
                    paths
                        .entry(dirwalk_entry.rela_path.to_str_lossy().into_owned())
                        .or_default()
                        .untracked = true;
                }
            },
        }
    }
    // NeedsUpdate-only entries are not changes.
    paths.retain(|_, s| s.staged.is_some() || s.unstaged.is_some() || s.untracked || s.conflicted);
    Ok(Collected { paths, originals })
}

fn to_changes(collected: Collected) -> Vec<FileChange> {
    let Collected {
        paths,
        mut originals,
    } = collected;
    paths
        .into_iter()
        .map(|(path, s)| {
            let state = if s.conflicted {
                FileState::Conflicted
            } else if s.untracked && s.staged.is_none() {
                FileState::Untracked
            } else {
                FileState::Tracked {
                    staged: s.staged.unwrap_or(ChangeKind::Unmodified),
                    unstaged: s.unstaged.unwrap_or(ChangeKind::Unmodified),
                }
            };
            let original_path = originals.remove(&path);
            FileChange {
                path,
                original_path,
                state,
            }
        })
        .collect()
}

fn upstream(
    repo: &gix::Repository,
    branch: &gix::refs::FullName,
) -> Result<Option<Upstream>, GitError> {
    let Some(tracking) =
        repo.branch_remote_tracking_ref_name(branch.as_ref(), gix::remote::Direction::Fetch)
    else {
        return Ok(None);
    };
    let tracking = tracking.map_err(backend)?;
    let name = tracking.as_ref().shorten().to_str_lossy().into_owned();
    let Some(mut up_ref) = repo
        .try_find_reference(tracking.as_ref())
        .map_err(backend)?
    else {
        return Ok(Some(Upstream {
            name,
            ahead: 0,
            behind: 0,
            gone: true,
        }));
    };
    let up = up_ref.peel_to_id().map_err(backend)?.detach();
    let local = repo
        .find_reference(branch.as_ref())
        .map_err(backend)?
        .peel_to_id()
        .map_err(backend)?
        .detach();
    let count = |tip: gix::ObjectId, hidden: gix::ObjectId| -> Result<u32, GitError> {
        let walk = repo
            .rev_walk([tip])
            .with_hidden([hidden])
            .all()
            .map_err(backend)?;
        let mut n = 0u32;
        for info in walk {
            info.map_err(backend)?;
            n += 1;
        }
        Ok(n)
    };
    let (ahead, behind) = if up == local {
        (0, 0)
    } else {
        (count(local, up)?, count(up, local)?)
    };
    Ok(Some(Upstream {
        name,
        ahead,
        behind,
        gone: false,
    }))
}

pub(super) fn stash_count(repo: &gix::Repository) -> Result<u32, GitError> {
    let Some(stash) = repo.try_find_reference("refs/stash").map_err(backend)? else {
        return Ok(0);
    };
    let mut platform = stash.log_iter();
    let Some(log) = platform.all().map_err(backend)? else {
        return Ok(0);
    };
    Ok(u32::try_from(log.count()).unwrap_or(u32::MAX))
}

fn describe(repo: &gix::Repository) -> Option<String> {
    let commit = repo.head_commit().ok()?;
    let format = commit
        .describe()
        .names(gix::commit::describe::SelectRef::AllTags)
        .id_as_fallback(false)
        .try_format()
        .ok()??;
    Some(format.to_string())
}

impl StatusReader for GixReader {
    fn summary(&self, path: &RepoPath) -> Result<RepoSummary, GitError> {
        let dirs = GitDirs::locate(path.as_path())?;
        let repo = self.open(path)?;
        let head_name = repo.head_name().map_err(backend)?;
        let head_oid = repo.head_id().ok().map(to_oid).transpose()?;
        let head = match (&head_name, head_oid) {
            (Some(name), Some(_)) => HeadState::Branch(name.shorten().to_str_lossy().into_owned()),
            (Some(name), None) => HeadState::Unborn(name.shorten().to_str_lossy().into_owned()),
            (None, Some(oid)) => HeadState::Detached(oid),
            (None, None) => return Err(GitError::Backend("HEAD is invalid".into())),
        };
        let upstream = match (&head_name, head_oid) {
            (Some(name), Some(_)) => upstream(&repo, name)?,
            _ => None,
        };
        let changes: ChangeCounts =
            count_changes(&to_changes(collect(&repo, UntrackedFiles::Collapsed)?));
        Ok(RepoSummary {
            head,
            head_oid,
            upstream,
            changes,
            stash_count: stash_count(&repo)?,
            describe: describe(&repo),
            last_fetch: dirs.last_fetch(),
            in_progress: dirs.in_progress(),
            has_submodules: has_submodules(path.as_path()),
            computed_at: Timestamp::now(),
        })
    }

    fn changes(&self, path: &RepoPath) -> Result<Vec<FileChange>, GitError> {
        let repo = self.open(path)?;
        Ok(to_changes(collect(&repo, UntrackedFiles::Files)?))
    }
}
