//! The working copy of a repo shown by a view with a staging panel (the
//! repo tab, the dashboard's quick edit pane): reading its changes and
//! opening a changed file's diff, which follows the files as they are
//! staged, unstaged or changed again.
//!
//! Written once here over [`WorkingCopy`], so a fix to either view is a fix
//! to both.

use std::sync::Arc;

use archipel_model::changes::FileChange;
use archipel_model::ports::DiffSide;
use gpui_kit::{AppContext as _, Context};

use crate::diff_view::{self, DiffDoc};
use crate::latest::Latest;
use crate::staging::Staging;

/// A working-copy file's diff, open in place of the view's main content.
#[derive(Clone)]
pub(crate) struct WorktreeDiff {
    /// The file, relative to the repo.
    pub file: String,
    /// Staged or unstaged changes.
    pub side: DiffSide,
    /// Its diff; `None` while it is read the first time.
    pub doc: Option<Arc<DiffDoc>>,
}

/// A view showing a repo's working copy (see the [module docs](self)).
pub(crate) trait WorkingCopy: Staging {
    /// The reads of the changes, so only the latest one lands.
    fn change_reads(&mut self) -> &mut Latest;
    /// Keeps the changes just read.
    fn set_changes(&mut self, changes: Vec<FileChange>);
    /// The open working-copy diff, if one is open.
    fn worktree_diff(&mut self) -> Option<&mut WorktreeDiff>;
    /// Opens `diff`, or closes the working-copy diff when `None`.
    fn show_worktree_diff(&mut self, diff: Option<WorktreeDiff>);
    /// Whether the open diff follows the working copy once read again;
    /// when not, it closes.
    fn follows_working_copy(&self) -> bool {
        true
    }
}

/// Reads the working copy changes again. The open diff follows: read again
/// while its file has changes on that side, moved to the other side once
/// they all went there (file or hunks staged), else closed.
pub(crate) fn load_changes<V: WorkingCopy>(v: &mut V, cx: &mut Context<V>) {
    let status = v.core().repo_actions().ports().status.clone();
    let path = v.repo_path().clone();
    let read = v.change_reads().begin();
    let task = cx.background_spawn(async move { status.changes(&path).map_err(|e| e.to_string()) });
    cx.spawn(async move |this, cx| {
        let result = task.await;
        let _ = this.update(cx, |v, cx| {
            if !v.change_reads().is_current(read) {
                return; // a later read started: it may see a newer state
            }
            match result {
                Ok(changes) => v.set_changes(changes),
                Err(e) => v.report(e, cx),
            }
            let open = v.worktree_diff().map(|d| (d.file.clone(), d.side));
            if let Some((file, side)) = open {
                match diff_view::follow(v.changes(), &file, side) {
                    Some(side) if v.follows_working_copy() => {
                        open_worktree_file(v, file, side, cx);
                    }
                    _ => v.show_worktree_diff(None),
                }
            }
            cx.notify();
        });
    })
    .detach();
}

/// Opens the diff of working-copy file `file` on `side`. The diff shown
/// stays until the new one is read when it is the same file (a reload),
/// so the view does not flicker.
pub(crate) fn open_worktree_file<V: WorkingCopy>(
    v: &mut V,
    file: String,
    side: DiffSide,
    cx: &mut Context<V>,
) {
    let shown = v
        .worktree_diff()
        .filter(|d| d.file == file && d.side == side)
        .and_then(|d| d.doc.take());
    v.show_worktree_diff(Some(WorktreeDiff {
        file: file.clone(),
        side,
        doc: shown,
    }));
    let diffs = v.core().diff_reader();
    let path = v.repo_path().clone();
    let task = {
        let file = file.clone();
        cx.background_spawn(async move {
            diffs
                .worktree_diff(&path, side, Some(&file))
                .map(|d| Arc::new(DiffDoc::new(d)))
        })
    };
    cx.spawn(async move |this, cx| {
        let result = task.await;
        let _ = this.update(cx, |v, cx| {
            let Some(open) = v.worktree_diff() else {
                return;
            };
            if open.file != file || open.side != side {
                return; // another file was opened meanwhile
            }
            match result {
                Ok(doc) => {
                    open.doc = Some(doc);
                    cx.notify();
                }
                Err(e) => v.report(e.to_string(), cx),
            }
        });
    })
    .detach();
    cx.notify();
}

/// Opens the changed file `delta` places from the open one (↑ ↓ while a
/// working-copy diff is open). Returns whether one is open.
pub(crate) fn step_worktree_file<V: WorkingCopy>(
    v: &mut V,
    delta: i64,
    cx: &mut Context<V>,
) -> bool {
    let Some(open) = v.worktree_diff().map(|d| (d.file.clone(), d.side)) else {
        return false;
    };
    let files = diff_view::worktree_files(v.changes());
    if let Some((file, side)) = diff_view::step(&files, &open.0, open.1, delta) {
        open_worktree_file(v, file, side, cx);
    }
    true
}
