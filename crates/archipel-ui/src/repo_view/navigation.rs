//! Keyboard navigation of the graph, and the diff opened in place of it
//! from the details pane.

use std::sync::Arc;

use archipel_model::ports::DiffSide;
use archipel_model::requests::StashApplyMode;
use archipel_model::{CancelToken, NoProgress, Oid};
use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, Context, KeyDownEvent, ScrollStrategy, Window, div};

use crate::diff_view::{self, DiffDoc};
use crate::widgets::{SHORT_OID, unmodified};

use super::details::file_title;
use super::{Item, OpenDiff, RepoView, lock};

/// Rows moved by Page Up / Page Down in the graph.
const PAGE_ROWS: i64 = 20;

impl RepoView {
    /// Opens working-copy file `file`'s diff on `side` in place of the
    /// graph. The diff shown stays until the new one is read when it is the
    /// same file (a reload), so the view does not flicker.
    pub(super) fn open_worktree_file(
        &mut self,
        file: String,
        side: DiffSide,
        cx: &mut Context<Self>,
    ) {
        let shown = match self.open_diff.take() {
            Some(OpenDiff::Worktree(f, s, doc)) if f == file && s == side => doc,
            _ => None,
        };
        self.open_diff = Some(OpenDiff::Worktree(file.clone(), side, shown));
        let diffs = self.core.diff_reader();
        let path = self.path.clone();
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
                let Some(OpenDiff::Worktree(f, s, doc)) = &mut v.open_diff else {
                    return;
                };
                if *f != file || *s != side {
                    return; // another file was opened meanwhile
                }
                match result {
                    Ok(d) => *doc = Some(d),
                    Err(e) => v.status = Some(e.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Opens file `file` of the selected commit's or stash's diff in place
    /// of the graph; `None`: all its files.
    pub(super) fn open_commit_file(&mut self, file: Option<usize>, cx: &mut Context<Self>) {
        let Some((_, doc)) = &self.details else {
            return;
        };
        let doc = match file {
            None => Arc::clone(doc),
            Some(i) => match doc.files.get(i) {
                Some(f) => Arc::new(DiffDoc::new(vec![f.clone()])),
                None => return,
            },
        };
        self.open_diff = Some(OpenDiff::Commit(file, doc));
        cx.notify();
    }

    /// Opens the first working-copy file (double click / Enter on the
    /// uncommitted-changes row).
    pub(super) fn open_first_change(&mut self, cx: &mut Context<Self>) {
        if let Some((file, side)) = diff_view::worktree_files(&self.changes).into_iter().next() {
            self.open_worktree_file(file, side, cx);
        }
    }

    /// Opens the file `delta` places from the open one in the details
    /// pane's list (keyboard navigation while a diff is open).
    fn step_file(&mut self, delta: i64, cx: &mut Context<Self>) {
        match &self.open_diff {
            Some(OpenDiff::Commit(file, _)) => {
                let count = self.details.as_ref().map_or(0, |(_, d)| d.files.len());
                let last = i64::try_from(count).unwrap_or(i64::MAX) - 1;
                // "All files" heads the list, at -1.
                let at = file.map_or(-1, |i| i64::try_from(i).unwrap_or(i64::MAX));
                let next = (at + delta).clamp(-1, last.max(-1));
                self.open_commit_file(usize::try_from(next).ok(), cx);
            }
            Some(OpenDiff::Worktree(file, side, _)) => {
                let files = diff_view::worktree_files(&self.changes);
                if let Some((file, side)) = diff_view::step(&files, file, *side, delta) {
                    self.open_worktree_file(file, side, cx);
                }
            }
            None => {}
        }
    }

    pub(super) fn close_diff(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_diff = None;
        self.focus.focus(window, cx);
        cx.notify();
    }

    /// Selects graph row `row` and scrolls it into view.
    fn show_row(&mut self, row: u32, oid: Oid, strategy: ScrollStrategy, cx: &mut Context<Self>) {
        let item = self.item_of_row(row);
        self.scroll
            .scroll_to_item(usize::try_from(item).unwrap_or(0), strategy);
        self.select_commit(row, oid, cx);
    }

    /// Moves the graph selection by `delta` rows (keyboard navigation).
    /// Rows are counted in list items, uncommitted changes and stashes included.
    fn step(&mut self, delta: i64, cx: &mut Context<Self>) {
        let Some(graph) = &self.graph else {
            return;
        };
        let last = i64::from(self.item_count()) - 1;
        if last < 0 {
            return;
        }
        let from = if self.wip_selected {
            Some(0)
        } else if let Some(oid) = self.stash_selected {
            self.stash_at(oid).map(|k| self.item_of_stash(k))
        } else {
            self.selected.map(|(r, _)| self.item_of_row(r))
        };
        let item = (from.map_or(-1, i64::from) + delta).clamp(0, last);
        let item = u32::try_from(item).unwrap_or(0);
        match self.item_at(item) {
            Item::Wip => {
                self.scroll.scroll_to_item(0, ScrollStrategy::Nearest);
                self.select_wip(cx);
            }
            Item::Stash(k) => {
                self.scroll
                    .scroll_to_item(usize::try_from(item).unwrap_or(0), ScrollStrategy::Nearest);
                self.select_stash(k, cx);
            }
            Item::Commit(row) => {
                let oid = lock(graph)
                    .oids(row..row + 1)
                    .ok()
                    .and_then(|o| o.first().copied());
                if let Some(oid) = oid {
                    self.show_row(row, oid, ScrollStrategy::Nearest, cx);
                }
            }
        }
    }

    /// Plain keys of the graph pane: arrows, pages, Enter and Esc.
    pub(super) fn on_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !unmodified(event.keystroke.modifiers) {
            return;
        }
        if self.menu.is_some() && event.keystroke.key == "escape" {
            self.menu = None;
            cx.stop_propagation();
            cx.notify();
            return;
        }
        if self.open_diff.is_some() {
            match event.keystroke.key.as_str() {
                "down" => self.step_file(1, cx),
                "up" => self.step_file(-1, cx),
                "escape" => self.close_diff(window, cx),
                _ => return,
            }
            cx.stop_propagation();
            return;
        }
        match event.keystroke.key.as_str() {
            "down" => self.step(1, cx),
            "up" => self.step(-1, cx),
            "pagedown" => self.step(PAGE_ROWS, cx),
            "pageup" => self.step(-PAGE_ROWS, cx),
            "home" => self.step(i64::MIN / 2, cx),
            "end" => self.step(i64::MAX / 2, cx),
            "enter" if self.wip_selected => self.open_first_change(cx),
            "enter" if self.stash_selected.is_some() => {
                if let Some(oid) = self.stash_selected {
                    self.stash_op(oid, Some(StashApplyMode::Keep), cx);
                }
            }
            "enter" => match self.selected {
                Some((row, _)) => self.checkout_row(row, cx),
                None => return,
            },
            _ => return,
        }
        cx.stop_propagation();
    }

    /// Selects the commit `oid` and scrolls to it (refs panel). The row is
    /// found once the topology is complete, without holding the view lock.
    pub(super) fn jump_to(&mut self, oid: Oid, cx: &mut Context<Self>) {
        let Some(graph) = self.graph.clone() else {
            return;
        };
        let task = cx.background_spawn(async move {
            let completion = lock(&graph).completion();
            if let Some(c) = completion {
                c.wait(&NoProgress)?;
            }
            let searcher = lock(&graph).searcher()?;
            searcher.row_of(oid, &CancelToken::new())
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |v, cx| match result {
                Ok(Some(row)) => v.show_row(row, oid, ScrollStrategy::Center, cx),
                Ok(None) => {
                    v.status = Some(format!(
                        "{} is not in the history shown",
                        oid.short(SHORT_OID)
                    ));
                    cx.notify();
                }
                Err(e) => {
                    v.status = Some(e.to_string());
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// The open diff, in place of the graph: a header naming it, then the
    /// diff (hunks stage / unstage on the working copy).
    pub(super) fn render_open_diff(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let (title, note, side, doc) = match &self.open_diff {
            Some(OpenDiff::Commit(file, doc)) => {
                let title = match file {
                    Some(_) => doc.files.first().map(file_title).unwrap_or_default(),
                    None => format!("All files ({})", doc.files.len()),
                };
                let at = self.details.as_ref().map(|(oid, _)| oid.short(SHORT_OID));
                (title, at.unwrap_or_default(), None, Some(Arc::clone(doc)))
            }
            Some(OpenDiff::Worktree(file, side, doc)) => (
                file.clone(),
                diff_view::side_note(*side).to_owned(),
                Some(*side),
                doc.clone(),
            ),
            None => return div().into_any_element(),
        };
        diff_view::render_open(self, title, &note, side, doc, cx)
    }
}
