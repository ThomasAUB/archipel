//! Git writes started from the tab (checkout, stash, branch deletion),
//! run on a background thread before the tab reloads.

use archipel_model::refs::{RefKind, RefTip};
use archipel_model::requests::{CheckoutTarget, DeleteMode, StashApplyMode};
use archipel_model::{Oid, RepoPath};
use archipel_ops::{OpClass, OpContext, OpError, RepoActions};
use gpui_kit::prelude::*;
use gpui_kit::{Context, Window};

use crate::staging;
use crate::widgets::SHORT_OID;

use super::menu::{MenuAction, is_not_merged};
use super::{RepoView, lock};

impl RepoView {
    /// Applies, pops (`Some`) or drops (`None`) the stash with commit id
    /// `oid`. Its index is looked up when the job runs: another stash may
    /// have been pushed since the list was read.
    pub(super) fn stash_op(
        &mut self,
        oid: Oid,
        mode: Option<StashApplyMode>,
        cx: &mut Context<Self>,
    ) {
        let label = match mode {
            Some(StashApplyMode::Pop) => "Pop stash",
            Some(_) => "Apply stash",
            None => "Drop stash",
        };
        self.run(label, cx, move |a, p, _| {
            let index = a
                .ports()
                .stashes
                .stashes(p)?
                .into_iter()
                .find(|s| s.oid == oid)
                .map(|s| s.index)
                .ok_or_else(|| OpError::Refused("the stash is gone".into()))?;
            match mode {
                Some(mode) => a.stash_apply(p, index, mode),
                None => a.stash_drop(p, index),
            }
        });
    }

    /// Checks out a ref (double click on a badge or in the refs panel).
    /// A remote branch with a local counterpart checks out that local
    /// branch, fast-forwarded to the remote tip when behind.
    pub(super) fn checkout_ref(&mut self, tip: &RefTip, cx: &mut Context<Self>) {
        match CheckoutTarget::for_ref(tip, |b| self.has_local(b)) {
            Some(CheckoutTarget::Branch(branch)) if tip.kind == RefKind::RemoteBranch => {
                self.checkout_updated(branch, tip.target, cx);
            }
            Some(target) => self.checkout(target, cx),
            None => {}
        }
    }

    /// Checks out local `branch`, fast-forwarded to `target` when behind.
    fn checkout_updated(&mut self, branch: String, target: Oid, cx: &mut Context<Self>) {
        self.run_noted(
            OpClass::Write,
            format!("Checkout {branch}"),
            cx,
            Box::new(move |a, p, _| {
                let (outcome, sync) = a.checkout_updated(p, &branch, target)?;
                let notes: Vec<String> = [sync.note(&branch), outcome.note()]
                    .into_iter()
                    .flatten()
                    .collect();
                Ok((!notes.is_empty()).then(|| notes.join("; ")))
            }),
        );
    }

    /// Checks out the commit of graph row `row`: through a branch pointing
    /// at it when there is one, detached otherwise.
    pub(super) fn checkout_row(&mut self, row: u32, cx: &mut Context<Self>) {
        let Some(graph_row) = self
            .graph
            .as_ref()
            .and_then(|g| lock(g).rows(row..row + 1).ok())
            .and_then(|mut r| r.pop())
        else {
            return;
        };
        // Through a remote branch, when the row has no local one: its
        // local counterpart is brought up to date as well.
        let refs = &graph_row.refs;
        if !refs.iter().any(|t| t.kind == RefKind::LocalBranch)
            && let Some(remote) = refs.iter().find(|t| t.kind == RefKind::RemoteBranch)
        {
            let remote = remote.clone();
            self.checkout_ref(&remote, cx);
            return;
        }
        let target =
            CheckoutTarget::for_commit(graph_row.oid, &graph_row.refs, |b| self.has_local(b));
        self.checkout(target, cx);
    }

    pub(super) fn checkout(&mut self, target: CheckoutTarget, cx: &mut Context<Self>) {
        let label = match &target {
            CheckoutTarget::Branch(b) => format!("Checkout {b}"),
            CheckoutTarget::TrackRemote(r) => format!("Checkout {r}"),
            CheckoutTarget::Detached(oid) => {
                format!("Checkout {} (detached)", oid.short(SHORT_OID))
            }
        };
        self.run_noted(
            OpClass::Write,
            label,
            cx,
            Box::new(move |a, p, _| a.checkout_autostash(p, &target).map(|o| o.note())),
        );
    }

    /// Runs a write on a background thread, then reloads.
    pub(super) fn run(
        &mut self,
        label: impl Into<String>,
        cx: &mut Context<Self>,
        job: impl FnOnce(&RepoActions, &RepoPath, &OpContext) -> Result<(), OpError> + Send + 'static,
    ) {
        self.run_as(OpClass::Write, label, cx, job);
    }

    pub(super) fn run_as(
        &mut self,
        class: OpClass,
        label: impl Into<String>,
        cx: &mut Context<Self>,
        job: impl FnOnce(&RepoActions, &RepoPath, &OpContext) -> Result<(), OpError> + Send + 'static,
    ) {
        self.run_noted(class, label, cx, staging::noted(Box::new(job)));
    }

    /// Like [`Self::run_as`], for a job whose note replaces "done".
    pub(super) fn run_noted(
        &mut self,
        class: OpClass,
        label: impl Into<String>,
        cx: &mut Context<Self>,
        job: staging::NotedJob,
    ) {
        let label = label.into();
        self.status = Some(format!("{label}…"));
        staging::submit_job(
            &self.core,
            &self.path,
            class,
            label,
            job,
            cx,
            |v, status, cx| {
                v.status = Some(status);
                v.reload(cx);
                cx.notify();
            },
        );
        cx.notify();
    }

    /// Deletes local branch `name` with `git branch -d`. Git refuses a
    /// branch that is not merged (typical once its PR was squash-merged or
    /// its remote branch deleted): the force delete is then offered instead
    /// of leaving only a status line.
    pub(super) fn delete_branch_safe(
        &mut self,
        name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let label = format!("Delete branch {name}");
        let actions = self.core.repo_actions().clone();
        let path = self.path.clone();
        let branch = name.clone();
        let task = cx.background_spawn(async move {
            actions.delete_branch(&path, &branch, DeleteMode::Safe, None)
        });
        self.status = Some(format!("{label}…"));
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |v, window, cx| {
                match result {
                    Ok(()) => v.status = Some(format!("{label}: done")),
                    Err(e) if is_not_merged(&e) => {
                        v.status = None;
                        v.choose(
                            MenuAction::DeleteBranch(name, DeleteMode::Force),
                            window,
                            cx,
                        );
                    }
                    Err(e) => v.status = Some(format!("{label} failed: {e}")),
                }
                v.reload(cx);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}
