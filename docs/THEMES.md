# Creating a theme

This tutorial walks you through making your own Archipel theme: a small
text file that you drop into a folder. You start from three colors, then
refine as much as you like. The file format reference is at the end.

## 1. Find the themes folder

User themes live in a `themes` folder inside Archipel's config directory:

| Platform | Folder |
|---|---|
| Linux | `~/.config/archipel/themes/` |
| macOS | `~/Library/Application Support/dev.Archipel.Archipel/themes/` |
| Windows | `%APPDATA%\Archipel\Archipel\config\themes\` |
| Any, with `ARCHIPEL_HOME` set | `$ARCHIPEL_HOME/config/themes/` |

Create the folder if it does not exist yet.

## 2. Make a theme from three colors

Create `ocean.toml` in that folder:

```toml
name = "Ocean"

[base]
background = "#0b1d2a"
text = "#d8e6f0"
accent = "#3fb6d3"
```

That is a complete theme. From these three colors Archipel derives every
other color: panels, hover and selection backgrounds, borders, muted text,
status colors, diff backgrounds, graph lanes and syntax highlighting. The
derivation works in a perceptual color space (OKLCH) and keeps text readable
against its background.

- The **file name** is the theme's id (`ocean`); `name` is what the theme
  picker shows. Without `name`, the id is shown.
- Whether the theme is **light or dark** follows from the background's
  lightness; you do not declare it.
- Colors are written `#rrggbb`, or `#rrggbbaa` with an alpha channel. The
  `#` is required; short forms like `#fff` are not accepted.

## 3. Load and select it

Themes are read when Archipel starts, so **restart Archipel** after creating
or editing a file. Then click the theme name at the right end of the status
bar (or run *Appearance and themes…* from the command palette) and pick
**Ocean**. The choice is remembered across launches.

## 4. Start from an existing theme

A theme can build on another one with `extends`. Everything the file does not
set comes from the parent:

```toml
name = "Ocean"
extends = "dark"
```

Without `extends`, the parent is `dark`. Built-in parents:

| Id | Theme |
|---|---|
| `dark` | Classic Dark |
| `light` | Classic Light |
| `high-contrast` | High Contrast |
| `quiet-cyber` | Quiet Cyber |

A parent can also be another of your themes (its file name without
`.toml`), so a variant only lists what it changes:

```toml
# ocean-compact.toml
name = "Ocean (compact)"
extends = "ocean"
density = "compact"
```

Setting any `[base]` color re-derives every color from the base (keeping
the parent's base colors you did not set), while tokens the parent
overrode explicitly stay overridden. To make a light variant of a dark
theme, extend it and change `background` and `text`. Quiet Cyber overrides
every token, so changing its `[base]` has no visible effect: extend it to
reuse its fonts and corners, or extend `dark` to re-derive colors.

## 5. Fine-tune individual colors

When a derived color is not quite what you want, override it in `[tokens]`:

```toml
[tokens]
"status.dirty" = "#f5a524"
"bg.selected" = "#123a4f"
"graph.lane.0" = "#58c4dc"
```

Everything you do not list stays derived, so start small and add tokens
only where needed. All token ids:

| Group | Tokens |
|---|---|
| Surfaces | `bg.app`, `bg.panel`, `bg.elevated` (menus, popovers), `bg.input`, `bg.hover`, `bg.selected` |
| Text | `fg.default`, `fg.muted`, `fg.subtle` (hints, placeholders), `fg.disabled` |
| Accent | `accent`, `accent.hover`, `accent.fg` (text on accent), `accent.subtle` (subtle accent background), `accent.text` (accent used as text) |
| Borders | `border.default`, `border.subtle` (separators), `border.focus` (focus ring) |
| Status | `status.dirty`, `status.ahead`, `status.behind`, `status.conflict`, `status.success`, `status.error`, `status.warning` |
| Diff | `diff.add.bg`, `diff.del.bg`, `diff.add.word.bg`, `diff.del.word.bg` (word-level highlights), `diff.add.fg`, `diff.del.fg` (text and gutter markers) |
| Graph lanes | `graph.lane.0` … `graph.lane.9` (lanes beyond 10 reuse the palette) |
| Syntax | `syntax.keyword`, `syntax.string`, `syntax.comment`, `syntax.function`, `syntax.type`, `syntax.number`, `syntax.constant`, `syntax.operator` |

Token keys can be quoted (`"bg.panel" = …`) or left unquoted
(`bg.panel = …`); both work.

For graph lanes, order the colors so neighbouring lanes contrast (for
example by alternating hue families).

## 6. Fonts, size, density, corners, animations

```toml
density = "comfortable"   # compact | comfortable | spacious
radius = 4.0              # corner radius in px, 0 to 24
animation = "normal"      # off | slow | normal | fast

[typography]
ui_font = "Inter"
mono_font = "JetBrains Mono"
size = 14.0               # base font size in px, 8 to 32
line_height = 1.5         # multiple of the size, 1.0 to 2.5
```

- **Fonts** must be installed on the system, except Jura and Hack (used by
  Quiet Cyber), which ship with Archipel; an installed copy takes precedence.
  If a font is missing, Archipel falls back to a common system font, so a
  shared theme still works on a machine without it.
- **Density** sets spacing and row heights: compact fits more rows on
  screen, spacious gives more air.
- `radius = 0.0` gives square corners.
- `animation = "off"` is also the right choice for reduced motion.

## 7. Share it

A theme is a single file: send `ocean.toml`, and the recipient drops it into
their themes folder and restarts Archipel. If it extends another user theme,
send that file too (otherwise it falls back to `dark`).

## Troubleshooting

- **The theme does not appear in the picker.** Check that the file is
  directly in the `themes` folder, ends in `.toml`, and that you restarted
  Archipel.
- **A value seems ignored.** Invalid entries (unknown key or token,
  malformed color, number out of range, unknown `density`) are dropped and
  the parent's value is used instead, so the rest of the theme still
  applies. Check the spelling against this page.
- **The whole theme looks like Classic Dark.** The file is not valid TOML
  (a missing quote or `=`, for example), or it `extends` a theme that does
  not exist. A broken theme file never prevents Archipel from starting.
- **The file is ignored entirely.** Its name is a built-in id (`dark.toml`,
  `light.toml`, `high-contrast.toml`, `quiet-cyber.toml`); rename it.
- **The accent differs in some windows.** A project's own color (*Set the
  project color…*) replaces the theme's accent in that project's window, and
  its optional background tint shifts the surfaces' hue. Both sit on top of
  your theme.

## Reference

Every key is optional:

```toml
name = "Midnight"         # shown in the theme picker (default: the file name)
extends = "dark"          # parent theme id (default: dark)
density = "compact"       # compact | comfortable | spacious
radius = 4.0              # corner radius in px, 0 to 24
animation = "off"         # off | slow | normal | fast

[base]                    # every non-overridden color is derived from these
background = "#101418"
text = "#e6e6e6"
accent = "#7aa2f7"

[typography]
ui_font = "Inter"
mono_font = "JetBrains Mono"
size = 13.0               # px, 8 to 32
line_height = 1.45        # 1.0 to 2.5

[tokens]                  # individual color overrides by token id
"bg.panel" = "#0c0f12"
```

For a complete example that sets every token, see the built-in Quiet Cyber
theme, which is written in this same format:
[crates/archipel-theme/themes/quiet-cyber.toml](../crates/archipel-theme/themes/quiet-cyber.toml).
