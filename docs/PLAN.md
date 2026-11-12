# Archipel — Implementation plan

Archipel is a fast, low-RAM, good-looking Git client built around **projects** (groups of repos) rather than single repos.
This plan turns the spec ([SPEC.md](SPEC.md)) into an ordered, testable implementation path.
Section numbers like "§3" refer to the spec's feature sections.

---

## 1. Guiding principles

1. **Measure from day one.** The non-functional targets are the product's reason to exist. Every phase has a perf/RAM exit criterion, checked by an automated harness, not by feel.
2. **UI-agnostic core.** All Git, project, cache and scheduling logic lives in Rust crates with no UI dependency. The UI only renders view-models and sends commands. This keeps the UI-stack decision reversible and makes the core testable headless.
3. **Hybrid Git backend** (from the spec): reads through `gix` (gitoxide), writes through the `git` CLI so hooks, credentials, signing and LFS behave exactly like the user's terminal.
4. **Never block the UI.** Every Git call is async, cancellable, and reports progress. Writes are serialized per repo; reads and network ops run in parallel across repos with bounded concurrency.
5. **Bounded memory.** No unbounded collection anywhere: history is paged, caches are LRU with a byte budget, inactive projects are reduced to a summary.
6. **Clean, fork-friendly architecture.** Code quality and architecture are first-class goals: one concern per module, dependencies pointing inward, small role-based traits (interface segregation), constructor injection, and registries as extension points so a fork can customize behavior by adding an implementation rather than editing the core. Binding rules: [ARCHITECTURE.md](ARCHITECTURE.md).

---

## 2. Decisions

### 2.1 Taken now

| Topic | Decision | Rationale |
|---|---|---|
| Language | Rust (stable, edition 2024) | Required by every stack option in the spec; `gix` is Rust. |
| Read backend | `gix`, behind a `GitRead` trait with a `git` CLI fallback implementation | Lets Phase 0 compare both and lets us fall back per operation if `gix` lacks a feature or is slower (e.g. status on huge worktrees). |
| Write backend | `git` CLI subprocess, `-c` overrides only when needed, `--porcelain`/`-z` outputs | Full compatibility with hooks, credentials, GPG/SSH signing, LFS. |
| Async runtime | `tokio` for orchestration + subprocess I/O; `rayon` (or `spawn_blocking`) for CPU-bound `gix` work | Standard, well-supported on Windows/Linux. |
| File watching | `notify` + `notify-debouncer-full` | Cross-platform; debouncing avoids refresh storms. |
| Project file | TOML, `archipel.toml`, repo paths **relative** to the file | Readable, diffable, shareable (§1). |
| Persistent local data | SQLite (`rusqlite`) in the user data dir: recent projects, UI state, dashboard summaries, stats | One durable store; stats stream to disk instead of RAM. |
| Platforms | Windows + Linux in CI from Phase 0; macOS best-effort later | Per spec. |
| License | MIT (already in repo) | — |

### 2.2 UI stack: GPUI, confirmed by the Phase 0 spike

**Choice: GPUI** (Zed's GPU-accelerated Rust UI framework) with `gpui-component` for ready-made widgets (virtual list/table, inputs, menus, themes). **Tauri** is built as the comparison spike in Phase 0; **egui** is the fallback if GPUI proves unworkable on Windows or Linux.

Why GPUI over the other native options:

| | GPUI | egui | Slint | Tauri |
|---|---|---|---|---|
| RAM baseline | low (no WebView) | low | lowest | high (WebView2 ≈ 80–150 MB) |
| Large virtualized lists, 100k rows | built for it (Zed's editor and panels) | possible, manual | `ListView`, fine | needs a JS virtual list, DOM cost |
| Text quality and typography (aesthetics goal) | excellent (subpixel, shaping) | weakest | good | excellent |
| Custom drawing (graph lanes, diff gutters) | yes, GPU paths/quads | yes, easiest | limited `Path` API | canvas/SVG |
| Polished look without heavy effort | yes (Zed-grade, themes) | hard, "tool" look | good | yes |
| Main risk | API not stable, thin docs | aesthetics ceiling | DSL friction for complex custom widgets | RAM target |

Mitigations for GPUI's risk: pin an exact git revision, isolate all GPUI use in `archipel-ui`, and keep the view-models in `archipel-app` so a fallback to egui only rewrites the UI crate.

Phase 0 still measures GPUI against the Tauri spike; GPUI stays the choice unless it misses the RAM or scrolling criteria and Tauri doesn't.

### 2.3 Answers to the spec's open questions

These are decided; each can be revisited without architectural changes.

| Open question | Decision |
|---|---|
| GitHub/GitLab integration (PR, CI status) | Out of v1. Only "open in browser" links derived from the remote URL (repo, branch, commit, "create PR" compare URL). Keep a `ForgeProvider` trait slot for P2. |
| Submodules | Not added automatically. Detected and shown as a badge on the parent repo; one-click "add submodules to project" (P1). Full manifest/workspace import stays P2. |
| Project file location | Always a shareable `archipel.toml` at a folder root; project definitions are never stored in the user data dir. A user-level registry points to the files; per-user UI state never goes in the shared file. |
| Multi-repo commit: same branch everywhere? | Not imposed. Each repo's branch is shown in the commit dialog with a warning when they differ; per-project setting "require same branch" for teams that want it. |
| Which stats matter | Three in P1: commits/week per author, branch age + merged-not-deleted branches (with batch delete), file hotspots by churn. Everything else waits for real usage. |
| Personal vs distributed | Open source (MIT), built to be distributable: signed installers, no telemetry. |

### 2.4 Undo scope (refines spec §6)

The spec asks for undo on *every* operation. That is achievable for everything **local**; remote operations cannot be truly undone, so they get a confirmed **compensating action** instead.

| Operation | Undo mechanism |
|---|---|
| commit, amend, merge, rebase, squash, cherry-pick, reset, fast-forward, pull (local part) | restore recorded ref + `HEAD` positions (`git update-ref`, `reset --keep`) |
| checkout / switch | switch back to the recorded previous `HEAD` |
| branch / tag create, delete, rename | recreate or delete the ref at its recorded OID |
| stash push / pop / drop | stash commits are recorded by OID, so a dropped stash can be restored with `git stash store` |
| discard changes, stage/unstage | before discarding, snapshot the worktree with `git stash create` (doesn't touch the stash list) and record its OID; undo re-applies it |
| fetch | nothing to undo (only remote-tracking refs move) |
| push | compensating action: "revert the pushed commits" (new commit) or "force-with-lease back to previous remote OID"; always with an explicit warning |

Undo is refused, with an explanation, when the worktree has diverged in a way that would lose work. Every undo is itself undoable (redo).

### 2.5 Graph memory (resolves tension with spec "never the full history in RAM")

The full history is never held on the heap:

- the commit topology (OID + parent indices) is written once to a compact **on-disk cache file per repo** (app cache dir), memory-mapped, and updated incrementally when refs move; the OS can page it out;
- lane layout stores a **checkpoint of active lanes every 1,000 rows**, so any visible window is computed by replaying from the nearest checkpoint instead of from the top;
- commit metadata (subject, author, date, refs) is loaded only for the visible window ± overscan, in a byte-budgeted LRU;
- the cache file and lane checkpoints are dropped from memory when leaving the repo view.

---

## 3. Architecture

### 3.1 Cargo workspace layout

```
archipel/
├─ Cargo.toml                  # workspace
├─ crates/
│  ├─ archipel-model/          # pure data types (Project, RepoSummary, CommitRow, ...) + port traits, no I/O
│  ├─ archipel-project/        # archipel.toml load/save, folder scan, user registry
│  ├─ archipel-git/            # gix + CLI adapters implementing the Git reader/writer traits, output parsers
│  ├─ archipel-ops/            # operation scheduler, per-repo queues, batch ops, results, undo journal
│  ├─ archipel-watch/          # file watching, debouncing, event → invalidation mapping
│  ├─ archipel-cache/          # byte-budgeted LRU caches, SQLite store
│  ├─ archipel-graph/          # commit-graph lane layout, paged history
│  ├─ archipel-stats/          # (P1) streaming stats computation
│  ├─ archipel-theme/          # theme tokens, derivation, built-in themes, theme files (pure)
│  ├─ archipel-app/            # app state, view-models, command bus — UI-agnostic
│  ├─ archipel-compose/        # shared composition root: wires adapters behind the ports
│  ├─ archipel-cli/            # headless binary: dump dashboard, run batch ops, benchmarks
│  └─ archipel-ui/             # the chosen UI frontend (only crate depending on the UI toolkit)
├─ tools/fixtures/             # scripts generating test repos (20-repo project, 100k-commit repo)
├─ benches/                    # perf + RAM harness
└─ docs/
```

Allowed dependency directions and the split of Git access into small traits (`StatusReader`, `HistoryReader`, `CommitWriter`, …) are defined in [ARCHITECTURE.md](ARCHITECTURE.md) §2–3.

`archipel-cli` is not a user feature; it is the measurement and integration-test vehicle that proves the core meets its budgets independently of the UI.

### 3.2 Runtime model

```
UI thread ──commands──▶ archipel-app ──▶ archipel-ops scheduler
   ▲                         │                ├─ per-repo write queue (serial)
   └──── view-model diffs ◀──┤                ├─ read pool (parallel, CPU-bound)
                             │                └─ network pool (parallel, bounded, e.g. 6)
                             ◀── invalidations ── archipel-watch
```

- Every operation returns an `OpHandle` with progress events, cancellation, and a typed result `Result<T, OpError>`.
- Batch operations return a `BatchReport { per_repo: Vec<(RepoId, Outcome)> }`; there is never a silent all-or-nothing (§4).
- Credential prompts: the CLI is run with `GIT_TERMINAL_PROMPT=0` and an app-provided `GIT_ASKPASS`/`SSH_ASKPASS` helper that forwards prompts to the UI. Existing credential helpers (Git Credential Manager, ssh-agent) keep working untouched.

### 3.3 Key data types (sketch)

```rust
struct RepoSummary {            // one dashboard row (§3), ~200 bytes
    head: HeadState,            // Branch(name) | Detached(oid) | Unborn
    upstream: Option<Upstream>, // name, ahead, behind
    changes: ChangeCounts,      // modified, staged, untracked, conflicted
    stash_count: u32,
    describe: Option<String>,   // latest tag + `git describe`
    last_fetch: Option<SystemTime>, // mtime of .git/FETCH_HEAD
    in_progress: Option<RepoOp>,    // merge / rebase / cherry-pick / bisect
    computed_at: Instant,
}
```

Inactive projects keep only `Vec<RepoSummary>` (also persisted in SQLite, so the dashboard paints instantly on startup and project switch, then refreshes in the background — this is how the < 200 ms switch target is met).

### 3.4 UI structure

```
┌──────────────┬─────────────────────────────────────────────────────────┐
│ PROJECT  ▾   │  [Dashboard]  [repo-a]  [repo-b]        ← tabs          │
│ (Ctrl+P)     ├─────────────────────────────────────────────────────────┤
│──────────────│                                                         │
│ Repos        │   Dashboard: table, one row per repo (§3 columns)       │
│  ● app    ↑2 │   + filter chips + batch action bar (on selection)      │
│  ○ libfoo    │                                                         │
│  ● plugin ↓3 │   ── or, in a repo tab ──                               │
│              │   ┌ graph (virtualized) ─────────┬ details / diff ─────┐│
│ Branches     │   │                              │                     ││
│ Tags         │   ├ working copy: unstaged/staged┴ commit box ─────────┤│
│ Stashes      │   └──────────────────────────────────────────────────────┘│
├──────────────┴─────────────────────────────────────────────────────────┤
│ Operations drawer: running ops, progress, batch reports, undo  (Ctrl+J) │
└────────────────────────────────────────────────────────────────────────┘
```

- **Sidebar:** project switcher at the top, the project's repos with live status dots; when a repo tab is active, its branches/tags/stashes (drag sources for §6).
- **Main area:** the dashboard tab is always first; opening a repo adds a tab. The multi-repo commit dialog (§4) opens as a side panel over the dashboard.
- **Operations drawer:** the single place for progress, batch results, retries, errors with the exact command, and undo.
- **Per-project UI state** (§2): open tabs, active tab, selected repo, dashboard filters and sort, panel sizes, sidebar collapse state. Stored in SQLite, keyed by project.
- **Keyboard:** `Ctrl+P` project switcher, `Ctrl+Shift+P` action palette (every action reachable by name), `Ctrl+Tab` cycle tabs, `Ctrl+Enter` commit, `Ctrl+Z` / `Ctrl+Shift+Z` undo/redo of the last Git operation, `Ctrl+J` operations drawer. All shortcuts listed in one rebindable keymap file.

### 3.5 Theming (user-customizable)

Requirement: a regular user can restyle the app **without writing code or editing files**; files exist only for sharing.

- **Semantic tokens only.** UI code never uses a literal color, font or size; it reads named tokens from the active theme: surfaces (`bg.app`, `bg.panel`, `bg.hover`, `bg.selected`), text (`fg.default`, `fg.muted`), `accent`, borders, status colors (`dirty`, `ahead`, `behind`, `conflict`, `success`, `error`), diff (`diff.add.bg`, `diff.del.bg`, word-level highlights), graph lane palette (8–12 colors), syntax highlighting, plus typography (UI font, mono font, sizes), spacing/density, corner radius and animation speed (including "off").
- **Derived themes.** A theme can be defined by just a **base background, a text color and an accent**; every other token is derived (OKLCH lightness/chroma steps), and any token can be overridden individually. This is what makes editing approachable.
- **In-app theme editor** (Settings → Appearance): pick a built-in theme or "duplicate and edit"; simple mode (base colors, accent, fonts, size, density, radius) and advanced mode (every token, grouped); **live preview** on the real UI; contrast warning when text/background falls below WCAG AA; reset per token.
- **Theme files:** TOML in the user config dir (`themes/*.toml`), with a `extends = "dark"` field so a shared theme lists only what it changes. Import/export from the editor; files hot-reloaded on change for power users.
- **Built-in themes:** light, dark, and a high-contrast variant; "follow OS" switches between a chosen light and dark theme.
- **Validation:** unknown or invalid tokens fall back to the parent theme with a warning; a broken theme file never prevents the app from starting.

### 3.6 Logging and diagnostics

- `tracing` with a rolling log file in the user data dir (default level `info`, `debug` togglable at runtime).
- Every Git CLI invocation is logged with its arguments, duration and exit code (credentials and environment never logged).
- "Copy diagnostics" action: app version, OS, git version, recent log tail, cache sizes. No telemetry, no automatic upload.

---

## 4. Phases

Each phase ends with a demoable result and its exit criteria passing in CI.

**Sizing** (one developer, part-time, ≈ 15 h/week; rough, to be re-estimated after Phase 0):

| Phase | Estimate | Cumulative |
|---|---|---|
| 0 — Foundations and spike | 3 weeks | 3 |
| 1 — Projects | 2 weeks | 5 |
| 2 — Dashboard, live updates, switching | 3 weeks | 8 |
| 3 — Batch ops, multi-repo commit | 3 weeks | 11 |
| 4 — Repo view: graph, diff, staging, repo actions | 8 weeks | 19 |
| 5 — Hardening, theming, release | 4 weeks | **23 → MVP** |
| 6 — Drag and drop + undo | 4 weeks | 27 |
| 7 — Statistics | 3 weeks | **30 → v1** |

### Phase 0 — Foundations and stack spike

Goal: lock the UI stack and validate the Git backend with numbers.

1. Workspace skeleton, CI (GitHub Actions, Windows + Linux): `fmt`, `clippy -D warnings`, tests. Logging setup (§3.6). Quality gates from [ARCHITECTURE.md](ARCHITECTURE.md) §6: workspace lints, `cargo deny`, and the `xtask check-deps` dependency-direction check.
2. Fixture generator (`tools/fixtures`):
   - a 20-repo project with a mix of clean/dirty/ahead/behind/detached/stash states;
   - a synthetic 100k-commit repo with merges (plus optionally a real large repo like `git/git` for realism).
3. Backend benchmark in `archipel-cli`: compute `RepoSummary` for all 20 repos via `gix` vs `git status --porcelain=v2 --branch -z` + `git describe`. Record time and RAM on both OSes.
4. UI spike, built twice (GPUI and Tauri), each showing:
   - a 20-row dashboard fed by the real core;
   - a virtualized list of 100k commit rows with simple lane drawing, scrolled continuously.
5. Measure: cold start, idle RSS with dashboard open, RSS after 10 minutes of scrolling, frame time while scrolling, and subjective look-and-feel effort.

**Decision criteria:** GPUI is kept if it meets idle RSS ≤ 100 MB for the spike (margin under the 150 MB target, since features will add more) and smooth scrolling at 100k rows on both OSes. Switch only if GPUI fails a criterion that Tauri meets; egui if both fail on RAM.

6. Validate the UI structure (§3.4) with the spike's shell: sidebar, tabs, operations drawer.
7. Theme foundation (§3.5): define the token set and the `Theme` type, ship light/dark, and switch theme at runtime in the spike to prove tokens apply live. From here on, a CI check rejects literal colors in `archipel-ui` outside the theme module.

**Exit:** ADR written in `docs/adr/0001-ui-stack.md` and `0002-read-backend.md`; spike code deleted or folded into `archipel-ui`.

### Phase 1 — Projects (§1, P0)

1. `archipel.toml` schema (versioned):
   ```toml
   version = 1
   name = "My product"

   [[repo]]
   path = "app"
   [[repo]]
   path = "../shared/libfoo"
   alias = "libfoo"
   ```
2. Load/save with validation (missing repo → shown as "missing", not an error that blocks the project).
3. Folder scan: walk a parent folder (depth-limited, skip `node_modules`, `target`, hidden dirs) and detect `.git` dirs **and** `.git` files (worktrees/submodules).
4. User registry in SQLite: known projects (their `archipel.toml` files), recent list, per-project UI state.
5. UI: create project (manual pick or scan with checkbox list), edit, remove.
6. Submodule detection: badge on the parent repo + "add submodules to project" action (P1, may slip to Phase 6).

**Exit:** round-trip tests for the file format; scan of a 2,000-folder tree < 1 s.

### Phase 2 — Dashboard, live updates, project switching (§2, §3 view, P0)

1. `RepoSummary` computation (backend chosen in Phase 0), parallel across repos.
2. Watcher per repo:
   - watch `.git/` (HEAD, refs, index, FETCH_HEAD, stash) and the worktree recursively;
   - map events to fine-grained invalidations (refs changed → recompute upstream/describe only; worktree changed → status only);
   - ignore `.git/*.lock` churn and ignored paths; debounce ~150 ms;
   - Linux inotify limits: if watches fail, degrade to watching `.git/` only + refresh on window focus, and tell the user.
3. Dashboard table with the six columns, density setting, sorting.
4. Filters: dirty only, behind remote, ahead, detached, operation in progress.
5. Project switcher: `Ctrl+P` fuzzy palette over projects + recent list; switching paints from cached summaries, then refreshes.
6. Unload inactive projects: drop watchers and caches, keep summaries.

**Exit:** dashboard of 20 repos rendered with fresh data < 500 ms; project switch < 200 ms (cached paint); app start < 1 s; idle RSS with a 20-repo project under budget.

### Phase 3 — Batch operations and multi-repo commit (§3 actions, §4, P0)

1. Scheduler (`archipel-ops`): per-repo serial write queue, bounded network pool, cancellation, progress parsing from `git --progress` stderr.
2. Batch actions on selected repos: fetch all, pull all (`--ff-only` by default, configurable), checkout same branch (skip repos where it doesn't exist, report it), create branch/tag on several repos.
3. Batch result panel: per-repo status, error output expandable, "retry failed".
4. Multi-repo commit dialog:
   - repos pre-checked when they have staged changes (or offer "stage all" per repo);
   - per-repo preview of staged files, current branch shown, warning on branch mismatch;
   - one message; ticket prefix (from a project setting or extracted from branch name by regex); message templates stored per project;
   - commit sequentially via `git commit -F -` (stdin) so hooks and signing run;
   - partial failure report (committed / failed with hook output), retry failed ones, optional push after commit.
5. Askpass helper and credential prompt UI (needed as soon as fetch/push exist).
6. Add a repo to a project by cloning from a URL (P1, may slip to Phase 6): clone into a chosen folder through the network pool, with progress, then append to the project file. *Done: Forge Panel → Clone → From a URL (`App::clone_urls_into_project`).*
7. "Open in browser" links (repo, branch, commit, compare/create-PR URL) derived from the remote URL for GitHub, GitLab, Bitbucket and Azure DevOps patterns.

**Exit:** integration tests on fixtures covering partial failure (a repo with a failing `pre-commit` hook), signed commits, and an HTTPS remote requiring credentials (manual test checklist for the latter).

### Phase 4 — Repo view: graph, diff, staging (§5, P0)

The largest phase.

1. **Paged history** (`archipel-graph`, design in §2.5):
   - topology pass streams commit OIDs + parent links (reading Git's commit-graph file when present) into the memory-mapped on-disk topology cache, updated incrementally when refs move;
   - lane layout with checkpoints every 1,000 rows; the first screen renders before the whole topology is walked;
   - commit metadata loaded only for the visible window ± overscan, held in a byte-budgeted LRU.
2. Virtualized graph view: lanes, ref badges, selection, search (by message/author/hash, run in background).
3. Commit details + diff viewer: tree diff, file list, unified/side-by-side, syntax highlighting for the visible region only (`syntect` or `tree-sitter`), large-file and binary guards.
4. Working copy view: staged/unstaged lists; stage/unstage per file, per hunk, per line by building a patch and applying it with `git apply --cached` (and `--reverse` to unstage); discard with confirmation.
5. Conflict state: show conflicted files and an "open external merge tool" action (`git mergetool`), per the non-goals; continue/abort for merge, rebase and cherry-pick in progress.
6. **Single-repo actions** (everything a daily client needs, from context menus, the sidebar and the action palette):
   - commit and **amend** from the repo view's commit box (same code path as §4, one repo);
   - branches: checkout, create (from any commit), rename, delete (with "not merged" warning), set upstream;
   - tags: create (lightweight/annotated), delete, push;
   - stashes: push (with message, optionally including untracked), apply, pop, drop;
   - remote ops on this repo only: fetch, pull, push (force-with-lease only as an explicit option);
   - reset (soft/mixed/hard, hard with confirmation), revert commit.
7. Journal hooks: every mutating action above records its "before" state in the operation journal from day one, so Phase 6 undo needs no retrofit.

**Exit:** 100k-commit repo opens to first frame < 500 ms and scrolls without dropped frames; RSS stays flat after scrolling the whole history (no growth beyond the LRU budget); hunk/line staging covered by tests against `git diff --cached` output; every single-repo action has an integration test on fixtures.

### Phase 5 — MVP hardening and release

1. Soak test: scripted 2-hour session (switch projects, scroll, fetch, commit) with RSS sampled every minute; fail on sustained growth.
2. Theming (§3.5): polish built-in light/dark/high-contrast themes, typography, restrained animations, density levels; theme derivation from base colors; in-app theme editor (simple + advanced modes, live preview, contrast warnings); TOML theme files with `extends`, import/export, hot reload. Exit: a non-developer tester creates and shares a custom theme without opening a file.
3. Keyboard pass: every action in the action palette, keymap file documented and rebindable, focus navigation across sidebar/table/graph/diff.
4. Settings: external merge/diff tool, pull strategy, concurrency limits, cache budget.
5. Error UX: every failed Git command shows the exact command and stderr.
6. Packaging: Windows installer (MSI or MSIX, signed) + winget manifest; Linux AppImage and `.deb` (Flatpak later). Auto-update optional.
7. Docs: README with screenshots, user guide for `archipel.toml`, `docs/EXTENDING.md` listing every extension point with a minimal example (for forks).

**Exit = MVP (all P0 features).**

### Phase 6 — Drag and drop + undo (§6, P1)

1. Operation journal (recording already in place since Phase 4): affected refs, their OIDs, `HEAD`, and a worktree snapshot OID for destructive ops; stored in SQLite per repo, bounded (e.g. last 200 ops per repo).
2. Undo/redo for every operation, per the table in §2.4: ref restoration for local ops, snapshot re-application for discards, recorded stash OIDs for stashes, and confirmed compensating actions for push. Refused with an explanation when it would lose work.
3. Batch undo: a batch operation (e.g. checkout on 12 repos) is undone as one unit, with a per-repo report.
4. Drag targets, always followed by a confirmation menu:
   - branch → branch: merge / rebase / fast-forward (options greyed when invalid);
   - local branch → remote: push (with force-with-lease only as an explicit option);
   - commit → branch: cherry-pick.
5. Same gestures available from context menus and keyboard (accessibility, and DnD is error-prone).

**Exit:** every operation type in §2.4 has an undo (and redo) test on fixtures.

### Phase 7 — Statistics (§7, P1)

1. Streaming computation over history (`gix` tree diffs or `git log --numstat -z`), incremental: store the last processed commit per repo and only process new commits.
2. Results in SQLite, aggregated per project on demand.
3. Three views first (see §2.3): activity per author over time, branch age + merged-not-deleted (with batch delete), hotspots by churn.
4. Runs at low priority, pausable, never on the UI thread.

**Exit:** stats on the 100k-commit repo computed without RSS exceeding the cache budget; incremental update after a new commit < 1 s.

### P2 backlog

- Import from submodules, `repo` manifest, VS Code `.code-workspace`.
- Forge integration (PR list, CI status) behind `ForgeProvider`.
- CI failure excerpt and rerun (spec §8) — done: `CiReader::ci_job_log` and `CiWriter` ports, provider methods `CiApi::job_log` / `log_excerpt` / `rerun_failed`, error rules in `archipel-forge::ci_log`, `App::ci_job_log` (cached per session) and `Command::RerunFailedCi`, the hoverable CI Tooltip.
- Conflict forecast (spec §3) — done: the `MergeForecaster` port (`git merge-tree --write-tree`, rebase replayed commit by commit), `App::load_forecast` (background, one at a time, cached by commit ids), `DashboardRow::forecast`, the Sync column badge (`DashboardColumn::badge`), the "Will conflict" filter, the Forecast Badge in the Quick Edit Header and the Repo Toolbar.
- macOS packaging and notarization.
- Worktrees as first-class project members.

---

## 5. Cross-cutting concerns

### 5.1 Performance and memory harness

- `archipel-cli bench` runs scenarios against fixtures and prints JSON (timings, peak/idle RSS).
- CI runs it on Windows and Linux on every PR; regressions over a threshold (e.g. +10% time, +15 MB RSS) fail the build.
- UI-level metrics (start time, frame times, RSS) captured by a debug overlay and a scripted run in Phase 0, 4 and 5.
- Every cache goes through `archipel-cache` with a named budget so total memory can be reported per cache.

| Target (from spec) | How it is measured |
|---|---|
| Start < 1 s | process start → first dashboard frame, cached project |
| Dashboard 20 repos < 500 ms | fresh computation, warm OS file cache |
| Project switch < 200 ms | switch command → painted from cache |
| Idle RSS < 150 MB (20 repos) | RSS 30 s after load, no interaction |
| No leak | soak test slope ≈ 0 |
| UI never blocked | no Git call on UI thread (enforced by API: UI crate can't access `archipel-git`) |

### 5.2 Git compatibility

- Always respect user config: run the CLI without overriding `core.*`, credential, signing or hook settings.
- `gix` reads must honor `core.autocrlf`, `.gitattributes`, `safe.directory`, worktrees, sparse checkout; compare against the CLI in tests for each fixture.
- LFS: status/diff show pointer state correctly; pull/checkout through the CLI handle smudge.
- Windows specifics: long paths, case-insensitive FS, `core.fileMode`, CRLF.
- Minimum supported `git` version checked at startup (e.g. 2.40) with a clear message.

### 5.3 Testing strategy

- Unit tests for parsers (porcelain v2, `--numstat`, progress lines) using recorded outputs.
- Integration tests creating real temporary repos with the `git` CLI (fixtures built by script, not committed binaries).
- Differential tests: `gix` read results vs CLI results on every fixture.
- Manual checklist for credentials, SSH passphrases, GPG/SSH signing, hooks, LFS — run before each release.

### 5.4 Project hygiene

- [ARCHITECTURE.md](ARCHITECTURE.md) holds the binding architecture and code quality rules, and its review checklist applies to every PR.
- `docs/adr/` for decisions (UI stack, read backend, file format, undo design).
- Conventional, small PRs per phase step; `CHANGELOG.md` from Phase 5.

---

## 6. Risks

| Risk | Impact | Mitigation |
|---|---|---|
| GPUI API changes or missing widgets | Rework, schedule | Pin an exact revision; rely on `gpui-component`; all GPUI code confined to `archipel-ui`; egui fallback. |
| GPUI on Linux/Windows GPU drivers (Vulkan/DirectX) misbehaves on some machines | Users can't run the app | Test on integrated + discrete GPUs and a VM in Phase 0; document requirements. |
| Native UI slows down styling and DnD work | Schedule | Keep custom widgets few (table, graph, diff); Tauri spike numbers kept as a fallback reference. |
| Hard-coded styles creep in, making themes incomplete | Themes look broken in places | Tokens-only rule enforced by a CI check from Phase 0; every screen reviewed under light, dark and high-contrast. |
| Topology cache on disk gets stale or corrupt | Wrong graph | Key the cache by repo path + ref tips; validate on open; rebuild on mismatch. |
| `gix` gaps (status edge cases, describe, sparse) | Wrong data in dashboard | `GitRead` trait with CLI fallback per operation; differential tests. |
| Watcher limits / event storms on big worktrees | CPU spikes, stale data | Debounce, fine-grained invalidation, degrade to `.git/`-only + focus refresh. |
| Credential prompts hang subprocesses | Blocked batch ops | `GIT_TERMINAL_PROMPT=0`, askpass helper, per-op timeouts and cancellation. |
| Undo after rebase/push is ambiguous | Data loss perception | Only undo local ref moves; refuse with explanation otherwise; always confirm. |
| Line-level staging edge cases (CRLF, no newline at EOF, binary) | Corrupted index | Patch generation tested against fixtures with those cases; fall back to file-level staging with a message. |

---

## 7. Immediate next steps

1. Create the workspace skeleton, logging and CI (Phase 0, step 1).
2. Write the fixture generator; it is reused by every later phase.
3. Run the backend benchmark and the GPUI + Tauri spikes; record the ADRs.
