//! The project dashboard view-model (spec §3).
//!
//! Extension points: [`DashboardColumn`] (registered in [`ColumnRegistry`]) and
//! [`RepoFilter`] (registered in [`FilterRegistry`]). The built-in columns
//! and the built-in filters use the same registries a fork would.

use std::cmp::Ordering;
use std::sync::Arc;

use archipel_model::changes::{ChangeKind, FileChange, FileState};
use archipel_model::ui_state::SortDirection;
use archipel_model::{
    ContainingBranch, HeadState, Oid, RepoId, RepoOp, RepoPath, RepoSummary, Timestamp,
};

use crate::forge::{
    BranchCi, CiColumn, CiFailingFilter, ForgeColumn, ForgeLimit, ForgeStatus,
    PullRequestOpenFilter,
};

/// Availability of a repository of the project.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RepoStatus {
    /// Summary not computed yet (and nothing cached).
    Loading,
    /// Up-to-date or cached summary (boxed: it dwarfs the other variants).
    Ready(Box<RepoSummary>),
    /// The path does not exist or is not a repository.
    Missing,
    /// Reading the repository failed.
    Error(String),
}

/// One dashboard row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DashboardRow {
    /// Repository id.
    pub id: RepoId,
    /// Display name (alias or folder name).
    pub name: String,
    /// Path.
    pub path: RepoPath,
    /// Status.
    pub status: RepoStatus,
    /// Whether the summary comes from the cache and a refresh is pending.
    pub stale: bool,
    /// Hidden by the user (only listed, dimmed, when "show hidden" is on).
    pub hidden: bool,
    /// Pull request, CI and review state on the forge (spec §8).
    pub forge: ForgeStatus,
    /// CI of the current branch's head on the forge (spec §8).
    pub ci: BranchCi,
    /// Set when `forge` and `ci` may be out of date (rate limited).
    pub forge_limit: Option<ForgeLimit>,
}

impl DashboardRow {
    /// The summary, when available.
    #[must_use]
    pub fn summary(&self) -> Option<&RepoSummary> {
        match &self.status {
            RepoStatus::Ready(s) => Some(s),
            _ => None,
        }
    }
}

/// Semantic tone of a cell; the UI maps it to theme status colors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    /// Default text.
    Normal,
    /// De-emphasized text.
    Muted,
    /// Uncommitted changes.
    Dirty,
    /// Commits to push.
    Ahead,
    /// Commits to pull.
    Behind,
    /// Conflict or operation in progress.
    Conflict,
    /// Error.
    Error,
    /// Passed, approved.
    Success,
    /// Waiting (CI running).
    Warning,
}

/// A rendered cell: primary text, optional secondary text (e.g. the path
/// under the repo name), and a tone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cell {
    /// Main text.
    pub text: String,
    /// Secondary text.
    pub detail: Option<String>,
    /// Tone.
    pub tone: Tone,
}

impl Cell {
    fn new(text: impl Into<String>, tone: Tone) -> Self {
        Self {
            text: text.into(),
            detail: None,
            tone,
        }
    }
}

/// Sort key of a cell.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum SortKey {
    /// Numeric key.
    Number(i64),
    /// Text key (compared case-insensitively by the caller's lowercase).
    Text(String),
    /// No value: sorted last.
    None,
}

/// A dashboard column.
pub trait DashboardColumn: Send + Sync {
    /// Stable id (saved in the UI state).
    fn id(&self) -> &str;
    /// Header text.
    fn header(&self) -> &str;
    /// The cell of a row at time `now` (for relative times).
    fn cell(&self, row: &DashboardRow, now: Timestamp) -> Cell;
    /// The sort key of a row.
    fn sort_key(&self, row: &DashboardRow) -> SortKey;
    /// Lines shown when hovering the cell of a row (none by default).
    fn tooltip(&self, _row: &DashboardRow) -> Option<Vec<String>> {
        None
    }
}

/// Human relative time ("3 min ago").
#[must_use]
pub fn relative_time(then: Timestamp, now: Timestamp) -> String {
    let secs = then.elapsed_until(now).as_secs();
    match secs {
        0..60 => "just now".into(),
        60..3_600 => format!("{} min ago", secs / 60),
        3_600..86_400 => format!("{} h ago", secs / 3_600),
        _ => format!("{} d ago", secs / 86_400),
    }
}

fn status_cell(row: &DashboardRow) -> Option<Cell> {
    match &row.status {
        RepoStatus::Ready(_) => None,
        RepoStatus::Loading => Some(Cell::new("…", Tone::Muted)),
        RepoStatus::Missing => Some(Cell::new("missing", Tone::Error)),
        RepoStatus::Error(e) => Some(Cell {
            text: "error".into(),
            detail: Some(e.clone()),
            tone: Tone::Error,
        }),
    }
}

struct RepoColumn;
impl DashboardColumn for RepoColumn {
    fn id(&self) -> &str {
        "repo"
    }
    fn header(&self) -> &str {
        "Repo"
    }
    fn cell(&self, row: &DashboardRow, _now: Timestamp) -> Cell {
        let tone = if matches!(row.status, RepoStatus::Missing | RepoStatus::Error(_)) {
            Tone::Error
        } else {
            Tone::Normal
        };
        Cell {
            text: row.name.clone(),
            detail: Some(row.path.as_path().display().to_string()),
            tone,
        }
    }
    fn sort_key(&self, row: &DashboardRow) -> SortKey {
        SortKey::Text(row.name.to_lowercase())
    }
}

struct BranchColumn;
impl DashboardColumn for BranchColumn {
    fn id(&self) -> &str {
        "branch"
    }
    fn header(&self) -> &str {
        "Branch"
    }
    fn cell(&self, row: &DashboardRow, _now: Timestamp) -> Cell {
        let Some(s) = row.summary() else {
            return status_cell(row).unwrap_or_else(|| Cell::new("", Tone::Muted));
        };
        let mut cell = match &s.head {
            HeadState::Branch(b) => Cell::new(b.clone(), Tone::Normal),
            HeadState::Unborn(b) => Cell::new(format!("{b} (no commit)"), Tone::Muted),
            HeadState::Detached(oid) => detached_cell(s, *oid),
        };
        if let Some(op) = s.in_progress {
            cell.detail = Some(format!("{} in progress", op_label(op)));
            cell.tone = Tone::Conflict;
        }
        cell
    }
    fn sort_key(&self, row: &DashboardRow) -> SortKey {
        row.summary()
            .and_then(|s| {
                s.head
                    .branch()
                    .or_else(|| s.containing_branches.first().map(|b| b.name.as_str()))
            })
            .map_or(SortKey::None, |b| SortKey::Text(b.to_lowercase()))
    }
}

/// A detached `HEAD` (typically a tag checked out) reads as the nearest
/// branch containing it, in git's `main~3` notation, marked detached. With no
/// branch containing it, new commits there would be lost: flag it.
fn detached_cell(s: &RepoSummary, oid: Oid) -> Cell {
    let at = s
        .describe
        .as_deref()
        .filter(|d| is_exact_tag(d))
        .map_or_else(|| oid.short(8), |tag| format!("tag {tag}"));
    match s.containing_branches.split_first() {
        Some((nearest, others)) => {
            let also = if others.is_empty() {
                String::new()
            } else {
                let others: Vec<String> = others.iter().map(ContainingBranch::label).collect();
                format!(" (also {})", others.join(", "))
            };
            let detail = format!("detached HEAD at {at}, on {}{also}", nearest.label());
            Cell {
                text: format!("{} (detached)", nearest.label()),
                detail: Some(detail),
                tone: Tone::Muted,
            }
        }
        None => Cell {
            text: format!("detached {}", oid.short(8)),
            detail: Some(format!("detached HEAD at {at}, on no branch")),
            tone: Tone::Conflict,
        },
    }
}

/// Label of an in-progress operation.
#[must_use]
pub fn op_label(op: RepoOp) -> &'static str {
    match op {
        RepoOp::Merge => "merge",
        RepoOp::Rebase => "rebase",
        RepoOp::CherryPick => "cherry-pick",
        RepoOp::Revert => "revert",
        RepoOp::Bisect => "bisect",
        RepoOp::ApplyMailbox => "am",
    }
}

struct SyncColumn;
impl DashboardColumn for SyncColumn {
    fn id(&self) -> &str {
        "sync"
    }
    fn header(&self) -> &str {
        "Sync"
    }
    fn cell(&self, row: &DashboardRow, _now: Timestamp) -> Cell {
        let Some(s) = row.summary() else {
            return Cell::new("", Tone::Muted);
        };
        match &s.upstream {
            None => Cell::new("no upstream", Tone::Muted),
            Some(u) if u.gone => Cell::new("upstream gone", Tone::Error),
            Some(u) => {
                let tone = match (u.ahead, u.behind) {
                    (0, 0) => Tone::Muted,
                    (_, 0) => Tone::Ahead,
                    _ => Tone::Behind,
                };
                let text = match (u.ahead, u.behind) {
                    (0, 0) => "in sync".to_owned(),
                    (a, 0) => format!("↑{a}"),
                    (0, b) => format!("↓{b}"),
                    (a, b) => format!("↑{a} ↓{b}"),
                };
                Cell {
                    text,
                    detail: Some(u.name.clone()),
                    tone,
                }
            }
        }
    }
    fn sort_key(&self, row: &DashboardRow) -> SortKey {
        row.summary().map_or(SortKey::None, |s| {
            SortKey::Number(i64::from(s.behind()) * 10_000 + i64::from(s.ahead()))
        })
    }
}

struct ChangesColumn;
impl DashboardColumn for ChangesColumn {
    fn id(&self) -> &str {
        "changes"
    }
    fn header(&self) -> &str {
        "Changes"
    }
    fn cell(&self, row: &DashboardRow, _now: Timestamp) -> Cell {
        let Some(s) = row.summary() else {
            return Cell::new("", Tone::Muted);
        };
        let c = s.changes;
        let mut parts = Vec::new();
        if c.conflicted > 0 {
            parts.push(format!("!{}", c.conflicted));
        }
        if c.staged > 0 {
            parts.push(format!("●{}", c.staged));
        }
        if c.modified > 0 {
            parts.push(format!("✎{}", c.modified));
        }
        if c.untracked > 0 {
            parts.push(format!("+{}", c.untracked));
        }
        if s.stash_count > 0 {
            parts.push(format!("≡{}", s.stash_count));
        }
        let tone = if c.conflicted > 0 {
            Tone::Conflict
        } else if c.is_dirty() {
            Tone::Dirty
        } else {
            Tone::Muted
        };
        let text = if parts.is_empty() {
            "clean".into()
        } else {
            parts.join(" ")
        };
        let detail = Some(format!(
            "{} modified, {} staged, {} untracked, {} stash(es)",
            c.modified, c.staged, c.untracked, s.stash_count
        ));
        Cell { text, detail, tone }
    }
    fn tooltip(&self, row: &DashboardRow) -> Option<Vec<String>> {
        let changed = &row.summary()?.changed_files;
        if changed.files.is_empty() {
            return None;
        }
        let mut lines: Vec<String> = changed.files.iter().map(change_line).collect();
        match changed.omitted() {
            0 => {}
            n => lines.push(format!("… and {n} more")),
        }
        Some(lines)
    }
    fn sort_key(&self, row: &DashboardRow) -> SortKey {
        row.summary().map_or(SortKey::None, |s| {
            let c = s.changes;
            SortKey::Number(i64::from(
                c.conflicted + c.staged + c.modified + c.untracked,
            ))
        })
    }
}

/// One changed file of the Changes tooltip, marked with the cell's symbols
/// (`!` conflicted, `●` staged, `✎` modified, `+` untracked).
fn change_line(change: &FileChange) -> String {
    let marks = match change.state {
        FileState::Conflicted => "!".to_owned(),
        FileState::Untracked => "+".to_owned(),
        FileState::Tracked { staged, unstaged } => {
            let mut m = String::new();
            if staged != ChangeKind::Unmodified {
                m.push('●');
            }
            if unstaged != ChangeKind::Unmodified {
                m.push('✎');
            }
            m
        }
    };
    let deleted = matches!(
        change.state,
        FileState::Tracked {
            staged: ChangeKind::Deleted,
            ..
        } | FileState::Tracked {
            unstaged: ChangeKind::Deleted,
            ..
        }
    );
    let path = match &change.original_path {
        Some(from) => format!("{from} → {}", change.path),
        None if deleted => format!("{} (deleted)", change.path),
        None => change.path.clone(),
    };
    format!("{marks} {path}")
}

struct VersionColumn;
impl DashboardColumn for VersionColumn {
    fn id(&self) -> &str {
        "version"
    }
    fn header(&self) -> &str {
        "Version"
    }
    fn cell(&self, row: &DashboardRow, _now: Timestamp) -> Cell {
        row.summary()
            .map_or_else(|| Cell::new("—", Tone::Muted), version_cell)
    }
    fn sort_key(&self, row: &DashboardRow) -> SortKey {
        row.summary()
            .map_or(SortKey::None, |s| SortKey::Text(version_cell(s).text))
    }
}

/// What `HEAD` is "at": the tag when `HEAD` is exactly on one, otherwise the
/// checked-out branch (or the short commit when detached). The nearest tag of
/// `git describe` is not used: in a shared library it is often another
/// product's release tag on an old commit, which reads as a wrong version.
fn version_cell(s: &RepoSummary) -> Cell {
    if let Some(tag) = s.describe.as_deref().filter(|d| is_exact_tag(d)) {
        return Cell {
            text: tag.to_owned(),
            detail: Some(format!("tag {tag}")),
            tone: Tone::Normal,
        };
    }
    match &s.head {
        HeadState::Branch(b) => Cell {
            text: b.clone(),
            detail: Some(format!("branch {b} (not on a tag)")),
            tone: Tone::Normal,
        },
        HeadState::Unborn(b) => Cell::new(b.clone(), Tone::Muted),
        HeadState::Detached(oid) => Cell {
            text: oid.short(8),
            detail: Some("detached, not on a tag".into()),
            tone: Tone::Muted,
        },
    }
}

/// `git describe --tags` prints `<tag>-<n>-g<hash>` when `HEAD` is past the
/// tag, and just `<tag>` when `HEAD` is exactly on it.
fn is_exact_tag(describe: &str) -> bool {
    let mut parts = describe.rsplitn(3, '-');
    let past_tag = matches!(
        (parts.next(), parts.next(), parts.next()),
        (Some(hash), Some(count), Some(_))
            if hash.starts_with('g')
                && !count.is_empty()
                && count.bytes().all(|b| b.is_ascii_digit())
    );
    !past_tag
}

struct FetchColumn;
impl DashboardColumn for FetchColumn {
    fn id(&self) -> &str {
        "fetch"
    }
    fn header(&self) -> &str {
        "Fetch"
    }
    fn cell(&self, row: &DashboardRow, now: Timestamp) -> Cell {
        match row.summary().and_then(|s| s.last_fetch) {
            Some(t) => Cell::new(relative_time(t, now), Tone::Muted),
            None => Cell::new("never", Tone::Muted),
        }
    }
    fn sort_key(&self, row: &DashboardRow) -> SortKey {
        row.summary()
            .and_then(|s| s.last_fetch)
            .map_or(SortKey::None, |t| SortKey::Number(t.0))
    }
}

/// Ordered dashboard columns.
#[derive(Clone, Default)]
pub struct ColumnRegistry {
    columns: Vec<Arc<dyn DashboardColumn>>,
}

impl ColumnRegistry {
    /// The six built-in columns of spec §3, then the forge status and CI (§8).
    #[must_use]
    pub fn with_builtins() -> Self {
        let mut r = Self::default();
        r.register(Arc::new(RepoColumn));
        r.register(Arc::new(BranchColumn));
        r.register(Arc::new(SyncColumn));
        r.register(Arc::new(ChangesColumn));
        r.register(Arc::new(VersionColumn));
        r.register(Arc::new(FetchColumn));
        r.register(Arc::new(ForgeColumn));
        r.register(Arc::new(CiColumn));
        r
    }

    /// Appends a column, or replaces the column with the same id in place.
    pub fn register(&mut self, column: Arc<dyn DashboardColumn>) {
        match self.columns.iter().position(|c| c.id() == column.id()) {
            Some(i) => self.columns[i] = column,
            None => self.columns.push(column),
        }
    }

    /// Removes a column.
    pub fn remove(&mut self, id: &str) {
        self.columns.retain(|c| c.id() != id);
    }

    /// Columns in display order.
    #[must_use]
    pub fn columns(&self) -> &[Arc<dyn DashboardColumn>] {
        &self.columns
    }

    /// A column by id.
    #[must_use]
    pub fn get(&self, id: &str) -> Option<&Arc<dyn DashboardColumn>> {
        self.columns.iter().find(|c| c.id() == id)
    }
}

/// A dashboard filter chip.
pub trait RepoFilter: Send + Sync {
    /// Stable id (saved in the UI state).
    fn id(&self) -> &str;
    /// Chip label.
    fn label(&self) -> &str;
    /// Whether a row passes the filter.
    fn matches(&self, row: &DashboardRow) -> bool;
}

struct FnFilter {
    id: &'static str,
    label: &'static str,
    test: fn(&DashboardRow) -> bool,
}

impl RepoFilter for FnFilter {
    fn id(&self) -> &str {
        self.id
    }
    fn label(&self) -> &str {
        self.label
    }
    fn matches(&self, row: &DashboardRow) -> bool {
        (self.test)(row)
    }
}

/// Available filters. Active filters combine with AND.
#[derive(Clone, Default)]
pub struct FilterRegistry {
    filters: Vec<Arc<dyn RepoFilter>>,
}

impl FilterRegistry {
    /// Built-in filters: dirty, behind, ahead, detached, in progress, problems.
    #[must_use]
    pub fn with_builtins() -> Self {
        let mut r = Self::default();
        let f = |id, label, test| Arc::new(FnFilter { id, label, test });
        r.register(f("dirty", "Dirty", |row| {
            row.summary().is_some_and(|s| s.changes.is_dirty())
        }));
        r.register(f("behind", "Behind remote", |row| {
            row.summary().is_some_and(|s| s.behind() > 0)
        }));
        r.register(f("ahead", "Ahead of remote", |row| {
            row.summary().is_some_and(|s| s.ahead() > 0)
        }));
        r.register(f("detached", "Detached", |row| {
            row.summary()
                .is_some_and(|s| matches!(s.head, HeadState::Detached(_)))
        }));
        r.register(f("in-progress", "Operation in progress", |row| {
            row.summary().is_some_and(|s| s.in_progress.is_some())
        }));
        r.register(f("problems", "Missing or failing", |row| {
            matches!(row.status, RepoStatus::Missing | RepoStatus::Error(_))
        }));
        r.register(Arc::new(PullRequestOpenFilter));
        r.register(Arc::new(CiFailingFilter));
        r
    }

    /// Adds a filter or replaces the one with the same id.
    pub fn register(&mut self, filter: Arc<dyn RepoFilter>) {
        match self.filters.iter().position(|f| f.id() == filter.id()) {
            Some(i) => self.filters[i] = filter,
            None => self.filters.push(filter),
        }
    }

    /// Every filter.
    #[must_use]
    pub fn filters(&self) -> &[Arc<dyn RepoFilter>] {
        &self.filters
    }

    /// Whether a row passes every active filter (unknown ids are ignored).
    #[must_use]
    pub fn matches(&self, active: &[String], row: &DashboardRow) -> bool {
        active.iter().all(|id| {
            self.filters
                .iter()
                .find(|f| f.id() == id)
                .is_none_or(|f| f.matches(row))
        })
    }
}

/// Filters and sorts rows for display.
#[must_use]
pub fn visible_rows<'a>(
    rows: &'a [DashboardRow],
    filters: &FilterRegistry,
    active: &[String],
    sort: Option<(&dyn DashboardColumn, SortDirection)>,
) -> Vec<&'a DashboardRow> {
    let mut out: Vec<&DashboardRow> = rows.iter().filter(|r| filters.matches(active, r)).collect();
    if let Some((column, direction)) = sort {
        out.sort_by(|a, b| {
            let (ka, kb) = (column.sort_key(a), column.sort_key(b));
            // Rows without a value always go last.
            match (&ka, &kb) {
                (SortKey::None, SortKey::None) => Ordering::Equal,
                (SortKey::None, _) => Ordering::Greater,
                (_, SortKey::None) => Ordering::Less,
                _ if direction == SortDirection::Descending => kb.cmp(&ka),
                _ => ka.cmp(&kb),
            }
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use archipel_model::{ChangeCounts, ChangedFiles, Upstream};

    use super::*;

    fn row(name: &str, f: impl FnOnce(&mut RepoSummary)) -> DashboardRow {
        let mut s = RepoSummary::empty("main");
        s.head = HeadState::Branch("main".into());
        f(&mut s);
        let path = RepoPath::new(format!("/p/{name}"));
        DashboardRow {
            id: path.id(),
            name: name.into(),
            path,
            status: RepoStatus::Ready(Box::new(s)),
            stale: false,
            hidden: false,
            forge: ForgeStatus::Unknown,
            ci: BranchCi::Unknown,
            forge_limit: None,
        }
    }

    #[test]
    fn builtin_columns_render() {
        let cols = ColumnRegistry::with_builtins();
        let ids: Vec<_> = cols.columns().iter().map(|c| c.id().to_owned()).collect();
        assert_eq!(
            ids,
            [
                "repo", "branch", "sync", "changes", "version", "fetch", "forge", "ci"
            ]
        );
        let r = row("app", |s| {
            s.upstream = Some(Upstream {
                name: "origin/main".into(),
                ahead: 2,
                behind: 1,
                gone: false,
            });
            s.changes = ChangeCounts {
                modified: 3,
                staged: 1,
                untracked: 0,
                conflicted: 0,
            };
            s.describe = Some("v1.2.0-3-gabc1234".into());
            s.last_fetch = Some(Timestamp(1000));
        });
        let now = Timestamp(1000 + 7200);
        let cell = |id: &str| cols.get(id).unwrap().cell(&r, now);
        assert_eq!(cell("sync").text, "↑2 ↓1");
        assert_eq!(cell("sync").tone, Tone::Behind);
        assert_eq!(cell("changes").text, "●1 ✎3");
        // Past a tag: the branch, not the nearest tag.
        assert_eq!(cell("version").text, "main");
        assert_eq!(cell("fetch").text, "2 h ago");
        assert_eq!(
            cell("repo").detail.as_deref(),
            Some(r.path.as_path().display().to_string().as_str())
        );
    }

    #[test]
    fn changes_tooltip_lists_files() {
        let cols = ColumnRegistry::with_builtins();
        let changes = cols.get("changes").unwrap();
        assert_eq!(changes.tooltip(&row("clean", |_| {})), None);
        let tracked = |path: &str, staged, unstaged| FileChange {
            path: path.into(),
            original_path: None,
            state: FileState::Tracked { staged, unstaged },
        };
        let mut files = vec![
            tracked("a.rs", ChangeKind::Modified, ChangeKind::Modified),
            tracked("b.rs", ChangeKind::Unmodified, ChangeKind::Deleted),
            FileChange {
                path: "new.rs".into(),
                original_path: Some("old.rs".into()),
                state: FileState::Tracked {
                    staged: ChangeKind::Renamed,
                    unstaged: ChangeKind::Unmodified,
                },
            },
            FileChange {
                path: "notes/".into(),
                original_path: None,
                state: FileState::Untracked,
            },
            FileChange {
                path: "c.rs".into(),
                original_path: None,
                state: FileState::Conflicted,
            },
        ];
        let r = row("app", |s| {
            s.changed_files = ChangedFiles::capped(files.clone());
        });
        assert_eq!(
            changes.tooltip(&r).unwrap(),
            [
                "●✎ a.rs",
                "✎ b.rs (deleted)",
                "● old.rs → new.rs",
                "+ notes/",
                "! c.rs"
            ]
        );
        files.extend((0..ChangedFiles::MAX).map(|i| {
            tracked(
                &format!("f{i}"),
                ChangeKind::Unmodified,
                ChangeKind::Modified,
            )
        }));
        let r = row("big", |s| s.changed_files = ChangedFiles::capped(files));
        let lines = changes.tooltip(&r).unwrap();
        assert_eq!(lines.len(), ChangedFiles::MAX + 1);
        assert_eq!(lines.last().unwrap(), "… and 5 more");
    }

    #[test]
    fn detached_head_shows_nearest_branch() {
        let cols = ColumnRegistry::with_builtins();
        let branch = cols.get("branch").unwrap();
        let oid = Oid::from_hex(&"ab".repeat(20)).unwrap();
        let on = |name: &str, is_remote, ahead| ContainingBranch {
            name: name.into(),
            is_remote,
            ahead,
        };
        let r = row("lib", |s| {
            s.head = HeadState::Detached(oid);
            s.describe = Some("v2.0".into());
            s.containing_branches = ContainingBranch::nearest(vec![
                on("origin/main", true, 3),
                on("develop", false, 9),
                on("main", false, 3),
            ]);
        });
        let cell = branch.cell(&r, Timestamp(0));
        assert_eq!(cell.text, "main~3 (detached)");
        assert_eq!(cell.tone, Tone::Muted);
        assert_eq!(
            cell.detail.as_deref(),
            Some("detached HEAD at tag v2.0, on main~3 (also origin/main~3, develop~9)")
        );
        assert_eq!(branch.sort_key(&r), SortKey::Text("main".into()));

        let orphan = row("lib", |s| s.head = HeadState::Detached(oid));
        let cell = branch.cell(&orphan, Timestamp(0));
        assert_eq!(cell.text, "detached abababab");
        assert_eq!(cell.tone, Tone::Conflict);
    }

    #[test]
    fn exact_tag_detection() {
        assert!(is_exact_tag("TAPE_J37"));
        assert!(is_exact_tag("release-1-2"));
        assert!(is_exact_tag("my-tag-good"));
        assert!(!is_exact_tag("TAPE_J37-2-g37b2f70"));
        assert!(!is_exact_tag("release-1-2-12-gdeadbeef"));
    }

    #[test]
    fn filters_and_sorting() {
        let rows = vec![
            row("b", |s| s.changes.modified = 1),
            row("a", |s| {
                s.upstream = Some(Upstream {
                    name: "o/m".into(),
                    ahead: 0,
                    behind: 4,
                    gone: false,
                });
            }),
            DashboardRow {
                status: RepoStatus::Missing,
                ..row("c", |_| {})
            },
        ];
        let filters = FilterRegistry::with_builtins();
        let cols = ColumnRegistry::with_builtins();
        let dirty = visible_rows(&rows, &filters, &["dirty".into()], None);
        assert_eq!(dirty.len(), 1);
        assert_eq!(dirty[0].name, "b");
        let problems = visible_rows(&rows, &filters, &["problems".into()], None);
        assert_eq!(problems[0].name, "c");
        let by_name = visible_rows(
            &rows,
            &filters,
            &[],
            Some((cols.get("repo").unwrap().as_ref(), SortDirection::Ascending)),
        );
        assert_eq!(
            by_name.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
            ["a", "b", "c"]
        );
        let by_sync = visible_rows(
            &rows,
            &filters,
            &[],
            Some((
                cols.get("sync").unwrap().as_ref(),
                SortDirection::Descending,
            )),
        );
        assert_eq!(by_sync[0].name, "a");
        assert_eq!(by_sync[2].name, "c", "rows without a value go last");
    }

    #[test]
    fn forks_can_replace_columns() {
        struct Custom;
        impl DashboardColumn for Custom {
            fn id(&self) -> &str {
                "version"
            }
            fn header(&self) -> &str {
                "Release"
            }
            fn cell(&self, _: &DashboardRow, _: Timestamp) -> Cell {
                Cell::new("x", Tone::Normal)
            }
            fn sort_key(&self, _: &DashboardRow) -> SortKey {
                SortKey::None
            }
        }
        let mut cols = ColumnRegistry::with_builtins();
        cols.register(Arc::new(Custom));
        assert_eq!(cols.columns()[4].header(), "Release");
        assert_eq!(cols.columns().len(), 8);
    }
}
