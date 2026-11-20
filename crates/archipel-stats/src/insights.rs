//! Repository insights (issue #93, statistics phase 1): contribution per
//! author, repository weight, commit size distribution, churn against age,
//! tag cadence and lead time to merge; and the programming language
//! breakdown (issue #94, phase 2).
//!
//! One walk of a repository's history gathers its raw facts into a
//! [`RepoHistory`]; [`Insights::build`] turns one history (a repository) or
//! several (a project) into the view model. Pure computation: the Git reads
//! happen in the caller.

use std::cmp::Reverse;
use std::collections::{HashMap, VecDeque};

use archipel_model::refs::TagInfo;
use archipel_model::stats::{CommitRecord, RepoStorage, TreeFile};
use archipel_model::{Oid, Timestamp};

use crate::languages::{self, LanguageShare, RepoLanguages};

const DAY: i64 = 86_400;

/// Upper bounds (inclusive) of the lines-per-commit buckets.
pub const LINE_BUCKETS: [(u64, &str); 8] = [
    (0, "0"),
    (2, "1–2"),
    (10, "3–10"),
    (50, "11–50"),
    (200, "51–200"),
    (1_000, "201–1k"),
    (5_000, "1k–5k"),
    (u64::MAX, "5k+"),
];

/// Upper bounds (inclusive) of the files-per-commit buckets.
pub const FILE_BUCKETS: [(u64, &str); 6] = [
    (1, "1"),
    (3, "2–3"),
    (10, "4–10"),
    (30, "11–30"),
    (100, "31–100"),
    (u64::MAX, "100+"),
];

/// A commit of at most this many changed lines is "tiny" (typo fixes).
pub const TINY_LINES: u64 = 2;
/// A commit of more than this many changed lines is "huge".
pub const HUGE_LINES: u64 = 1_000;
/// Consecutive tiny commits of one author from this many on form a streak.
pub const STREAK_MIN: u32 = 3;
/// Window of "recent" changes for churn against age, in days.
pub const RECENT_DAYS: i64 = 90;
/// A file younger than this is never an aging hotspot, in days.
pub const OLD_DAYS: i64 = 180;

/// Knobs of the computation.
#[derive(Clone, Copy, Debug)]
pub struct InsightsConfig {
    /// "Now", for ages and recent windows.
    pub now: Timestamp,
    /// Length of the top lists (biggest commits, aging hotspots, streaks).
    pub top: usize,
}

impl InsightsConfig {
    /// The defaults at `now`.
    #[must_use]
    pub fn at(now: Timestamp) -> Self {
        Self { now, top: 12 }
    }
}

/// Commits and changed lines of one author: the commits of every email
/// used under one name (after `.mailmap`), names compared ignoring case.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Contributor {
    /// The identity: the name lowercased, spaces collapsed (the email for
    /// a commit without name).
    pub key: String,
    /// Display name (the most recent one used).
    pub name: String,
    /// Email, lowercased (the most recent one used).
    pub email: String,
    /// Non-merge commits.
    pub commits: u64,
    /// Lines added.
    pub added: u64,
    /// Lines removed.
    pub removed: u64,
}

impl Contributor {
    /// Lines changed (added + removed).
    #[must_use]
    pub fn lines(&self) -> u64 {
        self.added + self.removed
    }
}

/// Commit sizes of one author.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AuthorSizes {
    /// Identity, as [`Contributor::key`].
    pub key: String,
    /// Display name.
    pub name: String,
    /// Non-merge commits.
    pub commits: u64,
    /// Median changed lines per commit.
    pub median_lines: u64,
    /// 90th percentile of changed lines per commit.
    pub p90_lines: u64,
    /// Median files per commit.
    pub median_files: u64,
    /// Commits of at most [`TINY_LINES`] lines.
    pub tiny: u64,
    /// Commits of more than [`HUGE_LINES`] lines.
    pub huge: u64,
}

/// One large commit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BigCommit {
    /// Repository name.
    pub repo: String,
    /// Commit id.
    pub oid: Oid,
    /// Author name.
    pub author: String,
    /// Subject line.
    pub subject: String,
    /// Changed lines.
    pub lines: u64,
    /// Changed files.
    pub files: u64,
    /// Author date.
    pub time: Timestamp,
}

/// Consecutive tiny commits of one author.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TinyStreak {
    /// Repository name.
    pub repo: String,
    /// Author name.
    pub author: String,
    /// Commits in the streak.
    pub commits: u32,
    /// First commit of the streak.
    pub from: Timestamp,
    /// Last commit of the streak.
    pub to: Timestamp,
}

/// Distribution of commit sizes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CommitSizes {
    /// Commits per [`LINE_BUCKETS`] bucket.
    pub lines: [u64; LINE_BUCKETS.len()],
    /// Commits per [`FILE_BUCKETS`] bucket.
    pub files: [u64; FILE_BUCKETS.len()],
    /// Median changed lines per commit.
    pub median_lines: u64,
    /// 90th percentile of changed lines per commit.
    pub p90_lines: u64,
    /// Per author, most commits first.
    pub authors: Vec<AuthorSizes>,
    /// Largest commits, largest first.
    pub biggest: Vec<BigCommit>,
    /// Longest streaks of tiny commits, longest first.
    pub streaks: Vec<TinyStreak>,
}

/// A file old but still changing often.
#[derive(Clone, Debug, PartialEq)]
pub struct AgingHotspot {
    /// Repository name.
    pub repo: String,
    /// Path.
    pub path: String,
    /// Days since the first commit touching it.
    pub age_days: u32,
    /// Commits touching it in the last [`RECENT_DAYS`] days.
    pub recent_commits: u32,
    /// Commits touching it over the history.
    pub commits: u32,
    /// Lines churned in the last [`RECENT_DAYS`] days.
    pub recent_churn: u64,
    /// Ranking score: recent commits weighted by the log of the age.
    pub score: f64,
}

/// One tag on the time line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TagPoint {
    /// Tag name.
    pub name: String,
    /// When it was made.
    pub time: Timestamp,
}

/// Tag and release cadence of one repository.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TagCadence {
    /// Repository name.
    pub repo: String,
    /// Tags, oldest first.
    pub tags: Vec<TagPoint>,
    /// Median days between two consecutive tags.
    pub median_interval_days: Option<u32>,
    /// The latest tag found in the walked history.
    pub last_tag: Option<TagPoint>,
    /// Commits of the walked history not in the latest tag (unreleased work;
    /// every commit when no tag is in the history).
    pub unreleased: u64,
    /// Author date of the oldest unreleased commit.
    pub unreleased_since: Option<Timestamp>,
}

/// One merge into the main line, with how long its branch lived.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MergeLead {
    /// Repository name.
    pub repo: String,
    /// Branch name, from the merge subject.
    pub branch: String,
    /// When it was merged (committer date of the merge).
    pub merged_at: Timestamp,
    /// From the branch's first commit to the merge, in seconds.
    pub lead_secs: u64,
    /// Commits the merge brought.
    pub commits: u32,
}

/// How serious an advice is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdviceLevel {
    /// All good.
    Good,
    /// Worth a look.
    Hint,
    /// Worth acting on.
    Warning,
}

/// A suggestion about a repository's weight.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Advice {
    /// How serious it is.
    pub level: AdviceLevel,
    /// What to do and why.
    pub text: String,
}

/// What one repository weighs, with suggestions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepoWeight {
    /// Repository name.
    pub repo: String,
    /// The measures.
    pub storage: RepoStorage,
    /// Suggestions (gc, LFS, shallow clone).
    pub advice: Vec<Advice>,
}

/// Raw facts of one non-merge commit.
#[derive(Clone, Copy, Debug)]
struct CommitFact {
    author: u32,
    time: Timestamp,
    lines: u64,
    files: u64,
}

#[derive(Clone, Copy, Debug, Default)]
struct PathFacts {
    first: i64,
    commits: u32,
    recent: u32,
    recent_churn: u64,
}

/// The raw facts of one repository's history, gathered in one walk.
#[derive(Debug, Default)]
pub struct RepoHistory {
    repo: String,
    branch: String,
    /// (name, email) per author index.
    authors: Vec<(String, String)>,
    added: Vec<u64>,
    removed: Vec<u64>,
    facts: Vec<CommitFact>,
    merges_total: u64,
    paths: HashMap<String, PathFacts>,
    biggest: Vec<BigCommit>,
    weeks: HashMap<i64, u32>,
    first: Option<Timestamp>,
    last: Option<Timestamp>,
    streaks: Vec<TinyStreak>,
    tags: TagCadence,
    merges: Vec<MergeLead>,
    weight: Option<RepoWeight>,
    languages: Option<RepoLanguages>,
}

impl RepoHistory {
    /// Adds the language breakdown of the walked branch's tree.
    pub fn set_languages(&mut self, files: &[TreeFile]) {
        self.languages = Some(languages::breakdown(&self.repo, files));
    }
}

/// One node of the commit graph kept for lead time and tag reachability.
struct Node {
    parents: Vec<u32>,
    author_time: i64,
    commit_time: i64,
    /// Subject, kept only for merges.
    subject: Option<String>,
}

/// Builds a [`RepoHistory`] from a stream of commits, newest first.
pub struct HistoryBuilder {
    config: InsightsConfig,
    history: RepoHistory,
    author_index: HashMap<String, u32>,
    index: HashMap<Oid, u32>,
    nodes: Vec<Node>,
    /// Parent oids not yet resolved to an index, per node.
    pending: Vec<Vec<Oid>>,
}

impl HistoryBuilder {
    /// Starts the history of repository `repo`, walked from `branch`.
    #[must_use]
    pub fn new(repo: &str, branch: &str, config: InsightsConfig) -> Self {
        Self {
            config,
            history: RepoHistory {
                repo: repo.to_owned(),
                branch: branch.to_owned(),
                tags: TagCadence {
                    repo: repo.to_owned(),
                    ..TagCadence::default()
                },
                ..RepoHistory::default()
            },
            author_index: HashMap::new(),
            index: HashMap::new(),
            nodes: Vec::new(),
            pending: Vec::new(),
        }
    }

    /// Adds the next (older) commit of the walk.
    #[allow(clippy::many_single_char_names)] // h(istory), a(dded), r(emoved)
    pub fn push(&mut self, record: CommitRecord) {
        let h = &mut self.history;
        let at = record.author_time;
        h.first = Some(h.first.map_or(at, |f| f.min(at)));
        h.last = Some(h.last.map_or(at, |l| l.max(at)));
        *h.weeks.entry(at.week_index()).or_default() += 1;
        let i = u32::try_from(self.nodes.len()).unwrap_or(u32::MAX);
        self.index.insert(record.oid, i);
        let merge = record.parents.len() > 1;
        self.nodes.push(Node {
            parents: Vec::new(),
            author_time: at.0,
            commit_time: record.commit_time.0,
            subject: merge.then(|| record.subject.clone()),
        });
        self.pending.push(record.parents);
        if merge {
            h.merges_total += 1;
            return;
        }
        let author = if let Some(&a) = self.author_index.get(&record.author_email) {
            a
        } else {
            let a = u32::try_from(h.authors.len()).unwrap_or(u32::MAX);
            self.author_index.insert(record.author_email.clone(), a);
            // Newest first: the first name seen is the latest one used.
            h.authors
                .push((record.author_name.clone(), record.author_email.clone()));
            h.added.push(0);
            h.removed.push(0);
            a
        };
        let recent = at.0 >= self.config.now.0 - RECENT_DAYS * DAY;
        let (mut added, mut removed) = (0u64, 0u64);
        for file in &record.files {
            let (a, r) = (
                u64::from(file.added.unwrap_or(0)),
                u64::from(file.removed.unwrap_or(0)),
            );
            added += a;
            removed += r;
            let p = h.paths.entry(file.path.clone()).or_insert(PathFacts {
                first: at.0,
                ..PathFacts::default()
            });
            p.first = p.first.min(at.0);
            p.commits = p.commits.saturating_add(1);
            if recent {
                p.recent = p.recent.saturating_add(1);
                p.recent_churn += a + r;
            }
        }
        h.added[author as usize] += added;
        h.removed[author as usize] += removed;
        let lines = added + removed;
        let files = record.files.len() as u64;
        h.facts.push(CommitFact {
            author,
            time: at,
            lines,
            files,
        });
        let top = self.config.top;
        if h.biggest.len() < top || h.biggest.last().is_some_and(|b| b.lines < lines) {
            h.biggest.push(BigCommit {
                repo: h.repo.clone(),
                oid: record.oid,
                author: record.author_name,
                subject: record.subject,
                lines,
                files,
                time: at,
            });
            h.biggest.sort_by_key(|b| Reverse(b.lines));
            h.biggest.truncate(top);
        }
    }

    /// Finishes the history with the repository's tags and weight.
    /// `unreleased` counts the walked commits not in [`latest_tag`] when
    /// that tag is off the walked history (`git rev-list --count`).
    #[must_use]
    pub fn finish(
        mut self,
        tags: &[TagInfo],
        unreleased: Option<u64>,
        storage: Option<RepoStorage>,
    ) -> RepoHistory {
        // Resolve parents to indices (commits outside the walk are dropped).
        for (node, parents) in self.nodes.iter_mut().zip(std::mem::take(&mut self.pending)) {
            node.parents = parents
                .iter()
                .filter_map(|p| self.index.get(p).copied())
                .collect();
        }
        self.streaks();
        self.merges();
        self.tags(tags, unreleased);
        let h = &mut self.history;
        h.weight = storage.map(|storage| RepoWeight {
            repo: h.repo.clone(),
            advice: advise(&storage, h.facts.len() as u64),
            storage,
        });
        self.history
    }

    /// Streaks of tiny commits per author, in time order.
    fn streaks(&mut self) {
        let h = &mut self.history;
        let mut order: Vec<&CommitFact> = h.facts.iter().collect();
        order.sort_by_key(|f| f.time);
        let mut current: HashMap<u32, (u32, Timestamp, Timestamp)> = HashMap::new();
        let mut done = Vec::new();
        let mut close = |author: u32, (n, from, to): (u32, Timestamp, Timestamp)| {
            if n >= STREAK_MIN {
                done.push(TinyStreak {
                    repo: h.repo.clone(),
                    author: h.authors[author as usize].0.clone(),
                    commits: n,
                    from,
                    to,
                });
            }
        };
        for f in order {
            if f.lines <= TINY_LINES {
                let s = current.entry(f.author).or_insert((0, f.time, f.time));
                s.0 += 1;
                s.2 = f.time;
            } else if let Some(s) = current.remove(&f.author) {
                close(f.author, s);
            }
        }
        for (author, s) in current {
            close(author, s);
        }
        done.sort_by(|a, b| b.commits.cmp(&a.commits).then(b.to.cmp(&a.to)));
        done.truncate(self.config.top);
        h.streaks = done;
    }

    /// Lead time of every merge on the first-parent line: from the oldest
    /// commit it brought to the merge itself.
    fn merges(&mut self) {
        let n = self.nodes.len();
        if n == 0 {
            return;
        }
        let mut claimed = vec![false; n];
        // The first-parent chain from the tip (the first commit walked).
        let mut mainline = Vec::new();
        let mut at = Some(0u32);
        while let Some(i) = at {
            if claimed[i as usize] {
                break;
            }
            claimed[i as usize] = true;
            mainline.push(i);
            at = self.nodes[i as usize].parents.first().copied();
        }
        // Oldest merge first: commits it brought are claimed, so a later
        // merge does not count them again.
        let mut out = Vec::new();
        for &m in mainline.iter().rev() {
            let node = &self.nodes[m as usize];
            if node.parents.len() < 2 {
                continue;
            }
            let mut queue: VecDeque<u32> = node.parents[1..].iter().copied().collect();
            let (mut count, mut oldest) = (0u32, i64::MAX);
            while let Some(c) = queue.pop_front() {
                let c = c as usize;
                if claimed[c] {
                    continue;
                }
                claimed[c] = true;
                count += 1;
                oldest = oldest.min(self.nodes[c].author_time);
                queue.extend(self.nodes[c].parents.iter().copied());
            }
            if count == 0 {
                continue;
            }
            out.push(MergeLead {
                repo: self.history.repo.clone(),
                branch: branch_of(node.subject.as_deref().unwrap_or_default()),
                merged_at: Timestamp(node.commit_time),
                lead_secs: u64::try_from(node.commit_time - oldest).unwrap_or(0),
                commits: count,
            });
        }
        self.history.merges = out;
    }

    /// Cadence of the tags, and the commits after the latest tag: counted
    /// in the walked history when it holds the tag, else `outside` (from
    /// Git), else after the latest tag the walked history holds.
    fn tags(&mut self, tags: &[TagInfo], outside: Option<u64>) {
        let mut points: Vec<TagPoint> = tags
            .iter()
            .map(|t| TagPoint {
                name: t.name.clone(),
                time: t.time,
            })
            .collect();
        points.sort_by(|a, b| a.time.cmp(&b.time).then_with(|| a.name.cmp(&b.name)));
        let mut intervals: Vec<u64> = points
            .windows(2)
            .map(|w| u64::try_from((w[1].time.0 - w[0].time.0) / DAY).unwrap_or(0))
            .collect();
        let median = (!intervals.is_empty()).then(|| percentile(&mut intervals, 50));
        let latest = latest_tag(tags);
        let off_history = latest.filter(|t| !self.index.contains_key(&t.target));
        if let (Some(tag), Some(count)) = (off_history, outside) {
            let t = &mut self.history.tags;
            t.tags = points;
            t.median_interval_days = median.map(|d| u32::try_from(d).unwrap_or(u32::MAX));
            t.last_tag = Some(TagPoint {
                name: tag.name.clone(),
                time: tag.time,
            });
            t.unreleased = count;
            t.unreleased_since = None;
            return;
        }
        let last = latest_tag(
            &tags
                .iter()
                .filter(|t| self.index.contains_key(&t.target))
                .cloned()
                .collect::<Vec<_>>(),
        )
        .cloned();
        let last = last.as_ref();
        let mut released = vec![false; self.nodes.len()];
        if let Some(tag) = last {
            let mut stack = vec![self.index[&tag.target]];
            while let Some(i) = stack.pop() {
                if std::mem::replace(&mut released[i as usize], true) {
                    continue;
                }
                stack.extend(self.nodes[i as usize].parents.iter().copied());
            }
        }
        let unreleased = released.iter().filter(|r| !**r).count() as u64;
        let since = self
            .nodes
            .iter()
            .zip(&released)
            .filter(|(_, r)| !**r)
            .map(|(n, _)| n.author_time)
            .min()
            .map(Timestamp);
        let t = &mut self.history.tags;
        t.tags = points;
        t.median_interval_days = median.map(|d| u32::try_from(d).unwrap_or(u32::MAX));
        t.last_tag = last.map(|l| TagPoint {
            name: l.name.clone(),
            time: l.time,
        });
        t.unreleased = unreleased;
        t.unreleased_since = since;
    }
}

/// The identity of an author: the name lowercased with spaces collapsed,
/// the email when there is no name.
fn person_key(name: &str, email: &str) -> String {
    let key = name
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    if key.is_empty() {
        email.to_owned()
    } else {
        key
    }
}

/// The most recent tag (by date, then name).
#[must_use]
pub fn latest_tag(tags: &[TagInfo]) -> Option<&TagInfo> {
    tags.iter()
        .max_by(|a, b| a.time.cmp(&b.time).then_with(|| a.name.cmp(&b.name)))
}

/// The branch a merge subject names (`Merge branch 'x'`,
/// `Merge pull request #1 from o/x`…), else the subject itself.
fn branch_of(subject: &str) -> String {
    if let Some(rest) = subject.strip_prefix("Merge pull request ") {
        // "#12 from owner/branch"
        if let Some((number, from)) = rest.split_once(" from ") {
            let name = from.split_once('/').map_or(from, |(_, b)| b);
            return format!("{} {number}", name.trim());
        }
    }
    for prefix in ["Merge branch '", "Merge remote-tracking branch '"] {
        if let Some(rest) = subject.strip_prefix(prefix)
            && let Some((name, _)) = rest.split_once('\'')
        {
            return name.to_owned();
        }
    }
    if let Some(rest) = subject.strip_prefix("Merge branch ") {
        return rest.split_whitespace().next().unwrap_or(rest).to_owned();
    }
    subject.chars().take(60).collect()
}

const MIB: u64 = 1024 * 1024;

/// Suggestions about a repository's weight.
fn advise(s: &RepoStorage, commits: u64) -> Vec<Advice> {
    let mut out = Vec::new();
    if s.loose_objects > 6_700 || s.packs > 50 {
        out.push(Advice {
            level: AdviceLevel::Warning,
            text: format!(
                "{} loose objects in {} packs: `git gc` would repack them.",
                s.loose_objects, s.packs
            ),
        });
    } else if s.loose_objects > 1_000 || s.packs > 10 {
        out.push(Advice {
            level: AdviceLevel::Hint,
            text: format!(
                "{} loose objects, {} packs: a `git gc` would tidy them.",
                s.loose_objects, s.packs
            ),
        });
    }
    let unreachable: Vec<_> = s
        .largest_blobs
        .iter()
        .filter(|b| !b.reachable && b.bytes >= MIB)
        .collect();
    if !unreachable.is_empty() {
        let bytes: u64 = unreachable.iter().map(|b| b.bytes).sum();
        out.push(Advice {
            level: AdviceLevel::Hint,
            text: format!(
                "{} large unreachable object(s) ({}) left behind: `git gc --prune=now` would drop them.",
                unreachable.len(),
                human_bytes(bytes)
            ),
        });
    }
    if let Some(big) = s.largest_blobs.iter().find(|b| b.reachable)
        && big.bytes >= 5 * MIB
        && !s.lfs
    {
        let heavy = s
            .largest_blobs
            .iter()
            .filter(|b| b.reachable && b.bytes >= MIB)
            .count();
        out.push(Advice {
            level: if big.bytes >= 50 * MIB {
                AdviceLevel::Warning
            } else {
                AdviceLevel::Hint
            },
            text: format!(
                "{heavy} blob(s) over 1 MiB in the history, the largest {}: \
                 Git LFS would keep binaries out of it.",
                human_bytes(big.bytes)
            ),
        });
    }
    if s.git_dir_bytes >= 500 * MIB && !s.shallow {
        out.push(Advice {
            level: AdviceLevel::Hint,
            text: format!(
                "{} of history over {commits} commits: a shallow (`--depth`) or \
                 partial (`--filter=blob:none`) clone would be lighter to check out.",
                human_bytes(s.git_dir_bytes)
            ),
        });
    }
    if out.is_empty() {
        out.push(Advice {
            level: AdviceLevel::Good,
            text: "Compact: nothing to clean up.".to_owned(),
        });
    }
    out
}

/// `bytes` as B, KiB, MiB or GiB.
#[must_use]
#[allow(clippy::cast_precision_loss)] // display only
pub fn human_bytes(bytes: u64) -> String {
    let b = bytes as f64;
    match bytes {
        0..1024 => format!("{bytes} B"),
        1024..MIB => format!("{:.1} KiB", b / 1024.0),
        MIB..0x4000_0000 => format!("{:.1} MiB", b / MIB as f64),
        _ => format!("{:.2} GiB", b / f64::from(0x4000_0000)),
    }
}

/// The `p`th percentile of `values` (sorted in place); 0 when empty.
fn percentile(values: &mut [u64], p: usize) -> u64 {
    if values.is_empty() {
        return 0;
    }
    values.sort_unstable();
    let i = (values.len() - 1) * p / 100;
    values[i]
}

/// Statistics of one repository or a whole project.
#[derive(Clone, Debug, Default)]
pub struct Insights {
    /// Repositories covered, with the branch walked in each.
    pub repos: Vec<(String, String)>,
    /// Non-merge commits.
    pub commits: u64,
    /// Merge commits.
    pub merges: u64,
    /// Oldest and newest author dates.
    pub span: Option<(Timestamp, Timestamp)>,
    /// Commits per week (Monday-based week index), oldest first.
    pub weeks: Vec<(i64, u32)>,
    /// Contributors, most commits first.
    pub contributors: Vec<Contributor>,
    /// Commit size distribution.
    pub sizes: CommitSizes,
    /// Old files still changing often, highest score first.
    pub aging: Vec<AgingHotspot>,
    /// Tag cadence per repository.
    pub tags: Vec<TagCadence>,
    /// Merges into the main line, oldest first.
    pub merges_lead: Vec<MergeLead>,
    /// Weight per repository, heaviest first.
    pub weights: Vec<RepoWeight>,
    /// Languages of all the repositories together, most lines first.
    pub languages: Vec<LanguageShare>,
    /// Languages per repository, most lines first (repositories whose tree
    /// could not be read are missing).
    pub repo_languages: Vec<RepoLanguages>,
}

impl Insights {
    /// The statistics of one history (a repository) or several (a project).
    #[must_use]
    #[allow(clippy::cast_precision_loss, clippy::too_many_lines)] // scores; one pass
    pub fn build(histories: &[&RepoHistory], config: InsightsConfig) -> Self {
        let mut out = Self::default();
        let mut people: HashMap<String, Contributor> = HashMap::new();
        let mut all_lines = Vec::new();
        let mut per_author: HashMap<String, (Vec<u64>, Vec<u64>)> = HashMap::new();
        let mut weeks: HashMap<i64, u32> = HashMap::new();
        for h in histories {
            out.repos.push((h.repo.clone(), h.branch.clone()));
            out.commits += h.facts.len() as u64;
            out.merges += h.merges_total;
            if let (Some(f), Some(l)) = (h.first, h.last) {
                out.span = Some(match out.span {
                    Some((a, b)) => (a.min(f), b.max(l)),
                    None => (f, l),
                });
            }
            for (w, n) in &h.weeks {
                *weeks.entry(*w).or_default() += n;
            }
            let keys: Vec<String> = h
                .authors
                .iter()
                .map(|(name, email)| person_key(name, email))
                .collect();
            for (i, (name, email)) in h.authors.iter().enumerate() {
                let c = people
                    .entry(keys[i].clone())
                    .or_insert_with(|| Contributor {
                        key: keys[i].clone(),
                        name: name.clone(),
                        email: email.clone(),
                        ..Contributor::default()
                    });
                c.added += h.added[i];
                c.removed += h.removed[i];
            }
            for f in &h.facts {
                let key = &keys[f.author as usize];
                if let Some(c) = people.get_mut(key) {
                    c.commits += 1;
                }
                all_lines.push(f.lines);
                let slot = per_author.entry(key.clone()).or_default();
                slot.0.push(f.lines);
                slot.1.push(f.files);
                let li = LINE_BUCKETS
                    .iter()
                    .position(|(max, _)| f.lines <= *max)
                    .unwrap_or(LINE_BUCKETS.len() - 1);
                out.sizes.lines[li] += 1;
                let fi = FILE_BUCKETS
                    .iter()
                    .position(|(max, _)| f.files <= *max)
                    .unwrap_or(FILE_BUCKETS.len() - 1);
                out.sizes.files[fi] += 1;
            }
            out.sizes.biggest.extend(h.biggest.iter().cloned());
            out.sizes.streaks.extend(h.streaks.iter().cloned());
            for (path, p) in &h.paths {
                let age = (config.now.0 - p.first) / DAY;
                if age < OLD_DAYS || p.recent < 2 {
                    continue;
                }
                let score = f64::from(p.recent) * ((age as f64) / 30.0).log2();
                out.aging.push(AgingHotspot {
                    repo: h.repo.clone(),
                    path: path.clone(),
                    age_days: u32::try_from(age).unwrap_or(u32::MAX),
                    recent_commits: p.recent,
                    commits: p.commits,
                    recent_churn: p.recent_churn,
                    score,
                });
            }
            out.tags.push(h.tags.clone());
            out.merges_lead.extend(h.merges.iter().cloned());
            out.weights.extend(h.weight.clone());
            out.repo_languages.extend(h.languages.clone());
        }
        let top = config.top;
        out.weeks = weeks.into_iter().collect();
        out.weeks.sort_unstable();
        out.contributors = people.into_values().collect();
        out.contributors.sort_by(|a, b| {
            b.commits
                .cmp(&a.commits)
                .then(b.lines().cmp(&a.lines()))
                .then_with(|| a.key.cmp(&b.key))
        });
        out.sizes.median_lines = percentile(&mut all_lines, 50);
        out.sizes.p90_lines = percentile(&mut all_lines, 90);
        let names: HashMap<&str, &str> = out
            .contributors
            .iter()
            .map(|c| (c.key.as_str(), c.name.as_str()))
            .collect();
        out.sizes.authors = per_author
            .into_iter()
            .map(|(key, (mut lines, mut files))| AuthorSizes {
                name: names.get(key.as_str()).copied().unwrap_or(&key).to_owned(),
                key,
                commits: lines.len() as u64,
                tiny: lines.iter().filter(|l| **l <= TINY_LINES).count() as u64,
                huge: lines.iter().filter(|l| **l > HUGE_LINES).count() as u64,
                median_lines: percentile(&mut lines, 50),
                p90_lines: percentile(&mut lines, 90),
                median_files: percentile(&mut files, 50),
            })
            .collect();
        out.sizes
            .authors
            .sort_by(|a, b| b.commits.cmp(&a.commits).then_with(|| a.name.cmp(&b.name)));
        out.sizes.biggest.sort_by_key(|b| Reverse(b.lines));
        out.sizes.biggest.truncate(top);
        out.sizes
            .streaks
            .sort_by(|a, b| b.commits.cmp(&a.commits).then(b.to.cmp(&a.to)));
        out.sizes.streaks.truncate(top);
        out.aging.sort_by(|a, b| {
            b.score
                .total_cmp(&a.score)
                .then_with(|| a.path.cmp(&b.path))
        });
        out.aging.truncate(top);
        out.tags.sort_by(|a, b| a.repo.cmp(&b.repo));
        out.merges_lead.sort_by_key(|m| m.merged_at);
        out.weights
            .sort_by_key(|w| Reverse(w.storage.git_dir_bytes));
        out.languages = languages::merge(&out.repo_languages);
        out.repo_languages
            .sort_by(|a, b| b.lines().cmp(&a.lines()).then_with(|| a.repo.cmp(&b.repo)));
        out
    }

    /// Median and 90th percentile of the lead time to merge, in seconds.
    #[must_use]
    pub fn lead_time(&self) -> Option<(u64, u64)> {
        let mut leads: Vec<u64> = self.merges_lead.iter().map(|m| m.lead_secs).collect();
        (!leads.is_empty()).then(|| (percentile(&mut leads, 50), percentile(&mut leads, 90)))
    }

    /// Commits not in the latest tag, summed over the repositories.
    #[must_use]
    pub fn unreleased(&self) -> u64 {
        self.tags.iter().map(|t| t.unreleased).sum()
    }

    /// Total size of the Git directories.
    #[must_use]
    pub fn git_bytes(&self) -> u64 {
        self.weights.iter().map(|w| w.storage.git_dir_bytes).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use archipel_model::diff::NumStat;

    fn oid(n: u8) -> Oid {
        Oid::from_bytes(&[n; 20]).unwrap()
    }

    fn rec(n: u8, parents: &[u8], who: &str, day: i64, lines: u32, path: &str) -> CommitRecord {
        CommitRecord {
            oid: oid(n),
            parents: parents.iter().map(|p| oid(*p)).collect(),
            author_name: who.to_uppercase(),
            author_email: format!("{who}@x"),
            author_time: Timestamp(day * DAY),
            commit_time: Timestamp(day * DAY),
            subject: if parents.len() > 1 {
                format!("Merge branch '{who}-feature'")
            } else {
                format!("c{n}")
            },
            files: if parents.len() > 1 {
                Vec::new()
            } else {
                vec![NumStat {
                    path: path.to_owned(),
                    added: Some(lines),
                    removed: Some(0),
                }]
            },
        }
    }

    /// main: 1 ← 2 ← 5(merge of 4) ← 6 ; branch: 2 ← 3 ← 4.
    fn history(now_day: i64) -> RepoHistory {
        let config = InsightsConfig::at(Timestamp(now_day * DAY));
        let mut b = HistoryBuilder::new("r", "main", config);
        // Newest first.
        for r in [
            rec(6, &[5], "ann", 400, 1, "old.rs"),
            rec(5, &[2, 4], "ann", 390, 0, ""),
            rec(4, &[3], "bob", 380, 2000, "old.rs"),
            rec(3, &[2], "bob", 370, 1, "new.rs"),
            rec(2, &[1], "ann", 10, 5, "old.rs"),
            rec(1, &[], "ann", 1, 100, "old.rs"),
        ] {
            b.push(r);
        }
        let tags = [TagInfo {
            name: "v1".into(),
            target: oid(2),
            annotated: false,
            time: Timestamp(10 * DAY),
        }];
        b.finish(&tags, None, Some(RepoStorage::default()))
    }

    #[test]
    fn contributors_sizes_and_merges() {
        let h = history(400);
        let i = Insights::build(&[&h], InsightsConfig::at(Timestamp(400 * DAY)));
        assert_eq!(i.commits, 5);
        assert_eq!(i.merges, 1);
        assert_eq!(i.contributors[0].email, "ann@x");
        assert_eq!(i.contributors[0].commits, 3);
        assert_eq!(i.contributors[1].lines(), 2001);
        assert_eq!(i.sizes.lines.iter().sum::<u64>(), 5);
        assert_eq!(i.sizes.biggest[0].lines, 2000);
        // The merge brought 3 and 4: from day 370 to day 390.
        assert_eq!(i.merges_lead.len(), 1);
        assert_eq!(i.merges_lead[0].commits, 2);
        assert_eq!(i.merges_lead[0].lead_secs, 20 * DAY as u64);
        assert_eq!(i.merges_lead[0].branch, "ann-feature");
        // v1 holds 1 and 2: 3, 4, 5, 6 are unreleased.
        assert_eq!(i.tags[0].unreleased, 4);
        assert_eq!(i.tags[0].last_tag.as_ref().unwrap().name, "v1");
        // old.rs: first seen on day 1, touched twice in the last 90 days.
        assert_eq!(i.aging.len(), 1);
        assert_eq!(i.aging[0].path, "old.rs");
        assert_eq!(i.aging[0].recent_commits, 2);
        assert_eq!(i.weights[0].advice[0].level, AdviceLevel::Good);
    }

    #[test]
    fn unreleased_after_a_tag_off_the_walked_branch() {
        let config = InsightsConfig::at(Timestamp(10 * DAY));
        let mut b = HistoryBuilder::new("r", "develop", config);
        b.push(rec(2, &[1], "ann", 2, 1, "a"));
        b.push(rec(1, &[], "ann", 1, 1, "a"));
        let tags = [
            TagInfo {
                name: "v1".into(),
                target: oid(1),
                annotated: false,
                time: Timestamp(DAY),
            },
            TagInfo {
                name: "v2".into(),
                target: oid(9),
                annotated: false,
                time: Timestamp(5 * DAY),
            },
        ];
        let h = b.finish(&tags, Some(7), None);
        let i = Insights::build(&[&h], config);
        assert_eq!(i.tags[0].last_tag.as_ref().unwrap().name, "v2");
        assert_eq!(i.tags[0].unreleased, 7);
        // Without the count from Git: after the latest tag it holds.
        let mut b = HistoryBuilder::new("r", "develop", config);
        b.push(rec(2, &[1], "ann", 2, 1, "a"));
        b.push(rec(1, &[], "ann", 1, 1, "a"));
        let i = Insights::build(&[&b.finish(&tags, None, None)], config);
        assert_eq!(i.tags[0].last_tag.as_ref().unwrap().name, "v1");
        assert_eq!(i.tags[0].unreleased, 1);
    }

    #[test]
    fn languages_per_repo_and_project() {
        let file = |path: &str, lines| TreeFile {
            path: path.to_owned(),
            lines,
            ..TreeFile::default()
        };
        let mut a = history(1_000);
        a.set_languages(&[file("src/a.rs", 50), file("x.py", 10)]);
        let mut b = history(1_000);
        b.repo = "b".into();
        b.set_languages(&[file("y.py", 100)]);
        let config = InsightsConfig::at(Timestamp(1_000 * DAY));
        let project = Insights::build(&[&a, &b], config);
        assert_eq!(project.languages[0].name, "Python");
        assert_eq!(project.languages[0].lines, 110);
        assert_eq!(project.languages[1].name, "Rust");
        assert_eq!(project.repo_languages.len(), 2);
        assert_eq!(project.repo_languages[0].repo, "b");
        let one = Insights::build(&[&a], config);
        assert_eq!(one.languages[0].name, "Rust");
    }

    #[test]
    fn project_sums_repos() {
        let (a, b) = (history(400), history(400));
        let i = Insights::build(&[&a, &b], InsightsConfig::at(Timestamp(400 * DAY)));
        assert_eq!(i.commits, 10);
        assert_eq!(i.contributors.len(), 2);
        assert_eq!(i.contributors[0].commits, 6);
        assert_eq!(i.merges_lead.len(), 2);
        assert_eq!(i.unreleased(), 8);
    }

    #[test]
    fn one_person_under_two_emails() {
        let config = InsightsConfig::at(Timestamp(10 * DAY));
        let mut b = HistoryBuilder::new("r", "main", config);
        let mut other = rec(2, &[1], "ann", 2, 4, "a");
        other.author_email = "ann@home".into();
        b.push(other);
        b.push(rec(1, &[], "ann", 1, 1, "a"));
        let i = Insights::build(&[&b.finish(&[], None, None)], config);
        assert_eq!(i.contributors.len(), 1);
        assert_eq!(i.contributors[0].commits, 2);
        assert_eq!(i.contributors[0].lines(), 5);
        assert_eq!(i.sizes.authors[0].commits, 2);
    }

    #[test]
    fn tiny_streaks_are_found() {
        let config = InsightsConfig::at(Timestamp(100 * DAY));
        let mut b = HistoryBuilder::new("r", "main", config);
        for n in (1..=5).rev() {
            b.push(rec(n, &[], "ann", i64::from(n), 1, "a"));
        }
        let h = b.finish(&[], None, None);
        let i = Insights::build(&[&h], config);
        assert_eq!(i.sizes.streaks.len(), 1);
        assert_eq!(i.sizes.streaks[0].commits, 5);
        assert_eq!(i.sizes.authors[0].tiny, 5);
    }

    #[test]
    fn branch_names_from_merge_subjects() {
        assert_eq!(branch_of("Merge branch 'feat/x' into main"), "feat/x");
        assert_eq!(
            branch_of("Merge pull request #12 from o/fix-it"),
            "fix-it #12"
        );
        assert_eq!(
            branch_of("Merge remote-tracking branch 'origin/a'"),
            "origin/a"
        );
    }

    #[test]
    fn advice_for_heavy_repos() {
        let s = RepoStorage {
            loose_objects: 9_000,
            git_dir_bytes: 600 * MIB,
            largest_blobs: vec![archipel_model::stats::BlobSize {
                oid: oid(1),
                path: "a.bin".into(),
                reachable: true,
                bytes: 80 * MIB,
            }],
            ..RepoStorage::default()
        };
        let a = advise(&s, 10);
        assert_eq!(a.len(), 3);
        assert!(a.iter().all(|a| a.level != AdviceLevel::Good));
        assert_eq!(human_bytes(1536), "1.5 KiB");
    }
}
