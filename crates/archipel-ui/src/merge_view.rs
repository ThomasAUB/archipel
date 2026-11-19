//! The merge view: a conflicted working-copy file opens here in place of
//! its diff, as three editors. **Ours** and **Theirs** (read only) show the
//! file as each side has it, their conflicting lines highlighted; the
//! **Result** below is the file itself, conflict markers included, and is
//! edited freely. The toolbar works on the current conflict (framed in all
//! three, ↑↓ buttons or Alt+↑ / Alt+↓ move through them): take ours,
//! theirs, both in either order, or the base when the markers hold it; or
//! every conflict at once. Ctrl+S saves the result without resolving,
//! **Mark resolved** (Ctrl+Enter) writes and stages it, refused while
//! markers are left; it is undoable (the conflict comes back). A file that
//! cannot be shown as text (binary, too large) or that a side deleted is
//! resolved whole: keep one side, or delete it. The external merge tool
//! stays one click away.

use std::ops::Range;
use std::sync::{Arc, Mutex, PoisonError};

use archipel_model::changes::{ConflictSides, FileState};
use archipel_ops::OpClass;
use archipel_ops::conflict::{self, Conflict, Pick, Side};
use archipel_theme::ColorToken as T;
use gpui_kit::component::input::{
    Editor, EditorState, InputEvent, Position, RangeDecoration, RangeDecorationCollection,
    RangeDecorationStyle,
};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, App, Context, Div, Entity, Hsla, KeyDownEvent, SharedString, Stateful,
    Subscription, Window, div,
};

use crate::file_edit::{editable_text, file_bytes, language_of};
use crate::staging::{Staging, small};
use crate::theme::{color, mono_font};
use crate::widgets::{button, primary_button};

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

/// A read-only side, with where each conflict of the opened file sits.
struct Pane {
    editor: Entity<EditorState>,
    /// The editor's text.
    text: String,
    /// Per conflict of the opened file, its lines in `text`.
    ranges: Vec<Range<usize>>,
    marks: RangeDecorationCollection,
}

struct TextMerge {
    /// The file as opened (LF line endings), conflict markers included.
    original: String,
    /// Its conflicts.
    conflicts: Vec<Conflict>,
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
    /// The conflicts left in the result.
    left: Vec<Conflict>,
    /// For each of `left`, the conflict of the opened file it still is.
    matched: Vec<Option<usize>>,
    /// Index into `left` of the current conflict.
    current: usize,
    _change: Subscription,
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
    let pane = |side: Side, window: &mut Window, cx: &mut Context<V>| {
        let (text, ranges) = conflict::side_text(&original, &conflicts, side);
        let editor = cx.new(|cx| {
            EditorState::new(window, cx)
                .language(language.clone())
                .line_number(true)
                .default_value(text.clone())
        });
        editor.update(cx, |e, cx| e.set_readonly(true, cx));
        let marks = editor.update(cx, |e, cx| {
            e.create_range_decorations_collection(Vec::new(), cx)
        });
        Pane {
            editor,
            text,
            ranges,
            marks,
        }
    };
    let ours = pane(Side::Ours, window, cx);
    let theirs = pane(Side::Theirs, window, cx);
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
    Loaded::Text(Box::new(TextMerge {
        has_base: conflicts.iter().any(|c| c.base.is_some()),
        labels: Labels::of(&conflicts),
        text: original.clone(),
        left: conflicts.clone(),
        matched: (0..conflicts.len()).map(Some).collect(),
        original,
        conflicts,
        on_disk: bytes,
        crlf,
        deleted,
        ours,
        theirs,
        result,
        result_marks,
        current: 0,
        _change: change,
    }))
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

/// Reads the result again: the conflicts left, which original conflict
/// each still is, and the highlights of all three editors.
fn refresh<V: Staging>(v: &mut V, cx: &mut Context<V>) {
    let Some(t) = text_mut(v) else {
        return;
    };
    t.text = t.result.read(cx).value().to_string();
    t.left = conflict::parse(&t.text);
    t.matched = conflict::match_conflicts(&t.original, &t.conflicts, &t.text, &t.left);
    t.current = t.current.min(t.left.len().saturating_sub(1));
    let fill = color(cx, T::StatusConflict).opacity(0.18);
    let done = color(cx, T::StatusSuccess).opacity(0.14);
    let frame = color(cx, T::Accent);
    let current_original = t.matched.get(t.current).copied().flatten();
    let mark = |range: &Range<usize>, tint: Hsla, current: bool| {
        let mut marks = vec![
            RangeDecoration::new(range.clone())
                .with_style(RangeDecorationStyle::Fill)
                .with_color(tint),
        ];
        if current {
            marks.push(
                RangeDecoration::new(range.clone())
                    .with_style(RangeDecorationStyle::Frame)
                    .with_color(frame),
            );
        }
        marks
    };
    let result_marks: Vec<RangeDecoration> = t
        .left
        .iter()
        .enumerate()
        .flat_map(|(i, c)| mark(&c.range, fill, i == t.current))
        .collect();
    let pane_marks = |pane: &Pane| -> Vec<RangeDecoration> {
        pane.ranges
            .iter()
            .enumerate()
            .flat_map(|(k, r)| {
                let open = t.matched.contains(&Some(k));
                mark(
                    r,
                    if open { fill } else { done },
                    current_original == Some(k),
                )
            })
            .collect()
    };
    let (ours, theirs) = (pane_marks(&t.ours), pane_marks(&t.theirs));
    t.result_marks.set(result_marks, cx);
    t.ours.marks.set(ours, cx);
    t.theirs.marks.set(theirs, cx);
}

/// Scrolls the three editors to the current conflict; the result keeps
/// the focus.
fn reveal<V: Staging>(v: &mut V, window: &mut Window, cx: &mut Context<V>) {
    let Some(t) = text_mut(v) else {
        return;
    };
    let Some(c) = t.left.get(t.current) else {
        return;
    };
    let line = |text: &str, at: usize| {
        Position::new(
            u32::try_from(conflict::line_of(text, at)).unwrap_or(u32::MAX),
            0,
        )
    };
    let mut moves = Vec::new();
    if let Some(k) = t.matched.get(t.current).copied().flatten() {
        for pane in [&t.ours, &t.theirs] {
            if let Some(r) = pane.ranges.get(k) {
                moves.push((pane.editor.clone(), line(&pane.text, r.start)));
            }
        }
    }
    moves.push((t.result.clone(), line(&t.text, c.range.start)));
    for (editor, at) in moves {
        editor.update(cx, |e, cx| e.set_cursor_position(at, window, cx));
    }
}

/// Moves to the next (`1`) or previous (`-1`) conflict left, wrapping.
fn step<V: Staging>(v: &mut V, delta: isize, window: &mut Window, cx: &mut Context<V>) {
    let Some(t) = text_mut(v) else {
        return;
    };
    let n = t.left.len();
    if n == 0 {
        return;
    }
    t.current = (t.current + n).wrapping_add_signed(delta) % n;
    refresh(v, cx);
    reveal(v, window, cx);
    cx.notify();
}

/// Replaces `range` of the result with `with`, undoably (Ctrl+Z in it).
fn replace<V: Staging>(
    v: &mut V,
    range: Range<usize>,
    with: String,
    window: &mut Window,
    cx: &mut Context<V>,
) {
    let Some(t) = text_mut(v) else {
        return;
    };
    let result = t.result.clone();
    result.update(cx, |e, cx| {
        e.set_selected_range(range, cx);
        e.replace(with, window, cx);
    });
    refresh(v, cx);
    reveal(v, window, cx);
    cx.notify();
}

/// Resolves the current conflict with `pick`; the next one becomes current.
fn take<V: Staging>(v: &mut V, pick: Pick, window: &mut Window, cx: &mut Context<V>) {
    let Some(t) = text_mut(v) else {
        return;
    };
    let Some(c) = t.left.get(t.current) else {
        return;
    };
    let (range, with) = (c.range.clone(), c.picked(&t.text, pick));
    replace(v, range, with, window, cx);
}

/// Resolves every conflict left with `pick`.
fn take_all<V: Staging>(v: &mut V, pick: Pick, window: &mut Window, cx: &mut Context<V>) {
    let Some(t) = text_mut(v) else {
        return;
    };
    let with = conflict::resolve_all(&t.text, pick);
    let range = 0..t.text.len();
    replace(v, range, with, window, cx);
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
fn resolve<V: Staging>(v: &mut V, cx: &mut Context<V>) {
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
    v.run_job(
        format!("Resolve {file}"),
        cx,
        Box::new(move |a, repo, _| {
            a.resolve_conflict(repo, &file, Some(&expected), Some(&contents))
        }),
    );
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

/// Runs the external merge tool on the file (`git mergetool`).
fn external<V: Staging>(v: &mut V, cx: &mut Context<V>) {
    let Some(file) = v.panel().merge.as_ref().map(|m| m.file.clone()) else {
        return;
    };
    let tool = v.core().settings().merge_tool.clone();
    v.run_job_as(
        OpClass::Write,
        format!("Merge tool on {file}"),
        cx,
        Box::new(move |a, repo, _| a.merge_tool(repo, Some(&file), tool.as_deref())),
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
        .on_key_down(cx.listener(|v: &mut V, e: &KeyDownEvent, window, cx| {
            let m = &e.keystroke.modifiers;
            let command = m.control || m.platform;
            match e.keystroke.key.as_str() {
                "s" if command => save(v, cx),
                "enter" if command => resolve(v, cx),
                "down" if m.alt => step(v, 1, window, cx),
                "up" if m.alt => step(v, -1, window, cx),
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

/// Title, a "conflicted" pill, the external tool and Mark resolved.
fn render_header<V: Staging>(v: &mut V, file: &str, cx: &mut Context<V>) -> Div {
    let conflict = color(cx, T::StatusConflict);
    let text = matches!(
        v.panel().merge.as_ref().and_then(|m| m.loaded.as_ref()),
        Some(Loaded::Text(_))
    );
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
        .when(text, |d| {
            d.child(
                div()
                    .flex_none()
                    .text_xs()
                    .text_color(color(cx, T::FgMuted))
                    .child(keys),
            )
        })
        .child(
            small(button("merge-external", "Open in merge tool", cx))
                .on_click(cx.listener(|v: &mut V, _, _, cx| external(v, cx))),
        )
        .when(text, |d| {
            d.child(
                small(primary_button("merge-resolve", "Mark resolved", cx))
                    .on_click(cx.listener(|v: &mut V, _, _, cx| resolve(v, cx))),
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
/// them, and the sides to take.
fn render_toolbar<V: Staging>(v: &mut V, cx: &mut Context<V>) -> Div {
    let Some(t) = text_mut(v) else {
        return div();
    };
    let (left, current, has_base) = (t.left.len(), t.current, t.has_base);
    let labels = t.labels.clone();
    let deleted = t.deleted;
    let muted = color(cx, T::FgMuted);
    let position = if left == 0 {
        "No conflict left: check the result, then Mark resolved".to_owned()
    } else {
        format!("Conflict {} of {left}", current + 1)
    };
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
        .child(div().text_color(muted).child(position))
        .when(left > 0, |d| {
            d.child(tool("merge-prev", "↑", cx, |v, w, cx| step(v, -1, w, cx)))
                .child(tool("merge-next", "↓", cx, |v, w, cx| step(v, 1, w, cx)))
                .child(div().w_2())
                .child(tool("merge-ours", "Take ours", cx, |v, w, cx| {
                    take(v, Pick::Ours, w, cx);
                }))
                .child(tool("merge-theirs", "Take theirs", cx, |v, w, cx| {
                    take(v, Pick::Theirs, w, cx);
                }))
                .child(tool("merge-both", "Ours, then theirs", cx, |v, w, cx| {
                    take(v, Pick::OursThenTheirs, w, cx);
                }))
                .child(tool(
                    "merge-both-rev",
                    "Theirs, then ours",
                    cx,
                    |v, w, cx| {
                        take(v, Pick::TheirsThenOurs, w, cx);
                    },
                ))
                .when(has_base, |d| {
                    d.child(tool("merge-base", "Take base", cx, |v, w, cx| {
                        take(v, Pick::Base, w, cx);
                    }))
                })
                .child(div().w_2())
                .child(div().text_color(muted).child("All:"))
                .child(tool("merge-all-ours", "Ours", cx, |v, w, cx| {
                    take_all(v, Pick::Ours, w, cx);
                }))
                .child(tool("merge-all-theirs", "Theirs", cx, |v, w, cx| {
                    take_all(v, Pick::Theirs, w, cx);
                }))
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

/// The three editors and the conflict toolbar.
fn render_text<V: Staging>(v: &mut V, cx: &mut Context<V>) -> AnyElement {
    let toolbar = render_toolbar(v, cx);
    let Some(t) = text_mut(v) else {
        return div().into_any_element();
    };
    let labels = t.labels.clone();
    let (ours, theirs, result) = (
        t.ours.editor.clone(),
        t.theirs.editor.clone(),
        t.result.clone(),
    );
    let pane = |id: &'static str, title: String, editor: Entity<EditorState>, cx: &App| {
        div()
            .id(id)
            .flex_1()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex_none()
                    .px_2()
                    .text_xs()
                    .text_color(color(cx, T::FgMuted))
                    .bg(color(cx, T::BgPanel))
                    .border_b_1()
                    .border_color(color(cx, T::BorderSubtle))
                    .truncate()
                    .child(title),
            )
            .child(
                div().flex_1().min_h_0().flex().flex_col().child(
                    Editor::new(&editor)
                        .bordered(false)
                        .font_family(mono_font(cx))
                        .size_full(),
                ),
            )
    };
    let sides = div()
        .flex_1()
        .min_h_0()
        .flex()
        .child(pane(
            "merge-pane-ours",
            format!("Ours · {}", labels.ours),
            ours,
            cx,
        ))
        .child(div().w_px().h_full().bg(color(cx, T::BorderSubtle)))
        .child(pane(
            "merge-pane-theirs",
            format!("Theirs · {}", labels.theirs),
            theirs,
            cx,
        ));
    let result = pane(
        "merge-pane-result",
        "Result · the file, editable".to_owned(),
        result,
        cx,
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
