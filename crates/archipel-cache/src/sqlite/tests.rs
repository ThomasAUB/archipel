//! White-box tests: migrations and corrupt data (need raw SQL access).

use std::sync::Mutex;

use archipel_model::journal::{
    EntryState, HeadRef, JournalEntry, JournalId, OperationKind, RefSnapshot,
};
use archipel_model::ports::store::{
    JournalStore, ProjectRegistry, StatsStore, SummaryStore, UiStateStore,
};
use archipel_model::testing::oid;
use archipel_model::{ProjectId, RepoId, StoreError, Timestamp};
use rusqlite::{Connection, params};

use super::{SqliteStore, configure, schema};

/// Tables every migrated database has.
const TABLES: &[&str] = &[
    "summaries",
    "ui_state",
    "registry",
    "journal",
    "stats_cursor",
    "stats_activity",
    "stats_churn",
];

fn tables(conn: &Connection) -> Vec<String> {
    let mut stmt = conn
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
        .unwrap();
    stmt.query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

fn raw(store: &SqliteStore) -> std::sync::MutexGuard<'_, Connection> {
    store.lock()
}

#[test]
fn migrates_an_empty_database_step_by_step() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.sqlite");
    {
        let mut conn = Connection::open(&path).unwrap();
        configure(&conn).unwrap();
        assert_eq!(schema::user_version(&conn).unwrap(), 0);
        assert!(tables(&conn).is_empty());
        schema::migrate_to(&mut conn, 1).unwrap();
        assert_eq!(schema::user_version(&conn).unwrap(), 1);
        assert!(tables(&conn).contains(&"summaries".to_owned()));
        assert!(!tables(&conn).contains(&"journal".to_owned()));
        conn.execute(
            "INSERT INTO ui_state (project, data) VALUES ('p', ?1)",
            params![serde_json::to_string(&archipel_model::ui_state::UiState::default()).unwrap()],
        )
        .unwrap();
    }
    let store = SqliteStore::open(&path).unwrap();
    assert_eq!(store.schema_version().unwrap(), SqliteStore::SCHEMA_VERSION);
    let names = tables(&raw(&store));
    for table in TABLES {
        assert!(names.contains(&(*table).to_owned()), "missing {table}");
    }
    // Data written at version 1 survives the upgrade.
    assert!(
        store
            .load_ui_state(&ProjectId("p".into()))
            .unwrap()
            .is_some()
    );
}

#[test]
fn v4_forgets_private_projects_and_their_state() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.sqlite");
    {
        let mut conn = Connection::open(&path).unwrap();
        configure(&conn).unwrap();
        schema::migrate_to(&mut conn, 3).unwrap();
        for sql in [
            r#"INSERT INTO registry VALUES ('dir:/w', '{"id":"dir:/w","name":"w","location":{"Private":"dir:/w"},"last_opened":null}')"#,
            r#"INSERT INTO registry VALUES ('file:/s', '{"id":"file:/s","name":"s","location":{"SharedFile":"/s"},"last_opened":null}')"#,
            "INSERT INTO private_projects VALUES ('dir:/w', '{}')",
            "INSERT INTO ui_state VALUES ('dir:/w', '{}')",
            "INSERT INTO ui_state VALUES ('file:/s', '{}')",
            "INSERT INTO summaries VALUES ('dir:/w', 0, 1, '{}')",
            "INSERT INTO summaries VALUES ('file:/s', 0, 1, '{}')",
        ] {
            conn.execute(sql, []).unwrap();
        }
    }
    let store = SqliteStore::open(&path).unwrap();
    let ids: Vec<_> = store
        .list_projects()
        .unwrap()
        .into_iter()
        .map(|e| e.id.0)
        .collect();
    assert_eq!(ids, ["file:/s"]);
    let conn = raw(&store);
    assert!(!tables(&conn).contains(&"private_projects".to_owned()));
    for table in ["ui_state", "summaries"] {
        let projects: Vec<String> = conn
            .prepare(&format!("SELECT project FROM {table}"))
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(projects, ["file:/s"], "{table}");
    }
}

#[test]
fn reopening_a_migrated_database_is_a_no_op() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.sqlite");
    drop(SqliteStore::open(&path).unwrap());
    let store = SqliteStore::open(&path).unwrap();
    assert_eq!(store.schema_version().unwrap(), SqliteStore::SCHEMA_VERSION);
}

#[test]
fn refuses_a_newer_schema() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.sqlite");
    {
        let conn = Connection::open(&path).unwrap();
        conn.pragma_update(None, "user_version", SqliteStore::SCHEMA_VERSION + 1)
            .unwrap();
    }
    let err = SqliteStore::open(&path).unwrap_err();
    assert!(
        matches!(err, StoreError::Backend(ref m) if m.contains("newer")),
        "{err:?}"
    );
}

#[test]
fn file_database_uses_wal() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(dir.path().join("a/b/store.sqlite")).unwrap();
    let mode: String = raw(&store)
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .unwrap();
    assert_eq!(mode, "wal");
    let fk: i64 = raw(&store)
        .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
        .unwrap();
    assert_eq!(fk, 1);
}

fn corrupt_store() -> SqliteStore {
    let conn = Connection::open_in_memory().unwrap();
    configure(&conn).unwrap();
    let store = SqliteStore {
        conn: Mutex::new(conn),
    };
    schema::migrate(&mut store.lock()).unwrap();
    {
        let conn = raw(&store);
        for sql in [
            "INSERT INTO summaries VALUES ('p', 0, 1, '{bad')",
            "INSERT INTO ui_state VALUES ('p', '42')",
            "INSERT INTO registry VALUES ('p', 'null')",
            "INSERT INTO journal (id, repo, batch, data) VALUES (1, 1, 7, 'nope')",
            "INSERT INTO stats_cursor VALUES (1, 'not-an-oid')",
            "INSERT INTO stats_activity VALUES (1, 0, 'a', -1)",
            "INSERT INTO stats_churn VALUES (1, 'f', -5, 0, 1)",
        ] {
            conn.execute(sql, []).unwrap();
        }
    }
    store
}

#[allow(clippy::needless_pass_by_value)] // takes call results directly
fn assert_corrupt<T: std::fmt::Debug>(result: Result<T, StoreError>) {
    assert!(matches!(result, Err(StoreError::Corrupt(_))), "{result:?}");
}

#[test]
fn undecodable_records_are_reported_as_corrupt() {
    let store = corrupt_store();
    let p = ProjectId("p".into());
    let repo = RepoId(1);
    assert_corrupt(store.load_summaries(&p));
    assert_corrupt(store.load_ui_state(&p));
    assert_corrupt(store.list_projects());
    assert_corrupt(store.mark_opened(&p, Timestamp(1)));
    assert_corrupt(store.recent(repo, 10));
    assert_corrupt(store.batch(archipel_model::journal::BatchId(7)));
    assert_corrupt(store.set_state(JournalId(1), EntryState::Undone));
    assert_corrupt(store.cursor(repo));
    assert_corrupt(store.activity(repo));
    assert_corrupt(store.hotspots(repo, 10));
}

#[test]
fn json_id_is_overridden_by_the_column() {
    let store = SqliteStore::open_in_memory().unwrap();
    let snapshot = RefSnapshot {
        head: HeadRef::Detached(oid(1)),
        refs: Vec::new(),
    };
    let entry = JournalEntry {
        id: JournalId(999),
        repo: RepoId(u64::MAX),
        batch: None,
        kind: OperationKind::Commit,
        label: "c".into(),
        before: snapshot.clone(),
        after: snapshot,
        worktree_snapshot: None,
        stash: None,
        time: Timestamp(0),
        state: EntryState::Done,
    };
    let id = store.append(&entry, 10).unwrap();
    assert_eq!(id, JournalId(1));
    assert_eq!(store.recent(RepoId(u64::MAX), 10).unwrap()[0].id, id);
}

#[test]
fn invalid_utf8_text_is_reported_as_corrupt() {
    let store = SqliteStore::open_in_memory().unwrap();
    raw(&store)
        .execute(
            "INSERT INTO summaries VALUES ('p', 0, 1, CAST(x'7b22ff227d' AS TEXT))",
            [],
        )
        .unwrap();
    assert_corrupt(store.load_summaries(&ProjectId("p".into())));
}

/// A registry entry for the recovery tests.
fn entry(id: &str) -> archipel_model::project::RegistryEntry {
    archipel_model::project::RegistryEntry {
        id: ProjectId(id.into()),
        name: id.into(),
        location: archipel_model::project::ProjectLocation::SharedFile(
            format!("/projects/{id}/archipel.toml").into(),
        ),
        last_opened: None,
    }
}

#[test]
fn a_file_that_is_not_a_database_is_replaced() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.sqlite");
    std::fs::write(&path, vec![0x5a; 8192]).unwrap();
    let store = SqliteStore::open(&path).unwrap();
    assert_eq!(store.schema_version().unwrap(), SqliteStore::SCHEMA_VERSION);
    assert!(store.list_projects().unwrap().is_empty());
    assert_eq!(
        std::fs::read(dir.path().join("store.sqlite.corrupt")).unwrap(),
        vec![0x5a; 8192],
        "the damaged file is kept aside"
    );
}

#[test]
fn a_damaged_database_is_replaced_and_the_registry_salvaged() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.sqlite");
    {
        let store = SqliteStore::open(&path).unwrap();
        store.upsert_project(&entry("kept")).unwrap();
        // Spread the summaries over many pages, each tagged with a marker.
        let conn = raw(&store);
        for i in 0..64 {
            let data = format!("\"MARKER{}\"", "x".repeat(900));
            conn.execute(
                "INSERT INTO summaries VALUES ('p', ?1, 1, ?2)",
                params![i, data],
            )
            .unwrap();
        }
        conn.pragma_update(None, "wal_checkpoint", "TRUNCATE")
            .unwrap();
    }
    // Overwrite the b-tree header of a page holding summaries.
    let mut bytes = std::fs::read(&path).unwrap();
    let at = bytes
        .windows(6)
        .rposition(|w| w == b"MARKER")
        .expect("marker on disk");
    let page = at / 4096 * 4096;
    assert!(page > 0, "not the header page");
    bytes[page..page + 12].fill(0xff);
    std::fs::write(&path, bytes).unwrap();

    let store = SqliteStore::open(&path).unwrap();
    let projects = store.list_projects().unwrap();
    assert_eq!(projects.len(), 1, "registry salvaged: {projects:?}");
    assert_eq!(projects[0].id, ProjectId("kept".into()));
    assert!(
        store
            .load_summaries(&ProjectId("p".into()))
            .unwrap()
            .is_empty()
    );
    assert!(dir.path().join("store.sqlite.corrupt").exists());
}
