//! What a Ctrl+click in a terminal opens: the URL or the file path under
//! the pointer.

use std::path::{Path, PathBuf};

/// Something clickable in the terminal's text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Link {
    /// A web address.
    Url(String),
    /// A file or folder that exists.
    Path(PathBuf),
}

/// Characters that end a link on either side.
fn is_boundary(c: char) -> bool {
    c.is_whitespace()
        || matches!(
            c,
            '"' | '\'' | '`' | '<' | '>' | '|' | '(' | ')' | '[' | ']' | '{' | '}'
        )
}

/// The word of `line` around column `col` (columns are `char`s).
fn word_at(line: &str, col: usize) -> Option<&str> {
    let chars: Vec<(usize, char)> = line.char_indices().collect();
    let (_, at) = *chars.get(col)?;
    if is_boundary(at) {
        return None;
    }
    let start = chars[..col]
        .iter()
        .rev()
        .find(|(_, c)| is_boundary(*c))
        .map_or(0, |(i, c)| i + c.len_utf8());
    let end = chars[col..]
        .iter()
        .find(|(_, c)| is_boundary(*c))
        .map_or(line.len(), |(i, _)| *i);
    Some(&line[start..end])
}

/// Strips what ends a sentence or a compiler location: trailing
/// punctuation, `:12`, `:12:5`.
fn trim_location(word: &str) -> &str {
    let mut w = word.trim_end_matches(['.', ',', ';', ':', '!', '?']);
    for _ in 0..2 {
        if let Some((head, tail)) = w.rsplit_once(':')
            && !tail.is_empty()
            && tail.chars().all(|c| c.is_ascii_digit())
        {
            w = head;
        }
    }
    w
}

/// The link at column `col` of `line`; relative paths are resolved from
/// `cwd` and kept only when they exist.
#[must_use]
pub fn link_at(line: &str, col: usize, cwd: &Path) -> Option<Link> {
    let word = word_at(line, col)?;
    if let Some(i) = word.find("://") {
        let scheme = &word[..i];
        if matches!(scheme, "http" | "https" | "ftp" | "file") {
            let url = word.trim_end_matches(['.', ',', ';', ':', '!', '?']);
            return Some(Link::Url(url.to_owned()));
        }
        return None;
    }
    let path = trim_location(word);
    let path = path
        .strip_prefix("a/")
        .or_else(|| path.strip_prefix("b/"))
        .filter(|p| cwd.join(p).exists())
        .unwrap_or(path);
    if path.is_empty() {
        return None;
    }
    let full = cwd.join(path);
    full.exists().then_some(Link::Path(full))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_are_found_without_their_punctuation() {
        let line = "see (https://example.com/a?b=1). or not";
        let cwd = Path::new("/");
        assert_eq!(
            link_at(line, 10, cwd),
            Some(Link::Url("https://example.com/a?b=1".into()))
        );
        assert_eq!(link_at(line, 0, cwd), None);
        assert_eq!(link_at(line, 3, cwd), None);
        assert_eq!(link_at("ssh://host/x", 2, cwd), None);
    }

    #[test]
    fn existing_paths_with_locations() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/main.rs"), "").unwrap();
        let cwd = dir.path();
        let want = Some(Link::Path(cwd.join("src/main.rs")));
        assert_eq!(link_at("error at src/main.rs:12:5: oops", 12, cwd), want);
        assert_eq!(link_at("M  src/main.rs", 5, cwd), want);
        assert_eq!(link_at("--- a/src/main.rs", 6, cwd), want);
        assert_eq!(link_at("src/gone.rs", 2, cwd), None);
        assert_eq!(link_at("é src/main.rs", 3, cwd), want);
    }
}
