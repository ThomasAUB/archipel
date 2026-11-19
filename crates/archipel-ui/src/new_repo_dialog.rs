//! The New Repository dialog: `git init` in a local folder, its files
//! optionally committed, the repository added to the project (issue #68).

use std::path::PathBuf;
use std::sync::Arc;

use archipel_app::{App as Core, CreateRepoRequest};
use archipel_theme::ColorToken as T;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::prelude::*;
use gpui_kit::{Context, Entity, EventEmitter, KeyDownEvent, Subscription, Window, div};

use crate::theme::color;
use crate::widgets::{button, check, dialog_frame, field, primary_button};

/// What the dialog reports to the workspace.
pub enum NewRepoEvent {
    /// Closed (cancelled or done).
    Closed,
}

/// The New Repository dialog.
pub struct NewRepoDialog {
    core: Arc<Core>,
    folder: Entity<InputState>,
    branch: Entity<InputState>,
    initial_commit: bool,
    /// Creating: the buttons wait.
    busy: bool,
    error: Option<String>,
    _subs: Vec<Subscription>,
}

impl EventEmitter<NewRepoEvent> for NewRepoDialog {}

impl NewRepoDialog {
    /// The dialog, its folder starting at the project's folder.
    pub fn new(core: Arc<Core>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let start = core
            .default_clone_folder()
            .map(|f| f.display().to_string())
            .unwrap_or_default();
        let folder = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Folder: a new one, or one holding files already")
                .default_value(start)
        });
        let branch = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Git's default (init.defaultBranch)")
        });
        folder.update(cx, |f, cx| f.focus(window, cx));
        let on_input = |this: &mut Self,
                        _: &Entity<InputState>,
                        e: &InputEvent,
                        window: &mut Window,
                        cx: &mut Context<Self>| {
            match e {
                InputEvent::Change => {
                    this.error = None;
                    cx.notify();
                }
                InputEvent::PressEnter { .. } => this.create(window, cx),
                _ => {}
            }
        };
        let subs = vec![
            cx.subscribe_in(&folder, window, on_input),
            cx.subscribe_in(&branch, window, on_input),
        ];
        Self {
            core,
            folder,
            branch,
            initial_commit: true,
            busy: false,
            error: None,
            _subs: subs,
        }
    }

    fn request(&self, cx: &gpui_kit::App) -> CreateRepoRequest {
        let branch = self.branch.read(cx).value().trim().to_owned();
        CreateRepoRequest {
            dir: PathBuf::from(self.folder.read(cx).value().trim()),
            initial_branch: (!branch.is_empty()).then_some(branch),
            initial_commit: self.initial_commit,
        }
    }

    fn create(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let request = self.request(cx);
        if let Some(why) = Core::create_repo_refusal(&request.dir) {
            self.error = Some(why);
            cx.notify();
            return;
        }
        self.busy = true;
        self.error = None;
        cx.notify();
        let core = self.core.clone();
        let task = cx.background_spawn(async move { core.create_repo(&request) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |d, cx| {
                d.busy = false;
                match result {
                    Ok(_) => cx.emit(NewRepoEvent::Closed),
                    Err(e) => d.error = Some(e.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    #[allow(clippy::unused_self)] // called like every other view action
    fn pick_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(gpui_kit::PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: None,
        });
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(paths))) = rx.await
                && let Some(path) = paths.into_iter().next()
            {
                let _ = this.update_in(cx, |d, window, cx| {
                    d.folder.update(cx, |f, cx| {
                        f.set_value(path.display().to_string(), window, cx);
                    });
                    cx.notify();
                });
            }
        })
        .detach();
    }
}

impl Render for NewRepoDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        dialog_frame("new-repo", "New repository".to_owned(), cx)
            .on_key_down(cx.listener(|_, e: &KeyDownEvent, _, cx| {
                if e.keystroke.key == "escape" {
                    cx.stop_propagation();
                    cx.emit(NewRepoEvent::Closed);
                }
            }))
            .child(field(
                "Folder",
                div()
                    .flex()
                    .gap_2()
                    .child(div().flex_1().min_w_0().child(Input::new(&self.folder)))
                    .child(
                        button("new-repo-pick", "…", cx)
                            .on_click(cx.listener(|d, _, window, cx| d.pick_folder(window, cx))),
                    ),
                cx,
            ))
            .child(field("Branch", Input::new(&self.branch), cx))
            .child(
                check(
                    "new-repo-commit",
                    "Commit the files already in the folder",
                    self.initial_commit,
                    cx,
                )
                .on_click(cx.listener(|d, _, _, cx| {
                    d.initial_commit = !d.initial_commit;
                    cx.notify();
                })),
            )
            .children(
                self.error
                    .clone()
                    .map(|e| div().text_color(color(cx, T::StatusError)).child(e)),
            )
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        button("new-repo-cancel", "Cancel", cx)
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(NewRepoEvent::Closed))),
                    )
                    .child(
                        primary_button(
                            "new-repo-create",
                            if self.busy { "Creating…" } else { "Create" },
                            cx,
                        )
                        .on_click(cx.listener(|d, _, window, cx| d.create(window, cx))),
                    ),
            )
    }
}
