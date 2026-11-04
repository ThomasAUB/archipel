//! [`ThemeRegistry`]: built-in themes plus user themes loaded from a
//! directory (`themes/*.toml`), with `extends` resolution, cycle detection
//! and single-file reload (the app wires file watching to
//! [`ThemeRegistry::reload_file`]).
//!
//! Loading never fails because of a theme's content: a broken file still
//! yields a usable theme (its parent or the default built-in) plus warnings.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::builtin;
use crate::error::{ThemeError, ThemeWarning};
use crate::file::ThemeFile;
use crate::settings::{Appearance, ThemeSelection};
use crate::theme::Theme;

/// Extension of theme files.
pub const THEME_FILE_EXTENSION: &str = "toml";

#[derive(Debug, Clone)]
struct Source {
    path: Option<PathBuf>,
    /// `None` when the file could not be read or parsed.
    file: Option<ThemeFile>,
    warnings: Vec<ThemeWarning>,
}

/// Holds built-in and user themes, resolved.
#[derive(Debug, Clone)]
pub struct ThemeRegistry {
    builtins: Vec<Theme>,
    sources: BTreeMap<String, Source>,
    resolved: BTreeMap<String, Theme>,
    warnings: BTreeMap<String, Vec<ThemeWarning>>,
}

impl Default for ThemeRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ThemeRegistry {
    /// A registry with only the built-in themes.
    #[must_use]
    pub fn new() -> Self {
        Self {
            builtins: builtin::all(),
            sources: BTreeMap::new(),
            resolved: BTreeMap::new(),
            warnings: BTreeMap::new(),
        }
    }

    /// Replaces all file-backed user themes with the `*.toml` files of `dir`
    /// (theme id = file stem). A missing directory means no user themes.
    /// Only failing to list the directory is an error; unreadable or broken
    /// files become fallback themes with warnings.
    pub fn load_dir(&mut self, dir: &Path) -> Result<(), ThemeError> {
        let io_err = |source| ThemeError::Io {
            path: dir.to_path_buf(),
            source,
        };
        self.sources.retain(|_, s| s.path.is_none());
        if !dir.exists() {
            self.resolve_all();
            return Ok(());
        }
        let mut paths = Vec::new();
        for entry in fs::read_dir(dir).map_err(io_err)? {
            let path = entry.map_err(io_err)?.path();
            if path.is_file() && is_theme_file(&path) {
                paths.push(path);
            }
        }
        paths.sort();
        for path in paths {
            self.read_source(&path);
        }
        self.resolve_all();
        Ok(())
    }

    /// Re-reads one theme file (or forgets it if it was deleted) and
    /// re-resolves every theme, since others may extend it. Returns the
    /// theme id the file maps to, if it is a theme file.
    pub fn reload_file(&mut self, path: &Path) -> Option<String> {
        if !is_theme_file(path) {
            return None;
        }
        let id = theme_id_for(path)?;
        if path.exists() {
            self.read_source(path);
        } else {
            self.sources.remove(&id);
        }
        self.resolve_all();
        Some(id)
    }

    /// Adds (or replaces) an in-memory user theme from TOML text, e.g. an
    /// import from the editor. Unlike file loading this is strict about
    /// syntax and reserved ids, so the user gets immediate feedback.
    pub fn import_str(&mut self, id: &str, text: &str) -> Result<&Theme, ThemeError> {
        if builtin::is_builtin(id) {
            return Err(ThemeError::ReservedId(id.to_owned()));
        }
        let (file, warnings) = ThemeFile::parse(text)?;
        self.sources.insert(
            id.to_owned(),
            Source {
                path: None,
                file: Some(file),
                warnings,
            },
        );
        self.resolve_all();
        Ok(self.get_or_default(id))
    }

    /// Removes a user theme (no-op for built-ins). Themes extending it fall
    /// back to the default parent with a warning.
    pub fn remove(&mut self, id: &str) {
        if self.sources.remove(id).is_some() {
            self.resolve_all();
        }
    }

    /// A theme by id.
    #[must_use]
    pub fn get(&self, id: &str) -> Option<&Theme> {
        self.builtins
            .iter()
            .find(|t| t.id == id)
            .or_else(|| self.resolved.get(id))
    }

    /// A theme by id, or the default built-in if it does not exist.
    #[must_use]
    pub fn get_or_default(&self, id: &str) -> &Theme {
        self.get(id).unwrap_or_else(|| self.default_theme())
    }

    /// The theme for a user selection and the current OS appearance.
    #[must_use]
    pub fn select(&self, selection: &ThemeSelection, os: Appearance) -> &Theme {
        self.get_or_default(selection.theme_id(os))
    }

    /// The default built-in theme.
    #[must_use]
    pub fn default_theme(&self) -> &Theme {
        // `builtins` always holds every built-in; index 1 is the fallback
        // only if the lookup somehow fails.
        self.builtins
            .iter()
            .find(|t| t.id == builtin::DEFAULT_ID)
            .unwrap_or(&self.builtins[1])
    }

    /// Built-ins first, then user themes sorted by id.
    pub fn themes(&self) -> impl Iterator<Item = &Theme> {
        self.builtins.iter().chain(self.resolved.values())
    }

    /// Ids of all themes, in [`themes`](Self::themes) order.
    pub fn ids(&self) -> impl Iterator<Item = &str> {
        self.themes().map(|t| t.id.as_str())
    }

    /// Warnings produced while loading/resolving theme `id`.
    #[must_use]
    pub fn warnings(&self, id: &str) -> &[ThemeWarning] {
        self.warnings.get(id).map_or(&[], Vec::as_slice)
    }

    /// All warnings, keyed by theme id.
    #[must_use]
    pub const fn all_warnings(&self) -> &BTreeMap<String, Vec<ThemeWarning>> {
        &self.warnings
    }

    /// Path of the file a user theme was loaded from.
    #[must_use]
    pub fn path_of(&self, id: &str) -> Option<&Path> {
        self.sources.get(id).and_then(|s| s.path.as_deref())
    }

    /// The theme `id` directly extends (for "reset to parent" and export).
    #[must_use]
    pub fn parent_of(&self, id: &str) -> Option<&Theme> {
        self.get(id)?.extends.as_deref().and_then(|p| self.get(p))
    }

    fn read_source(&mut self, path: &Path) {
        let Some(id) = theme_id_for(path) else {
            return;
        };
        let fallback = builtin::DEFAULT_ID.to_owned();
        let source = match fs::read_to_string(path) {
            Err(e) => Source {
                path: Some(path.to_path_buf()),
                file: None,
                warnings: vec![ThemeWarning::Unreadable {
                    message: e.to_string(),
                    fallback,
                }],
            },
            Ok(text) => match ThemeFile::parse(&text) {
                Ok((file, warnings)) => Source {
                    path: Some(path.to_path_buf()),
                    file: Some(file),
                    warnings,
                },
                Err(e) => Source {
                    path: Some(path.to_path_buf()),
                    file: None,
                    warnings: vec![ThemeWarning::Unparsable {
                        message: e.to_string(),
                        fallback,
                    }],
                },
            },
        };
        self.sources.insert(id, source);
    }

    fn resolve_all(&mut self) {
        self.resolved.clear();
        self.warnings.clear();
        let ids: Vec<String> = self.sources.keys().cloned().collect();
        for id in ids {
            if builtin::is_builtin(&id) {
                self.warnings
                    .entry(id.clone())
                    .or_default()
                    .push(ThemeWarning::ReservedId { id });
                continue;
            }
            let mut stack = Vec::new();
            self.resolve(&id, &mut stack);
        }
    }

    /// Resolves `id` (memoized). `stack` holds the ids being resolved, for
    /// cycle detection.
    fn resolve(&mut self, id: &str, stack: &mut Vec<String>) -> Theme {
        if let Some(t) = self.get(id) {
            return t.clone();
        }
        let default = self.default_theme().clone();
        let Some(source) = self.sources.get(id).cloned() else {
            return default;
        };
        let mut warnings = source.warnings;
        let theme = match source.file {
            None => {
                let mut t = default;
                id.clone_into(&mut t.id);
                id.clone_into(&mut t.name);
                t.extends = Some(builtin::DEFAULT_ID.to_owned());
                t
            }
            Some(file) => {
                let parent_id = file
                    .extends
                    .clone()
                    .unwrap_or_else(|| builtin::DEFAULT_ID.to_owned());
                stack.push(id.to_owned());
                let parent = if stack.contains(&parent_id) {
                    let mut chain = stack.clone();
                    chain.push(parent_id);
                    warnings.push(ThemeWarning::ExtendsCycle {
                        chain: chain.join(" -> "),
                        fallback: default.id.clone(),
                    });
                    default
                } else if self.get(&parent_id).is_some() || self.is_user_source(&parent_id) {
                    self.resolve(&parent_id, stack)
                } else {
                    warnings.push(ThemeWarning::UnknownParent {
                        parent: parent_id,
                        fallback: default.id.clone(),
                    });
                    default
                };
                stack.pop();
                file.apply(id, &parent)
            }
        };
        if !warnings.is_empty() {
            self.warnings.insert(id.to_owned(), warnings);
        }
        self.resolved.insert(id.to_owned(), theme.clone());
        theme
    }

    fn is_user_source(&self, id: &str) -> bool {
        !builtin::is_builtin(id) && self.sources.contains_key(id)
    }
}

fn is_theme_file(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case(THEME_FILE_EXTENSION))
}

fn theme_id_for(path: &Path) -> Option<String> {
    path.file_stem().and_then(|s| s.to_str()).map(str::to_owned)
}
