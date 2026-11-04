//! Typed errors and non-fatal warnings.

use std::path::PathBuf;

/// A fatal error from a strict API (parsing a file the user explicitly
/// imports, reading the themes directory).
#[derive(Debug, thiserror::Error)]
pub enum ThemeError {
    /// The text is not valid TOML (or not a table at the top level).
    #[error("invalid theme file: {message}")]
    Parse {
        /// Parser message.
        message: String,
    },
    /// Reading a file or directory failed.
    #[error("cannot read {}: {source}", path.display())]
    Io {
        /// Path that could not be read.
        path: PathBuf,
        /// Underlying error.
        #[source]
        source: std::io::Error,
    },
    /// The id is reserved by a built-in theme.
    #[error("theme id `{0}` is reserved by a built-in theme")]
    ReservedId(String),
}

/// A non-fatal problem found while loading a theme. The theme is still
/// produced, using the parent (or built-in) value for whatever was wrong.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ThemeWarning {
    /// A `[tokens]` key that is not a known token id.
    #[error("unknown token `{key}` ignored")]
    UnknownToken {
        /// The offending key.
        key: String,
    },
    /// A key outside `[tokens]` that is not part of the format.
    #[error("unknown key `{key}` ignored")]
    UnknownKey {
        /// The offending key (dotted path).
        key: String,
    },
    /// A known key with a value of the wrong type or out of range.
    #[error("invalid value for `{key}`: {reason}; using the parent value")]
    InvalidValue {
        /// The offending key (dotted path).
        key: String,
        /// What is wrong with it.
        reason: String,
    },
    /// `extends` names a theme that does not exist.
    #[error("parent theme `{parent}` not found; using `{fallback}`")]
    UnknownParent {
        /// The missing parent id.
        parent: String,
        /// Theme used instead.
        fallback: String,
    },
    /// `extends` forms a cycle.
    #[error("`extends` cycle {chain}; using `{fallback}`")]
    ExtendsCycle {
        /// The cycle, e.g. `a -> b -> a`.
        chain: String,
        /// Theme used instead of the cyclic parent.
        fallback: String,
    },
    /// The file is not valid TOML; the theme falls back entirely.
    #[error("theme file could not be parsed ({message}); using `{fallback}`")]
    Unparsable {
        /// Parser message.
        message: String,
        /// Theme used instead.
        fallback: String,
    },
    /// The file could not be read; the theme falls back entirely.
    #[error("theme file could not be read ({message}); using `{fallback}`")]
    Unreadable {
        /// I/O error message.
        message: String,
        /// Theme used instead.
        fallback: String,
    },
    /// A user file uses a built-in theme's id; it is ignored.
    #[error("theme id `{id}` is reserved by a built-in theme; file ignored")]
    ReservedId {
        /// The reserved id.
        id: String,
    },
}
