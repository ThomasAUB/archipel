//! Fake terminals: record what was launched and typed, echo the typed text
//! on the screen, and let tests send events.

use std::sync::{Arc, Mutex};

use super::lock;
use crate::error::TerminalError;
use crate::ports::{
    TerminalInput, TerminalLauncher, TerminalScreenReader, TerminalSession, TerminalSink,
};
use crate::terminal::{
    SelectionUpdate, TerminalCell, TerminalEvent, TerminalScreen, TerminalScroll, TerminalSize,
    TerminalSpec,
};

/// Fake launcher: every launch succeeds unless [`Self::fail`] is set.
#[derive(Default)]
pub struct FakeTerminals {
    launched: Mutex<Vec<(TerminalSpec, Arc<FakeTerminal>)>>,
    /// Makes every launch fail with this message.
    pub fail: Option<String>,
}

impl FakeTerminals {
    /// The specs launched so far, in order.
    #[must_use]
    pub fn specs(&self) -> Vec<TerminalSpec> {
        lock(&self.launched)
            .iter()
            .map(|(s, _)| s.clone())
            .collect()
    }

    /// The `i`-th terminal launched.
    #[must_use]
    pub fn terminal(&self, i: usize) -> Option<Arc<FakeTerminal>> {
        lock(&self.launched).get(i).map(|(_, t)| t.clone())
    }
}

impl TerminalLauncher for FakeTerminals {
    fn launch(
        &self,
        spec: TerminalSpec,
        sink: Arc<dyn TerminalSink>,
    ) -> Result<Arc<dyn TerminalSession>, TerminalError> {
        if let Some(message) = &self.fail {
            return Err(TerminalError::Spawn(message.clone()));
        }
        let terminal = Arc::new(FakeTerminal {
            sink,
            state: Mutex::new(FakeState {
                size: spec.size,
                ..FakeState::default()
            }),
        });
        lock(&self.launched).push((spec, terminal.clone()));
        Ok(terminal)
    }
}

#[derive(Default)]
struct FakeState {
    size: TerminalSize,
    written: Vec<u8>,
    selection: Option<SelectionUpdate>,
    scrolled: Vec<TerminalScroll>,
}

/// A fake running terminal.
pub struct FakeTerminal {
    sink: Arc<dyn TerminalSink>,
    state: Mutex<FakeState>,
}

impl FakeTerminal {
    /// Every byte written so far.
    #[must_use]
    pub fn written(&self) -> Vec<u8> {
        lock(&self.state).written.clone()
    }

    /// The current size.
    #[must_use]
    pub fn size(&self) -> TerminalSize {
        lock(&self.state).size
    }

    /// The scroll requests so far.
    #[must_use]
    pub fn scrolled(&self) -> Vec<TerminalScroll> {
        lock(&self.state).scrolled.clone()
    }

    /// Sends `event` to the frontend, as the program would.
    pub fn emit(&self, event: TerminalEvent) {
        self.sink.event(event);
    }
}

impl TerminalInput for FakeTerminal {
    fn write(&self, bytes: &[u8]) {
        lock(&self.state).written.extend_from_slice(bytes);
        self.sink.event(TerminalEvent::Changed);
    }

    fn resize(&self, size: TerminalSize) {
        lock(&self.state).size = size;
    }
}

impl TerminalScreenReader for FakeTerminal {
    /// The printable bytes written so far, wrapped at the width.
    fn screen(&self) -> TerminalScreen {
        let state = lock(&self.state);
        let (cols, rows) = (state.size.cols, state.size.rows);
        let mut cells = vec![TerminalCell::default(); usize::from(cols) * usize::from(rows)];
        let printable = state
            .written
            .iter()
            .filter(|b| b.is_ascii_graphic() || **b == b' ');
        for (cell, b) in cells.iter_mut().zip(printable) {
            cell.ch = char::from(*b);
        }
        TerminalScreen {
            cols,
            rows,
            cells,
            ..TerminalScreen::default()
        }
    }

    fn scroll(&self, scroll: TerminalScroll) {
        lock(&self.state).scrolled.push(scroll);
    }

    fn select(&self, update: SelectionUpdate) {
        lock(&self.state).selection = (update != SelectionUpdate::Clear).then_some(update);
    }

    fn selected_text(&self) -> Option<String> {
        lock(&self.state).selection.map(|_| "selected".to_owned())
    }
}
