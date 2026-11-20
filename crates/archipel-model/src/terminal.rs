//! The integrated terminal: what a terminal runs, and the screen it shows.
//!
//! These types keep the terminal emulator (an adapter) out of the frontend:
//! the UI draws a [`TerminalScreen`] and sends bytes, it never sees the
//! emulator's own grid or color types.

use std::path::PathBuf;

/// A program and its arguments.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ShellCommand {
    /// The program (`bash`, `pwsh.exe`…), found on the `PATH` when relative.
    pub program: String,
    /// Its arguments.
    pub args: Vec<String>,
}

/// Size of a terminal: its grid, and the pixel size of one cell (some
/// programs ask for it).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TerminalSize {
    /// Columns.
    pub cols: u16,
    /// Rows.
    pub rows: u16,
    /// Width of a cell, in pixels.
    pub cell_width: u16,
    /// Height of a cell, in pixels.
    pub cell_height: u16,
}

impl Default for TerminalSize {
    /// The classic 80 × 24.
    fn default() -> Self {
        Self {
            cols: 80,
            rows: 24,
            cell_width: 8,
            cell_height: 16,
        }
    }
}

/// What to start in a new terminal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalSpec {
    /// Starting folder.
    pub cwd: PathBuf,
    /// What runs; `None`: the user's login shell.
    pub command: Option<ShellCommand>,
    /// Initial size (the frontend resizes it once laid out).
    pub size: TerminalSize,
    /// Lines of scrollback kept above the screen.
    pub scrollback: usize,
    /// Extra environment variables.
    pub env: Vec<(String, String)>,
}

/// A cell color.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TermColor {
    /// The default text color.
    Foreground,
    /// The default background.
    Background,
    /// A palette color: 0–15 the ANSI colors (themed), 16–255 the xterm
    /// cube and gray ramp.
    Indexed(u8),
    /// A true color.
    Rgb(u8, u8, u8),
}

/// Style flags of a cell (a tiny bit set).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct CellFlags(u16);

impl CellFlags {
    /// Nothing.
    pub const NONE: Self = Self(0);
    /// Bold.
    pub const BOLD: Self = Self(1);
    /// Italic.
    pub const ITALIC: Self = Self(1 << 1);
    /// Underlined.
    pub const UNDERLINE: Self = Self(1 << 2);
    /// Foreground and background swapped.
    pub const INVERSE: Self = Self(1 << 3);
    /// Faint.
    pub const DIM: Self = Self(1 << 4);
    /// Struck through.
    pub const STRIKE: Self = Self(1 << 5);
    /// A double-width character (the next cell is its spacer).
    pub const WIDE: Self = Self(1 << 6);
    /// The second half of a double-width character: draws nothing.
    pub const SPACER: Self = Self(1 << 7);
    /// Inside the selection.
    pub const SELECTED: Self = Self(1 << 8);
    /// Hidden text.
    pub const HIDDEN: Self = Self(1 << 9);

    /// Whether every bit of `other` is set.
    #[must_use]
    pub fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// `self` with the bits of `other` set.
    #[must_use]
    pub fn with(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

/// One cell of the screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TerminalCell {
    /// The character (a space when empty).
    pub ch: char,
    /// Text color.
    pub fg: TermColor,
    /// Background color.
    pub bg: TermColor,
    /// Style.
    pub flags: CellFlags,
}

impl Default for TerminalCell {
    fn default() -> Self {
        Self {
            ch: ' ',
            fg: TermColor::Foreground,
            bg: TermColor::Background,
            flags: CellFlags::NONE,
        }
    }
}

/// How the cursor is drawn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CursorShape {
    /// A full cell.
    #[default]
    Block,
    /// A line under the cell.
    Underline,
    /// A bar left of the cell.
    Beam,
}

/// The cursor, in screen coordinates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TerminalCursor {
    /// Where it is.
    pub at: ScreenPoint,
    /// How it is drawn.
    pub shape: CursorShape,
}

/// Terminal modes the frontend needs to encode input.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)] // independent modes, set by the program
pub struct TerminalModes {
    /// Arrow keys send application sequences (`ESC O A`).
    pub app_cursor: bool,
    /// Pasted text is wrapped in bracketed-paste markers.
    pub bracketed_paste: bool,
    /// The alternate screen (full-screen programs) is shown.
    pub alt_screen: bool,
    /// On the alternate screen, the mouse wheel sends arrow keys.
    pub alternate_scroll: bool,
}

/// A position on the visible screen: row 0 is the top line shown.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct ScreenPoint {
    /// Row, from the top of the view.
    pub row: u16,
    /// Column.
    pub col: u16,
}

/// What a terminal shows now: the visible lines (scrolled back or not),
/// row by row.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TerminalScreen {
    /// Columns.
    pub cols: u16,
    /// Rows.
    pub rows: u16,
    /// `rows × cols` cells, row by row.
    pub cells: Vec<TerminalCell>,
    /// The cursor, when shown (hidden while scrolled back).
    pub cursor: Option<TerminalCursor>,
    /// How many lines the view is scrolled back into the history.
    pub scrolled_back: usize,
    /// Lines of history above the screen.
    pub history: usize,
    /// Input modes.
    pub modes: TerminalModes,
}

impl TerminalScreen {
    /// The cells of `row` (empty past the last row).
    #[must_use]
    pub fn row(&self, row: u16) -> &[TerminalCell] {
        let cols = usize::from(self.cols);
        let start = usize::from(row) * cols;
        self.cells.get(start..start + cols).unwrap_or(&[])
    }

    /// The text of `row`, one `char` per column (spacers of wide
    /// characters left out), trailing blanks kept.
    #[must_use]
    pub fn row_text(&self, row: u16) -> String {
        self.row(row)
            .iter()
            .filter(|c| !c.flags.contains(CellFlags::SPACER))
            .map(|c| c.ch)
            .collect()
    }
}

/// How the view scrolls through the history.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TerminalScroll {
    /// Lines back into the history (negative: toward the bottom).
    Lines(i32),
    /// A page back.
    PageUp,
    /// A page toward the bottom.
    PageDown,
    /// The oldest line.
    Top,
    /// The live screen.
    Bottom,
}

/// What a selection grows by.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SelectionKind {
    /// Character by character.
    #[default]
    Simple,
    /// Word by word (a double click).
    Word,
    /// Line by line (a triple click).
    Line,
}

/// A change to the selection, in screen coordinates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectionUpdate {
    /// Starts a selection at a point (`right_half`: in the right half of
    /// its cell).
    Start {
        /// Where.
        at: ScreenPoint,
        /// By what it grows.
        kind: SelectionKind,
        /// The pointer is in the right half of the cell.
        right_half: bool,
    },
    /// Moves the free end of the selection.
    Extend {
        /// Where.
        to: ScreenPoint,
        /// The pointer is in the right half of the cell.
        right_half: bool,
    },
    /// No selection.
    Clear,
}

/// Something a terminal tells its frontend, possibly from another thread.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TerminalEvent {
    /// The screen changed: draw it again.
    Changed,
    /// The program set the title (`None`: back to the default).
    Title(Option<String>),
    /// The bell rang.
    Bell,
    /// The program asked to put text on the clipboard (OSC 52).
    Clipboard(String),
    /// The program exited, with its exit code when it has one.
    Exited(Option<i32>),
}
