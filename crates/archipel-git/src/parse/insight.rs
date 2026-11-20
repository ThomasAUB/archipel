//! Parsers for the repository statistics: the history walk
//! (`git log -z --numstat` with parents and author), `git count-objects -v`
//! and `git cat-file --batch-check` lines.

use archipel_model::diff::NumStat;
use archipel_model::stats::{CommitRecord, RepoStorage};
use archipel_model::{GitError, Oid, Timestamp};

/// `--format` matching [`parse_record`]: each commit starts with `0x1E`,
/// then `hash\0parents\0name\0email\0author time\0commit time\0subject\0`,
/// then (after an optional `\n`) NUL-terminated `added\tremoved\tpath`
/// entries.
/// Author name and email go through `.mailmap` (`%aN`, `%aE`).
pub const RECORD_FORMAT: &str = "--format=%x1e%H%x00%P%x00%aN%x00%aE%x00%at%x00%ct%x00%s";

/// Parses one commit record (the bytes between two `0x1E` separators).
pub fn parse_record(record: &str) -> Result<CommitRecord, GitError> {
    let err = || GitError::Parse {
        what: "git log --numstat",
        detail: record.chars().take(120).collect(),
    };
    let mut fields = record.split('\0');
    let mut next = || fields.next().ok_or_else(err);
    let oid = Oid::from_hex(next()?).map_err(|_| err())?;
    let parents = next()?
        .split_ascii_whitespace()
        .map(|p| Oid::from_hex(p).map_err(|_| err()))
        .collect::<Result<Vec<_>, _>>()?;
    let author_name = next()?.to_owned();
    let author_email = next()?.to_lowercase();
    let author_time = Timestamp(next()?.parse().map_err(|_| err())?);
    let commit_time = Timestamp(next()?.parse().map_err(|_| err())?);
    let subject = next()?.to_owned();
    let mut files = Vec::new();
    for entry in fields {
        let entry = entry.trim_start_matches('\n');
        if entry.is_empty() {
            continue;
        }
        let mut parts = entry.splitn(3, '\t');
        let (Some(added), Some(removed), Some(path)) = (parts.next(), parts.next(), parts.next())
        else {
            return Err(err());
        };
        files.push(NumStat {
            path: path.to_owned(),
            added: added.parse().ok(),
            removed: removed.parse().ok(),
        });
    }
    Ok(CommitRecord {
        oid,
        parents,
        author_name,
        author_email,
        author_time,
        commit_time,
        subject,
        files,
    })
}

/// Reads the counters of `git count-objects -v` into `storage` (sizes are
/// printed in KiB).
pub fn parse_count_objects(text: &str, storage: &mut RepoStorage) {
    for line in text.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let Ok(value) = value.trim().parse::<u64>() else {
            continue;
        };
        match key.trim() {
            "count" => storage.loose_objects = value,
            "size" => storage.loose_bytes = value * 1024,
            "in-pack" => storage.packed_objects = value,
            "packs" => storage.packs = value,
            "size-pack" => storage.pack_bytes = value * 1024,
            _ => {}
        }
    }
}

/// Parses a `%(objecttype) %(objectname) %(objectsize)` line of
/// `git cat-file --batch-check`: the blob and its size, `None` for other
/// object types.
#[must_use]
pub fn parse_blob_size(line: &str) -> Option<(Oid, u64)> {
    let mut parts = line.split_ascii_whitespace();
    if parts.next()? != "blob" {
        return None;
    }
    let oid = Oid::from_hex(parts.next()?).ok()?;
    let size = parts.next()?.parse().ok()?;
    Some((oid, size))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_records_with_parents_and_files() {
        let rec = "f96d1449d86eeafda83db9c78c986adc8094f6d8\0\
                   c1212110207393d43e0800b83e4ed25e4aeab0df c1212110207393d43e0800b83e4ed25e4aeab0d0\0\
                   Ann Lee\0Ann@X.org\x00100\x00200\0Merge branch 'feat'\0\n1\t0\ta.rs\0-\t-\tb.bin\0";
        let c = parse_record(rec).unwrap();
        assert_eq!(c.parents.len(), 2);
        assert_eq!(c.author_name, "Ann Lee");
        assert_eq!(c.author_email, "ann@x.org");
        assert_eq!(c.author_time, Timestamp(100));
        assert_eq!(c.commit_time, Timestamp(200));
        assert_eq!(c.subject, "Merge branch 'feat'");
        assert_eq!(c.files.len(), 2);
        assert_eq!(c.files[1].added, None);

        let root = "f96d1449d86eeafda83db9c78c986adc8094f6d8\0\0A\0a@b\x001\x001\0init\0";
        let c = parse_record(root).unwrap();
        assert!(c.parents.is_empty());
        assert!(c.files.is_empty());
    }

    #[test]
    fn rejects_bad_records() {
        assert!(parse_record("nothex\0\0a\0b\x001\x001\0s\0").is_err());
        assert!(parse_record("f96d1449d86eeafda83db9c78c986adc8094f6d8\0").is_err());
    }

    #[test]
    fn parses_count_objects_and_blob_sizes() {
        let mut s = RepoStorage::default();
        parse_count_objects(
            "count: 12\nsize: 48\nin-pack: 3000\npacks: 2\nsize-pack: 1024\nprune-packable: 0\n",
            &mut s,
        );
        assert_eq!(s.loose_objects, 12);
        assert_eq!(s.loose_bytes, 48 * 1024);
        assert_eq!(s.packed_objects, 3000);
        assert_eq!(s.packs, 2);
        assert_eq!(s.pack_bytes, 1024 * 1024);
        let (_, size) =
            parse_blob_size("blob f96d1449d86eeafda83db9c78c986adc8094f6d8 4096").unwrap();
        assert_eq!(size, 4096);
        assert!(parse_blob_size("tree f96d1449d86eeafda83db9c78c986adc8094f6d8 40").is_none());
    }
}
