//! Parser of `git log -z --numstat --no-renames --format=%x1e%H%x00%ae%x00%at`.
//!
//! Layout: each commit starts with `0x1E`, followed by `hash\0email\0time\0`,
//! then (after an optional `\n`) NUL-terminated `added\tremoved\tpath` entries.
//! Binary files report `-` for both counts.

use archipel_model::diff::NumStat;
use archipel_model::ports::CommitChurn;
use archipel_model::{GitError, Oid, Timestamp};

/// `--format` matching this parser.
pub const CHURN_FORMAT: &str = "--format=%x1e%H%x00%ae%x00%at";

/// Parses one commit record (the bytes between two `0x1E` separators).
pub fn parse_churn_record(record: &str) -> Result<CommitChurn, GitError> {
    let err = || GitError::Parse {
        what: "git log --numstat",
        detail: record.chars().take(120).collect(),
    };
    let mut fields = record.split('\0');
    let oid = Oid::from_hex(fields.next().ok_or_else(err)?).map_err(|_| err())?;
    let author_email = fields.next().ok_or_else(err)?.to_lowercase();
    let author_time = Timestamp(fields.next().ok_or_else(err)?.parse().map_err(|_| err())?);
    let mut files = Vec::new();
    for entry in fields {
        let entry = entry.trim_start_matches('\n');
        if entry.is_empty() {
            continue;
        }
        let mut parts = entry.splitn(3, '\t');
        let added = parts.next().ok_or_else(err)?;
        let removed = parts.next().ok_or_else(err)?;
        let path = parts.next().ok_or_else(err)?;
        files.push(NumStat {
            path: path.to_owned(),
            added: added.parse().ok(),
            removed: removed.parse().ok(),
        });
    }
    Ok(CommitChurn {
        oid,
        author_email,
        author_time,
        files,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_records_with_and_without_files() {
        let rec = "f96d1449d86eeafda83db9c78c986adc8094f6d8\0A@B\x001793785327\0\n1\t0\tb.bin\0-\t-\tbin.dat\x001\t0\tx y.txt\0";
        let c = parse_churn_record(rec).unwrap();
        assert_eq!(c.author_email, "a@b");
        assert_eq!(c.author_time, Timestamp(1_793_785_327));
        assert_eq!(c.files.len(), 3);
        assert_eq!(c.files[1].added, None);
        assert_eq!(c.files[2].path, "x y.txt");

        let merge = "c1212110207 93d43e0800b83e4ed25e4aeab0df9\0a@b\x001\0".replace(' ', "");
        assert!(parse_churn_record(&merge).unwrap().files.is_empty());
    }

    #[test]
    fn rejects_bad_header() {
        assert!(parse_churn_record("nothex\0a\x001\0").is_err());
    }
}
