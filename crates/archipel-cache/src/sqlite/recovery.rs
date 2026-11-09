//! Recovery from a damaged database file (power loss, disk error, a file
//! copied while it was written…): the file is moved aside and what can still
//! be read of the recent-projects list is carried over to the fresh one.

use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use archipel_model::StoreError;
use rusqlite::{Connection, OpenFlags, params};

/// Suffix of the damaged file kept for inspection (the previous one is replaced).
const ASIDE_SUFFIX: &str = ".corrupt";
/// SQLite's companion files, which follow the database file.
const COMPANIONS: &[&str] = &["-wal", "-shm"];

/// `path` with `suffix` appended to its file name.
fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = OsString::from(path.as_os_str());
    name.push(suffix);
    PathBuf::from(name)
}

/// Moves the database at `path` and its WAL files to `<path>.corrupt`
/// (replacing an older one). Returns the new path of the database.
pub(super) fn move_aside(path: &Path) -> Result<PathBuf, StoreError> {
    let aside = with_suffix(path, ASIDE_SUFFIX);
    let fail = |e: io::Error| {
        StoreError::Backend(format!(
            "cannot move the damaged {} aside: {e}",
            path.display()
        ))
    };
    for suffix in std::iter::once("").chain(COMPANIONS.iter().copied()) {
        let target = with_suffix(&aside, suffix);
        match fs::remove_file(&target) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(fail(e)),
            _ => {}
        }
        match fs::rename(with_suffix(path, suffix), &target) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(fail(e)),
            _ => {}
        }
    }
    Ok(aside)
}

/// Copies the readable, well-formed rows of the damaged database's
/// `registry` into `conn`. Best effort: reading stops at the first damaged
/// page, and nothing is reported beyond a log line.
pub(super) fn salvage_registry(conn: &Connection, damaged: &Path) {
    let rows = match read_registry(damaged) {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!(error = %e, "could not salvage the recent projects");
            return;
        }
    };
    let mut kept = 0usize;
    for (id, data) in rows {
        let (Ok(id), Ok(data)) = (String::from_utf8(id), String::from_utf8(data)) else {
            continue;
        };
        if serde_json::from_str::<serde_json::Value>(&data).is_err() {
            continue;
        }
        if conn
            .execute(
                "INSERT OR IGNORE INTO registry (id, data) VALUES (?1, ?2)",
                params![id, data],
            )
            .is_ok()
        {
            kept += 1;
        }
    }
    tracing::info!(kept, "recent projects salvaged from the damaged database");
}

/// The raw `(id, data)` rows of `registry` readable before the first error.
type RawRow = (Vec<u8>, Vec<u8>);

fn read_registry(path: &Path) -> rusqlite::Result<Vec<RawRow>> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let mut stmt = conn.prepare("SELECT CAST(id AS BLOB), CAST(data AS BLOB) FROM registry")?;
    let mut rows = stmt.query([])?;
    let mut out = Vec::new();
    while let Ok(Some(row)) = rows.next() {
        if let (Ok(id), Ok(data)) = (row.get(0), row.get(1)) {
            out.push((id, data));
        }
    }
    Ok(out)
}
