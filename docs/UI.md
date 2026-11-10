# Archipel — UI vocabulary

The names of the windows, views, panels and parts of the interface, so that
specs, issues, commits and code reviews all use the same words.

Each name is **bold** where it is defined. Names are in Title Case when they
refer to the UI element ("the Quick Edit Pane"), and the code location says
where it is rendered. When a name in the code differs, the code name is given
too; prefer the names of this document in prose and in new code.

## 1. Overview

```text
┌──────────────────────────────────────────────────────────────────────────┐
│ Project Band (thin strip in the project's accent color)                  │
├──────────────┬───────────────────────────────────────────────────────────┤
│ SIDEBAR      │ TAB BAR   [Dashboard] [repo-a] [repo-b]                   │
│              ├───────────────────────────────────────────────────────────┤
│ Project      │                                                           │
│ Header       │ MAIN AREA                                                 │
│              │   the Dashboard        (Dashboard Tab active)             │
│ REPOSITORIES │   or a Repo View       (a Repo Tab active)                │
│   Repo List  │                                                           │
│              │                                                           │
│ Add / Clone  │                                                           │
├──────────────┴───────────────────────────────────────────────────────────┤
│ OPERATIONS DRAWER  (Ctrl+J, hidden by default)                           │
├──────────────────────────────────────────────────────────────────────────┤
│ Status Bar                                                               │
└──────────────────────────────────────────────────────────────────────────┘
  + Overlays on top: Palette, Commit Panel, Forge Panel, Branches Panel, Context Menus
```

| Name | What it is | Code |
| ---- | ---------- | ---- |
| **Window** | One project per window; several windows share one process. | [windows.rs](../crates/archipel-ui/src/windows.rs) |
| **Workspace** | Everything inside a window: the root view. | `Workspace` in [workspace.rs](../crates/archipel-ui/src/workspace.rs) |
| **Project Band** | Thin strip in the project's accent color at the very top. | `Workspace::render` (`BAND_HEIGHT`) |
| **Sidebar** | Left column: project and repos. | `Workspace::render_sidebar` |
| **Tab Bar** | The tabs above the Main Area. | `Workspace::render_tabs` |
| **Main Area** | What the active tab shows: the Dashboard or a Repo View. | `Workspace::render` |
| **Operations Drawer** | Log of operations, progress and errors (`Ctrl+J`). | `Workspace::render_drawer` |
| **Status Bar** | One line at the bottom: notice, warning or shortcut hints; click to dismiss a notice. At its right end, the **Project Color** swatch (with a project open): click for a color picker setting the project's accent color, as *Set the project color…* does. Then the **Theme Picker**: the current theme's name; click to pick another (Classic Dark, Classic Light, High Contrast, user themes). The choice is remembered across launches. | `Workspace::render_status` |
| **Overlay** | A panel drawn over the Workspace: Palette, Commit Panel, Forge Panel or Branches Panel. | `Overlay` enum |

## 2. Sidebar

```text
┌────────────────────────┐
│ PROJECT  (Ctrl+P)      │  <- Project Header
│ myproject              │
├────────────────────────┤
│ REPOSITORIES           │  <- Repo List
│ v libs (2)             │  <- Category Header
│     * libfoo      +2   │  <- Sidebar Repo Row
│     o libbar           │
│ v Uncategorized (1)    │
│     * app         -3   │
├────────────────────────┤
│[Add repos...][Clone...]│
└────────────────────────┘
  * / o  Status Dot (dirty / clean)    +2 / -3  Sync Badge (ahead / behind)
```

| Name | What it is |
| ---- | ---------- |
| **Project Header** | Current project name and icon; click opens the Project Switcher. Shows "No project" when none is open. |
| **Repo List** | The project's repositories, grouped by category when the project has categories (flat list otherwise). |
| **Category Header** | Collapsible group title with its repo count. Right-click opens the **Category Menu** (rename, remove). |
| **Sidebar Repo Row** | One repo: **Status Dot** (clean / dirty / …) and **Sync Badge** (`↑ahead ↓behind`). |
| **Add repos… / Clone…** | Add local repos (pick a repo or a folder to scan), or open the Forge Panel's **Clone** tab. |

## 3. Tab Bar

| Name | What it is |
| ---- | ---------- |
| **Dashboard Tab** | Always the first tab; shows the Dashboard. |
| **Repo Tab** | One per opened repo; shows its Repo View. `Ctrl+Tab` / `Ctrl+Shift+Tab` cycle tabs. Drag a repo tab onto another to reorder them (the Dashboard Tab stays first). |
| **Tab Menu** | Right-click on a tab: Close, Close others, Close tabs to the right, Close all. (`Workspace::render_tab_menu`) |

## 4. Dashboard

The project overview, one row per repo (spec §3). Rendered by
`Workspace::render_dashboard`; columns come from
[dashboard.rs](../crates/archipel-app/src/dashboard.rs).

```text
┌────────────────────────────────────────────────────┬───────────────────────┐
│ [Dirty] [Behind remote] [Category: all v] [Tree]   │ QUICK EDIT PANE       │  <- Dashboard Toolbar
├────────────────────────────────────────────────────┤                       │
│ Repo    Branch  Sync  Changes  Version  Fetch      │ libfoo  [Open] [x]    │  <- Column Header
├────────────────────────────────────────────────────┤ main  +2              │
│ libfoo  main    +2    3 M      v1.2.0   2m ago     │ ┌───────────────────┐ │  <- Dashboard Rows
│ libbar  dev     ok                      1h ago     │ │ Staging Panel     │ │
│ app     main    -3    1 S               5m ago     │ │                   │ │
│                                                    │ └───────────────────┘ │
└────────────────────────────────────────────────────┴───────────────────────┘
```

| Name | What it is |
| ---- | ---------- |
| **Welcome Screen** | Dashboard with no project open: "New project from a folder…", "Open an archipel.toml…". |
| **Dashboard Toolbar** | Above the rows: Filter Chips, Category Filter, View Chips, and the scan progress ("Scanning the folder… N found" + Stop). (`render_dashboard_toolbar`) |
| **Filter Chips** | Toggle filters (Dirty, Behind remote, Open pull request, CI failing, …); they combine with AND. |
| **Category Filter** | Dropdown button "Category: …" with a checklist of categories. (`render_category_filter`, `render_category_menu`) |
| **View Chips** | **Tree** (shows the Dependency Tree), **Branches…** (opens the Branches Panel) and **Hidden (N)** (shows hidden repos). |
| **Column Header** | Column titles; click sorts. Columns: Repo, Branch, Sync, Changes, Version, Fetch, then forge columns Pull request and CI. |
| **Dashboard Row** | One repo. Click: select it and open the Quick Edit Pane; Ctrl+click: add to the selection; double-click: open its Repo Tab. |
| **Dependency Tree** | Replaces the flat rows when Tree is on: repos and nested submodules with connectors, each **Tree Node** with its state and actions (init / update / open / add); a submodule that is a project repo keeps its dashboard columns and shows its init / update buttons on hover. (`render_tree`) |
| **Row Menu** | Right-click on a row: Open, Fetch, Hide / Unhide, Set category…, Remove from project, forge entries (pull request, CI run). (`render_row_menu`) |
| **Quick Edit Pane** | Resizable pane over the right edge of the rows, for everyday edits without the Repo Tab: a **Quick Edit Header** (repo name, branch, sync, Open, Close) above a Staging Panel. A click on the same row or Esc closes it. ([quick_edit.rs](../crates/archipel-ui/src/quick_edit.rs)) |
| **Quick Edit Diff** | A file clicked in the Quick Edit Pane shows its Diff View in place of the rows (toolbar and header included). |

## 5. Repo View

What a Repo Tab shows (spec §5). [repo_view.rs](../crates/archipel-ui/src/repo_view.rs).

```text
┌──────────────────────────────────────────────────────────────────────────┐
│ libfoo  CI ✓ 12  [Branch] [Fetch] [Pull] [Push] [PR #4] [Stash] [Undo] … │  <- Repo Toolbar, CI Badge, Repo Actions Bar
│ New branch at HEAD: [__________] [OK] [Cancel]                           │  <- Name Form
├────────────┬───────────────────────────────────┬─────────────────────────┤
│ REFS PANEL │ GRAPH PANE                        │ DETAILS PANE            │
│ LOCAL (2)  │ o Uncommitted changes (4)         │ Fix parser              │  <- WIP Row
│   main     │ | # stash@{0}                     │ Jane <jane@example.com> │  <- Stash Row
│   dev      │ * fix parser  [main]              │ 3f2a9c1...              │  <- Commit Row, Ref Badge
│ REMOTE (2) │ * add tests                       │ changed files...        │
│ TAGS (12)  │ |\                                │                         │
│ SUBMODULES │ * * ...                           │                         │
├────────────┴───────────────────────────────────┴─────────────────────────┤
│ Repo Status Line (click to dismiss)                                      │
└──────────────────────────────────────────────────────────────────────────┘
  The │ between panes are Splitters (drag to resize).
```

| Name | What it is |
| ---- | ---------- |
| **Repo Toolbar** | Top line, above the panes: the repo name, the **CI Badge** (the latest CI run of the current branch, as in the Dashboard's CI column; jobs on hover, click opens the run), and the **Repo Actions Bar** led by a Branch button. It stays put whatever is selected. (`render_toolbar`, `render_ci`, `render_repo_actions`) |
| **Name Form** | Inline form under the Repo Toolbar to name a new branch, a new tag, or rename a branch. (`NameForm`, `render_name_form`) |
| **Refs Panel** | Left pane, virtualized: sections **LOCAL**, **REMOTE**, **TAGS**, **SUBMODULES**, each collapsible. Branch rows can be dragged onto other branches. (`render_refs`) |
| **Graph Pane** | Centre pane: the virtualized **Commit Graph**, or an Open Diff. (`render_graph`) |
| **Graph Header** | Column titles above the Commit Graph (Message, Author, Age, Date, Commit). Right-click opens a menu ticking the shown optional columns; clicking one shows or hides it (saved per project). Age is the commit time relative to now; Date, the exact local date and time to the minute (`08/11/2026 14:32`, day and month in the system locale's order, `DateOrder`), is hidden by default. (`render_graph_header`, `GraphColumn`) |
| **WIP Row** | First row of the graph, "uncommitted changes", linked to HEAD by its own lane. Selecting it shows the Staging Panel in the Details Pane. |
| **Stash Row** | A stash, drawn just above the commit it was made on. |
| **Commit Row** | A commit with its lanes, **Ref Badges** (branches, tags — draggable) and metadata. Click: select; Ctrl+click: add to / remove from the selection; Shift+click: select the rows from the selected one; Esc: back to one. |
| **Details Pane** | Right pane, follows the selection: the **Commit Details** (summary, author, id, changed files) or **Stash Details** (message, base commit, Apply / Pop / Drop); for the WIP Row, the Staging Panel without its Repo Actions Bar (it is in the Repo Toolbar). (`render_details`) |
| **Open Diff** | A file clicked in the Details Pane opens its Diff View in place of the Commit Graph; Esc goes back. Either one file or all files of a commit / stash. (`OpenDiff`) |
| **Splitter** | Draggable separator resizing the Refs Panel and the Details Pane (widths saved per project). (`widgets::splitter`, `Pane`) |
| **Graph Menu** | Right-click on a commit, a multi-selection of commits (Squash, Copy commit ids), ref, stash, submodule or the Graph Header: checkout, branch, tag, merge, rebase, cherry-pick, revert, squash, reset, delete… (`MenuTarget`, `render_menu`) |
| **Drop Menu** | Shown when a branch is dropped on another: merge, rebase, fast-forward. Never a silent action. |
| **Confirm Dialog** | Modal asking before an action that may lose work. (`render_confirm`) |
| **Squash Dialog** | Modal opened from the Graph Menu once the commits to squash are checked: the combined messages to edit, Squash / Cancel. (`render_squash`) |
| **Repo Status Line** | Last operation result under the panes; long messages are condensed, click dismisses. |

## 6. Shared panels

Panels used in more than one place.

### Staging Panel

The working-copy panel, shared by the Repo View (WIP Row selected) and the
Quick Edit Pane (`render`). In the Repo View, the Repo Actions Bar is in the
Repo Toolbar instead (`render_body`). [staging.rs](../crates/archipel-ui/src/staging.rs).

```text
┌─ Repo Actions Bar ─────────────────────────────────┐
│ [Fetch] [Pull] [Push] [Stash] [Pop] [Undo] [Redo]  │
├─ Operation Banner (during a merge, rebase...) ─────┤
│ Rebase in progress     [Continue] [Abort]          │
├─ BRANCH ───────────────────────────────────────────┤
│ switch to: [dev v]  [New branch name] [Create]     │
├─ Unstaged Files ─────── [Stage all] [Discard all] ─┤
│   M src/lib.rs                           [+]       │
├─ Staged Files ───────────────────── [Unstage all] ─┤
│   A src/new.rs                           [-]       │  <- accent color
├─ Commit Box ───────────────────────────────────────┤
│ [Commit message                               ]    │
│ [ ] Amend    [Commit]  /  [Commit and push]        │
└────────────────────────────────────────────────────┘
```

| Name | What it is |
| ---- | ---------- |
| **Repo Actions Bar** | Fetch, Pull, Push, a pull request button (**Create PR…**, or **PR #n** when the branch has one: opens the Forge Panel on that repo; hidden when the repo is not on a forge), Stash, Pop, Undo, Redo (+ Branch in the Repo Toolbar). (`render_actions`) |
| **Operation Banner** | Shown while a merge, rebase, cherry-pick or revert is in progress: Continue / Abort. |
| **Branch Section** | Switch to another local branch or create one. |
| **Unstaged Files** | Working-tree changes, with Stage all / Discard all and per-file stage / discard. Discard asks for a second click. |
| **Staged Files** | The "ready to commit" list, in the accent color, with Unstage all. |
| **Commit Box** | Message input and Commit / Commit and push. |

### Diff View

A diff rendered as virtualized rows. [diff_view.rs](../crates/archipel-ui/src/diff_view.rs).
Shown as the Open Diff (Repo View) and as the Quick Edit Diff (Dashboard).

| Name | What it is |
| ---- | ---------- |
| **Diff Header** | File name, side (**Staged** / **Unstaged**), and **Edit** on a working-copy diff. |
| **Hunk Buttons** | Stage / Unstage / Discard on each hunk of a working-copy diff. |
| **Line Selection** | Changed lines picked with click, Ctrl+click, Shift+click; a **Selection Bar** offers Stage / Unstage / Discard lines and Clear. |
| **File Editor** | Replaces the Diff View to edit the file in place (Edit or double-click a line): Save (`Ctrl+S`), Cancel (`Esc`). ([file_edit.rs](../crates/archipel-ui/src/file_edit.rs)) |

## 7. Overlays

An Overlay sits on a transparent backdrop: clicking outside it dismisses it
(like `Esc`), and that click does not reach the Workspace underneath.

| Name | What it is | Opened by |
| ---- | ---------- | --------- |
| **Palette** | Fuzzy list with an input ([palette.rs](../crates/archipel-ui/src/palette.rs)). Its variants below. | — |
| ↳ **Project Switcher** | Pick a project (recent first). | `Ctrl+P`, Project Header |
| ↳ **Action Palette** | Every action by name. | `Ctrl+Shift+P` |
| ↳ **Prompt Palette** | The Palette as a one-line prompt: checkout target, branch name, tag name, category (with suggestions), category rename, project color, background tint, icon. (`PaletteKind`) | Actions |
| **Commit Panel** | Multi-repo commit (spec §4): repo list with staged-file previews, branch mismatch warning, one message, push option, per-repo report and Retry failed. ([commit_panel.rs](../crates/archipel-ui/src/commit_panel.rs)) | `Ctrl+K` |
| **Forge Panel** | Forge integration (spec §8), with four **Forge Tabs**: **Accounts** (sign in / out), **Pull requests** (open PRs of the selected repos, merge with a confirming second click), **New pull request** (create PRs for the selected repos), **Clone** (into a folder, by default the project file's: from URLs typed or pasted, no account needed, or by browsing an account / org / group; clones join the Repo List expanded, selected and scrolled into view). ([forge_panel.rs](../crates/archipel-ui/src/forge_panel.rs)) | Action Palette, Row Menu |
| **Branches Panel** | Branch cleanup across the selected repos (all when none), spec §7: **merged, not deleted** (pre-checked) and **stale** (unmerged, no commit for 90 days) local branches; Delete asks for a confirming second click, skips a branch that moved meanwhile; Undo on a repo restores its deleted branches one at a time. ([branches_panel.rs](../crates/archipel-ui/src/branches_panel.rs)) | Branches… chip, Action Palette |
| **Context Menus** | Row Menu, Category Menu, Tab Menu (Workspace); Graph Menu, Drop Menu (Repo View). | Right-click |

## 8. Naming rules

- **Pane** is a resizable region inside a view (Refs Panel's sibling panes,
  Quick Edit Pane); **Panel** is a self-contained block of UI that can be
  placed in several spots (Staging Panel) or shown as an Overlay (Commit
  Panel, Forge Panel); **View** is what a tab shows (Repo View) or a reusable
  renderer (Diff View).
- Say **Repo View** for the content and **Repo Tab** for the tab itself.
- Say **Commit Panel** for the multi-repo commit, **Commit Box** for the
  single-repo message area of the Staging Panel.
- Say **Operations Drawer** (not "log" or "console") and **Status Bar** (not
  "footer"); the line inside a Repo View is the **Repo Status Line**.
- Say **WIP Row** for the uncommitted-changes row of the graph.
- When adding a new panel or view, add it here with its code location.
