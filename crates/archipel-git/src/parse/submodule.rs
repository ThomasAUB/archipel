//! Parser of `git submodule status` (spec §9).

use archipel_model::{GitError, Oid};

/// State flag printed before each `git submodule status` line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubmoduleFlag {
    /// ` `: checked out at the commit the parent pins.
    InSync,
    /// `-`: not initialized.
    Uninitialized,
    /// `+`: checked out at another commit than the one pinned.
    OutOfSync,
    /// `U`: merge conflicts on the gitlink.
    Conflict,
}

/// One line of `git submodule status`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubmoduleStatusLine {
    /// State flag.
    pub flag: SubmoduleFlag,
    /// Commit printed: the checked-out one, or the pinned one when not
    /// initialized; `None` for the all-zero id of a conflict.
    pub oid: Option<Oid>,
    /// Path relative to the parent working tree.
    pub path: String,
}

/// Parses the output of `git submodule status` (no `--recursive`).
///
/// Line format: `<flag><40 or 64 hex> <path>[ (<describe>)]`.
pub fn parse_submodule_status(text: &str) -> Result<Vec<SubmoduleStatusLine>, GitError> {
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(parse_line)
        .collect()
}

fn parse_line(line: &str) -> Result<SubmoduleStatusLine, GitError> {
    let bad = || GitError::Parse {
        what: "submodule status",
        detail: line.to_owned(),
    };
    let mut chars = line.chars();
    let flag = match chars.next().ok_or_else(bad)? {
        ' ' => SubmoduleFlag::InSync,
        '-' => SubmoduleFlag::Uninitialized,
        '+' => SubmoduleFlag::OutOfSync,
        'U' => SubmoduleFlag::Conflict,
        _ => return Err(bad()),
    };
    let rest = chars.as_str();
    let (hex, rest) = rest.split_once(' ').ok_or_else(bad)?;
    let oid = if hex.bytes().all(|b| b == b'0') {
        None
    } else {
        Some(Oid::from_hex(hex).map_err(|_| bad())?)
    };
    // A trailing ` (describe)` is optional; paths may contain spaces.
    let path = match rest.rfind(" (") {
        Some(i) if rest.ends_with(')') => &rest[..i],
        _ => rest,
    };
    if path.is_empty() {
        return Err(bad());
    }
    Ok(SubmoduleStatusLine {
        flag,
        oid,
        path: super::unquote_path(path),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "1111111111111111111111111111111111111111";
    const B: &str = "2222222222222222222222222222222222222222";

    #[test]
    fn parses_every_flag() {
        let text = format!(
            " {A} libs/foo (v1.2-3-gabc)\n-{B} vendor/bar\n+{A} with space (heads/main)\nU0000000000000000000000000000000000000000 conflicted\n"
        );
        let lines = parse_submodule_status(&text).unwrap();
        assert_eq!(lines.len(), 4);
        assert_eq!(lines[0].flag, SubmoduleFlag::InSync);
        assert_eq!(lines[0].path, "libs/foo");
        assert_eq!(lines[0].oid, Some(Oid::from_hex(A).unwrap()));
        assert_eq!(lines[1].flag, SubmoduleFlag::Uninitialized);
        assert_eq!(lines[1].path, "vendor/bar");
        assert_eq!(lines[2].flag, SubmoduleFlag::OutOfSync);
        assert_eq!(lines[2].path, "with space");
        assert_eq!(lines[3].flag, SubmoduleFlag::Conflict);
        assert_eq!(lines[3].oid, None);
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_submodule_status("?nope").is_err());
        assert!(parse_submodule_status(" 12zz path").is_err());
        assert!(parse_submodule_status("").unwrap().is_empty());
    }
}
