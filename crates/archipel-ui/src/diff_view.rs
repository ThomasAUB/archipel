//! Diffs, shared by the repo tab and the dashboard's quick edit pane: a
//! diff flattened into fixed-height rows and rendered virtualized. On a
//! working-copy diff, hunks have stage / unstage / discard buttons, and
//! changed lines can be selected (click, Ctrl+click, Shift+click) to stage,
//! unstage or discard just them, and the file can be edited in place
//! (Edit, or a double-click on a line; see [`crate::file_edit`]). An
//! unstaged diff names the file's line ending, with a button converting
//! it to the other (LF ↔ CRLF). Lines are syntax highlighted after the
//! file's extension (see [`crate::syntax`]).

use std::collections::{BTreeSet, HashMap};
use std::ops::Range;
use std::sync::{Arc, Mutex, PoisonError};

use archipel_model::changes::{ChangeKind, FileChange};
use archipel_model::diff::{DiffLine, FileDiff, LineOrigin};
use archipel_model::ports::DiffSide;
use archipel_model::requests::PatchDirection;
use archipel_ops::patch::selection_patch;
use archipel_theme::ColorToken as T;
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, App, ClickEvent, Context, Div, HighlightStyle, SharedString, StyledText,
    UniformListScrollHandle, div, px, uniform_list,
};

use crate::file_edit::{self, LineEnding};
use crate::staging::{Staging, arm_discard};
use crate::syntax::{self, LineHighlighter, Spans};
use crate::theme::{color, mono_font, row_height};
use crate::widgets::{button, scroll_list};

/// One fixed-height row of a flattened diff.
#[derive(Clone, Copy)]
pub(crate) enum DiffRow {
    /// File header: file index.
    File(usize),
    /// A note in place of the hunks (binary, too large).
    Note(&'static str),
    /// Hunk header: file, hunk.
    Hunk(usize, usize),
    /// Diff line: file, hunk, line.
    Line(usize, usize, usize),
}

/// A diff flattened into rows once, so the view renders only the visible
/// ones (a diff may hold tens of thousands of lines).
pub(crate) struct DiffDoc {
    pub(crate) files: Vec<FileDiff>,
    rows: Vec<DiffRow>,
    /// Digits of the largest line number, so the gutter has one width.
    lineno_width: usize,
    /// Per file, the line ending of its new side (from its added and
    /// context lines), with whether the change is only line endings.
    endings: Vec<Option<Endings>>,
    /// Selected changed lines. A reloaded diff is a new doc, so the
    /// selection never outlives the lines it names.
    selection: Mutex<LineSelection>,
    /// Syntax colors, per `(file, hunk)`, computed as the hunk's lines are
    /// first shown; `None` for a file shown as plain text.
    colors: Mutex<HashMap<(usize, usize), Option<HunkColors>>>,
}

/// The syntax colors of one hunk, as far as its lines were highlighted.
struct HunkColors {
    /// The old side: context and removed lines.
    old: LineHighlighter,
    /// The new side: context and added lines.
    new: LineHighlighter,
    /// Spans of the hunk's first lines.
    lines: Vec<Spans>,
}

/// The line endings a file's diff shows.
#[derive(Clone, Copy)]
struct Endings {
    /// The ending most of the new side's lines have.
    new: LineEnding,
    /// Every removed line comes back added with only its ending changed.
    only: bool,
}

impl Endings {
    fn of(file: &FileDiff) -> Option<Self> {
        let (mut crlf, mut lf) = (0usize, 0usize);
        let (mut removed, mut added) = (Vec::new(), Vec::new());
        for line in file.hunks.iter().flat_map(|h| &h.lines) {
            if line.origin != LineOrigin::Removed {
                if line.content.ends_with('\r') {
                    crlf += 1;
                } else {
                    lf += 1;
                }
            }
            let text = line.content.trim_end_matches('\r');
            match line.origin {
                LineOrigin::Removed => removed.push((text, line.content.len())),
                LineOrigin::Added => added.push((text, line.content.len())),
                LineOrigin::Context => {}
            }
        }
        if crlf + lf == 0 {
            return None;
        }
        let only = !removed.is_empty()
            && removed.len() == added.len()
            && removed.iter().zip(&added).all(|(r, a)| r.0 == a.0)
            && removed.iter().zip(&added).any(|(r, a)| r.1 != a.1);
        Some(Self {
            new: if crlf > lf {
                LineEnding::Crlf
            } else {
                LineEnding::Lf
            },
            only,
        })
    }
}

/// Selected line indices, grouped by hunk index.
type Picks = Vec<(usize, Vec<usize>)>;

/// Changed lines selected in one file of a diff.
#[derive(Default)]
struct LineSelection {
    /// The file the lines belong to.
    file: usize,
    /// `(hunk, line)` pairs.
    lines: BTreeSet<(usize, usize)>,
    /// Row of the last plain or Ctrl click, where Shift+click extends from.
    anchor: Option<usize>,
}

/// A partial change of the working copy, on a hunk or the selected lines.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PartAction {
    Stage,
    Unstage,
    Discard,
}

impl PartAction {
    /// The actions offered on a working-copy side, in display order.
    fn for_side(side: DiffSide) -> &'static [Self] {
        match side {
            DiffSide::Unstaged => &[Self::Discard, Self::Stage],
            DiffSide::Staged => &[Self::Unstage],
        }
    }

    fn verb(self) -> &'static str {
        match self {
            Self::Stage => "Stage",
            Self::Unstage => "Unstage",
            Self::Discard => "Discard",
        }
    }

    /// Staging matches the index (old side); unstaging and discarding
    /// match the new side and apply in reverse.
    fn direction(self) -> PatchDirection {
        match self {
            Self::Stage => PatchDirection::Forward,
            Self::Unstage | Self::Discard => PatchDirection::Reverse,
        }
    }
}

impl DiffDoc {
    pub(crate) fn new(files: Vec<FileDiff>) -> Self {
        let mut rows = Vec::new();
        let mut max_lineno = 0;
        for (f, file) in files.iter().enumerate() {
            rows.push(DiffRow::File(f));
            if file.binary {
                rows.push(DiffRow::Note("Binary file"));
                continue;
            }
            if file.truncated {
                rows.push(DiffRow::Note("Diff too large to display"));
                continue;
            }
            for (h, hunk) in file.hunks.iter().enumerate() {
                rows.push(DiffRow::Hunk(f, h));
                rows.extend((0..hunk.lines.len()).map(|l| DiffRow::Line(f, h, l)));
                max_lineno = hunk
                    .lines
                    .iter()
                    .flat_map(|l| [l.old_lineno, l.new_lineno])
                    .flatten()
                    .fold(max_lineno, u32::max);
            }
        }
        let endings = files
            .iter()
            .map(|f| {
                (!f.binary && !f.truncated)
                    .then(|| Endings::of(f))
                    .flatten()
            })
            .collect();
        Self {
            files,
            rows,
            lineno_width: max_lineno.to_string().len(),
            endings,
            selection: Mutex::default(),
            colors: Mutex::default(),
        }
    }

    /// The syntax colors of line `l` of hunk `h` of file `f`. A hunk is
    /// highlighted from its first line up to the one asked, once.
    fn spans(&self, f: usize, h: usize, l: usize) -> Spans {
        let mut colors = self.colors.lock().unwrap_or_else(PoisonError::into_inner);
        let entry = colors.entry((f, h)).or_insert_with(|| {
            syntax::find(self.files[f].display_path()).map(|s| HunkColors {
                old: LineHighlighter::new(s),
                new: LineHighlighter::new(s),
                lines: Vec::new(),
            })
        });
        let Some(hunk) = entry else {
            return Spans::new();
        };
        let lines = &self.files[f].hunks[h].lines;
        while hunk.lines.len() <= l {
            let Some(line) = lines.get(hunk.lines.len()) else {
                return Spans::new();
            };
            let text = line.content.trim_end_matches('\r');
            let spans = match line.origin {
                LineOrigin::Context => {
                    hunk.old.line(text);
                    hunk.new.line(text)
                }
                LineOrigin::Removed => hunk.old.line(text),
                LineOrigin::Added => hunk.new.line(text),
            };
            hunk.lines.push(spans);
        }
        hunk.lines[l].clone()
    }

    fn selection(&self) -> std::sync::MutexGuard<'_, LineSelection> {
        self.selection
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// The changed line shown on `row`, if any.
    fn changed_line(&self, row: usize) -> Option<(usize, usize, usize)> {
        match *self.rows.get(row)? {
            DiffRow::Line(f, h, l)
                if self.files[f].hunks[h].lines[l].origin != LineOrigin::Context =>
            {
                Some((f, h, l))
            }
            _ => None,
        }
    }

    fn is_selected(&self, f: usize, h: usize, l: usize) -> bool {
        let s = self.selection();
        s.file == f && s.lines.contains(&(h, l))
    }

    /// A click on `row`: plain selects that line alone (or clears it when
    /// it was the only one), `toggle` adds or removes it, `extend` selects
    /// the changed lines from the anchor to it.
    fn click_line(&self, row: usize, toggle: bool, extend: bool) {
        let Some((f, h, l)) = self.changed_line(row) else {
            return;
        };
        let mut s = self.selection();
        if s.file != f {
            *s = LineSelection {
                file: f,
                ..LineSelection::default()
            };
        }
        match s.anchor {
            Some(anchor) if extend => {
                let (from, to) = (anchor.min(row), anchor.max(row));
                for r in from..=to {
                    if let Some((rf, rh, rl)) = self.changed_line(r)
                        && rf == f
                    {
                        s.lines.insert((rh, rl));
                    }
                }
            }
            _ if toggle || extend => {
                if !s.lines.remove(&(h, l)) {
                    s.lines.insert((h, l));
                }
                s.anchor = Some(row);
            }
            _ => {
                let only = s.lines.len() == 1 && s.lines.contains(&(h, l));
                s.lines.clear();
                if !only {
                    s.lines.insert((h, l));
                }
                s.anchor = Some(row);
            }
        }
    }

    /// The selected lines grouped by hunk, with their file; `None` when
    /// nothing is selected.
    fn selected(&self) -> Option<(usize, Picks)> {
        let s = self.selection();
        let mut picks: Picks = Vec::new();
        for &(h, l) in &s.lines {
            match picks.last_mut() {
                Some((last, lines)) if *last == h => lines.push(l),
                _ => picks.push((h, vec![l])),
            }
        }
        (!picks.is_empty()).then_some((s.file, picks))
    }

    fn clear_selection(&self) {
        *self.selection() = LineSelection::default();
    }

    /// The working-copy line (1-based) shown at line `l` of hunk `h` of
    /// file `f`; a removed line maps to the line now in its place.
    fn new_line_at(&self, f: usize, h: usize, l: usize) -> u32 {
        let hunk = &self.files[f].hunks[h];
        let line = &hunk.lines[l];
        line.new_lineno.unwrap_or_else(|| {
            hunk.lines[..l]
                .iter()
                .rev()
                .find_map(|p| p.new_lineno)
                .map_or(hunk.new_start.max(1), |n| n + 1)
        })
    }

    /// Where the editor opens on file `f`: its first changed line.
    fn first_change(&self, f: usize) -> u32 {
        let hunks = &self.files[f].hunks;
        hunks
            .iter()
            .enumerate()
            .find_map(|(h, hunk)| {
                let l = hunk
                    .lines
                    .iter()
                    .position(|l| l.origin != LineOrigin::Context)?;
                Some(self.new_line_at(f, h, l))
            })
            .unwrap_or(1)
    }

    /// Whether file `f` can be edited: a text file still in the worktree.
    fn editable(&self, f: usize) -> bool {
        let file = &self.files[f];
        !file.binary && file.new_path.is_some()
    }
}

/// Builds the patch of `picks` in file `f` and runs `action` with it;
/// Discard waits for a second click on `arm_key`.
fn run_part<V: Staging>(
    v: &mut V,
    doc: &DiffDoc,
    f: usize,
    picks: &[(usize, Vec<usize>)],
    action: PartAction,
    arm_key: String,
    cx: &mut Context<V>,
) {
    let file = &doc.files[f];
    let patch = match selection_patch(file, picks, action.direction()) {
        Ok(p) => p,
        Err(e) => return v.report(e.to_string(), cx),
    };
    match action {
        PartAction::Discard => {
            let target = file.display_path().to_owned();
            arm_discard(v, arm_key, cx, move |a, repo, _| {
                a.discard_patch(repo, &target, &patch)
            });
        }
        PartAction::Stage | PartAction::Unstage => {
            let direction = action.direction();
            v.run_job(
                format!("{} lines", action.verb()),
                cx,
                Box::new(move |a, repo, _| a.apply_patch(repo, &patch, direction)),
            );
        }
    }
}

/// A virtualized diff: only the visible rows are built each frame. With
/// `side`, hunk headers get stage / unstage / discard buttons and changed
/// lines can be selected; with `scroll`, the list can be scrolled from
/// outside.
pub(crate) fn diff_list<V: Staging>(
    id: SharedString,
    doc: Arc<DiffDoc>,
    side: Option<DiffSide>,
    scroll: Option<&UniformListScrollHandle>,
    cx: &mut Context<V>,
) -> AnyElement {
    let count = doc.rows.len();
    let scroll_id = SharedString::from(format!("{id}-scroll"));
    let list = uniform_list(
        id,
        count,
        cx.processor(move |this: &mut V, range: Range<usize>, _window, cx| {
            let armed = this.panel().armed().map(str::to_owned);
            range
                .filter_map(|i| doc.rows.get(i).map(|&row| (i, row)))
                .map(|(i, row)| render_diff_row(&doc, i, row, side, armed.as_deref(), cx))
                .collect()
        }),
    );
    let list = scroll_list(scroll_id, list);
    match scroll {
        Some(handle) => list.handle(handle),
        None => list,
    }
    .into_any_element()
}

/// Renders one row of a flattened diff (all rows share one height).
fn render_diff_row<V: Staging>(
    doc: &Arc<DiffDoc>,
    index: usize,
    row: DiffRow,
    side: Option<DiffSide>,
    armed: Option<&str>,
    cx: &mut Context<V>,
) -> AnyElement {
    let base = div()
        .w_full()
        .h(row_height(cx))
        .flex()
        .items_center()
        .px_2()
        .whitespace_nowrap()
        .overflow_hidden();
    match row {
        DiffRow::File(f) => {
            let file = &doc.files[f];
            let (added, removed) = file.line_stats();
            base.justify_between()
                .bg(color(cx, T::BgPanel))
                .border_t_1()
                .border_color(color(cx, T::BorderSubtle))
                .child(div().truncate().child(file.display_path().to_owned()))
                .child(
                    div()
                        .flex_none()
                        .text_xs()
                        .text_color(color(cx, T::FgMuted))
                        .child(format!("+{added} −{removed}")),
                )
                .into_any_element()
        }
        DiffRow::Note(note) => base
            .text_color(color(cx, T::FgMuted))
            .child(note)
            .into_any_element(),
        DiffRow::Hunk(f, h) => {
            let file = &doc.files[f];
            let hunk = &file.hunks[h];
            let mut header = base
                .justify_between()
                .text_xs()
                .text_color(color(cx, T::FgSubtle))
                .child(format!(
                    "@@ -{},{} +{},{} @@ {}",
                    hunk.old_start, hunk.old_lines, hunk.new_start, hunk.new_lines, hunk.header
                ));
            if let Some(side) = side {
                let path = file.display_path();
                let buttons = PartAction::for_side(side).iter().map(|&action| {
                    let key = format!("\0hunk-{path}-{h}");
                    let label = if action == PartAction::Discard && armed == Some(key.as_str()) {
                        "Discard hunk?".to_owned()
                    } else {
                        format!("{} hunk", action.verb())
                    };
                    let doc = doc.clone();
                    button(
                        SharedString::from(format!("hunk-{}-{path}-{h}", action.verb())),
                        label,
                        cx,
                    )
                    // The patch is built on click, not on every frame.
                    .on_click(cx.listener(move |v: &mut V, _, _, cx| {
                        let all = (0..doc.files[f].hunks[h].lines.len()).collect();
                        run_part(v, &doc, f, &[(h, all)], action, key.clone(), cx);
                    }))
                });
                header = header.child(div().flex().flex_none().gap_1().children(buttons));
            }
            header.into_any_element()
        }
        DiffRow::Line(f, h, l) => {
            let line = &doc.files[f].hunks[h].lines[l];
            let selectable = side.is_some() && line.origin != LineOrigin::Context;
            let selected = selectable && doc.is_selected(f, h, l);
            let spans = doc.spans(f, h, l);
            let row = diff_line_row(line, &spans, doc.lineno_width, selected, cx);
            let editable = side.is_some() && doc.editable(f);
            if !selectable && !editable {
                return row.into_any_element();
            }
            let doc = doc.clone();
            row.id(SharedString::from(format!("diff-line-{index}")))
                .when(selectable, Styled::cursor_pointer)
                .on_click(cx.listener(move |v: &mut V, e: &ClickEvent, window, cx| {
                    if editable && e.click_count() >= 2 {
                        // The first click of the two selected the line.
                        doc.clear_selection();
                        let file = doc.files[f].display_path().to_owned();
                        file_edit::open(v, file, doc.new_line_at(f, h, l), window, cx);
                        return;
                    }
                    if selectable {
                        let m = e.modifiers();
                        doc.click_line(index, m.control || m.platform, m.shift);
                        cx.notify();
                    }
                }))
                .into_any_element()
        }
    }
}

/// One line of a hunk: line-number gutter, +/- marker, then the file's
/// text, colored by `spans`. A `selected` line has its gutter in the accent
/// color.
fn diff_line_row(
    line: &DiffLine,
    spans: &[(Range<usize>, T)],
    lineno_width: usize,
    selected: bool,
    cx: &App,
) -> Div {
    let base = div()
        .w_full()
        .h(row_height(cx))
        .flex()
        .items_center()
        .whitespace_nowrap()
        .overflow_hidden();
    let (marker, marker_fg, bg) = match line.origin {
        LineOrigin::Added => ("+", T::DiffAddFg, Some(color(cx, T::DiffAddBg))),
        LineOrigin::Removed => ("-", T::DiffDelFg, Some(color(cx, T::DiffDelBg))),
        LineOrigin::Context => (" ", T::FgSubtle, None),
    };
    // The font is monospace, so padding aligns the gutter.
    let width = lineno_width;
    let number = |n: Option<u32>| n.map_or_else(String::new, |n| n.to_string());
    // Gutter (line numbers and marker) sits apart from the file's text,
    // on the panel color behind a separator: the marker must not read as
    // part of the line.
    base.font_family(mono_font(cx))
        .text_xs()
        .when_some(bg, gpui_kit::Styled::bg)
        .child(
            div()
                .flex_none()
                .h_full()
                .flex()
                .items_center()
                .gap_2()
                .px_2()
                .bg(color(cx, if selected { T::Accent } else { T::BgPanel }))
                .border_r_1()
                .border_color(color(cx, T::BorderSubtle))
                .text_color(color(cx, if selected { T::AccentFg } else { T::FgSubtle }))
                .child(format!(
                    "{:>width$} {:>width$}",
                    number(line.old_lineno),
                    number(line.new_lineno)
                ))
                .child(
                    div()
                        .text_color(color(cx, if selected { T::AccentFg } else { marker_fg }))
                        .child(marker),
                ),
        )
        .child(div().pl_2().child(
            StyledText::new(line.content.trim_end_matches('\r').to_owned()).with_highlights(
                spans.iter().map(|(range, token)| {
                    let style = HighlightStyle {
                        color: Some(color(cx, *token)),
                        ..HighlightStyle::default()
                    };
                    (range.clone(), style)
                }),
            ),
        ))
}

/// An open diff with its header (title, note, keys), in place of the
/// graph or the dashboard rows; `doc` is `None` while it loads.
pub(crate) fn render_open<V: Staging>(
    v: &mut V,
    title: String,
    note: &str,
    side: Option<DiffSide>,
    doc: Option<Arc<DiffDoc>>,
    cx: &mut Context<V>,
) -> AnyElement {
    let id = SharedString::from(format!("open-diff-{note}-{title}"));
    // A working-copy diff is titled with its file.
    let editing = file_edit::current(v, side.map(|_| title.as_str()), cx);
    let keys = if side.is_some() {
        "click lines to select · double-click edits · ↑↓ files · Esc closes"
    } else {
        "↑↓ files · Esc closes"
    };
    let selection = side.filter(|_| !editing).zip(
        doc.as_ref()
            .and_then(|d| d.selected().map(|s| (d.clone(), s))),
    );
    // Edit, on a working-copy diff of a text file still in the worktree.
    let edit_at = side
        .filter(|_| !editing)
        .and(doc.as_ref())
        .and_then(|d| (d.files.len() == 1 && d.editable(0)).then(|| d.first_change(0)));
    // Line-ending conversion, on an unstaged diff of a text file still in
    // the worktree: the side whose new lines are the file's.
    let endings = side
        .filter(|&s| !editing && s == DiffSide::Unstaged)
        .and(doc.as_ref())
        .and_then(|d| (d.files.len() == 1 && d.editable(0)).then_some(d.endings[0]))
        .flatten();
    // A one-file diff of a deleted file says so: all-red lines alone read
    // like an edit that removed everything.
    let deleted = doc
        .as_ref()
        .is_some_and(|d| d.files.len() == 1 && d.files[0].kind == ChangeKind::Deleted);
    let edit_bar = if editing {
        file_edit::render_bar(v, cx)
    } else {
        None
    };
    let header = div()
        .flex()
        .flex_none()
        .items_center()
        .gap_2()
        .px_2()
        .py_1()
        .border_b_1()
        .border_color(color(cx, T::BorderSubtle))
        .when(deleted, |header| {
            let del = color(cx, T::DiffDelFg);
            header.child(
                div()
                    .flex_none()
                    .px_1()
                    .text_xs()
                    .text_color(del)
                    .border_1()
                    .border_color(del)
                    .rounded_sm()
                    .child("deleted"),
            )
        })
        .child(div().flex_1().min_w_0().truncate().child(title.clone()))
        .when(!editing, |header| {
            header.child(
                div()
                    .flex_none()
                    .text_xs()
                    .text_color(color(cx, T::FgMuted))
                    .child(format!("{note} · {keys}")),
            )
        })
        .when_some(selection, |header, (side, (doc, (f, picks)))| {
            header.child(selection_bar(v, side, &doc, f, picks, cx))
        })
        .when_some(endings, |header, endings| {
            header.child(endings_bar(title.clone(), endings, cx))
        })
        .when_some(edit_at, |header, line| {
            header.child(button("diff-edit", "Edit", cx).on_click(cx.listener(
                move |v: &mut V, _, window, cx| {
                    file_edit::open(v, title.clone(), line, window, cx);
                },
            )))
        })
        .children(edit_bar);
    let body = match doc {
        _ if editing => file_edit::render_body(v, cx),
        Some(doc) => diff_list(id, doc, side, None, cx),
        None => div()
            .p_3()
            .text_color(color(cx, T::FgMuted))
            .child("Loading diff…")
            .into_any_element(),
    };
    div()
        .flex_1()
        .min_w(px(0.0))
        .flex()
        .flex_col()
        .overflow_hidden()
        .child(header)
        .child(body)
        .into_any_element()
}

/// The file's line ending and a button converting it to the other one, in
/// the open diff's header; says so when only line endings changed.
fn endings_bar<V: Staging>(file: String, endings: Endings, cx: &mut Context<V>) -> Div {
    let to = endings.new.other();
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap_1()
        .text_xs()
        .child(
            div()
                .text_color(color(
                    cx,
                    if endings.only {
                        T::StatusWarning
                    } else {
                        T::FgMuted
                    },
                ))
                .child(if endings.only {
                    format!("only line endings changed · {}", endings.new.name())
                } else {
                    endings.new.name().to_owned()
                }),
        )
        .child(
            button("diff-eol", format!("To {}", to.name()), cx).on_click(cx.listener(
                move |v: &mut V, _, _, cx| file_edit::convert_file(v, file.clone(), to, cx),
            )),
        )
}

/// The selected lines' count and actions, in the open diff's header.
fn selection_bar<V: Staging>(
    v: &mut V,
    side: DiffSide,
    doc: &Arc<DiffDoc>,
    f: usize,
    picks: Picks,
    cx: &mut Context<V>,
) -> Div {
    let count: usize = picks.iter().map(|(_, l)| l.len()).sum();
    let picks = Arc::new(picks);
    let armed = v.panel().armed().map(str::to_owned);
    let key = format!("\0lines-{}", doc.files[f].display_path());
    let buttons = PartAction::for_side(side).iter().map(|&action| {
        let label = if action == PartAction::Discard && armed.as_deref() == Some(key.as_str()) {
            "Discard lines?".to_owned()
        } else {
            format!("{} lines", action.verb())
        };
        let (doc, picks, key) = (doc.clone(), picks.clone(), key.clone());
        button(
            SharedString::from(format!("selection-{}", action.verb())),
            label,
            cx,
        )
        .on_click(cx.listener(move |v: &mut V, _, _, cx| {
            run_part(v, &doc, f, &picks, action, key.clone(), cx);
        }))
    });
    let clear = {
        let doc = doc.clone();
        button("selection-clear", "Clear", cx).on_click(cx.listener(move |_v: &mut V, _, _, cx| {
            doc.clear_selection();
            cx.notify();
        }))
    };
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap_1()
        .text_xs()
        .child(
            div()
                .text_color(color(cx, T::FgMuted))
                .child(format!("{count} selected")),
        )
        .children(buttons)
        .child(clear)
}

/// The note naming a working-copy side.
pub(crate) fn side_note(side: DiffSide) -> &'static str {
    match side {
        DiffSide::Staged => "Staged",
        DiffSide::Unstaged => "Unstaged",
    }
}

/// Working-copy files in the staging panel's order: unstaged, then staged.
pub(crate) fn worktree_files(changes: &[FileChange]) -> Vec<(String, DiffSide)> {
    let unstaged = changes.iter().filter(|c| c.is_unstaged());
    let staged = changes.iter().filter(|c| c.is_staged());
    unstaged
        .map(|c| (c.path.clone(), DiffSide::Unstaged))
        .chain(staged.map(|c| (c.path.clone(), DiffSide::Staged)))
        .collect()
}

/// Where the open diff of `file` on `side` goes once the working copy
/// changed: the same side while the file has changes there, the other
/// side once they all went there (file or hunks staged), else nowhere.
pub(crate) fn follow(changes: &[FileChange], file: &str, side: DiffSide) -> Option<DiffSide> {
    let other = match side {
        DiffSide::Staged => DiffSide::Unstaged,
        DiffSide::Unstaged => DiffSide::Staged,
    };
    let has_changes = |side| {
        changes.iter().any(|c| {
            c.path == file
                && match side {
                    DiffSide::Staged => c.is_staged(),
                    DiffSide::Unstaged => c.is_unstaged(),
                }
        })
    };
    [side, other].into_iter().find(|&s| has_changes(s))
}

/// The file `delta` places from `file` on `side` in `files`, clamped to
/// the list; `None` when the open file is not in it.
pub(crate) fn step(
    files: &[(String, DiffSide)],
    file: &str,
    side: DiffSide,
    delta: i64,
) -> Option<(String, DiffSide)> {
    let at = files.iter().position(|(f, s)| f == file && *s == side)?;
    let last = i64::try_from(files.len()).unwrap_or(i64::MAX) - 1;
    let at = i64::try_from(at).unwrap_or(0);
    let next = usize::try_from((at + delta).clamp(0, last)).unwrap_or(0);
    files.get(next).cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use archipel_model::diff::Hunk;

    fn file(lines: &[(LineOrigin, &str)]) -> FileDiff {
        FileDiff {
            old_path: Some("a".into()),
            new_path: Some("a".into()),
            kind: ChangeKind::Modified,
            binary: false,
            hunks: vec![Hunk {
                old_start: 1,
                old_lines: 0,
                new_start: 1,
                new_lines: 0,
                header: String::new(),
                lines: lines
                    .iter()
                    .map(|&(origin, content)| DiffLine {
                        origin,
                        content: content.into(),
                        old_lineno: None,
                        new_lineno: None,
                        no_newline_at_eof: false,
                    })
                    .collect(),
            }],
            truncated: false,
        }
    }

    #[test]
    fn endings_follow_the_new_side() {
        use LineOrigin::{Added, Context, Removed};
        let e = Endings::of(&file(&[
            (Removed, "a"),
            (Removed, "b"),
            (Added, "a\r"),
            (Added, "b\r"),
        ]))
        .unwrap();
        assert_eq!(e.new, LineEnding::Crlf);
        assert!(e.only);

        let e = Endings::of(&file(&[(Context, "x"), (Removed, "a\r"), (Added, "a")])).unwrap();
        assert_eq!(e.new, LineEnding::Lf);
        assert!(e.only);

        let e = Endings::of(&file(&[(Context, "x\r"), (Removed, "a\r"), (Added, "b\r")])).unwrap();
        assert_eq!(e.new, LineEnding::Crlf);
        assert!(!e.only);
    }
}

// Needs a headless renderer, which gpui only has on Linux and macOS.
#[cfg(test)]
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod render_tests;
