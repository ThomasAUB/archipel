# ADR 0002 — Read backend: `gix` by default, CLI as fallback

- Status: accepted
- Date: 2026-11-04
- Context: PLAN §2.1 and Phase 0 step 3.

## Context

Reads (status, refs, history) can be served in-process by `gix` or by
spawning the `git` CLI. Writes always go through the CLI (hooks, signing,
credentials, LFS). Both read backends are implemented in `archipel-git`
behind the same traits (`StatusReader`, `RefReader`, `HistoryReader`,
`StashReader`), and `tests/differential.rs` checks they return the same data
on every fixture state (clean, dirty, staged, untracked, ahead, behind,
diverged, detached, stashed, merge conflict).

## Measurements

`cargo bench -p archipel-git --bench read_backends` (release build, warm OS
cache; Windows 11, 16 logical CPUs, git 2.53). Median of N runs:

| Scenario | `git` CLI | `gix` |
|---|---:|---:|
| `RepoSummary` of a 20-repo project, parallel | 923 ms | **130 ms** |
| same, sequential | 1906 ms | 415 ms |
| `RepoSummary` of one repo | 91 ms | 19 ms |
| `changes()` on a 20k-file worktree | 103 ms | 92 ms |
| `summary()` on a 20k-file worktree | 103 ms | 95 ms |
| topology walk, 100k commits (with commit-graph) | 451 ms | 193 ms |
| first 1,000 topo-ordered commits of 100k | 239 ms | 131 ms |
| metadata of a 200-commit window | 82 ms | 12 ms |

The CLI cost is dominated by process creation on Windows (each summary
spawns `status` and `describe`, ~40 ms each); it cannot meet the spec's
"20-repo dashboard < 500 ms" target, `gix` meets it with a 4× margin.

## Decision

- `gix` is the default read backend (`Settings::read_backend = Gix`).
- The CLI implementation stays as a complete fallback, selectable per
  setting, and as the oracle of the differential tests.
- Readers not implemented with `gix` (diffs, churn, remotes) use the CLI;
  this is wired in the composition root, consumers only see traits.

## Consequences

- `gix` is pinned to a minor version; a `gix` upgrade must keep
  `tests/differential.rs` green.
- If a `gix` gap appears (sparse checkout, exotic config), the composition
  root can route that one trait to the CLI without touching consumers.
- Re-run the benchmark on Linux in CI (PLAN §5.1) to confirm the margins.
