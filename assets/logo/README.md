# Archipel logo

An "A" drawn as a git graph: two lanes branch from one commit (apex), and a merge forms the crossbar.

| File | Use |
|---|---|
| `archipel.svg` | master, 48 px and up |
| `archipel-small.svg` | hinted for 16–32 px (heavier strokes, straight crossbar) |
| `archipel.ico` | Windows icon (16–256 px), embedded as resource 1 by `crates/archipel-ui/build.rs` |
| `png/` | raster exports (32–1024 px) |
| `preview.html` | both SVGs at every size on light and dark |

Palette: background `#12202E`, left lane `#4FC3F7`, right lane `#F6AD55`, merge `#7EE0B5`, root commit `#E8EEF4`.

The ICO and PNGs are rendered with resvg: sizes up to 32 px come from `archipel-small.svg`, larger ones from `archipel.svg`.
