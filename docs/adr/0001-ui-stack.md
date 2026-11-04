# ADR 0001 — UI stack: GPUI (via `gpui-kit`)

- Status: accepted
- Date: 2026-11-04
- Context: PLAN §2.2 and Phase 0 steps 4–5.

## Context

PLAN §2.2 picks GPUI and keeps it "unless it misses the RAM or scrolling
criteria": idle RSS ≤ 100 MB for the spike, and smooth scrolling of a
100k-row virtualized list. Tauri was the comparison; egui the fallback.

## Spike

A release build of a GPUI window rendering a 100k-row virtualized list
(`uniform_list`), measured with the process RSS 6 s after start (Windows 11,
integrated + discrete GPU laptop):

| Variant | Idle RSS |
|---|---:|
| `gpui-kit` 0.9 with components and icon assets (first ever launch, cold shader/font caches) | 96 MiB |
| same, warm runs | 58–60 MiB |
| `gpui-kit` without components/assets, warm runs | 57–58 MiB |

The component library costs nothing at idle. The headless core with a
20-repo project open adds ~25 MiB (`archipel-cli bench`), so the full app is
expected around 85–95 MiB, under the 150 MB target.

The Tauri spike was not built: the decision rule only calls for it when GPUI
misses a criterion, and a WebView2 alone is documented at 80–150 MB.

## Full application (release build)

`archipel.exe` with a 20-repo project open (Windows 11, warm runs, medians
of 9 launches with `tools/bench-startup.ps1`):

| Measurement | Value | Spec target |
|---|---:|---|
| Idle RSS, sampled every 5 s for 30 s | 75 MiB, flat | < 150 MB, no growth |
| Process launch → first frame (dashboard painted) | 360–390 ms | < 1 s |
| First launch after a new build (antivirus scan) | ~1.1 s | |

### How startup was brought down

A first measurement (`Start-Process`, then polling for the window handle)
reported 1.1 s; that method adds ~0.5 s of polling latency. The app now
prints its own milestones (`ARCHIPEL_STARTUP_TRACE=1`) and the benchmark
measures launch → first frame from them. Launch → `main` is ~20 ms (a
trivial executable: ~10 ms), so loading the 35 MB binary and its DLLs is
not the problem. Then, step by step (median launch → first frame):

| Change | Before | After |
|---|---:|---|
| Theme font "Inter" is not installed on Windows: every text run searched for a fallback (first frame ~230 ms). Fonts are now resolved once against installed families, with platform fallbacks (Segoe UI / Consolas…) | 609 ms | 367 ms |
| `compose` (git version check 84 ms, SQLite, watcher) and reopening the last project run on a thread while GPUI initializes; A/B: sequential 515 ms vs parallel 356–377 ms | 515 ms | ~370 ms |

The font fix also lowered idle RSS from 111 MiB to 75 MiB. What remains is
GPUI's platform and window creation (~130 ms + ~100 ms).

## Decision

- GPUI through **`gpui-kit` 0.9** (a pinned GPUI snapshot, `gpui-pre`
  0.3.11, plus `gpui-component` widgets: inputs, buttons, menus).
- All GPUI code lives in `crates/archipel-ui`; the view-models stay in
  `archipel-app`, so a fallback to egui only rewrites the UI crate.
- Colors come only from `archipel-theme` tokens through `archipel-ui`'s
  `theme` module (`cargo xtask check-colors`).

## Consequences

- First build of the UI crate is long (~7 min release on this machine).
- `gpui-kit` pins its GPUI snapshot; upgrades are deliberate and re-run the
  spike measurements (`archipel-ui` idle RSS is logged at startup).
- Linux needs the Vulkan / Wayland / X11 development packages listed in CI.
