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
