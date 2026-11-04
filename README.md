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
| Desktop UI (GPUI) | `crates/archipel-ui` |
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

## Workspace

```
crates/
  archipel-model     pure types + port traits (no I/O), in-memory fakes
  archipel-git       gix readers + git CLI readers/writers, output parsers
  archipel-watch     file watching → fine-grained invalidations
  archipel-cache     byte-budgeted LRUs + SQLite store
  archipel-project   .gitproject.toml, folder scan, submodules
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

Requires `git` ≥ 2.40 on `PATH`. License: MIT.
