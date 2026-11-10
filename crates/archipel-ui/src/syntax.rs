//! Syntax highlighting of diff lines, in the theme's syntax colors.
//!
//! A diff shows pieces of a file, not the file, so lines are highlighted one
//! after the other with the parser's state carried from line to line: the
//! old side (context and removed lines) and the new side (context and added
//! lines) each keep their own, so a string or comment opened on one side does
//! not spill onto the other.
//!
//! Syntect names what it matched with `TextMate` scopes
//! (`keyword.control.rust`, `string.quoted.double`); those map onto the
//! handful of syntax tokens the theme defines.

use std::collections::HashMap;
use std::ops::Range;
use std::sync::LazyLock;

use archipel_theme::ColorToken as T;
use syntect::parsing::{ParseState, Scope, ScopeStack, SyntaxReference, SyntaxSet};

/// Colored byte ranges of one line, ordered and non-overlapping.
pub(crate) type Spans = Vec<(Range<usize>, T)>;

/// Deserializing the syntax set takes tens of milliseconds, so it happens
/// once, on the first highlight.
static SYNTAXES: LazyLock<SyntaxSet> = LazyLock::new(two_face::syntax::extra_newlines);

/// Lines longer than this are left plain: they are rarely code worth
/// reading in color (minified files, data) and slow to parse.
const MAX_LINE_BYTES: usize = 2000;

/// The syntax of `file`, by its extension or its name (`Makefile`); `None`
/// for a file shown as plain text.
pub(crate) fn find(file: &str) -> Option<&'static SyntaxReference> {
    let syntaxes = &*SYNTAXES;
    let name = file.rsplit(['/', '\\']).next().unwrap_or(file);
    let ext = name.rsplit_once('.').map(|(_, ext)| ext);
    ext.into_iter()
        .chain([name])
        .flat_map(|token| [token.to_owned(), token.to_lowercase()])
        .find_map(|token| syntaxes.find_syntax_by_token(&token))
        .filter(|syntax| syntax.name != "Plain Text")
}

/// Highlights consecutive lines of one file.
pub(crate) struct LineHighlighter {
    parse: ParseState,
    scopes: ScopeStack,
    /// Token of each scope met so far (`None`: let an enclosing scope
    /// decide).
    tokens: HashMap<Scope, Option<T>>,
}

impl LineHighlighter {
    pub(crate) fn new(syntax: &SyntaxReference) -> Self {
        Self {
            parse: ParseState::new(syntax),
            scopes: ScopeStack::new(),
            tokens: HashMap::new(),
        }
    }

    /// The spans of `text`, the next line of the file (without its line
    /// ending).
    pub(crate) fn line(&mut self, text: &str) -> Spans {
        let mut spans = Spans::new();
        if text.len() > MAX_LINE_BYTES {
            return spans;
        }
        // The syntaxes expect each line with its newline.
        let line = format!("{text}\n");
        // A line syntect cannot parse keeps the scopes it started with.
        let Ok(operations) = self.parse.parse_line(&line, &SYNTAXES) else {
            return spans;
        };
        let mut cursor = 0;
        for (index, operation) in operations {
            self.push(cursor..index.min(text.len()), &mut spans);
            let _ = self.scopes.apply(&operation);
            cursor = index.min(text.len());
        }
        self.push(cursor..text.len(), &mut spans);
        spans
    }

    /// Colors `range` with the innermost scope that names a token, merged
    /// with the previous span when they touch and share it.
    fn push(&mut self, range: Range<usize>, spans: &mut Spans) {
        if range.is_empty() {
            return;
        }
        let tokens = &mut self.tokens;
        let token = self
            .scopes
            .scopes
            .iter()
            .rev()
            .find_map(|scope| *tokens.entry(*scope).or_insert_with(|| token_of(*scope)));
        let Some(token) = token else {
            return;
        };
        match spans.last_mut() {
            Some((last, t)) if last.end == range.start && *t == token => last.end = range.end,
            _ => spans.push((range, token)),
        }
    }
}

/// The theme token for a `TextMate` scope, or `None` to let an enclosing
/// scope decide.
fn token_of(scope: Scope) -> Option<T> {
    let scope = scope.build_string();
    let is = |prefix: &str| {
        scope == prefix
            || scope
                .strip_prefix(prefix)
                .is_some_and(|rest| rest.starts_with('.'))
    };
    // Delimiters belong to what they delimit (a string's quotes color as
    // the string); the rest are containers with no color of their own.
    if is("punctuation.definition.string")
        || is("punctuation.definition.comment")
        || is("invalid")
        || is("meta")
        || is("source")
        || is("text")
    {
        return None;
    }
    let token = if is("comment") {
        T::SyntaxComment
    } else if is("string") || is("constant.character.escape") {
        T::SyntaxString
    } else if is("constant.numeric") {
        T::SyntaxNumber
    } else if is("constant") || is("support.constant") || is("entity.name.constant") {
        T::SyntaxConstant
    } else if is("keyword.operator") {
        T::SyntaxOperator
    } else if is("keyword") || is("storage.modifier") || is("entity.name.tag") {
        T::SyntaxKeyword
    } else if is("storage.type") {
        // `fn`, `class` and `let` are declarations, `int` and `bool` types.
        const DECLARATIONS: [&str; 13] = [
            "class",
            "enum",
            "function",
            "impl",
            "interface",
            "module",
            "namespace",
            "struct",
            "trait",
            "type",
            "let",
            "var",
            "const",
        ];
        if DECLARATIONS.iter().any(|word| scope.contains(word)) {
            T::SyntaxKeyword
        } else {
            T::SyntaxType
        }
    } else if is("entity.name.function")
        || is("support.function")
        || is("variable.function")
        || is("entity.other.attribute-name")
    {
        T::SyntaxFunction
    } else if is("entity.name")
        || is("entity.other.inherited-class")
        || is("support.type")
        || is("support.class")
    {
        T::SyntaxType
    } else {
        return None;
    };
    Some(token)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens(file: &str, lines: &[&str]) -> Vec<Vec<(String, T)>> {
        let mut h = LineHighlighter::new(find(file).unwrap());
        lines
            .iter()
            .map(|line| {
                h.line(line)
                    .into_iter()
                    .map(|(r, t)| (line[r].to_owned(), t))
                    .collect()
            })
            .collect()
    }

    #[test]
    fn finds_syntaxes_by_extension_or_name() {
        assert!(find("src/main.rs").is_some());
        assert!(find("web/App.TSX").is_some());
        assert!(find("Makefile").is_some());
        assert!(find("notes.txt").is_none());
        assert!(find("no_extension_here").is_none());
    }

    #[test]
    fn colors_keywords_strings_and_comments() {
        let lines = tokens(
            "a.rs",
            &["fn main() {", "    let s = \"hi\"; // greet", "}"],
        );
        assert!(lines[0].contains(&("fn".to_owned(), T::SyntaxKeyword)));
        assert!(lines[0].contains(&("main".to_owned(), T::SyntaxFunction)));
        assert!(lines[1].contains(&("let".to_owned(), T::SyntaxKeyword)));
        assert!(lines[1].contains(&("\"hi\"".to_owned(), T::SyntaxString)));
        assert!(
            lines[1]
                .iter()
                .any(|(s, t)| s.contains("greet") && *t == T::SyntaxComment)
        );
    }

    #[test]
    fn carries_state_across_lines() {
        let lines = tokens("a.c", &["/* open", "still comment */ int x;"]);
        assert_eq!(
            lines[1][0],
            ("still comment */".to_owned(), T::SyntaxComment)
        );
    }

    #[test]
    fn spans_stay_inside_the_line() {
        let mut h = LineHighlighter::new(find("a.py").unwrap());
        for line in ["x = 'é' # ü", "", "def f(): pass"] {
            for (r, _) in h.line(line) {
                assert!(r.end <= line.len() && line.is_char_boundary(r.start));
                assert!(line.is_char_boundary(r.end));
            }
        }
    }
}
