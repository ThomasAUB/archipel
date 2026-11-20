# ADR 0005 — Integrated terminal: alacritty_terminal behind terminal ports

- Status: accepted
- Date: 2026-11-20
- Context: issue #98, SPEC §11.

## Context

Anything Archipel does not cover (a one-off git command, a build, a script)
meant switching to an external terminal that knows nothing of the repo
selected in Archipel. The app needs a terminal panel: a real shell on a
pseudo-terminal (ConPTY on Windows), a VT emulator (colours, cursor
movement, alternate screen, scrollback, selection), drawn with GPUI — within
the RAM target, and without the UI depending on an emulator's types.

## Decision

- **One adapter crate, `archipel-term`**, wraps `alacritty_terminal` (the
  emulator Zed also uses). It brings its own PTY layer (Unix PTY, Windows
  ConPTY) and I/O event loop, so `portable-pty` (suggested in the issue)
  would only add a second PTY stack. `xtask` lists `alacritty_terminal` as
  wrapped by `archipel-term` only.
- **Ports in `archipel-model`**: `TerminalLauncher` (start a `TerminalSpec`,
  events to a `TerminalSink`), `TerminalInput` (write, resize) and
  `TerminalScreenReader` (screen, scroll, select, selected text);
  `TerminalSession` is both. The screen crosses the boundary as
  `TerminalScreen` (cells with `TermColor` and `CellFlags`, cursor, modes),
  never as the emulator's grid. `FakeTerminals` serves the consumers' tests.
- **`archipel-app`** knows which shell runs (`terminal_shell`, else the login
  shell) and how a one-off command is passed to it (`-c`, `/C`, `-Command`),
  and names tabs `repo · branch`. The launcher is `Services::terminals`.
- **`archipel-ui`** owns the panel and the drawing: one `TerminalView` per
  tab paints the cells in a canvas, each style run placed at its column so
  the grid stays aligned whatever the font. Key encoding (xterm sequences,
  bracketed paste) and link detection are pure, tested functions there.
- **Keys**: app shortcuts are bound without context, which ranks them above
  any view. The keymap strokes a shell needs (Ctrl + letter, Alt, unmodified
  keys) are also bound to `NoAction` in the `Terminal` context, after the
  keymap, so they reach the terminal when it is focused.
- **Memory**: nothing is spawned before the first terminal is opened;
  scrollback is capped (`terminal_scrollback`, default 2000 lines);
  closing a tab drops the session, which hangs the program up.

## Consequences

- A fork can swap the emulator (or start remote shells) by implementing the
  terminal ports and wiring them in `archipel_compose::compose`.
- Not covered yet: IME composition (keys arrive as typed characters), mouse
  reporting to full-screen programs, split views.
