//! Per-repo forge status in the dashboard (spec §8): the [`ForgeStatus`] of
//! a row, the "Pull request" column and the forge filters, registered like
//! any other column / filter.

use std::fmt::Write as _;

use archipel_model::Timestamp;
use archipel_model::forge::{CiState, PullRequestRef, PullRequestStatus, ReviewState};

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

/// Filter: repos whose pull request CI fails.
pub struct CiFailingFilter;

impl RepoFilter for CiFailingFilter {
    fn id(&self) -> &str {
        "ci-failing"
    }
    fn label(&self) -> &str {
        "CI failing"
    }
    fn matches(&self, row: &DashboardRow) -> bool {
        row.forge
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
        }
    }

    fn pr(ci: Option<CiState>, review: ReviewState) -> PullRequestStatus {
        PullRequestStatus {
            number: 12,
            title: "Add x".into(),
            url: "u".into(),
            draft: false,
            target: "main".into(),
            ci,
            review,
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
