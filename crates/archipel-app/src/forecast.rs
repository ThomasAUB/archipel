//! The conflict forecast of a dashboard row (spec §3): whether the current
//! branch would conflict with its upstream (what a pull would do) and with
//! the target of its pull request (the default branch when there is none),
//! and the local changes a pull would touch. Shown as a badge in the Sync
//! column (files on hover), the Quick Edit Header and the Repo Toolbar, and
//! by the "Will conflict" filter.
//!
//! Computed in the background by the app after each refresh, cached by the
//! commit ids of both sides (`App::load_forecast`).

use archipel_model::forecast::MergeStrategy;
use archipel_model::{Oid, RepoSummary};

use crate::dashboard::{CellBadge, DashboardRow, RepoFilter, Tone};

/// What the current branch was checked against.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ForecastBase {
    /// Its upstream: what a pull would bring.
    Upstream,
    /// The target branch of its open pull request (number).
    PullRequest(u64),
    /// The default branch (no open pull request).
    DefaultBranch,
}

/// The forecast against one branch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Forecast {
    /// What it was checked against.
    pub base: ForecastBase,
    /// The branch checked against (`origin/main`).
    pub against: String,
    /// The local commit (`HEAD`).
    pub ours: Oid,
    /// The commit of `against`.
    pub theirs: Oid,
    /// The strategy asked: with `ours` and `theirs`, what the result is
    /// cached by.
    pub asked: MergeStrategy,
    /// The strategy checked (a rebase of too many commits is checked as a
    /// merge).
    pub strategy: MergeStrategy,
    /// The first conflicting paths, at most [`Forecast::MAX_PATHS`].
    pub conflicts: Vec<String>,
    /// Number of conflicting paths, including those not kept.
    pub total: u32,
    /// For the upstream: the paths a pull would change, at most
    /// [`Forecast::MAX_INCOMING`]. Empty otherwise.
    pub incoming: Vec<String>,
}

impl Forecast {
    /// Most conflicting paths kept.
    pub const MAX_PATHS: usize = 50;
    /// Most incoming paths kept.
    pub const MAX_INCOMING: usize = 5000;

    /// Whether it would conflict.
    #[must_use]
    pub fn conflicts(&self) -> bool {
        self.total > 0
    }

    /// Whether it was computed for the same commits and strategy.
    #[must_use]
    pub fn is_for(&self, ours: Oid, theirs: Oid, asked: MergeStrategy) -> bool {
        (self.ours, self.theirs, self.asked) == (ours, theirs, asked)
    }

    /// "origin/main (pull, rebase checked)".
    fn describe(&self) -> String {
        let what = match self.base {
            ForecastBase::Upstream => "pull".to_owned(),
            ForecastBase::PullRequest(n) => format!("pull request #{n}"),
            ForecastBase::DefaultBranch => "default branch".to_owned(),
        };
        format!(
            "{} ({what}, {} checked)",
            self.against,
            self.strategy.label()
        )
    }
}

/// The conflict forecast of a row; empty until computed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConflictForecast {
    /// Against the upstream, when the branch is behind it.
    pub upstream: Option<Forecast>,
    /// Against the pull request's target, or the default branch.
    pub target: Option<Forecast>,
}

impl ConflictForecast {
    /// The forecasts made.
    pub fn forecasts(&self) -> impl Iterator<Item = &Forecast> {
        self.upstream.iter().chain(&self.target)
    }

    /// Whether the branch would conflict with its upstream or its target.
    #[must_use]
    pub fn will_conflict(&self) -> bool {
        self.forecasts().any(Forecast::conflicts)
    }

    /// Number of distinct files that would conflict.
    #[must_use]
    pub fn conflict_count(&self) -> u32 {
        let mut paths: Vec<&str> = self
            .forecasts()
            .flat_map(|f| f.conflicts.iter().map(String::as_str))
            .collect();
        paths.sort_unstable();
        paths.dedup();
        let kept = u32::try_from(paths.len()).unwrap_or(u32::MAX);
        // Paths past the kept ones may overlap: the largest total is a floor.
        self.forecasts().map(|f| f.total).fold(kept, u32::max)
    }

    /// The locally changed files (of `summary`) a pull would change too.
    #[must_use]
    pub fn touched<'a>(&'a self, summary: &'a RepoSummary) -> Vec<&'a str> {
        let Some(upstream) = &self.upstream else {
            return Vec::new();
        };
        let incoming = &upstream.incoming;
        let brought = |path: &str| {
            // An untracked folder shows as `dir/`.
            if path.ends_with('/') {
                incoming.iter().any(|p| p.starts_with(path))
            } else {
                incoming.binary_search_by(|p| p.as_str().cmp(path)).is_ok()
            }
        };
        summary
            .changed_files
            .files
            .iter()
            .flat_map(|c| std::iter::once(c.path.as_str()).chain(c.original_path.as_deref()))
            .filter(|p| brought(p))
            .collect()
    }

    /// The badge: "will conflict · 2", else "touches 2 modified" when a
    /// pull would change locally modified files.
    #[must_use]
    pub fn badge(&self, summary: Option<&RepoSummary>) -> Option<CellBadge> {
        if self.will_conflict() {
            return Some(CellBadge {
                text: format!("will conflict · {}", self.conflict_count()),
                tone: Tone::Conflict,
            });
        }
        let touched = summary.map_or(0, |s| self.touched(s).len());
        (touched > 0).then(|| CellBadge {
            text: format!("touches {touched} modified"),
            tone: Tone::Warning,
        })
    }

    /// The hover lines of the badge: each conflicting forecast with its
    /// files, then the modified files a pull would touch.
    #[must_use]
    pub fn tooltip(&self, summary: Option<&RepoSummary>) -> Vec<String> {
        let mut lines = Vec::new();
        for f in self.forecasts().filter(|f| f.conflicts()) {
            lines.push(format!(
                "Will conflict with {}: {}",
                f.describe(),
                files(f.total)
            ));
            lines.extend(f.conflicts.iter().map(|p| format!("  {p}")));
            let omitted = f
                .total
                .saturating_sub(u32::try_from(f.conflicts.len()).unwrap_or(u32::MAX));
            if omitted > 0 {
                lines.push(format!("  … and {omitted} more"));
            }
        }
        if let Some(summary) = summary {
            let touched = self.touched(summary);
            if !touched.is_empty() {
                lines.push(format!(
                    "Pull would touch {} modified {}:",
                    touched.len(),
                    if touched.len() == 1 { "file" } else { "files" }
                ));
                lines.extend(touched.iter().map(|p| format!("  {p}")));
            }
        }
        lines
    }
}

fn files(n: u32) -> String {
    if n == 1 {
        "1 file".to_owned()
    } else {
        format!("{n} files")
    }
}

/// The filter chip "Will conflict".
pub struct WillConflictFilter;

impl RepoFilter for WillConflictFilter {
    fn id(&self) -> &str {
        "will-conflict"
    }
    fn label(&self) -> &str {
        "Will conflict"
    }
    fn matches(&self, row: &DashboardRow) -> bool {
        row.forecast.will_conflict()
    }
}

#[cfg(test)]
mod tests {
    use archipel_model::changes::{ChangeKind, FileChange, FileState};
    use archipel_model::{ChangedFiles, HeadState};

    use super::*;

    fn oid(n: u8) -> Oid {
        Oid::from_hex(&format!("{n:02x}").repeat(20)).unwrap()
    }

    fn forecast(base: ForecastBase, against: &str, conflicts: &[&str]) -> Forecast {
        Forecast {
            base,
            against: against.into(),
            ours: oid(1),
            theirs: oid(2),
            asked: MergeStrategy::Rebase,
            strategy: MergeStrategy::Rebase,
            conflicts: conflicts.iter().map(|p| (*p).to_owned()).collect(),
            total: u32::try_from(conflicts.len()).unwrap(),
            incoming: Vec::new(),
        }
    }

    fn summary(changed: &[&str]) -> RepoSummary {
        let mut s = RepoSummary::empty("main");
        s.head = HeadState::Branch("main".into());
        s.changed_files = ChangedFiles::capped(
            changed
                .iter()
                .map(|p| FileChange {
                    path: (*p).to_owned(),
                    original_path: None,
                    state: if p.ends_with('/') {
                        FileState::Untracked
                    } else {
                        FileState::Tracked {
                            staged: ChangeKind::Unmodified,
                            unstaged: ChangeKind::Modified,
                        }
                    },
                })
                .collect(),
        );
        s
    }

    #[test]
    fn badge_counts_distinct_files_and_lists_them() {
        let f = ConflictForecast {
            upstream: Some(forecast(
                ForecastBase::Upstream,
                "origin/feat",
                &["a.rs", "b.rs"],
            )),
            target: Some(Forecast {
                strategy: MergeStrategy::Merge,
                ..forecast(ForecastBase::PullRequest(4), "origin/main", &["b.rs"])
            }),
        };
        assert!(f.will_conflict());
        let badge = f.badge(None).unwrap();
        assert_eq!(badge.text, "will conflict · 2");
        assert_eq!(badge.tone, Tone::Conflict);
        assert_eq!(
            f.tooltip(None),
            [
                "Will conflict with origin/feat (pull, rebase checked): 2 files",
                "  a.rs",
                "  b.rs",
                "Will conflict with origin/main (pull request #4, merge checked): 1 file",
                "  b.rs",
            ]
        );
    }

    #[test]
    fn clean_forecasts_show_nothing_but_touched_files() {
        let mut upstream = forecast(ForecastBase::Upstream, "origin/main", &[]);
        upstream.incoming = vec!["docs/new.md".into(), "src/lib.rs".into()];
        let f = ConflictForecast {
            upstream: Some(upstream),
            target: None,
        };
        assert!(!f.will_conflict());
        assert_eq!(f.badge(None), None);
        let s = summary(&["src/lib.rs", "src/main.rs", "docs/"]);
        assert_eq!(f.touched(&s), ["src/lib.rs", "docs/"]);
        let badge = f.badge(Some(&s)).unwrap();
        assert_eq!(badge.text, "touches 2 modified");
        assert_eq!(badge.tone, Tone::Warning);
        assert_eq!(
            f.tooltip(Some(&s)),
            [
                "Pull would touch 2 modified files:",
                "  src/lib.rs",
                "  docs/"
            ]
        );
        assert_eq!(ConflictForecast::default().badge(Some(&s)), None);
    }

    #[test]
    fn omitted_paths_are_counted() {
        let mut f = forecast(ForecastBase::DefaultBranch, "origin/main", &["a"]);
        f.total = 3;
        let f = ConflictForecast {
            upstream: None,
            target: Some(f),
        };
        assert_eq!(f.conflict_count(), 3);
        assert_eq!(f.tooltip(None).last().unwrap(), "  … and 2 more");
    }
}
