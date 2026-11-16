# ADR 0004 — Worktrees: rows like repos, discovered from Git, undo by snapshot

- Status: accepted
- Date: 2026-11-16
- Context: SPEC §10, [WORKTREES.md](../WORKTREES.md), PLAN Phase 8.

## Context

Linked worktrees (`git worktree add`) must be first-class: shown under the
repo they belong to, usable like any repo (columns, Quick Edit Pane, Repo
Tab, staging, commit, forge status, forecast, undo), created, removed and
grouped into Worktree Sets for agent runs across repos. Agents create and
remove them behind Archipel's back, so the list must follow Git live, and a
project may carry many of them without blowing the RAM and watch budgets.

## Decision

### A worktree is a row like a repo

A linked worktree is a `DashboardRow` with its own `RepoId` (derived from its
path, as for any repo), so every per-repo code path works on it unchanged.
What is new is the *relation*: `DashboardRow::worktree` (`WorktreeRowInfo`:
parent repo row, label, lock, missing) and the session's worktree lists,
never `RepoEntry`. Ordering, nesting and the set view are done by the
view-model (`dashboard::nested_rows`, `App::visible_rows`); batch "all"
means every repo, or the set's worktrees in a set view.

### Discovered, not stored

The list comes from `git worktree list --porcelain -z` (`WorktreeReader`,
CLI only: `gix` support is partial; a `gix` reader can replace it behind the
same trait). Only *how* to make worktrees is configuration: the location
templates and terminal / editor commands in the personal settings, the
`copy_to_worktrees` patterns per repo in `archipel.toml`. Worktree Sets, the
expanded groups and the Set Selector choice are personal UI state.

The watcher reports `<common dir>/worktrees/` changes as `Scope::WORKTREES`:
the repo's list is read again, not its summary. A linked worktree is watched
only while visible (expanded, open in a tab, in the active set); otherwise
its summary is refreshed on window focus. Fetches run once per repo and
their time is shared with its worktrees (refs are shared).

### Writes through the CLI, undo by snapshot

`WorktreeWriter` runs `git worktree add/remove/lock/unlock/prune/move`, as
every write goes through the CLI. `LocalFilesCopier` copies the local files
(globs, never overwriting, never following links, never into `.git`).

Removal is journaled on the repo (main worktree) as `WorktreeRemove`:
before removing, the worktree's changed paths are snapshotted with
`snapshot_paths` (a dangling commit, untracked files included) and its
`HEAD` and branch tip recorded (`JournalEntry::worktree`). Undo recreates the
branch when it was deleted, `git worktree add`s the same path on the same
branch, then `restore_paths` writes the files back byte for byte. It is
refused when the branch is checked out elsewhere or exists again. Undo of a
`WorktreeAdd` removes the worktree while it is clean (and the branch it
created while unmoved). Redo is refused for both: do it again instead.

Undo of a ref move touching a branch checked out in another worktree is
refused (it would change that worktree's `HEAD` under it).

## Consequences

- No new crate or dependency edge; three new ports (`WorktreeReader`,
  `WorktreeWriter`, `LocalFilesCopier`) with fakes in `testing`.
- A worktree row costs one summary; collapsed ones cost no watch.
- Snapshots are dangling objects: `git gc` may prune them after its grace
  period (two weeks by default), as for discard undo.
