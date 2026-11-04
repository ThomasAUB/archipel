//! Conversions between model values and SQLite column values.

use archipel_model::journal::BatchId;
use archipel_model::{RepoId, StoreError};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use serde::de::DeserializeOwned;

use super::db;

/// `RepoId` as the `i64` with the same bit pattern (lossless).
pub(super) fn repo_to_sql(repo: RepoId) -> i64 {
    i64::from_ne_bytes(repo.0.to_ne_bytes())
}

/// Inverse of [`repo_to_sql`].
pub(super) fn repo_from_sql(value: i64) -> RepoId {
    RepoId(u64::from_ne_bytes(value.to_ne_bytes()))
}

/// `BatchId` as the `i64` with the same bit pattern (lossless).
pub(super) fn batch_to_sql(batch: BatchId) -> i64 {
    i64::from_ne_bytes(batch.0.to_ne_bytes())
}

/// A position or limit as an SQLite integer (saturating: no table can hold
/// more than `i64::MAX` rows anyway).
pub(super) fn usize_to_sql(value: usize) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

/// A `u64` counter as an SQLite integer.
pub(super) fn u64_to_sql(value: u64) -> Result<i64, StoreError> {
    i64::try_from(value)
        .map_err(|_| StoreError::Backend(format!("counter {value} exceeds the SQLite range")))
}

/// A stored `u64` counter.
pub(super) fn u64_from_sql(value: i64, what: &str) -> Result<u64, StoreError> {
    u64::try_from(value).map_err(|_| StoreError::Corrupt(format!("{what}: invalid count {value}")))
}

/// A stored `u32` counter.
pub(super) fn u32_from_sql(value: i64, what: &str) -> Result<u32, StoreError> {
    u32::try_from(value).map_err(|_| StoreError::Corrupt(format!("{what}: invalid count {value}")))
}

/// Serializes a record to JSON.
pub(super) fn to_json<T: Serialize>(value: &T, what: &str) -> Result<String, StoreError> {
    serde_json::to_string(value)
        .map_err(|e| StoreError::Backend(format!("cannot encode {what}: {e}")))
}

/// Deserializes a stored JSON record.
pub(super) fn from_json<T: DeserializeOwned>(text: &str, what: &str) -> Result<T, StoreError> {
    serde_json::from_str(text).map_err(|e| StoreError::Corrupt(format!("{what}: {e}")))
}

/// Loads the JSON record of `key`; `sql` selects one `data` column with the key as `?1`.
pub(super) fn load_json<T: DeserializeOwned>(
    conn: &Connection,
    sql: &str,
    key: &str,
    what: &str,
) -> Result<Option<T>, StoreError> {
    let data: Option<String> = conn
        .prepare_cached(sql)
        .and_then(|mut stmt| stmt.query_row(params![key], |row| row.get(0)).optional())
        .map_err(db)?;
    data.map(|text| from_json(&text, what)).transpose()
}

/// Inserts or replaces the JSON record of `key`; `sql` takes the key as `?1`
/// and the JSON text as `?2`.
pub(super) fn save_json<T: Serialize>(
    conn: &Connection,
    sql: &str,
    key: &str,
    value: &T,
    what: &str,
) -> Result<(), StoreError> {
    let data = to_json(value, what)?;
    conn.prepare_cached(sql)
        .and_then(|mut stmt| stmt.execute(params![key, data]))
        .map_err(db)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_ids_round_trip_losslessly() {
        for raw in [
            0,
            1,
            u64::MAX,
            u64::MAX / 2,
            u64::MAX / 2 + 1,
            0xcbf2_9ce4_8422_2325,
        ] {
            assert_eq!(repo_from_sql(repo_to_sql(RepoId(raw))), RepoId(raw));
        }
    }

    #[test]
    fn counters_are_range_checked() {
        assert!(matches!(u64_to_sql(u64::MAX), Err(StoreError::Backend(_))));
        assert_eq!(u64_to_sql(42), Ok(42));
        assert!(matches!(u64_from_sql(-1, "x"), Err(StoreError::Corrupt(_))));
        assert!(matches!(
            u32_from_sql(i64::from(u32::MAX) + 1, "x"),
            Err(StoreError::Corrupt(_))
        ));
        assert_eq!(usize_to_sql(usize::MAX), i64::MAX);
    }
}
