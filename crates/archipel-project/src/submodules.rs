//! Submodule detection from a repository's `.gitmodules` file.

use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::Path;

use archipel_model::error::ProjectError;
use archipel_model::ids::RepoPath;

use crate::paths;

/// Name of the file declaring submodules.
const GITMODULES: &str = ".gitmodules";

/// Key holding a submodule's path inside its section.
const PATH_KEY: &str = "path";

/// Characters starting a comment in Git config files.
const COMMENT_CHARS: [char; 2] = ['#', ';'];

/// Absolute paths of the submodules declared in `repo_root/.gitmodules`.
///
/// Returns an empty list when the file does not exist. Paths are lexically
/// normalized, deduplicated and returned in declaration order; they are not
/// required to exist (an uninitialized submodule is still listed).
pub fn submodule_paths(repo_root: &Path) -> Result<Vec<RepoPath>, ProjectError> {
    let file = repo_root.join(GITMODULES);
    let text = match fs::read_to_string(&file) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => {
            return Err(ProjectError::Io {
                path: file,
                message: e.to_string(),
            });
        }
    };
    let mut seen = HashSet::new();
    Ok(parse_paths(&text)
        .into_iter()
        .map(|rel| paths::resolve(repo_root, &rel))
        .filter(|path| seen.insert(paths::dedup_key(path)))
        .map(RepoPath::new)
        .collect())
}

/// Extracts the `path = …` values of `[submodule "…"]` sections.
fn parse_paths(text: &str) -> Vec<String> {
    let mut in_submodule = false;
    let mut out = Vec::new();
    for line in text.lines().map(str::trim) {
        if line.is_empty() || line.starts_with(COMMENT_CHARS) {
            continue;
        }
        if let Some(header) = line.strip_prefix('[') {
            in_submodule = header
                .split(|c: char| c.is_whitespace() || c == '"' || c == ']')
                .next()
                .is_some_and(|section| section.eq_ignore_ascii_case("submodule"));
            continue;
        }
        if !in_submodule {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key.trim().eq_ignore_ascii_case(PATH_KEY) {
            let value = unquote(strip_comment(value.trim()));
            if !value.is_empty() {
                out.push(value.to_owned());
            }
        }
    }
    out
}

/// Removes a trailing comment outside of quotes.
fn strip_comment(value: &str) -> &str {
    let mut quoted = false;
    for (i, c) in value.char_indices() {
        match c {
            '"' => quoted = !quoted,
            c if !quoted && COMMENT_CHARS.contains(&c) => return value[..i].trim_end(),
            _ => {}
        }
    }
    value
}

fn unquote(value: &str) -> &str {
    value
        .strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .unwrap_or(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_paths_of_submodule_sections() {
        let text = r#"
# comment
[submodule "libfoo"]
	path = libs/foo
	url = https://example.com/foo.git
[submodule "bar"]
	url = ../bar.git
	path = "vendor/bar" ; trailing comment
[core]
	path = not-a-submodule
[Submodule "baz"]
	PATH = baz
"#;
        assert_eq!(parse_paths(text), ["libs/foo", "vendor/bar", "baz"]);
    }

    #[test]
    fn ignores_malformed_lines() {
        assert!(parse_paths("[submodule \"x\"]\npath\nurl = y\n").is_empty());
    }
}
