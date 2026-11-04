# Archipel

A fast, light-on-RAM Git client built around **projects**: groups of repos you
work on together. See the state of every repo at a glance, and act on them in
batch — fetch all, pull all, check out the same branch everywhere, commit the
same change in several repos with a clear per-repo report.

- Spec: [docs/SPEC.md](docs/SPEC.md) · Plan: [docs/PLAN.md](docs/PLAN.md) ·
  Architecture rules: [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) ·
  Extension points: [docs/EXTENDING.md](docs/EXTENDING.md) · Decisions: [docs/adr/](docs/adr/)

## Status

| Area | State |
|---|---|
| Core (projects, dashboard, live updates, batch ops, multi-repo commit, undo/redo, graph, stats, themes) | implemented, tested headless |
| Desktop UI (GPUI) | dashboard, project switcher, batch bar, repo graph + details, working copy with file/hunk staging, multi-repo commit, operations drawer, theme switch, launch from a directory, hidden repos, repo categories (grouped sidebar), several windows in one process with per-project color / icon / title, repo dependency tree, forge integration (GitHub / GitLab: pull request column, linked pull requests across repos, clone from an account) |
| Packaging (MSI / AppImage) | not yet |

## Measured budgets

`cargo run --release -p archipel-cli -- bench` checks the spec targets on
generated fixtures (20-repo project, 100k-commit history). Windows 11, 16 CPUs:

| Spec target | Measured |
|---|---|
| Dashboard of 20 repos < 500 ms | ~110 ms |
| Start → dashboard painted < 1 s | ~80 ms (from cache) |
| Project switch < 200 ms | ~35 ms |
| Idle RAM < 150 MB (20-repo project, core) | ~25 MiB |
| 100k-commit graph, first rows < 500 ms | ~160 ms cold, ~7 ms cached |
| RAM flat while scrolling the whole history | +7 MiB (bounded by the LRU budget) |
| Desktop app idle, 20-repo project (release) | 75 MiB, flat |
| Desktop app launch → first frame < 1 s | ~380 ms (`tools/bench-startup.ps1`) |

## Workspace

```
crates/
  archipel-model     pure types + port traits (no I/O), in-memory fakes
  archipel-git       gix readers + git CLI readers/writers, output parsers
  archipel-watch     file watching → fine-grained invalidations
  archipel-cache     byte-budgeted LRUs + SQLite store
  archipel-forge     GitHub / GitLab REST clients, OS keychain tokens
  archipel-project   archipel.toml, folder scan, submodules
  archipel-ops       scheduler, journal, undo/redo, batch ops, multi-repo commit, line staging
  archipel-graph     on-disk topology cache, lane layout, paged history
  archipel-stats     incremental statistics
  archipel-theme     theme tokens, derivation, theme files
  archipel-app       UI-agnostic application layer (view-models, commands, registries)
  archipel-compose   composition root shared by the binaries
  archipel-cli       headless binary + perf/RAM harness
  archipel-ui        GPUI desktop frontend
tools/fixtures       fixture generator (multi-repo projects, large histories)
xtask                quality gates (dependency directions, theme tokens)
```

## Use

```bash
archipel path/to/folder
```

Opens the folder's `archipel.toml`, or writes one listing the repos found
around it (scanned in the background; launching from inside a repo uses the
outermost repo). Launching again while Archipel runs opens another window
in the same process, or focuses the window already showing that project
(`--standalone` starts a separate process). Without a folder, the last project
reopens.

| Shortcut | Action |
|---|---|
| `Ctrl+P` / `Ctrl+Shift+P` | switch project / all actions |
| `Ctrl+click` | add a repo to the selection (batch actions apply to it) |
| `Ctrl+H` / `Ctrl+Shift+H` | hide (or unhide) the selected repos / list hidden repos |
| `Ctrl+Shift+T` | dependency tree (nested submodules) |
| `Ctrl+N` (or `Ctrl+Shift+N`) | new window |

Right-click a repo for its menu. The project color and icon are set from the
action palette ("Set the project color…", "Set the project icon…") and saved
in the project, so `archipel.toml` shares them with the team
(`[settings] accent = "#3b82f6"`, `icon = "🚀"`). F5 reloads the project file
(after a hand edit or a pull) and refreshes every repo.

Forges: "Forge accounts…" in the action palette signs in to GitHub, GitLab or
a self-hosted instance with a personal access token (stored in the OS
keychain, never in `archipel.toml`). The dashboard then shows each repo's
open pull request with its CI and review state; "Create pull requests…" opens
one per selected repo, linked together, and "Clone repositories from a forge
account…" adds clones to the project. On Linux the keychain is the Secret
Service (GNOME Keyring, KWallet).

## Develop

```bash
cargo test --workspace
```

```bash
cargo clippy --workspace --all-targets -- -D warnings
```

```bash
cargo xtask check-deps
```

```bash
cargo run --release -p archipel-cli -- dashboard path/to/folder-with-repos
```

```bash
cargo run --release -p archipel-cli -- tree path/to/folder-with-repos
```

Requires `git` ≥ 2.40 on `PATH`. License: MIT.

### Linux

Runs on Wayland and X11. Building the desktop app needs the GPUI system
libraries (Debian / Ubuntu names):

```bash
sudo apt install libxkbcommon-dev libxkbcommon-x11-dev libwayland-dev libvulkan-dev \
  libx11-xcb-dev libxcb1-dev libfontconfig-dev libfreetype-dev libasound2-dev pkg-config
```

For a launcher entry with the Archipel icon:

```bash
cargo install --path crates/archipel-ui
install -Dm644 assets/linux/archipel.desktop ~/.local/share/applications/archipel.desktop
install -Dm644 assets/logo/archipel.svg ~/.local/share/icons/hicolor/scalable/apps/archipel.svg
```
