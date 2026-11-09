# Archipel — Architecture and code quality rules

Archipel must be a codebase people **want to fork**: easy to read, easy to change in one place without touching ten others, and easy to customize without patching the core.
These rules are binding for every crate. [PLAN.md](PLAN.md) §3 describes *what* the modules are; this file describes *how* they must be built.

---

## 1. Goals

1. **Good architecture first.** Clear module boundaries, one responsibility per module, dependencies pointing in one direction.
2. **Interface segregation.** Code depends on small, role-specific interfaces (traits), never on a large "does everything" type.
3. **Fork-friendly.** Anyone forking the repo can replace or extend a behavior (Git backend, theme, keymap, dashboard columns, actions, forge links, stats…) by adding or swapping an implementation, not by editing core logic.
4. **High code quality.** Strict lints, typed errors, tests through interfaces, documented public APIs, enforced by CI.

---

## 2. Module boundaries

### 2.1 One concern per crate

Each crate in `crates/` owns exactly one concern (see PLAN §3.1). If a crate starts needing a second concern, split it rather than grow it. Inside a crate, the same rule applies to modules: a module is a folder or file with one clear job and a small `pub` surface.

### 2.2 Dependency direction

Dependencies only point **inward**, toward the pure layers. No cycles, no shortcuts.

```
            archipel-ui        archipel-cli          ← frontends / composition roots
                  \               /                    (both wired by archipel-compose)
                   archipel-app                       ← view-models, command bus, use cases
                  /    |     |     \
     archipel-ops  archipel-graph  archipel-stats  archipel-project   ← features
            |         |      \         /
      archipel-git  archipel-watch  archipel-cache  archipel-forge   ← adapters (I/O: git, FS, SQLite, HTTP + keychain)
                  \       |       /       /
                    archipel-model                    ← pure types + port traits (no I/O)
```

Edges the drawing leaves out (the `ALLOWED` table of `xtask` is the exact map):

- `archipel-ui` also uses `archipel-ops` (the `RepoActions` use cases, `OpClass`, the pure `patch` helpers) and `archipel-graph` (the history view model), besides `archipel-app`, `archipel-theme` and `archipel-model`; its `main` uses `archipel-compose`.
- `archipel-app` and `archipel-graph` use the pure LRU registry of `archipel-cache`, never its SQLite store.

Rules:

- `archipel-model` depends on nothing in the workspace and does no I/O. It holds the shared data types **and the port traits** (the interfaces other crates implement or consume).
- Feature crates (`ops`, `graph`, `stats`, `project`) depend on **traits** from `archipel-model`, not on concrete adapters. They never name `gix`, `rusqlite` or `notify` types.
- Adapter crates (`git`, `watch`, `cache`, `forge`) implement the traits. They never depend on feature crates. `archipel-forge` (GitHub / GitLab REST clients, OS keychain, ADR 0003) is reached by `archipel-app` only through the forge ports, and only from scheduler jobs (network pool), never from the UI thread.
- `archipel-app` knows use cases, not toolkits. `archipel-ui` is the only crate that depends on the UI toolkit, and it never depends on adapter crates directly.
- Only the **composition roots** (`archipel-ui`'s `main`, `archipel-cli`) know which concrete implementation goes behind which trait. Both get their wiring from `archipel-compose`, the single shared `compose` module; the UI library code itself never names an adapter.
- `archipel-theme` is a pure leaf crate (tokens, derivation, theme files) with no workspace dependency; `archipel-app` and `archipel-ui` use it.
- `archipel-graph` may use the pure LRU of `archipel-cache` (never its SQLite store).

A CI check (`cargo xtask check-deps`) fails the build if a crate depends on a crate it is not allowed to, if an allowed edge is no longer used, or if a crate other than its adapter depends on a wrapped third-party crate (§2.3); the allowed edges are the `ALLOWED` table in `xtask/src/main.rs`, kept in sync with this section. Dev-dependencies are free (adapter tests use fixtures and real backends).

### 2.3 Third-party types stay inside their adapter

`gix`, `rusqlite`, `notify`, `ureq`, `keyring`, `gpui` and similar types never appear in a public signature outside the crate that wraps them. Adapters convert to `archipel-model` types at the boundary. This is what makes a backend, a store or the UI toolkit replaceable.

---

## 3. Interface segregation

### 3.1 Small, role-based traits

Define traits by **what a consumer needs**, not by what an implementation can do. A consumer depends on the narrowest trait that covers its use.

Example for Git access — instead of one `GitRead` / `GitWrite` pair with dozens of methods:

| Trait | Methods (sketch) | Main consumers |
|---|---|---|
| `StatusReader` | `summary(repo) -> RepoSummary`, `changes(repo)` | dashboard, working copy view |
| `RefReader` | `branches`, `tags`, `upstream`, `describe` | sidebar, dashboard |
| `HistoryReader` | `topology(repo, from)`, `commit_meta(oids)` | `archipel-graph` |
| `DiffReader` | `tree_diff`, `file_diff` | diff viewer |
| `StashReader` | `list` | sidebar |
| `CommitWriter` | `commit`, `amend` | commit box, multi-repo commit |
| `IndexWriter` | `stage`, `unstage`, `apply_patch_cached`, `discard` | working copy view |
| `TreeReader` | `list_files` (a commit's tree, or the working copy), `read_blob` | repo view's file browser |
| `RefWriter` | `checkout`, `create/rename/delete branch`, `tag`, `reset`, `update_ref` | repo actions, undo |
| `StashWriter` | `push`, `apply`, `pop`, `drop`, `store` | repo actions, undo |
| `WorktreeSnapshots` | `snapshot_worktree`, `snapshot_paths`, `restore_paths` | undo journal |
| `RemoteOps` | `fetch`, `pull`, `push`, `clone` | batch ops |
| `HistoryRewriter` | `merge`, `rebase`, `cherry_pick`, `revert`, `continue/abort` | drag & drop, repo actions |
| `SubmoduleReader` | `submodules` (one level: initialized, pinned and checked-out commits) | dependency tree |
| `SubmoduleWriter` | `update_submodules` (`--init`, `--recursive`) | dependency tree actions |

One struct may implement several traits (the `gix` adapter implements all readers, the CLI adapter implements all writers and readers as a fallback), but **nobody depends on the struct**, only on the traits.

The same principle applies everywhere else, for example:

- storage: `SummaryStore`, `UiStateStore`, `JournalStore`, `StatsStore` rather than one `Database`;
- forges: `PullRequestReader` (dashboard column), `CiReader` (CI column), `PullRequestWriter` (create, link), `ForgeDirectory` (clone from an account), `ForgeSignIn` (verify a token, detect the provider), `CredentialStore` (OS keychain), `ForgeAccountStore` (signed-in hosts) rather than one `ForgeClient`;
- project: `ProjectSource` (load/save a project definition) separate from `RepoDiscovery` (scan a folder);
- watching: `ChangeSource` (emits invalidations) separate from the debouncing policy.

### 3.2 Trait design rules

- Keep traits small (guideline: ≤ 5 methods). A trait that keeps growing is two traits.
- Traits take and return `archipel-model` types only.
- Every trait has a doc comment stating its contract (thread-safety, cancellation, error cases).
- Every trait has an in-memory **fake** in a `testing` module (behind a `testing` feature) so consumers are tested without real repos.

---

## 4. Dependency injection and composition

- **Constructor injection.** A component receives the traits it needs in its constructor (`Arc<dyn Trait>` across async/thread boundaries, generics where monomorphization pays off in hot paths). No global state, no singletons, no `lazy_static` service locators.
- **One composition root per binary.** Wiring lives in a single `compose` module that builds the object graph from the settings. Swapping an implementation is a one-line change there, or a setting.
- **Defaults in one place.** All default values (budgets, timeouts, concurrency, paths) live in a `Settings` type with documented defaults, overridable by the settings file. No magic numbers scattered in code.

---

## 5. Customization points for forks

Extension is done through **registries** of trait objects, not through `match` statements spread across the code. Adding a column, an action or a provider means adding one implementation and registering it.

| What a fork may want to change | Extension point | Where it is registered |
|---|---|---|
| Look and feel | theme tokens + theme files (PLAN §3.5) | user config / built-in themes |
| Shortcuts | keymap file, actions referenced by id | user config |
| Actions (palette, menus, batch bar) | `Action` trait (id, label, availability, run) | `ActionRegistry` |
| Dashboard columns | `DashboardColumn` trait (header, value from `RepoSummary`, sort key, renderer hint) | `ColumnRegistry` |
| Dashboard filters | `RepoFilter` trait | `FilterRegistry` |
| Git backend per operation | the reader/writer traits of §3.1 | composition root / settings |
| "Open in browser" links | `ForgeProvider` trait (matches a remote URL, builds links) | `ForgeRegistry` |
| Forge APIs (pull requests, CI, review, clone from an account) | `archipel_forge::provider::{ForgeApi, PullRequestApi, DirectoryApi}` | `ForgeClient::register` in `archipel_compose::forge_ports` |
| Commit message helpers | `MessageTemplate`, `TicketPrefixExtractor` | per-project settings |
| Project import (P2, not implemented yet: `repo` manifest, VS Code workspace) | `ProjectImporter` trait (planned) | `ImporterRegistry` (planned) |
| Repo dependency tree (submodules; later `repo` manifests, package managers) | `DependencySource` trait | `Extensions::dependencies` |
| Statistics | `StatProvider` trait (incremental compute + view model) | `StatsRegistry` (built by the CLI; the desktop app has no statistics view yet) |
| Persistence | the store traits of §3.1 | composition root |

Rules:

- Built-in features use the same extension points as a fork would. If the core needs a private back door, the extension point is wrong.
- Optional features should become Cargo features (`stats`, `forge-links`, …) so a fork can drop them cleanly. None is feature-gated yet: today a fork drops a feature by not registering it.
- Every extension point is listed in `docs/EXTENDING.md` (written in Phase 5) with a minimal example.

---

## 6. Code quality rules

### 6.1 Lints and formatting

- `rustfmt` with a committed `rustfmt.toml`; CI rejects unformatted code.
- Workspace-level lints in the root `Cargo.toml` (`[workspace.lints]`), inherited by every crate:
  - `clippy::all` and `clippy::pedantic` as warnings, `-D warnings` in CI (individual pedantic lints may be allowed in one place, with a comment why);
  - `unsafe_code = "forbid"` (an exception needs an ADR and a `// SAFETY:` comment);
  - `missing_docs = "warn"` on library crates' public items;
  - `clippy::unwrap_used`, `clippy::expect_used`, `clippy::panic` denied outside tests and binaries' startup code.
- `cargo deny` for licenses, advisories and duplicate dependencies.

### 6.2 Errors

- Each library crate defines its own error enum with `thiserror`; errors carry context (repo, command, stderr) needed to display them.
- `anyhow` only in binaries and tests.
- No silent `.ok()` / `let _ =` on fallible calls unless the reason is commented.

### 6.3 API surface

- Items are private or `pub(crate)` by default; `pub` is a deliberate decision.
- Prefer newtypes over primitives for identifiers (`RepoId`, `ProjectId`, `Oid`), and enums over booleans in signatures.
- Functions do one thing; long functions and deep nesting are refactored, not commented.

### 6.4 Tests

- Consumers are unit-tested against trait fakes; adapters are tested against real temporary repos (PLAN §5.3).
- Every bug fix comes with a test that fails before the fix.
- Public behavior is tested through the public API, not private internals.

### 6.5 Documentation

- Each crate has a crate-level doc comment: its responsibility, what it may depend on, its main traits.
- Significant design choices get an ADR in `docs/adr/`.
- Architecture changes (new crate, new dependency edge, new extension point) update this file in the same PR.

---

## 7. Review checklist

Before merging, a change should answer yes to:

- [ ] Does each touched crate/module still have a single responsibility?
- [ ] Do dependencies still point inward, with no third-party type leaking out of its adapter?
- [ ] Does new code depend on the narrowest trait it needs?
- [ ] Could a fork change this behavior by adding an implementation instead of editing this code?
- [ ] Are new defaults in `Settings`, new errors typed, new public items documented?
- [ ] Is it tested, with fakes for consumers and real repos for adapters?
