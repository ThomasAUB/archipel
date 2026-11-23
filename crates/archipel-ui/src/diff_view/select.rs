//! Text selected in a diff, kept in the diff's own coordinates (row of the
//! flattened diff, byte in the line's text) so it outlives scrolling and
//! the virtualized rows drawing it. Only diff lines hold text: file and
//! hunk headers and notes are skipped when copying, unless as a patch.

use std::fmt::Write as _;
use std::ops::Range;

use archipel_model::diff::{DiffLine, LineOrigin};
use gpui_kit::{Bounds, Pixels, Point};

use super::{DiffDoc, DiffRow};

/// A position in the diff's text: a row of the flattened diff and a byte
/// offset in its line's text (without its `\r`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Caret {
    pub(crate) row: usize,
    pub(crate) col: usize,
}

impl Caret {
    pub(crate) fn new(row: usize, col: usize) -> Self {
        Self { row, col }
    }
}

/// What a drag extends by: characters after a press, words after a
/// double-click, lines after a triple-click.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Unit {
    #[default]
    Char,
    Word,
    Line,
}

/// The text selection of a diff and the drag making it.
#[derive(Clone, Debug, Default)]
pub(crate) struct TextSelection {
    /// The text first selected by the press (empty for a plain one, the
    /// word or the line for a double or triple click): the selection
    /// always keeps it.
    origin: (Caret, Caret),
    /// Where the drag is now (or ended).
    head: Caret,
    unit: Unit,
    /// The left button is held since a press on a line.
    pub(crate) dragging: bool,
    /// The mouse moved since the press, which is then no click: line
    /// picks and double-click editing leave it alone.
    pub(crate) dragged: bool,
}

impl TextSelection {
    /// The selected text, from its start to its end (equal when nothing is
    /// selected).
    pub(crate) fn range(&self) -> (Caret, Caret) {
        let (a, b) = self.origin;
        (a.min(self.head), b.max(self.head))
    }

    pub(crate) fn is_empty(&self) -> bool {
        let (a, b) = self.range();
        a == b
    }

    /// The bytes of row `row`'s text selected, of a line `len` bytes
    /// long, and whether its line break is too.
    pub(crate) fn on_row(&self, row: usize, len: usize) -> Option<(Range<usize>, bool)> {
        let (a, b) = self.range();
        if a == b || row < a.row || row > b.row {
            return None;
        }
        let from = if row == a.row { a.col.min(len) } else { 0 };
        let to = if row == b.row { b.col.min(len) } else { len };
        Some((from..to.max(from), row < b.row))
    }
}

impl DiffDoc {
    /// The diff line on `row`, if it is one.
    pub(crate) fn line_at(&self, row: usize) -> Option<&DiffLine> {
        match *self.rows.get(row)? {
            DiffRow::Line(f, h, l) => Some(&self.files[f].hunks[h].lines[l]),
            DiffRow::Gap(g, l) => Some(&self.gaps[g].lines[l]),
            _ => None,
        }
    }

    /// The text of the line on `row`, as shown (no `\r`); empty off lines.
    pub(crate) fn row_text(&self, row: usize) -> &str {
        self.line_at(row)
            .map_or("", |l| l.content.trim_end_matches('\r'))
    }

    pub(crate) fn text_selection(&self) -> TextSelection {
        self.text_sel().clone()
    }

    fn text_sel(&self) -> std::sync::MutexGuard<'_, TextSelection> {
        self.text
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// A left press at `at`: `click_count` 1 starts a selection there
    /// (extends the current one with `extend`), 2 selects the word, 3
    /// the line. Returns whether the press extended a selection, which
    /// is then no line pick.
    pub(crate) fn press(&self, at: Caret, click_count: usize, extend: bool) -> bool {
        let mut s = self.text_sel();
        s.dragging = true;
        s.dragged = false;
        if extend && click_count <= 1 && !s.is_empty() {
            s.head = self.snap(at, s.unit, s.origin);
            // No click: Shift+click picks no lines then.
            s.dragged = true;
            return true;
        }
        let (unit, origin) = match click_count {
            2 => (Unit::Word, self.word_at(at)),
            n if n >= 3 => (Unit::Line, self.line_span(at.row)),
            _ => (Unit::Char, (at, at)),
        };
        *s = TextSelection {
            origin,
            head: origin.0,
            unit,
            dragging: true,
            dragged: false,
        };
        false
    }

    /// The mouse, held, moved to `at`.
    pub(crate) fn drag_to(&self, at: Caret) -> bool {
        let mut s = self.text_sel();
        if !s.dragging {
            return false;
        }
        let head = self.snap(at, s.unit, s.origin);
        if head == s.head {
            return false;
        }
        s.head = head;
        s.dragged = true;
        true
    }

    /// The left button was released (anywhere).
    pub(crate) fn release(&self) {
        self.text_sel().dragging = false;
    }

    /// Selects nothing.
    pub(crate) fn clear_text(&self) {
        *self.text_sel() = TextSelection::default();
    }

    /// Whether some text is selected.
    pub(crate) fn has_text(&self) -> bool {
        !self.text_sel().is_empty()
    }

    /// `at` moved to the edge of the unit the selection grows by, on the
    /// side away from `origin`.
    fn snap(&self, at: Caret, unit: Unit, origin: (Caret, Caret)) -> Caret {
        match unit {
            Unit::Char => at,
            Unit::Word => {
                let (start, end) = self.word_at(at);
                if at < origin.0 { start } else { end }
            }
            Unit::Line => {
                let (start, end) = self.line_span(at.row);
                if at < origin.0 { start } else { end }
            }
        }
    }

    /// From the start of `row`'s line to the start of the next row: the
    /// whole line with its break (its end, on the last row).
    fn line_span(&self, row: usize) -> (Caret, Caret) {
        let end = if row + 1 < self.rows.len() {
            Caret::new(row + 1, 0)
        } else {
            Caret::new(row, self.row_text(row).len())
        };
        (Caret::new(row, 0), end)
    }

    /// The word around `at` (a run of word characters, of spaces, or one
    /// other character), or nothing past the line's end.
    fn word_at(&self, at: Caret) -> (Caret, Caret) {
        let r = word_bounds(self.row_text(at.row), at.col);
        (Caret::new(at.row, r.start), Caret::new(at.row, r.end))
    }

    /// The selected code: the lines' text only, joined by line breaks,
    /// without line numbers, `+` / `-` markers or headers.
    pub(crate) fn selected_text(&self) -> Option<String> {
        let s = self.text_sel().clone();
        let (a, b) = s.range();
        if a == b {
            return None;
        }
        let mut out = String::new();
        let mut first = true;
        for row in a.row..=b.row.min(self.rows.len().saturating_sub(1)) {
            let Some(line) = self.line_at(row) else {
                continue;
            };
            let text = line.content.trim_end_matches('\r');
            let Some((range, _)) = s.on_row(row, text.len()) else {
                continue;
            };
            if !first {
                out.push('\n');
            }
            first = false;
            out.push_str(text.get(range).unwrap_or(""));
        }
        // A selection ending at the start of a header row took the line
        // break before it (before a line, the empty last piece did).
        if b.col == 0 && b.row > a.row && self.line_at(b.row).is_none() {
            out.push('\n');
        }
        Some(out)
    }

    /// The rows from `a` to `b` as a patch: the lines whole, with their
    /// markers, under their file and hunk headers.
    pub(crate) fn patch_text(&self, rows: Range<usize>) -> String {
        let mut out = String::new();
        let rows = rows.start..rows.end.min(self.rows.len());
        // The hunk of the first row, when it starts inside one.
        if let Some(DiffRow::Line(f, h, _)) = self.rows.get(rows.start).copied() {
            self.push_file_header(&mut out, f);
            self.push_hunk_header(&mut out, f, h);
        }
        for row in rows {
            match self.rows[row] {
                DiffRow::File(f) => self.push_file_header(&mut out, f),
                // Outside the hunks: no part of a patch.
                DiffRow::Note(_) | DiffRow::Gap(..) => {}
                DiffRow::Hunk(f, h) => self.push_hunk_header(&mut out, f, h),
                DiffRow::Line(f, h, l) => {
                    let line = &self.files[f].hunks[h].lines[l];
                    out.push(match line.origin {
                        LineOrigin::Added => '+',
                        LineOrigin::Removed => '-',
                        LineOrigin::Context => ' ',
                    });
                    out.push_str(line.content.trim_end_matches('\r'));
                    out.push('\n');
                    if line.no_newline_at_eof {
                        out.push_str("\\ No newline at end of file\n");
                    }
                }
            }
        }
        out
    }

    /// The right-click menu, open at `at` on `row`.
    pub(crate) fn open_menu(&self, at: Point<Pixels>, row: usize) {
        *self.menu_lock() = Some((at, row));
    }

    pub(crate) fn close_menu(&self) -> bool {
        self.menu_lock().take().is_some()
    }

    pub(crate) fn menu(&self) -> Option<(Point<Pixels>, usize)> {
        *self.menu_lock()
    }

    fn menu_lock(&self) -> std::sync::MutexGuard<'_, Option<(Point<Pixels>, usize)>> {
        self.menu
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The bounds the list was last drawn in.
    pub(crate) fn view_bounds(&self) -> Option<Bounds<Pixels>> {
        *self
            .view
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub(crate) fn set_view_bounds(&self, bounds: Bounds<Pixels>) {
        *self
            .view
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(bounds);
    }

    /// The selected rows as a patch, see [`Self::patch_text`].
    pub(crate) fn selected_patch(&self) -> Option<String> {
        let (a, b) = self.text_sel().range();
        if a == b {
            return None;
        }
        // A selection ending at the start of a row leaves that row out.
        let end = if b.col == 0 && b.row > a.row {
            b.row
        } else {
            b.row + 1
        };
        Some(self.patch_text(a.row..end))
    }

    /// The rows of the hunk shown on `row` (its header included).
    pub(crate) fn hunk_rows(&self, row: usize) -> Option<Range<usize>> {
        let (DiffRow::Hunk(f, h) | DiffRow::Line(f, h, _)) = *self.rows.get(row)? else {
            return None;
        };
        let start = (0..=row)
            .rev()
            .find(|&r| matches!(self.rows[r], DiffRow::Hunk(rf, rh) if (rf, rh) == (f, h)))?;
        Some(start..start + 1 + self.files[f].hunks[h].lines.len())
    }

    fn push_file_header(&self, out: &mut String, f: usize) {
        let file = &self.files[f];
        let old = file
            .old_path
            .as_deref()
            .map_or_else(|| "/dev/null".to_owned(), |p| format!("a/{p}"));
        let new = file
            .new_path
            .as_deref()
            .map_or_else(|| "/dev/null".to_owned(), |p| format!("b/{p}"));
        let _ = writeln!(out, "--- {old}\n+++ {new}");
    }

    fn push_hunk_header(&self, out: &mut String, f: usize, h: usize) {
        let hunk = &self.files[f].hunks[h];
        let header = if hunk.header.is_empty() {
            String::new()
        } else {
            format!(" {}", hunk.header)
        };
        let _ = writeln!(
            out,
            "@@ -{},{} +{},{} @@{header}",
            hunk.old_start, hunk.old_lines, hunk.new_start, hunk.new_lines
        );
    }
}

/// A line's runs to draw: its syntax `spans` cut where the selection
/// `sel` starts and ends, each with its color (none: the default) and
/// whether it is selected. Text in no run keeps the default look.
pub(crate) fn runs<C: Copy>(
    spans: &[(Range<usize>, C)],
    sel: Option<&Range<usize>>,
) -> Vec<(Range<usize>, Option<C>, bool)> {
    let Some(sel) = sel.filter(|s| !s.is_empty()) else {
        return spans
            .iter()
            .map(|(r, c)| (r.clone(), Some(*c), false))
            .collect();
    };
    let mut cuts: Vec<usize> = spans
        .iter()
        .flat_map(|(r, _)| [r.start, r.end])
        .chain([sel.start, sel.end])
        .collect();
    cuts.sort_unstable();
    cuts.dedup();
    cuts.windows(2)
        .filter_map(|w| {
            let (from, to) = (w[0], w[1]);
            let color = spans
                .iter()
                .find(|(r, _)| r.start <= from && to <= r.end)
                .map(|(_, c)| *c);
            let selected = sel.start <= from && to <= sel.end;
            (color.is_some() || selected).then_some((from..to, color, selected))
        })
        .collect()
}

/// The word of `text` around byte `col`: a run of word characters
/// (letters, digits, `_`), a run of whitespace, or one other character.
/// Empty at or past the end.
pub(crate) fn word_bounds(text: &str, col: usize) -> Range<usize> {
    let col = col.min(text.len());
    let Some(c) = text[col..].chars().next() else {
        return col..col;
    };
    let class = |c: char| {
        if c.is_alphanumeric() || c == '_' {
            1
        } else if c.is_whitespace() {
            2
        } else {
            3
        }
    };
    let k = class(c);
    if k == 3 {
        return col..col + c.len_utf8();
    }
    let start = text[..col]
        .char_indices()
        .rev()
        .take_while(|&(_, c)| class(c) == k)
        .last()
        .map_or(col, |(i, _)| i);
    let end = text[col..]
        .char_indices()
        .find(|&(_, c)| class(c) != k)
        .map_or(text.len(), |(i, _)| col + i);
    start..end
}

#[cfg(test)]
mod tests {
    use super::*;
    use archipel_model::changes::ChangeKind;
    use archipel_model::diff::{FileDiff, Hunk};

    /// A one-hunk diff of `lines`: rows 0 (file), 1 (hunk), then the lines
    /// from row 2.
    fn doc(lines: &[(LineOrigin, &str)]) -> DiffDoc {
        DiffDoc::new(vec![FileDiff {
            old_path: Some("src/a.rs".into()),
            new_path: Some("src/a.rs".into()),
            kind: ChangeKind::Modified,
            binary: false,
            hunks: vec![Hunk {
                old_start: 3,
                old_lines: 2,
                new_start: 3,
                new_lines: 2,
                header: "fn main()".into(),
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
        }])
    }

    use LineOrigin::{Added, Context, Removed};

    fn sample() -> DiffDoc {
        doc(&[
            (Context, "let a = 1;\r"),
            (Removed, "let b = old(a);"),
            (Added, "let b = new_value(a);"),
        ])
    }

    #[test]
    fn copies_the_code_without_markers_or_headers() {
        let d = sample();
        d.press(Caret::new(2, 4), 1, false);
        assert!(d.drag_to(Caret::new(4, 9)));
        d.release();
        assert_eq!(
            d.selected_text().as_deref(),
            Some("a = 1;\nlet b = old(a);\nlet b = n")
        );
    }

    #[test]
    fn a_drag_backwards_selects_the_same() {
        let d = sample();
        d.press(Caret::new(4, 9), 1, false);
        d.drag_to(Caret::new(2, 4));
        assert_eq!(
            d.selected_text().as_deref(),
            Some("a = 1;\nlet b = old(a);\nlet b = n")
        );
    }

    #[test]
    fn a_press_alone_selects_nothing() {
        let d = sample();
        d.press(Caret::new(3, 2), 1, false);
        d.release();
        assert!(!d.has_text());
        assert_eq!(d.selected_text(), None);
        assert!(!d.text_selection().dragged);
    }

    #[test]
    fn double_click_selects_a_word_and_drags_by_words() {
        let d = sample();
        d.press(Caret::new(4, 10), 2, false);
        assert_eq!(d.selected_text().as_deref(), Some("new_value"));
        d.drag_to(Caret::new(4, 18));
        assert_eq!(d.selected_text().as_deref(), Some("new_value(a"));
        d.drag_to(Caret::new(4, 1));
        assert_eq!(d.selected_text().as_deref(), Some("let b = new_value"));
    }

    #[test]
    fn triple_click_selects_the_line() {
        let d = sample();
        d.press(Caret::new(3, 3), 3, false);
        assert_eq!(d.selected_text().as_deref(), Some("let b = old(a);\n"));
        d.drag_to(Caret::new(4, 0));
        assert_eq!(
            d.selected_text().as_deref(),
            Some("let b = old(a);\nlet b = new_value(a);")
        );
    }

    #[test]
    fn shift_press_extends() {
        let d = sample();
        d.press(Caret::new(2, 0), 1, false);
        d.drag_to(Caret::new(2, 3));
        d.release();
        assert!(d.press(Caret::new(3, 5), 1, true));
        assert_eq!(d.selected_text().as_deref(), Some("let a = 1;\nlet b"));
        // Without a selection, Shift+click is a line pick.
        d.clear_text();
        assert!(!d.press(Caret::new(3, 5), 1, true));
    }

    #[test]
    fn hunk_headers_are_skipped() {
        let d = sample();
        d.press(Caret::new(0, 0), 1, false);
        d.drag_to(Caret::new(2, 3));
        assert_eq!(d.selected_text().as_deref(), Some("let"));
    }

    #[test]
    fn copies_as_a_patch() {
        let d = sample();
        d.press(Caret::new(3, 4), 1, false);
        d.drag_to(Caret::new(4, 2));
        assert_eq!(
            d.selected_patch().as_deref(),
            Some(
                "--- a/src/a.rs\n+++ b/src/a.rs\n@@ -3,2 +3,2 @@ fn main()\n\
                 -let b = old(a);\n+let b = new_value(a);\n"
            )
        );
        let hunk = d.hunk_rows(3).unwrap();
        assert_eq!(hunk, 1..5);
        assert_eq!(
            d.patch_text(hunk),
            "@@ -3,2 +3,2 @@ fn main()\n let a = 1;\n-let b = old(a);\n+let b = new_value(a);\n"
        );
    }

    #[test]
    fn rows_show_their_part() {
        let d = sample();
        d.press(Caret::new(2, 4), 1, false);
        d.drag_to(Caret::new(4, 9));
        let s = d.text_selection();
        assert_eq!(s.on_row(1, 0), None);
        assert_eq!(s.on_row(2, 10), Some((4..10, true)));
        assert_eq!(s.on_row(3, 15), Some((0..15, true)));
        assert_eq!(s.on_row(4, 21), Some((0..9, false)));
        assert_eq!(s.on_row(5, 3), None);
    }

    #[test]
    fn runs_cut_spans_at_the_selection() {
        let spans = [(0..3, 'k'), (4..9, 's')];
        assert_eq!(
            runs(&spans, None),
            vec![(0..3, Some('k'), false), (4..9, Some('s'), false)]
        );
        assert_eq!(
            runs(&spans, Some(&(2..6))),
            vec![
                (0..2, Some('k'), false),
                (2..3, Some('k'), true),
                (3..4, None, true),
                (4..6, Some('s'), true),
                (6..9, Some('s'), false),
            ]
        );
        assert_eq!(runs::<char>(&[], Some(&(1..4))), vec![(1..4, None, true)]);
    }

    #[test]
    fn words() {
        assert_eq!(word_bounds("foo_bar(x)", 2), 0..7);
        assert_eq!(word_bounds("foo_bar(x)", 7), 7..8);
        assert_eq!(word_bounds("a   b", 2), 1..4);
        assert_eq!(word_bounds("héllo wörld", 8), 7..13);
        assert_eq!(word_bounds("abc", 3), 3..3);
    }
}
