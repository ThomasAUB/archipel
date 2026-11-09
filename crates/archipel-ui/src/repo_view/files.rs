//! The file browser: with **Files** on, the details pane lists every file of
//! the selected commit or stash (of the working copy for the
//! uncommitted-changes row, or when nothing is selected) as a folder tree,
//! with a fuzzy filter. A file clicked there opens read-only, highlighted,
//! in place of the graph; selecting another commit shows the same file
//! there, so a file can be followed through the history.

use std::collections::{BTreeMap, HashSet};
use std::ops::Range;
use std::sync::Arc;

use archipel_app::fuzzy;
use archipel_model::Oid;
use archipel_model::tree::{EntryKind, TreeEntry};
use archipel_theme::ColorToken as T;
use gpui_kit::component::input::{Editor, EditorState, Input, InputEvent, InputState};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, Context, Entity, FocusHandle, KeyDownEvent, MouseButton, MouseDownEvent,
    ScrollStrategy, SharedString, Subscription, UniformListScrollHandle, Window, div, px,
    uniform_list,
};

use crate::file_edit::{self, language_of};
use crate::theme::{color, mono_font, row_height};
use crate::widgets::{SHORT_OID, button, scroll_list, section, unmodified};

use super::{OpenDiff, RepoView};

/// Files above this size are not shown.
const MAX_VIEW_BYTES: usize = 4 * 1024 * 1024;
/// Filter matches listed at most.
const MAX_MATCHES: usize = 500;
/// Indentation of one tree level.
const INDENT: f32 = 12.0;

/// A folder of the tree: its sub-folders and files, by name.
#[derive(Default)]
struct Folder {
    folders: BTreeMap<String, Folder>,
    files: Vec<(String, EntryKind)>,
}

impl Folder {
    /// The folder tree of `files` (sorted by path).
    fn of(files: &[TreeEntry]) -> Self {
        let mut root = Self::default();
        for entry in files {
            let (dir, name) = match entry.path.rsplit_once('/') {
                Some((dir, name)) => (Some(dir), name),
                None => (None, entry.path.as_str()),
            };
            let mut folder = &mut root;
            for part in dir.into_iter().flat_map(|d| d.split('/')) {
                folder = folder.folders.entry(part.to_owned()).or_default();
            }
            folder.files.push((name.to_owned(), entry.kind));
        }
        root
    }

    /// The rows shown for this folder's content: folders first, then files,
    /// open folders followed by their content.
    fn push_rows(
        &self,
        prefix: &str,
        depth: usize,
        expanded: &HashSet<String>,
        out: &mut Vec<TreeRow>,
    ) {
        for (name, sub) in &self.folders {
            let path = join(prefix, name);
            let open = expanded.contains(&path);
            out.push(TreeRow {
                path: path.clone(),
                name: name.clone(),
                depth,
                kind: RowKind::Folder { open },
            });
            if open {
                sub.push_rows(&path, depth + 1, expanded, out);
            }
        }
        for (name, kind) in &self.files {
            out.push(TreeRow {
                path: join(prefix, name),
                name: name.clone(),
                depth,
                kind: RowKind::File(*kind),
            });
        }
    }
}

fn join(prefix: &str, name: &str) -> String {
    if prefix.is_empty() {
        name.to_owned()
    } else {
        format!("{prefix}/{name}")
    }
}

/// The folder holding `path`, `None` at the root.
fn parent(path: &str) -> Option<&str> {
    path.rsplit_once('/').map(|(dir, _)| dir)
}

/// What a row of the tree is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RowKind {
    Folder { open: bool },
    File(EntryKind),
}

/// One row of the tree list.
#[derive(Clone, Debug, PartialEq, Eq)]
struct TreeRow {
    /// Full path from the repository root.
    path: String,
    /// Name shown: the last path part, or the full path for a filter match.
    name: String,
    depth: usize,
    kind: RowKind,
}

/// The rows shown: the open folders of the tree, or, with a filter, the
/// files matching it, best first.
fn visible_rows(
    files: &[TreeEntry],
    root: &Folder,
    expanded: &HashSet<String>,
    filter: &str,
) -> Vec<TreeRow> {
    if filter.trim().is_empty() {
        let mut rows = Vec::new();
        root.push_rows("", 0, expanded, &mut rows);
        return rows;
    }
    fuzzy::rank(filter, files, |e| e.path.as_str())
        .into_iter()
        .take(MAX_MATCHES)
        .map(|e| TreeRow {
            path: e.path.clone(),
            name: e.path.clone(),
            depth: 0,
            kind: RowKind::File(e.kind),
        })
        .collect()
}

/// The browser's state, kept while **Files** is on.
pub(super) struct FileBrowser {
    /// Commit browsed; `None`: the working copy.
    at: Option<Oid>,
    /// Files of `at`, `None` while they load.
    files: Option<Arc<Vec<TreeEntry>>>,
    root: Arc<Folder>,
    /// Open folders, kept from one commit to the next.
    expanded: HashSet<String>,
    rows: Arc<Vec<TreeRow>>,
    /// Path of the selected row.
    selected: Option<String>,
    /// File open in place of the graph, reopened at the next commit selected.
    open: Option<String>,
    filter: Entity<InputState>,
    _filter_sub: Subscription,
    focus: FocusHandle,
    scroll: UniformListScrollHandle,
}

/// A file open in place of the graph.
pub(super) struct OpenFile {
    path: String,
    at: Option<Oid>,
    content: Content,
    /// The diff view's editor was open on it at the last render.
    editing: bool,
}

enum Content {
    Loading,
    /// Read, waiting for the next render to build its editor.
    Loaded(String),
    Shown(Entity<EditorState>),
    /// Why the file is not shown (binary, too large, unreadable).
    Note(String),
}

/// The text of `bytes` to show, CRLF turned into LF; refused for binary
/// and very large files.
fn viewable_text(bytes: &[u8]) -> Result<String, String> {
    let kb = bytes.len().div_ceil(1024);
    if bytes.len() > MAX_VIEW_BYTES {
        return Err(format!("Too large to show here ({kb} KB)"));
    }
    match std::str::from_utf8(bytes) {
        Ok(text) if !text.contains('\0') => Ok(text.replace("\r\n", "\n")),
        _ => Err(format!("Binary file ({kb} KB)")),
    }
}

impl RepoView {
    /// Turns the file browser on or off (toolbar **Files**).
    pub(super) fn toggle_browser(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.browser.take().is_some() {
            if matches!(self.open_diff, Some(OpenDiff::File(_))) {
                self.open_diff = None;
            }
            self.focus.focus(window, cx);
            cx.notify();
            return;
        }
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder("Filter files…"));
        let sub = cx.subscribe_in(
            &filter,
            window,
            |v: &mut Self, _, event: &InputEvent, window, cx| match event {
                InputEvent::Change => {
                    v.refresh_rows(cx);
                    if let Some(b) = &mut v.browser {
                        b.selected = b
                            .rows
                            .iter()
                            .find(|r| matches!(r.kind, RowKind::File(_)))
                            .map(|r| r.path.clone());
                        b.scroll.scroll_to_item(0, ScrollStrategy::Top);
                    }
                    cx.notify();
                }
                InputEvent::PressEnter { .. } => v.activate_selected(window, cx),
                _ => {}
            },
        );
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        self.browser = Some(FileBrowser {
            at: self.browse_target(),
            files: None,
            root: Arc::default(),
            expanded: HashSet::new(),
            rows: Arc::default(),
            selected: None,
            open: None,
            filter,
            _filter_sub: sub,
            focus,
            scroll: UniformListScrollHandle::new(),
        });
        self.load_tree(cx);
        cx.notify();
    }

    /// The commit the browser shows for the selection: the selected commit
    /// or stash, else the working copy.
    fn browse_target(&self) -> Option<Oid> {
        if self.wip_selected {
            None
        } else {
            self.stash_selected.or(self.selected.map(|(_, oid)| oid))
        }
    }

    /// Follows a selection change: lists the new commit's files and shows
    /// the open file there.
    pub(super) fn sync_browser(&mut self, cx: &mut Context<Self>) {
        let at = self.browse_target();
        let Some(b) = &mut self.browser else {
            return;
        };
        if b.at != at {
            b.at = at;
            b.files = None;
            self.load_tree(cx);
        }
        if let Some(path) = self.browser.as_ref().and_then(|b| b.open.clone()) {
            self.open_tree_file(path, cx);
        }
    }

    /// Lists the working copy again (it changed on disk) when it is browsed.
    pub(super) fn refresh_browser(&mut self, cx: &mut Context<Self>) {
        if self.browser.as_ref().is_some_and(|b| b.at.is_none()) {
            self.load_tree(cx);
        }
    }

    fn load_tree(&mut self, cx: &mut Context<Self>) {
        let Some(at) = self.browser.as_ref().map(|b| b.at) else {
            return;
        };
        let tree = self.core.repo_actions().ports().tree.clone();
        let path = self.path.clone();
        let task = cx.background_spawn(async move {
            let files = tree.list_files(&path, at)?;
            let root = Folder::of(&files);
            Ok::<_, archipel_model::GitError>((Arc::new(files), Arc::new(root)))
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |v, cx| {
                let Some(b) = &mut v.browser else {
                    return;
                };
                if b.at != at {
                    return; // another commit was selected meanwhile
                }
                match result {
                    Ok((files, root)) => {
                        b.files = Some(files);
                        b.root = root;
                    }
                    Err(e) => {
                        b.files = Some(Arc::default());
                        b.root = Arc::default();
                        v.status = Some(e.to_string());
                    }
                }
                v.refresh_rows(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// Recomputes the rows shown from the files, open folders and filter.
    fn refresh_rows(&mut self, cx: &gpui_kit::App) {
        let Some(b) = &mut self.browser else {
            return;
        };
        let filter = b.filter.read(cx).value().to_string();
        let files = b.files.clone().unwrap_or_default();
        b.rows = Arc::new(visible_rows(&files, &b.root, &b.expanded, &filter));
    }

    /// Opens `path` of the browsed commit (or working copy) in place of the
    /// graph, its folders opened in the tree.
    fn open_tree_file(&mut self, path: String, cx: &mut Context<Self>) {
        let Some(b) = &mut self.browser else {
            return;
        };
        let at = b.at;
        let mut dir = parent(&path);
        while let Some(d) = dir {
            b.expanded.insert(d.to_owned());
            dir = parent(d);
        }
        b.open = Some(path.clone());
        b.selected = Some(path.clone());
        let kind = b
            .files
            .as_ref()
            .and_then(|f| f.iter().find(|e| e.path == path).map(|e| e.kind));
        self.refresh_rows(cx);
        self.scroll_to_selected();
        let content = match kind {
            Some(EntryKind::Submodule) => {
                Content::Note("A submodule: open it from the refs panel".into())
            }
            _ => Content::Loading,
        };
        let load = matches!(content, Content::Loading);
        self.open_diff = Some(OpenDiff::File(OpenFile {
            path: path.clone(),
            at,
            content,
            editing: false,
        }));
        cx.notify();
        if !load {
            return;
        }
        let actions = self.core.repo_actions().clone();
        let repo = self.path.clone();
        let task = {
            let path = path.clone();
            cx.background_spawn(async move {
                actions
                    .read_file_at(&repo, at, &path)
                    .map_err(|e| e.to_string())
                    .and_then(|bytes| viewable_text(&bytes))
            })
        };
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |v, cx| {
                let Some(OpenDiff::File(open)) = &mut v.open_diff else {
                    return;
                };
                if open.path != path || open.at != at {
                    return; // another file was opened meanwhile
                }
                open.content = match result {
                    Ok(text) => Content::Loaded(text),
                    Err(e) if kind.is_none() && at.is_some() => {
                        Content::Note(format!("Not in this commit ({e})"))
                    }
                    Err(e) => Content::Note(e),
                };
                cx.notify();
            });
        })
        .detach();
    }

    /// Closes the open file (it is not reopened at the next commit).
    pub(super) fn forget_open_file(&mut self) {
        if let Some(b) = &mut self.browser {
            b.open = None;
        }
    }

    fn selected_index(&self) -> Option<usize> {
        let b = self.browser.as_ref()?;
        let selected = b.selected.as_deref()?;
        b.rows.iter().position(|r| r.path == selected)
    }

    fn scroll_to_selected(&mut self) {
        if let Some(i) = self.selected_index()
            && let Some(b) = &self.browser
        {
            b.scroll.scroll_to_item(i, ScrollStrategy::Nearest);
        }
    }

    /// Opens or closes folder `path`.
    fn toggle_folder(&mut self, path: &str, open: bool, cx: &gpui_kit::App) {
        if let Some(b) = &mut self.browser {
            if open {
                b.expanded.insert(path.to_owned());
            } else {
                b.expanded.remove(path);
            }
        }
        self.refresh_rows(cx);
    }

    /// Enter / click on the selected row: a folder opens or closes, a file
    /// opens in place of the graph.
    fn activate_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(row) = self
            .selected_index()
            .and_then(|i| self.browser.as_ref().map(|b| b.rows[i].clone()))
        else {
            return;
        };
        match row.kind {
            RowKind::Folder { open } => {
                self.toggle_folder(&row.path, !open, cx);
                cx.notify();
            }
            RowKind::File(_) => {
                self.open_tree_file(row.path, cx);
                if let Some(b) = &self.browser {
                    b.focus.focus(window, cx);
                }
            }
        }
    }

    /// Moves the tree selection by `delta` rows; while a file is open, the
    /// file selected opens in its place.
    fn step_tree(&mut self, delta: i64, cx: &mut Context<Self>) {
        let Some(b) = &mut self.browser else {
            return;
        };
        let last = i64::try_from(b.rows.len()).unwrap_or(i64::MAX) - 1;
        if last < 0 {
            return;
        }
        let at = b
            .selected
            .as_deref()
            .and_then(|s| b.rows.iter().position(|r| r.path == s))
            .map_or(-1, |i| i64::try_from(i).unwrap_or(0));
        let i = usize::try_from((at + delta).clamp(0, last)).unwrap_or(0);
        let row = b.rows[i].clone();
        b.selected = Some(row.path.clone());
        b.scroll.scroll_to_item(i, ScrollStrategy::Nearest);
        let following = matches!(self.open_diff, Some(OpenDiff::File(_)));
        if following && matches!(row.kind, RowKind::File(_)) {
            self.open_tree_file(row.path, cx);
        }
        cx.notify();
    }

    /// Opens the file `delta` files from the open one in the tree (↑↓ while
    /// the file has the keyboard).
    pub(super) fn step_open_file(&mut self, delta: i64, cx: &mut Context<Self>) {
        let Some(b) = &self.browser else {
            return;
        };
        let files: Vec<&TreeRow> = b
            .rows
            .iter()
            .filter(|r| matches!(r.kind, RowKind::File(_)))
            .collect();
        let at = b
            .open
            .as_deref()
            .and_then(|o| files.iter().position(|r| r.path == o));
        let last = i64::try_from(files.len()).unwrap_or(i64::MAX) - 1;
        if last < 0 {
            return;
        }
        let next = at
            .map_or(0, |i| i64::try_from(i).unwrap_or(0) + delta)
            .clamp(0, last);
        let path = files[usize::try_from(next).unwrap_or(0)].path.clone();
        self.open_tree_file(path, cx);
    }

    /// Keys of the tree: ↑↓ move, → opens a folder (or enters it), ← closes
    /// it (or goes to its parent), Enter opens, Esc closes the open file.
    fn on_tree_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if !unmodified(event.keystroke.modifiers) {
            return;
        }
        let row = self
            .selected_index()
            .and_then(|i| self.browser.as_ref().map(|b| b.rows[i].clone()));
        match (event.keystroke.key.as_str(), row) {
            ("down", _) => self.step_tree(1, cx),
            ("up", _) => self.step_tree(-1, cx),
            ("pagedown", _) => self.step_tree(20, cx),
            ("pageup", _) => self.step_tree(-20, cx),
            ("home", _) => self.step_tree(i64::MIN / 2, cx),
            ("end", _) => self.step_tree(i64::MAX / 2, cx),
            ("enter", _) => self.activate_selected(window, cx),
            ("right", Some(row)) => match row.kind {
                RowKind::Folder { open: false } => {
                    self.toggle_folder(&row.path, true, cx);
                    cx.notify();
                }
                RowKind::Folder { open: true } => self.step_tree(1, cx),
                RowKind::File(_) => self.activate_selected(window, cx),
            },
            ("left", Some(row)) => match (row.kind, parent(&row.path)) {
                (RowKind::Folder { open: true }, _) => {
                    self.toggle_folder(&row.path, false, cx);
                    cx.notify();
                }
                (_, Some(dir)) => {
                    if let Some(b) = &mut self.browser {
                        b.selected = Some(dir.to_owned());
                    }
                    self.scroll_to_selected();
                    cx.notify();
                }
                _ => return,
            },
            ("escape", _) if self.open_diff.is_some() => self.close_diff(window, cx),
            _ => return,
        }
        cx.stop_propagation();
    }

    /// The details pane while browsing: what is browsed, the filter and
    /// the tree.
    pub(super) fn render_browser(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let Some(b) = &self.browser else {
            return div().into_any_element();
        };
        let title = match b.at {
            Some(oid) => format!("FILES AT {}", oid.short(SHORT_OID)),
            None => "FILES IN THE WORKING COPY".to_owned(),
        };
        let count = b.files.as_ref().map(|f| f.len());
        let title = match count {
            Some(n) => format!("{title} ({n})"),
            None => title,
        };
        let key = b.at.map_or_else(|| "wc".to_owned(), |o| o.to_hex());
        let rows = b.rows.len();
        let body = match count {
            None => div()
                .px_2()
                .text_color(color(cx, T::FgMuted))
                .child("Loading files…")
                .into_any_element(),
            Some(_) if rows == 0 => div()
                .px_2()
                .text_color(color(cx, T::FgMuted))
                .child("No files")
                .into_any_element(),
            Some(_) => {
                let list = uniform_list(
                    SharedString::from(format!("tree-{key}")),
                    rows,
                    cx.processor(|this: &mut Self, range: Range<usize>, _window, cx| {
                        let Some(b) = &this.browser else {
                            return Vec::new();
                        };
                        let rows = b.rows.clone();
                        let selected = b.selected.clone();
                        let open = match &this.open_diff {
                            Some(OpenDiff::File(f)) => Some(f.path.clone()),
                            _ => None,
                        };
                        range
                            .filter_map(|i| rows.get(i))
                            .map(|row| {
                                let is_selected = selected.as_deref() == Some(row.path.as_str());
                                let is_open = open.as_deref() == Some(row.path.as_str());
                                render_tree_row(row, is_selected, is_open, cx)
                            })
                            .collect()
                    }),
                );
                scroll_list(SharedString::from(format!("tree-scroll-{key}")), list)
                    .handle(&b.scroll)
                    .into_any_element()
            }
        };
        div()
            .id("file-browser")
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .overflow_hidden()
            .track_focus(&b.focus)
            .on_key_down(cx.listener(|v, e: &KeyDownEvent, window, cx| {
                v.on_tree_key(e, window, cx);
            }))
            .child(section(title, cx))
            .child(div().px_2().pb_1().child(Input::new(&b.filter)))
            .child(body)
            .into_any_element()
    }

    /// The open file, in place of the graph: its path, where it is read
    /// from, then its text, read-only. A working-copy file goes to the
    /// diff view's editor on **Edit** or a double-click, at that line; once
    /// saved or cancelled, the file shows again, reloaded.
    pub(super) fn render_open_file(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(OpenDiff::File(open)) = &mut self.open_diff else {
            return div().into_any_element();
        };
        if let Content::Loaded(text) = &open.content {
            let text = text.clone();
            let language = language_of(&open.path);
            let editor = cx.new(|cx| {
                EditorState::new(window, cx)
                    .language(language)
                    .line_number(true)
                    .searchable(true)
                    .default_value(text)
            });
            open.content = Content::Shown(editor);
        }
        let (path, at, was_editing) = (open.path.clone(), open.at, open.editing);
        let viewer = match &open.content {
            Content::Shown(editor) => Some(editor.clone()),
            _ => None,
        };
        let note = match &open.content {
            Content::Note(note) => Some(note.clone()),
            _ => None,
        };
        let editable = at.is_none()
            && (viewer.is_some() || was_editing)
            && self.browser.as_ref().is_some_and(|b| {
                b.files.as_ref().is_some_and(|f| {
                    f.iter().any(|e| {
                        e.path == path && matches!(e.kind, EntryKind::File | EntryKind::Executable)
                    })
                })
            });
        let editing = editable && file_edit::current(self, Some(&path), cx);
        if let Some(OpenDiff::File(open)) = &mut self.open_diff {
            open.editing = editing;
        }
        if was_editing && !editing {
            // Saved or cancelled: show the file as it is on disk now.
            self.open_tree_file(path, cx);
            return self.render_open_file(window, cx);
        }
        let place = match at {
            Some(oid) => format!("at {}", oid.short(SHORT_OID)),
            None => "working copy".to_owned(),
        };
        let keys = if editable {
            "double-click edits · ↑↓ files · Esc closes"
        } else {
            "read-only · ↑↓ files · Esc closes"
        };
        let bar = if editing {
            file_edit::render_bar(self, cx)
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
            .child(div().flex_1().min_w_0().truncate().child(path.clone()))
            .when(!editing, |header| {
                header.child(
                    div()
                        .flex_none()
                        .text_xs()
                        .text_color(color(cx, T::FgMuted))
                        .child(format!("{place} · {keys}")),
                )
            })
            .when(editable && !editing, |header| {
                header.child(button("file-edit", "Edit", cx).on_click(
                    cx.listener(|v: &mut Self, _, window, cx| v.edit_open_file(window, cx)),
                ))
            })
            .children(bar);
        let body = if editing {
            file_edit::render_body(self, cx)
        } else if let Some(editor) = viewer {
            Self::render_viewer(&editor, editable, cx)
        } else if let Some(note) = note {
            muted(note, cx)
        } else {
            muted("Loading file…", cx)
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

    /// The read-only view of a file; a double-click on an editable one opens
    /// the editor at that line.
    fn render_viewer(
        editor: &Entity<EditorState>,
        editable: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .id("file-view")
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .when(editable, |d| {
                d.capture_any_mouse_down(cx.listener(|_, e: &MouseDownEvent, window, cx| {
                    if e.button == MouseButton::Left && e.click_count == 2 {
                        // Once the editor moved its cursor there.
                        cx.defer_in(window, |v: &mut Self, window, cx| {
                            v.edit_open_file(window, cx);
                        });
                    }
                }))
            })
            .child(
                // The element sets the state's read-only mode on every render.
                Editor::new(editor)
                    .readonly(true)
                    .bordered(false)
                    .font_family(mono_font(cx))
                    .size_full(),
            )
            .into_any_element()
    }

    /// Opens the diff view's editor on the open working-copy file, at the
    /// viewer's cursor line.
    fn edit_open_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(OpenDiff::File(open)) = &self.open_diff else {
            return;
        };
        if open.at.is_some() {
            return;
        }
        let line = match &open.content {
            Content::Shown(editor) => editor.read(cx).cursor_position().line + 1,
            _ => 1,
        };
        let path = open.path.clone();
        file_edit::open(self, path, line, window, cx);
    }
}

fn muted(text: impl Into<SharedString>, cx: &Context<RepoView>) -> AnyElement {
    div()
        .p_3()
        .text_color(color(cx, T::FgMuted))
        .child(text.into())
        .into_any_element()
}

/// One row of the tree: indented by depth, a chevron for folders; a click
/// selects it, and opens a file or opens / closes a folder.
fn render_tree_row(
    row: &TreeRow,
    selected: bool,
    open: bool,
    cx: &mut Context<RepoView>,
) -> AnyElement {
    let hover = color(cx, T::BgHover);
    let (glyph, note) = match row.kind {
        RowKind::Folder { open: true } => ("▾", None),
        RowKind::Folder { open: false } => ("▸", None),
        RowKind::File(EntryKind::Symlink) => ("", Some("link")),
        RowKind::File(EntryKind::Submodule) => ("", Some("submodule")),
        RowKind::File(EntryKind::Executable) => ("", Some("exec")),
        RowKind::File(EntryKind::File) => ("", None),
    };
    let path = row.path.clone();
    let kind = row.kind;
    let depth = f32::from(u16::try_from(row.depth).unwrap_or(u16::MAX));
    div()
        .id(SharedString::from(format!("tree-{}", row.path)))
        .w_full()
        .h(row_height(cx))
        .flex()
        .items_center()
        .gap_1()
        .pl(px(8.0 + depth * INDENT))
        .pr_2()
        .cursor_pointer()
        .when(open, |d| d.bg(color(cx, T::BgSelected)))
        .when(selected && !open, |d| d.bg(color(cx, T::BgHover)))
        .hover(move |s| s.bg(hover))
        .on_click(cx.listener(move |v, _, window, cx| {
            if let Some(b) = &mut v.browser {
                b.selected = Some(path.clone());
                b.focus.focus(window, cx);
            }
            match kind {
                RowKind::Folder { open } => v.toggle_folder(&path, !open, cx),
                RowKind::File(_) => v.open_tree_file(path.clone(), cx),
            }
            cx.notify();
        }))
        .child(
            div()
                .w(px(10.0))
                .flex_none()
                .text_color(color(cx, T::FgMuted))
                .child(glyph),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .when(matches!(kind, RowKind::Folder { .. }), |d| {
                    d.text_color(color(cx, T::FgMuted))
                })
                .child(row.name.clone()),
        )
        .children(note.map(|n| {
            div()
                .flex_none()
                .text_xs()
                .text_color(color(cx, T::FgSubtle))
                .child(n)
        }))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(paths: &[&str]) -> Vec<TreeEntry> {
        paths.iter().map(|p| TreeEntry::file(*p)).collect()
    }

    fn shown(rows: &[TreeRow]) -> Vec<String> {
        rows.iter()
            .map(|r| {
                let mark = match r.kind {
                    RowKind::Folder { open: true } => "v ",
                    RowKind::Folder { open: false } => "> ",
                    RowKind::File(_) => "",
                };
                format!("{}{mark}{}", "  ".repeat(r.depth), r.name)
            })
            .collect()
    }

    #[test]
    fn folders_come_first_and_open_ones_show_their_content() {
        let files = files(&["README.md", "src/lib.rs", "src/ui/view.rs", "docs/a.md"]);
        let root = Folder::of(&files);
        let rows = visible_rows(&files, &root, &HashSet::new(), "");
        assert_eq!(shown(&rows), ["> docs", "> src", "README.md"]);
        let expanded: HashSet<String> = ["src".to_owned(), "src/ui".to_owned()].into();
        let rows = visible_rows(&files, &root, &expanded, "");
        assert_eq!(
            shown(&rows),
            [
                "> docs",
                "v src",
                "  v ui",
                "    view.rs",
                "  lib.rs",
                "README.md"
            ]
        );
        assert_eq!(rows[3].path, "src/ui/view.rs");
    }

    #[test]
    fn a_filter_lists_matching_files_flat() {
        let files = files(&["src/lib.rs", "src/ui/view.rs", "docs/view.md"]);
        let root = Folder::of(&files);
        let rows = visible_rows(&files, &root, &HashSet::new(), "view");
        let mut matched = shown(&rows);
        matched.sort();
        assert_eq!(matched, ["docs/view.md", "src/ui/view.rs"]);
        assert!(rows.iter().all(|r| r.depth == 0 && r.name == r.path));
    }

    #[test]
    fn binary_and_huge_files_are_not_shown() {
        assert_eq!(viewable_text(b"a\r\nb\n").unwrap(), "a\nb\n");
        assert!(viewable_text(&[0xff, 0xfe]).is_err());
        assert!(viewable_text(b"a\0b").is_err());
        assert!(viewable_text(&vec![b'a'; MAX_VIEW_BYTES + 1]).is_err());
    }

    #[test]
    fn parent_folders() {
        assert_eq!(parent("a/b/c.rs"), Some("a/b"));
        assert_eq!(parent("c.rs"), None);
    }
}
