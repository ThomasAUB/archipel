<p align="center">
  <img src="assets/logo/archipel.svg" alt="Archipel logo" width="128">
</p>

# Archipel

[![CI](https://github.com/ThomasAUB/archipel/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/ThomasAUB/archipel/actions/workflows/ci.yml)
[![Coverage](https://coveralls.io/repos/github/ThomasAUB/archipel/badge.svg?branch=main)](https://coveralls.io/github/ThomasAUB/archipel?branch=main)

A fast, light-on-RAM Git client built around **projects**: groups of repos you
work on together. See the state of every repo at a glance, and act on them in
batch — fetch all, pull all, check out the same branch everywhere, commit the
same change in several repos with a clear per-repo report.

Runs on Windows, Linux (Wayland and X11) and macOS.

## Try it

### Download

Grab the archive for your platform from the
[latest release](https://github.com/ThomasAUB/archipel/releases/latest),
unpack it, and run `archipel`. You also need **git** 2.40 or newer on your
`PATH`.

| Platform | Archive | First launch |
|---|---|---|
| Windows x64 | `…-x86_64-pc-windows-msvc.zip` | The build is not signed yet: on the SmartScreen warning, click *More info → Run anyway*. |
| Linux x64 | `…-x86_64-unknown-linux-gnu.tar.gz` | Put `archipel` on your `PATH` (e.g. `~/.local/bin`). For a launcher entry, see [below](#optional-launcher-entry-linux). |
| macOS (Apple silicon) | `…-aarch64-apple-darwin.tar.gz` | The build is not notarized yet: run `xattr -d com.apple.quarantine archipel` once, then `./archipel`. |

Then [open a project](#3-open-a-project). Archipel tells you when a newer
release is out (an "↑ Update" badge in the status bar).

### Build from source

For other platforms, or to run the latest `main`. It takes a few minutes the
first time.

#### 1. Install the prerequisites

- **Rust** 1.92 or newer — install with [rustup](https://rustup.rs).
- **git** 2.40 or newer, on your `PATH`.
- Platform libraries:

  | Platform | What to install |
  |---|---|
  | Windows | The Visual Studio C++ Build Tools (rustup offers them during install). |
  | macOS | The Xcode Command Line Tools: `xcode-select --install` |
  | Linux (Debian / Ubuntu) | `sudo apt install libxkbcommon-dev libxkbcommon-x11-dev libwayland-dev libvulkan-dev libx11-xcb-dev libxcb1-dev libfontconfig-dev libfreetype-dev libasound2-dev pkg-config` |

#### 2. Build and install

```bash
git clone https://github.com/ThomasAUB/archipel
cd archipel
cargo install --path crates/archipel-ui
```

This puts the `archipel` binary in `~/.cargo/bin`.

#### 3. Open a project

Point it at a folder that contains several Git repos:

```bash
archipel path/to/folder
```

Archipel finds the repos around the folder and saves the list to an
`archipel.toml` there. Next time, `archipel` with no folder reopens the last
project.

**No multi-repo folder at hand?** Generate a demo project with 20 repos:

```bash
cargo run --release -p archipel-fixtures -- project ../archipel-demo
archipel ../archipel-demo
```

#### 4. Things to try first

- Press `Ctrl+P` to switch project, `Ctrl+Shift+P` to see every action.
- Click a repo for its quick edit pane (fetch, pull, push, branch, stage,
  commit…). Double-click opens the full tab.
- `Ctrl+click` several repos, then run a batch action on all of them.

On macOS, use `Cmd` instead of `Ctrl` (except for `Ctrl+Tab`).

### Optional: launcher entry (Linux)

With `archipel` on your `PATH`, from the unpacked release archive:

```bash
install -Dm644 archipel.desktop ~/.local/share/applications/archipel.desktop
install -Dm644 archipel.svg ~/.local/share/icons/hicolor/scalable/apps/archipel.svg
```

or from a clone of the repo:

```bash
install -Dm644 assets/linux/archipel.desktop ~/.local/share/applications/archipel.desktop
install -Dm644 assets/logo/archipel.svg ~/.local/share/icons/hicolor/scalable/apps/archipel.svg
```

On macOS there is no `.app` bundle yet: run `archipel` from a terminal.

## Usage

### Opening projects

`archipel path/to/folder` opens the folder's `archipel.toml`, or writes one
listing the repos found around it (scanned in the background; launching from
inside a repo uses the outermost repo). Launching again while Archipel runs
opens another window in the same process, or focuses the window already
showing that project (`--standalone` starts a separate process). Without a
folder, the last project reopens.

### Shortcuts

| Shortcut | Action |
|---|---|
| `Ctrl+P` / `Ctrl+Shift+P` | switch project / all actions |
| click a repo | quick edit pane: fetch / pull / push, stash / pop, undo / redo, create or switch branch, stage / unstage / discard files, commit (or amend, or commit & push), continue / abort a merge or rebase (`Esc` or clicking the repo again closes it, double click opens the full tab) |
| `Ctrl+click` | add a repo to the selection (batch actions apply to it) |
| `Ctrl+H` / `Ctrl+Shift+H` | hide (or unhide) the selected repos / list hidden repos |
| `Ctrl+Shift+T` | dependency tree (nested submodules) |
| `Ctrl+N` (or `Ctrl+Shift+N`) | new window |
| `F5` | reload the project file (after a hand edit or a pull) and refresh every repo |
| right-click a repo | repo menu |

On macOS, `Cmd` replaces `Ctrl` in every shortcut except `Ctrl+Tab`. The app
menu has Quit (`Cmd+Q`), Hide and Services; `Cmd+H` stays bound to hiding
repos, so Hide Archipel has no shortcut.

### Project color and icon

Set them from the action palette ("Set the project color…", "Set the project
icon…", "Set the project icon from an image…"). They are saved in
`archipel.toml`, so the team shares them:

```toml
[settings]
accent = "#3b82f6"
background_tint = 15
icon = "🚀"                 # or an image path: "assets/logo.svg"
```

An image icon (PNG, SVG, JPEG, WebP, GIF, BMP, ICO) is a path relative to
`archipel.toml`; it is shown next to the project name in the sidebar.

### Forges (GitHub / GitLab)

"Forge accounts…" in the action palette signs in to GitHub, GitLab or a
self-hosted instance with a personal access token. The token is stored in the
OS keychain (Windows Credential Manager, Secret Service on Linux, login
Keychain on macOS), never in `archipel.toml`.

The dashboard then shows each repo's open pull request with its CI and review
state. "Create pull requests…" opens one per selected repo, linked together,
and "Clone repositories from a forge account…" adds clones to the project.

## Status

| Area | State |
|---|---|
| Core (projects, dashboard, live updates, batch ops, multi-repo commit, undo/redo, graph, stats, themes) | implemented, tested headless |
| Desktop UI (GPUI) | dashboard, project switcher, batch bar, repo graph + details, working copy with file/hunk staging, multi-repo commit, operations drawer, theme switch, launch from a directory, hidden repos, repo categories (grouped sidebar), several windows in one process with per-project color / icon / title, repo dependency tree, clone from URLs into the project, forge integration (GitHub / GitLab: pull request column, linked pull requests across repos, clone from an account) |
| Packaging | release archives for Windows x64, Linux x64, macOS Apple silicon (unsigned); installers (MSI / AppImage / `.app`) not yet |

## Performance

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

## Develop

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo xtask check-deps
```

Headless views of a project, without the desktop UI:

```bash
cargo run --release -p archipel-cli -- dashboard path/to/folder-with-repos
cargo run --release -p archipel-cli -- tree path/to/folder-with-repos
```

Line coverage (needs `cargo install cargo-llvm-cov` and
`rustup component add llvm-tools-preview`):

```bash
cargo llvm-cov --workspace --exclude xtask --exclude archipel-fixtures --html --open
```

### Workspace

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

### Docs

[Spec](docs/SPEC.md) · [Plan](docs/PLAN.md) ·
[Architecture rules](docs/ARCHITECTURE.md) ·
[Extension points](docs/EXTENDING.md) ·
[Creating a theme](docs/THEMES.md) · [Decisions](docs/adr/)

## License

MIT — see [LICENSE](LICENSE).
