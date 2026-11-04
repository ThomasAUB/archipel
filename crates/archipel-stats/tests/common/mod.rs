//! Shared helpers for the fake-backed tests.

use std::sync::Arc;

use archipel_model::diff::NumStat;
use archipel_model::history::{CommitMeta, TopologyNode};
use archipel_model::ports::CommitChurn;
use archipel_model::refs::BranchInfo;
use archipel_model::stats::ActivityBucket;
use archipel_model::testing::{FakeGit, InMemoryStore, oid};
use archipel_model::{Oid, RepoPath, Timestamp};
use archipel_stats::{RepoTarget, StatSources, StatsEngine};

/// Seconds in a week.
pub const WEEK: i64 = 7 * 86_400;
/// A Monday, noon UTC (2024-01-01).
pub const MONDAY: i64 = 1_704_110_400;

/// One commit of a fake history.
pub fn churn(n: u64, email: &str, time: i64, files: &[(&str, u32, u32)]) -> CommitChurn {
    CommitChurn {
        oid: oid(n),
        author_email: email.to_owned(),
        author_time: Timestamp(time),
        files: files
            .iter()
            .map(|(p, a, r)| NumStat {
                path: (*p).to_owned(),
                added: Some(*a),
                removed: Some(*r),
            })
            .collect(),
    }
}

/// A fake repository plus its store.
pub struct Fixture {
    pub git: Arc<FakeGit>,
    pub store: Arc<InMemoryStore>,
    pub target: RepoTarget,
}

impl Fixture {
    pub fn new(name: &str) -> Self {
        Self::with(
            Arc::new(FakeGit::new()),
            Arc::new(InMemoryStore::new()),
            name,
        )
    }

    /// Another repository sharing the same fakes.
    pub fn with(git: Arc<FakeGit>, store: Arc<InMemoryStore>, name: &str) -> Self {
        let path = RepoPath::new(format!("/fake/{name}"));
        let target = RepoTarget::from_path(path);
        git.with_repo(&target.path, |_| ());
        Self { git, store, target }
    }

    pub fn engine(&self) -> StatsEngine {
        StatsEngine::new(
            self.git.clone(),
            self.store.clone(),
            self.git.clone(),
            self.git.clone(),
        )
    }

    pub fn sources(&self) -> StatSources {
        StatSources {
            store: self.store.clone(),
            refs: self.git.clone(),
            history: self.git.clone(),
        }
    }

    /// Replaces the history by a linear chain (`commits` oldest first) with
    /// `main` checked out at the last commit.
    pub fn set_linear(&self, commits: &[CommitChurn]) {
        self.git.with_repo(&self.target.path, |r| {
            r.churn = commits.iter().rev().cloned().collect();
            r.history = commits
                .iter()
                .enumerate()
                .rev()
                .map(|(i, c)| {
                    let parents = if i == 0 {
                        Vec::new()
                    } else {
                        vec![commits[i - 1].oid]
                    };
                    (node(c.oid, parents), meta(c.oid))
                })
                .collect();
            if let Some(last) = commits.last() {
                r.refs.insert("refs/heads/main".into(), last.oid);
                r.branches = vec![branch("main", last.oid, true, false, last.author_time.0)];
            }
        });
    }
}

pub fn node(oid: Oid, parents: Vec<Oid>) -> TopologyNode {
    TopologyNode { oid, parents }
}

pub fn meta(oid: Oid) -> CommitMeta {
    CommitMeta {
        oid,
        summary: String::new(),
        author_name: String::new(),
        author_email: String::new(),
        author_time: Timestamp(0),
        commit_time: Timestamp(0),
    }
}

pub fn branch(name: &str, target: Oid, is_head: bool, is_remote: bool, time: i64) -> BranchInfo {
    BranchInfo {
        name: name.to_owned(),
        is_remote,
        target,
        is_head,
        upstream: None,
        tip_time: Timestamp(time),
    }
}

/// Activity as sorted `(week offset from MONDAY, author, commits)` triples.
pub fn activity_triples(buckets: &[ActivityBucket]) -> Vec<(i64, String, u32)> {
    let base = Timestamp(MONDAY).week_index();
    let mut v: Vec<_> = buckets
        .iter()
        .map(|b| (b.week - base, b.author.clone(), b.commits))
        .collect();
    v.sort();
    v
}
