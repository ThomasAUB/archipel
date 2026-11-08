#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Contract tests of `SqliteStore`, written through the store traits and run
//! against both a temp-file database and an in-memory one.

use std::path::{Path, PathBuf};

use archipel_cache::SqliteStore;
use archipel_model::journal::{
    BatchId, EntryState, HeadRef, JournalEntry, JournalId, OperationKind, RefSnapshot, StashRecord,
};
use archipel_model::ports::store::{
    JournalStore, ProjectRegistry, StatsStore, SummaryStore, UiStateStore,
};
use archipel_model::project::{ProjectLocation, RegistryEntry};
use archipel_model::stats::{ActivityBucket, Hotspot, StatsDelta};
use archipel_model::summary::{
    ChangeCounts, ChangedFiles, HeadState, RepoOp, RepoSummary, Upstream,
};
use archipel_model::testing::{InMemoryStore, oid};
use archipel_model::ui_state::{GraphColumn, SortDirection, TabState, UiState};
use archipel_model::{ProjectId, RepoId, StoreError, Timestamp};

/// Runs `test` against a fresh temp-file store and a fresh in-memory store.
fn each_store(test: impl Fn(&SqliteStore)) {
    let dir = tempfile::tempdir().unwrap();
    test(&SqliteStore::open(dir.path().join("store.sqlite")).unwrap());
    test(&SqliteStore::open_in_memory().unwrap());
}

fn pid(name: &str) -> ProjectId {
    ProjectId(name.to_owned())
}

fn summary(n: u32) -> RepoSummary {
    RepoSummary {
        head: HeadState::Branch(format!("branch-{n}")),
        head_oid: Some(oid(u64::from(n))),
        upstream: Some(Upstream {
            name: "origin/main".into(),
            ahead: n,
            behind: 2,
            gone: false,
        }),
        changes: ChangeCounts {
            modified: n,
            staged: 1,
            untracked: 2,
            conflicted: 0,
        },
        changed_files: ChangedFiles::default(),
        containing_branches: Vec::new(),
        stash_count: 3,
        describe: Some(format!("v1.{n}-2-gabc")),
        last_fetch: Some(Timestamp(1_700_000_000)),
        in_progress: Some(RepoOp::Rebase),
        has_submodules: true,
        computed_at: Timestamp(1_700_000_100),
    }
}

fn ui_state() -> UiState {
    UiState {
        tabs: vec![TabState::Dashboard, TabState::Repo(RepoId(u64::MAX))],
        active_tab: 1,
        selected_repo: Some(RepoId(7)),
        filters: vec!["dirty".into()],
        sort_column: Some("name".into()),
        sort_direction: SortDirection::Descending,
        sidebar_width: 300.5,
        refs_width: 200.0,
        details_width: 500.0,
        quick_edit_width: 400.0,
        sidebar_collapsed: true,
        drawer_height: 120.0,
        selection: vec![RepoId(8)],
        hidden_repos: vec![RepoId(9)],
        show_hidden: true,
        show_dependency_tree: true,
        collapsed_nodes: vec!["k".into()],
        collapsed_categories: vec!["libs".into()],
        collapsed_ref_sections: vec!["TAGS".into()],
        hidden_graph_columns: vec![GraphColumn::Commit],
        category_filters: vec!["tools".into()],
        accent: Some("#112233".into()),
        icon: Some("🧪".into()),
    }
}

fn registry_entry(id: &str) -> RegistryEntry {
    RegistryEntry {
        id: pid(id),
        name: format!("Project {id}"),
        location: ProjectLocation::SharedFile(PathBuf::from(format!("/work/{id}archipel.toml"))),
        last_opened: None,
    }
}

fn journal_entry(repo: u64, batch: Option<u64>, label: &str) -> JournalEntry {
    JournalEntry {
        id: JournalId(0),
        repo: RepoId(repo),
        batch: batch.map(BatchId),
        kind: OperationKind::Commit,
        label: label.to_owned(),
        before: RefSnapshot {
            head: HeadRef::Symbolic("refs/heads/main".into()),
            refs: vec![("refs/heads/main".into(), Some(oid(1)))],
        },
        after: RefSnapshot {
            head: HeadRef::Detached(oid(2)),
            refs: vec![
                ("refs/heads/main".into(), Some(oid(2))),
                ("refs/tags/x".into(), None),
            ],
        },
        worktree_snapshot: Some(oid(3)),
        stash: Some(StashRecord {
            oid: oid(4),
            message: "wip".into(),
        }),
        time: Timestamp(42),
        state: EntryState::Done,
    }
}

fn labels(entries: &[JournalEntry]) -> Vec<&str> {
    entries.iter().map(|e| e.label.as_str()).collect()
}

fn bucket(week: i64, author: &str, commits: u32) -> ActivityBucket {
    ActivityBucket {
        week,
        author: author.to_owned(),
        commits,
    }
}

fn hotspot(path: &str, added: u64, removed: u64, commits: u32) -> Hotspot {
    Hotspot {
        path: path.to_owned(),
        added,
        removed,
        commits,
    }
}

// ---- summaries ----

#[test]
fn summaries_round_trip_in_order_and_are_replaced() {
    each_store(|store| {
        let s: &dyn SummaryStore = store;
        assert!(s.load_summaries(&pid("p")).unwrap().is_empty());
        let saved = vec![
            (RepoId(u64::MAX), summary(1)),
            (RepoId(3), summary(2)),
            (RepoId(0), summary(3)),
        ];
        s.save_summaries(&pid("p"), &saved).unwrap();
        s.save_summaries(&pid("other"), &saved[..1]).unwrap();
        assert_eq!(s.load_summaries(&pid("p")).unwrap(), saved);

        let replaced = vec![(RepoId(9), RepoSummary::empty("main"))];
        s.save_summaries(&pid("p"), &replaced).unwrap();
        assert_eq!(s.load_summaries(&pid("p")).unwrap(), replaced);
        assert_eq!(s.load_summaries(&pid("other")).unwrap(), saved[..1]);

        s.save_summaries(&pid("p"), &[]).unwrap();
        assert!(s.load_summaries(&pid("p")).unwrap().is_empty());
    });
}

// ---- UI state ----

#[test]
fn ui_state_round_trips_and_is_overwritten() {
    each_store(|store| {
        let s: &dyn UiStateStore = store;
        assert_eq!(s.load_ui_state(&pid("p")).unwrap(), None);
        s.save_ui_state(&pid("p"), &ui_state()).unwrap();
        assert_eq!(s.load_ui_state(&pid("p")).unwrap(), Some(ui_state()));
        s.save_ui_state(&pid("p"), &UiState::default()).unwrap();
        assert_eq!(
            s.load_ui_state(&pid("p")).unwrap(),
            Some(UiState::default())
        );
        assert_eq!(s.load_ui_state(&pid("q")).unwrap(), None);
    });
}

// ---- registry ----

#[test]
fn registry_upsert_list_remove() {
    each_store(|store| {
        let r: &dyn ProjectRegistry = store;
        assert!(r.list_projects().unwrap().is_empty());
        r.upsert_project(&registry_entry("b")).unwrap();
        r.upsert_project(&registry_entry("a")).unwrap();
        r.upsert_project(&registry_entry("c")).unwrap();
        assert_eq!(
            r.list_projects().unwrap(),
            vec![
                registry_entry("a"),
                registry_entry("b"),
                registry_entry("c")
            ]
        );

        let mut renamed = registry_entry("a");
        renamed.name = "Renamed".into();
        r.upsert_project(&renamed).unwrap();
        assert_eq!(r.list_projects().unwrap()[0], renamed);

        r.remove_project(&pid("b")).unwrap();
        r.remove_project(&pid("missing")).unwrap();
        assert_eq!(
            r.list_projects().unwrap(),
            vec![renamed, registry_entry("c")]
        );
    });
}

#[test]
fn mark_opened_updates_or_reports_not_found() {
    each_store(|store| {
        let r: &dyn ProjectRegistry = store;
        r.upsert_project(&registry_entry("a")).unwrap();
        r.mark_opened(&pid("a"), Timestamp(123)).unwrap();
        assert_eq!(
            r.list_projects().unwrap()[0].last_opened,
            Some(Timestamp(123))
        );
        assert_eq!(
            r.mark_opened(&pid("zz"), Timestamp(1)),
            Err(StoreError::NotFound("zz".into()))
        );
    });
}

#[test]
fn journal_append_assigns_increasing_ids_and_recent_is_newest_first() {
    each_store(|store| {
        let j: &dyn JournalStore = store;
        let a = j.append(&journal_entry(1, None, "a"), 10).unwrap();
        let b = j.append(&journal_entry(2, None, "b"), 10).unwrap();
        let c = j.append(&journal_entry(1, None, "c"), 10).unwrap();
        assert!(a < b && b < c);
        let recent = j.recent(RepoId(1), 10).unwrap();
        assert_eq!(labels(&recent), ["c", "a"]);
        assert_eq!(recent[0].id, c);
        assert_eq!(recent[1].id, a);
        let mut expected = journal_entry(1, None, "c");
        expected.id = c;
        assert_eq!(recent[0], expected);
        assert_eq!(labels(&j.recent(RepoId(1), 1).unwrap()), ["c"]);
        assert!(j.recent(RepoId(1), 0).unwrap().is_empty());
        assert!(j.recent(RepoId(3), 10).unwrap().is_empty());
    });
}

#[test]
fn journal_prunes_each_repo_to_limit() {
    each_store(|store| {
        let j: &dyn JournalStore = store;
        for i in 0..5 {
            j.append(&journal_entry(1, None, &format!("r1-{i}")), 3)
                .unwrap();
            j.append(&journal_entry(u64::MAX, None, &format!("r2-{i}")), 10)
                .unwrap();
        }
        assert_eq!(
            labels(&j.recent(RepoId(1), 100).unwrap()),
            ["r1-4", "r1-3", "r1-2"]
        );
        // The other repo is untouched by repo 1's pruning.
        assert_eq!(j.recent(RepoId(u64::MAX), 100).unwrap().len(), 5);
        // A lower limit prunes older entries on the next append.
        j.append(&journal_entry(u64::MAX, None, "r2-last"), 2)
            .unwrap();
        assert_eq!(
            labels(&j.recent(RepoId(u64::MAX), 100).unwrap()),
            ["r2-last", "r2-4"]
        );
        // Limit 0 keeps nothing, but still returns an id.
        let id = j.append(&journal_entry(1, None, "gone"), 0).unwrap();
        assert!(id.0 > 0);
        assert!(j.recent(RepoId(1), 100).unwrap().is_empty());
    });
}

#[test]
fn journal_ids_are_not_reused_after_pruning() {
    each_store(|store| {
        let j: &dyn JournalStore = store;
        let first = j.append(&journal_entry(1, None, "a"), 0).unwrap();
        let second = j.append(&journal_entry(1, None, "b"), 0).unwrap();
        assert!(second > first);
    });
}

#[test]
fn journal_batch_returns_entries_in_order() {
    each_store(|store| {
        let j: &dyn JournalStore = store;
        j.append(&journal_entry(1, Some(7), "a"), 10).unwrap();
        j.append(&journal_entry(2, Some(8), "x"), 10).unwrap();
        j.append(&journal_entry(2, Some(7), "b"), 10).unwrap();
        j.append(&journal_entry(3, Some(u64::MAX), "big"), 10)
            .unwrap();
        j.append(&journal_entry(3, None, "none"), 10).unwrap();
        assert_eq!(labels(&j.batch(BatchId(7)).unwrap()), ["a", "b"]);
        assert_eq!(labels(&j.batch(BatchId(u64::MAX)).unwrap()), ["big"]);
        assert!(j.batch(BatchId(99)).unwrap().is_empty());
    });
}

#[test]
fn journal_set_state() {
    each_store(|store| {
        let j: &dyn JournalStore = store;
        let id = j.append(&journal_entry(1, None, "a"), 10).unwrap();
        j.set_state(id, EntryState::Undone).unwrap();
        let entry = &j.recent(RepoId(1), 1).unwrap()[0];
        assert_eq!((entry.id, entry.state), (id, EntryState::Undone));
        j.set_state(id, EntryState::Done).unwrap();
        assert_eq!(j.recent(RepoId(1), 1).unwrap()[0].state, EntryState::Done);
        assert_eq!(
            j.set_state(JournalId(9999), EntryState::Done),
            Err(StoreError::NotFound("journal entry 9999".into()))
        );
    });
}

// ---- stats ----

#[test]
fn stats_accumulate_over_several_deltas() {
    each_store(|store| {
        let s: &dyn StatsStore = store;
        let repo = RepoId(u64::MAX - 1);
        assert_eq!(s.cursor(repo).unwrap(), None);
        assert!(s.activity(repo).unwrap().is_empty());
        assert!(s.hotspots(repo, 10).unwrap().is_empty());

        s.apply_delta(
            repo,
            &StatsDelta {
                activity: vec![
                    bucket(10, "a@x", 2),
                    bucket(11, "b@x", 1),
                    bucket(10, "a@x", 1),
                ],
                churn: vec![hotspot("src/a.rs", 10, 2, 1), hotspot("b.rs", 1, 1, 1)],
            },
            oid(1),
        )
        .unwrap();
        assert_eq!(s.cursor(repo).unwrap(), Some(oid(1)));

        s.apply_delta(
            repo,
            &StatsDelta {
                activity: vec![bucket(11, "b@x", 4), bucket(9, "c@x", 1)],
                churn: vec![hotspot("b.rs", 30, 0, 2), hotspot("c.rs", 6, 6, 1)],
            },
            oid(2),
        )
        .unwrap();
        s.apply_delta(repo, &StatsDelta::default(), oid(3)).unwrap();
        assert_eq!(s.cursor(repo).unwrap(), Some(oid(3)));

        assert_eq!(
            s.activity(repo).unwrap(),
            vec![
                bucket(9, "c@x", 1),
                bucket(10, "a@x", 3),
                bucket(11, "b@x", 5)
            ]
        );
        // b.rs: 32 churn; a.rs: 12; c.rs: 12 (tie broken by path).
        assert_eq!(
            s.hotspots(repo, 10).unwrap(),
            vec![
                hotspot("b.rs", 31, 1, 3),
                hotspot("c.rs", 6, 6, 1),
                hotspot("src/a.rs", 10, 2, 1)
            ]
        );
        assert_eq!(
            s.hotspots(repo, 1).unwrap(),
            vec![hotspot("b.rs", 31, 1, 3)]
        );
        assert!(s.hotspots(repo, 0).unwrap().is_empty());
        // Other repos are unaffected.
        assert_eq!(s.cursor(RepoId(1)).unwrap(), None);
    });
}

#[test]
fn stats_reset_forgets_one_repo() {
    each_store(|store| {
        let s: &dyn StatsStore = store;
        let delta = StatsDelta {
            activity: vec![bucket(1, "a", 1)],
            churn: vec![hotspot("f", 1, 1, 1)],
        };
        s.apply_delta(RepoId(1), &delta, oid(1)).unwrap();
        s.apply_delta(RepoId(2), &delta, oid(2)).unwrap();
        s.reset_stats(RepoId(1)).unwrap();
        assert_eq!(s.cursor(RepoId(1)).unwrap(), None);
        assert!(s.activity(RepoId(1)).unwrap().is_empty());
        assert!(s.hotspots(RepoId(1), 10).unwrap().is_empty());
        assert_eq!(s.cursor(RepoId(2)).unwrap(), Some(oid(2)));
        assert_eq!(s.activity(RepoId(2)).unwrap().len(), 1);
        s.reset_stats(RepoId(3)).unwrap();
    });
}

#[test]
fn apply_delta_is_atomic() {
    each_store(|store| {
        let s: &dyn StatsStore = store;
        let delta = StatsDelta {
            activity: vec![bucket(1, "a", 1)],
            churn: vec![hotspot("ok", 1, 1, 1), hotspot("too-big", u64::MAX, 0, 1)],
        };
        assert!(matches!(
            s.apply_delta(RepoId(1), &delta, oid(1)),
            Err(StoreError::Backend(_))
        ));
        assert_eq!(s.cursor(RepoId(1)).unwrap(), None);
        assert!(s.activity(RepoId(1)).unwrap().is_empty());
        assert!(s.hotspots(RepoId(1), 10).unwrap().is_empty());
    });
}

// ---- parity with the in-memory reference ----

#[test]
fn journal_and_stats_match_the_in_memory_reference() {
    each_store(|store| {
        let reference = InMemoryStore::new();
        let stores: [&dyn JournalStore; 2] = [store, &reference];
        for (i, repo) in [1, 2, 1, 1, 3, 1, 2].into_iter().enumerate() {
            let entry = journal_entry(repo, Some(repo % 2), &format!("e{i}"));
            let ids: Vec<_> = stores
                .iter()
                .map(|s| s.append(&entry, 3).unwrap())
                .collect();
            assert_eq!(ids[0], ids[1]);
        }
        for repo in 1..=3 {
            assert_eq!(
                stores[0].recent(RepoId(repo), 10).unwrap(),
                stores[1].recent(RepoId(repo), 10).unwrap()
            );
        }
        for batch in 0..2 {
            assert_eq!(
                stores[0].batch(BatchId(batch)).unwrap(),
                stores[1].batch(BatchId(batch)).unwrap()
            );
        }

        let stats: [&dyn StatsStore; 2] = [store, &reference];
        let deltas = [
            StatsDelta {
                activity: vec![bucket(3, "x", 1), bucket(1, "y", 2)],
                churn: vec![hotspot("a", 5, 0, 1), hotspot("b", 2, 3, 1)],
            },
            StatsDelta {
                activity: vec![bucket(1, "y", 1)],
                churn: vec![hotspot("b", 1, 0, 1), hotspot("c", 9, 9, 2)],
            },
        ];
        for (n, delta) in (1..).zip(&deltas) {
            for s in stats {
                s.apply_delta(RepoId(5), delta, oid(n)).unwrap();
            }
        }
        assert_eq!(
            stats[0].cursor(RepoId(5)).unwrap(),
            stats[1].cursor(RepoId(5)).unwrap()
        );
        assert_eq!(
            stats[0].hotspots(RepoId(5), 10).unwrap(),
            stats[1].hotspots(RepoId(5), 10).unwrap()
        );
        // Activity order is unspecified by the trait: compare as sets.
        let mut expected = stats[1].activity(RepoId(5)).unwrap();
        expected.sort_by(|a, b| (a.week, &a.author).cmp(&(b.week, &b.author)));
        assert_eq!(stats[0].activity(RepoId(5)).unwrap(), expected);
    });
}

// ---- persistence ----

fn fill(path: &Path) {
    let store = SqliteStore::open(path).unwrap();
    store
        .save_summaries(&pid("p"), &[(RepoId(u64::MAX), summary(1))])
        .unwrap();
    store.save_ui_state(&pid("p"), &ui_state()).unwrap();
    store.upsert_project(&registry_entry("p")).unwrap();
    store.mark_opened(&pid("p"), Timestamp(5)).unwrap();
    store
        .append(&journal_entry(1, Some(4), "kept"), 10)
        .unwrap();
    store
        .apply_delta(
            RepoId(1),
            &StatsDelta {
                activity: vec![bucket(1, "a", 1)],
                churn: vec![hotspot("f", 1, 2, 1)],
            },
            oid(9),
        )
        .unwrap();
}

#[test]
fn file_database_persists_across_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nested/dir/store.sqlite");
    fill(&path);
    let store = SqliteStore::open(&path).unwrap();
    assert_eq!(store.schema_version().unwrap(), SqliteStore::SCHEMA_VERSION);
    assert_eq!(
        store.load_summaries(&pid("p")).unwrap(),
        vec![(RepoId(u64::MAX), summary(1))]
    );
    assert_eq!(store.load_ui_state(&pid("p")).unwrap(), Some(ui_state()));
    let mut entry = registry_entry("p");
    entry.last_opened = Some(Timestamp(5));
    assert_eq!(store.list_projects().unwrap(), vec![entry]);
    assert_eq!(labels(&store.batch(BatchId(4)).unwrap()), ["kept"]);
    assert_eq!(store.cursor(RepoId(1)).unwrap(), Some(oid(9)));
    assert_eq!(
        store.hotspots(RepoId(1), 5).unwrap(),
        vec![hotspot("f", 1, 2, 1)]
    );
    // Ids keep increasing after reopen.
    let next = store.append(&journal_entry(1, None, "next"), 10).unwrap();
    assert_eq!(next, JournalId(2));
}

#[test]
fn store_is_usable_from_several_threads() {
    let store = std::sync::Arc::new(SqliteStore::open_in_memory().unwrap());
    let handles: Vec<_> = (0..4)
        .map(|t| {
            let store = std::sync::Arc::clone(&store);
            std::thread::spawn(move || {
                for i in 0..25 {
                    store
                        .append(&journal_entry(t, None, &format!("{t}-{i}")), 100)
                        .unwrap();
                }
            })
        })
        .collect();
    for handle in handles {
        handle.join().unwrap();
    }
    for t in 0..4 {
        assert_eq!(store.recent(RepoId(t), 100).unwrap().len(), 25);
    }
}
