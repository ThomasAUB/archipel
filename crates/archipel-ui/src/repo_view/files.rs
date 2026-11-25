//! The file browser: with **Files** on, the details pane lists every file of
//! the selected commit or stash (of the working copy for the
//! uncommitted-changes row, or when nothing is selected) as a folder tree,
//! with a fuzzy filter. A file clicked there opens read-only, highlighted,
//! in place of the graph; selecting another commit shows the same file
//! there, so a file can be followed through the history. An image file
//! shows as its image (an SVG can show its source instead), a Markdown file
//! rendered (or its source).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::ops::Range;
use std::sync::Arc;

use archipel_app::fuzzy;
use archipel_model::file_history::BlameOptions;
use archipel_model::tree::{EntryKind, TreeEntry};
use archipel_model::{CancelToken, GitError, Oid};
use archipel_theme::ColorToken as T;
use gpui_kit::component::input::{Editor, EditorState, Input, InputEvent, InputState};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, App, Context, Div, Entity, FocusHandle, Image, ImageFormat, KeyDownEvent,
    MouseButton, MouseDownEvent, ScrollStrategy, SharedString, Subscription, SvgRenderer,
    UniformListScrollHandle, Window, div, px, uniform_list,
};

use crate::diff_view::{self, DiffDoc};
use crate::file_edit::{self, language_of};
use crate::image_view::viewer::ImageViewer;
use crate::image_view::{self, Picture, image_format};
use crate::markdown_view::{self, Link, MarkdownDoc};
use crate::theme::{color, mono_font, row_height};
use crate::widgets::{
    ListMove, SHORT_OID, TextTooltip, button, list_row, scroll_list, section, unmodified,
};

use super::file_history::{self, BlameDoc, FileHistory, FileMode};
use super::{OpenDiff, RepoView};

/// Files above this size are not shown.
const MAX_VIEW_BYTES: usize = 4 * 1024 * 1024;
/// Filter matches listed at most.
const MAX_MATCHES: usize = 500;
/// Indentation of one tree level.
const INDENT: f32 = 12.0;
/// Width the open file's path keeps in its header.
const PATH_MIN_WIDTH: f32 = 160.0;

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
    pub(super) at: Option<Oid>,
    /// Files of `at`, `None` while they load.
    files: Option<Arc<Vec<TreeEntry>>>,
    root: Arc<Folder>,
    /// Open folders, kept from one commit to the next.
    expanded: HashSet<String>,
    rows: Arc<Vec<TreeRow>>,
    /// Path of the selected row.
    selected: Option<String>,
    /// File open in place of the graph, reopened at the next commit selected.
    pub(super) open: Option<String>,
    /// SVG files show their source, not their image.
    svg_source: bool,
    /// Markdown files show their source, not rendered.
    md_source: bool,
    /// What the open file shows, kept from one commit to the next.
    pub(super) mode: FileMode,
    /// How the blame matches lines to commits.
    blame_options: BlameOptions,
    /// The open file's history, shown in place of the tree.
    pub(super) history: Option<FileHistory>,
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
    /// The heading to scroll to once shown (a link led here).
    anchor: Option<String>,
    /// Cancels its load once another file, commit or mode is shown; also
    /// tells its load's result from an older one's.
    cancel: CancelToken,
}

impl Drop for OpenFile {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

enum Content {
    Loading,
    /// Read, waiting for the next render to build its editor.
    Loaded(String),
    Shown(Entity<EditorState>),
    /// An image file, decoded.
    Image(Entity<ImageViewer>),
    /// A Markdown file, rendered.
    Markdown(Box<MarkdownDoc>),
    /// The selected commit's change to the file.
    Change(Arc<DiffDoc>),
    /// Who last changed each line.
    Blame(Arc<BlameDoc>),
    /// Why the file is not shown (binary, too large, unreadable).
    Note(String),
}

/// What a file shows once read.
enum Readable {
    Text(String),
    Image(Picture),
    /// A Markdown file's text, and the images it shows by their source.
    Markdown(String, HashMap<String, Arc<Image>>),
}

/// What to show of `bytes` of `path`: an image file its image (an SVG
/// its source with `svg_source`), any other file, or an image that does
/// not decode, its text.
fn readable(
    path: &str,
    bytes: &[u8],
    svg: &SvgRenderer,
    svg_source: bool,
) -> Result<Readable, String> {
    let format = image_format(path).filter(|&f| !(svg_source && f == ImageFormat::Svg));
    if let Some(picture) = format.and_then(|f| image_view::decode(f, bytes, svg).ok()) {
        return Ok(Readable::Image(picture));
    }
    viewable_text(bytes).map(Readable::Text)
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
            svg_source: false,
            md_source: false,
            mode: FileMode::default(),
            blame_options: BlameOptions::default(),
            history: None,
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
            self.stash_selected.or(self.selected.map(|s| s.oid))
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
    pub(super) fn open_tree_file(&mut self, path: String, cx: &mut Context<Self>) {
        let Some(b) = &mut self.browser else {
            return;
        };
        let (at, sources, mode) = (b.at, (b.svg_source, b.md_source), b.mode);
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
        if let Some(OpenDiff::File(old)) = &self.open_diff {
            old.cancel.cancel();
        }
        let cancel = CancelToken::new();
        self.open_diff = Some(OpenDiff::File(OpenFile {
            path: path.clone(),
            at,
            content,
            editing: false,
            anchor: None,
            cancel: cancel.clone(),
        }));
        cx.notify();
        if !load {
            return;
        }
        // Read at a commit whose tree does not list it (listed or not yet).
        let absent = kind.is_none() && at.is_some();
        match mode {
            FileMode::Content => self.load_content(path, at, sources, absent, cancel, cx),
            FileMode::Change => self.load_change(path, at, cancel, cx),
            FileMode::Blame => self.load_blame(path, at, absent, cancel, cx),
        }
    }

    /// Shows `content` in the open file, unless another load replaced the
    /// one `cancel` belongs to.
    fn land(&mut self, cancel: &CancelToken, content: Content, cx: &mut Context<Self>) {
        let Some(OpenDiff::File(open)) = &mut self.open_diff else {
            return;
        };
        if open.cancel.same_as(cancel) {
            open.content = content;
            cx.notify();
        }
    }

    /// Reads the open file's content: a Markdown file (unless its source
    /// is asked for) with the images it shows, from the same commit.
    fn load_content(
        &mut self,
        path: String,
        at: Option<Oid>,
        (svg_source, md_source): (bool, bool),
        absent: bool,
        cancel: CancelToken,
        cx: &mut Context<Self>,
    ) {
        let actions = self.core.repo_actions().clone();
        let repo = self.path.clone();
        let svg = cx.svg_renderer();
        let task = cx.background_spawn(async move {
            let bytes = actions
                .read_file_at(&repo, at, &path)
                .map_err(|e| e.to_string())?;
            match readable(&path, &bytes, &svg, svg_source)? {
                Readable::Text(text) if !md_source && markdown_view::is_markdown(&path) => {
                    let images = markdown_view::load_images(&path, &text, |image| {
                        actions.read_file_at(&repo, at, image).ok()
                    });
                    Ok(Readable::Markdown(text, images))
                }
                shown => Ok(shown),
            }
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |v, cx| {
                let content = match result {
                    Ok(Readable::Text(text)) => Content::Loaded(text),
                    Ok(Readable::Image(picture)) => {
                        Content::Image(cx.new(|cx| ImageViewer::new(picture, cx)))
                    }
                    Ok(Readable::Markdown(text, images)) => {
                        Content::Markdown(Box::new(MarkdownDoc::new(text, images, cx)))
                    }
                    Err(e) if absent => Content::Note(format!("Not in this commit ({e})")),
                    Err(e) => Content::Note(e),
                };
                v.land(&cancel, content, cx);
            });
        })
        .detach();
    }

    /// Reads what the browsed commit changed in the open file: from the
    /// selected commit's diff when it is listed already.
    fn load_change(
        &mut self,
        path: String,
        at: Option<Oid>,
        cancel: CancelToken,
        cx: &mut Context<Self>,
    ) {
        let Some(oid) = at else {
            let note = "The working copy: select a commit to see its change to this file";
            self.land(&cancel, Content::Note(note.into()), cx);
            return;
        };
        let unchanged = move || Content::Note(format!("Not changed in {}", oid.short(SHORT_OID)));
        if let Some((listed, doc)) = &self.details
            && *listed == oid
        {
            let content = file_history::change_of(&doc.files, &path).map_or_else(unchanged, |f| {
                Content::Change(file_history::change_doc(f, oid))
            });
            self.land(&cancel, content, cx);
            return;
        }
        let diffs = self.core.diff_reader();
        let repo = self.path.clone();
        let task = cx.background_spawn(async move { diffs.commit_diff(&repo, oid) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |v, cx| {
                let content = match result {
                    Ok(files) => file_history::change_of(&files, &path)
                        .map_or_else(unchanged, |f| {
                            Content::Change(file_history::change_doc(f, oid))
                        }),
                    Err(e) => Content::Note(e.to_string()),
                };
                v.land(&cancel, content, cx);
            });
        })
        .detach();
    }

    /// Reads who last changed each line of the open file.
    fn load_blame(
        &mut self,
        path: String,
        at: Option<Oid>,
        absent: bool,
        cancel: CancelToken,
        cx: &mut Context<Self>,
    ) {
        let Some(options) = self.browser.as_ref().map(|b| b.blame_options) else {
            return;
        };
        let reader = self.core.repo_actions().ports().file_history.clone();
        let repo = self.path.clone();
        let task = {
            let cancel = cancel.clone();
            cx.background_spawn(async move {
                reader
                    .blame(&repo, &path, at, options, &cancel)
                    .map(|blame| BlameDoc::new(blame, MAX_VIEW_BYTES))
            })
        };
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |v, cx| {
                let content = match result {
                    Ok(Ok(doc)) => Content::Blame(Arc::new(doc)),
                    Ok(Err(note)) => Content::Note(note),
                    Err(GitError::Cancelled) => return,
                    Err(e) if absent => Content::Note(format!("Not in this commit ({e})")),
                    Err(e) => Content::Note(e.to_string()),
                };
                v.land(&cancel, content, cx);
            });
        })
        .detach();
    }

    /// Shows the open file as `mode` from now on.
    fn set_file_mode(&mut self, mode: FileMode, cx: &mut Context<Self>) {
        let Some(b) = &mut self.browser else {
            return;
        };
        b.mode = mode;
        if let Some(path) = b.open.clone() {
            self.open_tree_file(path, cx);
        }
    }

    /// Flips a blame option, and blames the open file again.
    fn toggle_blame_option(&mut self, flip: fn(&mut BlameOptions), cx: &mut Context<Self>) {
        let Some(b) = &mut self.browser else {
            return;
        };
        flip(&mut b.blame_options);
        if let Some(path) = b.open.clone() {
            self.open_tree_file(path, cx);
        }
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
    fn step_tree(&mut self, to: ListMove, cx: &mut Context<Self>) {
        let Some(b) = &mut self.browser else {
            return;
        };
        let at = b
            .selected
            .as_deref()
            .and_then(|s| b.rows.iter().position(|r| r.path == s));
        let Some(i) = to.apply(at, b.rows.len()) else {
            return;
        };
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
        if let Some(to) = ListMove::from_key(&event.keystroke.key) {
            self.step_tree(to, cx);
            cx.stop_propagation();
            return;
        }
        match (event.keystroke.key.as_str(), row) {
            ("enter", _) => self.activate_selected(window, cx),
            ("right", Some(row)) => match row.kind {
                RowKind::Folder { open: false } => {
                    self.toggle_folder(&row.path, true, cx);
                    cx.notify();
                }
                RowKind::Folder { open: true } => self.step_tree(ListMove::By(1), cx),
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
        if self.history_shown() {
            return self.render_history(cx);
        }
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
        let rendered = prepare_markdown(open, cx);
        let (path, at, was_editing) = (open.path.clone(), open.at, open.editing);
        let viewer = match &open.content {
            Content::Shown(editor) => Some(editor.clone()),
            _ => None,
        };
        let (mode, options, history) = self
            .browser
            .as_ref()
            .map(|b| (b.mode, b.blame_options, b.history.is_some()))
            .unwrap_or_default();
        // At a commit, Edit goes to the working-copy file (see
        // `edit_open_file`).
        let editable = (viewer.is_some() || rendered || was_editing)
            && mode == FileMode::Content
            && self.browser.as_ref().is_some_and(|b| {
                b.files.as_ref().is_some_and(|f| {
                    f.iter().any(|e| {
                        e.path == path && matches!(e.kind, EntryKind::File | EntryKind::Executable)
                    })
                })
            });
        let editing = editable && at.is_none() && file_edit::current(self, Some(&path), cx);
        if let Some(OpenDiff::File(open)) = &mut self.open_diff {
            open.editing = editing;
        }
        if was_editing && !editing {
            // Saved or cancelled: show the file as it is on disk now.
            self.open_tree_file(path, cx);
            return self.render_open_file(window, cx);
        }
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
            .child(path_title(path.clone()))
            // The mode buttons last, at the right edge, and what depends on
            // the mode (the hint, the blame options, the toggles, Edit)
            // before them, where coming and going it only gives way to the
            // title: a mode button stays under the pointer once clicked.
            .when(!editing, |header| {
                header.child(hint(file_note(at, editable && !rendered, mode), cx))
            })
            .when(!editing && mode == FileMode::Blame, |header| {
                header.child(blame_option_buttons(options, cx))
            })
            .when(
                !editing
                    && mode == FileMode::Content
                    && image_format(&path) == Some(ImageFormat::Svg),
                |header| header.child(self.svg_toggle(path.clone(), cx)),
            )
            .when(
                !editing && mode == FileMode::Content && markdown_view::is_markdown(&path),
                |header| header.child(self.markdown_toggle(path.clone(), cx)),
            )
            .when(editable && !editing, |header| {
                header.child(button("file-edit", "Edit", cx).on_click(
                    cx.listener(|v: &mut Self, _, window, cx| v.edit_open_file(window, cx)),
                ))
            })
            .when(!editing, |header| {
                header.child(Self::mode_buttons(mode, history, cx))
            })
            .children(bar);
        let body = if editing {
            file_edit::render_body(self, cx)
        } else {
            self.render_file_body(editable, cx)
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

    /// The open file's body, out of the editor: its text, image, change or
    /// blame, or why it is not shown.
    fn render_file_body(&self, editable: bool, cx: &mut Context<Self>) -> AnyElement {
        let Some(OpenDiff::File(open)) = &self.open_diff else {
            return div().into_any_element();
        };
        let at = open.at.map_or_else(|| "wc".to_owned(), |o| o.to_hex());
        let key = format!("{}@{at}", open.path);
        match &open.content {
            Content::Change(doc) => diff_view::diff_list(
                SharedString::from(format!("file-change-{key}")),
                doc.clone(),
                None,
                None,
                cx,
            ),
            Content::Blame(doc) => file_history::render_blame(doc, &key, cx),
            Content::Shown(editor) => Self::render_viewer(editor, editable, cx),
            Content::Image(image) => image.clone().into_any_element(),
            Content::Markdown(doc) => {
                let view = cx.entity().downgrade();
                doc.render(move |href, window, cx| {
                    let href = href.to_string();
                    let _ = view.update(cx, |v, cx| v.follow_markdown_link(&href, window, cx));
                })
            }
            Content::Note(note) => muted(note.clone(), cx),
            Content::Loading | Content::Loaded(_) => muted("Loading file…", cx),
        }
    }

    /// The buttons picking what the open file shows, and the one showing
    /// its history.
    fn mode_buttons(mode: FileMode, history: bool, cx: &mut Context<Self>) -> AnyElement {
        let selected = color(cx, T::BgSelected);
        let mut row = div().flex_none().flex().items_center().gap_1();
        for (m, label, tip) in FileMode::ALL {
            row = row.child(
                button(SharedString::from(format!("file-mode-{label}")), label, cx)
                    .when(m == mode, |b| b.bg(selected))
                    .tooltip(move |_, cx| TextTooltip::view(&[tip.into()], cx))
                    .on_click(cx.listener(move |v: &mut Self, _, _, cx| v.set_file_mode(m, cx))),
            );
        }
        row.child(
            button("file-history", "History", cx)
                .when(history, |b| b.bg(selected))
                .tooltip(|_, cx| {
                    TextTooltip::view(&["The commits that touched this file".into()], cx)
                })
                .on_click(cx.listener(|v: &mut Self, _, _, cx| v.toggle_history(cx))),
        )
        .into_any_element()
    }

    /// The button showing an SVG's source or its image.
    fn svg_toggle(&self, path: String, cx: &mut Context<Self>) -> AnyElement {
        let source = self.browser.as_ref().is_some_and(|b| b.svg_source);
        let label = if source { "Image" } else { "Source" };
        button("file-svg-source", label, cx)
            .on_click(cx.listener(move |v: &mut Self, _, _, cx| {
                if let Some(b) = &mut v.browser {
                    b.svg_source = !b.svg_source;
                }
                v.open_tree_file(path.clone(), cx);
            }))
            .into_any_element()
    }

    /// The button showing a Markdown file's source, or rendered.
    fn markdown_toggle(&self, path: String, cx: &mut Context<Self>) -> AnyElement {
        let source = self.browser.as_ref().is_some_and(|b| b.md_source);
        let label = if source { "Rendered" } else { "Source" };
        button("file-md-source", label, cx)
            .on_click(cx.listener(move |v: &mut Self, _, _, cx| {
                if let Some(b) = &mut v.browser {
                    b.md_source = !b.md_source;
                }
                v.open_tree_file(path.clone(), cx);
            }))
            .into_any_element()
    }

    /// Follows `href`, clicked in the open Markdown file: a heading of it
    /// scrolls into view, a file of the repo opens in its place (a folder:
    /// its README), a web link opens in the browser.
    fn follow_markdown_link(&mut self, href: &str, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(OpenDiff::File(open)) = &mut self.open_diff else {
            return;
        };
        match markdown_view::link_target(&open.path, href) {
            Some(Link::External(url)) => cx.open_url(&url),
            Some(Link::Anchor(anchor)) => {
                if let Content::Markdown(doc) = &mut open.content {
                    doc.scroll_to(anchor, cx);
                    cx.notify();
                }
            }
            Some(Link::File { path, anchor }) => {
                let path = self.link_file(path);
                self.open_tree_file(path, cx);
                if let Some(OpenDiff::File(open)) = &mut self.open_diff {
                    open.anchor = anchor;
                }
            }
            None => {}
        }
    }

    /// The file a link to `path` opens: `path`, or for a folder, its README.
    fn link_file(&self, path: String) -> String {
        let Some(files) = self.browser.as_ref().and_then(|b| b.files.as_ref()) else {
            return path;
        };
        if files.iter().any(|e| e.path == path) {
            return path;
        }
        let folder = format!("{path}/");
        files
            .iter()
            .filter_map(|e| e.path.strip_prefix(&folder).map(|name| (name, &e.path)))
            .find(|(name, _)| {
                !name.contains('/') && name.to_ascii_lowercase().starts_with("readme")
            })
            .map_or(path, |(_, file)| file.clone())
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

    /// Opens the diff view's editor on the open file as it is in the
    /// working copy, at the viewer's cursor line. Opened at a commit, the
    /// uncommitted-changes row is selected first: what is edited and saved
    /// is the file on disk, local changes included, never that commit's
    /// version.
    fn edit_open_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(OpenDiff::File(open)) = &self.open_diff else {
            return;
        };
        let line = match &open.content {
            Content::Shown(editor) => editor.read(cx).cursor_position().line + 1,
            _ => 1,
        };
        let path = open.path.clone();
        if open.at.is_some() {
            // Reopens the file, from the working copy, in the browser.
            self.select_wip(cx);
        }
        file_edit::open(self, path, line, window, cx);
    }

    /// Opens the editor on `path` as it is in the working copy, at `line`,
    /// from a commit's diff (the diff view, or the browser's Change mode):
    /// the uncommitted-changes row is selected and the file shown in the
    /// browser, as Edit does from a file opened at a commit.
    pub(super) fn edit_working_file(
        &mut self,
        path: String,
        line: u32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.browser.is_none() {
            self.toggle_browser(window, cx);
        }
        if let Some(b) = &mut self.browser {
            b.mode = FileMode::Content;
            b.open = Some(path.clone());
        }
        // Reopens `path`, from the working copy, in the browser.
        self.select_wip(cx);
        file_edit::open(self, path, line, window, cx);
    }
}

/// Readies `open` for its render when it is a rendered Markdown file
/// (theme, heading a link led to); whether it is one.
fn prepare_markdown(open: &mut OpenFile, cx: &mut App) -> bool {
    let Content::Markdown(doc) = &mut open.content else {
        return false;
    };
    if let Some(anchor) = open.anchor.take() {
        doc.scroll_to(anchor, cx);
    }
    doc.prepare(cx);
    true
}

/// The open file's path, in its header: it keeps some room, the hint
/// after it gives way first.
fn path_title(path: String) -> Div {
    div()
        .flex_1()
        .min_w(px(PATH_MIN_WIDTH))
        .truncate()
        .child(path)
}

/// The open file's hint (where it is read from, its keys), in its header.
fn hint(text: String, cx: &Context<RepoView>) -> Div {
    div()
        .min_w_0()
        .truncate()
        .text_xs()
        .text_color(color(cx, T::FgMuted))
        .child(text)
}

/// The blame's options, each a toggle.
fn blame_option_buttons(options: BlameOptions, cx: &mut Context<RepoView>) -> AnyElement {
    let selected = color(cx, T::BgSelected);
    let toggle = |id: &'static str,
                  label: &'static str,
                  tip: &'static str,
                  on: bool,
                  flip: fn(&mut BlameOptions),
                  cx: &mut Context<RepoView>| {
        button(id, label, cx)
            .when(on, |b| b.bg(selected))
            .tooltip(move |_, cx| TextTooltip::view(&[tip.into()], cx))
            .on_click(
                cx.listener(move |v: &mut RepoView, _, _, cx| v.toggle_blame_option(flip, cx)),
            )
    };
    div()
        .flex_none()
        .flex()
        .gap_1()
        .child(toggle(
            "blame-whitespace",
            "Ignore whitespace",
            "Whitespace-only changes do not count",
            options.ignore_whitespace,
            |o| o.ignore_whitespace = !o.ignore_whitespace,
            cx,
        ))
        .child(toggle(
            "blame-moves",
            "Detect moves",
            "Lines moved or copied keep the commit that wrote them (slower)",
            options.detect_moves,
            |o| o.detect_moves = !o.detect_moves,
            cx,
        ))
        .into_any_element()
}

/// Where the open file is read from, and its keys.
fn file_note(at: Option<Oid>, editable: bool, mode: FileMode) -> String {
    let place = match at {
        Some(oid) => format!("at {}", oid.short(SHORT_OID)),
        None => "working copy".to_owned(),
    };
    if mode != FileMode::Content {
        return format!("{place} · ↑↓ files · Esc closes");
    }
    let keys = if editable && at.is_none() {
        "double-click edits · ↑↓ files · Esc closes"
    } else if editable {
        "double-click edits the working copy · ↑↓ files · Esc closes"
    } else {
        "read-only · ↑↓ files · Esc closes"
    };
    format!("{place} · {keys}")
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
    list_row(SharedString::from(format!("tree-{}", row.path)), open, cx)
        .w_full()
        .h(row_height(cx))
        .flex()
        .items_center()
        .gap_1()
        .pl(px(8.0 + depth * INDENT))
        .pr_2()
        // The keyboard cursor, when not on the open file: not the hover
        // tint, which the pointer gives any row.
        .when(selected && !open, |d| d.bg(color(cx, T::AccentSubtle)))
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
