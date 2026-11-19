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

/// Sets `merge_tool` in the settings file (removes it for `None`: Git's
/// `merge.tool`), leaving the rest of the file — comments, order, line
/// endings — as the user wrote it. Creates the file when missing; refuses,
/// writing nothing, when it is not valid TOML.
pub fn save_merge_tool(path: &Path, tool: Option<&str>) -> Result<(), String> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
    };
    let eol = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let is_key = |line: &str| {
        line.trim_start()
            .strip_prefix("merge_tool")
            .is_some_and(|rest| rest.trim_start().starts_with('='))
    };
    let mut lines: Vec<String> = text
        .lines()
        .filter(|l| !is_key(l))
        .map(str::to_owned)
        .collect();
    if let Some(tool) = tool {
        // Top-level keys go before the first table.
        let value = toml::Value::String(tool.to_owned());
        lines.insert(0, format!("merge_tool = {value}"));
    }
    let mut out = lines.join(eol);
    if !out.is_empty() {
        out.push_str(eol);
    }
    if let Err(e) = toml::from_str::<Settings>(&out) {
        return Err(format!("invalid {}: {e}", path.display()));
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    }
    std::fs::write(path, out).map_err(|e| format!("cannot write {}: {e}", path.display()))
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
    fn merge_tool_is_saved_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("config").join("settings.toml");
        save_merge_tool(&p, Some("meld")).unwrap();
        assert_eq!(load_settings(&p).0.merge_tool.as_deref(), Some("meld"));

        std::fs::write(
            &p,
            "# mine\r\nnetwork_concurrency = 2\r\nmerge_tool = \"kdiff3\"\r\n",
        )
        .unwrap();
        save_merge_tool(&p, Some("my \"tool\"")).unwrap();
        let text = std::fs::read_to_string(&p).unwrap();
        assert_eq!(
            text,
            "merge_tool = 'my \"tool\"'\r\n# mine\r\nnetwork_concurrency = 2\r\n"
        );
        let (s, w) = load_settings(&p);
        assert!(w.is_none());
        assert_eq!(s.merge_tool.as_deref(), Some("my \"tool\""));
        assert_eq!(s.network_concurrency, 2);

        save_merge_tool(&p, None).unwrap();
        assert_eq!(
            std::fs::read_to_string(&p).unwrap(),
            "# mine\r\nnetwork_concurrency = 2\r\n"
        );

        std::fs::write(&p, "network_concurrency = \"x\"").unwrap();
        assert!(save_merge_tool(&p, Some("meld")).is_err());
        assert_eq!(
            std::fs::read_to_string(&p).unwrap(),
            "network_concurrency = \"x\""
        );
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
