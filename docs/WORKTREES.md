# Worktrees — UI/UX spec

Detailed spec of SPEC.md §10. It says **what** the user sees and does; the
implementation steps go to [PLAN.md](PLAN.md). Names follow [UI.md](UI.md);
the new names defined here (in **bold**) move to UI.md as they are built.

## 1. Why

A linked worktree (`git worktree add`) is a second checkout of the same repo,
on another branch, in another folder, sharing objects and refs. It is the
natural way to:

- **run coding agents in parallel**: one agent per branch, each in its own
  folder, without stashing or switching under another agent's feet;
- keep a long build or test run going on one branch while working on another;
- review or hotfix a branch without disturbing the current work.

Single-repo clients treat worktrees as an afterthought (a list in a menu).
Archipel's twist is the multi-repo one: an agent run that spans an app and its
libs needs **the same branch, checked out as a worktree in several repos, side
by side in one folder**. That is a **Worktree Set** (§5), and it is the main
reason for this feature.

### What exists today

- The folder scan finds linked worktrees (`.git` file) and adds each one as an
  ordinary, unrelated repo of the project; nothing tells it is a worktree of
  another repo.
- Reads and the watcher already handle a linked worktree's split Git
  directories (`git_dir` vs `common_dir`).
- No way to list, create, open, lock, prune or remove worktrees from the app.
  Checking out a branch that is checked out in another worktree fails with
  Git's raw error.

## 2. Principles

1. **A worktree belongs to its repo.** It is shown under the repo it is a
   worktree of, never as an unrelated repo. The repo's **main worktree** (the
   original checkout) stays the repo's row; linked worktrees are its children.
2. **Worktrees are personal and local.** Their paths are machine-specific and
   they come and go: they are discovered from Git (`git worktree list
   --porcelain`), **never written to `archipel.toml`**. Only *how* to make them
   (location template, files to copy) may be shared (§8).
3. **A worktree is a full working copy.** Everything the app does on a repo —
   dashboard columns, Quick Edit Pane, Repo View, staging, commit, forge
   status, conflict forecast, undo — works the same on a worktree.
4. **Never lose work.** Removing a worktree with uncommitted changes, unpushed
   commits or an operation in progress says exactly what would be lost, and is
   undoable (§6.4).
5. **Turn Git's refusals into choices.** "Branch is already checked out at …"
   becomes "Open that worktree" (§7).

## 3. Model

| Concept | Definition |
| ------- | ---------- |
| **Main Worktree** | The repo's own checkout (the folder in `archipel.toml`). For a bare repo there is none: the repo row shows only its linked worktrees. |
| **Linked Worktree** | Any other worktree of the repo, as `git worktree list` reports it. Identified by its path; labelled by its branch (or `detached @ abc1234`). |
| **Worktree State** | On top of the usual repo state: **locked** (with the lock reason), **missing** (its folder is gone: prunable), **managed** (created by Archipel under the location template, §8). |
| **Worktree Set** | A name plus one linked worktree per repo, for several repos of the project, all on the same branch, under one **Set Folder** (§5). Personal state. |

A repo listed in `archipel.toml` whose path turns out to be a linked worktree
of another listed repo is shown nested under that repo (once, not twice). If
its main worktree is not in the project, it stays a top-level row with a
**Worktree Glyph** and "worktree of `<main folder name>`" as its path hint.

## 4. Where worktrees appear

### 4.1 Sidebar

```text
REPOSITORIES
v libs (2)
    * libfoo        +2   ⧉2      <- Worktree Count, click expands
    |-  ⧉ feat/parser  *         <- Sidebar Worktree Row
    '-  ⧉ hotfix/1.2   o  🔒
    o libbar
```

- A **Worktree Count** (`⧉n`) on a Sidebar Repo Row that has linked
  worktrees; clicking it (or `→` on the row) expands the **Sidebar Worktree
  Rows**: Worktree Glyph, branch, Status Dot, Sync Badge, lock icon, or
  "missing" dimmed. Collapsed by default; expanded state is personal.
- Click selects the worktree like a repo (Quick Edit Pane on the Dashboard);
  double-click opens its Repo Tab.

### 4.2 Dashboard

```text
Repo                 Branch        Sync  Changes  Version  Fetch
libfoo        ⧉2 v   main          +2    3 M      v1.2.0   2m ago
 |- ⧉ libfoo         feat/parser   +5    12 M              2m ago
 '- ⧉ libfoo  🔒     hotfix/1.2    ok                      2m ago
libbar               dev           ok                      1h ago
```

- **Worktree Rows** under the repo row, indented with connectors like the
  Dependency Tree, collapsed behind the Worktree Count; the Repo cell shows the
  worktree's folder path on hover. Every column works per worktree, except
  **Fetch**, shared by the repo (refs are shared).
- Sorting keeps worktree rows under their repo. Filters apply to worktree rows
  too: a repo row stays visible (dimmed) when only one of its worktrees
  matches, so the match keeps its context.
- New Filter Chip **Worktrees**: only repos with linked worktrees, expanded.
- View Chip **Worktrees: all ▾** — the **Set Selector** (§5.3).
- Click, Ctrl+click, double-click and the Quick Edit Pane behave as on a repo
  row; the Quick Edit Header shows the Worktree Glyph, the branch and the
  folder path (click copies it).
- Batch actions take worktree rows as targets (pull, push, stash, commit,
  check out — see §7 for checkout).

### 4.3 Tabs

- A worktree opens in its own **Repo Tab**, labelled `libfoo · feat/parser`
  with the Worktree Glyph; tab hover shows the folder path.
- Opening a worktree that already has a tab focuses it.

### 4.4 Repo View

- **WORKTREES** section in the Refs Panel, after LOCAL, collapsible, listing
  every worktree of the repo (the main one first, the current one marked):
  branch, Status Dot, short path, lock / missing state. Double-click opens its
  Repo Tab; right-click opens the Worktree Menu (§6.5).
- In LOCAL, a branch checked out in **another** worktree carries a small
  Worktree Glyph; hover says where. Its Ref Badges in the graph carry the same
  glyph.
- The Repo Toolbar of a linked worktree shows a **Worktree Chip**
  (`⧉ worktree of libfoo`) whose menu jumps to the other worktrees.
- The graph is the repo's graph (shared refs); only the current worktree's WIP
  Row is drawn. Optional (Graph Header menu, off by default): a dimmed
  **Worktree HEAD Marker** on the commits other worktrees have checked out.

### 4.5 Palette

- **Worktree Switcher** (`Ctrl+Shift+O`): fuzzy list of every worktree of the
  project (`repo · branch · path`), open tabs first; Enter opens its Repo Tab.
- Action Palette entries: *New worktree…*, *New worktree set…*, *Remove
  worktree…*, *Prune missing worktrees*, *Lock / Unlock worktree*, *Reveal
  worktree folder*, *Copy worktree path*, *Finish worktree set…*.

## 5. Worktree Sets (multi-repo)

The flow for one agent run across several repos:

1. **New worktree set…** — choose repos, branch, base → one folder holding a
   worktree of each repo on that branch.
2. Point the agent (or yourself) at the Set Folder.
3. Watch the set on the Dashboard; review and commit across the set with the
   Commit Panel; open PRs across the set with the Forge Panel.
4. **Finish worktree set…** — remove the worktrees, delete merged branches.

Several sets can live at once (one per agent).

### 5.1 New Worktree Set dialog

Opened from the Action Palette, the Row Menu on a multi-selection, or the Set
Selector's "New set…".

```text
┌ New worktree set ───────────────────────────────────────────┐
│ Name / branch   [agent/parser-rework________________]        │
│ Folder          [~/work/myproject.worktrees/agent-parser…] […]│
│                                                              │
│ Repos                         Base                 Result    │
│ [x] app          main  v  (default branch)   new branch      │
│ [x] libfoo       main  v                     new branch      │
│ [x] libbar       dev   v  ⚠ branch exists → use it           │
│ [ ] plugin-x                                                 │
│                                                              │
│ [x] Init submodules   [x] Copy local files (.env, …)         │
│                                 [Cancel]  [Create 3 worktrees]│
└──────────────────────────────────────────────────────────────┘
```

- **Name** is also the branch name (editable separately under "Branch name
  differs" if needed); validated as a branch name live, per repo.
- **Folder** prefilled from the location template (§8); each repo's worktree
  goes to `<Set Folder>/<repo name>` (alias used if set), so the Set Folder
  mirrors the project's layout and an agent can be pointed at it.
- **Repos**: pre-checked = the selected rows, or all visible repos when none.
- **Base** per repo: the default branch by default, or the current branch, or
  any branch / tag / commit (dropdown with fuzzy search). A repo where the
  branch already exists uses it ("use existing"), unless it is checked out in
  another worktree: then that repo row is disabled with the reason and an
  "Open it" link.
- **Init submodules**: checked by default when the repo has submodules.
- **Copy local files**: copies the untracked, ignored files matching the
  repo's copy patterns (§8) — typically `.env` — from the main worktree.
- **Create** runs one creation per repo in parallel (Operations Drawer
  progress); partial failure is reported per repo with **Retry failed**, like
  the Commit Panel. No "all or nothing".
- After creation: the set becomes the active Set Selector choice, and a
  Status Bar notice offers **Copy folder path** and **Reveal folder**.

### 5.2 Set Folder

- A plain folder holding the worktrees, plus nothing else written by Archipel
  (no hidden metadata file: sets are remembered in personal state, keyed by
  the worktree paths).
- A set whose worktrees were all removed outside Archipel disappears. A set
  with some worktrees missing shows them as missing (prune / recreate).
- Launching `archipel <Set Folder>` opens the project that owns those
  worktrees with that set active (not a new auto-scanned project).

### 5.3 Set Selector

View Chip in the Dashboard Toolbar, label `Worktrees: all ▾`:

- **All** (default): repos with their worktrees nested (§4.2).
- **Main only**: hides linked worktrees.
- **`<set name>`** (one entry per set): the Dashboard shows, for each repo of
  the set, **its worktree of the set in place of the repo row** (flat, same
  columns); repos outside the set are hidden, toggle "Show other repos"
  shows them dimmed. The Project Band gets a striped pattern and the window
  title becomes `myproject [agent/parser-rework] — Archipel`, so it is obvious
  which checkouts batch actions act on.
- **New set…**, **Manage sets…** (rename, finish, forget).

In a set view, every batch action, the Commit Panel (`Ctrl+K`, set worktrees
pre-checked) and the Forge Panel's **New pull request** (one PR per repo for
the set's branch, linked together as in SPEC §8) target the set's worktrees.
The choice is per window and personal.

P2: **Open set in new window** — a second window on the same project, locked to
that set, so two agent runs can be followed side by side.

### 5.4 Finish Worktree Set

Dialog listing each worktree of the set with what removing it would lose:

| Per worktree | Shown | Default |
| ------------ | ----- | ------- |
| clean, branch merged into its PR target or pushed | "safe" | remove, delete local branch if merged |
| uncommitted changes | "n files changed" (expandable) | not checked |
| commits not pushed anywhere | "n unpushed commits" | not checked |
| operation in progress | "rebase in progress" | not checkable |

**Finish** removes the checked worktrees (and branches), then the Set Folder if
it is empty. Undoable per worktree (§6.4).

## 6. Single-worktree actions

### 6.1 Entry points for "New worktree…"

- Graph Menu on a local or remote branch: **Check out in new worktree…**; on a
  commit or tag: **New worktree here…** (new branch, or detached).
- Refs Panel WORKTREES header **+**; Branch button menu in the Repo Toolbar.
- Row Menu (Dashboard, Sidebar): **New worktree…**.
- Drag a branch from LOCAL / REMOTE onto the WORKTREES header (Drop Menu:
  *Check out in new worktree*).
- Action Palette.

### 6.2 New Worktree dialog

A single-repo version of §5.1:

- **Branch**: existing local branch, remote branch (creates the tracking local
  branch), or new branch from a base (prefilled from the entry point), or
  **detached**.
- **Folder**: prefilled `<location template>/<branch slug>`, editable, with a
  folder picker; warns when not empty, refuses when it is inside another
  worktree of the same repo.
- **Init submodules**, **Copy local files**, **Open in a tab after creating**
  (default on).
- Enter creates; errors stay in the dialog.

### 6.3 Opening a worktree elsewhere

Worktree Menu and Repo Toolbar overflow: **Reveal folder**, **Copy path**,
**Open in terminal**, **Open in external editor** — using the user's settings
for terminal and editor. These are the hand-off points to an agent.

### 6.4 Remove worktree

- From the Worktree Menu, Sidebar / Dashboard Row Menu, a Repo Tab of the
  worktree (closes the tab), or `Delete` on a selected worktree row.
- **Clean worktree**: Confirm Dialog "Remove worktree `feat/parser` at
  `<path>`?" with **Also delete branch** (checked only when the branch is
  merged into the default branch or fully pushed).
- **Dirty / unpushed / in progress**: the Confirm Dialog lists what would be
  lost (files, unpushed commits); **Remove anyway** needs a second click, as
  Discard does. An operation in progress must be aborted first (button in the
  dialog).
- **Undo** (Ctrl+Z, Operations Drawer): before removing, the working copy is
  snapshotted (as for discard) and the branch tip recorded; undo recreates the
  worktree at the same path on the same branch (recreating a deleted branch)
  and restores the snapshot. Refused, with the reason, when the path is now
  taken or the branch is checked out elsewhere.
- The main worktree cannot be removed (the entry is absent, not disabled).

### 6.5 Worktree Menu

Right-click on a worktree anywhere (Sidebar, Dashboard, Refs Panel WORKTREES,
Worktree Chip): Open · Reveal folder · Copy path · Open in terminal · Open in
external editor · Lock… (reason prompt) / Unlock · Move… (`git worktree move`;
refused for a worktree with submodules, as Git does) · Remove… · for a missing
one: Prune · Locate… (`git worktree repair` against a chosen folder).

### 6.6 Missing and locked worktrees

- **Missing** (folder deleted or on an unplugged drive): dimmed rows, "missing"
  instead of columns, never watched. **Prune missing worktrees** in the
  Action Palette and on the Worktree Count's hover; Prune skips locked ones
  (a locked worktree on removable media is the usual reason to lock).
- **Locked**: lock icon, reason on hover; Remove and Prune ask to unlock first.

## 7. Guard rails on existing actions

| Action | Today | With worktrees |
| ------ | ----- | -------------- |
| Check out branch B, B checked out in worktree W | raw Git error | Prompt: **Open W** (default) · Check out detached at B · Cancel |
| Batch checkout over repos | per-repo error | repos where B is in another worktree are skipped, listed as "in worktree …" in the report |
| Delete branch B checked out in W | raw Git error | refused, with **Remove worktree W…** |
| Rename branch checked out in another worktree | — | allowed (Git updates it); the other worktree's tab and rows update |
| Rebase / reset / merge on a branch checked out elsewhere (drag & drop) | — | the Drop Menu entry is disabled with "checked out in W — open it there" |
| Branches Panel cleanup | — | branches checked out in a worktree are never pre-checked; shown with the Worktree Glyph |
| Undo of a ref move that touches a branch checked out in another worktree | — | refused, with the reason (it would change that worktree's HEAD under it) |

## 8. Settings and project file

| Setting | Where | Default |
| ------- | ----- | ------- |
| `worktree_location` — template for new worktrees; `{project}`, `{repo}`, `{branch}`, `{set}` placeholders, relative to the repo's main worktree or absolute | personal settings | `../{repo}.worktrees/{branch}`; sets: `../{project}.worktrees/{set}/{repo}` relative to the project file folder |
| `worktree_copy` — glob patterns of untracked or ignored files copied from the main worktree on creation | per repo in `archipel.toml` (`copy_to_worktrees = [".env", "config/local.toml"]`): relative `/` patterns, so it stays cross-platform | none |
| `worktree_auto_expand` — expand Worktree Rows when a new worktree appears | personal | on |

No shell command runs on creation (`archipel.toml` holds no shell commands,
SPEC §1); setup steps belong to the agent or the user.

Personal state (user data dir) keeps: Worktree Sets, which worktree groups are
expanded, the Set Selector choice per window.

## 9. Refresh, performance, memory

- The worktree list of a repo is read on load and whenever the watcher sees
  `<common_dir>/worktrees/` change: worktrees created or removed from a
  terminal or by an agent appear and disappear live.
- A linked worktree is watched and refreshed like a repo **only while it is
  visible** (expanded, in the active set, or open in a tab); otherwise it keeps
  a cached summary refreshed when the window regains focus. Hidden repos hide
  their worktrees.
- Background fetch fetches **once per repo**, not per worktree; all worktrees'
  Sync columns update from it. The conflict forecast and the forge status are
  computed per worktree branch.
- RAM: a collapsed worktree costs one summary line; the targets of SPEC
  (< 150 MB idle, 20 repos) must hold with 3 worktrees per repo collapsed.

## 10. Keyboard

| Key | Action |
| --- | ------ |
| `Ctrl+Shift+O` | Worktree Switcher |
| `→` / `←` on a repo row | expand / collapse its worktrees |
| `Delete` on a worktree row | Remove worktree… |
| (none, rebindable) | New worktree…, New worktree set… |

## 11. Priorities

- **P1 — see and use**: discovery and nesting (§3, §4.1–4.4), Worktree
  Switcher, Worktree Menu (open, reveal, copy path, lock, prune), guard rails
  (§7), refresh rules (§9).
- **P1 — create and remove**: New Worktree dialog, Remove with undo, location
  template, copy patterns.
- **P1 — sets**: New Worktree Set, Set Selector, set-scoped Commit Panel and
  batch actions, Finish Worktree Set.
- **P2**: Open set in new window, Worktree HEAD Marker, Move / Locate,
  set-scoped New pull request, launching on a Set Folder.

## 12. Open questions

1. Should the **Set Folder** carry a generated `archipel.toml` so an agent's
   own tools (or another Archipel) see the set as a project? It would sit
   outside the repos, so it pollutes no repo, but it conflicts with
   "worktrees are never in `archipel.toml`"; left out for now.
2. Should creating a set also **stash-and-carry** uncommitted changes of the
   main worktrees (`git worktree add` + apply), for "move my current work to a
   worktree and let the agent continue"? Useful, but surprising as a default:
   a separate action *Move changes to new worktree…* is the likely answer.
3. `Ctrl+Shift+O` is free today; confirm it does not clash with GPUI text
   inputs.
