//! The Conventional Commits helper of the staging panel's commit box
//! (spec §5): type chips setting the `type(scope)!: ` header, scope
//! suggestions, and a lint line under the box that only warns.
//!
//! The repo's recent commit subjects tell whether it follows the
//! convention (`conventional_commits = "auto"`) and which scopes it uses;
//! they are read off the UI thread when `HEAD` moves and cached per repo.

use std::collections::HashMap;
use std::ops::ControlFlow;
use std::sync::{Arc, LazyLock, Mutex};

use archipel_app::message::{
    self, Lint, clear_type, conventional_enabled, conventional_types, follows_convention,
    history_scopes, parse_header, set_scope, set_type, suggest_scopes, toggle_breaking,
};
use archipel_model::history::WalkOrder;
use archipel_model::ports::HistoryReader;
use archipel_model::project::ConventionalCommits;
use archipel_model::{CancelToken, Oid, RepoPath};
use archipel_theme::ColorToken as T;
use gpui_kit::component::input::Position;
use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, Context, SharedString, Window, div};

use crate::staging::Staging;
use crate::theme::color;
use crate::widgets::chip;

/// Recent commits read to detect the convention and the scopes in use.
const RECENT: usize = 100;

/// Scope chips offered at most.
const MAX_SCOPES: usize = 6;

/// Recent commit subjects of each repo (newest first), with the `HEAD`
/// they were read at.
type Recent = (Head, Arc<Vec<String>>);
static CACHE: LazyLock<Mutex<HashMap<RepoPath, Recent>>> = LazyLock::new(Mutex::default);

/// The commit `HEAD` resolves to (`None` when unborn).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Head(Option<Oid>);

/// The helper's state, held by the staging [`Panel`](crate::staging::Panel).
#[derive(Default)]
pub struct Conventional {
    /// The project's `conventional_commits`.
    mode: ConventionalCommits,
    /// The project's types.
    types: Vec<String>,
    /// Folder name of the repo (its main worktree's for a linked one).
    repo_name: String,
    /// The `HEAD` the history is being or was read at (`None`: not yet).
    reading: Option<Head>,
    /// Most recent commits follow the convention.
    detected: bool,
    /// Scopes used in the recent commits, most used first.
    scopes: Vec<String>,
}

impl Conventional {
    /// Whether the chips and the lint show.
    pub fn enabled(&self) -> bool {
        conventional_enabled(self.mode, self.detected)
    }

    fn learn(&mut self, subjects: &[String]) {
        self.detected = follows_convention(subjects, &self.types);
        self.scopes = history_scopes(subjects);
    }
}

/// Takes the project settings again and reads the repo's recent history
/// when `HEAD` moved since it was read (the summary was refreshed).
pub fn refresh<V: Staging>(v: &mut V, cx: &mut Context<V>) {
    let core = v.core().clone();
    let path = v.repo_path().clone();
    let settings = core
        .project()
        .map(|(_, def)| def.settings)
        .unwrap_or_default();
    let repo_name = core
        .row(path.id())
        .and_then(|r| r.worktree.map(|w| w.main_name))
        .unwrap_or_else(|| path.name());
    let head = Head(v.panel().summary().and_then(|s| s.head_oid));
    let c = &mut v.panel().conventional;
    let types = conventional_types(&settings);
    if c.mode != settings.conventional_commits || c.types != types {
        c.mode = settings.conventional_commits;
        c.types = types;
        c.reading = None; // the detection depends on the types
        cx.notify();
    }
    c.repo_name = repo_name;
    if c.mode == ConventionalCommits::Off || c.reading == Some(head) {
        return;
    }
    c.reading = Some(head);
    let cached = CACHE
        .lock()
        .ok()
        .and_then(|m| m.get(&path).cloned())
        .filter(|(at, _)| *at == head);
    if let Some((_, subjects)) = cached {
        c.learn(&subjects);
        cx.notify();
        return;
    }
    let history = core.history_reader();
    let task = cx.background_spawn(async move {
        let subjects = recent_subjects(history.as_ref(), &path);
        if let Ok(mut m) = CACHE.lock() {
            m.insert(path, (head, subjects.clone()));
        }
        subjects
    });
    cx.spawn(async move |this, cx| {
        let subjects = task.await;
        let _ = this.update(cx, |v: &mut V, cx| {
            let c = &mut v.panel().conventional;
            // HEAD moved again meanwhile: a newer read is on its way.
            if c.reading == Some(head) {
                c.learn(&subjects);
                cx.notify();
            }
        });
    })
    .detach();
}

/// The subjects of the [`RECENT`] newest commits of the repo, newest
/// first (none when it cannot be read).
fn recent_subjects(history: &dyn HistoryReader, path: &RepoPath) -> Arc<Vec<String>> {
    let mut oids = Vec::with_capacity(RECENT);
    let walked = history.walk_topology(
        path,
        WalkOrder::Date,
        &mut |node| {
            oids.push(node.oid);
            if oids.len() < RECENT {
                ControlFlow::Continue(())
            } else {
                ControlFlow::Break(())
            }
        },
        &CancelToken::new(),
    );
    let subjects = match walked.and_then(|()| history.commit_meta(path, &oids)) {
        Ok(metas) => metas.into_iter().map(|m| m.summary).collect(),
        Err(e) => {
            tracing::debug!(error = %e, "recent commits not read");
            Vec::new()
        }
    };
    Arc::new(subjects)
}

/// Replaces the commit message with `edit` of it (undoable), the caret
/// at the end of the subject line, so the description can be typed next.
fn edit_message<V: Staging>(
    v: &mut V,
    window: &mut Window,
    cx: &mut Context<V>,
    edit: impl FnOnce(&str) -> String,
) {
    let message = v.panel().message().clone();
    let text = edit(&message.read(cx).value());
    let column = text.lines().next().unwrap_or_default().chars().count();
    message.update(cx, |m, cx| {
        m.replace_all(text, window, cx);
        #[allow(clippy::cast_possible_truncation)] // a subject line
        m.set_cursor_position(Position::new(0, column as u32), window, cx);
    });
    cx.notify();
}

/// The chip row above the commit message: the types, `!` and the scopes
/// suggested once a type is set. `None` while the helper is hidden.
pub fn render_chips<V: Staging>(v: &mut V, cx: &mut Context<V>) -> Option<AnyElement> {
    if !v.panel().conventional.enabled() {
        return None;
    }
    let text = v.panel().message().read(cx).value().to_string();
    let staged: Vec<String> = v
        .changes()
        .iter()
        .filter(|c| c.is_staged())
        .map(|c| c.path.clone())
        .collect();
    let c = &v.panel().conventional;
    let header = parse_header(&text);
    let kind = header.map(|h| h.kind);
    let mut types: Vec<AnyElement> = c
        .types
        .iter()
        .map(|t| {
            let active = kind == Some(t.as_str());
            let ty = t.clone();
            chip(
                SharedString::from(format!("cc-type-{t}")),
                t.clone(),
                active,
                cx,
            )
            .on_click(cx.listener(move |v: &mut V, _, window, cx| {
                let ty = ty.clone();
                edit_message(v, window, cx, |m| {
                    if active {
                        clear_type(m)
                    } else {
                        set_type(m, &ty)
                    }
                });
            }))
            .into_any_element()
        })
        .collect();
    let mut scopes = Vec::new();
    if let Some(h) = header {
        types.push(
            chip("cc-breaking", "!", h.breaking, cx)
                .on_click(cx.listener(|v: &mut V, _, window, cx| {
                    edit_message(v, window, cx, toggle_breaking);
                }))
                .into_any_element(),
        );
        let mut offered = suggest_scopes(&staged, &c.repo_name, &c.scopes, MAX_SCOPES);
        if let Some(s) = h
            .scope
            .filter(|s| !s.is_empty() && !offered.iter().any(|o| o == s))
        {
            offered.insert(0, s.to_owned());
        }
        scopes = offered
            .into_iter()
            .map(|s| {
                let active = h.scope == Some(s.as_str());
                let label = format!("({s})");
                chip(
                    SharedString::from(format!("cc-scope-{s}")),
                    label,
                    active,
                    cx,
                )
                .on_click(cx.listener(move |v: &mut V, _, window, cx| {
                    let scope = (!active).then(|| s.clone());
                    edit_message(v, window, cx, |m| set_scope(m, scope.as_deref()));
                }))
                .into_any_element()
            })
            .collect();
    }
    Some(
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(div().flex().flex_wrap().gap_1().children(types))
            .when(!scopes.is_empty(), |d| {
                d.child(div().flex().flex_wrap().gap_1().children(scopes))
            })
            .into_any_element(),
    )
}

/// The muted line under the commit message listing the rules it breaks;
/// `None` while the helper is hidden or the message is fine.
pub fn render_lint<V: Staging>(v: &mut V, cx: &mut Context<V>) -> Option<AnyElement> {
    if !v.panel().conventional.enabled() {
        return None;
    }
    let text = v.panel().message().read(cx).value().to_string();
    let warnings = message::lint(&text, &v.panel().conventional.types);
    if warnings.is_empty() {
        return None;
    }
    let line = warnings
        .iter()
        .map(Lint::to_string)
        .collect::<Vec<_>>()
        .join(" · ");
    Some(
        div()
            .text_xs()
            .text_color(color(cx, T::FgMuted))
            .child(format!("⚠ {line}"))
            .into_any_element(),
    )
}
