//! One terminal: draws its screen cell by cell, sends the keys typed,
//! selects with the mouse, scrolls through the history and opens the URL
//! or path under a Ctrl+click.

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use archipel_app::{App as Core, AppError};
use archipel_model::ports::TerminalSession;
use archipel_model::terminal::{
    CellFlags, CursorShape, ScreenPoint, SelectionKind, SelectionUpdate, TerminalCell,
    TerminalEvent, TerminalModes, TerminalScreen, TerminalScroll, TerminalSize, TerminalSpec,
};
use gpui_kit::prelude::*;
use gpui_kit::{
    App, Bounds, ClipboardItem, Context, EventEmitter, FocusHandle, Font, FontStyle, FontWeight,
    KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point,
    ScrollEvent, SharedString, StrikethroughStyle, Task, TextAlign, TextRun, UnderlineStyle,
    Window, canvas, div, fill, font, outline, point, px, size,
};

use super::input::{self, Key};
use super::links::{self, Link};
use crate::theme::{TerminalColors, mono_font, text_size};

/// Line height, in font sizes.
const LINE_HEIGHT: f32 = 1.3;

/// What a terminal tells its panel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TerminalViewEvent {
    /// The program exited, with its exit code when it has one.
    Exited(Option<i32>),
    /// The program asked to put text on the clipboard.
    Copied,
}

impl EventEmitter<TerminalViewEvent> for TerminalView {}

/// How a terminal's program ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ended {
    /// With this exit code.
    Code(i32),
    /// Without one (killed by a signal).
    Killed,
}

/// Where the grid was painted, for the pointer.
#[derive(Clone, Copy, Debug)]
struct GridLayout {
    origin: Point<Pixels>,
    cell: gpui_kit::Size<Pixels>,
}

/// One running terminal.
pub struct TerminalView {
    session: Arc<dyn TerminalSession>,
    focus: FocusHandle,
    /// Its starting folder (relative paths in the output resolve from it).
    cwd: PathBuf,
    /// How the program ended, once it did.
    exited: Option<Ended>,
    /// Modes of the last screen drawn.
    modes: TerminalModes,
    /// Where the last paint put the grid.
    layout: Rc<Cell<Option<GridLayout>>>,
    /// Size last given to the program.
    size: Rc<Cell<TerminalSize>>,
    /// A left button press is selecting, and whether it moved yet.
    selecting: Option<bool>,
    /// Wheel movement not yet worth a whole line.
    wheel: f32,
    _events: Task<()>,
}

/// A terminal started, before its view exists.
pub struct Started {
    session: Arc<dyn TerminalSession>,
    events: tokio::sync::mpsc::UnboundedReceiver<TerminalEvent>,
    cwd: PathBuf,
    size: TerminalSize,
}

/// Starts `spec`.
pub fn start(core: &Core, spec: TerminalSpec) -> Result<Started, AppError> {
    let (tx, events) = tokio::sync::mpsc::unbounded_channel();
    let sink = Arc::new(move |event: TerminalEvent| {
        // The view is gone: nobody to tell.
        let _ = tx.send(event);
    });
    let (cwd, size) = (spec.cwd.clone(), spec.size);
    let session = core.launch_terminal(spec, sink)?;
    Ok(Started {
        session,
        events,
        cwd,
        size,
    })
}

impl TerminalView {
    /// The view of a started terminal.
    pub fn new(started: Started, cx: &mut Context<Self>) -> Self {
        let Started {
            session,
            events: mut rx,
            cwd,
            size,
        } = started;
        let events = cx.spawn(async move |this, cx| {
            while let Some(first) = rx.recv().await {
                let mut batch = vec![first];
                while let Ok(more) = rx.try_recv() {
                    batch.push(more);
                }
                if this.update(cx, |v, cx| v.on_events(batch, cx)).is_err() {
                    break;
                }
            }
        });
        Self {
            session,
            focus: cx.focus_handle(),
            cwd,
            exited: None,
            modes: TerminalModes::default(),
            layout: Rc::new(Cell::new(None)),
            size: Rc::new(Cell::new(size)),
            selecting: None,
            wheel: 0.0,
            _events: events,
        }
    }

    /// The folder it started in.
    pub fn cwd(&self) -> &std::path::Path {
        &self.cwd
    }

    /// How the program ended, once it did.
    pub fn exited(&self) -> Option<Ended> {
        self.exited
    }

    /// The grid size last given to the program.
    pub fn size(&self) -> TerminalSize {
        self.size.get()
    }

    fn on_events(&mut self, events: Vec<TerminalEvent>, cx: &mut Context<Self>) {
        for event in events {
            match event {
                TerminalEvent::Changed | TerminalEvent::Title(_) | TerminalEvent::Bell => {}
                TerminalEvent::Clipboard(text) => {
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                    cx.emit(TerminalViewEvent::Copied);
                }
                TerminalEvent::Exited(code) => {
                    self.exited = Some(code.map_or(Ended::Killed, Ended::Code));
                    cx.emit(TerminalViewEvent::Exited(code));
                }
            }
        }
        cx.notify();
    }

    /// Sends `bytes` as typed: back to the live screen, selection dropped.
    fn send(&mut self, bytes: &[u8], cx: &mut Context<Self>) {
        self.session.scroll(TerminalScroll::Bottom);
        self.session.select(SelectionUpdate::Clear);
        self.session.write(bytes);
        cx.notify();
    }

    fn copy(&self, cx: &mut Context<Self>) {
        if let Some(text) = self.session.selected_text() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    fn paste(&mut self, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|c| c.text()) {
            let bytes = input::paste(&text, self.modes);
            self.send(&bytes, cx);
        }
    }

    fn on_key(&mut self, e: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let k = &e.keystroke;
        let m = k.modifiers;
        let mac = cfg!(target_os = "macos");
        let (copy, paste) = if mac {
            (m.platform && k.key == "c", m.platform && k.key == "v")
        } else {
            (
                m.control && m.shift && k.key == "c",
                m.control && m.shift && k.key == "v" || m.shift && k.key == "insert",
            )
        };
        if copy || paste {
            if copy {
                self.copy(cx);
            } else {
                self.paste(cx);
            }
            cx.stop_propagation();
            return;
        }
        // Shift + paging keys scroll the history, outside full-screen programs.
        if m.shift && !m.control && !m.alt && !self.modes.alt_screen {
            let scroll = match k.key.as_str() {
                "pageup" => Some(TerminalScroll::PageUp),
                "pagedown" => Some(TerminalScroll::PageDown),
                "home" => Some(TerminalScroll::Top),
                "end" => Some(TerminalScroll::Bottom),
                _ => None,
            };
            if let Some(scroll) = scroll {
                self.session.scroll(scroll);
                cx.stop_propagation();
                cx.notify();
                return;
            }
        }
        // Command shortcuts stay the app's on macOS.
        if m.platform {
            return;
        }
        let typed = k.key_char.as_deref();
        // Option composes characters on macOS: send what it typed.
        let alt = m.alt && !(mac && typed.is_some() && !m.control);
        let key = Key {
            name: &k.key,
            typed,
            ctrl: m.control,
            alt,
            shift: m.shift,
        };
        if let Some(bytes) = input::encode(key, self.modes) {
            self.send(&bytes, cx);
            cx.stop_propagation();
        }
    }

    /// The cell under `position`, and whether in its right half.
    fn cell_at(&self, position: Point<Pixels>) -> Option<(ScreenPoint, bool)> {
        let layout = self.layout.get()?;
        let size = self.size.get();
        let x = f32::from(position.x - layout.origin.x) / f32::from(layout.cell.width);
        let y = f32::from(position.y - layout.origin.y) / f32::from(layout.cell.height);
        let clamp = |v: f32, n: u16| {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // clamped
            let i = v.floor().clamp(0.0, f32::from(n.saturating_sub(1))) as u16;
            i
        };
        Some((
            ScreenPoint {
                row: clamp(y, size.rows),
                col: clamp(x, size.cols),
            },
            x.fract() >= 0.5,
        ))
    }

    fn on_mouse_down(&mut self, e: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.focus.focus(window, cx);
        let Some((at, right_half)) = self.cell_at(e.position) else {
            return;
        };
        if e.modifiers.secondary(cx) {
            self.open_link(at, cx);
            return;
        }
        let update = if e.modifiers.shift && self.session.selected_text().is_some() {
            SelectionUpdate::Extend { to: at, right_half }
        } else {
            let kind = match e.click_count {
                0 | 1 => SelectionKind::Simple,
                2 => SelectionKind::Word,
                _ => SelectionKind::Line,
            };
            SelectionUpdate::Start {
                at,
                kind,
                right_half,
            }
        };
        self.session.select(update);
        // A double or triple click selected something already.
        self.selecting = Some(e.click_count > 1 || e.modifiers.shift);
        cx.notify();
    }

    fn on_mouse_move(&mut self, e: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.selecting.is_none() || e.pressed_button != Some(MouseButton::Left) {
            return;
        }
        if let Some((to, right_half)) = self.cell_at(e.position) {
            self.session
                .select(SelectionUpdate::Extend { to, right_half });
            self.selecting = Some(true);
            cx.notify();
        }
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        // A plain click selects nothing.
        if self.selecting.take() == Some(false) {
            self.session.select(SelectionUpdate::Clear);
            cx.notify();
        }
    }

    fn on_scroll(&mut self, e: &ScrollEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(layout) = self.layout.get() else {
            return;
        };
        let line = layout.cell.height;
        self.wheel += f32::from(e.delta.pixel_delta(line).y) / f32::from(line);
        let lines = self.wheel.trunc();
        self.wheel -= lines;
        #[allow(clippy::cast_possible_truncation)] // a few lines per event
        let lines = lines as i32;
        if lines == 0 {
            return;
        }
        if self.modes.alt_screen && self.modes.alternate_scroll {
            // Full-screen programs (less, vim) scroll with arrow keys.
            let arrow: &[u8] = match (lines > 0, self.modes.app_cursor) {
                (true, true) => b"\x1bOA",
                (true, false) => b"\x1b[A",
                (false, true) => b"\x1bOB",
                (false, false) => b"\x1b[B",
            };
            self.session
                .write(&arrow.repeat(lines.unsigned_abs() as usize));
        } else {
            self.session.scroll(TerminalScroll::Lines(lines));
        }
        cx.stop_propagation();
        cx.notify();
    }

    /// Opens the URL or the existing path at `at`.
    fn open_link(&mut self, at: ScreenPoint, cx: &mut Context<Self>) {
        let screen = self.session.screen();
        // Columns of the text, without the spacers of wide characters.
        let col = screen.row(at.row)[..usize::from(at.col).min(usize::from(screen.cols))]
            .iter()
            .filter(|c| !c.flags.contains(CellFlags::SPACER))
            .count();
        match links::link_at(&screen.row_text(at.row), col, &self.cwd) {
            Some(Link::Url(url)) => cx.open_url(&url),
            Some(Link::Path(path)) => cx.open_with_system(&path),
            None => {}
        }
    }
}

impl gpui_kit::Focusable for TerminalView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let screen = self.session.screen();
        self.modes = screen.modes;
        let colors = TerminalColors::of(cx);
        let font_size = text_size(cx);
        let base = font(mono_font(cx));
        let text_system = window.text_system();
        let font_id = text_system.resolve_font(&base);
        let cell_width = text_system
            .advance(font_id, font_size, 'm')
            .map_or(font_size * 0.6, |s| s.width);
        let cell = size(cell_width, (font_size * LINE_HEIGHT).round());
        let painter = Painter {
            screen,
            colors,
            font: base,
            font_size,
            cell,
            focused: self.focus.is_focused(window),
        };
        let (layout, last_size, session) =
            (self.layout.clone(), self.size.clone(), self.session.clone());
        div()
            .id("terminal")
            .key_context("Terminal")
            .track_focus(&self.focus)
            .size_full()
            .overflow_hidden()
            .px_1()
            .bg(painter.colors.bg)
            .cursor_text()
            .on_key_down(cx.listener(Self::on_key))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_scroll(cx.listener(Self::on_scroll))
            .child(
                canvas(
                    move |bounds: Bounds<Pixels>, window: &mut Window, _: &mut App| {
                        layout.set(Some(GridLayout {
                            origin: bounds.origin,
                            cell,
                        }));
                        let fit = |length: Pixels, unit: Pixels| {
                            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                            let n = (f32::from(length) / f32::from(unit)).floor().max(1.0) as u16;
                            n
                        };
                        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                        let new = TerminalSize {
                            cols: fit(bounds.size.width, cell.width).max(2),
                            rows: fit(bounds.size.height, cell.height),
                            cell_width: f32::from(cell.width).round() as u16,
                            cell_height: f32::from(cell.height).round() as u16,
                        };
                        if last_size.get() != new {
                            last_size.set(new);
                            session.resize(new);
                            // This frame drew the old size: draw the new one.
                            window.request_animation_frame();
                        }
                    },
                    move |bounds, (), window, cx| painter.paint(bounds.origin, window, cx),
                )
                .size_full(),
            )
    }
}

/// Paints a screen.
struct Painter {
    screen: TerminalScreen,
    colors: TerminalColors,
    font: Font,
    font_size: Pixels,
    cell: gpui_kit::Size<Pixels>,
    focused: bool,
}

/// How one cell is drawn.
#[derive(Clone, Copy, PartialEq)]
struct Look {
    fg: gpui_kit::Hsla,
    bg: gpui_kit::Hsla,
    /// Bold, italic, underline and strike-through flags.
    style: CellFlags,
}

impl Painter {
    fn look(&self, cell: &TerminalCell) -> Look {
        let f = cell.flags;
        let (mut fg, mut bg) = (self.colors.get(cell.fg), self.colors.get(cell.bg));
        if f.contains(CellFlags::INVERSE) {
            std::mem::swap(&mut fg, &mut bg);
        }
        if f.contains(CellFlags::DIM) {
            fg = fg.opacity(0.6);
        }
        if f.contains(CellFlags::HIDDEN) {
            fg = bg;
        }
        if f.contains(CellFlags::SELECTED) {
            bg = self.colors.selection;
        }
        Look {
            fg,
            bg,
            style: [
                CellFlags::BOLD,
                CellFlags::ITALIC,
                CellFlags::UNDERLINE,
                CellFlags::STRIKE,
            ]
            .into_iter()
            .filter(|s| f.contains(*s))
            .fold(CellFlags::NONE, CellFlags::with),
        }
    }

    fn x(&self, origin: Point<Pixels>, col: usize) -> Pixels {
        #[allow(clippy::cast_precision_loss)] // a screen's columns
        let col = col as f32;
        origin.x + self.cell.width * col
    }

    fn paint(&self, origin: Point<Pixels>, window: &mut Window, cx: &mut App) {
        for row in 0..self.screen.rows {
            let y = origin.y + self.cell.height * f32::from(row);
            let looks: Vec<Look> = self.screen.row(row).iter().map(|c| self.look(c)).collect();
            self.paint_backgrounds(&looks, point(origin.x, y), window);
            self.paint_text(row, &looks, point(origin.x, y), window, cx);
        }
        self.paint_cursor(origin, window, cx);
    }

    /// Runs of one background color other than the default.
    fn paint_backgrounds(&self, looks: &[Look], origin: Point<Pixels>, window: &mut Window) {
        let mut col = 0;
        while col < looks.len() {
            let bg = looks[col].bg;
            let run = looks[col..].iter().take_while(|l| l.bg == bg).count();
            if bg != self.colors.bg {
                #[allow(clippy::cast_precision_loss)] // a screen's columns
                let width = self.cell.width * run as f32;
                let bounds = Bounds::new(
                    point(self.x(origin, col), origin.y),
                    size(width, self.cell.height),
                );
                window.paint_quad(fill(bounds, bg));
            }
            col += run;
        }
    }

    /// Runs of one text style, each placed at its column so the grid stays
    /// aligned whatever the font's glyphs.
    fn paint_text(
        &self,
        row: u16,
        looks: &[Look],
        origin: Point<Pixels>,
        window: &mut Window,
        cx: &mut App,
    ) {
        let cells = self.screen.row(row);
        let mut col = 0;
        while col < cells.len() {
            let look = looks[col];
            let mut text = String::new();
            let mut end = col;
            while end < cells.len() && looks[end] == look {
                let c = &cells[end];
                end += 1;
                if c.flags.contains(CellFlags::SPACER) {
                    // The rest of the run would start one column early.
                    break;
                }
                text.push(c.ch);
                if c.flags.contains(CellFlags::WIDE) {
                    break;
                }
            }
            let blank = text.chars().all(|c| c == ' ');
            if !(blank
                && !look.style.contains(CellFlags::UNDERLINE)
                && !look.style.contains(CellFlags::STRIKE))
            {
                self.paint_run(
                    &text,
                    look,
                    point(self.x(origin, col), origin.y),
                    window,
                    cx,
                );
            }
            col = end;
        }
    }

    fn paint_run(
        &self,
        text: &str,
        look: Look,
        at: Point<Pixels>,
        window: &mut Window,
        cx: &mut App,
    ) {
        let run = TextRun {
            len: text.len(),
            font: Font {
                weight: if look.style.contains(CellFlags::BOLD) {
                    FontWeight::BOLD
                } else {
                    self.font.weight
                },
                style: if look.style.contains(CellFlags::ITALIC) {
                    FontStyle::Italic
                } else {
                    FontStyle::Normal
                },
                ..self.font.clone()
            },
            color: look.fg,
            background_color: None,
            underline: look
                .style
                .contains(CellFlags::UNDERLINE)
                .then_some(UnderlineStyle {
                    thickness: px(1.0),
                    color: Some(look.fg),
                    wavy: false,
                }),
            strikethrough: look
                .style
                .contains(CellFlags::STRIKE)
                .then_some(StrikethroughStyle {
                    thickness: px(1.0),
                    color: Some(look.fg),
                }),
        };
        let line = window.text_system().shape_line(
            SharedString::from(text.to_owned()),
            self.font_size,
            &[run],
            None,
        );
        // A glyph that cannot be drawn is not worth stopping the frame for.
        let _ = line.paint(at, self.cell.height, TextAlign::Left, None, window, cx);
    }

    fn paint_cursor(&self, origin: Point<Pixels>, window: &mut Window, cx: &mut App) {
        let Some(cursor) = self.screen.cursor else {
            return;
        };
        let at = point(
            self.x(origin, usize::from(cursor.at.col)),
            origin.y + self.cell.height * f32::from(cursor.at.row),
        );
        let color = self.colors.cursor;
        let full = Bounds::new(at, self.cell);
        match cursor.shape {
            CursorShape::Block if self.focused => {
                window.paint_quad(fill(full, color));
                let cell = self
                    .screen
                    .row(cursor.at.row)
                    .get(usize::from(cursor.at.col))
                    .copied()
                    .unwrap_or_default();
                if cell.ch != ' ' {
                    let look = Look {
                        fg: self.colors.bg,
                        bg: color,
                        ..self.look(&cell)
                    };
                    self.paint_run(&cell.ch.to_string(), look, at, window, cx);
                }
            }
            CursorShape::Block => {
                window.paint_quad(outline(full, color, gpui_kit::BorderStyle::Solid));
            }
            CursorShape::Beam => {
                window.paint_quad(fill(
                    Bounds::new(at, size(px(2.0), self.cell.height)),
                    color,
                ));
            }
            CursorShape::Underline => {
                let y = at.y + self.cell.height - px(2.0);
                window.paint_quad(fill(
                    Bounds::new(point(at.x, y), size(self.cell.width, px(2.0))),
                    color,
                ));
            }
        }
    }
}
