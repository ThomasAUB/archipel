#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! `StatsRegistry`, built-in providers and project aggregation on fakes.

#[allow(dead_code)]
mod common;

use std::sync::Arc;
use std::time::Duration;

use archipel_model::testing::oid;
use archipel_model::{CancelToken, StoreError, Timestamp};
use archipel_stats::{
    ACTIVITY_ID, BRANCHES_ID, BranchesView, HOTSPOTS_ID, HotspotsProvider, PauseToken, RepoTarget,
    StatProvider, StatSources, StatView, StatsConfig, StatsError, StatsRegistry, TableView,
};
use common::{Fixture, MONDAY, WEEK, branch, churn, meta, node};

/// A fork-style provider: number of local branches per repo.
struct BranchCount;

impl StatProvider for BranchCount {
    fn id(&self) -> &'static str {
        "branch-count"
    }
    fn label(&self) -> &'static str {
        "Branch count"
    }
    fn compute(&self, sources: &StatSources, repo: &RepoTarget) -> Result<StatView, StatsError> {
        let n = sources.refs.branches(&repo.path)?.len();
        Ok(StatView::Table(TableView {
            columns: vec!["repo".into(), "branches".into()],
            rows: vec![vec![repo.name.clone(), n.to_string()]],
        }))
    }
    fn aggregate(&self, views: &[StatView]) -> StatView {
        let rows = views
            .iter()
            .flat_map(|v| match v {
                StatView::Table(t) => t.rows.clone(),
                _ => Vec::new(),
            })
            .collect();
        StatView::Table(TableView {
            columns: vec!["repo".into(), "branches".into()],
            rows,
        })
    }
}

fn update(fx: &Fixture) {
    fx.engine()
        .update(
            fx.target.id,
            &fx.target.path,
            &CancelToken::new(),
            &PauseToken::new(),
        )
        .unwrap();
}

#[test]
fn builtins_are_registered_and_custom_providers_accepted() {
    let mut registry = StatsRegistry::with_builtins(&StatsConfig::default());
    assert_eq!(registry.ids(), vec![ACTIVITY_ID, BRANCHES_ID, HOTSPOTS_ID]);

    assert!(registry.register(Arc::new(BranchCount)).is_none());
    assert_eq!(registry.ids().len(), 4);

    let fx = Fixture::new("r");
    fx.set_linear(&[churn(1, "a@x", MONDAY, &[("f", 1, 0)])]);
    let stats = registry.compute_repo(&fx.sources(), &fx.target);
    let Some(StatView::Table(t)) = stats.view("branch-count") else {
        panic!("custom view missing: {stats:?}");
    };
    assert_eq!(t.rows, vec![vec!["r".to_owned(), "1".to_owned()]]);

    // A fork overrides a built-in by registering the same id, in place.
    let old = registry.register(Arc::new(HotspotsProvider::new(1)));
    assert!(old.is_some());
    assert_eq!(registry.ids()[2], HOTSPOTS_ID);
    assert!(registry.unregister("branch-count").is_some());
    assert!(registry.get("branch-count").is_none());
}

#[test]
fn provider_errors_are_isolated_per_provider() {
    let fx = Fixture::new("r");
    fx.set_linear(&[churn(1, "a@x", MONDAY, &[("f", 1, 0)])]);
    fx.git.fail(
        &fx.target.path,
        "branches",
        archipel_model::GitError::Backend("x".into()),
    );
    let registry = StatsRegistry::with_builtins(&StatsConfig::default());
    let stats = registry.compute_repo(&fx.sources(), &fx.target);
    assert!(stats.view(ACTIVITY_ID).is_some());
    assert!(stats.view(BRANCHES_ID).is_none());
    // Store errors convert too.
    let e: StatsError = StoreError::Backend("db".into()).into();
    assert!(matches!(e, StatsError::Store(_)));
}

fn branches_view(fx: &Fixture) -> BranchesView {
    let registry = StatsRegistry::with_builtins(&StatsConfig::default());
    match registry
        .compute_repo(&fx.sources(), &fx.target)
        .view(BRANCHES_ID)
    {
        Some(StatView::Branches(b)) => b.clone(),
        other => panic!("{other:?}"),
    }
}

/// main: 1-2-3-4 ; merged at 2 ; open: 2-5 ; wip (current) at 1.
fn branchy_repo(fx: &Fixture) {
    fx.git.with_repo(&fx.target.path, |r| {
        r.history = vec![
            (node(oid(5), vec![oid(2)]), meta(oid(5))),
            (node(oid(4), vec![oid(3)]), meta(oid(4))),
            (node(oid(3), vec![oid(2)]), meta(oid(3))),
            (node(oid(2), vec![oid(1)]), meta(oid(2))),
            (node(oid(1), vec![]), meta(oid(1))),
        ];
        r.branches = vec![
            branch("main", oid(4), false, false, MONDAY + 4),
            branch("merged", oid(2), false, false, MONDAY + 2),
            branch("open", oid(5), false, false, MONDAY + 5),
            branch("wip", oid(1), true, false, MONDAY + 1),
            branch("origin/merged", oid(2), false, true, MONDAY + 2),
        ];
    });
}

fn names(v: &BranchesView) -> Vec<(&str, bool, bool)> {
    v.branches
        .iter()
        .map(|r| (r.age.name.as_str(), r.age.merged, r.current))
        .collect()
}

#[test]
fn branches_fall_back_to_main_and_flag_merged_not_deleted() {
    let fx = Fixture::new("r");
    branchy_repo(&fx);
    let view = branches_view(&fx);
    // Oldest first; default branch excluded; current never flagged merged.
    assert_eq!(
        names(&view),
        vec![
            ("wip", false, true),
            ("merged", true, false),
            ("open", false, false)
        ]
    );
    let merged: Vec<&str> = view
        .merged_not_deleted()
        .map(|r| r.age.name.as_str())
        .collect();
    assert_eq!(merged, vec!["merged"]);
}

#[test]
fn stale_branches_are_old_unmerged_and_not_checked_out() {
    let fx = Fixture::new("r");
    branchy_repo(&fx);
    let view = branches_view(&fx);
    let stale = |now: i64, secs: u64| -> Vec<String> {
        view.stale(Timestamp(now), Duration::from_secs(secs))
            .map(|r| r.age.name.clone())
            .collect()
    };
    // `wip` is current, `merged` is merged: only `open` can be stale.
    assert_eq!(stale(MONDAY + 5, 0), vec!["open"]);
    assert_eq!(stale(MONDAY + 10, 5), vec!["open"]);
    assert!(stale(MONDAY + 10, 6).is_empty());
}

#[test]
fn default_branch_comes_from_origin_head_then_configuration() {
    let fx = Fixture::new("r");
    branchy_repo(&fx);
    fx.git.with_repo(&fx.target.path, |r| {
        // origin/HEAD -> origin/open: `open` becomes the default branch.
        r.branches
            .push(branch("origin/open", oid(5), false, true, MONDAY + 5));
        r.refs.insert("refs/remotes/origin/HEAD".into(), oid(5));
    });
    let view = branches_view(&fx);
    assert_eq!(
        names(&view),
        vec![
            ("wip", false, true),
            ("merged", true, false),
            ("main", false, false)
        ]
    );

    // An explicit configuration wins over origin/HEAD.
    let mut fx = fx;
    fx.target.default_branch = Some("main".into());
    let view = branches_view(&fx);
    assert_eq!(
        names(&view),
        vec![
            ("wip", false, true),
            ("merged", true, false),
            ("open", false, false)
        ]
    );
}

#[test]
fn master_is_the_last_fallback_and_no_default_means_nothing_merged() {
    let fx = Fixture::new("r");
    branchy_repo(&fx);
    fx.git
        .with_repo(&fx.target.path, |r| r.branches[0].name = "master".into());
    assert_eq!(branches_view(&fx).merged_not_deleted().count(), 1);

    fx.git
        .with_repo(&fx.target.path, |r| r.branches[0].name = "trunk".into());
    let view = branches_view(&fx);
    assert_eq!(view.merged_not_deleted().count(), 0);
    assert_eq!(view.branches.len(), 4, "trunk is listed when not default");
}

#[test]
fn project_aggregation_sums_activity_and_merges_hotspots() {
    let a = Fixture::new("alpha");
    let b = Fixture::with(a.git.clone(), a.store.clone(), "beta");
    a.set_linear(&[
        churn(1, "x@x", MONDAY, &[("big.rs", 100, 50)]),
        churn(2, "y@x", MONDAY + WEEK, &[("small.rs", 1, 0)]),
    ]);
    b.set_linear(&[
        churn(11, "X@x", MONDAY + 60, &[("mid.rs", 30, 0)]),
        churn(12, "z@x", MONDAY + 120, &[("tiny.rs", 1, 1)]),
    ]);
    update(&a);
    update(&b);

    let registry = StatsRegistry::with_builtins(&StatsConfig { hotspot_limit: 2 });
    let project = registry.compute_project(&a.sources(), &[a.target.clone(), b.target.clone()]);
    assert_eq!(project.repos.len(), 2);
    assert_eq!(project.repos[1].name, "beta");

    let Some(StatView::Activity(act)) = project.aggregated(ACTIVITY_ID) else {
        panic!()
    };
    assert_eq!(
        common::activity_triples(&act.buckets),
        vec![
            (0, "x@x".into(), 2),
            (0, "z@x".into(), 1),
            (1, "y@x".into(), 1)
        ]
    );
    assert_eq!(act.total_commits(), 4);
    assert_eq!(act.authors(), vec!["x@x", "y@x", "z@x"]);

    let Some(StatView::Hotspots(hot)) = project.aggregated(HOTSPOTS_ID) else {
        panic!()
    };
    let rows: Vec<(&str, &str)> = hot
        .hotspots
        .iter()
        .map(|r| (r.repo.as_str(), r.hotspot.path.as_str()))
        .collect();
    assert_eq!(rows, vec![("alpha", "big.rs"), ("beta", "mid.rs")]);

    let Some(StatView::Branches(br)) = project.aggregated(BRANCHES_ID) else {
        panic!()
    };
    assert!(br.branches.is_empty(), "only `main` exists in both repos");
}
