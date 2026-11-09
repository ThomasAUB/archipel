//! Quick edit of a working-copy file from its open diff: the diff gives way
//! to a code editor on the file, opened at the line of the change; Ctrl+S
//! saves and goes back to the (reloaded) diff, Esc goes back without
//! saving (twice when there are unsaved changes). A save is refused when
//! the file changed on disk since it was opened, so nothing is lost; the
//! editor then stays open.

use std::sync::{Arc, Mutex, PoisonError};

use archipel_theme::ColorToken as T;
use gpui_kit::component::input::{Editor, EditorState, Position};
use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, Context, Entity, KeyDownEvent, SharedString, Window, div};

use crate::staging::Staging;
use crate::theme::{color, mono_font};
use crate::widgets::{button, primary_button, unmodified};

/// Files above this size are not opened in the editor.
const MAX_EDIT_BYTES: usize = 4 * 1024 * 1024;

/// The file being edited, kept in the staging panel's state.
pub(crate) struct FileEdit {
    /// Path relative to the repo root (the open diff's file).
    file: String,
    /// The editor, `None` while the file loads.
    editor: Option<Entity<EditorState>>,
    /// The bytes the editor was opened on: a save is refused when the
    /// file no longer holds them.
    original: Vec<u8>,
    /// The file's lines end with CRLF: the editor works on LF, the save
    /// puts CRLF back.
    crlf: bool,
    /// Esc was pressed once with unsaved changes.
    armed_close: bool,
    /// The bytes written by the last successful save, set by the save job.
    saved: Arc<Mutex<Option<Vec<u8>>>>,
}

impl FileEdit {
    /// The file edited.
    pub(crate) fn file(&self) -> &str {
        &self.file
    }
}

/// Opens `file` in the editor at `line` (1-based, in the working copy).
pub(crate) fn open<V: Staging>(
    v: &mut V,
    file: String,
    line: u32,
    window: &mut Window,
    cx: &mut Context<V>,
) {
    v.panel().edit = Some(FileEdit {
        file: file.clone(),
        editor: None,
        original: Vec::new(),
        crlf: false,
        armed_close: false,
        saved: Arc::default(),
    });
    let actions = v.core().repo_actions().clone();
    let repo = v.repo_path().clone();
    let task = {
        let file = file.clone();
        cx.background_spawn(async move { actions.read_file(&repo, &file) })
    };
    cx.spawn_in(window, async move |this, cx| {
        let result = task.await;
        let _ = this.update_in(cx, |v: &mut V, window, cx| {
            if v.panel().edit.as_ref().map(FileEdit::file) != Some(file.as_str()) {
                return; // closed or another file opened meanwhile
            }
            let loaded = result
                .map_err(|e| e.to_string())
                .and_then(|bytes| editable_text(&file, bytes));
            match loaded {
                Ok((bytes, text, crlf)) => {
                    let language = language_of(&file);
                    let editor = cx.new(|cx| {
                        EditorState::new(window, cx)
                            .language(language)
                            .line_number(true)
                            .default_value(text)
                    });
                    editor.update(cx, |e, cx| {
                        e.set_cursor_position(Position::new(line.saturating_sub(1), 0), window, cx);
                    });
                    if let Some(edit) = &mut v.panel().edit {
                        edit.editor = Some(editor);
                        edit.original = bytes;
                        edit.crlf = crlf;
                    }
                }
                Err(e) => {
                    v.panel().edit = None;
                    v.report(e, cx);
                }
            }
            cx.notify();
        });
    })
    .detach();
    cx.notify();
}

/// The text of `bytes` for the editor, with whether its lines end with
/// CRLF; refused for files the editor would mangle.
fn editable_text(file: &str, bytes: Vec<u8>) -> Result<(Vec<u8>, String, bool), String> {
    if bytes.len() > MAX_EDIT_BYTES {
        return Err(format!("{file} is too large to edit here"));
    }
    let Ok(text) = std::str::from_utf8(&bytes) else {
        return Err(format!(
            "{file} is not UTF-8 text: it cannot be edited here"
        ));
    };
    let lf = text.matches('\n').count();
    let crlf_count = text.matches("\r\n").count();
    // Mixed line endings are kept as they are.
    let crlf = crlf_count > 0 && crlf_count == lf;
    let text = if crlf {
        text.replace("\r\n", "\n")
    } else {
        text.to_owned()
    };
    Ok((bytes, text, crlf))
}

/// The bytes to write for the editor's `text`.
fn file_bytes(text: &str, crlf: bool) -> Vec<u8> {
    if crlf {
        text.replace('\n', "\r\n").into_bytes()
    } else {
        text.as_bytes().to_vec()
    }
}

/// The highlighting language named after the file's extension (or its
/// name, for `Makefile`, `Dockerfile`…).
pub(crate) fn language_of(file: &str) -> String {
    let name = file.rsplit('/').next().unwrap_or(file);
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => ext.to_lowercase(),
        _ => name.to_lowercase(),
    }
}

/// Whether the editor holds changes not saved yet.
fn is_dirty(edit: &FileEdit, cx: &gpui_kit::App) -> bool {
    edit.editor
        .as_ref()
        .is_some_and(|e| file_bytes(&e.read(cx).value(), edit.crlf) != edit.original)
}

/// Writes the editor's text over the file, refused when the file changed
/// meanwhile. The editor closes once the save is done.
pub(crate) fn save<V: Staging>(v: &mut V, cx: &mut Context<V>) {
    let Some(edit) = &mut v.panel().edit else {
        return;
    };
    let Some(editor) = &edit.editor else {
        return;
    };
    let contents = file_bytes(&editor.read(cx).value(), edit.crlf);
    edit.armed_close = false;
    let (file, original, saved) = (edit.file.clone(), edit.original.clone(), edit.saved.clone());
    v.run_job(
        format!("Save {file}"),
        cx,
        Box::new(move |a, repo, _| {
            a.save_file(repo, &file, &original, &contents)?;
            *saved.lock().unwrap_or_else(PoisonError::into_inner) = Some(contents);
            Ok(())
        }),
    );
}

/// Esc in the editor: back to the diff, unless there are unsaved changes
/// and this is the first Esc.
fn escape<V: Staging>(v: &mut V, cx: &mut Context<V>) {
    let close = match &v.panel().edit {
        Some(edit) => edit.armed_close || !is_dirty(edit, cx),
        None => return,
    };
    if close {
        v.panel().edit = None;
    } else if let Some(edit) = &mut v.panel().edit {
        edit.armed_close = true;
    }
    cx.notify();
}

/// Closes the edit when its save went through: the diff shows again,
/// reloaded. Text typed after the save keeps the editor open, on the new
/// contents.
fn settle_save<V: Staging>(v: &mut V, cx: &gpui_kit::App) {
    let Some(edit) = &mut v.panel().edit else {
        return;
    };
    let Some(saved) = edit
        .saved
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .take()
    else {
        return;
    };
    edit.original = saved;
    if !is_dirty(edit, cx) {
        v.panel().edit = None;
    }
}

/// The edit of `file`, when one is open: drops an edit of another file
/// (another diff was opened).
pub(crate) fn current<V: Staging>(v: &mut V, file: Option<&str>, cx: &gpui_kit::App) -> bool {
    settle_save(v, cx);
    let panel = v.panel();
    match (&panel.edit, file) {
        (Some(edit), Some(file)) if edit.file == file => true,
        (Some(_), _) => {
            panel.edit = None;
            false
        }
        (None, _) => false,
    }
}

/// The header's buttons and note while editing.
pub(crate) fn render_bar<V: Staging>(v: &mut V, cx: &mut Context<V>) -> Option<AnyElement> {
    let edit = v.panel().edit.as_ref()?;
    let note = if edit.armed_close {
        "Unsaved changes: Esc again discards them"
    } else if is_dirty(edit, cx) {
        "Modified · Ctrl+S saves · Esc back to the diff"
    } else {
        "Ctrl+S saves · Esc back to the diff"
    };
    Some(
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
                        if edit.armed_close {
                            T::StatusWarning
                        } else {
                            T::FgMuted
                        },
                    ))
                    .child(note),
            )
            .child(
                primary_button("edit-save", "Save", cx)
                    .on_click(cx.listener(|v: &mut V, _, _, cx| save(v, cx))),
            )
            .child(button("edit-cancel", "Cancel", cx).on_click(cx.listener(
                |v: &mut V, _, _, cx| {
                    v.panel().edit = None;
                    cx.notify();
                },
            )))
            .into_any_element(),
    )
}

/// The editor in place of the diff; Ctrl+S and Esc are caught around it
/// (the editor's own keys, arrows included, go first).
pub(crate) fn render_body<V: Staging>(v: &mut V, cx: &mut Context<V>) -> AnyElement {
    let editor = v.panel().edit.as_ref().and_then(|e| e.editor.clone());
    let body = div()
        .id(SharedString::from("file-edit"))
        .flex_1()
        .min_h_0()
        .flex()
        .flex_col()
        .on_key_down(cx.listener(|v: &mut V, e: &KeyDownEvent, _, cx| {
            let m = &e.keystroke.modifiers;
            match e.keystroke.key.as_str() {
                "s" if m.control || m.platform => save(v, cx),
                "escape" if unmodified(*m) => escape(v, cx),
                _ => return,
            }
            cx.stop_propagation();
        }));
    match editor {
        Some(editor) => body
            .child(
                Editor::new(&editor)
                    .bordered(false)
                    .font_family(mono_font(cx))
                    .size_full(),
            )
            .into_any_element(),
        None => body
            .p_3()
            .text_color(color(cx, T::FgMuted))
            .child("Loading file…")
            .into_any_element(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_endings_survive_a_round_trip() {
        let (_, text, crlf) = editable_text("a", b"x\r\ny\r\n".to_vec()).unwrap();
        assert!(crlf);
        assert_eq!(text, "x\ny\n");
        assert_eq!(file_bytes(&text, crlf), b"x\r\ny\r\n");

        let (_, text, crlf) = editable_text("a", b"x\ny".to_vec()).unwrap();
        assert!(!crlf);
        assert_eq!(file_bytes(&text, crlf), b"x\ny");

        // Mixed endings stay as they are.
        let (_, text, crlf) = editable_text("a", b"x\r\ny\n".to_vec()).unwrap();
        assert!(!crlf);
        assert_eq!(file_bytes(&text, crlf), b"x\r\ny\n");
    }

    #[test]
    fn binary_and_huge_files_are_refused() {
        assert!(editable_text("a", vec![0xff, 0xfe, 0x00]).is_err());
        assert!(editable_text("a", vec![b'a'; MAX_EDIT_BYTES + 1]).is_err());
    }

    #[test]
    fn language_follows_the_extension() {
        assert_eq!(language_of("src/main.rs"), "rs");
        assert_eq!(language_of("web/App.TSX"), "tsx");
        assert_eq!(language_of("Makefile"), "makefile");
        assert_eq!(language_of("conf/.gitignore"), ".gitignore");
    }
}
