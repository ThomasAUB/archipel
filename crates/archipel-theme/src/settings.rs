//! Non-color theme settings: typography, density (spacing scale), corner
//! radius, animation speed, light/dark appearance and the "follow OS"
//! theme selection.

use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Smallest accepted base font size, in logical pixels.
pub const MIN_FONT_SIZE: f32 = 8.0;
/// Largest accepted base font size, in logical pixels.
pub const MAX_FONT_SIZE: f32 = 32.0;
/// Lightest accepted UI font weight (CSS scale: 400 regular, 700 bold).
pub const MIN_FONT_WEIGHT: f32 = 100.0;
/// Heaviest accepted UI font weight.
pub const MAX_FONT_WEIGHT: f32 = 900.0;
/// Smallest accepted line height (multiple of the font size).
pub const MIN_LINE_HEIGHT: f32 = 1.0;
/// Largest accepted line height (multiple of the font size).
pub const MAX_LINE_HEIGHT: f32 = 2.5;
/// Largest accepted corner radius, in logical pixels.
pub const MAX_RADIUS: f32 = 24.0;
/// Default corner radius, in logical pixels.
pub const DEFAULT_RADIUS: f32 = 6.0;

/// Fonts and text metrics.
#[derive(Debug, Clone, PartialEq)]
pub struct Typography {
    /// UI font family name. The UI layer falls back to a system font if it
    /// is not installed.
    pub ui_font: String,
    /// Monospace font family name (diffs, hashes, code).
    pub mono_font: String,
    /// Base font size in logical pixels.
    pub size: f32,
    /// Weight of regular UI text (CSS scale, 400 is regular). Thin families
    /// read better a step heavier.
    pub weight: f32,
    /// Line height as a multiple of the font size.
    pub line_height: f32,
}

impl Default for Typography {
    fn default() -> Self {
        Self {
            ui_font: "Inter".to_owned(),
            mono_font: "JetBrains Mono".to_owned(),
            size: 13.0,
            weight: 400.0,
            line_height: 1.45,
        }
    }
}

/// Information density: drives the spacing scale and row heights.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Density {
    /// Tight spacing, more rows on screen.
    Compact,
    /// Balanced default.
    #[default]
    Comfortable,
    /// Generous spacing.
    Spacious,
}

/// Spacing scale in logical pixels derived from a [`Density`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Spacing {
    /// Extra-small gap.
    pub xs: f32,
    /// Small gap.
    pub sm: f32,
    /// Medium gap (default padding).
    pub md: f32,
    /// Large gap.
    pub lg: f32,
    /// Extra-large gap (section separation).
    pub xl: f32,
    /// Height of a list/table row.
    pub row_height: f32,
}

/// Spacing unit (px) for [`Density::Compact`].
const COMPACT_UNIT: f32 = 3.0;
/// Spacing unit (px) for [`Density::Comfortable`].
const COMFORTABLE_UNIT: f32 = 4.0;
/// Spacing unit (px) for [`Density::Spacious`].
const SPACIOUS_UNIT: f32 = 5.0;
/// List row height (px) per density: compact, comfortable, spacious.
const ROW_HEIGHTS: [f32; 3] = [22.0, 26.0, 32.0];

impl Density {
    /// All densities, from tightest to loosest.
    pub const ALL: [Self; 3] = [Self::Compact, Self::Comfortable, Self::Spacious];

    /// Stable id used in theme files.
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::Compact => "compact",
            Self::Comfortable => "comfortable",
            Self::Spacious => "spacious",
        }
    }

    /// Parses a stable id.
    #[must_use]
    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|d| d.id() == id)
    }

    /// The spacing scale (1, 2, 3, 4, 6 units).
    #[must_use]
    pub const fn spacing(self) -> Spacing {
        let (unit, row_height) = match self {
            Self::Compact => (COMPACT_UNIT, ROW_HEIGHTS[0]),
            Self::Comfortable => (COMFORTABLE_UNIT, ROW_HEIGHTS[1]),
            Self::Spacious => (SPACIOUS_UNIT, ROW_HEIGHTS[2]),
        };
        Spacing {
            xs: unit,
            sm: unit * 2.0,
            md: unit * 3.0,
            lg: unit * 4.0,
            xl: unit * 6.0,
            row_height,
        }
    }
}

/// Global animation speed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AnimationSpeed {
    /// No animations (also the right choice for reduced-motion users).
    Off,
    /// Slower than default.
    Slow,
    /// Default speed.
    #[default]
    Normal,
    /// Faster than default.
    Fast,
}

/// Duration multiplier for [`AnimationSpeed::Slow`].
const SLOW_FACTOR: f32 = 1.5;
/// Duration multiplier for [`AnimationSpeed::Fast`].
const FAST_FACTOR: f32 = 0.6;

impl AnimationSpeed {
    /// All speeds.
    pub const ALL: [Self; 4] = [Self::Off, Self::Slow, Self::Normal, Self::Fast];

    /// Stable id used in theme files.
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Slow => "slow",
            Self::Normal => "normal",
            Self::Fast => "fast",
        }
    }

    /// Parses a stable id.
    #[must_use]
    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|d| d.id() == id)
    }

    /// Whether animations run at all.
    #[must_use]
    pub const fn is_enabled(self) -> bool {
        !matches!(self, Self::Off)
    }

    /// Multiplier applied to nominal animation durations (`0` when off).
    #[must_use]
    pub const fn factor(self) -> f32 {
        match self {
            Self::Off => 0.0,
            Self::Slow => SLOW_FACTOR,
            Self::Normal => 1.0,
            Self::Fast => FAST_FACTOR,
        }
    }

    /// Scales a nominal duration (zero when off).
    #[must_use]
    pub fn scale(self, nominal: Duration) -> Duration {
        nominal.mul_f32(self.factor())
    }
}

/// Light or dark appearance (of a theme or of the OS).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Appearance {
    /// Dark text on a light background.
    Light,
    /// Light text on a dark background.
    Dark,
}

/// Which theme the user chose: a fixed one, or one per OS appearance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum ThemeSelection {
    /// Always use this theme id.
    Fixed {
        /// Theme id.
        id: String,
    },
    /// Follow the OS appearance.
    FollowOs {
        /// Theme id used when the OS is in light mode.
        light: String,
        /// Theme id used when the OS is in dark mode.
        dark: String,
    },
}

impl Default for ThemeSelection {
    fn default() -> Self {
        Self::FollowOs {
            light: crate::builtin::LIGHT_ID.to_owned(),
            dark: crate::builtin::DARK_ID.to_owned(),
        }
    }
}

impl ThemeSelection {
    /// Theme id to use given the current OS appearance.
    #[must_use]
    pub fn theme_id(&self, os: Appearance) -> &str {
        match self {
            Self::Fixed { id } => id,
            Self::FollowOs { light, dark } => match os {
                Appearance::Light => light,
                Appearance::Dark => dark,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn follow_os_picks_by_appearance() {
        let sel = ThemeSelection::FollowOs {
            light: "paper".into(),
            dark: "night".into(),
        };
        assert_eq!(sel.theme_id(Appearance::Light), "paper");
        assert_eq!(sel.theme_id(Appearance::Dark), "night");
        let fixed = ThemeSelection::Fixed { id: "x".into() };
        assert_eq!(fixed.theme_id(Appearance::Light), "x");
    }

    #[test]
    fn animation_off_is_zero() {
        let d = Duration::from_millis(200);
        assert_eq!(AnimationSpeed::Off.scale(d), Duration::ZERO);
        assert_eq!(AnimationSpeed::Normal.scale(d), d);
        assert!(AnimationSpeed::Fast.scale(d) < d);
    }

    #[test]
    fn density_spacing_grows() {
        let c = Density::Compact.spacing();
        let s = Density::Spacious.spacing();
        assert!(c.md < s.md && c.row_height < s.row_height);
        assert_eq!(Density::from_id("compact"), Some(Density::Compact));
    }
}
