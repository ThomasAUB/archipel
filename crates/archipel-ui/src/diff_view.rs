//! Diffs, shared by the repo tab and the dashboard's quick edit pane: a
//! diff flattened into fixed-height rows and rendered virtualized, with
//! stage / unstage buttons on the hunks of a working-copy diff.

use std::ops::Range;
use std::sync::Arc;

use archipel_model::changes::FileChange;
use archipel_model::diff::{DiffLine, FileDiff, LineOrigin};
use archipel_model::ports::DiffSide;
use archipel_model::requests::PatchDirection;
use archipel_ops::patch::hunk_patch;
use archipel_theme::ColorToken as T;
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, App, Context, SharedString, UniformListScrollHandle, div, px, uniform_list,
};

use crate::staging::Staging;
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
        Self {
            files,
            rows,
            lineno_width: max_lineno.to_string().len(),
        }
    }
}

/// A virtualized diff: only the visible rows are built each frame. With
/// `side`, hunk headers get a stage / unstage button; with `scroll`, the
/// list can be scrolled from outside.
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
        cx.processor(move |_this: &mut V, range: Range<usize>, _window, cx| {
            range
                .filter_map(|i| doc.rows.get(i).copied())
                .map(|row| render_diff_row(&doc, row, side, cx))
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
    row: DiffRow,
    side: Option<DiffSide>,
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
                let (label, direction) = if side == DiffSide::Staged {
                    ("Unstage hunk", PatchDirection::Reverse)
                } else {
                    ("Stage hunk", PatchDirection::Forward)
                };
                let doc = doc.clone();
                header = header.child(
                    button(
                        SharedString::from(format!("hunk-{}-{h}", file.display_path())),
                        label,
                        cx,
                    )
                    // The patch is built on click, not on every frame.
                    .on_click(cx.listener(move |v: &mut V, _, _, cx| {
                        match hunk_patch(&doc.files[f], h) {
                            Ok(p) => v.run_job(
                                label.into(),
                                cx,
                                Box::new(move |a, repo| a.apply_patch(repo, &p, direction)),
                            ),
                            Err(e) => v.report(e.to_string(), cx),
                        }
                    })),
                );
            }
            header.into_any_element()
        }
        DiffRow::Line(f, h, l) => {
            diff_line_row(&doc.files[f].hunks[h].lines[l], doc.lineno_width, cx)
        }
    }
}

/// One line of a hunk: line-number gutter, +/- marker, then the file's text.
fn diff_line_row(line: &DiffLine, lineno_width: usize, cx: &App) -> AnyElement {
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
                .bg(color(cx, T::BgPanel))
                .border_r_1()
                .border_color(color(cx, T::BorderSubtle))
                .text_color(color(cx, T::FgSubtle))
                .child(format!(
                    "{:>width$} {:>width$}",
                    number(line.old_lineno),
                    number(line.new_lineno)
                ))
                .child(div().text_color(color(cx, marker_fg)).child(marker)),
        )
        .child(
            div()
                .pl_2()
                .child(line.content.trim_end_matches('\r').to_owned()),
        )
        .into_any_element()
}

/// An open diff with its header (title, note, keys), in place of the
/// graph or the dashboard rows; `doc` is `None` while it loads.
pub(crate) fn render_open<V: Staging>(
    title: String,
    note: &str,
    side: Option<DiffSide>,
    doc: Option<Arc<DiffDoc>>,
    cx: &mut Context<V>,
) -> AnyElement {
    let id = SharedString::from(format!("open-diff-{note}-{title}"));
    let header = div()
        .flex()
        .flex_none()
        .items_center()
        .gap_2()
        .px_2()
        .py_1()
        .border_b_1()
        .border_color(color(cx, T::BorderSubtle))
        .child(div().flex_1().min_w_0().truncate().child(title))
        .child(
            div()
                .flex_none()
                .text_xs()
                .text_color(color(cx, T::FgMuted))
                .child(format!("{note} · ↑↓ files · Esc closes")),
        );
    let body = match doc {
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
mod render_tests;
