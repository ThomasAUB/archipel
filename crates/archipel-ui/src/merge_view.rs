//! The merge view, the default way to resolve conflicts: a conflicted
//! working-copy file opens here in place of its diff, by itself after an
//! operation stops on conflicts. **Ours** and **Theirs** show the file as
//! each side has it, its conflicting lines pickable with the mouse: a
//! click picks or drops a line, a drag gives the lines it crosses that
//! state, Shift+click extends from the last line clicked, a double-click
//! (or the block's box) takes or drops the whole block. Once the mouse is
//! released, the **Result** below, the file itself, is rebuilt with the
//! lines picked, the side picked first leading (⇅ swaps); picked lines
//! keep a bar in the gutter. Dropping every line brings the conflict
//! back. The Result is also edited freely; its blocks are followed
//! through the edits. The toolbar works on the current conflict (framed;
//! ↑↓, Alt+↑ / Alt+↓ or a click on it moves): take ours (Alt+←), theirs
//! (Alt+→), both in either order, the base when the markers hold it, or
//! none; or every conflict left at once. Ctrl+S saves the result without
//! resolving, **Mark resolved** (Ctrl+Enter) writes and stages it,
//! refused while markers are left, then opens the next conflicted file;
//! it is undoable (the conflict comes back). A file that cannot be shown
//! as text (binary, too large) or that a side deleted is resolved whole:
//! keep one side, or delete it. An external merge tool is picked with
//! "Set merge tool…" in the action palette, in place of this view.

use std::ops::Range;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use archipel_model::changes::{ConflictSides, FileState};
use archipel_model::ports::DiffSide;
use archipel_ops::conflict::{self, Conflict, LinePicks, Pick, Side};
use archipel_theme::ColorToken as T;
use gpui_kit::component::input::{
    Editor, EditorState, InputEvent, Position, RangeDecoration, RangeDecorationCollection,
    RangeDecorationStyle,
};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, App, ClickEvent, Context, Div, Entity, HighlightStyle, Hsla, KeyDownEvent,
    MouseButton, MouseDownEvent, MouseMoveEvent, ScrollStrategy, SharedString, Stateful,
    StyledText, Subscription, UniformListScrollHandle, Window, div, uniform_list,
};

use crate::file_edit::{editable_text, file_bytes, language_of};
use crate::staging::{Staging, small};
use crate::syntax::{self, LineHighlighter, Spans};
use crate::theme::{color, mono_font, row_height};
use crate::widgets::{button, primary_button, scroll_list};

/// The merge of one conflicted file, kept in the staging panel's state.
pub(crate) struct MergeEdit {
    /// Path relative to the repo root.
    file: String,
    /// `None` while the file and its versions load.
    loaded: Option<Loaded>,
    /// The bytes written by the last Ctrl+S save, set by the save job.
    saved: Arc<Mutex<Option<Vec<u8>>>>,
}

enum Loaded {
    /// Shown as text, in three editors.
    Text(Box<TextMerge>),
    /// Resolved whole: binary, too large, or not readable as text.
    Whole(WholeMerge),
    /// The versions could not be read (the reason).
    Failed(String),
}

/// One row of a side's pane.
struct PaneRow {
    /// The line, without its line break.
    text: String,
    /// 1-based line number in that side's file; 0 for a placeholder.
    lineno: usize,
    /// The conflict the row belongs to, and its line in that side of it
    /// (`None`: the placeholder of a side with no line there).
    at: Option<(usize, Option<usize>)>,
    /// The first row of its conflict: it carries the block's checkbox.
    first: bool,
}

/// A side, line by line, its conflicting lines pickable.
struct Pane {
    rows: Arc<Vec<PaneRow>>,
    /// The syntax colors of each row, once highlighted off the UI thread
    /// (the rows show plain until then: highlighting a long file takes a
    /// while).
    colors: Arc<OnceLock<Vec<Spans>>>,
    /// Per conflict, its first row.
    starts: Vec<usize>,
    scroll: UniformListScrollHandle,
}

/// A conflict of the opened file, followed in the result.
struct Chunk {
    /// Where it is in the result: its markers while unresolved, else what
    /// it was resolved with (and edited into).
    range: Range<usize>,
    picks: LinePicks,
    /// The block as opened, markers included: put back when nothing is
    /// picked any more.
    block: String,
    /// Its range in the result still holds conflict markers.
    open: bool,
}

/// A drag over the lines of one side of one conflict: the lines from
/// `from` to `to` get state `picked`, over the picks it started from.
struct Drag {
    side: Side,
    k: usize,
    from: usize,
    to: usize,
    picked: bool,
    before: LinePicks,
}

struct TextMerge {
    /// Its conflicts, as opened (LF line endings), conflict markers
    /// included.
    conflicts: Vec<Conflict>,
    /// The file as opened.
    original: String,
    /// The bytes on disk when opened (or last saved): resolving is
    /// refused when the file no longer holds them.
    on_disk: Vec<u8>,
    /// The file's lines end with CRLF: the editors work on LF.
    crlf: bool,
    /// The markers hold the base (`diff3` style).
    has_base: bool,
    labels: Labels,
    /// The side that deleted the file, if one did.
    deleted: Option<Side>,
    ours: Pane,
    theirs: Pane,
    result: Entity<EditorState>,
    result_marks: RangeDecorationCollection,
    /// The result's text as last read.
    text: String,
    chunks: Vec<Chunk>,
    /// Conflict blocks in the result (those of `chunks` and any typed in).
    left: usize,
    /// Index into `chunks` of the current conflict.
    current: usize,
    /// The last line clicked (side, conflict, line), for Shift+click.
    anchor: Option<(Side, usize, usize)>,
    /// The drag over lines under way.
    drag: Option<Drag>,
    _change: Subscription,
}

impl TextMerge {
    fn pane(&self, side: Side) -> &Pane {
        match side {
            Side::Ours => &self.ours,
            Side::Theirs => &self.theirs,
        }
    }

    /// The conflicts still open, in order.
    fn open(&self) -> impl Iterator<Item = usize> + '_ {
        self.chunks
            .iter()
            .enumerate()
            .filter(|(_, c)| c.open)
            .map(|(k, _)| k)
    }
}

struct WholeMerge {
    sides: ConflictSides,
    /// The bytes on disk when opened; `None` when there is no file.
    on_disk: Option<Vec<u8>>,
    labels: Labels,
    /// Why it is resolved whole.
    reason: String,
}

/// The names of the two sides, from the markers when they carry them.
#[derive(Clone)]
struct Labels {
    ours: String,
    theirs: String,
}

impl Labels {
    fn of(conflicts: &[Conflict]) -> Self {
        let pick = |label: Option<&String>, fallback: &str| {
            label
                .filter(|l| !l.is_empty())
                .map_or_else(|| fallback.to_owned(), |l| shorten(l))
        };
        Self {
            ours: pick(conflicts.first().map(|c| &c.ours_label), "current"),
            theirs: pick(conflicts.first().map(|c| &c.theirs_label), "incoming"),
        }
    }
}

/// A marker label made short: a commit id with its subject keeps the id
/// and the subject's start.
fn shorten(label: &str) -> String {
    const MAX: usize = 40;
    if label.chars().count() <= MAX {
        return label.to_owned();
    }
    let cut: String = label.chars().take(MAX).collect();
    format!("{cut}…")
}

/// Whether working-copy `file` is conflicted.
pub(crate) fn is_conflicted<V: Staging>(v: &V, file: &str) -> bool {
    v.changes()
        .iter()
        .any(|c| c.path == file && c.state == FileState::Conflicted)
}

/// Drops a merge of another file than `file` (or of none).
pub(crate) fn forget<V: Staging>(v: &mut V, file: Option<&str>) {
    let panel = v.panel();
    if panel
        .merge
        .as_ref()
        .is_some_and(|m| Some(m.file.as_str()) != file)
    {
        panel.merge = None;
    }
}

/// Starts loading `file`: its bytes on disk and its three versions.
fn load<V: Staging>(v: &mut V, file: String, window: &Window, cx: &mut Context<V>) {
    v.panel().merge = Some(MergeEdit {
        file: file.clone(),
        loaded: None,
        saved: Arc::default(),
    });
    let actions = v.core().repo_actions().clone();
    let repo = v.repo_path().clone();
    let task = {
        let file = file.clone();
        cx.background_spawn(async move {
            let sides = actions.conflict_sides(&repo, &file);
            let on_disk = actions.read_file(&repo, &file).ok();
            (sides, on_disk)
        })
    };
    cx.spawn_in(window, async move |this, cx| {
        let (sides, on_disk) = task.await;
        let _ = this.update_in(cx, |v: &mut V, window, cx| {
            if v.panel().merge.as_ref().map(|m| m.file.as_str()) != Some(file.as_str()) {
                return; // closed or another file opened meanwhile
            }
            let loaded = match sides {
                Err(e) => Loaded::Failed(e.to_string()),
                Ok(sides) => build(&file, sides, on_disk, window, cx),
            };
            if let Some(m) = &mut v.panel().merge {
                m.loaded = Some(loaded);
            }
            if let Some(Loaded::Text(_)) = loaded_mut(v) {
                refresh(v, cx);
                reveal(v, window, cx);
                highlight_panes(v, &file, cx);
            }
            cx.notify();
        });
    })
    .detach();
}

/// The merge state of a loaded file.
fn build<V: Staging>(
    file: &str,
    sides: ConflictSides,
    on_disk: Option<Vec<u8>>,
    window: &mut Window,
    cx: &mut Context<V>,
) -> Loaded {
    let deleted = match (&sides.ours, &sides.theirs) {
        (None, _) => Some(Side::Ours),
        (_, None) => Some(Side::Theirs),
        _ => None,
    };
    // Git's own test: a NUL byte early on makes a file binary.
    let binary = on_disk
        .as_ref()
        .is_some_and(|b| b.iter().take(8000).any(|&c| c == 0));
    if binary {
        return Loaded::Whole(WholeMerge {
            labels: Labels::of(&[]),
            sides,
            on_disk,
            reason: format!("{file} is binary"),
        });
    }
    let text = match on_disk.clone().map(|b| editable_text(file, b)) {
        Some(Ok(text)) => text,
        Some(Err(reason)) => {
            return Loaded::Whole(WholeMerge {
                labels: Labels::of(&[]),
                sides,
                on_disk,
                reason,
            });
        }
        None => {
            return Loaded::Whole(WholeMerge {
                labels: Labels::of(&[]),
                sides,
                on_disk,
                reason: format!("{file} is not in the working copy"),
            });
        }
    };
    let (bytes, original, crlf) = text;
    let conflicts = conflict::parse(&original);
    let language = language_of(file);
    let result = cx.new(|cx| {
        EditorState::new(window, cx)
            .language(language.clone())
            .line_number(true)
            .default_value(original.clone())
    });
    let result_marks = result.update(cx, |e, cx| {
        e.create_range_decorations_collection(Vec::new(), cx)
    });
    let change = cx.subscribe_in(
        &result,
        window,
        |v: &mut V, _, event: &InputEvent, _, cx| {
            if matches!(event, InputEvent::Change) {
                refresh(v, cx);
                cx.notify();
            }
        },
    );
    let chunks = conflicts
        .iter()
        .map(|c| Chunk {
            range: c.range.clone(),
            picks: LinePicks::new(c, &original),
            block: original[c.range.clone()].to_owned(),
            open: true,
        })
        .collect();
    Loaded::Text(Box::new(TextMerge {
        has_base: conflicts.iter().any(|c| c.base.is_some()),
        labels: Labels::of(&conflicts),
        ours: pane(&original, &conflicts, Side::Ours),
        theirs: pane(&original, &conflicts, Side::Theirs),
        text: original.clone(),
        left: conflicts.len(),
        chunks,
        conflicts,
        original,
        on_disk: bytes,
        crlf,
        deleted,
        result,
        result_marks,
        current: 0,
        anchor: None,
        drag: None,
        _change: change,
    }))
}

/// The syntax colors of `rows`, lines of `file`, in order.
fn highlight(file: &str, rows: &[PaneRow]) -> Vec<Spans> {
    let Some(syntax) = syntax::find(file) else {
        return Vec::new();
    };
    let mut h = LineHighlighter::new(syntax);
    rows.iter()
        .map(|row| {
            if row.lineno == 0 {
                Spans::new()
            } else {
                h.line(&row.text)
            }
        })
        .collect()
}

/// Highlights both panes off the UI thread, each side on its own; the
/// view is drawn again once a side is.
fn highlight_panes<V: Staging>(v: &mut V, file: &str, cx: &mut Context<V>) {
    let Some(t) = text_mut(v) else {
        return;
    };
    for pane in [&t.ours, &t.theirs] {
        let (rows, colors, file) = (pane.rows.clone(), pane.colors.clone(), file.to_owned());
        let task = cx.background_spawn(async move { highlight(&file, &rows) });
        cx.spawn(async move |this, cx| {
            let _ = colors.set(task.await);
            let _ = this.update(cx, |_, cx| cx.notify());
        })
        .detach();
    }
}

/// The rows of `side` of `original` (blocks `conflicts`): its lines, a
/// placeholder where a conflict has none on that side.
fn pane(original: &str, conflicts: &[Conflict], side: Side) -> Pane {
    let (text, ranges) = conflict::side_text(original, conflicts, side);
    let mut rows = Vec::new();
    let mut starts = vec![0; ranges.len()];
    let mut next = 0; // the next conflict to meet
    let mut in_chunk = 0; // the line's index in its conflict
    let placeholders = |rows: &mut Vec<PaneRow>, starts: &mut Vec<usize>, k: usize| {
        starts[k] = rows.len();
        rows.push(PaneRow {
            text: String::new(),
            lineno: 0,
            at: Some((k, None)),
            first: true,
        });
    };
    for (n, line) in conflict::line_ranges(&text, 0..text.len())
        .into_iter()
        .enumerate()
    {
        // Empty sides of the conflicts before this line.
        while let Some(r) = ranges.get(next)
            && r.is_empty()
            && r.start <= line.start
        {
            placeholders(&mut rows, &mut starts, next);
            next += 1;
        }
        let k = ranges
            .get(next)
            .filter(|r| r.contains(&line.start))
            .map(|_| next);
        let first = k.is_some_and(|k| ranges[k].start == line.start);
        if first && let Some(k) = k {
            starts[k] = rows.len();
            in_chunk = 0;
        }
        let at = k.map(|k| (k, Some(in_chunk)));
        in_chunk += 1;
        if k.is_some_and(|k| line.end >= ranges[k].end) {
            next += 1;
        }
        let content = text[line]
            .trim_end_matches('\n')
            .trim_end_matches('\r')
            .to_owned();
        rows.push(PaneRow {
            text: content,
            lineno: n + 1,
            at,
            first,
        });
    }
    while next < ranges.len() {
        placeholders(&mut rows, &mut starts, next);
        next += 1;
    }
    Pane {
        rows: Arc::new(rows),
        colors: Arc::default(),
        starts,
        scroll: UniformListScrollHandle::new(),
    }
}

fn loaded_mut<V: Staging>(v: &mut V) -> Option<&mut Loaded> {
    v.panel().merge.as_mut()?.loaded.as_mut()
}

fn text_mut<V: Staging>(v: &mut V) -> Option<&mut TextMerge> {
    match loaded_mut(v)? {
        Loaded::Text(t) => Some(t),
        _ => None,
    }
}

/// Reads the result again: follows the conflicts through the edit, which
/// are still open, and the highlights of the result.
fn refresh<V: Staging>(v: &mut V, cx: &mut Context<V>) {
    let Some(t) = text_mut(v) else {
        return;
    };
    let text = t.result.read(cx).value().to_string();
    let mut ranges: Vec<Range<usize>> = t.chunks.iter().map(|c| c.range.clone()).collect();
    conflict::track_edit(&t.text, &text, &mut ranges);
    t.text = text;
    for (c, r) in t.chunks.iter_mut().zip(ranges) {
        c.open = !conflict::parse(t.text.get(r.clone()).unwrap_or_default()).is_empty();
        c.range = r;
    }
    t.left = conflict::parse(&t.text).len();
    t.current = t.current.min(t.chunks.len().saturating_sub(1));
    let fill = color(cx, T::StatusConflict).opacity(0.18);
    let done = color(cx, T::StatusSuccess).opacity(0.14);
    let frame = color(cx, T::Accent);
    let marks: Vec<RangeDecoration> = t
        .chunks
        .iter()
        .enumerate()
        .filter(|(_, c)| !c.range.is_empty())
        .flat_map(|(k, c)| {
            let tint: Hsla = if c.open { fill } else { done };
            let mut marks = vec![
                RangeDecoration::new(c.range.clone())
                    .with_style(RangeDecorationStyle::Fill)
                    .with_color(tint),
            ];
            if k == t.current {
                marks.push(
                    RangeDecoration::new(c.range.clone())
                        .with_style(RangeDecorationStyle::Frame)
                        .with_color(frame),
                );
            }
            marks
        })
        .collect();
    t.result_marks.set(marks, cx);
}

/// Scrolls the panes and the result to the current conflict.
fn reveal<V: Staging>(v: &mut V, window: &mut Window, cx: &mut Context<V>) {
    let Some(t) = text_mut(v) else {
        return;
    };
    let Some(c) = t.chunks.get(t.current) else {
        return;
    };
    for pane in [&t.ours, &t.theirs] {
        if let Some(&row) = pane.starts.get(t.current) {
            pane.scroll
                .scroll_to_item(row.saturating_sub(3), ScrollStrategy::Top);
        }
    }
    let line = u32::try_from(conflict::line_of(&t.text, c.range.start)).unwrap_or(u32::MAX);
    let result = t.result.clone();
    result.update(cx, |e, cx| {
        e.set_cursor_position(Position::new(line, 0), window, cx);
    });
}

/// Moves to the next (`1`) or previous (`-1`) conflict still open (any
/// conflict when none is), wrapping.
fn step<V: Staging>(v: &mut V, delta: isize, window: &mut Window, cx: &mut Context<V>) {
    let Some(t) = text_mut(v) else {
        return;
    };
    let n = t.chunks.len();
    if n == 0 {
        return;
    }
    let any_open = t.open().next().is_some();
    let mut k = t.current;
    for _ in 0..n {
        k = (k + n).wrapping_add_signed(delta) % n;
        if !any_open || t.chunks[k].open {
            break;
        }
    }
    t.current = k;
    refresh(v, cx);
    reveal(v, window, cx);
    cx.notify();
}

/// Replaces conflict `k` in the result with `with` (undoably: Ctrl+Z in
/// the result), the conflicts after it moved along.
fn put<V: Staging>(v: &mut V, k: usize, with: String, window: &mut Window, cx: &mut Context<V>) {
    let Some(t) = text_mut(v) else {
        return;
    };
    let Some(range) = t.chunks.get(k).map(|c| c.range.clone()) else {
        return;
    };
    if t.text.get(range.clone()) == Some(with.as_str()) {
        return;
    }
    // Set ahead: the editor's change then reads as no edit to follow.
    t.text.replace_range(range.clone(), &with);
    let (old, new) = (range.len(), with.len());
    t.chunks[k].range = range.start..range.start + new;
    for c in &mut t.chunks[k + 1..] {
        c.range = c.range.start + new - old..c.range.end + new - old;
    }
    let result = t.result.clone();
    result.update(cx, |e, cx| {
        e.set_selected_range(range, cx);
        e.replace(with, window, cx);
    });
    refresh(v, cx);
}

/// Rebuilds conflict `k` in the result from its picks: the lines picked,
/// or its markers back when none is.
fn apply_picks<V: Staging>(v: &mut V, k: usize, window: &mut Window, cx: &mut Context<V>) {
    let Some(t) = text_mut(v) else {
        return;
    };
    let (Some(chunk), Some(c)) = (t.chunks.get(k), t.conflicts.get(k)) else {
        return;
    };
    let with = chunk
        .picks
        .compose(c, &t.original)
        .unwrap_or_else(|| chunk.block.clone());
    put(v, k, with, window, cx);
}

/// The mouse pressed on line `line` of `side` of conflict `k`: picks or
/// drops it and starts a drag giving that state to the lines crossed;
/// with Shift, gives it to the lines from the last one clicked (the
/// result follows on release). A double click takes or drops the whole
/// block instead, at once.
#[allow(clippy::many_single_char_names)] // v, k, t as everywhere here
fn press_line<V: Staging>(
    v: &mut V,
    side: Side,
    k: usize,
    line: usize,
    e: &MouseDownEvent,
    window: &mut Window,
    cx: &mut Context<V>,
) {
    let Some(t) = text_mut(v) else {
        return;
    };
    let Some(chunk) = t.chunks.get_mut(k) else {
        return;
    };
    t.current = k;
    let state = chunk.picks.of(side).get(line).copied().unwrap_or(false);
    if e.click_count >= 2 {
        // The first click flipped the line: back, then the whole block.
        chunk.picks.set(side, [line], !state);
        chunk.picks.toggle_side(side);
        t.drag = None;
        t.anchor = None;
    } else {
        let picked = !state;
        let from = match t.anchor {
            Some((s, a, from)) if e.modifiers.shift && s == side && a == k => from,
            _ => line,
        };
        t.drag = Some(Drag {
            side,
            k,
            from,
            to: line,
            picked,
            before: chunk.picks.clone(),
        });
        chunk
            .picks
            .set(side, from.min(line)..=from.max(line), picked);
        if !e.modifiers.shift {
            t.anchor = Some((side, k, line));
        }
        // The result follows on release.
        cx.notify();
        return;
    }
    apply_picks(v, k, window, cx);
    cx.notify();
}

/// The mouse moved over line `line` of `side` of conflict `k`: a drag on
/// that side of that conflict now ends there.
fn drag_over<V: Staging>(
    v: &mut V,
    side: Side,
    k: usize,
    line: usize,
    event: &MouseMoveEvent,
    window: &mut Window,
    cx: &mut Context<V>,
) {
    let Some(t) = text_mut(v) else {
        return;
    };
    if event.pressed_button != Some(MouseButton::Left) {
        finish_drag(v, window, cx); // released out of the view
        return;
    }
    let Some(d) = t.drag.as_mut().filter(|d| d.side == side && d.k == k) else {
        return;
    };
    if d.to == line {
        return;
    }
    d.to = line;
    let mut picks = d.before.clone();
    picks.set(side, d.from.min(line)..=d.from.max(line), d.picked);
    if let Some(chunk) = t.chunks.get_mut(k) {
        chunk.picks = picks;
    }
    t.anchor = Some((side, k, line));
    cx.notify();
}

/// The mouse released: the drag is over, the result rebuilt from the
/// picks it made (one edit, undone at once).
fn finish_drag<V: Staging>(v: &mut V, window: &mut Window, cx: &mut Context<V>) {
    let Some(d) = text_mut(v).and_then(|t| t.drag.take()) else {
        return;
    };
    apply_picks(v, d.k, window, cx);
    cx.notify();
}

/// A click on the block checkbox of `side` of conflict `k`: ticks all its
/// lines, or unticks them when all are; a side with no line there is
/// taken whole (the conflict's lines removed).
fn click_block<V: Staging>(
    v: &mut V,
    side: Side,
    k: usize,
    window: &mut Window,
    cx: &mut Context<V>,
) {
    let Some(t) = text_mut(v) else {
        return;
    };
    let Some(chunk) = t.chunks.get_mut(k) else {
        return;
    };
    t.current = k;
    t.anchor = None;
    if chunk.picks.of(side).is_empty() {
        chunk.picks.pick(Pick::Base);
        put(v, k, String::new(), window, cx);
    } else {
        chunk.picks.toggle_side(side);
        apply_picks(v, k, window, cx);
    }
    cx.notify();
}

/// Resolves conflict `k` with `pick` (`None`: with nothing).
fn resolve_with<V: Staging>(
    v: &mut V,
    k: usize,
    pick: Option<Pick>,
    window: &mut Window,
    cx: &mut Context<V>,
) {
    let Some(t) = text_mut(v) else {
        return;
    };
    let (Some(chunk), Some(c)) = (t.chunks.get_mut(k), t.conflicts.get(k)) else {
        return;
    };
    let with = match pick {
        Some(Pick::Base) => {
            chunk.picks.pick(Pick::Base);
            c.picked(&t.original, Pick::Base)
        }
        Some(pick) => {
            chunk.picks.pick(pick);
            chunk.picks.compose(c, &t.original).unwrap_or_default()
        }
        None => {
            chunk.picks.pick(Pick::Base);
            String::new()
        }
    };
    put(v, k, with, window, cx);
}

/// Resolves the current conflict with `pick` (`None`: with nothing), then
/// moves to the next one open.
fn take<V: Staging>(v: &mut V, pick: Option<Pick>, window: &mut Window, cx: &mut Context<V>) {
    let Some(k) = text_mut(v).map(|t| t.current) else {
        return;
    };
    resolve_with(v, k, pick, window, cx);
    if text_mut(v).is_some_and(|t| t.open().next().is_some()) {
        step(v, 1, window, cx);
    } else {
        reveal(v, window, cx);
        cx.notify();
    }
}

/// Resolves every conflict still open with `pick`.
fn take_all<V: Staging>(v: &mut V, pick: Pick, window: &mut Window, cx: &mut Context<V>) {
    let open: Vec<usize> = text_mut(v).map(|t| t.open().collect()).unwrap_or_default();
    // From the last one: the ranges before stay put.
    for k in open.into_iter().rev() {
        resolve_with(v, k, Some(pick), window, cx);
    }
    reveal(v, window, cx);
    cx.notify();
}

/// Puts the other side's picked lines first in the current conflict.
fn swap_order<V: Staging>(v: &mut V, window: &mut Window, cx: &mut Context<V>) {
    let Some(t) = text_mut(v) else {
        return;
    };
    let k = t.current;
    let Some(chunk) = t.chunks.get_mut(k) else {
        return;
    };
    chunk.picks.theirs_first = !chunk.picks.theirs_first;
    apply_picks(v, k, window, cx);
    cx.notify();
}

/// The result's bytes and the bytes the file must still hold.
fn result_bytes<V: Staging>(v: &mut V, cx: &App) -> Option<(String, Vec<u8>, Vec<u8>, usize)> {
    let file = v.panel().merge.as_ref()?.file.clone();
    let Some(Loaded::Text(t)) = v.panel().merge.as_ref()?.loaded.as_ref() else {
        return None;
    };
    let text = t.result.read(cx).value().to_string();
    let left = conflict::parse(&text).len();
    Some((file, file_bytes(&text, t.crlf), t.on_disk.clone(), left))
}

/// Ctrl+S: writes the result over the file, without resolving it.
fn save<V: Staging>(v: &mut V, cx: &mut Context<V>) {
    let Some((file, contents, expected, _)) = result_bytes(v, cx) else {
        return;
    };
    let Some(saved) = v.panel().merge.as_ref().map(|m| m.saved.clone()) else {
        return;
    };
    v.run_job(
        format!("Save {file}"),
        cx,
        Box::new(move |a, repo, _| {
            a.save_file(repo, &file, &expected, &contents)?;
            *saved.lock().unwrap_or_else(PoisonError::into_inner) = Some(contents);
            Ok(())
        }),
    );
}

/// Mark resolved: writes the result and stages it, refused while
/// conflict markers are left.
fn resolve<V: Staging>(v: &mut V, window: &mut Window, cx: &mut Context<V>) {
    let Some((file, contents, expected, left)) = result_bytes(v, cx) else {
        return;
    };
    if left > 0 {
        let s = if left == 1 { "" } else { "s" };
        v.report(
            format!("{left} conflict{s} left in {file}: take a side or edit the markers out first"),
            cx,
        );
        return;
    }
    let next = next_conflicted(v, &file);
    let label = format!("Resolve {file}");
    let job_file = file.clone();
    v.run_job(
        label,
        cx,
        Box::new(move |a, repo, _| {
            a.resolve_conflict(repo, &job_file, Some(&expected), Some(&contents))
        }),
    );
    if let Some(next) = next {
        v.click_file(next, DiffSide::Unstaged, window, cx);
    } else {
        let then = match v.panel().summary().and_then(|s| s.in_progress) {
            Some(_) => ": every conflict is resolved, Continue when ready",
            None => ": every conflict is resolved, commit when ready",
        };
        v.report(format!("Resolve {file}{then}"), cx);
    }
}

/// The conflicted file after `file` in the changes, wrapping; `None` when
/// it is the only one.
fn next_conflicted<V: Staging>(v: &V, file: &str) -> Option<String> {
    let conflicted: Vec<&str> = v
        .changes()
        .iter()
        .filter(|c| c.state == FileState::Conflicted)
        .map(|c| c.path.as_str())
        .collect();
    let at = conflicted.iter().position(|&f| f == file);
    let after = at.map_or(0, |i| i + 1);
    conflicted
        .iter()
        .cycle()
        .skip(after)
        .take(conflicted.len())
        .find(|&&f| f != file)
        .map(|&f| f.to_owned())
}

/// Resolves the file whole with one side's version (deleting it when that
/// side has none), or deletes it when `side` is `None`.
fn resolve_whole<V: Staging>(v: &mut V, side: Option<Side>, cx: &mut Context<V>) {
    let Some(m) = v.panel().merge.as_ref() else {
        return;
    };
    let file = m.file.clone();
    let expected = match &m.loaded {
        Some(Loaded::Text(t)) => Some(t.on_disk.clone()),
        Some(Loaded::Whole(w)) => w.on_disk.clone(),
        _ => None,
    };
    let label = match side {
        Some(Side::Ours) => format!("Resolve {file} with ours"),
        Some(Side::Theirs) => format!("Resolve {file} with theirs"),
        None => format!("Delete {file}"),
    };
    v.run_job(
        label,
        cx,
        Box::new(move |a, repo, _| {
            let contents = match side {
                None => None,
                Some(side) => {
                    let sides = a.conflict_sides(repo, &file)?;
                    match side {
                        Side::Ours => sides.ours,
                        Side::Theirs => sides.theirs,
                    }
                }
            };
            a.resolve_conflict(repo, &file, expected.as_deref(), contents.as_deref())
        }),
    );
}

/// Takes in a save that went through: the file now holds its bytes.
fn settle_save<V: Staging>(v: &mut V) {
    let Some(m) = v.panel().merge.as_mut() else {
        return;
    };
    let Some(saved) = m
        .saved
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .take()
    else {
        return;
    };
    if let Some(Loaded::Text(t)) = &mut m.loaded {
        t.on_disk = saved;
    }
}

/// The merge view of conflicted `file`, in place of its diff.
pub(crate) fn render<V: Staging>(
    v: &mut V,
    file: &str,
    window: &Window,
    cx: &mut Context<V>,
) -> AnyElement {
    settle_save(v);
    if v.panel().merge.as_ref().map(|m| m.file.as_str()) != Some(file) {
        load(v, file.to_owned(), window, cx);
    }
    let header = render_header(v, file, cx);
    let body = match v.panel().merge.as_ref().and_then(|m| m.loaded.as_ref()) {
        None => note("Loading the conflict…", cx).into_any_element(),
        Some(Loaded::Failed(reason)) => note(reason.clone(), cx).into_any_element(),
        Some(Loaded::Whole(_)) => render_whole(v, cx),
        Some(Loaded::Text(_)) => render_text(v, cx),
    };
    div()
        .id(SharedString::from(format!("merge-{file}")))
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .overflow_hidden()
        .on_mouse_up(
            MouseButton::Left,
            cx.listener(|v: &mut V, _, window, cx| finish_drag(v, window, cx)),
        )
        .on_mouse_up_out(
            MouseButton::Left,
            cx.listener(|v: &mut V, _, window, cx| finish_drag(v, window, cx)),
        )
        .on_key_down(cx.listener(|v: &mut V, e: &KeyDownEvent, window, cx| {
            let m = &e.keystroke.modifiers;
            let command = m.control || m.platform;
            match e.keystroke.key.as_str() {
                "s" if command => save(v, cx),
                "enter" if command => resolve(v, window, cx),
                "down" if m.alt => step(v, 1, window, cx),
                "up" if m.alt => step(v, -1, window, cx),
                "left" if m.alt => take(v, Some(Pick::Ours), window, cx),
                "right" if m.alt => take(v, Some(Pick::Theirs), window, cx),
                _ => return,
            }
            cx.stop_propagation();
        }))
        .child(header)
        .child(body)
        .into_any_element()
}

fn note(text: impl Into<SharedString>, cx: &App) -> Div {
    div()
        .p_3()
        .text_color(color(cx, T::FgMuted))
        .child(text.into())
}

/// Title, a "conflicted" pill and Mark resolved.
fn render_header<V: Staging>(v: &mut V, file: &str, cx: &mut Context<V>) -> Div {
    let conflict = color(cx, T::StatusConflict);
    let text = matches!(
        v.panel().merge.as_ref().and_then(|m| m.loaded.as_ref()),
        Some(Loaded::Text(_))
    );
    let files = v
        .changes()
        .iter()
        .filter(|c| c.state == FileState::Conflicted)
        .count();
    let keys = format!(
        "{} saves · {} marks resolved",
        crate::keys::fixed_label("ctrl-s"),
        crate::keys::fixed_label("ctrl-enter")
    );
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap_2()
        .px_2()
        .py_1()
        .border_b_1()
        .border_color(color(cx, T::BorderSubtle))
        .child(
            div()
                .flex_none()
                .px_1()
                .text_xs()
                .text_color(conflict)
                .border_1()
                .border_color(conflict)
                .rounded_sm()
                .child("conflicted"),
        )
        .child(div().flex_1().min_w_0().truncate().child(file.to_owned()))
        .when(files > 1, |d| {
            d.child(
                div()
                    .flex_none()
                    .text_xs()
                    .text_color(color(cx, T::FgMuted))
                    .child(format!("{files} conflicted files")),
            )
        })
        .when(text, |d| {
            d.child(
                div()
                    .flex_none()
                    .text_xs()
                    .text_color(color(cx, T::FgMuted))
                    .child(keys),
            )
        })
        .when(text, |d| {
            d.child(
                small(primary_button("merge-resolve", "Mark resolved", cx))
                    .on_click(cx.listener(|v: &mut V, _, w, cx| resolve(v, w, cx))),
            )
        })
}

/// A toolbar button running `f` on the view.
fn tool<V: Staging>(
    id: &'static str,
    label: impl Into<SharedString>,
    cx: &mut Context<V>,
    f: impl Fn(&mut V, &mut Window, &mut Context<V>) + 'static,
) -> Stateful<Div> {
    small(button(id, label, cx))
        .on_click(cx.listener(move |v: &mut V, _, window, cx| f(v, window, cx)))
}

/// The conflict toolbar: where the current conflict is, moving through
/// them, and what to resolve it with.
#[allow(clippy::too_many_lines)] // declarative element tree
fn render_toolbar<V: Staging>(v: &mut V, cx: &mut Context<V>) -> Div {
    let Some(t) = text_mut(v) else {
        return div();
    };
    let total = t.chunks.len();
    let open = t.open().count();
    let (current, has_base, deleted) = (t.current, t.has_base, t.deleted);
    let both = t
        .chunks
        .get(current)
        .is_some_and(|c| c.picks.ours.contains(&true) && c.picks.theirs.contains(&true));
    let current_open = t.chunks.get(current).is_some_and(|c| c.open);
    let labels = t.labels.clone();
    let left = t.left;
    let muted = color(cx, T::FgMuted);
    let position = if total == 0 {
        String::new()
    } else {
        let state = if current_open { "" } else { " (resolved)" };
        format!("Conflict {} of {total}{state}", current + 1)
    };
    let summary = match (open, left) {
        (0, 0) => "All resolved: check the result, then Mark resolved".to_owned(),
        (0, n) => format!("{n} conflict block(s) typed in the result"),
        (1, _) => "1 left".to_owned(),
        (n, _) => format!("{n} left"),
    };
    let summary_color = color(
        cx,
        if open == 0 && left == 0 {
            T::StatusSuccess
        } else {
            T::FgMuted
        },
    );
    let alt = |key: &str| crate::keys::fixed_label(&format!("alt-{key}"));
    div()
        .flex()
        .flex_none()
        .flex_wrap()
        .items_center()
        .gap_1()
        .px_2()
        .py_1()
        .border_b_1()
        .border_color(color(cx, T::BorderSubtle))
        .text_xs()
        .when(total > 0, |d| {
            d.child(tool("merge-prev", "↑", cx, |v, w, cx| step(v, -1, w, cx)))
                .child(tool("merge-next", "↓", cx, |v, w, cx| step(v, 1, w, cx)))
                .child(div().child(position))
                .child(
                    div()
                        .text_color(summary_color)
                        .child(format!("· {summary}")),
                )
                .child(div().w_2())
                .child(tool(
                    "merge-ours",
                    format!("◧ Ours ({})", alt("left")),
                    cx,
                    |v, w, cx| take(v, Some(Pick::Ours), w, cx),
                ))
                .child(tool(
                    "merge-theirs",
                    format!("Theirs ◨ ({})", alt("right")),
                    cx,
                    |v, w, cx| take(v, Some(Pick::Theirs), w, cx),
                ))
                .child(tool("merge-both", "Both", cx, |v, w, cx| {
                    take(v, Some(Pick::OursThenTheirs), w, cx);
                }))
                .when(both, |d| {
                    d.child(tool("merge-swap", "⇅ Swap order", cx, swap_order))
                })
                .when(has_base, |d| {
                    d.child(tool("merge-base", "Base", cx, |v, w, cx| {
                        take(v, Some(Pick::Base), w, cx);
                    }))
                })
                .child(tool("merge-none", "None", cx, |v, w, cx| {
                    take(v, None, w, cx);
                }))
                .when(open > 1, |d| {
                    d.child(div().w_2())
                        .child(div().text_color(muted).child("All left:"))
                        .child(tool("merge-all-ours", "Ours", cx, |v, w, cx| {
                            take_all(v, Pick::Ours, w, cx);
                        }))
                        .child(tool("merge-all-theirs", "Theirs", cx, |v, w, cx| {
                            take_all(v, Pick::Theirs, w, cx);
                        }))
                })
        })
        .when(total == 0 && left == 0, |d| {
            d.child(
                div()
                    .text_color(summary_color)
                    .child("No conflict markers left: check the result, then Mark resolved"),
            )
        })
        .when_some(deleted, |d, side| {
            let who = match side {
                Side::Ours => &labels.ours,
                Side::Theirs => &labels.theirs,
            };
            d.child(div().w_2())
                .child(
                    div()
                        .text_color(color(cx, T::StatusWarning))
                        .child(format!("Deleted on {who}")),
                )
                .child(
                    small(button("merge-delete", "Delete file", cx))
                        .on_click(cx.listener(|v: &mut V, _, _, cx| resolve_whole(v, None, cx))),
                )
        })
}

/// One row of a side's pane: the block's checkbox on its first row, the
/// line's checkbox, its number and text.
#[allow(clippy::too_many_lines)] // declarative element tree
fn render_pane_row<V: Staging>(
    side: Side,
    index: usize,
    row: &PaneRow,
    t: &TextMerge,
    cx: &mut Context<V>,
) -> AnyElement {
    let gutter = color(cx, T::BgPanel);
    let base = div()
        .id(SharedString::from(format!("merge-{side:?}-{index}")))
        .w_full()
        .h(row_height(cx))
        .flex()
        .items_center()
        .whitespace_nowrap()
        .overflow_hidden()
        .font_family(mono_font(cx))
        .text_xs();
    let number = if row.lineno == 0 {
        String::new()
    } else {
        row.lineno.to_string()
    };
    let spans = t
        .pane(side)
        .colors
        .get()
        .and_then(|c| c.get(index))
        .map_or(&[][..], Vec::as_slice);
    let text = |row: &PaneRow, cx: &App| {
        StyledText::new(row.text.clone()).with_highlights(spans.iter().map(|(r, token)| {
            (
                r.clone(),
                HighlightStyle {
                    color: Some(color(cx, *token)),
                    ..HighlightStyle::default()
                },
            )
        }))
    };
    let Some((k, line)) = row.at else {
        // Outside the conflicts: as is.
        return base
            .child(div().flex_none().w_10().h_full().bg(gutter))
            .child(
                div()
                    .flex_none()
                    .w_16()
                    .h_full()
                    .pr_2()
                    .flex()
                    .items_center()
                    .justify_end()
                    .bg(gutter)
                    .text_color(color(cx, T::FgSubtle))
                    .child(number),
            )
            .child(div().pl_2().child(text(row, cx)))
            .into_any_element();
    };
    let Some(chunk) = t.chunks.get(k) else {
        return base.into_any_element();
    };
    let picks = chunk.picks.of(side);
    let picked = line.and_then(|l| picks.get(l).copied()).unwrap_or(false);
    let current = k == t.current;
    let pick_bar = color(cx, T::Accent);
    let tint = if picked {
        color(cx, T::StatusSuccess).opacity(0.22)
    } else if chunk.open {
        color(cx, T::StatusConflict).opacity(0.14)
    } else {
        color(cx, T::FgMuted).opacity(0.06)
    };
    let edge = if current {
        color(cx, T::Accent)
    } else {
        tint.opacity(0.0)
    };
    // The block's box: all, some or none of this side's lines picked.
    let block = row.first.then(|| {
        let (all, some) = (
            !picks.is_empty() && picks.iter().all(|&p| p),
            picks.contains(&true),
        );
        let glyph = match (all, some) {
            (true, _) => "☑",
            (false, true) => "◪",
            _ => "☐",
        };
        div()
            .id(SharedString::from(format!("merge-block-{side:?}-{k}")))
            .cursor_pointer()
            .px_1()
            .rounded_sm()
            .hover(|s| s.bg(color(cx, T::BgHover)))
            .text_color(color(cx, if some { T::AccentText } else { T::FgMuted }))
            .child(glyph)
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(move |v: &mut V, _: &ClickEvent, window, cx| {
                cx.stop_propagation();
                click_block(v, side, k, window, cx);
            }))
    });
    let content = match line {
        Some(_) => div().pl_2().child(text(row, cx)).into_any_element(),
        None => div()
            .pl_2()
            .italic()
            .text_color(color(cx, T::FgSubtle))
            .child("no line on this side: click to remove the other side's")
            .into_any_element(),
    };
    base.bg(tint)
        .cursor_pointer()
        .hover(|s| s.bg(color(cx, T::BgHover)))
        .child(
            div()
                .flex_none()
                .w_10()
                .h_full()
                .flex()
                .items_center()
                .justify_center()
                .bg(gutter)
                .border_l_2()
                .border_color(edge)
                .children(block),
        )
        .child(
            div()
                .flex_none()
                .w_16()
                .h_full()
                .pr_2()
                .flex()
                .items_center()
                .justify_end()
                .bg(gutter)
                .border_r_4()
                .border_color(if picked { pick_bar } else { gutter })
                .text_color(color(cx, if picked { T::AccentText } else { T::FgSubtle }))
                .child(number),
        )
        .child(content)
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(
                move |v: &mut V, e: &MouseDownEvent, window, cx| match line {
                    Some(l) => press_line(v, side, k, l, e, window, cx),
                    None => click_block(v, side, k, window, cx),
                },
            ),
        )
        .on_mouse_move(
            cx.listener(move |v: &mut V, e: &MouseMoveEvent, window, cx| {
                if let Some(l) = line {
                    drag_over(v, side, k, l, e, window, cx);
                }
            }),
        )
        .into_any_element()
}

/// A side's pane, virtualized.
fn render_pane<V: Staging>(side: Side, t: &TextMerge, cx: &mut Context<V>) -> AnyElement {
    let pane = t.pane(side);
    let rows = pane.rows.clone();
    let list = uniform_list(
        SharedString::from(format!("merge-rows-{side:?}")),
        rows.len(),
        cx.processor(move |v: &mut V, range: Range<usize>, _window, cx| {
            let Some(t) = text_mut(v) else {
                return Vec::new();
            };
            range
                .filter_map(|i| rows.get(i).map(|row| render_pane_row(side, i, row, t, cx)))
                .collect()
        }),
    );
    scroll_list(SharedString::from(format!("merge-scroll-{side:?}")), list)
        .handle(&pane.scroll)
        .into_any_element()
}

/// The panes of both sides over the result, under the conflict toolbar.
fn render_text<V: Staging>(v: &mut V, cx: &mut Context<V>) -> AnyElement {
    let toolbar = render_toolbar(v, cx);
    let Some(t) = text_mut(v) else {
        return div().into_any_element();
    };
    let labels = t.labels.clone();
    let result = t.result.clone();
    let (ours, theirs) = (
        render_pane(Side::Ours, t, cx),
        render_pane(Side::Theirs, t, cx),
    );
    let title = |title: String, hint: &'static str, cx: &App| {
        div()
            .flex_none()
            .flex()
            .gap_2()
            .px_2()
            .text_xs()
            .bg(color(cx, T::BgPanel))
            .border_b_1()
            .border_color(color(cx, T::BorderSubtle))
            .child(
                div()
                    .flex_none()
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .child(title),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(color(cx, T::FgSubtle))
                    .child(hint),
            )
    };
    let column = |id: &'static str, head: Div, body: AnyElement| {
        div()
            .id(id)
            .flex_1()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .child(head)
            .child(div().flex_1().min_h_0().flex().flex_col().child(body))
    };
    let hint = "click or drag over lines · Shift+click extends · double-click: whole block";
    let sides = div()
        .flex_1()
        .min_h_0()
        .flex()
        .child(column(
            "merge-pane-ours",
            title(format!("Ours · {}", labels.ours), hint, cx),
            ours,
        ))
        .child(div().w_px().h_full().bg(color(cx, T::BorderSubtle)))
        .child(column(
            "merge-pane-theirs",
            title(format!("Theirs · {}", labels.theirs), hint, cx),
            theirs,
        ));
    let result = column(
        "merge-pane-result",
        title(
            "Result".to_owned(),
            "the file as it will be saved · edit it freely",
            cx,
        ),
        Editor::new(&result)
            .bordered(false)
            .font_family(mono_font(cx))
            .size_full()
            .into_any_element(),
    )
    .border_t_1()
    .border_color(color(cx, T::BorderSubtle));
    div()
        .flex_1()
        .min_h_0()
        .flex()
        .flex_col()
        .child(toolbar)
        .child(sides)
        .child(result)
        .into_any_element()
}

/// A file resolved whole: keep one side's version, or delete it.
fn render_whole<V: Staging>(v: &mut V, cx: &mut Context<V>) -> AnyElement {
    let Some(Some(Loaded::Whole(w))) = v.panel().merge.as_ref().map(|m| m.loaded.as_ref()) else {
        return div().into_any_element();
    };
    let (labels, reason) = (w.labels.clone(), w.reason.clone());
    let describe = |bytes: &Option<Vec<u8>>| match bytes {
        Some(b) => format!("{} bytes", b.len()),
        None => "deleted".to_owned(),
    };
    let (ours, theirs) = (describe(&w.sides.ours), describe(&w.sides.theirs));
    let keep = |id: &'static str, label: String, side: Side, cx: &mut Context<V>| {
        button(id, label, cx).on_click(cx.listener(move |v: &mut V, _, _, cx| {
            resolve_whole(v, Some(side), cx);
        }))
    };
    div()
        .p_3()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .text_color(color(cx, T::FgMuted))
                .child(format!("{reason}: resolve it whole.")),
        )
        .child(
            div()
                .flex()
                .gap_2()
                .child(keep(
                    "merge-keep-ours",
                    format!("Keep ours · {} ({ours})", labels.ours),
                    Side::Ours,
                    cx,
                ))
                .child(keep(
                    "merge-keep-theirs",
                    format!("Keep theirs · {} ({theirs})", labels.theirs),
                    Side::Theirs,
                    cx,
                ))
                .child(
                    button("merge-delete-whole", "Delete file", cx)
                        .on_click(cx.listener(|v: &mut V, _, _, cx| resolve_whole(v, None, cx))),
                ),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use archipel_ops::conflict::{Side, parse};

    use super::{highlight, pane};

    #[test]
    fn pane_rows_place_each_line_in_its_conflict() {
        let text =
            "a\n<<<<<<< o\no1\no2\n=======\n>>>>>>> t\nb\n<<<<<<< o\nx\n=======\ny\n>>>>>>> t\n";
        let c = parse(text);
        let ours = pane(text, &c, Side::Ours);
        let at: Vec<_> = ours
            .rows
            .iter()
            .map(|r| (r.text.as_str(), r.lineno, r.at, r.first))
            .collect();
        assert_eq!(
            at,
            [
                ("a", 1, None, false),
                ("o1", 2, Some((0, Some(0))), true),
                ("o2", 3, Some((0, Some(1))), false),
                ("b", 4, None, false),
                ("x", 5, Some((1, Some(0))), true),
            ]
        );
        assert_eq!(ours.starts, [1, 4]);
        // The first conflict has no line on their side: a placeholder.
        let theirs = pane(text, &c, Side::Theirs);
        let at: Vec<_> = theirs
            .rows
            .iter()
            .map(|r| (r.text.as_str(), r.lineno, r.at))
            .collect();
        assert_eq!(
            at,
            [
                ("a", 1, None),
                ("", 0, Some((0, None))),
                ("b", 2, None),
                ("y", 3, Some((1, Some(0)))),
            ]
        );
        assert_eq!(theirs.starts, [1, 3]);
        // Highlighting gives a row its colors, a placeholder none.
        let colors = highlight("x.rs", &theirs.rows);
        assert_eq!(colors.len(), theirs.rows.len());
        assert!(colors[1].is_empty());
        assert!(highlight("x.unknown-ext", &theirs.rows).is_empty());
    }
}
