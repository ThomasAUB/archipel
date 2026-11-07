//! Overlays over the whole window: palettes (project switcher, actions,
//! prompts), the commit, forge and branches panels.

use archipel_app::Command;
use archipel_model::project::ProjectLocation;
use archipel_model::{ProjectId, RepoId};
use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, App, Context, Entity, MouseDownEvent, SharedString, Window, div};

use crate::branches_panel::{BranchesPanel, BranchesPanelEvent};
use crate::commit_panel::{CommitPanel, CommitPanelEvent};
use crate::forge_panel::{ForgePanel, ForgePanelEvent, ForgeTab};
use crate::palette::{Palette, PaletteEvent, PaletteItem};

use super::Workspace;

/// What a palette overlay is for.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum PaletteKind {
    Projects,
    Actions,
    Checkout,
    Branch,
    Tag,
    Color,
    BackgroundTint,
    Icon,
    Category,
    RenameCategory,
}

/// The overlay shown over the window, its view kept while it is open.
pub(super) enum Overlay {
    Palette(Entity<Palette>),
    Commit(Entity<CommitPanel>),
    Forge(Entity<ForgePanel>),
    Branches(Entity<BranchesPanel>),
}

impl Workspace {
    /// The open overlay over a backdrop catching clicks outside it.
    pub(super) fn render_overlay(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        self.overlay.as_ref().map(|(o, _)| {
            let view = match o {
                Overlay::Palette(p) => p.clone().into_any_element(),
                Overlay::Commit(c) => c.clone().into_any_element(),
                Overlay::Forge(f) => f.clone().into_any_element(),
                Overlay::Branches(b) => b.clone().into_any_element(),
            };
            // A click outside the overlay (which occludes its own area)
            // dismisses it, without reaching what lies underneath.
            div()
                .id("overlay-backdrop")
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .occlude()
                .on_any_mouse_down(cx.listener(|this, _: &MouseDownEvent, window, cx| {
                    this.close_overlay(window, cx);
                }))
                .child(view)
                .into_any_element()
        })
    }

    /// Items of the project switcher: new and open, then every project,
    /// noting the ones shown by a window.
    fn project_items(&self, window: &Window, cx: &App) -> Vec<PaletteItem> {
        let mut items = vec![
            PaletteItem {
                id: "+folder".into(),
                label: "New project from a folder…".into(),
                detail: "scan for repositories".into(),
            },
            PaletteItem {
                id: "+file".into(),
                label: "Open an archipel.toml…".into(),
                detail: "shared project file".into(),
            },
        ];
        let current = self.core.project().map(|(id, _)| id);
        for p in self.core.projects().unwrap_or_default() {
            let ProjectLocation::SharedFile(file) = &p.location;
            let mut detail = file.display().to_string();
            if current.as_ref() == Some(&p.id) {
                detail = format!("this window · {detail}");
            } else if crate::windows::is_open(window, cx, &p.id) {
                detail = format!("open in another window · {detail}");
            }
            let label = match self
                .core
                .project_icon(&p.id)
                .as_ref()
                .and_then(|i| i.text())
            {
                Some(icon) => format!("{icon}  {}", p.name),
                None => p.name,
            };
            items.push(PaletteItem {
                id: p.id.0,
                label,
                detail,
            });
        }
        items
    }

    /// Items of the action palette: the actions available, with their keys.
    fn action_items(&self) -> Vec<PaletteItem> {
        let ctx = self.core.action_context();
        let ext = self.core.extensions();
        ext.actions
            .available(&ctx)
            .into_iter()
            .map(|a| PaletteItem {
                id: a.id().to_owned(),
                label: a.label().to_owned(),
                detail: ext
                    .keymap
                    .keys_for(a.id())
                    .first()
                    .map(ToString::to_string)
                    .unwrap_or_default(),
            })
            .collect()
    }

    /// Opens the palette of `kind`.
    pub(super) fn open_palette(
        &mut self,
        kind: PaletteKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (title, items, prompt) = match kind {
            PaletteKind::Projects => ("Switch project", self.project_items(window, cx), false),
            PaletteKind::Actions => ("Actions", self.action_items(), false),
            PaletteKind::Checkout => (
                "Check out branch on selected repos (all when none)",
                Vec::new(),
                true,
            ),
            PaletteKind::Branch => (
                "Create branch on selected repos (all when none)",
                Vec::new(),
                true,
            ),
            PaletteKind::Tag => (
                "Create tag on selected repos (all when none)",
                Vec::new(),
                true,
            ),
            PaletteKind::Color => (
                "Project color: #rrggbb (empty: automatic, from the project path)",
                Vec::new(),
                true,
            ),
            PaletteKind::BackgroundTint => (
                "Project background tint: % of the project color, 0–100 (empty: none)",
                Vec::new(),
                true,
            ),
            PaletteKind::Icon => (
                "Project icon: emoji or image file (empty: none)",
                Vec::new(),
                true,
            ),
            PaletteKind::RenameCategory => {
                // A plain prompt: an empty Enter must not pick (merge into)
                // another category. Typing an existing name merges into it.
                let old = self.renaming_category.clone().unwrap_or_default();
                return self.show_palette(
                    kind,
                    format!("Rename category “{old}” (an existing name merges into it)"),
                    Vec::new(),
                    true,
                    window,
                    cx,
                );
            }
            PaletteKind::Category => {
                let n = self.category_targets.len();
                let title = if n > 1 {
                    format!("Category of {n} repos: pick one or type a new name")
                } else {
                    "Category: pick one or type a new name".to_owned()
                };
                let groups = self.core.sidebar_groups();
                let count = |c: &str| {
                    groups
                        .iter()
                        .find(|g| g.category.as_deref() == Some(c))
                        .map_or(0, |g| g.rows.len())
                };
                let mut items: Vec<PaletteItem> = self
                    .core
                    .categories()
                    .into_iter()
                    .map(|c| PaletteItem {
                        detail: match count(&c) {
                            1 => "1 repo".to_owned(),
                            k => format!("{k} repos"),
                        },
                        id: c.clone(),
                        label: c,
                    })
                    .collect();
                items.push(PaletteItem {
                    id: String::new(),
                    label: "No category".into(),
                    detail: "listed under Uncategorized".into(),
                });
                return self.show_palette(kind, title, items, true, window, cx);
            }
        };
        self.show_palette(kind, title, items, prompt, window, cx);
    }

    fn show_palette(
        &mut self,
        kind: PaletteKind,
        title: impl Into<SharedString>,
        items: Vec<PaletteItem>,
        prompt: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let palette = cx.new(|cx| Palette::new(title, items, prompt, window, cx));
        let sub = cx.subscribe_in(
            &palette,
            window,
            move |this: &mut Self, _, event: &PaletteEvent, window, cx| {
                this.overlay = None;
                this.focus.focus(window, cx);
                if let PaletteEvent::Confirmed(value) = event {
                    this.on_palette(kind, value.clone(), window, cx);
                }
                cx.notify();
            },
        );
        self.overlay = Some((Overlay::Palette(palette), sub));
        cx.notify();
    }

    /// Dismisses the open overlay (as Escape / Close would).
    fn close_overlay(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.overlay = None;
        self.focus.focus(window, cx);
        cx.notify();
    }

    fn on_palette(
        &mut self,
        kind: PaletteKind,
        value: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let targets = self.selected_or_all();
        match kind {
            PaletteKind::Projects => match value.as_str() {
                "+folder" => self.new_project_from_folder(cx),
                "+file" => self.open_project_file(cx),
                id => {
                    let id = ProjectId(id.to_owned());
                    // Already shown by another window: focus it, no duplicate.
                    if !crate::windows::focus_project(cx, &id, Some(window.window_handle()))
                        && let Err(e) = self.core.open_project(&id)
                    {
                        self.notice = Some(e.to_string());
                    }
                }
            },
            PaletteKind::Actions => self.run_action(&value, window, cx),
            PaletteKind::Checkout => self.dispatch(Command::CheckoutAll(value, targets), cx),
            PaletteKind::Branch => self.dispatch(Command::CreateBranchAll(value, targets), cx),
            PaletteKind::Tag => self.dispatch(Command::CreateTagAll(value, targets), cx),
            PaletteKind::Color => self.dispatch(Command::SetAccent(Some(value)), cx),
            PaletteKind::BackgroundTint => {
                self.dispatch(Command::SetBackgroundTint(Some(value)), cx);
            }
            PaletteKind::Icon => self.dispatch(Command::SetIcon(Some(value)), cx),
            PaletteKind::Category => {
                let ids = std::mem::take(&mut self.category_targets);
                let category = Some(value).filter(|v| !v.is_empty());
                self.dispatch(Command::SetCategory(ids, category), cx);
            }
            PaletteKind::RenameCategory => {
                if let Some(old) = self.renaming_category.take() {
                    self.dispatch(Command::RenameCategory(old, Some(value)), cx);
                }
            }
        }
    }

    /// Opens the category palette for `targets`.
    pub(super) fn set_category(
        &mut self,
        targets: Vec<RepoId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if targets.is_empty() {
            return;
        }
        self.category_targets = targets;
        self.open_palette(PaletteKind::Category, window, cx);
    }

    pub(super) fn open_commit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.core.project().is_none() {
            return;
        }
        let core = self.core.clone();
        let panel = cx.new(|cx| CommitPanel::new(core, window, cx));
        let sub = cx.subscribe_in(
            &panel,
            window,
            |this: &mut Self, _, event: &CommitPanelEvent, window, cx| {
                match event {
                    CommitPanelEvent::Closed => {
                        this.overlay = None;
                        this.focus.focus(window, cx);
                    }
                    CommitPanelEvent::Finished(summary) => this.notice = Some(summary.clone()),
                }
                cx.notify();
            },
        );
        self.overlay = Some((Overlay::Commit(panel), sub));
    }

    /// Opens the forge panel on `tab` (reusing it when already open).
    pub(super) fn open_forge(
        &mut self,
        tab: ForgeTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some((Overlay::Forge(panel), _)) = &self.overlay {
            panel.update(cx, |p, cx| p.show(tab, window, cx));
            return;
        }
        let core = self.core.clone();
        let panel = cx.new(|cx| ForgePanel::new(core, tab, window, cx));
        let sub = cx.subscribe_in(
            &panel,
            window,
            |this: &mut Self, _, event: &ForgePanelEvent, window, cx| {
                match event {
                    ForgePanelEvent::Closed => {
                        this.overlay = None;
                        this.focus.focus(window, cx);
                    }
                    ForgePanelEvent::Notice(text) => this.notice = Some(text.clone()),
                }
                cx.notify();
            },
        );
        self.overlay = Some((Overlay::Forge(panel), sub));
        cx.notify();
    }

    /// Opens the branches panel on the selected repos (all when none).
    pub(super) fn open_branches(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.core.project().is_none() {
            return;
        }
        let (core, scope) = (self.core.clone(), self.selected_or_all());
        let panel = cx.new(|cx| BranchesPanel::new(core, scope, cx));
        let sub = cx.subscribe_in(
            &panel,
            window,
            |this: &mut Self, _, event: &BranchesPanelEvent, window, cx| {
                match event {
                    BranchesPanelEvent::Closed => {
                        this.overlay = None;
                        this.focus.focus(window, cx);
                    }
                    BranchesPanelEvent::Finished(summary) => this.notice = Some(summary.clone()),
                }
                cx.notify();
            },
        );
        self.overlay = Some((Overlay::Branches(panel), sub));
        cx.notify();
    }
}
