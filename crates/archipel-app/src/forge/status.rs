//! Per-repo forge status in the dashboard (spec §8): the [`ForgeStatus`] and
//! [`BranchCi`] of a row, the "Pull request" and "CI" columns and the forge
//! filters, registered like any other column / filter.

use std::fmt::Write as _;

use archipel_model::Timestamp;
use archipel_model::forge::{
    CiJob, CiJobState, CiRun, CiState, PullRequestRef, PullRequestStatus, ReviewState,
};

use crate::dashboard::{Cell, DashboardColumn, DashboardRow, RepoFilter, SortKey, Tone};

/// What the dashboard knows about a repo on its forge.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum ForgeStatus {
    /// Not read yet.
    #[default]
    Unknown,
    /// No remote on a known forge, or no branch checked out.
    NotOnForge,
    /// On a recognized forge, not signed in to its host.
    SignedOut {
        /// Host to sign in to.
        host: String,
    },
    /// No open pull request for the current branch.
    NoPullRequest {
        /// The branch.
        branch: String,
    },
    /// The open pull request of the current branch.
    Open(PullRequestStatus),
    /// Reading the status failed.
    Error(String),
}

impl ForgeStatus {
    /// The open pull request, if any.
    #[must_use]
    pub fn pull_request(&self) -> Option<&PullRequestStatus> {
        match self {
            Self::Open(pr) => Some(pr),
            _ => None,
        }
    }
}

/// What the dashboard knows about the CI of a repo's current branch.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum BranchCi {
    /// Not read yet, or the repo is not on a forge the user signed in to
    /// (the "Pull request" status says which).
    #[default]
    Unknown,
    /// The branch is not on the forge, or has no CI.
    None,
    /// The latest CI run of the branch head.
    Run(CiRun),
    /// Reading the CI failed.
    Error(String),
}

impl BranchCi {
    /// The CI run, if any.
    #[must_use]
    pub fn run(&self) -> Option<&CiRun> {
        match self {
            Self::Run(run) => Some(run),
            _ => None,
        }
    }
}

/// Mark of a job state, as in the cells (`✓` passed, `✗` failed…).
#[must_use]
pub fn job_mark(state: CiJobState) -> &'static str {
    match state {
        CiJobState::Success => "✓",
        CiJobState::Failed => "✗",
        CiJobState::Canceled => "⊘",
        CiJobState::Running => "●",
        CiJobState::Queued => "◌",
        CiJobState::Manual => "▷",
        CiJobState::Skipped => "–",
    }
}

/// Short job state label.
#[must_use]
pub fn job_label(state: CiJobState) -> &'static str {
    match state {
        CiJobState::Success => "passed",
        CiJobState::Failed => "failed",
        CiJobState::Canceled => "canceled",
        CiJobState::Running => "running",
        CiJobState::Queued => "queued",
        CiJobState::Manual => "manual",
        CiJobState::Skipped => "skipped",
    }
}

/// Short CI label.
#[must_use]
pub fn ci_label(ci: Option<CiState>) -> &'static str {
    match ci {
        None => "no CI",
        Some(CiState::Pending) => "CI running",
        Some(CiState::Success) => "CI passed",
        Some(CiState::Failure) => "CI failed",
    }
}

/// Short review label.
#[must_use]
pub fn review_label(review: ReviewState) -> &'static str {
    match review {
        ReviewState::Pending => "review pending",
        ReviewState::Approved => "approved",
        ReviewState::ChangesRequested => "changes requested",
    }
}

/// The dashboard column "Pull request": number, CI and review state.
pub struct ForgeColumn;

impl ForgeColumn {
    fn open_cell(pr: &PullRequestStatus) -> Cell {
        let ci = match pr.ci {
            None => "",
            Some(CiState::Pending) => " ●",
            Some(CiState::Success) => " ✓",
            Some(CiState::Failure) => " ✗",
        };
        let review = match pr.review {
            ReviewState::Pending => "",
            ReviewState::Approved => " approved",
            ReviewState::ChangesRequested => " changes",
        };
        let draft = if pr.draft { " draft" } else { "" };
        let tone = match (pr.ci, pr.review) {
            (Some(CiState::Failure), _) => Tone::Error,
            (_, ReviewState::ChangesRequested) => Tone::Conflict,
            (Some(CiState::Pending), _) => Tone::Warning,
            (_, ReviewState::Approved) => Tone::Success,
            _ => Tone::Normal,
        };
        Cell {
            text: format!("#{}{ci}{review}{draft}", pr.number),
            detail: Some(format!(
                "{} → {} · {} · {}",
                pr.title,
                pr.target,
                ci_label(pr.ci),
                review_label(pr.review)
            )),
            tone,
        }
    }
}

impl DashboardColumn for ForgeColumn {
    fn id(&self) -> &str {
        "forge"
    }

    fn header(&self) -> &str {
        "Pull request"
    }

    fn cell(&self, row: &DashboardRow, _now: Timestamp) -> Cell {
        let muted = |text: &str, detail: Option<String>| Cell {
            text: text.into(),
            detail,
            tone: Tone::Muted,
        };
        match &row.forge {
            ForgeStatus::Unknown => muted("", None),
            ForgeStatus::NotOnForge => muted("—", None),
            ForgeStatus::SignedOut { host } => muted(
                "sign in",
                Some(format!(
                    "Not signed in to {host}: “Forge accounts…” in the palette"
                )),
            ),
            ForgeStatus::NoPullRequest { branch } => {
                muted("none", Some(format!("No open pull request for {branch}")))
            }
            ForgeStatus::Open(pr) => Self::open_cell(pr),
            ForgeStatus::Error(e) => Cell {
                text: "error".into(),
                detail: Some(e.clone()),
                tone: Tone::Error,
            },
        }
    }

    fn sort_key(&self, row: &DashboardRow) -> SortKey {
        // Pull requests needing attention first: failing CI, changes
        // requested, running, then the others.
        row.forge.pull_request().map_or(SortKey::None, |pr| {
            let rank = match (pr.ci, pr.review) {
                (Some(CiState::Failure), _) => 0,
                (_, ReviewState::ChangesRequested) => 1,
                (Some(CiState::Pending), _) => 2,
                _ => 3,
            };
            SortKey::Number(rank)
        })
    }
}

/// Most jobs listed in the CI tooltip.
const MAX_TOOLTIP_JOBS: usize = 25;

/// The dashboard column "CI": the latest run of the current branch, with
/// its jobs in the tooltip.
pub struct CiColumn;

impl CiColumn {
    fn run_cell(run: &CiRun) -> Cell {
        let total = run.jobs.len();
        let count = |f: fn(CiJobState) -> bool| run.jobs.iter().filter(|j| f(j.state)).count();
        let (text, tone) = match run.state {
            CiState::Failure => {
                let failed = count(CiJobState::is_failure);
                (
                    if failed == 0 {
                        "✗ failed".to_owned()
                    } else {
                        format!("✗ {failed}/{total} failed")
                    },
                    Tone::Error,
                )
            }
            CiState::Pending => {
                let done = total - count(CiJobState::is_pending);
                (
                    if total == 0 {
                        "● running".to_owned()
                    } else {
                        format!("● {done}/{total}")
                    },
                    Tone::Warning,
                )
            }
            CiState::Success => (
                if total == 0 {
                    "✓ passed".to_owned()
                } else {
                    format!("✓ {total}")
                },
                Tone::Success,
            ),
        };
        Cell {
            text,
            detail: Some(ci_label(Some(run.state)).to_owned()),
            tone,
        }
    }

    /// Attention order of a job in the tooltip: failures first, then what
    /// is still running or waiting, then the rest.
    fn job_rank(job: &CiJob) -> u8 {
        match job.state {
            CiJobState::Failed | CiJobState::Canceled => 0,
            CiJobState::Running => 1,
            CiJobState::Queued | CiJobState::Manual => 2,
            CiJobState::Success | CiJobState::Skipped => 3,
        }
    }

    fn job_line(job: &CiJob) -> String {
        let stage = job
            .stage
            .as_deref()
            .map_or_else(String::new, |s| format!(" ({s})"));
        format!(
            "{} {}{stage} · {}",
            job_mark(job.state),
            job.name,
            job_label(job.state)
        )
    }
}

impl DashboardColumn for CiColumn {
    fn id(&self) -> &str {
        "ci"
    }

    fn header(&self) -> &str {
        "CI"
    }

    fn cell(&self, row: &DashboardRow, _now: Timestamp) -> Cell {
        ci_cell(&row.ci, &row.forge)
    }

    fn tooltip(&self, row: &DashboardRow) -> Option<Vec<String>> {
        ci_tooltip(&row.ci, &row.forge)
    }

    fn sort_key(&self, row: &DashboardRow) -> SortKey {
        // Failing first, then running, then passed.
        row.ci.run().map_or(SortKey::None, |run| {
            SortKey::Number(match run.state {
                CiState::Failure => 0,
                CiState::Pending => 1,
                CiState::Success => 2,
            })
        })
    }
}

/// The CI of a branch as a cell: the "CI" column, also shown in the
/// repo view.
#[must_use]
pub fn ci_cell(ci: &BranchCi, forge: &ForgeStatus) -> Cell {
    let muted = |text: &str| Cell {
        text: text.into(),
        detail: None,
        tone: Tone::Muted,
    };
    match (ci, forge) {
        (BranchCi::Run(run), _) => CiColumn::run_cell(run),
        (BranchCi::Error(e), _) => Cell {
            text: "error".into(),
            detail: Some(e.clone()),
            tone: Tone::Error,
        },
        (BranchCi::None, _) | (BranchCi::Unknown, ForgeStatus::NotOnForge) => muted("—"),
        (BranchCi::Unknown, ForgeStatus::SignedOut { .. }) => muted("sign in"),
        (BranchCi::Unknown, _) => muted(""),
    }
}

/// The tooltip of [`ci_cell`]: the run and its jobs, failures first.
#[must_use]
pub fn ci_tooltip(ci: &BranchCi, forge: &ForgeStatus) -> Option<Vec<String>> {
    match (ci, forge) {
        (BranchCi::Run(run), _) => {
            let short: String = run.sha.chars().take(8).collect();
            let mut lines = vec![format!("{} · {short}", ci_label(Some(run.state)))];
            let mut jobs: Vec<&CiJob> = run.jobs.iter().collect();
            jobs.sort_by_key(|j| CiColumn::job_rank(j));
            lines.extend(
                jobs.iter()
                    .take(MAX_TOOLTIP_JOBS)
                    .map(|j| CiColumn::job_line(j)),
            );
            if jobs.len() > MAX_TOOLTIP_JOBS {
                lines.push(format!("… and {} more", jobs.len() - MAX_TOOLTIP_JOBS));
            }
            Some(lines)
        }
        (BranchCi::Error(e), _) => Some(vec![e.clone()]),
        (BranchCi::None, _) => Some(vec![
            "No CI run for this branch on the forge (not pushed, or no CI)".into(),
        ]),
        (BranchCi::Unknown, ForgeStatus::SignedOut { host }) => Some(vec![format!(
            "Not signed in to {host}: “Forge accounts…” in the palette"
        )]),
        _ => None,
    }
}

/// Filter: repos with an open pull request.
pub struct PullRequestOpenFilter;

impl RepoFilter for PullRequestOpenFilter {
    fn id(&self) -> &str {
        "pr-open"
    }
    fn label(&self) -> &str {
        "Open pull request"
    }
    fn matches(&self, row: &DashboardRow) -> bool {
        row.forge.pull_request().is_some()
    }
}

/// Filter: repos whose branch or pull request CI fails.
pub struct CiFailingFilter;

impl RepoFilter for CiFailingFilter {
    fn id(&self) -> &str {
        "ci-failing"
    }
    fn label(&self) -> &str {
        "CI failing"
    }
    fn matches(&self, row: &DashboardRow) -> bool {
        row.ci
            .run()
            .is_some_and(|run| run.state == CiState::Failure)
            || row
                .forge
                .pull_request()
                .is_some_and(|pr| pr.ci == Some(CiState::Failure))
    }
}

/// The description of each pull request of a multi-repo change: the
/// original text plus links to every pull request of the change.
#[must_use]
pub fn linked_description(description: &str, all: &[(String, PullRequestRef)]) -> String {
    let mut out = description.trim_end().to_owned();
    if !out.is_empty() {
        out.push_str("\n\n");
    }
    out.push_str("---\nPart of a change across several repositories:\n");
    for (path, pr) in all {
        // Writing to a String cannot fail.
        let _ = writeln!(out, "- [{path} #{}]({})", pr.number, pr.url);
    }
    out
}

#[cfg(test)]
mod tests {
    use archipel_model::RepoPath;
    use archipel_model::forge::MergeState;

    use super::*;
    use crate::dashboard::RepoStatus;

    fn row(forge: ForgeStatus) -> DashboardRow {
        let path = RepoPath::new("/p/a");
        DashboardRow {
            id: path.id(),
            name: "a".into(),
            path,
            status: RepoStatus::Loading,
            stale: false,
            hidden: false,
            forge,
            ci: BranchCi::Unknown,
        }
    }

    fn pr(ci: Option<CiState>, review: ReviewState) -> PullRequestStatus {
        PullRequestStatus {
            number: 12,
            title: "Add x".into(),
            url: "u".into(),
            draft: false,
            source: "feat".into(),
            target: "main".into(),
            head_sha: "abc".into(),
            author: "me".into(),
            ci,
            review,
            merge: MergeState::Unknown,
        }
    }

    #[test]
    fn cells_and_tones() {
        let now = Timestamp(0);
        let cell = |f| ForgeColumn.cell(&row(f), now);
        let c = cell(ForgeStatus::Open(pr(
            Some(CiState::Success),
            ReviewState::Approved,
        )));
        assert_eq!(c.text, "#12 ✓ approved");
        assert_eq!(c.tone, Tone::Success);
        assert_eq!(
            c.detail.as_deref(),
            Some("Add x → main · CI passed · approved")
        );
        let c = cell(ForgeStatus::Open(pr(
            Some(CiState::Failure),
            ReviewState::Approved,
        )));
        assert_eq!(c.tone, Tone::Error);
        let c = cell(ForgeStatus::Open(pr(None, ReviewState::ChangesRequested)));
        assert_eq!((c.text.as_str(), c.tone), ("#12 changes", Tone::Conflict));
        assert_eq!(
            cell(ForgeStatus::SignedOut {
                host: "github.com".into()
            })
            .text,
            "sign in"
        );
        assert_eq!(cell(ForgeStatus::Error("x".into())).tone, Tone::Error);
        assert!(CiFailingFilter.matches(&row(ForgeStatus::Open(pr(
            Some(CiState::Failure),
            ReviewState::Pending
        )))));
        assert!(!PullRequestOpenFilter.matches(&row(ForgeStatus::NotOnForge)));
    }

    fn job(name: &str, state: CiJobState) -> CiJob {
        CiJob {
            name: name.into(),
            stage: Some("test".into()),
            state,
            url: None,
        }
    }

    fn ci_row(state: CiState, jobs: Vec<CiJob>) -> DashboardRow {
        let mut r = row(ForgeStatus::NoPullRequest {
            branch: "main".into(),
        });
        r.ci = BranchCi::Run(CiRun {
            state,
            sha: "0123456789abcdef".into(),
            url: None,
            jobs,
        });
        r
    }

    #[test]
    fn ci_cells_count_jobs() {
        let now = Timestamp(0);
        let r = ci_row(
            CiState::Failure,
            vec![
                job("build", CiJobState::Success),
                job("lint", CiJobState::Failed),
                job("docs", CiJobState::Skipped),
            ],
        );
        let c = CiColumn.cell(&r, now);
        assert_eq!((c.text.as_str(), c.tone), ("✗ 1/3 failed", Tone::Error));
        let tip = CiColumn.tooltip(&r).unwrap();
        assert_eq!(tip[0], "CI failed · 01234567");
        assert_eq!(tip[1], "✗ lint (test) · failed", "failures first");
        assert!(CiFailingFilter.matches(&r));

        let r = ci_row(
            CiState::Pending,
            vec![
                job("build", CiJobState::Success),
                job("test", CiJobState::Running),
                job("deploy", CiJobState::Queued),
            ],
        );
        let c = CiColumn.cell(&r, now);
        assert_eq!((c.text.as_str(), c.tone), ("● 1/3", Tone::Warning));
        assert!(!CiFailingFilter.matches(&r));

        let r = ci_row(CiState::Success, vec![job("build", CiJobState::Success)]);
        assert_eq!(CiColumn.cell(&r, now).text, "✓ 1");

        let mut r = row(ForgeStatus::NotOnForge);
        assert_eq!(CiColumn.cell(&r, now).text, "—");
        r.ci = BranchCi::Error("boom".into());
        assert_eq!(CiColumn.cell(&r, now).tone, Tone::Error);
    }

    #[test]
    fn links_every_pull_request() {
        let all = vec![
            (
                "o/a".to_owned(),
                PullRequestRef {
                    number: 1,
                    url: "ua".into(),
                },
            ),
            (
                "o/b".to_owned(),
                PullRequestRef {
                    number: 2,
                    url: "ub".into(),
                },
            ),
        ];
        let d = linked_description("Body\n", &all);
        assert!(d.starts_with("Body\n\n---\n"));
        assert!(d.contains("- [o/a #1](ua)\n- [o/b #2](ub)\n"));
    }
}
