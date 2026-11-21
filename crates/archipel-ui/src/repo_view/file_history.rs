//! The history of the file open in the file browser. **History** lists, in
//! the details pane, the commits that touched it (renames followed); a
//! click selects the commit and shows what it changed in the file. The open
//! file shows as itself (**File**), as the selected commit's change to it
//! (**Change**), or with who last changed each line (**Blame**): a gutter
//! naming the commit, author and age of each block of lines, shaded by
//! age. A click on a block selects its commit; Alt+click selects the
//! commit before it, to step past a reformat or a move.

use std::ops::Range;
use std::sync::Arc;

use archipel_app::dashboard::{exact_time, relative_time};
use archipel_model::diff::FileDiff;
use archipel_model::file_history::{Blame, BlameHunk, BlameParent, FileRevision};
use archipel_model::{CancelToken, GitError, Oid, Timestamp};
use archipel_theme::ColorToken as T;
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, ClickEvent, Context, Div, Hsla, SharedString, Stateful, UniformListScrollHandle,
    Window, div, px, uniform_list,
};

use crate::diff_view::{DiffDoc, Revs};
use crate::theme::{color, mono_font, row_height};
use crate::widgets::{SHORT_OID, TextTooltip, button, list_row, scroll_list, section};

use super::RepoView;

/// Commits read per page of a file's history.
const HISTORY_PAGE: usize = 200;
/// Width of the blame gutter.
const GUTTER_WIDTH: f32 = 280.0;
/// Width of the age bar at the gutter's left edge.
const AGE_BAR_WIDTH: f32 = 3.0;
/// Opacity of the age bar of the oldest lines (the newest are opaque).
const OLDEST_OPACITY: f32 = 0.12;
/// Spaces a tab is shown as in the blame view.
const TAB: &str = "    ";

/// What the open file shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum FileMode {
    /// Its content.
    #[default]
    Content,
    /// The selected commit's change to it.
    Change,
    /// Its content, with who last changed each line.
    Blame,
}

impl FileMode {
    /// The modes, their button labels and tooltips.
    pub(super) const ALL: [(Self, &'static str, &'static str); 3] = [
        (Self::Content, "File", "The file as it is there"),
        (
            Self::Change,
            "Change",
            "What the selected commit changed in the file",
        ),
        (
            Self::Blame,
            "Blame",
            "Who last changed each line: click a block for its commit, Alt+click for the one before",
        ),
    ];
}

/// The commits that touched a file, read a page at a time.
pub(super) struct FileHistory {
    /// The file, as named where the history starts.
    path: String,
    /// Where the history starts: the commit browsed when it was opened
    /// (`None`: `HEAD`).
    from: Option<Oid>,
    entries: Arc<Vec<FileRevision>>,
    loading: bool,
    /// The last page was full: more commits may follow.
    more: bool,
    cancel: CancelToken,
    scroll: UniformListScrollHandle,
}

impl Drop for FileHistory {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

/// A blame ready to show.
pub(super) struct BlameDoc {
    blame: Blame,
    /// Oldest and newest author dates of the committed lines.
    span: Option<(Timestamp, Timestamp)>,
    /// Digits of the last line number.
    number_width: usize,
}

impl BlameDoc {
    /// Checks that `blame` can be shown: refused for binary and very large
    /// files, as the file view.
    pub(super) fn new(blame: Blame, max_bytes: usize) -> Result<Self, String> {
        let bytes: usize = blame.lines.iter().map(|l| l.len() + 1).sum();
        if bytes > max_bytes {
            return Err(format!(
                "Too large to blame here ({} KB)",
                bytes.div_ceil(1024)
            ));
        }
        if blame.lines.iter().any(|l| l.contains('\0')) {
            return Err("Binary file: no blame".into());
        }
        if blame.lines.is_empty() {
            return Err("Empty file".into());
        }
        Ok(Self {
            span: blame.time_span(),
            number_width: blame.lines.len().to_string().len(),
            blame,
        })
    }

    /// How recent `t` is among the blamed lines: 0 for the oldest, 1 for
    /// the newest.
    fn recency(&self, t: Timestamp) -> f32 {
        let Some((lo, hi)) = self.span else {
            return 1.0;
        };
        if hi.0 <= lo.0 {
            return 1.0;
        }
        #[allow(clippy::cast_precision_loss)] // a shade: precision is moot
        let f = (t.0 - lo.0) as f32 / (hi.0 - lo.0) as f32;
        f.clamp(0.0, 1.0)
    }
}

/// The file `path`'s change in a commit's diff: the file there, or, for a
/// renamed or deleted file, the one it was.
pub(super) fn change_of(files: &[FileDiff], path: &str) -> Option<FileDiff> {
    files
        .iter()
        .find(|f| f.new_path.as_deref() == Some(path))
        .or_else(|| files.iter().find(|f| f.old_path.as_deref() == Some(path)))
        .cloned()
}

/// A one-file diff of commit `oid`, to show in the open file.
pub(super) fn change_doc(file: FileDiff, oid: Oid) -> Arc<DiffDoc> {
    Arc::new(DiffDoc::new(vec![file]).with_revs(Some(Revs::span(oid, oid))))
}

impl RepoView {
    /// Shows or hides the history of the open file in the details pane.
    pub(super) fn toggle_history(&mut self, cx: &mut Context<Self>) {
        let Some(b) = &mut self.browser else {
            return;
        };
        if b.history.take().is_none()
            && let Some(path) = b.open.clone()
        {
            b.history = Some(FileHistory {
                path,
                from: b.at,
                entries: Arc::default(),
                loading: false,
                more: true,
                cancel: CancelToken::new(),
                scroll: UniformListScrollHandle::new(),
            });
            self.load_history_page(cx);
        }
        cx.notify();
    }

    /// Opens `path` in the file browser (turned on if needed) at the
    /// selected commit or in the working copy: its blame (`Blame`), or its
    /// history (`History`), next to the commit's change to it.
    fn browse_file(
        &mut self,
        path: String,
        mode: FileMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.browser.is_none() {
            self.toggle_browser(window, cx);
        }
        let Some(b) = &mut self.browser else {
            return;
        };
        let history = mode != FileMode::Blame;
        b.mode = match mode {
            FileMode::Change if b.at.is_none() => FileMode::Content,
            m => m,
        };
        b.history = None;
        self.open_tree_file(path, cx);
        if history {
            self.toggle_history(cx);
        }
    }

    /// The History and Blame buttons of a one-file diff's header.
    pub(super) fn file_history_buttons(path: String, cx: &mut Context<Self>) -> AnyElement {
        let blamed = path.clone();
        div()
            .flex_none()
            .flex()
            .gap_1()
            .child(
                button("diff-file-history", "History", cx)
                    .tooltip(|_, cx| {
                        TextTooltip::view(&["The commits that touched this file".into()], cx)
                    })
                    .on_click(cx.listener(move |v: &mut Self, _, window, cx| {
                        v.browse_file(path.clone(), FileMode::Change, window, cx);
                    })),
            )
            .child(
                button("diff-file-blame", "Blame", cx)
                    .tooltip(|_, cx| TextTooltip::view(&["Who last changed each line".into()], cx))
                    .on_click(cx.listener(move |v: &mut Self, _, window, cx| {
                        v.browse_file(blamed.clone(), FileMode::Blame, window, cx);
                    })),
            )
            .into_any_element()
    }

    /// Whether the history of the open file is shown.
    pub(super) fn history_shown(&self) -> bool {
        self.browser.as_ref().is_some_and(|b| b.history.is_some())
    }

    /// Reads the next page of the file's history.
    fn load_history_page(&mut self, cx: &mut Context<Self>) {
        let Some(h) = self.browser.as_mut().and_then(|b| b.history.as_mut()) else {
            return;
        };
        if h.loading || !h.more {
            return;
        }
        h.loading = true;
        let (path, from, skip, cancel) =
            (h.path.clone(), h.from, h.entries.len(), h.cancel.clone());
        let reader = self.core.repo_actions().ports().file_history.clone();
        let repo = self.path.clone();
        let task = {
            let cancel = cancel.clone();
            cx.background_spawn(async move {
                reader.file_log(&repo, &path, from, skip, HISTORY_PAGE, &cancel)
            })
        };
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |v, cx| {
                let Some(h) = v.browser.as_mut().and_then(|b| b.history.as_mut()) else {
                    return;
                };
                if !h.cancel.same_as(&cancel) {
                    return; // closed, or another file's history meanwhile
                }
                h.loading = false;
                match result {
                    Ok(page) => {
                        h.more = page.len() == HISTORY_PAGE;
                        let mut entries = Vec::clone(&h.entries);
                        entries.extend(page);
                        h.entries = Arc::new(entries);
                    }
                    Err(GitError::Cancelled) => {}
                    Err(e) => {
                        h.more = false;
                        v.status = Some(e.to_string());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Selects commit `oid` with the open file named `path` there, showing
    /// that file. From the history, a file shown as is shows the commit's
    /// change instead.
    fn show_revision(&mut self, oid: Oid, path: String, change: bool, cx: &mut Context<Self>) {
        if let Some(b) = &mut self.browser {
            if change && b.mode == FileMode::Content {
                b.mode = FileMode::Change;
            }
            b.open = Some(path);
        }
        self.jump_to(oid, cx);
    }

    /// The history of the open file, in the details pane.
    pub(super) fn render_history(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let Some((h, at)) = self
            .browser
            .as_ref()
            .and_then(|b| b.history.as_ref().map(|h| (h, b.at)))
        else {
            return div().into_any_element();
        };
        let name = h.path.rsplit('/').next().unwrap_or(&h.path).to_owned();
        let count = if h.more {
            format!("{}+", h.entries.len())
        } else {
            h.entries.len().to_string()
        };
        let title = format!("HISTORY OF {name} ({count})");
        let entries = h.entries.clone();
        let rows = entries.len() + usize::from(h.more);
        let loading = h.loading;
        let key = SharedString::from(format!("file-history-{}", h.path));
        let scroll = h.scroll.clone();
        let header = div()
            .flex()
            .items_center()
            .pr_2()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(section(title, cx)),
            )
            .child(
                button("file-history-close", "Files", cx)
                    .tooltip(|_, cx| TextTooltip::view(&["Back to the file tree".into()], cx))
                    .on_click(cx.listener(|v, _, _, cx| v.toggle_history(cx))),
            );
        let body = if entries.is_empty() && !h.more {
            div()
                .px_2()
                .text_color(color(cx, T::FgMuted))
                .child("No commit touched this file")
                .into_any_element()
        } else {
            let now = Timestamp::now();
            let list = uniform_list(
                key.clone(),
                rows,
                cx.processor(move |_: &mut Self, range: Range<usize>, _window, cx| {
                    range
                        .map(|i| match entries.get(i) {
                            Some(entry) => {
                                render_revision(entry, at == Some(entry.meta.oid), now, cx)
                            }
                            None => render_more(loading, cx),
                        })
                        .collect()
                }),
            );
            scroll_list(SharedString::from(format!("{key}-scroll")), list)
                .handle(&scroll)
                .into_any_element()
        };
        div()
            .id("file-history")
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .overflow_hidden()
            .child(header)
            .child(body)
            .into_any_element()
    }

    /// Selects a blamed block's commit (`before`: the commit before it).
    fn follow_blame(
        &mut self,
        oid: Oid,
        path: String,
        parent: Option<BlameParent>,
        before: bool,
        cx: &mut Context<Self>,
    ) {
        if !before {
            self.show_revision(oid, path, false, cx);
            return;
        }
        if let Some(p) = parent {
            self.show_revision(p.oid, p.path, false, cx);
        } else {
            self.status = Some(format!(
                "{} added these lines: there is nothing before it",
                oid.short(SHORT_OID)
            ));
            cx.notify();
        }
    }
}

/// The blame of the open file: a gutter, the line numbers and the lines.
pub(super) fn render_blame(
    doc: &Arc<BlameDoc>,
    key: &str,
    cx: &mut Context<RepoView>,
) -> AnyElement {
    let now = Timestamp::now();
    let rows = doc.blame.lines.len();
    let id = SharedString::from(format!("blame-{key}"));
    let doc = doc.clone();
    let list = uniform_list(
        id.clone(),
        rows,
        cx.processor(move |_: &mut RepoView, range: Range<usize>, _window, cx| {
            range.map(|i| render_blame_line(&doc, i, now, cx)).collect()
        }),
    );
    div()
        .flex_1()
        .min_h_0()
        .flex()
        .flex_col()
        .font_family(mono_font(cx))
        .text_sm()
        .child(scroll_list(
            SharedString::from(format!("{id}-scroll")),
            list,
        ))
        .into_any_element()
}

/// Line `i` of a blame: the gutter (filled on the first line of a block),
/// the line number, the line.
fn render_blame_line(
    doc: &BlameDoc,
    i: usize,
    now: Timestamp,
    cx: &mut Context<RepoView>,
) -> AnyElement {
    let blame = &doc.blame;
    let (Some(hunk), Some(line)) = (blame.hunk_at(i), blame.lines.get(i)) else {
        return div().into_any_element();
    };
    let commit = &blame.commits[hunk.commit];
    let first = hunk.start == i;
    let bar: Hsla = match commit.oid {
        Some(_) => {
            let shade = OLDEST_OPACITY + (1.0 - OLDEST_OPACITY) * doc.recency(commit.author_time);
            color(cx, T::Accent).opacity(shade)
        }
        None => color(cx, T::StatusDirty),
    };
    let mut gutter = div()
        .id(("blame-gutter", i))
        .w(px(GUTTER_WIDTH))
        .h_full()
        .flex_none()
        .flex()
        .items_center()
        .gap_2()
        .pr_2()
        .text_xs()
        .child(div().w(px(AGE_BAR_WIDTH)).h_full().flex_none().bg(bar));
    if first {
        let (id_text, who) = match commit.oid {
            Some(oid) => (oid.short(SHORT_OID), commit.author_name.clone()),
            None => (String::new(), "Not committed yet".to_owned()),
        };
        gutter = gutter
            .child(
                div()
                    .flex_none()
                    .text_color(color(cx, T::FgSubtle))
                    .child(id_text),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(color(cx, T::FgMuted))
                    .child(who),
            )
            .when(commit.oid.is_some(), |g| {
                g.child(
                    div()
                        .flex_none()
                        .text_color(color(cx, T::FgSubtle))
                        .child(relative_time(commit.author_time, now)),
                )
            });
    }
    if commit.oid.is_some() {
        gutter = blame_gutter_actions(gutter, blame, hunk, cx);
    }
    let number = format!("{:>w$}", i + 1, w = doc.number_width);
    div()
        .w_full()
        .h(row_height(cx))
        .flex()
        .items_center()
        .when(first && i > 0, |d| {
            d.border_t_1().border_color(color(cx, T::BorderSubtle))
        })
        .child(gutter)
        .child(
            div()
                .flex_none()
                .px_2()
                .text_color(color(cx, T::FgSubtle))
                .child(number),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .whitespace_nowrap()
                .overflow_hidden()
                .child(line.replace('\t', TAB)),
        )
        .into_any_element()
}

/// The tooltip and clicks of a committed block's gutter: its commit's
/// details; a click selects the commit, Alt+click the one before it.
fn blame_gutter_actions(
    gutter: Stateful<Div>,
    blame: &Blame,
    hunk: &BlameHunk,
    cx: &mut Context<RepoView>,
) -> Stateful<Div> {
    let commit = &blame.commits[hunk.commit];
    let Some(oid) = commit.oid else {
        return gutter;
    };
    let mut tip = vec![
        commit.summary.clone(),
        format!("{} <{}>", commit.author_name, commit.author_email),
        format!(
            "{} · {}",
            exact_time(commit.author_time),
            oid.short(SHORT_OID)
        ),
    ];
    if hunk.path != blame_path(blame) {
        tip.push(format!("in {}", hunk.path));
    }
    tip.push(match commit.parent {
        Some(_) => "Click: select this commit · Alt+click: the commit before it".into(),
        None => "Click: select this commit (it added these lines)".into(),
    });
    let (path, parent) = (hunk.path.clone(), commit.parent.clone());
    let hover = color(cx, T::BgHover);
    gutter
        .cursor_pointer()
        .hover(move |s| s.bg(hover))
        .tooltip(move |_, cx| TextTooltip::view(&tip, cx))
        .on_click(cx.listener(move |v, e: &ClickEvent, _, cx| {
            v.follow_blame(oid, path.clone(), parent.clone(), e.modifiers().alt, cx);
        }))
}

/// The path most of the blame's lines name: the file's own.
fn blame_path(blame: &Blame) -> &str {
    blame.hunks.last().map_or("", |h| h.path.as_str())
}

/// One commit of the history: its summary, then its id, author and age.
fn render_revision(
    entry: &FileRevision,
    selected: bool,
    now: Timestamp,
    cx: &mut Context<RepoView>,
) -> AnyElement {
    let meta = &entry.meta;
    let (oid, path) = (meta.oid, entry.path.clone());
    let tip = vec![
        meta.summary.clone(),
        format!("{} <{}>", meta.author_name, meta.author_email),
        format!(
            "{} · {}",
            exact_time(meta.author_time),
            oid.short(SHORT_OID)
        ),
        format!("as {}", entry.path),
    ];
    list_row(
        SharedString::from(format!("file-rev-{}", oid.to_hex())),
        selected,
        cx,
    )
    .w_full()
    .h(row_height(cx) * 2.0)
    .px_2()
    .flex()
    .flex_col()
    .justify_center()
    .tooltip(move |_, cx| TextTooltip::view(&tip, cx))
    .on_click(cx.listener(move |v, _, _, cx| {
        v.show_revision(oid, path.clone(), true, cx);
    }))
    .child(div().truncate().child(meta.summary.clone()))
    .child(
        div()
            .flex()
            .gap_2()
            .text_xs()
            .text_color(color(cx, T::FgSubtle))
            .child(
                div()
                    .flex_none()
                    .font_family(mono_font(cx))
                    .child(oid.short(SHORT_OID)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(meta.author_name.clone()),
            )
            .child(
                div()
                    .flex_none()
                    .child(relative_time(meta.author_time, now)),
            ),
    )
    .into_any_element()
}

/// The last row of a history with more commits to read.
fn render_more(loading: bool, cx: &mut Context<RepoView>) -> AnyElement {
    let label = if loading { "Loading…" } else { "Load more" };
    list_row("file-history-more", false, cx)
        .w_full()
        .h(row_height(cx) * 2.0)
        .px_2()
        .flex()
        .items_center()
        .text_color(color(cx, T::FgMuted))
        .on_click(cx.listener(|v, _, _, cx| {
            v.load_history_page(cx);
            cx.notify();
        }))
        .child(label)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use archipel_model::changes::ChangeKind;
    use archipel_model::file_history::BlameCommit;

    fn diff(old: Option<&str>, new: Option<&str>) -> FileDiff {
        FileDiff {
            old_path: old.map(Into::into),
            new_path: new.map(Into::into),
            kind: ChangeKind::Modified,
            binary: false,
            hunks: Vec::new(),
            truncated: false,
        }
    }

    #[test]
    fn a_change_is_found_by_its_new_path_then_its_old_one() {
        let files = [
            diff(Some("old.rs"), Some("new.rs")),
            diff(Some("gone.rs"), None),
            diff(Some("x.rs"), Some("old.rs")),
        ];
        let found = |p| change_of(&files, p).map(|f| f.display_path().to_owned());
        assert_eq!(found("new.rs").as_deref(), Some("new.rs"));
        assert_eq!(found("gone.rs").as_deref(), Some("gone.rs"));
        // A new file took the old name: that one is the file there.
        assert_eq!(found("old.rs").as_deref(), Some("old.rs"));
        assert_eq!(
            change_of(&files, "x.rs").map(|f| f.new_path),
            Some(Some("old.rs".into()))
        );
        assert!(change_of(&files, "none").is_none());
    }

    fn blame(lines: &[&str], times: &[i64]) -> Blame {
        Blame {
            commits: times
                .iter()
                .map(|&t| BlameCommit {
                    oid: Some(archipel_model::testing::oid(u64::try_from(t).unwrap())),
                    summary: String::new(),
                    author_name: String::new(),
                    author_email: String::new(),
                    author_time: Timestamp(t),
                    parent: None,
                })
                .collect(),
            hunks: vec![BlameHunk {
                commit: 0,
                start: 0,
                len: lines.len(),
                path: "f".into(),
            }],
            lines: lines.iter().map(|&l| l.to_owned()).collect(),
        }
    }

    #[test]
    fn binary_empty_and_huge_files_are_not_blamed() {
        assert!(BlameDoc::new(blame(&["a", "b"], &[1]), 100).is_ok());
        assert!(BlameDoc::new(blame(&["a\0b"], &[1]), 100).is_err());
        assert!(BlameDoc::new(blame(&[], &[1]), 100).is_err());
        assert!(BlameDoc::new(blame(&["0123456789"], &[1]), 10).is_err());
    }

    #[test]
    fn recency_spans_the_oldest_to_the_newest_line() {
        let doc = BlameDoc::new(blame(&["a"], &[100, 300, 200]), 100).unwrap();
        assert!((doc.recency(Timestamp(100))).abs() < f32::EPSILON);
        assert!((doc.recency(Timestamp(200)) - 0.5).abs() < f32::EPSILON);
        assert!((doc.recency(Timestamp(300)) - 1.0).abs() < f32::EPSILON);
        let single = BlameDoc::new(blame(&["a"], &[5]), 100).unwrap();
        assert!((single.recency(Timestamp(5)) - 1.0).abs() < f32::EPSILON);
    }
}
