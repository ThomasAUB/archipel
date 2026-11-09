# Spec — Archipel, a multi-repo Git client

This spec defines **what** the product is and **why**. The implementation path (architecture, phases, measurements) lives in [PLAN.md](PLAN.md), which refers to the feature sections below as §1–§9.

## Vision

A Git client that is **fast, light on RAM and good-looking**, designed for working on **several repos at once**, grouped into *projects*. GitKraken, Fork and SourceTree are centered on a single repo; Archipel is centered on the project: see the state of all its repos at a glance and act on them in batch.

**A project is a place, not a list.** Each project opens as its own dashboard, shaped to the work it holds: which repos show and how they are grouped, which columns and panels appear, its color, icon and window title. Switching projects should feel like walking into a different room, not re-filtering a repo list. That identity belongs to the project definition (`archipel.toml`), so the team shares it; purely personal view state (collapsed groups, layout) stays private.

**Built for the AI coding workflow.** A coding agent works across repo boundaries: in a single run it can change an app, the libs it depends on and their plugins all at once. Single-repo clients make you go through those edits one repo at a time. Archipel shows **all the edits of a run together**: the dashboard (§3) shows which repos changed, the multi-repo commit (§4) lets you review the staged files of every touched repo, and you commit the whole change set in one pass. Reviewing and landing what the agent did across the project is a first-class flow, not a chore repeated per repo.

**Target user:** a developer juggling N related repos (app + libs + plugins, microservices, a product split across repos, …), often with an AI coding agent editing several of them at once.

## Non-goals (v1)

- No hosting, no built-in PR review (v1: links to GitHub/GitLab only; deeper forge integration is §8, P2).
- No code editor or advanced conflict resolution (open the external merge tool instead).
- No web / in-browser WASM version: a browser has no filesystem access and cannot run `git`.

## Features

Priority: **P0** = MVP, **P1** = v1, **P2** = later.

### 1. Projects (groups of repos) — P0
- A project = a name + a list of local repos.
- Add repos by manual selection **or** by scanning a parent folder (automatic `.git` detection) **or** by cloning from a URL (several at once, any URL `git clone` accepts, no forge account needed).
- The definition is stored in a readable file (`archipel.toml`), so it can be versioned and shared with the team.
- P2: import from submodules, `repo` manifest, VS Code workspace.

**The project file is strictly cross-platform — P0**
> Implemented except: a repo on another Windows drive is still written as an absolute path, and names are not yet checked for portability.

An `archipel.toml` written on any OS opens unchanged on Windows, Linux and macOS, and saving it from another OS produces no spurious diff.
- UTF-8 TOML; written with LF line endings and no BOM, read with either line ending and with or without a BOM.
- Repo paths are relative to the file's folder and written with `/`; `\` is accepted on read. An absolute path is never written: a repo with no relative path from the file (e.g. another Windows drive) cannot be added to a shared project file, and the user is told why.
- Paths and names must be valid on every OS: no characters or names Windows forbids (`<>:"|?*`, `CON`, `NUL`…, trailing dot or space), and no two entries that differ only by case. Such an entry is refused when added, and reported (not silently dropped) when loading a file written by hand.
- Nothing OS-specific is stored: no drive letters, environment variables, shell commands or platform-only settings. Anything machine-specific belongs in the personal state kept in the user data dir.
- Output is deterministic: same key order, repo order and formatting on every OS, so the file only changes when the project does.

**Launch from a directory — P1**
> Implemented: `archipel <dir>`. Not yet: the file-manager "Open with Archipel" entry (needs the installer, see Packaging in the README).

- `archipel <dir>` (or `archipel .`, or "Open with Archipel" from the file manager) opens the app on that directory.
- If the directory holds an `archipel.toml`, that project is opened.
- Otherwise the repos around are detected automatically and added to the list: the directory is scanned downward (same depth-limited scan as "scan a parent folder"); if it is itself inside a repo, the scan starts from the outermost enclosing repo root.
- The result is written as an `archipel.toml` at the scanned folder (the directory, or the outermost enclosing repo root); launching again from the same directory opens that file.
- No confirmation dialog: the list appears immediately and fills in as the scan progresses (cancellable).

**Hidden repos — P1**

- Any repo of a project can be hidden when it isn't wanted (context menu or keyboard), individually or in batch.
- Hidden repos disappear from the dashboard, graph switcher, batch actions and multi-repo commit; they are neither watched nor refreshed (saves CPU and RAM).
- A "show hidden repos" toggle lists them, dimmed, with an "unhide" action.
- Hiding is a personal choice: stored in the personal state in the user data dir, never in the shared `archipel.toml`. Removing a repo from the project stays a separate, explicit action.

**Repo categories — P1**

- Any repo can be given a category (a free-form name: "apps", "libs", "plugins", …), individually or in batch, from the repo context menu or the action palette; picking an existing category or typing a new one, or clearing it.
- The sidebar groups repos by category under collapsible headers (name + repo count), categories sorted by name, repos keeping project order inside each group; repos without a category come last under "Uncategorized". With no category at all, the sidebar stays a flat list.
- A category header's context menu renames the category (on every repo using it; an existing name merges into that category) or removes it (its repos become uncategorized, they stay in the project). A category exists only while a repo uses it.
- Categories describe the project, so they are stored on each repo entry of the project definition (`category = "libs"` in `archipel.toml`) and shared with the team. Which groups are collapsed is personal (user data dir).

### 2. Project switching — P0
- Fast keyboard switcher (`Ctrl+P` style), plus a recent-projects list.
- Each project keeps its own UI state (selected repo, filters, layout).
- Switch in < 200 ms (state cached, refreshed in the background).

**Several instances side by side — P1**

- Several projects / directories can be open at the same time, each in its own window, behaving as an independent instance (own selected repo, filters, layout, background refresh).
- **One process, many windows:** launching `archipel <dir>` while Archipel is running hands the request to the running process (local IPC), which opens a new window. An extra window costs only its project's data, not another GPU context, font set and caches — consistent with the RAM target.
- Opening a directory / project that is already open focuses its window instead of creating a duplicate.
- Shared state (settings, recent projects, on-disk caches) stays consistent when several windows write to it concurrently.

**Recognizing a window at a glance — P1**
> Implemented except the taskbar badge (it needs a native Windows API call, i.e. an `unsafe` exception and an ADR).

- **Per-project accent color:** derived automatically from the project path (stable across launches), overridable by the user. Shown as a thin colored band at the top of the window, and on the selection and project name; built on the theme engine's accent-derived palette. An optional per-project background tint, also shared through the project file, mixes a percentage of the accent's hue into the window's surfaces while keeping the theme's lightness (lifted slightly towards the text only when a very dark or very light background cannot hold the color), so it suits light and dark themes alike and every user keeps their own theme.
- **Project name first in the window title** (`myproject — Archipel`), so the taskbar, Alt+Tab and window lists stay readable when titles are truncated.
- **Taskbar badge:** a small dot in the project color with its initials, overlaid on the taskbar icon (native overlay on Windows; best effort on Linux), to tell grouped windows apart.
- **Optional project icon:** an emoji, or an image file (PNG, SVG…) kept relative to `archipel.toml`, chosen by the user, shown next to the name in the project switcher (and in the title bar for an emoji, since titles are text).
- Color and icon belong to the project: stored in the project definition, so `archipel.toml` shares them with the team (a new file gets the automatic color written in it).

### 3. Project dashboard — P0
A minimal view, one row per repo:

| Column | Content                                            |
| ------ | -------------------------------------------------- |
| Repo   | name + path                                        |
| Branch | current branch (or detached HEAD)                  |
| Sync   | ahead / behind upstream                            |
| Changes| modified / staged / untracked file counts, stashes |
| Version| tag when HEAD is exactly on one, else the branch (short commit if detached) |
| Fetch  | time of last fetch                                 |

- Live updates through a file watcher (no expensive polling).
- Filters: "dirty repos only", "behind remote", …
- Category filter (once the project has categories): a single dropdown button in the toolbar, so many categories do not crowd it. Its label sums up the selection ("Category: all", "Category: libs", "Category: libs +2"); its menu is a checklist of the categories, plus "Uncategorized" when some repo has none, and "Show all categories" to clear the selection. The menu stays open while ticking, so several can be selected; selecting several shows the repos of any of them; it combines with the other filters (AND). The selection is personal UI state and follows a category rename (a removed category's selection becomes "Uncategorized").
- Quick edit: a click on a repo opens a pane beside the rows for the everyday edits, without opening the repo tab: fetch / pull / push (a branch without upstream gets one on the default remote), stash / pop, undo / redo, create or switch branch, stage / unstage / discard files, commit (amend, commit & push), continue / abort a merge, rebase, cherry-pick or revert in progress.
- Batch actions: **fetch all**, pull all, check out the same branch on every repo, create a branch/tag on several repos.
- Background fetch (`auto_fetch_secs`, default 10 min, 0 = off): one repo at a time, spread over the interval, open and selected repos first. It never gets in the way: skipped when the repo is busy, cancelled when the user writes to it, absent from the operations drawer. A repo whose fetches bring nothing is fetched less and less often (up to 8× the interval), a failing one waits the longest; a manual fetch resets the repos it covers. A fetch that moves no ref only updates the "last fetch" time.
- Update check (`update_check_hours`, default 24, 0 = off): 30 s after the first window opens, then at that interval, the latest GitHub release of Archipel is compared with the running version; a newer one shows an "↑ Update x.y.z" badge in the status bar that opens the release page, × hides it until the next release. Failures are silent (debug log) and retried after an hour. Builds updated by a package manager are compiled with `ARCHIPEL_NO_UPDATE_CHECK` set: no check at all.

### 4. Multi-repo commit — P0
- Pick the repos to commit (pre-checked: those with changes).
- One message applied everywhere; preview of the staged files per repo.
- Partial-failure handling: if a hook fails on one repo, show a clear report (committed / failed) and allow a retry — no fake "all or nothing".
- Options: push after commit, ticket prefix (e.g. `JIRA-123`), message templates.

### 5. Repo view + graph — P0
- Commit graph, diff, staging per file / hunk / line.
- File browser: **Files** in the toolbar swaps the details pane for the repo's file tree, at the selected commit or stash, or in the working copy (uncommitted-changes row selected, or nothing selected); it follows the selection. A filter box narrows it to the files matching fuzzily; ↑↓, Page Up / Down, Home / End move, → / ← open or close a folder, Enter opens, Esc closes. A file opens read-only in place of the graph, highlighted and searchable; a working-copy file goes to the quick file edit on **Edit** or a double-click, at that line, and shows again, reloaded, once saved or cancelled. Binary files and files over 4 MB are not shown; submodules are listed, not opened.
- Quick file edit from a working-copy diff (repo tab and dashboard quick edit pane): **Edit**, or a double-click on a line, swaps the diff for a code editor on the file, at that line. Ctrl+S saves and goes back to the reloaded diff; Esc goes back without saving (a second Esc when there are unsaved changes). A save is refused, the editor staying open, when the file changed on disk since it was opened. Line endings (CRLF) are kept; binary, non-UTF-8 and very large files are not edited there.
- Ignore from the staging panel: an unstaged file's ignore button opens a dialog proposing patterns built from it, narrowest first: the file itself, every file of that name, every file of its extension, those directly in its folder, then each of its folders (an untracked folder: itself, every folder of that name, its parents). Picking one fills an editable pattern field; **Add to .gitignore** (or Enter) appends it as a line of the `.gitignore` at the repo root, created when missing, in the file's line ending, unless it is already there. Names are escaped so they match literally; a tracked file gets a note that Git keeps tracking it.
- Virtualized graph: smooth even with 100k+ commits.
- Everyday repo actions: commit / amend, branches (checkout, create, rename, delete, set upstream), tags, stashes, fetch / pull / push, reset, revert, continue / abort a merge or rebase in progress.
- Checkout keeps uncommitted changes: when Git refuses because they touch files that differ on the target, they are stashed, the target is checked out and they are re-applied there (staged state kept). If they conflict with the target, the target stays clean, the changes stay in an `archipel: autostash` stash entry and the status line says so; undoing the checkout switches back and pops them. Batch checkout over many repos does not autostash.

### 6. Branch drag and drop — P1
Always confirmed through a menu, never a silent action:
- branch A → branch B: *merge A into B* / *rebase A onto B* / *fast-forward*
- local branch → remote: *push*
- commit → branch: *cherry-pick*
- **Undo** for every local operation; a push gets a confirmed compensating action instead (revert or force-with-lease). Undo is refused, with an explanation, when it would lose work. Details: [PLAN.md](PLAN.md) §2.4.

### 7. Statistics — P1
> In the desktop app so far: the Branches Panel (merged-not-deleted and stale branches across repos, batch delete). Activity and hotspots are computed (`archipel-cli stats`) but not shown yet.

Computed locally, per repo and aggregated per project. Three views in v1:
- activity over time (commits / week) per author;
- branch age, and merged branches that were never deleted;
- file hotspots by churn (lines added / removed).

Anything else waits for real usage.

### 8. Forge integration (GitHub, GitLab, …) — P2
> Implemented for GitHub (and GitHub Enterprise Server) and GitLab (gitlab.com and self-hosted): sign-in by personal access token, "Pull request" dashboard column, "CI" dashboard column (latest run of the current branch, jobs on hover), pull requests across repos, open pull requests listed per repo and merged from Archipel, clone from an account (ADR 0003). Not yet: OAuth device flow (it needs an OAuth application registered on each forge), Gitea / Forgejo, Bitbucket and Azure DevOps providers (links only).

Easy, low-friction integration with code hosting forges:
- Providers: GitHub, GitLab (including self-hosted), then Gitea / Forgejo, Bitbucket, Azure DevOps — each as a pluggable provider behind one small trait, so a fork can add its own without patching the core.
- Setup in a few clicks: provider auto-detected from the remote URL; sign-in by OAuth device flow or personal access token, stored in the OS keychain (never in `archipel.toml`).
- Clone from an account / organization / group: browse and pick repos, clone several at once straight into a project.
- Per-repo forge status in the dashboard: open PR/MR for the current branch, its CI status, review state.
- CI status of the current branch in the dashboard, with or without a PR: overall state and job counts in a "CI" column, every job (failures first) on hover, "Open CI run" from the row menu.
- Create a PR/MR from a branch (title, description, target branch prefilled); multi-repo: one PR per repo for the same branch, linked together.
- List the open PRs/MRs of the selected repos (source → target, author, CI, review, mergeability) and merge one: merge commit, squash or rebase (GitHub; GitLab uses the project's method), optionally deleting the source branch. A second click confirms; the forge refuses if the head moved since the list was read. The repo is fetched afterwards.
- Still no built-in code review (see Non-goals): reviewing happens on the forge.

### 9. Repo dependency tree — P2

Show how the repos of a project depend on each other, as a small tree:
- Nested submodules at any depth (repo → submodule → submodule → …), with each node's state (initialized or not, pinned commit vs. checked-out commit, dirty).
- Repos of the project that are themselves submodules of another project repo are shown under their parent rather than twice.
- **Toggleable:** off by default, a single toggle shows it in the dashboard / sidebar, so the flat view stays uncluttered when nesting doesn't matter. The choice is per project UI state.
- Compact rendering (indentation + connectors), collapsible nodes; works with the dashboard filters.
- Actions from a node: init / update a submodule (recursive or not), open it as a repo, add it to the project.
- Later: other dependency sources through the same extension point (e.g. `repo` manifests, package-manager path / git dependencies).

## Non-functional requirements

- **Performance:** start < 1 s; dashboard of a 20-repo project < 500 ms; no Git operation ever blocks the UI.
- **Memory (RAM):** a small footprint, unlike Electron clients (GitKraken often reaches 500 MB – 1 GB).
  - Targets: < 150 MB idle with a 20-repo project open; no steady growth over a long session (no leaks).
  - Lazy loading: graph and history loaded per visible window (paged / streamed log); the full history is never held in memory.
  - Inactive projects are unloaded (only a light dashboard summary stays cached).
  - Bounded caches (max size, LRU eviction) for diffs, blobs and stats; stats are computed as a stream and persisted to disk rather than kept in RAM.
- **Aesthetics:** light/dark themes, careful typography, restrained animations, adjustable information density.
- **Themes customizable by a regular user:** no code or file editing required — built-in theme editor with live preview, a full theme derived from a few base colors (background, accent), adjustable fonts / size / density / corner radius, import / export of a readable theme file for sharing.
- **Compatibility:** respects the user's Git config (credentials, SSH, GPG/SSH signing, hooks, LFS).
- **Platforms:** Windows, Linux and macOS (all three in CI); macOS packaging later.
- **Code quality and architecture:** a first-class goal. Code split into single-responsibility modules with interface segregation (small role-based traits); the project must be easy to **customize by anyone who forks it** (extension points, swappable implementations, no core patching needed). Rules: [ARCHITECTURE.md](ARCHITECTURE.md).

## Stack

- **UI:** GPUI (+ `gpui-component`), to be confirmed in Phase 0 by measuring it against a Tauri spike; egui as the fallback. A native UI is preferred because a WebView alone (e.g. WebView2) costs roughly 80–150 MB, which conflicts with the RAM target. Rationale: [PLAN.md](PLAN.md) §2.2.
- **Hybrid Git backend:**
  - **reads** (status, log, graph) through `gitoxide` (`gix`) → fast, parallel across N repos;
  - **writes** (commit, push, rebase) through the `git` CLI → guaranteed compatibility with hooks, credentials and signing.

## Resolved questions

- **GitHub / GitLab integration:** out of v1. Only "open in browser" links (repo, branch, commit, create PR). Full integration is §8 (P2).
- **Submodules:** not added automatically; detected (badge) with an "add submodules to project" action (P1). Nested display as a toggleable tree is §9 (P2).
- **Project file:** a project definition always lives in an `archipel.toml` at a folder root, never in the user data dir. The user data dir only keeps the registry of known projects, personal state (UI state, hidden repos) and app data (caches, undo journal, stats); none of it goes in the shared file.
- **Multi-repo commit:** the same branch is not required; warning when branches differ, plus a per-project "require same branch" option.
- **Distribution:** open source (MIT), signed installers, no telemetry.
- **Adding a repo by cloning** (URL): P1.
