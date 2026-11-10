//! Right-click menus of the graph and refs panel: what they offer for each
//! target ([`menu_entries`]), and running the action picked.

use archipel_app::forge::pick_remote;
use archipel_model::refs::{RefKind, RefTip};
use archipel_model::requests::{CheckoutTarget, DeleteMode, MergeMode, ResetMode, StashApplyMode};
use archipel_model::submodule::SubmoduleInfo;
use archipel_model::ui_state::GraphColumn;
use archipel_model::{Oid, RepoPath};
use archipel_ops::{OpClass, OpError};
use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, ClipboardItem, Context, Pixels, Point, Window};

use crate::widgets::{
    SHORT_OID, check_menu_item, context_menu, menu_item, menu_separator, popup_at,
};

use super::RepoView;
use super::dialogs::{Confirm, NameForm};
use super::squash::SquashRange;

/// What a right-click menu acts on.
#[derive(Clone)]
pub(super) enum MenuTarget {
    /// A commit row: its id and the refs pointing at it.
    Commit(Oid, Vec<RefTip>),
    /// Several selected commits, newest first.
    Commits(Vec<Oid>),
    /// A branch or tag (graph badge or refs panel).
    Ref(RefTip),
    /// A stash, by commit id.
    Stash(Oid),
    /// A submodule (refs panel).
    Submodule(SubmoduleInfo),
    /// Branch `source` dropped on branch `target` (drag and drop of
    /// branch badges or refs panel rows).
    Drop { source: RefTip, target: RefTip },
    /// The commit graph header: its optional columns, the hidden ones
    /// listed.
    Columns(Vec<GraphColumn>),
}

/// An action picked in a right-click menu.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum MenuAction {
    Name(NameForm),
    Checkout(CheckoutTarget),
    CherryPick(Oid),
    Revert(Oid),
    /// Opens the squash dialog once the range is checked.
    Squash(SquashRange),
    Reset(Oid, ResetMode),
    Merge(String),
    Rebase(String),
    /// Checks out local branch `into`, then merges `rev` into it.
    MergeInto {
        rev: String,
        into: String,
    },
    /// Checks out local branch `branch`, then rebases it onto `onto`.
    RebaseBranch {
        branch: String,
        onto: String,
    },
    DeleteBranch(String, DeleteMode),
    /// A remote branch, `remote/branch`, deleted on the remote.
    DeleteRemoteBranch(String),
    DeleteTag(String),
    PushTag(String),
    Stash(Oid, Option<StashApplyMode>),
    /// A submodule opened as a repo tab.
    OpenSubmodule(RepoPath),
    /// `git submodule update` of one submodule (path relative to the
    /// repo), initializing it first when `init`.
    UpdateSubmodule {
        name: String,
        init: bool,
    },
    Copy(String),
    /// Shows or hides a commit graph column.
    ToggleColumn(GraphColumn),
}

impl MenuAction {
    /// The question asked before an action that cannot be undone from the
    /// journal or loses work; `None` runs it right away.
    fn confirmation(&self) -> Option<String> {
        match self {
            Self::Reset(oid, ResetMode::Hard) => Some(format!(
                "Reset the current branch to {} and discard every uncommitted change?",
                oid.short(SHORT_OID)
            )),
            Self::DeleteBranch(name, DeleteMode::Force) => Some(format!(
                "Delete {name} even though it is not merged? Its unmerged commits are only \
                 reachable through Undo afterwards."
            )),
            Self::DeleteRemoteBranch(name) => {
                Some(format!("Delete {name} on the remote? Others lose it too."))
            }
            _ => None,
        }
    }
}

/// One line of a right-click menu.
#[derive(Debug, PartialEq, Eq)]
enum MenuEntry {
    Item {
        label: String,
        action: MenuAction,
        danger: bool,
        /// On/off entries: whether the tick is shown. `None`: a plain
        /// action.
        checked: Option<bool>,
    },
    Separator,
}

fn item(label: impl Into<String>, action: MenuAction) -> MenuEntry {
    MenuEntry::Item {
        label: label.into(),
        action,
        danger: false,
        checked: None,
    }
}

fn danger(label: impl Into<String>, action: MenuAction) -> MenuEntry {
    MenuEntry::Item {
        label: label.into(),
        action,
        danger: true,
        checked: None,
    }
}

/// The entries of the menu on `target`. `current`: the checked-out branch;
/// `has_local`: whether a local branch has this name.
fn menu_entries(
    target: &MenuTarget,
    current: Option<&str>,
    has_local: impl Fn(&str) -> bool,
) -> Vec<MenuEntry> {
    match target {
        MenuTarget::Commit(oid, refs) => commit_entries(*oid, refs, current),
        MenuTarget::Commits(oids) => commits_entries(oids),
        MenuTarget::Ref(tip) => ref_entries(tip, current, has_local),
        MenuTarget::Stash(oid) => stash_entries(*oid),
        MenuTarget::Drop { source, target } => drop_entries(source, target, current),
        MenuTarget::Submodule(sub) => submodule_entries(sub),
        MenuTarget::Columns(hidden) => column_entries(hidden),
    }
}

/// The menu of the graph header: each optional column, ticked when shown.
fn column_entries(hidden: &[GraphColumn]) -> Vec<MenuEntry> {
    GraphColumn::ALL
        .into_iter()
        .map(|c| MenuEntry::Item {
            label: c.label().into(),
            action: MenuAction::ToggleColumn(c),
            danger: false,
            checked: Some(!hidden.contains(&c)),
        })
        .collect()
}

/// The menu of commit `oid`, which `refs` point at.
fn commit_entries(oid: Oid, refs: &[RefTip], current: Option<&str>) -> Vec<MenuEntry> {
    use MenuAction as A;
    let onto = current.unwrap_or("HEAD");
    let mut out = vec![
        item("New branch here…", A::Name(NameForm::Branch(Some(oid)))),
        item("New tag here…", A::Name(NameForm::Tag(oid))),
        item(
            "Checkout (detached)",
            A::Checkout(CheckoutTarget::Detached(oid)),
        ),
        MenuEntry::Separator,
        item("Cherry-pick", A::CherryPick(oid)),
        item("Revert", A::Revert(oid)),
    ];
    // Squashes rewrite the current branch: offered on any commit, refused
    // (with the reason) off its first-parent history.
    out.push(item(
        "Squash into parent…",
        A::Squash(SquashRange::IntoParent(oid)),
    ));
    if !refs.iter().any(|r| r.kind == RefKind::Head) {
        out.push(item(
            format!("Squash from here to {onto}…"),
            A::Squash(SquashRange::UpToHead(oid)),
        ));
    }
    out.extend([
        item(
            format!("Reset {onto} here (soft)"),
            A::Reset(oid, ResetMode::Soft),
        ),
        item(
            format!("Reset {onto} here (mixed)"),
            A::Reset(oid, ResetMode::Mixed),
        ),
        danger(
            format!("Reset {onto} here (hard)…"),
            A::Reset(oid, ResetMode::Hard),
        ),
    ]);
    let deletes: Vec<MenuEntry> = refs
        .iter()
        .filter_map(|r| match r.kind {
            RefKind::LocalBranch if Some(r.name.as_str()) != current => Some(danger(
                format!("Delete branch {}", r.name),
                A::DeleteBranch(r.name.clone(), DeleteMode::Safe),
            )),
            RefKind::Tag => Some(danger(
                format!("Delete tag {}", r.name),
                A::DeleteTag(r.name.clone()),
            )),
            _ => None,
        })
        .collect();
    if !deletes.is_empty() {
        out.push(MenuEntry::Separator);
        out.extend(deletes);
    }
    out.push(MenuEntry::Separator);
    out.push(item("Copy commit id", A::Copy(oid.to_hex())));
    out
}

/// The menu of several selected commits, newest first.
fn commits_entries(oids: &[Oid]) -> Vec<MenuEntry> {
    use MenuAction as A;
    let ids: Vec<String> = oids.iter().map(Oid::to_hex).collect();
    vec![
        item(
            format!("Squash {} commits…", oids.len()),
            A::Squash(SquashRange::Selected(oids.to_vec())),
        ),
        MenuEntry::Separator,
        item("Copy commit ids", A::Copy(ids.join(" "))),
    ]
}

/// The menu of a branch or tag.
fn ref_entries(
    tip: &RefTip,
    current: Option<&str>,
    has_local: impl Fn(&str) -> bool,
) -> Vec<MenuEntry> {
    use MenuAction as A;
    let mut out = Vec::new();
    let name = &tip.name;
    let is_current = tip.kind == RefKind::LocalBranch && Some(name.as_str()) == current;
    if !is_current && let Some(t) = CheckoutTarget::for_ref(tip, &has_local) {
        out.push(item(format!("Checkout {name}"), A::Checkout(t)));
    }
    if matches!(tip.kind, RefKind::LocalBranch | RefKind::RemoteBranch) && !is_current {
        let into = current.unwrap_or("HEAD");
        out.push(item(
            format!("Merge {name} into {into}"),
            A::Merge(name.clone()),
        ));
        out.push(item(
            format!("Rebase {into} onto {name}"),
            A::Rebase(name.clone()),
        ));
    }
    if matches!(
        tip.kind,
        RefKind::LocalBranch | RefKind::RemoteBranch | RefKind::Tag
    ) && !is_current
    {
        out.push(item(
            format!("Cherry-pick tip of {name}"),
            A::CherryPick(tip.target),
        ));
    }
    out.push(item(
        format!("New branch from {name}…"),
        A::Name(NameForm::Branch(Some(tip.target))),
    ));
    match tip.kind {
        RefKind::LocalBranch => {
            out.push(item("Rename…", A::Name(NameForm::Rename(name.clone()))));
            if !is_current {
                out.push(MenuEntry::Separator);
                out.push(danger(
                    format!("Delete {name}"),
                    A::DeleteBranch(name.clone(), DeleteMode::Safe),
                ));
                out.push(danger(
                    format!("Force delete {name}…"),
                    A::DeleteBranch(name.clone(), DeleteMode::Force),
                ));
            }
        }
        RefKind::RemoteBranch => {
            out.push(MenuEntry::Separator);
            out.push(danger(
                format!("Delete {name} on the remote…"),
                A::DeleteRemoteBranch(name.clone()),
            ));
        }
        RefKind::Tag => {
            out.push(item(format!("Push {name}"), A::PushTag(name.clone())));
            out.push(MenuEntry::Separator);
            out.push(danger(format!("Delete {name}"), A::DeleteTag(name.clone())));
        }
        RefKind::Head | RefKind::Stash => {}
    }
    out.push(MenuEntry::Separator);
    out.push(item("Copy name", A::Copy(name.clone())));
    out
}

/// The menu of the stash with commit id `oid`.
fn stash_entries(oid: Oid) -> Vec<MenuEntry> {
    use MenuAction as A;
    vec![
        item("Apply", A::Stash(oid, Some(StashApplyMode::Keep))),
        item("Pop", A::Stash(oid, Some(StashApplyMode::Pop))),
        danger("Drop", A::Stash(oid, None)),
    ]
}

/// The menu of a submodule.
fn submodule_entries(sub: &SubmoduleInfo) -> Vec<MenuEntry> {
    use MenuAction as A;
    let mut out = Vec::new();
    let name = sub.name.clone();
    if sub.initialized {
        out.push(item("Open", A::OpenSubmodule(sub.path.clone())));
        let label = if sub.is_out_of_sync() {
            "Update to the pinned commit"
        } else {
            "Update"
        };
        out.push(item(label, A::UpdateSubmodule { name, init: false }));
    } else {
        out.push(item("Initialize", A::UpdateSubmodule { name, init: true }));
    }
    out.push(MenuEntry::Separator);
    out.push(item("Copy path", A::Copy(sub.name.clone())));
    out
}

/// The actions offered when branch `source` is dropped on branch
/// `target`: merge `source` into `target` (when `target` is local) and
/// rebase `source` onto `target` (when `source` is local). A branch other
/// than `current` is checked out first.
fn drop_entries(source: &RefTip, target: &RefTip, current: Option<&str>) -> Vec<MenuEntry> {
    use MenuAction as A;
    let mut out = Vec::new();
    if source == target {
        return out;
    }
    let (s, t) = (&source.name, &target.name);
    if target.kind == RefKind::LocalBranch {
        out.push(if Some(t.as_str()) == current {
            item(format!("Merge {s} into {t}"), A::Merge(s.clone()))
        } else {
            item(
                format!("Checkout {t} and merge {s} into it"),
                A::MergeInto {
                    rev: s.clone(),
                    into: t.clone(),
                },
            )
        });
    }
    if source.kind == RefKind::LocalBranch {
        out.push(if Some(s.as_str()) == current {
            item(format!("Rebase {s} onto {t}"), A::Rebase(t.clone()))
        } else {
            item(
                format!("Checkout {s} and rebase it onto {t}"),
                A::RebaseBranch {
                    branch: s.clone(),
                    onto: t.clone(),
                },
            )
        });
    }
    out
}

/// Whether `git branch -d` failed because the branch is not merged. Git's
/// message may be translated; its hint names `git branch -D` in any locale.
pub(super) fn is_not_merged(e: &OpError) -> bool {
    matches!(
        e,
        OpError::Git(archipel_model::GitError::CommandFailed { stderr, .. })
            if stderr.contains("not fully merged") || stderr.contains("branch -D")
    )
}

impl RepoView {
    /// Opens the right-click menu on `target` at `position`.
    pub(super) fn open_menu(
        &mut self,
        target: MenuTarget,
        position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        self.menu = Some((target, position));
        cx.notify();
    }

    /// Runs a menu action, asking first when it is destructive.
    pub(super) fn choose(
        &mut self,
        action: MenuAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.menu = None;
        if let Some(question) = action.confirmation() {
            self.confirm = Some(Confirm { question, action });
            self.prompt_focus.focus(window, cx);
            cx.notify();
        } else {
            self.perform(action, window, cx);
        }
    }

    #[allow(clippy::too_many_lines)] // one arm per menu action
    pub(super) fn perform(
        &mut self,
        action: MenuAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match action {
            MenuAction::Name(form) => self.open_name_form(Some(form), window, cx),
            MenuAction::Checkout(target) => self.checkout(target, cx),
            MenuAction::CherryPick(oid) => {
                self.run(
                    format!("Cherry-pick {}", oid.short(SHORT_OID)),
                    cx,
                    move |a, p, _| a.cherry_pick(p, &[oid]),
                );
            }
            MenuAction::Revert(oid) => {
                self.run(
                    format!("Revert {}", oid.short(SHORT_OID)),
                    cx,
                    move |a, p, _| a.revert(p, oid),
                );
            }
            MenuAction::Squash(range) => self.start_squash(range, window, cx),
            MenuAction::Reset(oid, mode) => {
                self.run(
                    format!("Reset to {}", oid.short(SHORT_OID)),
                    cx,
                    move |a, p, _| a.reset(p, &oid.to_hex(), mode),
                );
            }
            MenuAction::Merge(rev) => {
                self.run(format!("Merge {rev}"), cx, move |a, p, _| {
                    a.merge(p, &rev, MergeMode::Auto)
                });
            }
            MenuAction::Rebase(onto) => {
                self.run(format!("Rebase onto {onto}"), cx, move |a, p, _| {
                    a.rebase(p, &onto)
                });
            }
            MenuAction::MergeInto { rev, into } => {
                self.run(format!("Merge {rev} into {into}"), cx, move |a, p, _| {
                    a.checkout(p, &CheckoutTarget::Branch(into), None)?;
                    a.merge(p, &rev, MergeMode::Auto)
                });
            }
            MenuAction::RebaseBranch { branch, onto } => {
                self.run(
                    format!("Rebase {branch} onto {onto}"),
                    cx,
                    move |a, p, _| {
                        a.checkout(p, &CheckoutTarget::Branch(branch), None)?;
                        a.rebase(p, &onto)
                    },
                );
            }
            MenuAction::DeleteBranch(name, DeleteMode::Safe) => {
                self.delete_branch_safe(name, window, cx);
            }
            MenuAction::DeleteBranch(name, mode) => {
                self.run(format!("Delete branch {name}"), cx, move |a, p, _| {
                    a.delete_branch(p, &name, mode, None)
                });
            }
            MenuAction::DeleteRemoteBranch(name) => {
                self.run_as(
                    OpClass::Network,
                    format!("Delete {name}"),
                    cx,
                    move |a, p, op| a.delete_remote_branch(p, &name, op, op.cancel()),
                );
            }
            MenuAction::DeleteTag(name) => {
                self.run(format!("Delete tag {name}"), cx, move |a, p, _| {
                    a.delete_tag(p, &name)
                });
            }
            MenuAction::PushTag(name) => {
                self.run_as(
                    OpClass::Network,
                    format!("Push tag {name}"),
                    cx,
                    move |a, p, op| {
                        let remotes = a.ports().remotes.remotes(p)?;
                        let remote = pick_remote(&remotes, None).ok_or_else(|| {
                            OpError::Refused("the repository has no remote".into())
                        })?;
                        a.push_tag(p, &remote.name, &name, op, op.cancel())
                    },
                );
            }
            MenuAction::Stash(oid, mode) => self.stash_op(oid, mode, cx),
            MenuAction::OpenSubmodule(path) => {
                if let Err(e) = self
                    .core
                    .dispatch(archipel_app::Command::OpenSubmodule(path))
                {
                    self.status = Some(e.to_string());
                    cx.notify();
                }
            }
            MenuAction::UpdateSubmodule { name, init } => {
                let command = archipel_app::Command::UpdateSubmodules {
                    parent: self.path.clone(),
                    paths: vec![name],
                    init,
                    recursive: false,
                };
                if let Err(e) = self.core.dispatch(command) {
                    self.status = Some(e.to_string());
                    cx.notify();
                }
            }
            MenuAction::Copy(text) => {
                cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
                self.status = Some(format!("Copied {text}"));
                cx.notify();
            }
            MenuAction::ToggleColumn(column) => {
                self.core
                    .update_ui_state(|ui| ui.toggle_graph_column(column));
                cx.notify();
            }
        }
    }

    /// The open right-click menu.
    pub(super) fn render_menu(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (target, position) = self.menu.as_ref()?;
        let position = *position;
        let entries = menu_entries(target, self.current_branch(), |b| self.has_local(b));
        let rows = entries
            .into_iter()
            .enumerate()
            .map(|(i, entry)| match entry {
                MenuEntry::Separator => menu_separator(cx).into_any_element(),
                MenuEntry::Item {
                    label,
                    action,
                    danger,
                    checked,
                } => match checked {
                    Some(on) => check_menu_item(("repo-menu", i), label, on, cx),
                    None => menu_item(("repo-menu", i), label, danger, cx),
                }
                .on_click(cx.listener(move |v, _, window, cx| {
                    v.choose(action.clone(), window, cx);
                }))
                .into_any_element(),
            });
        let menu = context_menu(cx)
            .on_mouse_down_out(cx.listener(|v, _, _, cx| {
                v.menu = None;
                cx.notify();
            }))
            .children(rows);
        Some(popup_at(position, menu).into_any_element())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unmerged_branch_refusal_is_recognized() {
        let failed = |stderr: &str| {
            OpError::Git(archipel_model::GitError::CommandFailed {
                command: "git branch -d feat".into(),
                exit_code: Some(1),
                stderr: stderr.into(),
            })
        };
        assert!(is_not_merged(&failed(
            "error: the branch 'feat' is not fully merged\nhint: If you are sure you want \
             to delete it, run 'git branch -D feat'"
        )));
        assert!(is_not_merged(&failed(
            "erreur : la branche 'feat' n'est pas totalement fusionnée\nastuce : lancez \
             'git branch -D feat'"
        )));
        assert!(!is_not_merged(&failed(
            "error: cannot delete branch 'feat' used by worktree at 'C:/w'"
        )));
    }

    fn tip(name: &str, kind: RefKind) -> RefTip {
        RefTip {
            name: name.into(),
            kind,
            target: Oid::from_bytes(&[1; 20]).unwrap(),
        }
    }

    fn actions(entries: &[MenuEntry]) -> Vec<&MenuAction> {
        entries
            .iter()
            .filter_map(|e| match e {
                MenuEntry::Item { action, .. } => Some(action),
                MenuEntry::Separator => None,
            })
            .collect()
    }

    #[test]
    fn column_menu_ticks_the_shown_columns() {
        let entries = menu_entries(&MenuTarget::Columns(vec![GraphColumn::Age]), None, |_| true);
        let ticks: Vec<(&str, Option<bool>)> = entries
            .iter()
            .filter_map(|e| match e {
                MenuEntry::Item { label, checked, .. } => Some((label.as_str(), *checked)),
                MenuEntry::Separator => None,
            })
            .collect();
        assert_eq!(
            ticks,
            [
                ("Author", Some(true)),
                ("Age", Some(false)),
                ("Date", Some(true)),
                ("Commit", Some(true))
            ]
        );
        let toggles: Vec<MenuAction> = GraphColumn::ALL
            .into_iter()
            .map(MenuAction::ToggleColumn)
            .collect();
        assert_eq!(actions(&entries), toggles.iter().collect::<Vec<_>>());
    }

    #[test]
    fn submodule_menu_opens_or_initializes() {
        let sub = |initialized| SubmoduleInfo {
            name: "libs/foo".into(),
            path: RepoPath::new("/repo/libs/foo"),
            initialized,
            pinned: None,
            checked_out: None,
        };
        let entries = menu_entries(&MenuTarget::Submodule(sub(true)), None, |_| true);
        let initialized = actions(&entries);
        assert!(initialized.contains(&&MenuAction::OpenSubmodule(sub(true).path)));
        assert!(initialized.contains(&&MenuAction::UpdateSubmodule {
            name: "libs/foo".into(),
            init: false
        }));
        let entries = menu_entries(&MenuTarget::Submodule(sub(false)), None, |_| true);
        let uninitialized = actions(&entries);
        assert!(
            !uninitialized
                .iter()
                .any(|a| matches!(a, MenuAction::OpenSubmodule(_)))
        );
        assert!(uninitialized.contains(&&MenuAction::UpdateSubmodule {
            name: "libs/foo".into(),
            init: true
        }));
    }

    #[test]
    fn commit_menu_offers_deleting_its_refs_but_not_the_current_branch() {
        let oid = Oid::from_bytes(&[2; 20]).unwrap();
        let refs = vec![
            tip("main", RefKind::LocalBranch),
            tip("feat", RefKind::LocalBranch),
            tip("v1", RefKind::Tag),
            tip("origin/main", RefKind::RemoteBranch),
        ];
        let entries = menu_entries(&MenuTarget::Commit(oid, refs), Some("main"), |_| true);
        let actions = actions(&entries);
        assert!(actions.contains(&&MenuAction::Name(NameForm::Branch(Some(oid)))));
        assert!(actions.contains(&&MenuAction::Name(NameForm::Tag(oid))));
        assert!(actions.contains(&&MenuAction::DeleteBranch("feat".into(), DeleteMode::Safe)));
        assert!(actions.contains(&&MenuAction::DeleteTag("v1".into())));
        assert!(!actions.iter().any(|a| matches!(
            a,
            MenuAction::DeleteBranch(n, _) if n == "main"
        )));
        assert!(
            !actions
                .iter()
                .any(|a| matches!(a, MenuAction::DeleteRemoteBranch(_)))
        );
    }

    #[test]
    fn commit_menu_offers_squashing_up_to_head_except_on_head() {
        let oid = Oid::from_bytes(&[4; 20]).unwrap();
        let squashes = |refs: Vec<RefTip>| -> Vec<MenuAction> {
            let entries = menu_entries(&MenuTarget::Commit(oid, refs), Some("main"), |_| true);
            actions(&entries)
                .into_iter()
                .filter(|a| matches!(a, MenuAction::Squash(_)))
                .cloned()
                .collect()
        };
        assert_eq!(
            squashes(Vec::new()),
            [
                MenuAction::Squash(SquashRange::IntoParent(oid)),
                MenuAction::Squash(SquashRange::UpToHead(oid)),
            ]
        );
        assert_eq!(
            squashes(vec![
                tip("HEAD", RefKind::Head),
                tip("main", RefKind::LocalBranch)
            ]),
            [MenuAction::Squash(SquashRange::IntoParent(oid))]
        );
    }

    #[test]
    fn several_commits_menu_squashes_them() {
        let oids = vec![
            Oid::from_bytes(&[5; 20]).unwrap(),
            Oid::from_bytes(&[6; 20]).unwrap(),
        ];
        let entries = menu_entries(&MenuTarget::Commits(oids.clone()), Some("main"), |_| true);
        assert_eq!(
            actions(&entries)[0],
            &MenuAction::Squash(SquashRange::Selected(oids))
        );
    }

    #[test]
    fn current_branch_menu_has_no_checkout_merge_or_delete() {
        let entries = menu_entries(
            &MenuTarget::Ref(tip("main", RefKind::LocalBranch)),
            Some("main"),
            |_| true,
        );
        for a in actions(&entries) {
            assert!(
                !matches!(
                    a,
                    MenuAction::Checkout(_)
                        | MenuAction::CherryPick(_)
                        | MenuAction::Merge(_)
                        | MenuAction::Rebase(_)
                        | MenuAction::DeleteBranch(..)
                ),
                "{a:?}"
            );
        }
    }

    #[test]
    fn other_branch_menu_checks_out_merges_and_deletes() {
        let entries = menu_entries(
            &MenuTarget::Ref(tip("feat", RefKind::LocalBranch)),
            Some("main"),
            |_| true,
        );
        let actions = actions(&entries);
        assert!(
            actions.contains(&&MenuAction::Checkout(CheckoutTarget::Branch(
                "feat".into()
            )))
        );
        assert!(actions.contains(&&MenuAction::Merge("feat".into())));
        assert!(actions.contains(&&MenuAction::CherryPick(
            tip("feat", RefKind::LocalBranch).target
        )));
        assert!(actions.contains(&&MenuAction::Name(NameForm::Rename("feat".into()))));
        assert!(actions.contains(&&MenuAction::DeleteBranch("feat".into(), DeleteMode::Force)));
    }

    #[test]
    fn dropping_a_branch_offers_merge_and_rebase() {
        let entries = |s: RefTip, t: RefTip| {
            let entries = drop_entries(&s, &t, Some("main"));
            entries
                .into_iter()
                .filter_map(|e| match e {
                    MenuEntry::Item { action, .. } => Some(action),
                    MenuEntry::Separator => None,
                })
                .collect::<Vec<_>>()
        };
        let main = tip("main", RefKind::LocalBranch);
        let feat = tip("feat", RefKind::LocalBranch);
        let remote = tip("origin/feat", RefKind::RemoteBranch);
        // feat dropped on the current branch: merge into it, or checkout
        // feat and rebase it.
        assert_eq!(
            entries(feat.clone(), main.clone()),
            vec![
                MenuAction::Merge("feat".into()),
                MenuAction::RebaseBranch {
                    branch: "feat".into(),
                    onto: "main".into()
                },
            ]
        );
        // The current branch dropped on feat.
        assert_eq!(
            entries(main.clone(), feat.clone()),
            vec![
                MenuAction::MergeInto {
                    rev: "main".into(),
                    into: "feat".into()
                },
                MenuAction::Rebase("feat".into()),
            ]
        );
        // A remote branch cannot be rebased nor merged into.
        assert_eq!(
            entries(remote.clone(), main.clone()),
            vec![MenuAction::Merge("origin/feat".into())]
        );
        assert_eq!(
            entries(main.clone(), remote.clone()),
            vec![MenuAction::Rebase("origin/feat".into())]
        );
        assert!(entries(main.clone(), main).is_empty());
    }

    #[test]
    fn tag_and_remote_menus() {
        let tag = menu_entries(&MenuTarget::Ref(tip("v1", RefKind::Tag)), None, |_| false);
        let tag = actions(&tag);
        assert!(tag.contains(&&MenuAction::PushTag("v1".into())));
        assert!(tag.contains(&&MenuAction::DeleteTag("v1".into())));
        let remote = menu_entries(
            &MenuTarget::Ref(tip("origin/feat", RefKind::RemoteBranch)),
            Some("main"),
            |_| false,
        );
        let remote = actions(&remote);
        assert!(
            remote.contains(&&MenuAction::Checkout(CheckoutTarget::TrackRemote(
                "origin/feat".into()
            )))
        );
        assert!(remote.contains(&&MenuAction::DeleteRemoteBranch("origin/feat".into())));
    }

    #[test]
    fn only_losing_or_remote_actions_ask_first() {
        let oid = Oid::from_bytes(&[3; 20]).unwrap();
        assert!(
            MenuAction::Reset(oid, ResetMode::Hard)
                .confirmation()
                .is_some()
        );
        assert!(
            MenuAction::Reset(oid, ResetMode::Mixed)
                .confirmation()
                .is_none()
        );
        assert!(
            MenuAction::DeleteBranch("x".into(), DeleteMode::Force)
                .confirmation()
                .is_some()
        );
        assert!(
            MenuAction::DeleteBranch("x".into(), DeleteMode::Safe)
                .confirmation()
                .is_none()
        );
        assert!(
            MenuAction::DeleteRemoteBranch("origin/x".into())
                .confirmation()
                .is_some()
        );
        assert!(MenuAction::DeleteTag("v1".into()).confirmation().is_none());
    }
}
