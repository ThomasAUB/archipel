//! Loading `settings.toml`: any field may be omitted (defaults apply), and a
//! broken file never prevents the app from starting. Also the remembered
//! theme choice (`appearance.toml`), written by the app rather than the user.

use std::path::Path;

use archipel_model::settings::Settings;
use archipel_theme::ThemeSelection;

/// Loads settings from a TOML file. Returns the defaults and a warning when
/// the file is missing-but-unreadable or invalid; a missing file is silent.
#[must_use]
pub fn load_settings(path: &Path) -> (Settings, Option<String>) {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (Settings::default(), None),
        Err(e) => {
            return (
                Settings::default(),
                Some(format!("cannot read {}: {e}", path.display())),
            );
        }
    };
    match toml::from_str(&text) {
        Ok(s) => (s, None),
        Err(e) => (
            Settings::default(),
            Some(format!("invalid {}: {e}", path.display())),
        ),
    }
}

/// Loads the remembered theme choice (`appearance.toml`). `None` when the
/// file is missing (silent) or broken (with a warning): the caller then uses
/// the default theme.
#[must_use]
pub fn load_theme_selection(path: &Path) -> (Option<ThemeSelection>, Option<String>) {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (None, None),
        Err(e) => return (None, Some(format!("cannot read {}: {e}", path.display()))),
    };
    match toml::from_str(&text) {
        Ok(s) => (Some(s), None),
        Err(e) => (None, Some(format!("invalid {}: {e}", path.display()))),
    }
}

/// Saves the theme choice, creating the config directory if needed.
pub fn save_theme_selection(path: &Path, selection: &ThemeSelection) -> Result<(), String> {
    let text = toml::to_string(selection).map_err(|e| e.to_string())?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    }
    std::fs::write(path, text).map_err(|e| format!("cannot write {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_selection_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("config").join("appearance.toml");
        assert_eq!(load_theme_selection(&p), (None, None));
        let sel = ThemeSelection::Fixed {
            id: "quiet-cyber".into(),
        };
        save_theme_selection(&p, &sel).unwrap();
        assert_eq!(load_theme_selection(&p), (Some(sel), None));
        std::fs::write(&p, "mode = 3").unwrap();
        let (s, w) = load_theme_selection(&p);
        assert!(s.is_none() && w.is_some());
    }

    #[test]
    fn partial_and_broken_files() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("settings.toml");
        assert_eq!(load_settings(&p), (Settings::default(), None));
        std::fs::write(&p, "network_concurrency = 2\nread_backend = \"cli\"\n").unwrap();
        let (s, w) = load_settings(&p);
        assert!(w.is_none());
        assert_eq!(s.network_concurrency, 2);
        assert_eq!(s.read_backend, archipel_model::settings::ReadBackend::Cli);
        assert_eq!(s.journal_limit, Settings::default().journal_limit);
        std::fs::write(&p, "network_concurrency = \"x\"").unwrap();
        let (s, w) = load_settings(&p);
        assert!(w.is_some());
        assert_eq!(s, Settings::default());
    }
}
