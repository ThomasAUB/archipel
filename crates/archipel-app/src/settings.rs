//! Loading `settings.toml`: any field may be omitted (defaults apply), and a
//! broken file never prevents the app from starting.

use std::path::Path;

use archipel_model::settings::Settings;

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

#[cfg(test)]
mod tests {
    use super::*;

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
