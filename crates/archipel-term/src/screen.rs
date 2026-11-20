//! The emulator's visible grid as a [`TerminalScreen`].

use alacritty_terminal::event::EventListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::color::Colors;
use alacritty_terminal::term::{Term, TermMode};
use alacritty_terminal::vte::ansi::{Color, CursorShape as Shape, NamedColor};
use archipel_model::terminal::{
    CellFlags, CursorShape, ScreenPoint, TermColor, TerminalCell, TerminalCursor, TerminalModes,
    TerminalScreen,
};

/// The screen as shown now (scrolled back or not).
pub(crate) fn snapshot<L: EventListener>(term: &Term<L>) -> TerminalScreen {
    let cols = term.columns();
    let rows = term.screen_lines();
    let content = term.renderable_content();
    let offset = content.display_offset;
    let shift = i64::try_from(offset).unwrap_or(i64::MAX);
    let mut cells = vec![TerminalCell::default(); cols * rows];
    for indexed in content.display_iter {
        let Ok(row) = usize::try_from(i64::from(indexed.point.line.0) + shift) else {
            continue;
        };
        let col = indexed.point.column.0;
        if row >= rows || col >= cols {
            continue;
        }
        let cell = indexed.cell;
        let mut flags = cell_flags(cell.flags);
        if content.selection.is_some_and(|s| s.contains(indexed.point)) {
            flags = flags.with(CellFlags::SELECTED);
        }
        cells[row * cols + col] = TerminalCell {
            ch: cell.c,
            fg: color(cell.fg, content.colors),
            bg: color(cell.bg, content.colors),
            flags,
        };
    }
    let cursor_row = i64::from(content.cursor.point.line.0) + shift;
    let cursor = match content.cursor.shape {
        Shape::Hidden => None,
        Shape::Block | Shape::HollowBlock => Some(CursorShape::Block),
        Shape::Underline => Some(CursorShape::Underline),
        Shape::Beam => Some(CursorShape::Beam),
    }
    .zip(u16::try_from(cursor_row).ok())
    .filter(|(_, row)| usize::from(*row) < rows)
    .map(|(shape, row)| TerminalCursor {
        at: ScreenPoint {
            row,
            col: u16::try_from(content.cursor.point.column.0).unwrap_or(u16::MAX),
        },
        shape,
    });
    let mode = content.mode;
    TerminalScreen {
        cols: u16::try_from(cols).unwrap_or(u16::MAX),
        rows: u16::try_from(rows).unwrap_or(u16::MAX),
        cells,
        cursor,
        scrolled_back: offset,
        history: term.grid().history_size(),
        modes: TerminalModes {
            app_cursor: mode.contains(TermMode::APP_CURSOR),
            bracketed_paste: mode.contains(TermMode::BRACKETED_PASTE),
            alt_screen: mode.contains(TermMode::ALT_SCREEN),
            alternate_scroll: mode.contains(TermMode::ALTERNATE_SCROLL),
        },
    }
}

fn cell_flags(flags: Flags) -> CellFlags {
    [
        (Flags::BOLD, CellFlags::BOLD),
        (Flags::ITALIC, CellFlags::ITALIC),
        (Flags::INVERSE, CellFlags::INVERSE),
        (Flags::DIM, CellFlags::DIM),
        (Flags::STRIKEOUT, CellFlags::STRIKE),
        (Flags::WIDE_CHAR, CellFlags::WIDE),
        (Flags::WIDE_CHAR_SPACER, CellFlags::SPACER),
        (Flags::LEADING_WIDE_CHAR_SPACER, CellFlags::SPACER),
        (Flags::HIDDEN, CellFlags::HIDDEN),
    ]
    .into_iter()
    .chain(
        flags
            .intersects(Flags::ALL_UNDERLINES)
            .then_some((Flags::empty(), CellFlags::UNDERLINE)),
    )
    .filter(|(from, _)| flags.contains(*from))
    .fold(CellFlags::NONE, |acc, (_, to)| acc.with(to))
}

/// A cell color. The 16 ANSI colors stay indexed (the frontend themes
/// them) unless the program redefined them (OSC 4).
fn color(c: Color, overrides: &Colors) -> TermColor {
    let indexed = |i: u8| match overrides[usize::from(i)] {
        Some(rgb) => TermColor::Rgb(rgb.r, rgb.g, rgb.b),
        None => TermColor::Indexed(i),
    };
    match c {
        Color::Spec(rgb) => TermColor::Rgb(rgb.r, rgb.g, rgb.b),
        Color::Indexed(i) => indexed(i),
        Color::Named(named) => match named {
            NamedColor::Background => TermColor::Background,
            NamedColor::Foreground
            | NamedColor::Cursor
            | NamedColor::BrightForeground
            | NamedColor::DimForeground => TermColor::Foreground,
            other => {
                let n = other as usize;
                // Dim colors (DimBlack…) map to their normal color.
                let i = if n < 16 {
                    n
                } else {
                    n - NamedColor::DimBlack as usize
                };
                indexed(u8::try_from(i).unwrap_or(7))
            }
        },
    }
}
