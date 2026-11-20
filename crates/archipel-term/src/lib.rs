//! # archipel-term
//!
//! The integrated terminal's adapter: implements the terminal ports of
//! `archipel-model` ([`TerminalLauncher`], [`TerminalSession`]) with
//! `alacritty_terminal`, which runs the program on a pseudo-terminal (a Unix
//! PTY, `ConPTY` on Windows) and emulates the terminal (VT parser, grid,
//! scrollback, selection).
//!
//! * Dependencies: `archipel-model` only; no emulator type leaves this crate
//!   (the screen is converted to [`TerminalScreen`]).
//! * Threads: each terminal has one I/O thread (alacritty's event loop),
//!   which parses the output and sends [`TerminalEvent`]s to the sink.
//!   Every port method only locks the terminal state briefly.
//! * Lifetime: dropping the session shuts the event loop down, which hangs
//!   the program up.

mod screen;

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::event_loop::{EventLoop, EventLoopSender, Msg};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::tty;
use archipel_model::TerminalError;
use archipel_model::ports::{
    TerminalInput, TerminalLauncher, TerminalScreenReader, TerminalSession, TerminalSink,
};
use archipel_model::terminal::{
    ScreenPoint, SelectionKind, SelectionUpdate, TerminalEvent, TerminalScreen, TerminalScroll,
    TerminalSize, TerminalSpec,
};

/// Starts terminals on pseudo-terminals.
#[derive(Clone, Copy, Debug, Default)]
pub struct PtyTerminals;

impl PtyTerminals {
    /// The launcher.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

/// Environment every terminal gets, unless the spec sets it.
const DEFAULT_ENV: &[(&str, &str)] = &[
    ("TERM", "xterm-256color"),
    ("COLORTERM", "truecolor"),
    ("TERM_PROGRAM", "Archipel"),
];

impl TerminalLauncher for PtyTerminals {
    fn launch(
        &self,
        spec: TerminalSpec,
        sink: Arc<dyn TerminalSink>,
    ) -> Result<Arc<dyn TerminalSession>, TerminalError> {
        let size = Grid::from(spec.size);
        let config = Config {
            scrolling_history: spec.scrollback,
            ..Config::default()
        };
        let listener = Listener {
            sink,
            sender: Arc::new(OnceLock::new()),
            exit_code: Arc::new(Mutex::new(None)),
        };
        let term = Arc::new(FairMutex::new(Term::new(config, &size, listener.clone())));
        let mut env: HashMap<String, String> = spec.env.into_iter().collect();
        for (key, value) in DEFAULT_ENV {
            env.entry((*key).to_owned())
                .or_insert_with(|| (*value).to_owned());
        }
        let options = tty::Options {
            shell: spec.command.map(|c| tty::Shell::new(c.program, c.args)),
            working_directory: Some(spec.cwd),
            drain_on_exit: true,
            env,
            #[cfg(target_os = "windows")]
            escape_args: true,
        };
        let pty = tty::new(&options, window_size(spec.size), 0)
            .map_err(|e| TerminalError::Spawn(e.to_string()))?;
        let event_loop = EventLoop::new(term.clone(), listener.clone(), pty, true, false)
            .map_err(|e| TerminalError::Spawn(e.to_string()))?;
        let channel = event_loop.channel();
        // Set once, before the loop runs: nothing can have set it.
        let _ = listener.sender.set(channel.clone());
        event_loop.spawn();
        Ok(Arc::new(PtySession { term, channel }))
    }
}

/// A grid size, as the emulator takes it.
#[derive(Clone, Copy, Debug)]
struct Grid {
    cols: usize,
    rows: usize,
}

impl From<TerminalSize> for Grid {
    fn from(size: TerminalSize) -> Self {
        Self {
            cols: usize::from(size.cols.max(2)),
            rows: usize::from(size.rows.max(1)),
        }
    }
}

impl Dimensions for Grid {
    fn total_lines(&self) -> usize {
        self.rows
    }

    fn screen_lines(&self) -> usize {
        self.rows
    }

    fn columns(&self) -> usize {
        self.cols
    }
}

fn window_size(size: TerminalSize) -> WindowSize {
    let grid = Grid::from(size);
    WindowSize {
        num_lines: u16::try_from(grid.rows).unwrap_or(u16::MAX),
        num_cols: u16::try_from(grid.cols).unwrap_or(u16::MAX),
        cell_width: size.cell_width.max(1),
        cell_height: size.cell_height.max(1),
    }
}

/// Turns the emulator's events into [`TerminalEvent`]s, and answers the
/// program's queries (cursor position, device attributes) on the PTY.
#[derive(Clone)]
struct Listener {
    sink: Arc<dyn TerminalSink>,
    /// The event loop's input channel, set once it exists.
    sender: Arc<OnceLock<EventLoopSender>>,
    /// The program's exit code, between its exit and the end of the loop
    /// (which first reads the last output).
    exit_code: Arc<Mutex<Option<i32>>>,
}

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        let out = match event {
            Event::Wakeup => TerminalEvent::Changed,
            Event::Title(title) => TerminalEvent::Title(Some(title)),
            Event::ResetTitle => TerminalEvent::Title(None),
            Event::Bell => TerminalEvent::Bell,
            Event::ClipboardStore(_, text) => TerminalEvent::Clipboard(text),
            Event::PtyWrite(text) => {
                if let Some(sender) = self.sender.get()
                    && let Err(e) = sender.send(Msg::Input(Cow::Owned(text.into_bytes())))
                {
                    tracing::debug!(error = ?e, "terminal reply not sent");
                }
                return;
            }
            Event::ChildExit(status) => {
                *self
                    .exit_code
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner) = status.code();
                return;
            }
            Event::Exit => TerminalEvent::Exited(
                *self
                    .exit_code
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner),
            ),
            Event::MouseCursorDirty
            | Event::ClipboardLoad(..)
            | Event::ColorRequest(..)
            | Event::TextAreaSizeRequest(_)
            | Event::CursorBlinkingChange => return,
        };
        self.sink.event(out);
    }
}

/// A running terminal.
struct PtySession {
    term: Arc<FairMutex<Term<Listener>>>,
    channel: EventLoopSender,
}

impl PtySession {
    /// `at` (screen coordinates) in the grid, as the view is scrolled now.
    fn grid_point(term: &Term<Listener>, at: ScreenPoint) -> Point {
        let offset = i32::try_from(term.grid().display_offset()).unwrap_or(i32::MAX);
        let last_col = term.columns().saturating_sub(1);
        Point::new(
            Line(i32::from(at.row) - offset),
            Column(usize::from(at.col).min(last_col)),
        )
    }
}

impl Drop for PtySession {
    fn drop(&mut self) {
        // The loop is gone when the program already exited.
        let _ = self.channel.send(Msg::Shutdown);
    }
}

impl TerminalInput for PtySession {
    fn write(&self, bytes: &[u8]) {
        if let Err(e) = self.channel.send(Msg::Input(Cow::Owned(bytes.to_vec()))) {
            tracing::debug!(error = ?e, "terminal input dropped (program exited)");
        }
    }

    fn resize(&self, size: TerminalSize) {
        self.term.lock().resize(Grid::from(size));
        if let Err(e) = self.channel.send(Msg::Resize(window_size(size))) {
            tracing::debug!(error = ?e, "terminal resize dropped (program exited)");
        }
    }
}

impl TerminalScreenReader for PtySession {
    fn screen(&self) -> TerminalScreen {
        screen::snapshot(&self.term.lock())
    }

    fn scroll(&self, scroll: TerminalScroll) {
        self.term.lock().scroll_display(match scroll {
            TerminalScroll::Lines(n) => Scroll::Delta(n),
            TerminalScroll::PageUp => Scroll::PageUp,
            TerminalScroll::PageDown => Scroll::PageDown,
            TerminalScroll::Top => Scroll::Top,
            TerminalScroll::Bottom => Scroll::Bottom,
        });
    }

    fn select(&self, update: SelectionUpdate) {
        let mut term = self.term.lock();
        let side = |right: bool| if right { Side::Right } else { Side::Left };
        match update {
            SelectionUpdate::Start {
                at,
                kind,
                right_half,
            } => {
                let ty = match kind {
                    SelectionKind::Simple => SelectionType::Simple,
                    SelectionKind::Word => SelectionType::Semantic,
                    SelectionKind::Line => SelectionType::Lines,
                };
                let point = Self::grid_point(&term, at);
                term.selection = Some(Selection::new(ty, point, side(right_half)));
            }
            SelectionUpdate::Extend { to, right_half } => {
                let point = Self::grid_point(&term, to);
                if let Some(selection) = term.selection.as_mut() {
                    selection.update(point, side(right_half));
                }
            }
            SelectionUpdate::Clear => term.selection = None,
        }
    }

    fn selected_text(&self) -> Option<String> {
        self.term
            .lock()
            .selection_to_string()
            .filter(|t| !t.is_empty())
    }
}

#[cfg(test)]
mod tests;
