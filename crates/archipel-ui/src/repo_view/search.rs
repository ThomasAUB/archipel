//! Find in the commit graph (`Ctrl+F`): a bar above the graph searching
//! the whole history in the background, by commit id prefix, full message
//! (summary and body) or author. Enter / ↓ / F3 go to the next match,
//! Shift+Enter / ↑ / Shift+F3 to the previous one, Esc closes; matching
//! rows are tinted in the graph, and the matched text is highlighted in
//! the summaries and in the details pane's message.

use std::ops::Range;
use std::sync::Arc;
use std::time::Duration;

use archipel_graph::{GraphError, SearchQuery, SearchResult};
use archipel_model::{CancelToken, NoProgress};
use archipel_theme::ColorToken as T;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, App, Context, Entity, HighlightStyle, KeyDownEvent, ScrollStrategy, SharedString,
    StyledText, Subscription, Window, div, px,
};

use crate::keys::fixed_label;
use crate::latest::{Latest, Ticket};
use crate::staging;
use crate::theme::color;
use crate::widgets::{TextTooltip, button, unmodified};

use super::{RepoView, SharedView, lock};

/// Wait after a keystroke before searching, so typing runs one search.
const DEBOUNCE: Duration = Duration::from_millis(150);

/// The find bar and its last results.
pub(super) struct GraphSearch {
    input: Entity<InputState>,
    _sub: Subscription,
    runs: Latest,
    /// Cancels the running search when a newer one starts.
    cancel: CancelToken,
    /// Matching graph rows, top to bottom.
    hits: Vec<u32>,
    /// Index in `hits` of the match last gone to.
    current: Option<usize>,
    /// The search stopped at its hit limit.
    truncated: bool,
    searching: bool,
    error: Option<String>,
}

impl GraphSearch {
    fn query(&self, cx: &gpui_kit::App) -> String {
        self.input.read(cx).value().trim().to_owned()
    }
}

/// What a search finds: the matches and the rows searched.
type Found = Result<(SearchResult, u32), GraphError>;

impl RepoView {
    /// Opens the find bar, or focuses it and selects its text when open.
    pub(super) fn open_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(s) = &self.search {
            s.input.update(cx, |i, cx| {
                i.focus(window, cx);
                i.select_all(window, cx);
            });
            return;
        }
        let input = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Find a commit: message, author or id…")
        });
        let sub = cx.subscribe_in(
            &input,
            window,
            |v: &mut Self, _, event: &InputEvent, _, cx| {
                if let InputEvent::Change = event {
                    v.run_search(true, cx);
                }
            },
        );
        input.update(cx, |i, cx| i.focus(window, cx));
        self.search = Some(GraphSearch {
            input,
            _sub: sub,
            runs: Latest::default(),
            cancel: CancelToken::new(),
            hits: Vec::new(),
            current: None,
            truncated: false,
            searching: false,
            error: None,
        });
        cx.notify();
    }

    /// Ctrl+F anywhere in the tab, once the focused element let it pass
    /// (an open file has its own find): opens the find bar.
    pub(super) fn on_find_key(
        &mut self,
        e: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let m = e.keystroke.modifiers;
        if e.keystroke.key == "f"
            && (m.control || m.platform)
            && !m.shift
            && !m.alt
            && self.open_diff.is_none()
        {
            self.open_search(window, cx);
            cx.stop_propagation();
        }
    }

    /// Gives the keyboard to the graph (the tab was just shown), so its
    /// keys and Ctrl+F work without a click first.
    pub fn focus_graph(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.focus.focus(window, cx);
    }

    /// Closes the find bar and gives the keyboard back to the graph.
    pub(super) fn close_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(s) = self.search.take() {
            s.cancel.cancel();
        }
        self.focus.focus(window, cx);
        cx.notify();
    }

    /// Searches the history again for the query (the graph reloaded): the
    /// matches are updated without moving the selection.
    pub(super) fn refresh_search(&mut self, cx: &mut Context<Self>) {
        self.run_search(false, cx);
    }

    /// Searches for the typed text, after a short pause when `jump` (typing),
    /// then goes to the first match from the selected commit down when `jump`.
    fn run_search(&mut self, jump: bool, cx: &mut Context<Self>) {
        let graph = self.graph.clone();
        let Some(s) = &mut self.search else {
            return;
        };
        let text = s.query(cx);
        s.cancel.cancel();
        let cancel = CancelToken::new();
        s.cancel = cancel.clone();
        let run = s.runs.begin();
        s.error = None;
        let Some(graph) = graph.filter(|_| !text.is_empty()) else {
            s.hits.clear();
            s.current = None;
            s.truncated = false;
            s.searching = false;
            cx.notify();
            return;
        };
        s.searching = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            if jump {
                cx.background_executor().timer(DEBOUNCE).await;
                let current = this
                    .update(cx, |v, _| {
                        v.search.as_ref().is_some_and(|s| s.runs.is_current(run))
                    })
                    .unwrap_or(false);
                if !current {
                    return; // typed on meanwhile
                }
            }
            let source = Arc::clone(&graph);
            let found = cx
                .background_executor()
                .spawn(async move { search_graph(&graph, &text, &cancel) })
                .await;
            let _ = this.update(cx, |v, cx| v.land_search(run, &source, found, jump, cx));
        })
        .detach();
    }

    fn land_search(
        &mut self,
        run: Ticket,
        source: &SharedView,
        found: Found,
        jump: bool,
        cx: &mut Context<Self>,
    ) {
        if !self.graph.as_ref().is_some_and(|g| Arc::ptr_eq(g, source)) {
            return; // reloaded meanwhile: the reload searches again
        }
        let at = self.selected.and_then(|s| s.row);
        let Some(s) = &mut self.search else {
            return;
        };
        if !s.runs.is_current(run) {
            return;
        }
        s.searching = false;
        let (found, rows) = match found {
            Ok(found) => found,
            Err(e) if e.is_cancelled() => return,
            Err(e) => {
                s.error = Some(e.to_string());
                s.hits.clear();
                s.current = None;
                cx.notify();
                return;
            }
        };
        s.truncated = found.truncated;
        s.hits = found.rows;
        s.current = if jump {
            // The first match from the selected commit down, wrapping.
            let from = at.map_or(0, |r| s.hits.partition_point(|&h| h < r));
            (!s.hits.is_empty()).then(|| from % s.hits.len())
        } else {
            at.and_then(|r| s.hits.binary_search(&r).ok())
        };
        let target = s.current.filter(|_| jump);
        // The search waited for the whole topology, which may not be
        // counted yet.
        self.row_count = self.row_count.max(rows);
        if let Some(i) = target {
            self.go_to_hit(i, cx);
        }
        cx.notify();
    }

    /// Selects match `i` and scrolls it to the middle of the graph.
    fn go_to_hit(&mut self, i: usize, cx: &mut Context<Self>) {
        let Some(s) = &mut self.search else {
            return;
        };
        let Some(&row) = s.hits.get(i) else {
            return;
        };
        s.current = Some(i);
        let oid = self
            .graph
            .as_ref()
            .and_then(|g| lock(g).oids(row..row + 1).ok())
            .and_then(|o| o.first().copied());
        if let Some(oid) = oid {
            self.show_row(row, oid, ScrollStrategy::Center, cx);
        }
        cx.notify();
    }

    /// Goes to the next match (`forward`) or the previous one, counted from
    /// the selected commit when it is not the current match.
    pub(super) fn step_hit(&mut self, forward: bool, cx: &mut Context<Self>) {
        let at = self.selected.and_then(|s| s.row);
        let Some(s) = &self.search else {
            return;
        };
        let len = s.hits.len();
        if len == 0 {
            return;
        }
        let current = s.current.filter(|&c| at == s.hits.get(c).copied());
        let next = match (current, at) {
            (Some(c), _) if forward => (c + 1) % len,
            (Some(c), _) => (c + len - 1) % len,
            (None, None) if forward => 0,
            (None, None) => len - 1,
            (None, Some(r)) if forward => s.hits.partition_point(|&h| h <= r) % len,
            (None, Some(r)) => (s.hits.partition_point(|&h| h < r) + len - 1) % len,
        };
        self.go_to_hit(next, cx);
    }

    /// `text` with the find bar's query highlighted in it, while the bar is
    /// open (plain text otherwise).
    pub(super) fn highlighted(&self, text: impl Into<SharedString>, cx: &App) -> StyledText {
        let text: SharedString = text.into();
        let query = self
            .search
            .as_ref()
            .map(|s| s.query(cx).to_lowercase())
            .unwrap_or_default();
        let ranges = find_ci(&text, &query);
        let style = HighlightStyle {
            background_color: Some(color(cx, T::StatusWarning).opacity(0.4)),
            ..HighlightStyle::default()
        };
        StyledText::new(text).with_highlights(ranges.into_iter().map(move |r| (r, style)))
    }

    /// Whether graph row `row` matches the search.
    pub(super) fn is_hit(&self, row: u32) -> bool {
        self.search
            .as_ref()
            .is_some_and(|s| s.hits.binary_search(&row).is_ok())
    }

    /// Keys of the find bar. The input lets Enter, Esc and the arrows
    /// through: they must not reach the graph (Enter checks out).
    fn on_search_key(&mut self, e: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let k = &e.keystroke;
        let m = k.modifiers;
        let plain = !(m.control || m.platform || m.alt);
        match k.key.as_str() {
            "escape" if plain => self.close_search(window, cx),
            "enter" | "f3" if plain => self.step_hit(!m.shift, cx),
            "down" if unmodified(m) => self.step_hit(true, cx),
            "up" if unmodified(m) => self.step_hit(false, cx),
            _ => return,
        }
        cx.stop_propagation();
    }

    /// The find bar above the graph: the query, the match count and the
    /// previous / next / close buttons.
    pub(super) fn render_search(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let s = self.search.as_ref()?;
        let n = s.hits.len();
        let more = if s.truncated { "+" } else { "" };
        let count = if let Some(e) = &s.error {
            e.clone()
        } else if s.searching {
            "Searching…".to_owned()
        } else if s.query(cx).is_empty() {
            String::new()
        } else if n == 0 {
            "No match".to_owned()
        } else {
            match s.current {
                Some(c) => format!("{} of {n}{more}", c + 1),
                None => format!("{n}{more} matches"),
            }
        };
        let tip = |text: String| {
            move |_: &mut Window, cx: &mut gpui_kit::App| {
                TextTooltip::view(std::slice::from_ref(&text), cx)
            }
        };
        Some(
            div()
                .id("graph-search")
                .flex_none()
                .flex()
                .items_center()
                .gap_2()
                .px_2()
                .py_1()
                .border_b_1()
                .border_color(color(cx, T::BorderSubtle))
                .on_key_down(cx.listener(|v, e: &KeyDownEvent, window, cx| {
                    v.on_search_key(e, window, cx);
                }))
                .child(div().flex_1().min_w(px(0.0)).child(Input::new(&s.input)))
                .child(
                    div()
                        .flex_none()
                        .text_xs()
                        .text_color(color(cx, T::FgMuted))
                        .child(count),
                )
                .child(
                    staging::small(button("search-prev", "↑", cx))
                        .tooltip(tip(format!(
                            "Previous match ({})",
                            fixed_label("shift-enter")
                        )))
                        .on_click(cx.listener(|v, _, _, cx| v.step_hit(false, cx))),
                )
                .child(
                    staging::small(button("search-next", "↓", cx))
                        .tooltip(tip(format!("Next match ({})", fixed_label("enter"))))
                        .on_click(cx.listener(|v, _, _, cx| v.step_hit(true, cx))),
                )
                .child(
                    staging::small(button("search-close", "×", cx))
                        .tooltip(tip(format!("Close ({})", fixed_label("escape"))))
                        .on_click(cx.listener(|v, _, window, cx| v.close_search(window, cx))),
                )
                .into_any_element(),
        )
    }
}

/// Searches `graph` for `text` once its topology is complete; off the UI
/// thread, without holding the lock while the build finishes.
fn search_graph(graph: &SharedView, text: &str, cancel: &CancelToken) -> Found {
    let completion = lock(graph).completion();
    if let Some(c) = completion {
        c.wait(&NoProgress)?;
    }
    let searcher = lock(graph).searcher()?;
    let found = searcher.search(&SearchQuery::new(text), cancel)?;
    Ok((found, searcher.row_count()))
}

/// Byte ranges of `needle_lower` (lowercase) in `text`, case-insensitive,
/// not overlapping. Empty for an empty needle.
fn find_ci(text: &str, needle_lower: &str) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    if needle_lower.is_empty() {
        return out;
    }
    let mut from = 0;
    for (start, _) in text.char_indices() {
        if start < from {
            continue;
        }
        if let Some(end) = match_at(text, start, needle_lower) {
            out.push(start..end);
            from = end;
        }
    }
    out
}

/// End of the match of `needle_lower` starting at byte `start` of `text`,
/// comparing lowercase characters.
fn match_at(text: &str, start: usize, needle_lower: &str) -> Option<usize> {
    let mut needle = needle_lower.chars().peekable();
    for (i, c) in text[start..].char_indices() {
        for lower in c.to_lowercase() {
            needle.next_if_eq(&lower)?;
        }
        if needle.peek().is_none() {
            return Some(start + i + c.len_utf8());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::find_ci;

    #[test]
    fn finds_every_match_case_insensitively() {
        assert_eq!(find_ci("Fix bug, then FIX docs", "fix"), vec![0..3, 14..17]);
        assert_eq!(find_ci("aaaa", "aa"), vec![0..2, 2..4]);
        assert_eq!(find_ci("Closes #42", "#42"), vec![7..10]);
        assert!(find_ci("anything", "").is_empty());
        assert!(find_ci("short", "shorter").is_empty());
    }

    #[test]
    fn ranges_fall_on_character_boundaries() {
        let text = "Écrire le Résumé";
        let ranges = find_ci(text, "résumé");
        assert_eq!(ranges.len(), 1);
        assert_eq!(&text[ranges[0].clone()], "Résumé");
        assert_eq!(&text[find_ci(text, "écr")[0].clone()], "Écr");
    }
}
