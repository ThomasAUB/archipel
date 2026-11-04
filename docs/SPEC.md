# Spec — Archipel, a multi-repo Git client

This spec defines **what** the product is and **why**. The implementation path (architecture, phases, measurements) lives in [PLAN.md](PLAN.md), which refers to the feature sections below as §1–§9.

## Vision

A Git client that is **fast, light on RAM and good-looking**, designed for working on **several repos at once**, grouped into *projects*. GitKraken, Fork and SourceTree are centered on a single repo; Archipel is centered on the project: see the state of all its repos at a glance and act on them in batch.

**Target user:** a developer juggling N related repos (app + libs + plugins, microservices, a product split across repos, …).

## Non-goals (v1)

- No hosting, no built-in PR review (v1: links to GitHub/GitLab only; deeper forge integration is §8, P2).
- No code editor or advanced conflict resolution (open the external merge tool instead).
- No web / in-browser WASM version: a browser has no filesystem access and cannot run `git`.

## Features

Priority: **P0** = MVP, **P1** = v1, **P2** = later.

### 1. Projects (groups of repos) — P0
- A project = a name + a list of local repos.
- Add repos by manual selection **or** by scanning a parent folder (automatic `.git` detection).
- The definition is stored in a readable file (`.gitproject.toml`), so it can be versioned and shared with the team.
- P2: import from submodules, `repo` manifest, VS Code workspace.

**Launch from a directory — P1**
> **Not in the current version.** Today folder scanning exists in the CLI (`archipel-cli scan` / `add`); the desktop app does not take a directory at launch.

- `archipel <dir>` (or `archipel .`, or "Open with Archipel" from the file manager) opens the app on that directory.
- If the directory holds a `.gitproject.toml`, that project is opened.
- Otherwise the repos around are detected automatically and added to the list: the directory is scanned downward (same depth-limited scan as "scan a parent folder"); if it is itself inside a repo, the scan starts from the outermost enclosing repo root.
- The result is a private project tied to that directory (in the user data dir); launching again from the same directory reopens it and adds any newly found repos — it never re-adds hidden ones.
- No confirmation dialog: the list appears immediately and fills in as the scan progresses (cancellable).

**Hidden repos — P1**
> **Not in the current version.**

- Any repo of a project can be hidden when it isn't wanted (context menu or keyboard), individually or in batch.
- Hidden repos disappear from the dashboard, graph switcher, batch actions and multi-repo commit; they are neither watched nor refreshed (saves CPU and RAM).
- A "show hidden repos" toggle lists them, dimmed, with an "unhide" action.
- Hiding is a personal choice: stored in the private project state, never in the shared `.gitproject.toml`. Removing a repo from the project stays a separate, explicit action.

### 2. Project switching — P0
- Fast keyboard switcher (`Ctrl+P` style), plus a recent-projects list.
- Each project keeps its own UI state (selected repo, filters, layout).
- Switch in < 200 ms (state cached, refreshed in the background).

### 3. Project dashboard — P0
A minimal view, one row per repo:

| Column | Content                                            |
| ------ | -------------------------------------------------- |
| Repo   | name + path                                        |
| Branch | current branch (or detached HEAD)                  |
| Sync   | ahead / behind upstream                            |
| Changes| modified / staged / untracked file counts, stashes |
| Version| latest tag + `git describe`                        |
| Fetch  | time of last fetch                                 |

- Live updates through a file watcher (no expensive polling).
- Filters: "dirty repos only", "behind remote", …
- Batch actions: **fetch all**, pull all, check out the same branch on every repo, create a branch/tag on several repos.

### 4. Multi-repo commit — P0
- Pick the repos to commit (pre-checked: those with changes).
- One message applied everywhere; preview of the staged files per repo.
- Partial-failure handling: if a hook fails on one repo, show a clear report (committed / failed) and allow a retry — no fake "all or nothing".
- Options: push after commit, ticket prefix (e.g. `JIRA-123`), message templates.

### 5. Repo view + graph — P0
- Commit graph, diff, staging per file / hunk / line.
- Virtualized graph: smooth even with 100k+ commits.
- Everyday repo actions: commit / amend, branches (checkout, create, rename, delete, set upstream), tags, stashes, fetch / pull / push, reset, revert, continue / abort a merge or rebase in progress.

### 6. Branch drag and drop — P1
Always confirmed through a menu, never a silent action:
- branch A → branch B: *merge A into B* / *rebase A onto B* / *fast-forward*
- local branch → remote: *push*
- commit → branch: *cherry-pick*
- **Undo** for every local operation; a push gets a confirmed compensating action instead (revert or force-with-lease). Undo is refused, with an explanation, when it would lose work. Details: [PLAN.md](PLAN.md) §2.4.

### 7. Statistics — P1
Computed locally, per repo and aggregated per project. Three views in v1:
- activity over time (commits / week) per author;
- branch age, and merged branches that were never deleted;
- file hotspots by churn (lines added / removed).

Anything else waits for real usage.

### 8. Forge integration (GitHub, GitLab, …) — P2
> **Not in the current version.** Today only "open in browser" links exist (see Resolved questions).

Easy, low-friction integration with code hosting forges:
- Providers: GitHub, GitLab (including self-hosted), then Gitea / Forgejo, Bitbucket, Azure DevOps — each as a pluggable provider behind one small trait, so a fork can add its own without patching the core.
- Setup in a few clicks: provider auto-detected from the remote URL; sign-in by OAuth device flow or personal access token, stored in the OS keychain (never in `.gitproject.toml`).
- Clone from an account / organization / group: browse and pick repos, clone several at once straight into a project.
- Per-repo forge status in the dashboard: open PR/MR for the current branch, its CI status, review state.
- Create a PR/MR from a branch (title, description, target branch prefilled); multi-repo: one PR per repo for the same branch, linked together.
- Still no built-in code review (see Non-goals): reviewing happens on the forge.

### 9. Repo dependency tree — P2
> **Not in the current version.** Today submodules are only detected (badge) — see Resolved questions.

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
- **Platforms:** Windows and Linux first, macOS later.
- **Code quality and architecture:** a first-class goal. Code split into single-responsibility modules with interface segregation (small role-based traits); the project must be easy to **customize by anyone who forks it** (extension points, swappable implementations, no core patching needed). Rules: [ARCHITECTURE.md](ARCHITECTURE.md).

## Stack

- **UI:** GPUI (+ `gpui-component`), to be confirmed in Phase 0 by measuring it against a Tauri spike; egui as the fallback. A native UI is preferred because a WebView alone (e.g. WebView2) costs roughly 80–150 MB, which conflicts with the RAM target. Rationale: [PLAN.md](PLAN.md) §2.2.
- **Hybrid Git backend:**
  - **reads** (status, log, graph) through `gitoxide` (`gix`) → fast, parallel across N repos;
  - **writes** (commit, push, rebase) through the `git` CLI → guaranteed compatibility with hooks, credentials and signing.

## Resolved questions

- **GitHub / GitLab integration:** out of v1. Only "open in browser" links (repo, branch, commit, create PR). Full integration is §8 (P2).
- **Submodules:** not added automatically; detected (badge) with an "add submodules to project" action (P1). Nested display as a toggleable tree is §9 (P2).
- **Project file:** both — a shareable `.gitproject.toml` at a folder root, or a private project in the user data dir. UI state never goes in the shared file.
- **Multi-repo commit:** the same branch is not required; warning when branches differ, plus a per-project "require same branch" option.
- **Distribution:** open source (MIT), signed installers, no telemetry.
- **Adding a repo by cloning** (URL): P1.
