# TODO — feature ideas

Features no other Git client offers, picked because they save time on a
multi-repo project: fewer tools to switch between, faster workflows, and the
state of the project readable at a glance. Each one is described here before
it gets a section in [SPEC.md](SPEC.md) and a step in [PLAN.md](PLAN.md).
Names follow [UI.md](UI.md).

## 1. Agent checkpoints

**Problem.** A coding agent edits several repos in one run. Seeing what it
did, and undoing all or part of it, means going through each repo by hand,
and undoing is risky when the changes were already mixed with your own.

**Feature.**

- **Checkpoint** (button and shortcut): records the working copy of every repo
  of the project (or the selected ones), staged and unstaged, untracked files
  included, without changing anything on disk.
- **Changes since checkpoint**: one view listing, per repo, the files changed
  since the checkpoint, with their diffs. This is exactly "what the agent did",
  even when the repos were already dirty before the run.
- **Roll back**: restores the checkpoint for the whole project, some repos, or
  some files. It asks for confirmation and takes a new checkpoint first, so
  the roll back can itself be undone.
- Checkpoints are listed with their time and an optional label, and expire
  after a configurable delay.
- Optional: **auto-checkpoint** when the watcher sees edits start after a
  quiet period (the usual start of an agent run).

**How.** `git stash create --include-untracked` (or a temporary index plus
`git write-tree`) builds a commit of the working state without touching it.
Store it under `refs/archipel/checkpoints/<id>`, so `git gc` keeps it and
other tools ignore it. The diff is a tree diff between the checkpoint and the
current working state. The roll back restores the files from the checkpoint
tree, and goes through the journal of `archipel-ops` like the other undoable
operations.

**Notes.**
- Checkpoints are local and personal: never pushed, never in `archipel.toml`.
- Big untracked files (build output) are skipped when `.gitignore` ignores
  them, as `git stash` does.

## 2. "Needs attention" inbox

**Problem.** Knowing what needs you each morning means a tour of the forge
(reviews, CI, PRs), the terminal (unpushed work, stashes) and every repo.

**Feature.** One list, across **all known projects** (not only the open
window), ranked by urgency, each item with its one-click fix:

| Item | Fix |
| ---- | --- |
| Merge, rebase, cherry-pick or revert in progress | Open the repo on the Operation Banner |
| CI failing on the current branch | Show the failure (CI Tooltip, SPEC §8), Rerun |
| Pull request approved and mergeable, not merged | Open the pull request |
| Review requested from you | Open the pull request |
| Will conflict with upstream or the PR target (Forecast Badge, SPEC §3) | Open the repo |
| Commits not pushed for more than a day | Push |
| Stash older than a week | Show it: Apply / Pop / Drop |
| Branch far behind the default branch | Pull / rebase |

It opens from a counter in the Status Bar and from the Action Palette.
Dismissing an item hides it until its state changes. Thresholds are personal
settings.

**How.** Almost all the data already exists in the dashboard view-models and
the forge status. The new parts are: a light background refresh of projects
that are not open (from the cached summary, without watchers, at a slow
interval), the forge "review requested" query, and stash dates.

## 3. Cross-repo search

**Problem.** "Where is this symbol used?" and "when did this string
disappear?" across N repos means a shell loop or an editor workspace that
holds every repo.

**Feature.** A search overlay (shortcut `Ctrl+Shift+F`) with two modes:

- **Files**: searches the working trees of every repo (or the selected ones,
  or one category). Results are grouped by repo, then by file, with the
  matching lines; a click opens the file in the File Editor, or in the
  external editor.
- **History**: finds the commits that added or removed a string
  (`git log -S`) or a regex (`git log -G`), in every repo. Results are grouped
  by repo; a click opens the commit in its Repo View.

Options: case, regex, path filter. Results stream in while the search runs,
and the search can be cancelled.

**How.** Files: `git grep` per repo (it respects `.gitignore` and is fast),
run in parallel by the scheduler and streamed. History: `git log -S` / `-G`
with a format that only returns ids and summaries, limited and paged. No
index to build or keep in RAM.
