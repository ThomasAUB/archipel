//! Typed requests for write operations (enums instead of boolean flags).

use serde::{Deserialize, Serialize};

use crate::ids::Oid;
use crate::refs::{RefKind, RefTip};

/// Whether a commit creates a new commit or amends `HEAD`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum CommitMode {
    /// Create a new commit.
    #[default]
    New,
    /// Replace the `HEAD` commit.
    Amend,
}

/// A commit request. The message goes through stdin so hooks and signing run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitRequest {
    /// Full commit message.
    pub message: String,
    /// New commit or amend.
    pub mode: CommitMode,
}

/// Direction in which a patch is applied to the index.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PatchDirection {
    /// Apply the patch (stage).
    Forward,
    /// Apply the patch in reverse (unstage).
    Reverse,
}

/// What to check out.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CheckoutTarget {
    /// An existing local branch.
    Branch(String),
    /// Create a local branch tracking the given remote branch (`origin/x` → `x`).
    TrackRemote(String),
    /// Detach `HEAD` at a commit.
    Detached(Oid),
}

/// A local branch, as checking out a remote branch through it needs it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LocalCounterpart {
    /// The branch it tracks (`origin/main`), if any.
    pub upstream: Option<String>,
}

/// The local branch named after remote branch `remote` (`origin/x` → `x`).
fn local_name(remote: &str) -> &str {
    remote.split_once('/').map_or(remote, |(_, b)| b)
}

impl CheckoutTarget {
    /// What checking out `tip` means: a local branch is switched to; a
    /// remote branch through its local counterpart (`local` gives it by
    /// name), created to track it when there is none; a tag detaches at its
    /// commit. `None` for `HEAD` (already there), stashes, and a remote
    /// branch whose counterpart tracks another branch (see
    /// [`Self::diverted`]).
    #[must_use]
    pub fn for_ref(tip: &RefTip, local: impl Fn(&str) -> Option<LocalCounterpart>) -> Option<Self> {
        match tip.kind {
            RefKind::LocalBranch => Some(Self::Branch(tip.name.clone())),
            RefKind::RemoteBranch => {
                let name = local_name(&tip.name);
                match local(name) {
                    None => Some(Self::TrackRemote(tip.name.clone())),
                    Some(l) if l.upstream.as_ref().is_some_and(|u| *u != tip.name) => None,
                    Some(_) => Some(Self::Branch(name.to_owned())),
                }
            }
            RefKind::Tag => Some(Self::Detached(tip.target)),
            RefKind::Head | RefKind::Stash => None,
        }
    }

    /// For a remote branch whose local counterpart tracks another branch
    /// (`fork/main` while `main` tracks `origin/main`): that local branch
    /// and its upstream. Switching to it would not check out the remote
    /// branch, so there is no implicit target: the user picks (that local
    /// branch, the remote one detached, or a new branch tracking it).
    #[must_use]
    pub fn diverted(
        tip: &RefTip,
        local: impl Fn(&str) -> Option<LocalCounterpart>,
    ) -> Option<(String, String)> {
        if tip.kind != RefKind::RemoteBranch {
            return None;
        }
        let name = local_name(&tip.name);
        let upstream = local(name)?.upstream?;
        (upstream != tip.name).then(|| (name.to_owned(), upstream))
    }

    /// What checking out commit `oid` means, given the refs pointing at it:
    /// its first local branch, else its first remote branch (see
    /// [`Self::for_ref`]), else the commit itself, detached.
    #[must_use]
    pub fn for_commit(
        oid: Oid,
        tips: &[RefTip],
        local: impl Fn(&str) -> Option<LocalCounterpart>,
    ) -> Self {
        [RefKind::LocalBranch, RefKind::RemoteBranch]
            .iter()
            .find_map(|kind| tips.iter().find(|t| t.kind == *kind))
            .and_then(|t| Self::for_ref(t, local))
            .unwrap_or(Self::Detached(oid))
    }
}

/// How to delete a branch.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeleteMode {
    /// Refuse when the branch is not merged (`-d`).
    Safe,
    /// Delete anyway (`-D`).
    Force,
}

/// A tag creation request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TagRequest {
    /// Tag name.
    pub name: String,
    /// Commit-ish to tag (`HEAD` when `None`).
    pub target: Option<String>,
    /// Message for an annotated tag; lightweight when `None`.
    pub message: Option<String>,
}

/// A `git submodule update` request (spec §9).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubmoduleUpdate {
    /// Submodule paths relative to the parent; every submodule when empty.
    pub paths: Vec<String>,
    /// Initialize submodules that are not yet (`--init`).
    pub init: bool,
    /// Also update nested submodules (`--recursive`).
    pub recursive: bool,
}

/// A `git submodule add` request (spec §9).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubmoduleAdd {
    /// Repository to clone: a URL, or a path relative to the parent's
    /// remote (`../other.git`).
    pub url: String,
    /// Where it goes, relative to the parent working tree, `/`-separated.
    pub path: String,
}

/// Mode of `git reset`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResetMode {
    /// Move `HEAD` only.
    Soft,
    /// Move `HEAD` and reset the index.
    Mixed,
    /// Move `HEAD`, reset index and worktree (destructive).
    Hard,
    /// Like hard but refuses to lose local changes.
    Keep,
}

/// Whether untracked files are included in a stash.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum UntrackedMode {
    /// Tracked files only.
    #[default]
    Exclude,
    /// Include untracked files.
    Include,
}

/// A stash push request.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StashRequest {
    /// Optional message.
    pub message: Option<String>,
    /// Whether to include untracked files.
    pub untracked: UntrackedMode,
}

/// Whether applying a stash also drops it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum StashApplyMode {
    /// `git stash apply`.
    Keep,
    /// `git stash pop`.
    Pop,
}

/// How `pull` integrates remote changes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum PullMode {
    /// `--ff-only` (default: never creates merge commits silently).
    #[default]
    FastForwardOnly,
    /// `--rebase`.
    Rebase,
    /// `--no-rebase` (merge).
    Merge,
}

/// A fetch request.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FetchRequest {
    /// Remote name; all remotes when `None`.
    pub remote: Option<String>,
    /// Prune deleted remote branches.
    pub prune: bool,
}

/// Whether a push may overwrite the remote.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ForceMode {
    /// Regular push.
    #[default]
    None,
    /// `--force-with-lease`, optionally with the expected remote OID.
    WithLease(Option<Oid>),
}

/// A push request.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PushRequest {
    /// Remote name (`origin` when `None`).
    pub remote: Option<String>,
    /// Refspec or branch name (current branch when `None`).
    pub refspec: Option<String>,
    /// Set upstream (`-u`).
    pub set_upstream: bool,
    /// Force mode.
    pub force: ForceMode,
    /// Push tags too.
    pub tags: bool,
}

/// How a merge is performed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum MergeMode {
    /// Default: fast-forward when possible, else merge commit.
    #[default]
    Auto,
    /// Fast-forward only.
    FastForwardOnly,
    /// Always create a merge commit.
    NoFastForward,
}

/// Action on a stopped merge, rebase, cherry-pick or revert.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SequenceAction {
    /// `--continue`.
    Continue,
    /// `--abort`.
    Abort,
    /// `--skip` (rebase / cherry-pick).
    Skip,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tip(name: &str, kind: RefKind, target: Oid) -> RefTip {
        RefTip {
            name: name.into(),
            kind,
            target,
        }
    }

    fn none(_: &str) -> Option<LocalCounterpart> {
        None
    }

    /// Local branches `feat/x` (no upstream) and `main` (tracks
    /// `origin/main`).
    fn locals(name: &str) -> Option<LocalCounterpart> {
        match name {
            "feat/x" => Some(LocalCounterpart::default()),
            "main" => Some(LocalCounterpart {
                upstream: Some("origin/main".into()),
            }),
            _ => None,
        }
    }

    #[test]
    fn ref_checkout_targets() {
        let oid = Oid::from_bytes(&[7; 20]).unwrap();
        let local = tip("main", RefKind::LocalBranch, oid);
        assert_eq!(
            CheckoutTarget::for_ref(&local, none),
            Some(CheckoutTarget::Branch("main".into()))
        );
        let remote = tip("origin/feat/x", RefKind::RemoteBranch, oid);
        assert_eq!(
            CheckoutTarget::for_ref(&remote, none),
            Some(CheckoutTarget::TrackRemote("origin/feat/x".into()))
        );
        assert_eq!(
            CheckoutTarget::for_ref(&remote, locals),
            Some(CheckoutTarget::Branch("feat/x".into()))
        );
        assert_eq!(CheckoutTarget::diverted(&remote, locals), None);
        let tag = tip("v1.0", RefKind::Tag, oid);
        assert_eq!(
            CheckoutTarget::for_ref(&tag, none),
            Some(CheckoutTarget::Detached(oid))
        );
        assert_eq!(
            CheckoutTarget::for_ref(&tip("HEAD", RefKind::Head, oid), none),
            None
        );
        assert_eq!(
            CheckoutTarget::for_ref(&tip("stash", RefKind::Stash, oid), none),
            None
        );
    }

    #[test]
    fn a_remote_branch_is_not_checked_out_through_a_local_branch_tracking_another() {
        let oid = Oid::from_bytes(&[5; 20]).unwrap();
        // `main` tracks `origin/main`: checking out `origin/main` goes
        // through it, `fork/main` does not.
        let origin = tip("origin/main", RefKind::RemoteBranch, oid);
        assert_eq!(
            CheckoutTarget::for_ref(&origin, locals),
            Some(CheckoutTarget::Branch("main".into()))
        );
        assert_eq!(CheckoutTarget::diverted(&origin, locals), None);
        let fork = tip("fork/main", RefKind::RemoteBranch, oid);
        assert_eq!(CheckoutTarget::for_ref(&fork, locals), None);
        assert_eq!(
            CheckoutTarget::diverted(&fork, locals),
            Some(("main".into(), "origin/main".into()))
        );
        // A commit only `fork/main` points at: checked out detached.
        assert_eq!(
            CheckoutTarget::for_commit(oid, &[fork], locals),
            CheckoutTarget::Detached(oid)
        );
        let local = tip("main", RefKind::LocalBranch, oid);
        assert_eq!(CheckoutTarget::diverted(&local, locals), None);
    }

    #[test]
    fn commit_checkout_prefers_local_then_remote_branches() {
        let oid = Oid::from_bytes(&[3; 20]).unwrap();
        let head = tip("HEAD", RefKind::Head, oid);
        let tag = tip("v2", RefKind::Tag, oid);
        let remote = tip("origin/dev", RefKind::RemoteBranch, oid);
        let local = tip("dev", RefKind::LocalBranch, oid);
        assert_eq!(
            CheckoutTarget::for_commit(
                oid,
                &[head.clone(), tag.clone(), remote.clone(), local],
                none
            ),
            CheckoutTarget::Branch("dev".into())
        );
        assert_eq!(
            CheckoutTarget::for_commit(oid, &[tag.clone(), remote], none),
            CheckoutTarget::TrackRemote("origin/dev".into())
        );
        assert_eq!(
            CheckoutTarget::for_commit(oid, &[head, tag], none),
            CheckoutTarget::Detached(oid)
        );
        assert_eq!(
            CheckoutTarget::for_commit(oid, &[], none),
            CheckoutTarget::Detached(oid)
        );
    }
}
