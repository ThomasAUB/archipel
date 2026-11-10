//! The rebindable keymap (PLAN §3.4): keystrokes → action ids.
//!
//! Defaults live in [`Keymap::defaults`]; the user's keymap file (TOML,
//! `"ctrl-shift-p" = "palette.actions"`) overrides or removes bindings
//! (`"ctrl-j" = ""`). Every shortcut is listed in one place.

use std::collections::BTreeMap;
use std::fmt;

/// A key with modifiers, normalized (`ctrl-shift-p`).
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Keystroke {
    /// Control (Command is mapped to it on macOS by the UI).
    pub ctrl: bool,
    /// Shift.
    pub shift: bool,
    /// Alt / Option.
    pub alt: bool,
    /// The key, lowercase (`p`, `enter`, `tab`, `f5`).
    pub key: String,
}

/// Error parsing a keystroke.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid keystroke {0:?}")]
pub struct InvalidKeystroke(pub String);

impl Keystroke {
    /// Parses `ctrl-shift-p`, `Ctrl+Enter`, `alt-f4`… (separators `-` or `+`).
    pub fn parse(text: &str) -> Result<Self, InvalidKeystroke> {
        let err = || InvalidKeystroke(text.to_owned());
        let lower = text.trim().to_ascii_lowercase();
        let mut parts: Vec<&str> = lower.split(['-', '+']).collect();
        // `ctrl--` / `ctrl++`: the key itself is the separator.
        if parts.len() >= 2
            && parts[parts.len() - 1].is_empty()
            && parts[parts.len() - 2].is_empty()
        {
            parts.truncate(parts.len() - 2);
            parts.push(if lower.ends_with('+') { "+" } else { "-" });
        }
        let key = parts.pop().filter(|k| !k.is_empty()).ok_or_else(err)?;
        let mut stroke = Self {
            ctrl: false,
            shift: false,
            alt: false,
            key: key.to_owned(),
        };
        for m in parts {
            match m {
                "ctrl" | "control" | "cmd" | "super" => stroke.ctrl = true,
                "shift" => stroke.shift = true,
                "alt" | "option" => stroke.alt = true,
                _ => return Err(err()),
            }
        }
        Ok(stroke)
    }
}

impl fmt::Display for Keystroke {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.ctrl {
            f.write_str("ctrl-")?;
        }
        if self.alt {
            f.write_str("alt-")?;
        }
        if self.shift {
            f.write_str("shift-")?;
        }
        f.write_str(&self.key)
    }
}

/// Bindings from keystrokes to action ids.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Keymap {
    bindings: BTreeMap<Keystroke, String>,
}

/// Default shortcuts (PLAN §3.4).
const DEFAULTS: &[(&str, &str)] = &[
    ("ctrl-p", "palette.projects"),
    ("ctrl-shift-p", "palette.actions"),
    ("ctrl-tab", "tabs.next"),
    ("ctrl-shift-tab", "tabs.previous"),
    ("ctrl-enter", "commit.submit"),
    ("ctrl-z", "history.undo"),
    ("ctrl-shift-z", "history.redo"),
    ("ctrl-j", "drawer.toggle"),
    ("ctrl-shift-f", "repos.fetch_all"),
    ("ctrl-k", "commit.open"),
    ("f5", "repos.refresh"),
    ("ctrl-h", "repos.toggle_hidden"),
    ("ctrl-shift-h", "repos.show_hidden"),
    ("ctrl-shift-t", "view.dependency_tree"),
    ("ctrl-n", "window.new"),
    ("ctrl-shift-n", "window.new"),
    ("ctrl-`", "window.next"),
    ("ctrl-shift-`", "window.previous"),
    ("ctrl-f6", "window.next"),
    ("ctrl-shift-f6", "window.previous"),
];

/// Problems found while loading a keymap file (never fatal).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeymapWarning(pub String);

impl Keymap {
    /// The default bindings.
    #[must_use]
    pub fn defaults() -> Self {
        let mut map = Self::default();
        for (k, a) in DEFAULTS {
            if let Ok(stroke) = Keystroke::parse(k) {
                map.bindings.insert(stroke, (*a).to_owned());
            }
        }
        map
    }

    /// Applies a user keymap file over the defaults. Invalid entries are
    /// skipped with a warning; an empty action unbinds the key.
    #[must_use]
    pub fn with_overrides(mut self, toml_text: &str) -> (Self, Vec<KeymapWarning>) {
        let mut warnings = Vec::new();
        let table: toml::Table = match toml_text.parse() {
            Ok(t) => t,
            Err(e) => {
                warnings.push(KeymapWarning(format!("keymap file ignored: {e}")));
                return (self, warnings);
            }
        };
        for (key, value) in table {
            let Some(action) = value.as_str() else {
                warnings.push(KeymapWarning(format!("{key}: the action must be a string")));
                continue;
            };
            match Keystroke::parse(&key) {
                Ok(stroke) if action.is_empty() => {
                    self.bindings.remove(&stroke);
                }
                Ok(stroke) => {
                    self.bindings.insert(stroke, action.to_owned());
                }
                Err(e) => warnings.push(KeymapWarning(e.to_string())),
            }
        }
        (self, warnings)
    }

    /// The action bound to a keystroke.
    #[must_use]
    pub fn action(&self, stroke: &Keystroke) -> Option<&str> {
        self.bindings.get(stroke).map(String::as_str)
    }

    /// The keystrokes bound to an action (for menus and the palette).
    #[must_use]
    pub fn keys_for(&self, action: &str) -> Vec<&Keystroke> {
        self.bindings
            .iter()
            .filter(|(_, a)| *a == action)
            .map(|(k, _)| k)
            .collect()
    }

    /// Binds a keystroke (replacing any previous binding of that key).
    pub fn bind(&mut self, stroke: Keystroke, action: impl Into<String>) {
        self.bindings.insert(stroke, action.into());
    }

    /// Every binding, sorted by keystroke.
    pub fn iter(&self) -> impl Iterator<Item = (&Keystroke, &str)> {
        self.bindings.iter().map(|(k, a)| (k, a.as_str()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_normalizes() {
        let a = Keystroke::parse("Ctrl+Shift+P").unwrap();
        assert_eq!(a, Keystroke::parse("shift-ctrl-p").unwrap());
        assert_eq!(a.to_string(), "ctrl-shift-p");
        assert_eq!(Keystroke::parse("ctrl--").unwrap().key, "-");
        assert!(Keystroke::parse("hyper-x").is_err());
        assert!(Keystroke::parse("").is_err());
    }

    #[test]
    fn defaults_cover_documented_shortcuts() {
        let k = Keymap::defaults();
        assert_eq!(
            k.action(&Keystroke::parse("ctrl-p").unwrap()),
            Some("palette.projects")
        );
        assert_eq!(
            k.action(&Keystroke::parse("ctrl-shift-z").unwrap()),
            Some("history.redo")
        );
        assert_eq!(k.keys_for("drawer.toggle")[0].to_string(), "ctrl-j");
        for (key, action) in [
            ("ctrl-`", "window.next"),
            ("ctrl-shift-`", "window.previous"),
            ("ctrl-f6", "window.next"),
            ("ctrl-shift-f6", "window.previous"),
        ] {
            assert_eq!(k.action(&Keystroke::parse(key).unwrap()), Some(action));
        }
    }

    #[test]
    fn overrides_rebind_and_unbind() {
        let (k, warnings) = Keymap::defaults().with_overrides(
            "\"ctrl-j\" = \"\"\n\"alt-d\" = \"drawer.toggle\"\n\"bogus-key-x\" = \"a\"\n\"ctrl-q\" = 3\n",
        );
        assert_eq!(k.action(&Keystroke::parse("ctrl-j").unwrap()), None);
        assert_eq!(
            k.action(&Keystroke::parse("alt-d").unwrap()),
            Some("drawer.toggle")
        );
        assert_eq!(warnings.len(), 2);
        let (same, w) = Keymap::defaults().with_overrides("not toml [");
        assert_eq!(same, Keymap::defaults());
        assert_eq!(w.len(), 1);
    }
}
