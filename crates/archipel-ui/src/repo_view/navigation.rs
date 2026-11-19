//! Keyboard navigation of the graph, and the diff opened in place of it
//! from the details pane.

use std::collections::BTreeMap;
use std::sync::Arc;

use archipel_graph::GraphError;
use archipel_model::requests::StashApplyMode;
use archipel_model::{CancelToken, NoProgress, Oid};
use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, Context, KeyDownEvent, ScrollStrategy, Window, div};

use crate::diff_view::{self, DiffDoc, Revs};
use crate::widgets::{ListMove, SHORT_OID, unmodified};
use crate::working_copy;

use super::details::file_title;
use super::{Item, OpenDiff, RepoView, Selected, SharedView, lock};

impl RepoView {
    /// Opens file `file` of the selected commit's or stash's diff in place
    /// of the graph; `None`: all its files.
    pub(super) fn open_commit_file(&mut self, file: Option<usize>, cx: &mut Context<Self>) {
        let Some(doc) = self.listed_doc() else {
            return;
        };
        let doc = match file {
            None => Arc::clone(doc),
            Some(i) => match doc.files.get(i) {
                Some(f) => Arc::new(DiffDoc::new(vec![f.clone()]).with_revs(doc.revs.clone())),
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
            working_copy::open_worktree_file(self, file, side, cx);
        }
    }

    /// Opens the file `delta` places from the open one in the details
    /// pane's list (keyboard navigation while a diff is open).
    fn step_file(&mut self, delta: i64, cx: &mut Context<Self>) {
        match &self.open_diff {
            Some(OpenDiff::Commit(file, _)) => {
                let count = self.listed_doc().map_or(0, |d| d.files.len());
                let last = i64::try_from(count).unwrap_or(i64::MAX) - 1;
                // "All files" heads the list, at -1.
                let at = file.map_or(-1, |i| i64::try_from(i).unwrap_or(i64::MAX));
                let next = (at + delta).clamp(-1, last.max(-1));
                self.open_commit_file(usize::try_from(next).ok(), cx);
            }
            Some(OpenDiff::Worktree(_)) => {
                working_copy::step_worktree_file(self, delta, cx);
            }
            Some(OpenDiff::File(_)) => self.step_open_file(delta, cx),
            None => {}
        }
    }

    pub(super) fn close_diff(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.open_diff, Some(OpenDiff::File(_))) {
            self.forget_open_file();
        }
        self.open_diff = None;
        self.focus.focus(window, cx);
        cx.notify();
    }

    /// The selected commits by graph row: the multi-selection, else the
    /// selected commit alone.
    fn selection(&self) -> BTreeMap<u32, Oid> {
        if self.multi.is_empty() {
            self.selected
                .and_then(Selected::placed)
                .into_iter()
                .collect()
        } else {
            self.multi.clone()
        }
    }

    /// The commits of the multi-selection, newest first.
    pub(super) fn selected_commits(&self) -> Vec<Oid> {
        self.multi.values().copied().collect()
    }

    /// Ctrl+click on commit `oid` at graph row `row`: adds it to the
    /// selection, or takes it out (the last one stays).
    pub(super) fn toggle_commit(&mut self, row: u32, oid: Oid, cx: &mut Context<Self>) {
        let mut set = self.selection();
        if set.remove(&row).is_none() {
            set.insert(row, oid);
        }
        self.set_selection(set, (row, oid), cx);
    }

    /// Shift+click on commit `oid` at graph row `row`: selects the commit
    /// rows from the selected one to it that are in the range, i.e. the
    /// newer end and its ancestors down to the older end; the commits of
    /// other branches in between, whose changes the range does not hold,
    /// are left out. Every row of the span while the graph is still loading.
    pub(super) fn select_commit_span(&mut self, row: u32, oid: Oid, cx: &mut Context<Self>) {
        let Some(anchor) = self.selected.and_then(Selected::placed) else {
            self.select_commit(row, oid, cx);
            return;
        };
        let (newest, oldest) = (anchor.0.min(row), anchor.0.max(row));
        let set: BTreeMap<u32, Oid> = self
            .graph
            .as_ref()
            .map(|g| {
                let g = lock(g);
                let rows = g.rows(newest..oldest + 1).unwrap_or_default();
                let reachable = g.reachable_rows(newest, oldest).ok().flatten();
                rows.into_iter()
                    .filter(|r| {
                        r.row == oldest
                            || reachable
                                .as_ref()
                                .is_none_or(|kept| kept.binary_search(&r.row).is_ok())
                    })
                    .map(|r| (r.row, r.oid))
                    .collect()
            })
            .unwrap_or_default();
        self.set_selection(set, anchor, cx);
    }

    /// Selects the commits of `set`; the details pane shows `focus`, or
    /// the newest of `set` when `focus` is not in it.
    fn set_selection(
        &mut self,
        set: BTreeMap<u32, Oid>,
        focus: (u32, Oid),
        cx: &mut Context<Self>,
    ) {
        let Some((&first, &first_oid)) = set.iter().next() else {
            return;
        };
        let shown = if set.contains_key(&focus.0) {
            focus
        } else {
            (first, first_oid)
        };
        if self.selected != Some(Selected::at(shown.0, shown.1)) {
            self.select_commit(shown.0, shown.1, cx);
        }
        self.multi = if set.len() > 1 { set } else { BTreeMap::new() };
        // A diff opened from the previous selection's files no longer
        // matches the list.
        if matches!(self.open_diff, Some(OpenDiff::Commit(..))) {
            self.open_diff = None;
        }
        self.load_compare(cx);
        cx.notify();
    }

    /// The ends of a multi-selection: the oldest commit (lowest in the
    /// graph), then the newest. Commits selected in between do not change
    /// the diff, which holds the changes of the oldest to the newest.
    pub(super) fn compared(&self) -> Option<(Oid, Oid)> {
        if self.multi.len() < 2 {
            return None;
        }
        let newer = *self.multi.values().next()?;
        let older = *self.multi.values().next_back()?;
        Some((older, newer))
    }

    /// The diff whose files the details pane lists: between the ends of
    /// the multi-selection, else of the selected commit or stash.
    pub(super) fn listed_doc(&self) -> Option<&Arc<DiffDoc>> {
        match self.compared() {
            Some(pair) => self
                .compare
                .as_ref()
                .filter(|(older, newer, _)| (*older, *newer) == pair)
                .map(|(_, _, doc)| doc),
            None => self.details.as_ref().map(|(_, doc)| doc),
        }
    }

    /// Reads the diff between the ends of the multi-selection, when
    /// several commits are selected and it is not read yet.
    fn load_compare(&mut self, cx: &mut Context<Self>) {
        let Some((older, newer)) = self.compared() else {
            return;
        };
        if self
            .compare
            .as_ref()
            .is_some_and(|(o, n, _)| (*o, *n) == (older, newer))
        {
            return;
        }
        self.compare = None;
        let diffs = self.core.diff_reader();
        let path = self.path.clone();
        let task = cx.background_spawn(async move {
            diffs
                .range_diff(&path, older, newer)
                .map(|d| Arc::new(DiffDoc::new(d).with_revs(Some(Revs::span(older, newer)))))
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |v, cx| {
                if v.compared() != Some((older, newer)) {
                    return; // the selection changed meanwhile
                }
                match result {
                    Ok(d) => v.compare = Some((older, newer, d)),
                    Err(e) => v.status = Some(e.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Selects graph row `row` and scrolls it into view.
    pub(super) fn show_row(
        &mut self,
        row: u32,
        oid: Oid,
        strategy: ScrollStrategy,
        cx: &mut Context<Self>,
    ) {
        let item = self.item_of_row(row);
        self.scroll
            .scroll_to_item(usize::try_from(item).unwrap_or(0), strategy);
        self.select_commit(row, oid, cx);
    }

    /// Moves the graph selection (keyboard navigation).
    /// Rows are counted in list items, uncommitted changes and stashes included.
    fn step(&mut self, to: ListMove, cx: &mut Context<Self>) {
        let Some(graph) = &self.graph else {
            return;
        };
        let from = if self.wip_selected {
            Some(0)
        } else if let Some(oid) = self.stash_selected {
            self.stash_at(oid).map(|k| self.item_of_stash(k))
        } else {
            self.selected
                .and_then(|s| s.row)
                .map(|r| self.item_of_row(r))
        };
        let len = usize::try_from(self.item_count()).unwrap_or(0);
        let from = from.and_then(|i| usize::try_from(i).ok());
        let Some(item) = to.apply(from, len).and_then(|i| u32::try_from(i).ok()) else {
            return;
        };
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

    /// Keys of the graph pane: arrows, pages, Enter and Esc; F3 / Shift+F3
    /// go through the find bar's matches.
    pub(super) fn on_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let k = &event.keystroke;
        let m = k.modifiers;
        if self.open_diff.is_none() && !m.alt {
            let primary = m.control || m.platform;
            if k.key == "f3" && !primary && self.search.is_some() {
                self.step_hit(!m.shift, cx);
                cx.stop_propagation();
                return;
            }
        }
        if !unmodified(event.keystroke.modifiers) {
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
        if let Some(to) = ListMove::from_key(&event.keystroke.key) {
            self.step(to, cx);
            cx.stop_propagation();
            return;
        }
        match event.keystroke.key.as_str() {
            "enter" if self.wip_selected => self.open_first_change(cx),
            "enter" if self.stash_selected.is_some() => {
                if let Some(oid) = self.stash_selected {
                    self.stash_op(oid, Some(StashApplyMode::Keep), cx);
                }
            }
            "escape" if !self.multi.is_empty() => {
                self.multi.clear();
                cx.notify();
            }
            "escape" if self.search.is_some() => self.close_search(window, cx),
            "enter" => match self.selected.and_then(Selected::placed) {
                Some((row, oid)) => self.checkout_row(row, oid, cx),
                None => return,
            },
            _ => return,
        }
        cx.stop_propagation();
    }

    /// Selects the commit `oid` and scrolls to it (refs panel). The row is
    /// found once the topology is complete, without holding the view lock.
    /// Finds the selected commit again in a just-loaded graph: commits
    /// added or removed move it to another row. It is shown unplaced (no
    /// row highlighted, no row action) until found, and unselected when
    /// the new history no longer has it.
    pub(super) fn place_selection(&mut self, cx: &mut Context<Self>) {
        let run = self.loads.placing.begin();
        let (Some(selected), Some(graph)) = (self.selected, self.graph.clone()) else {
            return;
        };
        let oid = selected.oid;
        let unmoved = selected.row.is_some_and(|row| {
            lock(&graph)
                .oids(row..row + 1)
                .is_ok_and(|o| o.first() == Some(&oid))
        });
        if unmoved {
            return;
        }
        self.selected = Some(Selected { oid, row: None });
        let task = cx.background_spawn(async move { find_row(&graph, oid) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |v, cx| {
                if !v.loads.placing.is_current(run) || v.selected.is_none_or(|s| s.oid != oid) {
                    return; // reloaded or selected elsewhere meanwhile
                }
                match result {
                    Ok(Some(row)) => v.selected = Some(Selected::at(row, oid)),
                    // Gone from the history (amended, rebased, reset away).
                    Ok(None) => {
                        v.selected = None;
                        v.details = None;
                        v.message = None;
                        v.reword = None;
                        if matches!(v.open_diff, Some(OpenDiff::Commit(..))) {
                            v.open_diff = None;
                        }
                    }
                    Err(e) if e.is_cancelled() => {}
                    Err(e) => v.status = Some(e.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn jump_to(&mut self, oid: Oid, cx: &mut Context<Self>) {
        let Some(graph) = self.graph.clone() else {
            return;
        };
        let source = Arc::clone(&graph);
        let task = cx.background_spawn(async move { find_row(&graph, oid) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |v, cx| match result {
                // The row is of a graph reloaded meanwhile.
                _ if !v.graph.as_ref().is_some_and(|g| Arc::ptr_eq(g, &source)) => {}
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
    pub(super) fn render_open_diff(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (title, note, side, doc) = match &self.open_diff {
            Some(OpenDiff::Commit(file, doc)) => {
                let title = match file {
                    Some(_) => doc.files.first().map(file_title).unwrap_or_default(),
                    None => format!("All files ({})", doc.files.len()),
                };
                let at = match self.compared() {
                    Some((older, newer)) => Some(format!(
                        "{}^..{}",
                        older.short(SHORT_OID),
                        newer.short(SHORT_OID)
                    )),
                    None => self.details.as_ref().map(|(oid, _)| oid.short(SHORT_OID)),
                };
                (title, at.unwrap_or_default(), None, Some(Arc::clone(doc)))
            }
            Some(OpenDiff::Worktree(d)) => (
                d.file.clone(),
                diff_view::side_note(d.side).to_owned(),
                Some(d.side),
                d.doc.clone(),
            ),
            Some(OpenDiff::File(_)) | None => return div().into_any_element(),
        };
        diff_view::render_open(self, title, &note, side, doc, window, cx)
    }
}

/// The row of commit `oid` in `graph`, once its topology is complete; read
/// off the UI thread, without holding the lock while the build finishes.
fn find_row(graph: &SharedView, oid: Oid) -> Result<Option<u32>, GraphError> {
    let completion = lock(graph).completion();
    if let Some(c) = completion {
        c.wait(&NoProgress)?;
    }
    let searcher = lock(graph).searcher()?;
    searcher.row_of(oid, &CancelToken::new())
}
