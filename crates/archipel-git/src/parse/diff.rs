//! Parser of unified diffs (`git diff`, `git show`) into [`FileDiff`]s.
//!
//! Expects the `a/` and `b/` prefixes (the CLI adapter forces them with
//! `--src-prefix`/`--dst-prefix`, whatever the user's `diff.noprefix`).

use archipel_model::changes::ChangeKind;
use archipel_model::diff::{DiffLine, FileDiff, Hunk, LineOrigin};

use super::unquote_path;

/// Parses a whole unified diff. Files whose hunks exceed `max_file_bytes`
/// are returned with `truncated = true` and no hunks.
#[must_use]
pub fn parse_unified_diff(text: &str, max_file_bytes: usize) -> Vec<FileDiff> {
    let mut files = Vec::new();
    let mut current: Option<(FileDiff, usize)> = None;
    let mut old_line = 0u32;
    let mut new_line = 0u32;
    // Split on `\n` only: a `\r` before it belongs to the content (CRLF
    // files), and patches built from the diff must reproduce it exactly.
    for line in text.split_terminator('\n') {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            if let Some((file, _)) = current.take() {
                files.push(file);
            }
            current = Some((new_file(rest), 0));
            continue;
        }
        let Some((file, size)) = current.as_mut() else {
            continue;
        };
        if file.hunks.is_empty() && !file.truncated && parse_header_line(file, line) {
            continue;
        }
        if let Some(hunk) = parse_hunk_header(line) {
            old_line = hunk.old_start;
            new_line = hunk.new_start;
            if !file.truncated {
                file.hunks.push(hunk);
            }
            continue;
        }
        *size += line.len() + 1;
        if *size > max_file_bytes && !file.truncated {
            file.truncated = true;
            file.hunks.clear();
        }
        let (origin, content) = match line.as_bytes().first() {
            Some(b'+') => (LineOrigin::Added, &line[1..]),
            Some(b'-') => (LineOrigin::Removed, &line[1..]),
            Some(b' ') => (LineOrigin::Context, &line[1..]),
            Some(b'\\') => {
                if let Some(last) = file.hunks.last_mut().and_then(|h| h.lines.last_mut()) {
                    last.no_newline_at_eof = true;
                }
                continue;
            }
            None => (LineOrigin::Context, ""),
            _ => continue,
        };
        let (old_lineno, new_lineno) = match origin {
            LineOrigin::Added => (None, Some(new_line)),
            LineOrigin::Removed => (Some(old_line), None),
            LineOrigin::Context => (Some(old_line), Some(new_line)),
        };
        if origin != LineOrigin::Added {
            old_line += 1;
        }
        if origin != LineOrigin::Removed {
            new_line += 1;
        }
        if file.truncated {
            continue;
        }
        if let Some(hunk) = file.hunks.last_mut() {
            hunk.lines.push(DiffLine {
                origin,
                content: content.to_owned(),
                old_lineno,
                new_lineno,
                no_newline_at_eof: false,
            });
        }
    }
    if let Some((file, _)) = current {
        files.push(file);
    }
    files
}

fn new_file(header: &str) -> FileDiff {
    let (old, new) = split_git_header(header);
    FileDiff {
        old_path: old,
        new_path: new,
        kind: ChangeKind::Modified,
        binary: false,
        hunks: Vec::new(),
        truncated: false,
    }
}

/// Splits `a/x b/y` (paths may be quoted or contain spaces).
fn split_git_header(rest: &str) -> (Option<String>, Option<String>) {
    if let Some(quoted) = rest.strip_prefix('"') {
        if let Some(end) = quoted.find("\" ").map(|i| i + 2) {
            let a = unquote_path(&rest[..end]);
            let b = unquote_path(rest[end + 1..].trim());
            return (strip(&a, "a/"), strip(&b, "b/"));
        }
    }
    // Unquoted: when old and new paths are equal, the header is symmetric.
    let n = rest.len().saturating_sub(1) / 2;
    if rest.is_char_boundary(n) && rest.len() % 2 == 1 {
        let (a, b) = (&rest[..n], &rest[n + 1..]);
        if a.strip_prefix("a/")
            .is_some_and(|x| b.strip_prefix("b/") == Some(x))
        {
            return (strip(a, "a/"), strip(b, "b/"));
        }
    }
    match rest.find(" b/") {
        Some(i) => (strip(&rest[..i], "a/"), strip(&rest[i + 1..], "b/")),
        None => (None, None),
    }
}

fn strip(path: &str, prefix: &str) -> Option<String> {
    #[allow(clippy::unnecessary_wraps)] // callers store it in `Option` fields
    let stripped = Some(path.strip_prefix(prefix).unwrap_or(path).to_owned());
    stripped
}

/// Handles extended header lines; returns `true` when the line was one.
fn parse_header_line(file: &mut FileDiff, line: &str) -> bool {
    if line.starts_with("new file mode") {
        file.kind = ChangeKind::Added;
        file.old_path = None;
    } else if line.starts_with("deleted file mode") {
        file.kind = ChangeKind::Deleted;
        file.new_path = None;
    } else if let Some(p) = line.strip_prefix("rename from ") {
        file.kind = ChangeKind::Renamed;
        file.old_path = Some(unquote_path(p));
    } else if let Some(p) = line.strip_prefix("rename to ") {
        file.new_path = Some(unquote_path(p));
    } else if let Some(p) = line.strip_prefix("copy from ") {
        file.kind = ChangeKind::Copied;
        file.old_path = Some(unquote_path(p));
    } else if let Some(p) = line.strip_prefix("copy to ") {
        file.new_path = Some(unquote_path(p));
    } else if line.starts_with("Binary files ") || line == "GIT binary patch" {
        file.binary = true;
    } else if let Some(p) = line.strip_prefix("--- ") {
        if p != "/dev/null" && file.kind != ChangeKind::Added {
            file.old_path = strip(&unquote_path(p.trim_end_matches('\t')), "a/");
        }
    } else if let Some(p) = line.strip_prefix("+++ ") {
        if p != "/dev/null" && file.kind != ChangeKind::Deleted {
            file.new_path = strip(&unquote_path(p.trim_end_matches('\t')), "b/");
        }
    } else {
        return line.starts_with("index ")
            || line.starts_with("old mode")
            || line.starts_with("new mode")
            || line.starts_with("similarity index")
            || line.starts_with("dissimilarity index");
    }
    true
}

/// Parses `@@ -a,b +c,d @@ header`.
fn parse_hunk_header(line: &str) -> Option<Hunk> {
    let rest = line.strip_prefix("@@ -")?;
    let (ranges, header) = rest.split_once(" @@")?;
    let (old, new) = ranges.split_once(" +")?;
    let (old_start, old_lines) = parse_range(old)?;
    let (new_start, new_lines) = parse_range(new)?;
    Some(Hunk {
        old_start,
        old_lines,
        new_start,
        new_lines,
        header: header.trim_start().to_owned(),
        lines: Vec::new(),
    })
}

fn parse_range(range: &str) -> Option<(u32, u32)> {
    match range.split_once(',') {
        Some((start, len)) => Some((start.parse().ok()?, len.parse().ok()?)),
        None => Some((range.parse().ok()?, 1)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIFF: &str = "\
diff --git a/src/my file.rs b/src/my file.rs
index 1111111..2222222 100644
--- a/src/my file.rs
+++ b/src/my file.rs
@@ -1,3 +1,4 @@ fn main() {
 one
-two
+deux
+zwei
 three
diff --git a/new.txt b/new.txt
new file mode 100644
index 0000000..3333333
--- /dev/null
+++ b/new.txt
@@ -0,0 +1 @@
+hello
\\ No newline at end of file
diff --git a/old.txt b/renamed.txt
similarity index 100%
rename from old.txt
rename to renamed.txt
diff --git a/img.png b/img.png
index 4444444..5555555 100644
Binary files a/img.png and b/img.png differ
diff --git a/gone.txt b/gone.txt
deleted file mode 100644
index 6666666..0000000
--- a/gone.txt
+++ /dev/null
@@ -1 +0,0 @@
-bye
";

    #[test]
    fn parses_files_and_kinds() {
        let files = parse_unified_diff(DIFF, usize::MAX);
        assert_eq!(files.len(), 5);
        assert_eq!(files[0].display_path(), "src/my file.rs");
        assert_eq!(files[0].kind, ChangeKind::Modified);
        assert_eq!(files[1].kind, ChangeKind::Added);
        assert_eq!(files[1].old_path, None);
        assert_eq!(files[2].kind, ChangeKind::Renamed);
        assert_eq!(files[2].old_path.as_deref(), Some("old.txt"));
        assert_eq!(files[2].new_path.as_deref(), Some("renamed.txt"));
        assert!(files[3].binary);
        assert_eq!(files[4].kind, ChangeKind::Deleted);
        assert_eq!(files[4].new_path, None);
    }

    #[test]
    fn parses_hunks_and_line_numbers() {
        let files = parse_unified_diff(DIFF, usize::MAX);
        let hunk = &files[0].hunks[0];
        assert_eq!(
            (
                hunk.old_start,
                hunk.old_lines,
                hunk.new_start,
                hunk.new_lines
            ),
            (1, 3, 1, 4)
        );
        assert_eq!(hunk.header, "fn main() {");
        let numbers: Vec<_> = hunk
            .lines
            .iter()
            .map(|l| (l.old_lineno, l.new_lineno))
            .collect();
        assert_eq!(
            numbers,
            [
                (Some(1), Some(1)),
                (Some(2), None),
                (None, Some(2)),
                (None, Some(3)),
                (Some(3), Some(4))
            ]
        );
        assert_eq!(files[0].line_stats(), (2, 1));
        assert!(files[1].hunks[0].lines[0].no_newline_at_eof);
    }

    #[test]
    fn truncates_large_files() {
        let files = parse_unified_diff(DIFF, 10);
        assert!(files[0].truncated);
        assert!(files[0].hunks.is_empty());
    }

    #[test]
    fn keeps_carriage_returns() {
        let diff =
            "diff --git a/w.txt b/w.txt\n--- a/w.txt\n+++ b/w.txt\n@@ -1 +1 @@\n-a\r\n+b\r\n";
        let files = parse_unified_diff(diff, usize::MAX);
        assert_eq!(files[0].hunks[0].lines[1].content, "b\r");
    }

    #[test]
    fn splits_asymmetric_header() {
        assert_eq!(
            split_git_header("a/x y b/z"),
            (Some("x y".to_owned()), Some("z".to_owned()))
        );
        assert_eq!(
            split_git_header(r#""a/t\tab" "b/t\tab""#),
            (Some("t\tab".to_owned()), Some("t\tab".to_owned()))
        );
    }
}
