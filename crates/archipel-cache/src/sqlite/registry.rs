//! The project registry.

use archipel_model::ports::store::ProjectRegistry;
use archipel_model::project::RegistryEntry;
use archipel_model::{ProjectId, StoreError, Timestamp};
use rusqlite::params;

use super::codec::{from_json, load_json, save_json};
use super::{SqliteStore, db, write_tx};

/// Name of a registry record in error messages.
const REGISTRY_ENTRY: &str = "registry entry";

/// Selects the record of one registry entry.
const SELECT_ENTRY: &str = "SELECT data FROM registry WHERE id = ?1";
/// Inserts or replaces one registry entry.
const UPSERT_ENTRY: &str = "INSERT OR REPLACE INTO registry (id, data) VALUES (?1, ?2)";

impl ProjectRegistry for SqliteStore {
    /// Every known project, ordered by id.
    fn list_projects(&self) -> Result<Vec<RegistryEntry>, StoreError> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare_cached("SELECT data FROM registry ORDER BY id")
            .map_err(db)?;
        let rows: Vec<String> = stmt
            .query_map([], |row| row.get(0))
            .and_then(Iterator::collect)
            .map_err(db)?;
        rows.iter()
            .map(|data| from_json(data, REGISTRY_ENTRY))
            .collect()
    }

    fn upsert_project(&self, entry: &RegistryEntry) -> Result<(), StoreError> {
        save_json(
            &self.lock(),
            UPSERT_ENTRY,
            &entry.id.0,
            entry,
            REGISTRY_ENTRY,
        )
    }

    fn remove_project(&self, id: &ProjectId) -> Result<(), StoreError> {
        self.lock()
            .execute("DELETE FROM registry WHERE id = ?1", params![id.0])
            .map_err(db)?;
        Ok(())
    }

    fn mark_opened(&self, id: &ProjectId, when: Timestamp) -> Result<(), StoreError> {
        let mut conn = self.lock();
        let tx = write_tx(&mut conn)?;
        let mut entry: RegistryEntry = load_json(&tx, SELECT_ENTRY, &id.0, REGISTRY_ENTRY)?
            .ok_or_else(|| StoreError::NotFound(id.to_string()))?;
        entry.last_opened = Some(when);
        save_json(&tx, UPSERT_ENTRY, &id.0, &entry, REGISTRY_ENTRY)?;
        tx.commit().map_err(db)
    }
}
