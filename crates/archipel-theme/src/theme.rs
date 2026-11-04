//! The resolved [`Theme`]: base colors, per-token overrides, the computed
//! color table and non-color settings.

use std::collections::BTreeMap;

use crate::color::Color;
use crate::derive::{BaseColors, derive_colors};
use crate::settings::{AnimationSpeed, Appearance, DEFAULT_RADIUS, Density, Spacing, Typography};
use crate::token::{ColorSet, ColorToken};

/// A fully resolved theme. Every token has a value.
///
/// Colors are always `derive_colors(base)` with `overrides` applied on top;
/// the setters keep that invariant.
#[derive(Debug, Clone, PartialEq)]
pub struct Theme {
    /// Stable id (built-in id or theme file stem).
    pub id: String,
    /// Display name.
    pub name: String,
    /// Id of the parent theme this one was resolved from, if any.
    pub extends: Option<String>,
    /// Fonts and text metrics.
    pub typography: Typography,
    /// Information density.
    pub density: Density,
    /// Corner radius in logical pixels.
    pub radius: f32,
    /// Animation speed.
    pub animation: AnimationSpeed,
    base: BaseColors,
    overrides: BTreeMap<ColorToken, Color>,
    colors: ColorSet,
}

impl Theme {
    /// A theme derived entirely from base colors, with default settings.
    #[must_use]
    pub fn from_base(id: impl Into<String>, name: impl Into<String>, base: BaseColors) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            extends: None,
            typography: Typography::default(),
            density: Density::default(),
            radius: DEFAULT_RADIUS,
            animation: AnimationSpeed::default(),
            colors: derive_colors(&base),
            base,
            overrides: BTreeMap::new(),
        }
    }

    /// Base colors the theme is derived from.
    #[must_use]
    pub const fn base(&self) -> &BaseColors {
        &self.base
    }

    /// Explicit per-token overrides (accumulated along the `extends` chain).
    #[must_use]
    pub const fn overrides(&self) -> &BTreeMap<ColorToken, Color> {
        &self.overrides
    }

    /// The complete color table.
    #[must_use]
    pub const fn colors(&self) -> &ColorSet {
        &self.colors
    }

    /// Color of one token.
    #[must_use]
    pub const fn color(&self, token: ColorToken) -> Color {
        self.colors.get(token)
    }

    /// Light or dark, from the base background.
    #[must_use]
    pub fn appearance(&self) -> Appearance {
        self.base.appearance()
    }

    /// Spacing scale for the theme's density.
    #[must_use]
    pub const fn spacing(&self) -> Spacing {
        self.density.spacing()
    }

    /// Replaces the base colors and re-derives every non-overridden token.
    pub fn set_base(&mut self, base: BaseColors) {
        self.base = base;
        self.recompute();
    }

    /// Overrides one token.
    pub fn set_token(&mut self, token: ColorToken, color: Color) {
        self.overrides.insert(token, color);
        self.colors.set(token, color);
    }

    /// Removes the override of one token, returning it to its derived value.
    pub fn clear_override(&mut self, token: ColorToken) {
        if self.overrides.remove(&token).is_some() {
            self.colors.set(token, derive_colors(&self.base).get(token));
        }
    }

    /// Replaces all overrides at once.
    pub fn set_overrides(&mut self, overrides: BTreeMap<ColorToken, Color>) {
        self.overrides = overrides;
        self.recompute();
    }

    /// Colors this theme's base would give with only `overrides` applied.
    #[must_use]
    pub fn colors_with(base: &BaseColors, overrides: &BTreeMap<ColorToken, Color>) -> ColorSet {
        let mut colors = derive_colors(base);
        for (&t, &c) in overrides {
            colors.set(t, c);
        }
        colors
    }

    fn recompute(&mut self) {
        self.colors = Self::colors_with(&self.base, &self.overrides);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builtin;

    #[test]
    fn override_and_clear() {
        let mut t = builtin::dark();
        let derived = t.color(ColorToken::BgPanel);
        t.set_token(ColorToken::BgPanel, Color::rgb(1, 2, 3));
        assert_eq!(t.color(ColorToken::BgPanel), Color::rgb(1, 2, 3));
        t.set_base(BaseColors {
            accent: Color::rgb(200, 0, 0),
            ..*t.base()
        });
        assert_eq!(t.color(ColorToken::BgPanel), Color::rgb(1, 2, 3));
        t.clear_override(ColorToken::BgPanel);
        assert_eq!(t.color(ColorToken::BgPanel), derived);
    }
}
