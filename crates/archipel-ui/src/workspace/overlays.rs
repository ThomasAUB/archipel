//! Overlays over the whole window: palettes (project switcher, actions,
//! prompts), the commit, forge and branches panels.

use archipel_app::Command;
use archipel_app::forge::pick_remote;
use archipel_model::ports::{MergeToolInfo, RemoteInfo};
use archipel_model::project::ProjectLocation;
use archipel_model::settings::MergeToolChoice;
use archipel_model::{ProjectId, RepoId};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, App, Context, Entity, MouseDownEvent, SharedString, Subscription, Window, div,
};

use crate::branches_panel::{BranchesPanel, BranchesPanelEvent};
use crate::commit_panel::{CommitPanel, CommitPanelEvent};
use crate::forge_panel::{ForgePanel, ForgePanelEvent, ForgeTab};
use crate::palette::{Palette, PaletteEvent, PaletteItem};

use super::Workspace;

/// What a palette overlay is for, with what it acts on: the palette holds
/// it, so nothing is left behind when it is dismissed.
#[derive(Clone, PartialEq, Eq)]
pub(super) enum PaletteKind {
    Projects,
    Actions,
    Checkout,
    Branch,
    Tag,
    Color,
    BackgroundTint,
    Icon,
    /// The category of these repos.
    Category(Vec<RepoId>),
    /// A new name for this category.
    RenameCategory(String),
    /// The Worktree Switcher (spec §10).
    Worktrees,
    /// The reason this worktree is locked.
    LockReason(RepoId),
    /// A new name for the active Worktree Set.
    RenameSet,
    /// Which remote of this repo gets a new URL (its remotes).
    PickRemote(RepoId, Vec<RemoteInfo>),
    /// The merge tool (the tools Git knows, installed first).
    MergeTool(Vec<MergeToolInfo>),
    /// The new URL of a remote of this repo.
    RemoteUrl {
        repo: RepoId,
        remote: String,
        /// Its URL now (empty: the remote does not exist yet).
        current: String,
    },
}

/// The overlay shown over the window, its view kept while it is open.
pub(super) enum Overlay {
    Palette(Entity<Palette>),
    Commit(Entity<CommitPanel>),
    Forge(Entity<ForgePanel>),
    Branches(Entity<BranchesPanel>),
    NewRepo(Entity<crate::new_repo_dialog::NewRepoDialog>),
    NewWorktree(Entity<crate::worktree_dialog::NewWorktreeDialog>),
    RemoveWorktree(Entity<crate::worktree_dialog::RemoveWorktreeDialog>),
    NewSet(Entity<crate::worktree_set_dialog::NewSetDialog>),
    FinishSet(Entity<crate::worktree_set_dialog::FinishSetDialog>),
}

/// A dialog centered over the window.
fn centered(view: AnyElement) -> AnyElement {
    div()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .child(view)
        .into_any_element()
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
                Overlay::NewRepo(d) => centered(d.clone().into_any_element()),
                Overlay::NewWorktree(d) => centered(d.clone().into_any_element()),
                Overlay::RemoveWorktree(d) => centered(d.clone().into_any_element()),
                Overlay::NewSet(d) => centered(d.clone().into_any_element()),
                Overlay::FinishSet(d) => centered(d.clone().into_any_element()),
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

    /// Items of the project switcher: the projects of the other windows
    /// (most recently active first, so Enter goes back to the previous one),
    /// new and open, then every other project, this window's last.
    fn project_items(&self, window: &Window, cx: &App) -> Vec<PaletteItem> {
        let actions = vec![
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
        let in_windows = crate::windows::open_projects_by_recency(cx, window.window_handle());
        let mut windows = Vec::new();
        let mut others = Vec::new();
        let mut this = Vec::new();
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
            let rank = in_windows.iter().position(|id| id == &p.id);
            let is_current = current.as_ref() == Some(&p.id);
            let item = PaletteItem {
                id: p.id.0,
                label,
                detail,
            };
            match rank {
                Some(r) => windows.push((r, item)),
                None if is_current => this.push(item),
                None => others.push(item),
            }
        }
        windows.sort_by_key(|(r, _)| *r);
        windows
            .into_iter()
            .map(|(_, item)| item)
            .chain(actions)
            .chain(others)
            .chain(this)
            .collect()
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
                    .map(|k| crate::keys::label(k))
                    .unwrap_or_default(),
            })
            .collect()
    }

    /// Opens the palette of `kind`.
    #[allow(clippy::too_many_lines)] // one arm per palette
    pub(super) fn open_palette(
        &mut self,
        kind: PaletteKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (title, items, prompt) = match &kind {
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
            PaletteKind::Worktrees => (
                "Switch worktree (repo · branch · folder)",
                self.worktree_items(),
                false,
            ),
            PaletteKind::LockReason(_) => (
                "Lock the worktree: why? (e.g. on a removable drive)",
                Vec::new(),
                true,
            ),
            PaletteKind::RenameSet => {
                let old = self.core.active_set().map(|s| s.name).unwrap_or_default();
                return self.show_palette(
                    kind,
                    format!("Rename worktree set “{old}”"),
                    Vec::new(),
                    true,
                    window,
                    cx,
                );
            }
            PaletteKind::PickRemote(_, remotes) => {
                let items = remotes
                    .iter()
                    .map(|r| PaletteItem {
                        id: r.name.clone(),
                        label: r.name.clone(),
                        detail: r.url.clone(),
                    })
                    .collect();
                return self.show_palette(
                    kind,
                    "Change the URL of which remote?",
                    items,
                    false,
                    window,
                    cx,
                );
            }
            PaletteKind::MergeTool(tools) => {
                let current = self.core.merge_tool_choice();
                let title = "What opens on conflicts: pick one, or type a merge tool's name";
                let mark = |c: &MergeToolChoice| if *c == current { "current · " } else { "" };
                let fixed = [
                    (
                        MergeToolChoice::BuiltIn,
                        "Built-in merge view",
                        "ours, theirs and the result, in the repo tab",
                    ),
                    (
                        MergeToolChoice::GitConfig,
                        "Git's merge tool",
                        "the external tool of Git's merge.tool",
                    ),
                ];
                let items = fixed
                    .into_iter()
                    .map(|(c, label, detail)| PaletteItem {
                        id: c.setting().unwrap_or(MergeToolChoice::BUILT_IN).to_owned(),
                        label: label.into(),
                        detail: format!("{}{detail}", mark(&c)),
                    })
                    .chain(tools.iter().map(|t| PaletteItem {
                        id: t.name.clone(),
                        label: t.name.clone(),
                        detail: format!(
                            "{}{}{}",
                            mark(&MergeToolChoice::External(t.name.clone())),
                            if t.available { "" } else { "not installed · " },
                            t.detail
                        ),
                    }))
                    .collect();
                return self.show_palette(kind, title, items, true, window, cx);
            }
            PaletteKind::RemoteUrl {
                remote, current, ..
            } => {
                let title = if current.is_empty() {
                    format!("URL of the new remote “{remote}”")
                } else {
                    format!("New URL of “{remote}” (was {current})")
                };
                let current = current.clone();
                self.show_palette(kind, title, Vec::new(), true, window, cx);
                if let Some((Overlay::Palette(palette), _)) = &self.overlay {
                    palette.update(cx, |p, cx| p.set_query(current, window, cx));
                }
                return;
            }
            PaletteKind::RenameCategory(old) => {
                // A plain prompt: an empty Enter must not pick (merge into)
                // another category. Typing an existing name merges into it.
                let old = old.clone();
                return self.show_palette(
                    kind,
                    format!("Rename category “{old}” (an existing name merges into it)"),
                    Vec::new(),
                    true,
                    window,
                    cx,
                );
            }
            PaletteKind::Category(targets) => {
                let n = targets.len();
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
                this.close_overlay(window, cx);
                if let PaletteEvent::Confirmed(value) = event {
                    this.on_palette(kind.clone(), value.clone(), window, cx);
                }
            },
        );
        self.show_overlay(Overlay::Palette(palette), sub, cx);
    }

    /// Shows `overlay` (its events heard through `sub`) in place of any
    /// other overlay or menu. Every overlay opens through here.
    pub(super) fn show_overlay(
        &mut self,
        overlay: Overlay,
        sub: Subscription,
        cx: &mut Context<Self>,
    ) {
        self.menu = None;
        self.overlay = Some((overlay, sub));
        cx.notify();
    }

    /// Dismisses the open overlay (as Escape / Close would) and gives the
    /// keyboard back to the window. Every overlay closes through here.
    pub(super) fn close_overlay(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
        let targets = self.selection();
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
            PaletteKind::Category(ids) => {
                let category = Some(value).filter(|v| !v.is_empty());
                self.dispatch(Command::SetCategory(ids, category), cx);
            }
            PaletteKind::RenameCategory(old) => {
                self.dispatch(Command::RenameCategory(old, Some(value)), cx);
            }
            PaletteKind::Worktrees => {
                if let Ok(raw) = value.parse::<u64>() {
                    self.dispatch(Command::OpenRepoTab(RepoId(raw)), cx);
                }
            }
            PaletteKind::LockReason(id) => {
                self.dispatch(Command::LockWorktree(id, Some(value.trim().to_owned())), cx);
            }
            PaletteKind::RenameSet => {
                if let Some(set) = self.core.active_set() {
                    self.dispatch(Command::RenameWorktreeSet(set.name, value), cx);
                }
            }
            PaletteKind::PickRemote(repo, remotes) => {
                if let Some(r) = remotes.into_iter().find(|r| r.name == value) {
                    let kind = PaletteKind::RemoteUrl {
                        repo,
                        remote: r.name,
                        current: r.url,
                    };
                    self.open_palette(kind, window, cx);
                }
            }
            PaletteKind::MergeTool(_) => {
                self.set_merge_tool(MergeToolChoice::from_setting(Some(&value)), cx);
            }
            PaletteKind::RemoteUrl {
                repo,
                remote,
                current,
            } => {
                let url = value.trim().to_owned();
                if url != current {
                    self.dispatch(Command::SetRemoteUrl { repo, remote, url }, cx);
                }
            }
        }
    }

    /// Asks what opens on conflicts: the built-in merge view, Git's merge
    /// tool, or a tool Git knows (installed first, read off the UI thread);
    /// any name can be typed (a `mergetool.<name>.cmd` of the user's).
    pub(super) fn choose_merge_tool(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let core = self.core.clone();
        let repo = core.action_context().target_repo();
        let task = cx.background_spawn(async move { core.merge_tool_choices(repo) });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |w, window, cx| {
                let tools = result.unwrap_or_else(|e| {
                    w.notice = Some(format!("cannot list the merge tools: {e}"));
                    Vec::new()
                });
                w.open_palette(PaletteKind::MergeTool(tools), window, cx);
            });
        })
        .detach();
    }

    /// Uses `choice` in every window and saves it in the settings file.
    fn set_merge_tool(&mut self, choice: MergeToolChoice, cx: &mut Context<Self>) {
        let saved = match cx.try_global::<crate::SettingsFile>() {
            Some(file) => archipel_app::settings::save_merge_tool(&file.0, choice.setting()),
            None => Ok(()),
        };
        let now = match &choice {
            MergeToolChoice::BuiltIn => "the built-in merge view".to_owned(),
            MergeToolChoice::GitConfig => "Git's merge tool (merge.tool)".to_owned(),
            MergeToolChoice::External(t) => format!("“{t}”"),
        };
        self.core.set_merge_tool_choice(choice);
        self.notice = Some(match saved {
            Ok(()) => format!("Conflicts now open {now}"),
            Err(e) => format!("Conflicts now open {now}, for this session only: {e}"),
        });
        cx.notify();
    }

    /// Asks for the new URL of a remote of the repo of row `repo`: of the
    /// remote of remote branch `branch` (`origin/main`) when given, else of
    /// its only remote, of the one picked when it has several, or of a new
    /// `origin` when it has none. The remotes are read off the UI thread.
    pub(super) fn change_remote_url(
        &mut self,
        repo: RepoId,
        branch: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let core = self.core.clone();
        let task = cx.background_spawn(async move { core.remotes(repo) });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |w, window, cx| match result {
                Err(e) => {
                    w.notice = Some(e.to_string());
                    cx.notify();
                }
                Ok(mut remotes) => {
                    // The remote of the branch: the longest name prefix, as
                    // remote names may hold a slash.
                    let owner = branch.as_deref().and_then(|b| {
                        remotes
                            .iter()
                            .filter(|r| b.starts_with(&format!("{}/", r.name)))
                            .max_by_key(|r| r.name.len())
                            .cloned()
                    });
                    let kind = match owner {
                        Some(r) => PaletteKind::RemoteUrl {
                            repo,
                            remote: r.name,
                            current: r.url,
                        },
                        None => match remotes.len() {
                            0 => PaletteKind::RemoteUrl {
                                repo,
                                remote: "origin".into(),
                                current: String::new(),
                            },
                            1 => {
                                let r = remotes.remove(0);
                                PaletteKind::RemoteUrl {
                                    repo,
                                    remote: r.name,
                                    current: r.url,
                                }
                            }
                            _ => {
                                // The usual one (origin) first.
                                if let Some(first) =
                                    pick_remote(&remotes, None).map(|r| r.name.clone())
                                    && let Some(i) = remotes.iter().position(|r| r.name == first)
                                {
                                    let r = remotes.remove(i);
                                    remotes.insert(0, r);
                                }
                                PaletteKind::PickRemote(repo, remotes)
                            }
                        },
                    };
                    w.open_palette(kind, window, cx);
                }
            });
        })
        .detach();
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
        self.open_palette(PaletteKind::Category(targets), window, cx);
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
                    CommitPanelEvent::Closed => this.close_overlay(window, cx),
                    CommitPanelEvent::Finished(summary) => this.notice = Some(summary.clone()),
                }
                cx.notify();
            },
        );
        self.show_overlay(Overlay::Commit(panel), sub, cx);
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
                    ForgePanelEvent::Closed => this.close_overlay(window, cx),
                    ForgePanelEvent::Notice(text) => this.notice = Some(text.clone()),
                }
                cx.notify();
            },
        );
        self.show_overlay(Overlay::Forge(panel), sub, cx);
    }

    /// Opens the branches panel on the selected repos (all when none).
    pub(super) fn open_branches(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.core.project().is_none() {
            return;
        }
        let (core, scope) = (self.core.clone(), self.selection());
        let panel = cx.new(|cx| BranchesPanel::new(core, scope, cx));
        let sub = cx.subscribe_in(
            &panel,
            window,
            |this: &mut Self, _, event: &BranchesPanelEvent, window, cx| {
                match event {
                    BranchesPanelEvent::Closed => this.close_overlay(window, cx),
                    BranchesPanelEvent::Finished(summary) => this.notice = Some(summary.clone()),
                }
                cx.notify();
            },
        );
        self.show_overlay(Overlay::Branches(panel), sub, cx);
    }
}
