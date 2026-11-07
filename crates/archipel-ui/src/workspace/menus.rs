//! Right-click menus of the window: repo row, sidebar category header and
//! tab.

use archipel_app::Command;
use archipel_model::RepoId;
use archipel_theme::ColorToken as T;
use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, Context, div};

use crate::forge_panel::ForgeTab;
use crate::theme::color;
use crate::widgets::{context_menu, menu_item, menu_separator, popup_at};

use super::Workspace;
use super::overlays::PaletteKind;

impl Workspace {
    /// Row menu entry: open the repo's pull request, or create pull requests
    /// for the targeted repos (spec §8).
    fn forge_menu_item(
        &self,
        id: RepoId,
        targets: Vec<RepoId>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let hover = color(cx, T::BgHover);
        let open = self
            .core
            .rows()
            .into_iter()
            .find(|r| r.id == id)
            .and_then(|r| r.forge.pull_request().map(|pr| (pr.number, pr.url.clone())));
        let base = div()
            .id("menu-forge")
            .px_3()
            .py_1()
            .cursor_pointer()
            .hover(move |s| s.bg(hover));
        match open {
            Some((number, url)) => base
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.row_menu = None;
                    cx.open_url(&url);
                    cx.notify();
                }))
                .child(format!("Open pull request #{number}"))
                .into_any_element(),
            None => base
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.row_menu = None;
                    this.open_forge(ForgeTab::PullRequests(targets.clone()), window, cx);
                }))
                .child("Create pull request…")
                .into_any_element(),
        }
    }

    /// Row menu entry: open the CI run of the repo's branch, when it has one.
    fn ci_menu_item(&self, id: RepoId, cx: &mut Context<Self>) -> Option<AnyElement> {
        let hover = color(cx, T::BgHover);
        let url = self
            .core
            .rows()
            .into_iter()
            .find(|r| r.id == id)
            .and_then(|r| r.ci.run().and_then(|run| run.url.clone()))?;
        Some(
            div()
                .id("menu-ci")
                .px_3()
                .py_1()
                .cursor_pointer()
                .hover(move |s| s.bg(hover))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.row_menu = None;
                    cx.open_url(&url);
                    cx.notify();
                }))
                .child("Open CI run")
                .into_any_element(),
        )
    }

    /// Right-click menu of a sidebar category header: rename, remove.
    pub(super) fn render_category_menu(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (category, position) = self.category_menu.clone()?;
        let hover = color(cx, T::BgHover);
        let rename = category.clone();
        let menu = context_menu(cx)
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                this.category_menu = None;
                cx.notify();
            }))
            .child(
                div()
                    .id("category-menu-rename")
                    .px_3()
                    .py_1()
                    .cursor_pointer()
                    .hover(move |s| s.bg(hover))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.category_menu = None;
                        this.renaming_category = Some(rename.clone());
                        this.open_palette(PaletteKind::RenameCategory, window, cx);
                    }))
                    .child("Rename category…"),
            )
            .child(
                div()
                    .id("category-menu-remove")
                    .px_3()
                    .py_1()
                    .cursor_pointer()
                    .text_color(color(cx, T::StatusError))
                    .hover(move |s| s.bg(hover))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.category_menu = None;
                        this.dispatch(Command::RenameCategory(category.clone(), None), cx);
                    }))
                    .child("Remove category (repos stay in the project)"),
            );
        Some(popup_at(position, menu).into_any_element())
    }

    /// Right-click menu of a tab: close it, the others, those to its right, all.
    pub(super) fn render_tab_menu(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (index, position) = self.tab_menu?;
        let count = self.core.ui_state()?.tabs.len();
        let mut entries = Vec::new();
        if index > 0 {
            entries.push(("Close", Command::CloseTab(index)));
            entries.push(("Close others", Command::CloseOtherTabs(index)));
        }
        if index + 1 < count {
            entries.push(("Close tabs to the right", Command::CloseTabsToRight(index)));
        }
        let rows = entries.into_iter().enumerate().map(|(k, (label, cmd))| {
            menu_item(("tab-menu", k), label, false, cx)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.tab_menu = None;
                    this.dispatch(cmd.clone(), cx);
                }))
                .into_any_element()
        });
        let menu = context_menu(cx)
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                this.tab_menu = None;
                cx.notify();
            }))
            .children(rows)
            .when(index > 0 || index + 1 < count, |m| {
                m.child(menu_separator(cx))
            })
            .child(
                menu_item("tab-menu-all", "Close all", false, cx).on_click(cx.listener(
                    |this, _, _, cx| {
                        this.tab_menu = None;
                        this.dispatch(Command::CloseAllTabs, cx);
                    },
                )),
            );
        Some(popup_at(position, menu).into_any_element())
    }

    /// The context menu of a repo row (right click).
    #[allow(clippy::too_many_lines)] // declarative element tree
    pub(super) fn render_row_menu(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (id, position) = self.row_menu?;
        let ui = self.core.ui_state()?;
        // Acting on a selected row acts on the whole selection.
        let selection = ui.batch_selection();
        let targets = if selection.contains(&id) {
            selection
        } else {
            vec![id]
        };
        let all_hidden = targets.iter().all(|r| ui.is_hidden(*r));
        let n = targets.len();
        let hover = color(cx, T::BgHover);
        let subtle = color(cx, T::FgSubtle);
        let item = |eid: &'static str,
                    label: String,
                    shortcut: Option<&'static str>,
                    command: Command,
                    cx: &mut Context<Self>| {
            div()
                .id(eid)
                .flex()
                .justify_between()
                .gap_6()
                .px_3()
                .py_1()
                .cursor_pointer()
                .hover(move |s| s.bg(hover))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.row_menu = None;
                    this.dispatch(command.clone(), cx);
                }))
                .child(label)
                .children(shortcut.map(|k| div().text_color(subtle).child(k)))
        };
        let plural = if n > 1 {
            format!(" {n} repos")
        } else {
            String::new()
        };
        let (hide_label, hide_shortcut) = if all_hidden {
            (format!("Unhide{plural}"), None)
        } else {
            (format!("Hide{plural}"), Some("Ctrl+H"))
        };
        let menu = context_menu(cx)
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                this.row_menu = None;
                cx.notify();
            }))
            .child(item(
                "menu-open",
                "Open".into(),
                None,
                Command::OpenRepoTab(id),
                cx,
            ))
            .child(item(
                "menu-hide",
                hide_label,
                hide_shortcut,
                Command::ToggleHidden(targets.clone()),
                cx,
            ))
            .child({
                let targets = targets.clone();
                div()
                    .id("menu-category")
                    .px_3()
                    .py_1()
                    .cursor_pointer()
                    .hover(move |s| s.bg(hover))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.row_menu = None;
                        this.set_category(targets.clone(), window, cx);
                    }))
                    .child(if n > 1 {
                        format!("Set category of{plural}…")
                    } else {
                        "Set category…".to_owned()
                    })
            })
            .child(item(
                "menu-fetch",
                format!("Fetch{plural}"),
                None,
                Command::Fetch(targets.clone()),
                cx,
            ))
            .child(self.forge_menu_item(id, targets, cx))
            .children(self.ci_menu_item(id, cx))
            .child(
                div()
                    .id("menu-remove")
                    .px_3()
                    .py_1()
                    .cursor_pointer()
                    .text_color(color(cx, T::StatusError))
                    .hover(move |s| s.bg(hover))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.row_menu = None;
                        if let Err(e) = this.core.remove_repo(id) {
                            this.notice = Some(e.to_string());
                        }
                        cx.notify();
                    }))
                    .child("Remove from project"),
            );
        Some(popup_at(position, menu).into_any_element())
    }
}
