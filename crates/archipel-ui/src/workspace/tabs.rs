//! The tab strip (dashboard and repo tabs, reordered by drag and drop)
//! and the repo view of the active tab.

use std::collections::HashMap;

use archipel_app::Command;
use archipel_model::RepoId;
use archipel_model::ui_state::TabState;
use archipel_theme::ColorToken as T;
use gpui_kit::prelude::*;
use gpui_kit::{
    Context, Entity, MouseButton, MouseDownEvent, MouseUpEvent, SharedString, Window, div, px,
};

use crate::repo_view::RepoView;
use crate::staging::StagingEvent;
use crate::theme::{color, radius};

use super::Workspace;
use super::menus::Menu;

/// Drag payload of a repo tab: its index.
struct TabDrag(usize);

/// The tab label following the pointer while a tab is dragged.
struct TabGhost(SharedString);

impl Render for TabGhost {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_3()
            .py_1()
            .rounded(radius(cx))
            .whitespace_nowrap()
            .bg(color(cx, T::BgElevated))
            .border_1()
            .border_color(color(cx, T::Accent))
            .text_color(color(cx, T::FgDefault))
            .shadow_md()
            .child(self.0.clone())
    }
}

impl Workspace {
    pub(super) fn render_tabs(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = self.core.ui_state().unwrap_or_default();
        let names: HashMap<RepoId, (String, Option<String>)> = self
            .core
            .rows()
            .into_iter()
            .map(|r| {
                // A worktree: `libfoo · feat/parser` with the glyph, its
                // folder on hover (spec §10).
                let path = r
                    .is_worktree()
                    .then(|| r.path.as_path().display().to_string());
                let label = match &r.worktree {
                    Some(_) => format!("{} {}", archipel_app::dashboard::WORKTREE_GLYPH, r.label()),
                    None => r.name.clone(),
                };
                (r.id, (label, path))
            })
            .collect();
        let tabs = ui.tabs.iter().enumerate().map(|(i, tab)| {
            let (label, hover) = match tab {
                TabState::Dashboard => ("Dashboard".to_owned(), None),
                TabState::Repo(id) => names
                    .get(id)
                    .cloned()
                    .unwrap_or_else(|| ("repo".into(), None)),
            };
            let active = i == ui.active_tab;
            let mut el = div()
                .id(SharedString::from(format!("tab-{i}")))
                .flex()
                .items_center()
                .gap_2()
                .px_3()
                .h_full()
                .flex_shrink_0()
                .whitespace_nowrap()
                .cursor_pointer()
                .border_b_2()
                .border_color(if active {
                    color(cx, T::Accent)
                } else {
                    color(cx, T::BgApp)
                })
                // Inactive labels stay at full text color: the accent underline
                // alone marks the active tab.
                .text_color(color(cx, T::FgDefault))
                .on_click(cx.listener(move |this, _, _, cx| {
                    let () = this.core.update_ui_state(|u| u.active_tab = i);
                    cx.notify();
                }))
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener({
                        let tab = tab.clone();
                        move |this, e: &MouseDownEvent, _, cx| {
                            this.open_menu(Menu::Tab(tab.clone()), e.position, cx);
                        }
                    }),
                )
                // Every tab takes drops (on the dashboard: becomes the first
                // repo tab); the side bar shows where the dragged tab lands.
                .drag_over::<TabDrag>(move |s, d, _, cx| {
                    let s = s.bg(color(cx, T::BgHover));
                    if d.0 < i {
                        s.border_r_2().border_color(color(cx, T::Accent))
                    } else {
                        s.border_l_2().border_color(color(cx, T::Accent))
                    }
                })
                .on_drop(cx.listener(move |this, d: &TabDrag, _, cx| {
                    this.dispatch(Command::MoveTab { from: d.0, to: i }, cx);
                }))
                .when_some(hover, |d, path| {
                    d.tooltip(move |_, cx| {
                        crate::widgets::TextTooltip::view(std::slice::from_ref(&path), cx)
                    })
                })
                .child(label.clone());
            if i > 0 {
                let label = SharedString::from(label);
                el = el
                    .on_drag(TabDrag(i), move |_, _, _, cx| {
                        cx.new(|_| TabGhost(label.clone()))
                    })
                    // Middle click closes a repo tab, as in a browser.
                    .on_mouse_up(
                        MouseButton::Middle,
                        cx.listener(move |this, _: &MouseUpEvent, _, cx| {
                            this.dispatch(Command::CloseTab(i), cx);
                        }),
                    );
                el = el.child(
                    div()
                        .id(SharedString::from(format!("tab-close-{i}")))
                        .text_color(color(cx, T::FgMuted))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            // Stop the click bubbling to the tab itself, whose handler
                            // would otherwise activate index `i` — now the next tab.
                            cx.stop_propagation();
                            this.dispatch(Command::CloseTab(i), cx);
                        }))
                        .child("×"),
                );
            }
            el
        });
        div()
            .h(px(34.0))
            .flex()
            .flex_none()
            .items_end()
            .border_b_1()
            .border_color(color(cx, T::BorderSubtle))
            .children(tabs)
    }

    pub(super) fn active_repo_view(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<RepoView>> {
        let ui = self.core.ui_state()?;
        let Some(TabState::Repo(id)) = ui.tabs.get(ui.active_tab) else {
            self.focused_tab = None;
            return None;
        };
        let view = if let Some(view) = self.repo_views.get(id) {
            view.clone()
        } else {
            let row = self.core.row(*id)?;
            let core = self.core.clone();
            let view = cx.new(|cx| RepoView::new(core, row.path.clone(), row.label(), window, cx));
            // Lives as long as the view.
            cx.subscribe_in(&view, window, Self::on_staging).detach();
            self.repo_views.insert(*id, view.clone());
            view
        };
        // A tab just shown takes the keyboard, once no overlay holds it.
        if self.focused_tab != Some(*id) && self.overlay.is_none() {
            self.focused_tab = Some(*id);
            view.update(cx, |v, cx| v.focus_graph(window, cx));
        }
        Some(view)
    }

    /// The staging panel of a repo tab or of the quick edit pane asks for
    /// something only the workspace can show.
    pub(super) fn on_staging<V>(
        &mut self,
        _: &Entity<V>,
        event: &StagingEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            StagingEvent::OpenForge(tab) => self.open_forge(tab.clone(), window, cx),
            StagingEvent::OpenRepo(id) => self.dispatch(Command::OpenRepoTab(*id), cx),
            StagingEvent::NewWorktree(repo, start) => {
                self.open_new_worktree(*repo, start, window, cx);
            }
            StagingEvent::RemoveWorktree(id) => self.open_remove_worktree(*id, window, cx),
            StagingEvent::SwitchWorktree => {
                self.open_palette(super::overlays::PaletteKind::Worktrees, window, cx);
            }
            StagingEvent::WorktreeMenu(id, position) => {
                self.open_menu(Menu::Row(*id), *position, cx);
            }
            StagingEvent::ChangeRemoteUrl(id, branch) => {
                self.change_remote_url(*id, branch.clone(), window, cx);
            }
        }
    }
}
