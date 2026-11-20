//! Terminals: a shell (or a command) on a pseudo-terminal, with a terminal
//! emulator turning its output into a screen.

use std::sync::Arc;

use crate::error::TerminalError;
use crate::terminal::{
    SelectionUpdate, TerminalEvent, TerminalScreen, TerminalScroll, TerminalSize, TerminalSpec,
};

/// Receives the events of a terminal, from the terminal's own thread. Must
/// not block.
pub trait TerminalSink: Send + Sync {
    /// Called for every event, in order.
    fn event(&self, event: TerminalEvent);
}

impl<F: Fn(TerminalEvent) + Send + Sync> TerminalSink for F {
    fn event(&self, event: TerminalEvent) {
        self(event);
    }
}

/// Starts terminals. Nothing runs until [`launch`](Self::launch) is called:
/// a frontend starts a terminal when it is first shown.
pub trait TerminalLauncher: Send + Sync {
    /// Starts `spec`; its events go to `sink` until the session is dropped.
    /// Dropping the session ends the program (hang-up).
    fn launch(
        &self,
        spec: TerminalSpec,
        sink: Arc<dyn TerminalSink>,
    ) -> Result<Arc<dyn TerminalSession>, TerminalError>;
}

/// Input side of a running terminal. Every call returns at once.
pub trait TerminalInput: Send + Sync {
    /// Sends bytes to the program, as typed.
    fn write(&self, bytes: &[u8]);
    /// Resizes the grid and tells the program.
    fn resize(&self, size: TerminalSize);
}

/// Screen side of a running terminal: what to draw, and the view's own
/// state (scroll position, selection). Every call returns at once.
pub trait TerminalScreenReader: Send + Sync {
    /// The visible screen now.
    fn screen(&self) -> TerminalScreen;
    /// Scrolls the view through the history.
    fn scroll(&self, scroll: TerminalScroll);
    /// Changes the selection.
    fn select(&self, update: SelectionUpdate);
    /// The selected text, if any.
    fn selected_text(&self) -> Option<String>;
}

/// A running terminal.
pub trait TerminalSession: TerminalInput + TerminalScreenReader {}

impl<T: TerminalInput + TerminalScreenReader> TerminalSession for T {}
