//! Built-in themes: light, dark, high-contrast (dark) and Quiet Cyber (dark).

use std::collections::BTreeMap;

use crate::color::Color;
use crate::derive::BaseColors;
use crate::file::ThemeFile;
use crate::theme::Theme;
use crate::token::ColorToken;

/// Id of the built-in light theme.
pub const LIGHT_ID: &str = "light";
/// Id of the built-in dark theme.
pub const DARK_ID: &str = "dark";
/// Id of the built-in high-contrast (dark) theme.
pub const HIGH_CONTRAST_ID: &str = "high-contrast";
/// Id of the built-in Quiet Cyber theme.
pub const QUIET_CYBER_ID: &str = "quiet-cyber";
/// Theme used when nothing else is available (unknown id, broken parent…).
pub const DEFAULT_ID: &str = DARK_ID;
/// Ids of all built-in themes.
pub const IDS: [&str; 4] = [LIGHT_ID, DARK_ID, HIGH_CONTRAST_ID, QUIET_CYBER_ID];

/// The built-in light theme.
#[must_use]
pub fn light() -> Theme {
    Theme::from_base(
        LIGHT_ID,
        "Classic Light",
        BaseColors {
            // Cool off-white paper, slate ink, a restrained cobalt.
            background: Color::rgb(0xfa, 0xfb, 0xfd),
            text: Color::rgb(0x20, 0x24, 0x2c),
            accent: Color::rgb(0x3b, 0x67, 0xc1),
        },
    )
}

/// The built-in dark theme.
#[must_use]
pub fn dark() -> Theme {
    Theme::from_base(
        DARK_ID,
        "Classic Dark",
        BaseColors {
            // Slate (faint blue cast), dimmed cool grey text (~11:1, not a
            // glaring 14:1), periwinkle.
            background: Color::rgb(0x1c, 0x1f, 0x25),
            text: Color::rgb(0xcd, 0xd2, 0xda),
            accent: Color::rgb(0x7a, 0xa1, 0xe9),
        },
    )
}

/// The built-in high-contrast dark theme (pure black, white text, strong
/// borders, AAA text contrast).
#[must_use]
pub fn high_contrast() -> Theme {
    let mut theme = Theme::from_base(
        HIGH_CONTRAST_ID,
        "High Contrast",
        BaseColors {
            background: Color::BLACK,
            text: Color::WHITE,
            accent: Color::rgb(0xff, 0xd6, 0x0a),
        },
    );
    let overrides = BTreeMap::from([
        (ColorToken::FgMuted, Color::rgb(0xe0, 0xe0, 0xe0)),
        (ColorToken::FgSubtle, Color::rgb(0xc4, 0xc4, 0xc4)),
        (ColorToken::Border, Color::rgb(0xc0, 0xc0, 0xc0)),
        (ColorToken::BorderSubtle, Color::rgb(0x80, 0x80, 0x80)),
        (ColorToken::BorderFocus, Color::rgb(0xff, 0xd6, 0x0a)),
    ]);
    theme.set_overrides(overrides);
    theme
}

/// Source of the Quiet Cyber theme, in the user theme file format.
pub const QUIET_CYBER_TOML: &str = include_str!("../themes/quiet-cyber.toml");

/// The built-in Quiet Cyber theme: matte charcoal with desaturated teal,
/// mauve and amber, square corners. Defined as a theme file on top of
/// [`dark`]; falls back to plain dark should the embedded file not parse
/// (the tests make sure it does).
#[must_use]
pub fn quiet_cyber() -> Theme {
    let mut theme = ThemeFile::parse(QUIET_CYBER_TOML)
        .map_or_else(|_| dark(), |(file, _)| file.apply(QUIET_CYBER_ID, &dark()));
    theme.extends = None;
    theme
}

/// All built-in themes.
#[must_use]
pub fn all() -> Vec<Theme> {
    vec![light(), dark(), high_contrast(), quiet_cyber()]
}

/// A built-in theme by id.
#[must_use]
pub fn get(id: &str) -> Option<Theme> {
    match id {
        LIGHT_ID => Some(light()),
        DARK_ID => Some(dark()),
        HIGH_CONTRAST_ID => Some(high_contrast()),
        QUIET_CYBER_ID => Some(quiet_cyber()),
        _ => None,
    }
}

/// Whether `id` names a built-in theme.
#[must_use]
pub fn is_builtin(id: &str) -> bool {
    IDS.contains(&id)
}
