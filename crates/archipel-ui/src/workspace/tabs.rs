//! The tab strip (dashboard and repo tabs, reordered by drag and drop)
//! and the repo view of the active tab.

use std::collections::HashMap;

use archipel_app::Command;
use archipel_model::RepoId;
use archipel_model::ui_state::TabState;
use archipel_theme::ColorToken as T;
use gpui_kit::prelude::*;
use gpui_kit::{Context, Entity, MouseButton, MouseDownEvent, SharedString, Window, div, px};

use crate::repo_view::RepoView;
use crate::staging::StagingEvent;
use crate::theme::{color, radius};

use super::Workspace;

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
        let names: HashMap<RepoId, String> = self
            .core
            .rows()
            .into_iter()
            .map(|r| (r.id, r.name))
            .collect();
        let tabs = ui.tabs.iter().enumerate().map(|(i, tab)| {
            let label = match tab {
                TabState::Dashboard => "Dashboard".to_owned(),
                TabState::Repo(id) => names.get(id).cloned().unwrap_or_else(|| "repo".into()),
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
                    cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                        this.tab_menu = Some((i, e.position));
                        cx.notify();
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
                .child(label.clone());
            if i > 0 {
                let label = SharedString::from(label);
                el = el.on_drag(TabDrag(i), move |_, _, _, cx| {
                    cx.new(|_| TabGhost(label.clone()))
                });
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
        let TabState::Repo(id) = ui.tabs.get(ui.active_tab)? else {
            return None;
        };
        if let Some(view) = self.repo_views.get(id) {
            return Some(view.clone());
        }
        let row = self.core.rows().into_iter().find(|r| r.id == *id)?;
        let core = self.core.clone();
        let view = cx.new(|cx| RepoView::new(core, row.path, row.name, window, cx));
        // Lives as long as the view.
        cx.subscribe_in(&view, window, Self::on_staging).detach();
        self.repo_views.insert(*id, view.clone());
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
        }
    }
}
