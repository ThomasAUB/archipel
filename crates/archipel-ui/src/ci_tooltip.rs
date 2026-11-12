//! The CI tooltip of the dashboard "CI" cell and of the CI Badge (spec §8):
//! the run and its jobs, then the last error lines of each failed job's
//! log, read on the first hover and kept by the app, with "Open the full
//! log" and "Rerun failed jobs". It is hoverable, so its links can be
//! clicked.

use std::sync::Arc;

use archipel_app::{App as Core, Command};
use archipel_model::RepoId;
use archipel_model::forge::{CiJobLog, CiRun};
use archipel_theme::ColorToken as T;
use gpui_kit::prelude::*;
use gpui_kit::{AnyView, App, AppContext, Context, SharedString, Window, div, px};

use crate::theme::{color, mono_font, radius, text_size, ui_font};

/// Failed jobs whose log is shown at most.
const MAX_LOGS: usize = 3;

/// What is known of one failed job's log.
enum LogState {
    Loading,
    Loaded(CiJobLog),
    Failed(String),
}

struct JobLog {
    name: String,
    url: Option<String>,
    state: LogState,
}

/// The tooltip view.
pub struct CiTooltip {
    core: Arc<Core>,
    repo: RepoId,
    lines: Vec<SharedString>,
    logs: Vec<JobLog>,
    /// The run has failed jobs: offer to rerun them.
    rerun: bool,
    /// The rerun was asked from this tooltip.
    rerun_sent: bool,
}

impl CiTooltip {
    /// The view a `.hoverable_tooltip(...)` builder returns: `lines` (as
    /// [`archipel_app::forge::ci_tooltip`] gives them), then the logs of
    /// the failed jobs of `run`, read in the background when not known yet.
    pub fn view(
        core: Arc<Core>,
        repo: RepoId,
        lines: &[String],
        run: Option<&CiRun>,
        cx: &mut App,
    ) -> AnyView {
        let lines = lines.iter().cloned().map(SharedString::from).collect();
        let failed: Vec<_> = run
            .map(|r| r.failed_jobs().cloned().collect())
            .unwrap_or_default();
        cx.new(|cx: &mut Context<Self>| {
            let mut logs = Vec::new();
            for job in failed.iter().filter(|j| j.id.is_some()).take(MAX_LOGS) {
                let Some(id) = job.id else { continue };
                let state = if let Some(log) = core.cached_ci_log(repo, id) {
                    LogState::Loaded(log)
                } else {
                    Self::fetch(&core, repo, job.clone(), logs.len(), cx);
                    LogState::Loading
                };
                logs.push(JobLog {
                    name: job.name.clone(),
                    url: job.url.clone(),
                    state,
                });
            }
            Self {
                core,
                repo,
                lines,
                logs,
                rerun: !failed.is_empty(),
                rerun_sent: false,
            }
        })
        .into()
    }

    /// Reads the log of `job` (shown at `index`) on the app runtime.
    fn fetch(
        core: &Arc<Core>,
        repo: RepoId,
        job: archipel_model::forge::CiJob,
        index: usize,
        cx: &mut Context<Self>,
    ) {
        let app = core.clone();
        let handle = core
            .scheduler()
            .runtime()
            .spawn(async move { app.ci_job_log(repo, &job).await });
        cx.spawn(async move |this, cx| {
            let state = match handle.await {
                Ok(Ok(log)) => LogState::Loaded(log),
                Ok(Err(e)) => LogState::Failed(e.to_string()),
                Err(e) => LogState::Failed(e.to_string()),
            };
            let _ = this.update(cx, |t, cx| {
                if let Some(log) = t.logs.get_mut(index) {
                    log.state = state;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn link(
        id: impl Into<gpui_kit::ElementId>,
        label: &'static str,
        cx: &Context<Self>,
    ) -> gpui_kit::Stateful<gpui_kit::Div> {
        let hover = color(cx, T::BgHover);
        div()
            .id(id)
            .px_1()
            .rounded(radius(cx))
            .text_color(color(cx, T::AccentText))
            .cursor_pointer()
            .hover(move |s| s.bg(hover))
            .child(label)
    }

    fn render_log(i: usize, log: &JobLog, cx: &Context<Self>) -> gpui_kit::AnyElement {
        let subtle = color(cx, T::FgSubtle);
        let body = match &log.state {
            LogState::Loading => div()
                .text_color(subtle)
                .child("Reading the log…")
                .into_any_element(),
            LogState::Failed(e) => div()
                .text_color(color(cx, T::StatusError))
                .child(format!("Cannot read the log: {e}"))
                .into_any_element(),
            LogState::Loaded(l) if l.excerpt.is_empty() => div()
                .text_color(subtle)
                .child("The log is empty")
                .into_any_element(),
            LogState::Loaded(l) => div()
                .flex()
                .flex_col()
                .px_2()
                .py_1()
                .rounded(radius(cx))
                .bg(color(cx, T::BgPanel))
                .font_family(mono_font(cx))
                .text_xs()
                .children(l.excerpt.iter().map(|line| {
                    let gap = line == CiJobLog::GAP;
                    div()
                        .when(gap, |d| d.text_color(subtle))
                        .child(SharedString::from(line.clone()))
                }))
                .into_any_element(),
        };
        let what = match &log.state {
            LogState::Loaded(l) if !l.matched => "end of the log",
            _ => "last error lines",
        };
        div()
            .flex()
            .flex_col()
            .gap_1()
            .pt_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .text_color(color(cx, T::StatusError))
                            .child(format!("✗ {}", log.name)),
                    )
                    .child(div().text_color(subtle).child(what))
                    .when_some(log.url.clone(), |d, url| {
                        d.child(
                            Self::link(("ci-log-open", i), "Open the full log", cx)
                                .on_click(move |_, _, cx| cx.open_url(&url)),
                        )
                    }),
            )
            .child(body)
            .into_any_element()
    }
}

impl Render for CiTooltip {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let logs: Vec<_> = self
            .logs
            .iter()
            .enumerate()
            .map(|(i, log)| Self::render_log(i, log, cx))
            .collect();
        let rerun =
            self.rerun.then(|| {
                if self.rerun_sent {
                    div()
                        .pt_2()
                        .text_color(color(cx, T::FgSubtle))
                        .child("Rerun asked: see the operations drawer")
                        .into_any_element()
                } else {
                    div()
                        .pt_2()
                        .flex()
                        .child(Self::link("ci-rerun", "Rerun failed jobs", cx).on_click(
                            cx.listener(|t, _, _, cx| {
                                if let Err(e) =
                                    t.core.dispatch(Command::RerunFailedCi(vec![t.repo]))
                                {
                                    tracing::warn!(error = %e, "cannot rerun the CI");
                                }
                                t.rerun_sent = true;
                                cx.notify();
                            }),
                        ))
                        .into_any_element()
                }
            });
        div()
            .px_2()
            .py_1()
            .flex()
            .flex_col()
            .max_w(px(760.0))
            .font_family(ui_font(cx))
            .font_weight(crate::theme::ui_weight(cx))
            .text_size(text_size(cx))
            .text_color(color(cx, T::FgDefault))
            .bg(color(cx, T::BgElevated))
            .border_1()
            .border_color(color(cx, T::Border))
            .rounded(radius(cx))
            .shadow_md()
            .children(self.lines.iter().map(|line| div().child(line.clone())))
            .children(logs)
            .children(rerun)
    }
}
