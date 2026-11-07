//! Project identity (spec §2): name, accent color, background tint and icon
//! that make a project's windows recognizable, and the user-typed values
//! that set them.
//!
//! * The colors and icon live in the project definition, so a project file
//!   carries them for the whole team; an older personal choice in the UI
//!   state still wins until replaced.
//! * An image icon inside the project's folder is stored relative to it.

use std::path::{Path, PathBuf};

use archipel_model::ProjectId;
use archipel_model::project::{ProjectLocation, ProjectSettings};
use archipel_model::ui_state::UiState;

use super::{App, AppError, AppEvent, lock};

/// How a project's window is recognized (spec §2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectIdentity {
    /// Project name.
    pub name: String,
    /// Accent color of the project's windows.
    pub accent: archipel_theme::Color,
    /// The accent is the automatic one (derived from the project path).
    pub automatic_accent: bool,
    /// Percentage of the accent tinting the window background (`None`:
    /// untinted).
    pub background_tint: Option<u8>,
    /// Icon, if any.
    pub icon: Option<ProjectIcon>,
    /// One or two letters for badges.
    pub initials: String,
}

impl ProjectIdentity {
    /// Window title: project name first so truncated titles stay readable.
    /// An image icon cannot go in a title; a text one leads it.
    #[must_use]
    pub fn window_title(&self) -> String {
        match self.icon.as_ref().and_then(ProjectIcon::text) {
            Some(icon) => format!("{icon} {} — Archipel", self.name),
            None => format!("{} — Archipel", self.name),
        }
    }
}

/// A project icon (spec §2), read from the `icon` setting.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProjectIcon {
    /// Text drawn as is: an emoji, a symbol.
    Text(String),
    /// An image file (absolute path).
    Image(PathBuf),
}

/// Extensions that make an `icon` value an image file.
const ICON_IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "gif", "webp", "bmp", "ico", "svg"];

impl ProjectIcon {
    /// Reads an `icon` setting: a value naming an image file (`logo.png`,
    /// `assets/icon.svg`) is a path, relative to the project's folder
    /// `base`; anything else is text. `None` when blank, or for a relative
    /// image path in a project without a folder.
    #[must_use]
    pub fn parse(value: &str, base: Option<&Path>) -> Option<Self> {
        let value = value.trim();
        if value.is_empty() {
            return None;
        }
        let path = Path::new(value);
        if !is_image_path(path) {
            return Some(Self::Text(value.to_owned()));
        }
        if path.is_absolute() {
            return Some(Self::Image(path.to_path_buf()));
        }
        base.map(|b| Self::Image(b.join(path)))
    }

    /// The text of a text icon.
    #[must_use]
    pub fn text(&self) -> Option<&str> {
        match self {
            Self::Text(t) => Some(t),
            Self::Image(_) => None,
        }
    }
}

fn is_image_path(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| ICON_IMAGE_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

/// The folder a project's relative paths start from: the project file's folder.
fn project_folder(location: &ProjectLocation) -> Option<PathBuf> {
    let ProjectLocation::SharedFile(file) = location;
    file.parent().map(Path::to_path_buf)
}

/// An `icon` value as stored: an image inside the project's folder is kept
/// relative to it, with `/` separators, so the file works for the whole team.
fn stored_icon(value: &str, folder: Option<&Path>) -> String {
    let path = Path::new(value);
    let relative = folder
        .filter(|_| path.is_absolute() && is_image_path(path))
        .and_then(|f| path.strip_prefix(f).ok());
    match relative {
        Some(rel) => rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/"),
        None => value.to_owned(),
    }
}

/// Initials of a project name: first letters of the first two words, or
/// the first two letters of a single word.
#[must_use]
pub fn initials(name: &str) -> String {
    let words: Vec<&str> = name
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    let letters: String = match words.as_slice() {
        [] => "?".into(),
        [one] => one.chars().take(2).collect(),
        [a, b, ..] => a.chars().take(1).chain(b.chars().take(1)).collect(),
    };
    letters.to_uppercase()
}

/// A user-typed color as `#rrggbb` (`None` when blank).
fn parse_color(color: Option<String>) -> Result<Option<String>, AppError> {
    let Some(hex) = color.map(|a| a.trim().to_owned()).filter(|a| !a.is_empty()) else {
        return Ok(None);
    };
    let hex = if hex.starts_with('#') {
        hex
    } else {
        format!("#{hex}")
    };
    archipel_theme::Color::from_hex(&hex)
        .map_err(|e| AppError::InvalidInput(format!("{hex} is not a color (#rrggbb): {e}")))?;
    Ok(Some(hex))
}

/// Validates a percentage typed by the user (`15` or `15%`, 0–100); empty
/// means none.
fn parse_percent(percent: Option<String>) -> Result<Option<u8>, AppError> {
    let Some(text) = percent
        .map(|p| p.trim().to_owned())
        .filter(|p| !p.is_empty())
    else {
        return Ok(None);
    };
    text.trim_end_matches('%')
        .trim()
        .parse::<u8>()
        .ok()
        .filter(|&p| p <= 100)
        .map(Some)
        .ok_or_else(|| AppError::InvalidInput(format!("{text} is not a percentage (0–100)")))
}

impl App {
    /// Icon of any known project (switcher): personal choice, else the team
    /// default of its project file.
    #[must_use]
    pub fn project_icon(&self, id: &ProjectId) -> Option<ProjectIcon> {
        if self.project().is_some_and(|(open, _)| &open == id) {
            return self.identity()?.icon;
        }
        let entry = self
            .services
            .registry
            .list_projects()
            .ok()?
            .into_iter()
            .find(|e| &e.id == id)?;
        let folder = project_folder(&entry.location);
        let personal = self.services.ui_state.load_ui_state(id).ok().flatten();
        let icon = match personal.and_then(|u| u.icon) {
            Some(icon) => icon,
            None => {
                self.services
                    .projects
                    .load(&entry.location)
                    .ok()?
                    .settings
                    .icon?
            }
        };
        ProjectIcon::parse(&icon, folder.as_deref())
    }

    /// How the open project's window is recognized (spec §2): the color
    /// and icon of the project definition (a legacy personal choice wins),
    /// else automatic.
    #[must_use]
    pub fn identity(&self) -> Option<ProjectIdentity> {
        let guard = lock(&self.session);
        let s = guard.as_ref()?;
        let parse = |hex: Option<&str>| hex.and_then(|h| archipel_theme::Color::from_hex(h).ok());
        let chosen =
            parse(s.ui.accent.as_deref()).or_else(|| parse(s.def.settings.accent.as_deref()));
        let icon =
            s.ui.icon
                .as_deref()
                .or(s.def.settings.icon.as_deref())
                .and_then(|i| ProjectIcon::parse(i, project_folder(&s.location).as_deref()));
        Some(ProjectIdentity {
            name: s.def.name.clone(),
            accent: chosen.unwrap_or_else(|| archipel_theme::accent_for_key(&s.id.0)),
            automatic_accent: chosen.is_none(),
            background_tint: s.def.settings.background_tint.filter(|&t| t > 0),
            icon,
            initials: initials(&s.def.name),
        })
    }

    /// Sets the accent color typed by the user (blank: automatic).
    pub(super) fn set_accent(&self, accent: Option<String>) -> Result<(), AppError> {
        let accent = parse_color(accent)?;
        self.save_identity(|settings, ui| {
            settings.accent = accent;
            ui.accent = None;
        })
    }

    /// Sets the background tint typed by the user (blank: untinted).
    pub(super) fn set_background_tint(&self, tint: Option<String>) -> Result<(), AppError> {
        let tint = parse_percent(tint)?;
        self.save_identity(|settings, _| settings.background_tint = tint)
    }

    /// Sets the icon typed or picked by the user (blank: none).
    pub(super) fn set_icon(&self, icon: Option<String>) -> Result<(), AppError> {
        let folder = lock(&self.session)
            .as_ref()
            .and_then(|s| project_folder(&s.location));
        let icon = icon
            .map(|i| stored_icon(i.trim(), folder.as_deref()))
            .filter(|i| !i.is_empty());
        self.save_identity(|settings, ui| {
            settings.icon = icon;
            ui.icon = None;
        })
    }

    /// Changes the project's colors or icon in its definition, so a project
    /// file carries them for the whole team (spec §2), and drops any older
    /// personal override.
    fn save_identity(
        &self,
        f: impl FnOnce(&mut ProjectSettings, &mut UiState),
    ) -> Result<(), AppError> {
        self.edit_definition(|s| {
            f(&mut s.def.settings, &mut s.ui);
            Some(())
        })?;
        self.emit(AppEvent::UiStateChanged);
        Ok(())
    }
}
