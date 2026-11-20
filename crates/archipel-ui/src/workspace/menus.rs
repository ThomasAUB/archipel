//! Menus of the window: the right-click menus (repo row, sidebar category
//! header, tab) and the dropdowns (category filter, theme, Set Selector).
//!
//! One menu is open at a time, in [`Workspace::menu`] with what it acts on:
//! opening one closes the other, and Escape, a click outside or running an
//! action closes it.

use archipel_app::Command;
use archipel_model::RepoId;
use archipel_model::ui_state::TabState;
use archipel_theme::ColorToken as T;
use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, Context, MouseDownEvent, Pixels, Point, div};

use crate::forge_panel::ForgeTab;
use crate::theme::color;
use crate::widgets::{context_menu, menu_item, menu_separator, popup_at};

use super::Workspace;
use super::overlays::PaletteKind;

/// A menu of the window and what it acts on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Menu {
    /// Right-click menu of a repo or worktree row.
    Row(RepoId),
    /// Right-click menu of a sidebar category header.
    Category(String),
    /// Right-click menu of a tab: the tab itself, not its index, so it
    /// still acts on that tab when others close meanwhile.
    Tab(TabState),
    /// The dashboard's category filter dropdown.
    CategoryFilter,
    /// The status bar's theme picker.
    Theme,
    /// The dashboard's Set Selector dropdown.
    SetSelector,
}

impl Workspace {
    /// Opens `menu` at `position`, closing any other.
    pub(super) fn open_menu(
        &mut self,
        menu: Menu,
        position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        self.menu = Some((menu, position));
        self.menu_closed_at = None;
        cx.notify();
    }

    /// Closes the open menu, if any.
    pub(super) fn close_menu(&mut self, cx: &mut Context<Self>) {
        if self.menu.take().is_some() {
            cx.notify();
        }
    }

    /// A left mouse down on the button of dropdown `menu`: opens it, or
    /// closes it when open. A click outside a menu closes it before the
    /// button hears of the click, so the click that just closed it from
    /// its own button does not reopen it.
    pub(super) fn toggle_menu(&mut self, menu: Menu, e: &MouseDownEvent, cx: &mut Context<Self>) {
        let just_closed = self.menu_closed_at.take() == Some((menu.clone(), e.position));
        let open = self.menu.as_ref().is_some_and(|(m, _)| *m == menu);
        if just_closed || open {
            self.close_menu(cx);
        } else {
            self.open_menu(menu, e.position, cx);
        }
        cx.stop_propagation();
    }

    /// Where `menu` is open, if it is the open menu.
    pub(super) fn menu_position(&self, menu: &Menu) -> Option<Point<Pixels>> {
        self.menu
            .as_ref()
            .filter(|(m, _)| m == menu)
            .map(|(_, p)| *p)
    }

    /// `body` as the open menu's popup at `position`; a click outside it
    /// closes it.
    pub(super) fn menu_popup(
        position: Point<Pixels>,
        body: impl IntoElement,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let frame = div()
            .on_mouse_down_out(cx.listener(|this, e: &MouseDownEvent, _, cx| {
                if let Some((menu, _)) = this.menu.take() {
                    this.menu_closed_at = Some((menu, e.position));
                    cx.notify();
                }
            }))
            .child(body);
        popup_at(position, frame).into_any_element()
    }

    /// The open right-click menu, drawn over the window. The dropdowns are
    /// drawn by their button.
    pub(super) fn render_context_menu(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (menu, position) = self.menu.clone()?;
        let body = match menu {
            Menu::Row(id) => self.row_menu(id, cx)?,
            Menu::Category(category) => Self::category_menu(category, cx),
            Menu::Tab(tab) => self.tab_menu(&tab, cx)?,
            Menu::CategoryFilter | Menu::Theme | Menu::SetSelector => return None,
        };
        Some(Self::menu_popup(position, body, cx))
    }

    /// Row menu entry: open the repo's pull request, or create pull requests
    /// for the targeted repos (spec §8).
    fn forge_menu_item(
        &self,
        id: RepoId,
        targets: Vec<RepoId>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let open = self
            .core
            .row(id)
            .and_then(|r| r.forge.pull_request().map(|pr| (pr.number, pr.url.clone())));
        match open {
            Some((number, url)) => menu_item(
                "menu-forge",
                format!("Open pull request #{number}"),
                false,
                cx,
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.menu = None;
                cx.open_url(&url);
                cx.notify();
            }))
            .into_any_element(),
            None => menu_item("menu-forge", "Create pull request…", false, cx)
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.menu = None;
                    this.open_forge(ForgeTab::Create(targets.clone()), window, cx);
                }))
                .into_any_element(),
        }
    }

    /// Row menu entry for a repo whose current branch has an open pull
    /// request: cancel its armed auto-merge (and those of the other
    /// targeted repos), or open the pull request list to arm it with the
    /// merge choices.
    fn auto_merge_menu_item(
        &self,
        id: RepoId,
        targets: &[RepoId],
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let row = self.core.row(id)?;
        let pr = row.forge.pull_request()?;
        let number = pr.number;
        let armed = pr.auto_merge || row.auto_merge.as_ref().is_some_and(|w| w.number == number);
        let targets = targets.to_vec();
        Some(if armed {
            menu_item(
                "menu-auto-merge",
                format!("Cancel auto-merge of #{number}"),
                false,
                cx,
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.menu = None;
                this.dispatch(Command::CancelAutoMerge(targets.clone()), cx);
            }))
            .into_any_element()
        } else if pr.draft {
            return None;
        } else {
            menu_item(
                "menu-auto-merge",
                format!("Merge #{number} when CI passes…"),
                false,
                cx,
            )
            .on_click(cx.listener(move |this, _, window, cx| {
                this.menu = None;
                this.open_forge(ForgeTab::Open(targets.clone()), window, cx);
            }))
            .into_any_element()
        })
    }

    /// Row menu entry: open the CI run of the repo's branch, when it has one.
    fn ci_menu_item(&self, id: RepoId, cx: &mut Context<Self>) -> Option<AnyElement> {
        let url = self
            .core
            .row(id)
            .and_then(|r| r.ci.run().and_then(|run| run.url.clone()))?;
        Some(
            menu_item("menu-ci", "Open CI run", false, cx)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.menu = None;
                    cx.open_url(&url);
                    cx.notify();
                }))
                .into_any_element(),
        )
    }

    /// Row menu entry: rerun the failed CI jobs of the targeted repos whose
    /// CI fails, when there are any.
    fn ci_rerun_menu_item(&self, targets: &[RepoId], cx: &mut Context<Self>) -> Option<AnyElement> {
        let failing: Vec<RepoId> = targets
            .iter()
            .filter_map(|id| self.core.row(*id))
            .filter(|r| {
                r.ci.run()
                    .is_some_and(|run| run.failed_jobs().next().is_some())
            })
            .map(|r| r.id)
            .collect();
        if failing.is_empty() {
            return None;
        }
        let label = match failing.len() {
            1 => "Rerun failed jobs".to_owned(),
            n => format!("Rerun failed jobs of {n} repos"),
        };
        Some(
            menu_item("menu-ci-rerun", label, false, cx)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.menu = None;
                    this.dispatch(Command::RerunFailedCi(failing.clone()), cx);
                }))
                .into_any_element(),
        )
    }

    /// Right-click menu of a sidebar category header: rename, remove.
    fn category_menu(category: String, cx: &mut Context<Self>) -> AnyElement {
        let rename = category.clone();
        context_menu(cx)
            .child(
                menu_item("category-menu-rename", "Rename category…", false, cx).on_click(
                    cx.listener(move |this, _, window, cx| {
                        this.menu = None;
                        this.open_palette(PaletteKind::RenameCategory(rename.clone()), window, cx);
                    }),
                ),
            )
            .child(
                menu_item(
                    "category-menu-remove",
                    "Remove category (repos stay in the project)",
                    true,
                    cx,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.menu = None;
                    this.dispatch(Command::RenameCategory(category.clone(), None), cx);
                })),
            )
            .into_any_element()
    }

    /// Right-click menu of a tab: close it, the others, those to its right,
    /// all. None once the tab is closed.
    fn tab_menu(&mut self, tab: &TabState, cx: &mut Context<Self>) -> Option<AnyElement> {
        let tabs = self.core.ui_state()?.tabs;
        let index = tabs.iter().position(|t| t == tab)?;
        let count = tabs.len();
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
                    this.menu = None;
                    this.dispatch(cmd.clone(), cx);
                }))
                .into_any_element()
        });
        let menu = context_menu(cx)
            .children(rows)
            .when(index > 0 || index + 1 < count, |m| {
                m.child(menu_separator(cx))
            })
            .child(
                menu_item("tab-menu-all", "Close all", false, cx).on_click(cx.listener(
                    |this, _, _, cx| {
                        this.menu = None;
                        this.dispatch(Command::CloseAllTabs, cx);
                    },
                )),
            );
        Some(menu.into_any_element())
    }

    /// The context menu of a repo row (right click).
    #[allow(clippy::too_many_lines)] // declarative element tree
    fn row_menu(&mut self, id: RepoId, cx: &mut Context<Self>) -> Option<AnyElement> {
        let ui = self.core.ui_state()?;
        // Acting on a selected row acts on the whole selection.
        let selection = ui.batch_selection();
        let targets = if selection.contains(&id) {
            selection
        } else {
            vec![id]
        };
        let all_hidden = targets.iter().all(|r| ui.is_hidden(*r));
        // A linked worktree is removed as a worktree, not from the project.
        let is_worktree = self.core.row(id).is_some_and(|r| r.parent().is_some());
        let n = targets.len();
        let hover = color(cx, T::BgHover);
        let subtle = color(cx, T::FgSubtle);
        let item = |eid: &'static str,
                    label: String,
                    shortcut: Option<String>,
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
                    this.menu = None;
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
        let hide_label = if all_hidden {
            format!("Unhide{plural}")
        } else {
            format!("Hide{plural}")
        };
        // The same key hides and unhides.
        let hide_shortcut = self.shortcut("repos.toggle_hidden");
        let menu = context_menu(cx)
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
                let label = if n > 1 {
                    format!("Set category of{plural}…")
                } else {
                    "Set category…".to_owned()
                };
                menu_item("menu-category", label, false, cx).on_click(cx.listener(
                    move |this, _, window, cx| {
                        this.menu = None;
                        this.set_category(targets.clone(), window, cx);
                    },
                ))
            })
            .child(item(
                "menu-fetch",
                format!("Fetch{plural}"),
                None,
                Command::Fetch(targets.clone()),
                cx,
            ))
            .when(n == 1, |d| {
                d.child(
                    menu_item("menu-remote-url", "Change remote URL…", false, cx).on_click(
                        cx.listener(move |this, _, window, cx| {
                            this.menu = None;
                            this.change_remote_url(id, None, window, cx);
                        }),
                    ),
                )
            })
            .child({
                let label = if n > 1 {
                    format!("Statistics of{plural}…")
                } else {
                    "Statistics…".to_owned()
                };
                let (scope, focus) = (targets.clone(), (n == 1).then_some(id));
                menu_item("menu-stats", label, false, cx).on_click(cx.listener(
                    move |this, _, window, cx| {
                        this.menu = None;
                        this.open_stats(scope.clone(), focus, window, cx);
                    },
                ))
            })
            .children(self.ci_menu_item(id, cx))
            .children(self.ci_rerun_menu_item(&targets, cx))
            .children(self.auto_merge_menu_item(id, &targets, cx))
            .child(self.forge_menu_item(id, targets, cx))
            .children(self.worktree_menu_items(id, cx))
            .when(!is_worktree, |d| d.child(menu_separator(cx)))
            .when(!is_worktree, |d| {
                d.child(
                    menu_item("menu-remove", "Remove from project", true, cx).on_click(
                        cx.listener(move |this, _, _, cx| {
                            this.menu = None;
                            this.dispatch(Command::RemoveRepo(id), cx);
                        }),
                    ),
                )
            });
        Some(menu.into_any_element())
    }
}
