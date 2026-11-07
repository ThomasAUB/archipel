//! Undo / redo of journaled operations (spec §6, PLAN §2.4).
//!
//! * Local operations are undone by restoring recorded ref and `HEAD`
//!   positions (`reset --keep` for the checked-out branch, so local changes
//!   are never overwritten), recorded stash commits, or worktree snapshots.
//! * Undo is **refused with an explanation** when the repository moved since
//!   the operation (undoing would lose work).
//! * A push cannot be undone: [`UndoPlan::Compensate`] offers compensating
//!   actions that must be confirmed explicitly ([`UndoService::compensate`]).
//! * Every undo can be redone, and vice versa.

use archipel_model::journal::{
    BatchId, EntryState, HeadRef, JournalEntry, OperationKind, RefSnapshot, StashRecord,
};
use archipel_model::requests::{CheckoutTarget, ForceMode, PushRequest, ResetMode, StashApplyMode};
use archipel_model::{CancelToken, Oid, ProgressSink, RepoId, RepoPath};

use crate::actions::{GitPorts, split_remote_ref};
use crate::batch::{BatchReport, Outcome};
use crate::error::OpError;
use crate::journal::Journal;

use OperationKind as K;

/// Undo or redo.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    /// Go back to the state before the operation.
    Undo,
    /// Re-apply an undone operation.
    Redo,
}

/// One elementary step of an undo plan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UndoStep {
    /// Move the checked-out branch (and worktree, safely) to a commit.
    ResetKeep(Oid),
    /// Move the checked-out branch only; the index and worktree keep the
    /// changes (undoing a commit leaves its changes staged, as before it).
    ResetSoft(Oid),
    /// Set or delete a ref, checking its current value.
    UpdateRef {
        /// Full ref name.
        name: String,
        /// New value (`None` deletes).
        to: Option<Oid>,
        /// Expected current value.
        expected: Option<Oid>,
    },
    /// Switch `HEAD` back.
    Checkout(HeadRef),
    /// Put a stash commit back in the stash list.
    StashStore(StashRecord),
    /// Pop the stash entry with this commit id.
    StashPop(Oid),
    /// Drop the stash entry with this commit id.
    StashDrop(Oid),
    /// Re-apply a worktree snapshot taken before a destructive operation.
    ApplySnapshot(Oid),
    /// Write back the files of a path snapshot taken before a discard.
    RestorePaths(Oid),
}

/// A compensating action for a push.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Compensation {
    /// Create revert commits for the pushed commits (newest first), then push them.
    RevertCommits(Vec<Oid>),
    /// Force the remote branch back with `--force-with-lease`.
    ForceWithLease {
        /// Remote name.
        remote: String,
        /// Branch name on the remote.
        branch: String,
        /// Value the remote must still have (what was pushed).
        expected: Oid,
        /// Value to restore (`None`: the branch did not exist, delete it).
        target: Option<Oid>,
    },
}

/// What undoing (or redoing) an entry would do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UndoPlan {
    /// Local steps, executed in order; `note` explains any partial effect.
    Steps {
        /// The steps.
        steps: Vec<UndoStep>,
        /// Extra information for the user.
        note: Option<String>,
    },
    /// The operation cannot be undone; these compensating actions can be
    /// offered, always with an explicit confirmation.
    Compensate(Vec<Compensation>),
    /// Nothing to do, with the reason.
    Nothing(String),
}

/// Plans and executes undo / redo.
#[derive(Clone)]
pub struct UndoService {
    git: GitPorts,
    journal: Journal,
}

/// Longest first-parent chain walked to list pushed commits.
const MAX_REVERT_CHAIN: usize = 1000;

fn short(name: &str) -> &str {
    name.trim_start_matches("refs/heads/")
        .trim_start_matches("refs/tags/")
        .trim_start_matches("refs/remotes/")
}

impl UndoService {
    /// Creates the service.
    #[must_use]
    pub fn new(git: GitPorts, journal: Journal) -> Self {
        Self { git, journal }
    }

    /// The entry `Ctrl+Z` would undo: the newest entry still in effect.
    pub fn next_undo(&self, repo: RepoId, depth: usize) -> Result<Option<JournalEntry>, OpError> {
        Ok(self
            .journal
            .store()
            .recent(repo, depth)?
            .into_iter()
            .find(|e| e.state == EntryState::Done))
    }

    /// The entry `Ctrl+Shift+Z` would redo: the oldest undone entry newer
    /// than every entry still in effect.
    pub fn next_redo(&self, repo: RepoId, depth: usize) -> Result<Option<JournalEntry>, OpError> {
        let recent = self.journal.store().recent(repo, depth)?;
        Ok(recent
            .into_iter()
            .take_while(|e| e.state == EntryState::Undone)
            .last())
    }

    /// Plans undoing or redoing `entry`, checking the repository did not move.
    pub fn plan(
        &self,
        repo: &RepoPath,
        entry: &JournalEntry,
        direction: Direction,
    ) -> Result<UndoPlan, OpError> {
        let expected_state = if direction == Direction::Undo {
            EntryState::Done
        } else {
            EntryState::Undone
        };
        if entry.state != expected_state {
            return Err(OpError::Refused(
                format!("“{}” is already {:?}", entry.label, entry.state).to_lowercase(),
            ));
        }
        let (from, to) = match direction {
            Direction::Undo => (&entry.after, &entry.before),
            Direction::Redo => (&entry.before, &entry.after),
        };
        match entry.kind {
            K::Commit
            | K::Amend
            | K::Merge
            | K::Rebase
            | K::CherryPick
            | K::Revert
            | K::Reset
            | K::Pull
            | K::FastForward
            | K::BranchCreate
            | K::BranchDelete
            | K::BranchRename
            | K::TagCreate
            | K::TagDelete => {
                self.check_unchanged(repo, entry, from)?;
                // A commit's "before" had its changes staged: move the branch only.
                let soft = matches!(entry.kind, K::Commit | K::Amend);
                let mut steps = ref_steps(from, to, soft);
                if direction == Direction::Undo && entry.kind == K::Reset {
                    steps.extend(entry.worktree_snapshot.map(UndoStep::ApplySnapshot));
                }
                Ok(UndoPlan::Steps { steps, note: None })
            }
            K::Checkout => {
                self.check_unchanged(repo, entry, from)?;
                let mut steps = vec![UndoStep::Checkout(to.head.clone())];
                steps.extend(
                    ref_steps(from, to, false)
                        .into_iter()
                        .filter(|s| matches!(s, UndoStep::UpdateRef { .. })),
                );
                // Changes the checkout left in the stash (they conflict
                // with its target) come back once `HEAD` is back.
                if direction == Direction::Undo {
                    steps.extend(entry.stash.as_ref().map(|s| UndoStep::StashPop(s.oid)));
                }
                Ok(UndoPlan::Steps { steps, note: None })
            }
            K::StashPush | K::StashDrop | K::StashPop => stash_plan(entry, direction),
            K::Discard => match (direction, entry.worktree_snapshot) {
                (Direction::Undo, Some(oid)) => Ok(UndoPlan::Steps {
                    steps: vec![UndoStep::RestorePaths(oid)],
                    note: None,
                }),
                (Direction::Undo, None) => Ok(UndoPlan::Nothing("nothing was discarded".into())),
                (Direction::Redo, _) => Err(OpError::Refused(
                    "discard the files again instead of redoing".into(),
                )),
            },
            K::Push => match direction {
                Direction::Undo => self.push_compensations(repo, entry),
                Direction::Redo => Err(OpError::Refused("push again instead of redoing".into())),
            },
            K::Fetch | K::Stage => Ok(UndoPlan::Nothing(
                "this operation has nothing to undo".into(),
            )),
        }
    }

    fn check_unchanged(
        &self,
        repo: &RepoPath,
        entry: &JournalEntry,
        expected: &RefSnapshot,
    ) -> Result<(), OpError> {
        let names: Vec<String> = expected.refs.iter().map(|(n, _)| n.clone()).collect();
        let current = self.git.refs.snapshot(repo, &names)?;
        if current.head != expected.head {
            return Err(OpError::Refused(format!(
                "cannot undo “{}”: HEAD moved since (it would lose work)",
                entry.label
            )));
        }
        for (name, value) in &expected.refs {
            if current.target(name) != *value {
                return Err(OpError::Refused(format!(
                    "cannot undo “{}”: {} moved since (it would lose commits)",
                    entry.label,
                    short(name)
                )));
            }
        }
        Ok(())
    }

    fn push_compensations(
        &self,
        repo: &RepoPath,
        entry: &JournalEntry,
    ) -> Result<UndoPlan, OpError> {
        let Some((name, after)) = entry
            .after
            .refs
            .first()
            .and_then(|(n, v)| v.map(|v| (n.clone(), v)))
        else {
            return Ok(UndoPlan::Nothing(
                "the push did not move a remote branch".into(),
            ));
        };
        let before = entry.before.target(&name);
        if before == Some(after) {
            return Ok(UndoPlan::Nothing(
                "the push did not change the remote".into(),
            ));
        }
        let rest = name.trim_start_matches("refs/remotes/");
        let remotes = self.git.remotes.remotes(repo)?;
        let names: Vec<&str> = remotes.iter().map(|r| r.name.as_str()).collect();
        let (remote, branch) = split_remote_ref(rest, &names);
        let mut options = Vec::new();
        if let Some(before) = before
            && let Some(commits) = self.first_parent_chain(repo, after, before)?
        {
            options.push(Compensation::RevertCommits(commits));
        }
        options.push(Compensation::ForceWithLease {
            remote: remote.to_owned(),
            branch: branch.to_owned(),
            expected: after,
            target: before,
        });
        Ok(UndoPlan::Compensate(options))
    }

    /// Commits from `tip` down to (excluding) `base` along first parents.
    fn first_parent_chain(
        &self,
        repo: &RepoPath,
        tip: Oid,
        base: Oid,
    ) -> Result<Option<Vec<Oid>>, OpError> {
        let mut chain = Vec::new();
        let mut current = tip;
        while current != base {
            if chain.len() >= MAX_REVERT_CHAIN {
                return Ok(None);
            }
            chain.push(current);
            match self.git.refs.resolve(repo, &format!("{current}~1"))? {
                Some(parent) => current = parent,
                None => return Ok(None),
            }
        }
        Ok(Some(chain))
    }

    /// Plans and executes an undo or redo, then flips the entry state.
    /// Returns a note for the user when the effect is partial.
    /// Compensating actions are never executed here (see [`Self::compensate`]).
    pub fn execute(
        &self,
        repo: &RepoPath,
        entry: &JournalEntry,
        direction: Direction,
    ) -> Result<Option<String>, OpError> {
        let (steps, note) = match self.plan(repo, entry, direction)? {
            UndoPlan::Steps { steps, note } => (steps, note),
            UndoPlan::Nothing(reason) => return Ok(Some(reason)),
            UndoPlan::Compensate(_) => {
                return Err(OpError::Refused(
                    "a push cannot be undone; choose a compensating action (revert or force-with-lease)".into(),
                ));
            }
        };
        self.run_steps(repo, &steps)?;
        let state = if direction == Direction::Undo {
            EntryState::Undone
        } else {
            EntryState::Done
        };
        // The repository already moved: a journal error must not report the
        // undo as failed, or the user would retry an undo that happened.
        if let Err(e) = self.journal.store().set_state(entry.id, state) {
            tracing::warn!(error = %e, entry = ?entry.id, "could not record the undo state");
        }
        Ok(note)
    }

    /// Runs `steps` in order. When one fails after others ran, moves `HEAD`
    /// and the touched refs back to where they were, so a failed undo
    /// leaves the repository as it found it rather than half moved (stash
    /// and worktree steps are not reverted; they only come last).
    fn run_steps(&self, repo: &RepoPath, steps: &[UndoStep]) -> Result<(), OpError> {
        let mut names = touched_refs(steps);
        // Resets move the checked-out branch without naming it.
        if let HeadRef::Symbolic(branch) = self.git.refs.snapshot(repo, &[])?.head
            && !names.contains(&branch)
        {
            names.push(branch);
        }
        let start = self.git.refs.snapshot(repo, &names)?;
        for (i, step) in steps.iter().enumerate() {
            if let Err(error) = self.run_step(repo, step) {
                if i == 0 {
                    return Err(error);
                }
                return Err(match self.roll_back(repo, &start) {
                    Ok(()) => error,
                    Err(e) => OpError::Refused(format!(
                        "{error}; restoring the previous state also failed ({e}): check the repository"
                    )),
                });
            }
        }
        Ok(())
    }

    fn roll_back(&self, repo: &RepoPath, start: &RefSnapshot) -> Result<(), OpError> {
        let names: Vec<String> = start.refs.iter().map(|(n, _)| n.clone()).collect();
        let now = self.git.refs.snapshot(repo, &names)?;
        let mut steps = Vec::new();
        if now.head != start.head {
            steps.push(UndoStep::Checkout(start.head.clone()));
        }
        // After switching back, the refs move from the state reached so far.
        let after_checkout = RefSnapshot {
            head: start.head.clone(),
            refs: now.refs,
        };
        steps.extend(ref_steps(&after_checkout, start, false));
        for step in &steps {
            self.run_step(repo, step)?;
        }
        Ok(())
    }

    fn stash_index(&self, repo: &RepoPath, oid: Oid) -> Result<u32, OpError> {
        self.git
            .stashes
            .stashes(repo)?
            .into_iter()
            .find(|s| s.oid == oid)
            .map(|s| s.index)
            .ok_or_else(|| OpError::Refused("the stash entry no longer exists".into()))
    }

    fn run_step(&self, repo: &RepoPath, step: &UndoStep) -> Result<(), OpError> {
        let g = &self.git;
        match step {
            UndoStep::ResetKeep(oid) => g.ref_writer.reset(repo, &oid.to_hex(), ResetMode::Keep)?,
            UndoStep::ResetSoft(oid) => g.ref_writer.reset(repo, &oid.to_hex(), ResetMode::Soft)?,
            UndoStep::UpdateRef { name, to, expected } => {
                g.ref_writer.update_ref(repo, name, *to, *expected)?;
            }
            UndoStep::Checkout(HeadRef::Symbolic(name)) => {
                g.branches
                    .checkout(repo, &CheckoutTarget::Branch(short(name).to_owned()))?;
            }
            UndoStep::Checkout(HeadRef::Detached(oid)) => {
                g.branches.checkout(repo, &CheckoutTarget::Detached(*oid))?;
            }
            UndoStep::StashStore(record) => {
                g.stash_writer
                    .stash_store(repo, record.oid, &record.message)?;
            }
            UndoStep::StashPop(oid) => {
                let index = self.stash_index(repo, *oid)?;
                g.stash_writer
                    .stash_apply(repo, index, StashApplyMode::Pop)?;
            }
            UndoStep::StashDrop(oid) => {
                let index = self.stash_index(repo, *oid)?;
                g.stash_writer.drop_stash(repo, index)?;
            }
            UndoStep::ApplySnapshot(oid) => {
                g.stash_writer
                    .stash_store(repo, *oid, "archipel: restored by undo")?;
                g.stash_writer.stash_apply(repo, 0, StashApplyMode::Pop)?;
            }
            UndoStep::RestorePaths(oid) => g.snapshots.restore_paths(repo, *oid)?,
        }
        Ok(())
    }

    /// Runs a confirmed compensating action for a push, then marks the entry undone.
    pub fn compensate(
        &self,
        repo: &RepoPath,
        entry: &JournalEntry,
        choice: &Compensation,
        progress: &dyn ProgressSink,
        cancel: &CancelToken,
    ) -> Result<(), OpError> {
        match choice {
            Compensation::RevertCommits(commits) => {
                for commit in commits {
                    self.git.rewriter.revert(repo, *commit)?;
                }
                self.git
                    .remote
                    .push(repo, &PushRequest::default(), progress, cancel)?;
            }
            Compensation::ForceWithLease {
                remote,
                branch,
                expected,
                target,
            } => {
                let refspec = match target {
                    Some(t) => format!("{t}:refs/heads/{branch}"),
                    None => format!(":refs/heads/{branch}"),
                };
                let request = PushRequest {
                    remote: Some(remote.clone()),
                    refspec: Some(refspec),
                    force: ForceMode::WithLease(Some(*expected)),
                    ..PushRequest::default()
                };
                self.git.remote.push(repo, &request, progress, cancel)?;
            }
        }
        self.journal
            .store()
            .set_state(entry.id, EntryState::Undone)?;
        Ok(())
    }

    /// Undoes (or redoes) every entry of a batch as one unit, newest first,
    /// with a per-repo report. `locate` maps a repo id to its path.
    pub fn execute_batch(
        &self,
        batch: BatchId,
        direction: Direction,
        locate: &dyn Fn(RepoId) -> Option<RepoPath>,
    ) -> Result<BatchReport<Option<String>>, OpError> {
        let mut entries = self.journal.store().batch(batch)?;
        entries.sort_by_key(|e| std::cmp::Reverse(e.id));
        let mut report = BatchReport::default();
        for entry in entries {
            let outcome = match locate(entry.repo) {
                None => Outcome::Skipped("repository is not in the project anymore".into()),
                Some(path) => match self.execute(&path, &entry, direction) {
                    Ok(note) => Outcome::Done(note),
                    Err(e) => Outcome::Failed(e),
                },
            };
            report.push(entry.repo, outcome);
        }
        Ok(report)
    }
}

/// Steps moving every ref from its `from` value to its `to` value; the
/// branch `HEAD` is on moves with `reset --keep` (or `--soft` when `soft`).
/// The refs `steps` move, for the snapshot a rollback returns to.
fn touched_refs(steps: &[UndoStep]) -> Vec<String> {
    let mut names = Vec::new();
    for step in steps {
        if let UndoStep::UpdateRef { name, .. } = step
            && !names.contains(name)
        {
            names.push(name.clone());
        }
    }
    names
}

fn ref_steps(from: &RefSnapshot, to: &RefSnapshot, soft: bool) -> Vec<UndoStep> {
    let move_head = |oid: Oid| {
        if soft {
            UndoStep::ResetSoft(oid)
        } else {
            UndoStep::ResetKeep(oid)
        }
    };
    let head_branch = match &from.head {
        HeadRef::Symbolic(name) => Some(name.as_str()),
        HeadRef::Detached(_) => None,
    };
    let mut steps = Vec::new();
    for (name, target) in &to.refs {
        let current = from.target(name);
        if current == *target {
            continue;
        }
        match (head_branch == Some(name.as_str()), target) {
            (true, Some(oid)) => steps.push(move_head(*oid)),
            _ => steps.push(UndoStep::UpdateRef {
                name: name.clone(),
                to: *target,
                expected: current,
            }),
        }
    }
    if let (HeadRef::Detached(a), HeadRef::Detached(b)) = (&from.head, &to.head)
        && a != b
    {
        steps.push(move_head(*b));
    }
    steps
}

fn stash_plan(entry: &JournalEntry, direction: Direction) -> Result<UndoPlan, OpError> {
    let record = entry
        .stash
        .clone()
        .ok_or_else(|| OpError::Refused("no stash was recorded".into()))?;
    let steps = match (entry.kind, direction) {
        (K::StashPush, Direction::Undo) => vec![UndoStep::StashPop(record.oid)],
        (OperationKind::StashDrop, Direction::Undo) => vec![UndoStep::StashStore(record)],
        (OperationKind::StashDrop, Direction::Redo) => vec![UndoStep::StashDrop(record.oid)],
        (OperationKind::StashPop, Direction::Undo) => {
            return Ok(UndoPlan::Steps {
                steps: vec![UndoStep::StashStore(record)],
                note: Some(
                    "the stash entry is back; the changes it applied are kept in the working tree"
                        .into(),
                ),
            });
        }
        _ => {
            return Err(OpError::Refused(
                "stash the changes again instead of redoing".into(),
            ));
        }
    };
    Ok(UndoPlan::Steps { steps, note: None })
}
