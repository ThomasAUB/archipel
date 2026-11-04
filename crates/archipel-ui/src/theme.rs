//! The bridge between `archipel-theme` tokens and GPUI colors.
//!
//! This is the **only** module of the UI allowed to deal with concrete
//! colors (`cargo xtask check-colors`); views ask for semantic tokens.

use archipel_app::dashboard::Tone;
use archipel_theme::{Color, ColorToken, Theme, ThemeRegistry, builtin};
use gpui_kit::{App, Global, Hsla, Pixels, Rgba, px};

/// The active theme, as a GPUI global.
pub struct UiTheme {
    /// Built-in and user themes.
    pub registry: ThemeRegistry,
    /// The active theme.
    pub theme: Theme,
}

impl Global for UiTheme {}

impl UiTheme {
    /// Loads user themes from `dir` and activates `id` (default theme when unknown).
    pub fn load(dir: &std::path::Path, id: &str) -> Self {
        let mut registry = ThemeRegistry::new();
        if let Err(e) = registry.load_dir(dir) {
            tracing::warn!(error = %e, "cannot load user themes");
        }
        let theme = registry.get(id).cloned().unwrap_or_else(builtin::dark);
        Self { registry, theme }
    }

    /// Activates another theme (live: every view re-renders).
    pub fn set(&mut self, id: &str) {
        if let Some(t) = self.registry.get(id) {
            self.theme = t.clone();
        }
    }
}

#[allow(clippy::cast_lossless)] // u8 → f32 is exact
fn hsla(c: Color) -> Hsla {
    Rgba {
        r: f32::from(c.r) / 255.0,
        g: f32::from(c.g) / 255.0,
        b: f32::from(c.b) / 255.0,
        a: f32::from(c.a) / 255.0,
    }
    .into()
}

/// The color of a token in the active theme.
pub fn color(cx: &App, token: ColorToken) -> Hsla {
    hsla(cx.global::<UiTheme>().theme.color(token))
}

/// The color of graph lane `index` (wraps around the palette).
pub fn lane(cx: &App, index: u32) -> Hsla {
    let i = usize::try_from(index).unwrap_or(0);
    hsla(cx.global::<UiTheme>().theme.colors().lane(i))
}

/// The color of a dashboard cell tone.
pub fn tone(cx: &App, tone: Tone) -> Hsla {
    color(
        cx,
        match tone {
            Tone::Normal => ColorToken::FgDefault,
            Tone::Muted => ColorToken::FgMuted,
            Tone::Dirty => ColorToken::StatusDirty,
            Tone::Ahead => ColorToken::StatusAhead,
            Tone::Behind => ColorToken::StatusBehind,
            Tone::Conflict => ColorToken::StatusConflict,
            Tone::Error => ColorToken::StatusError,
        },
    )
}

/// Row height from the theme density.
pub fn row_height(cx: &App) -> Pixels {
    px(cx.global::<UiTheme>().theme.spacing().row_height)
}

/// Corner radius from the theme.
pub fn radius(cx: &App) -> Pixels {
    px(cx.global::<UiTheme>().theme.radius)
}

/// UI font family.
pub fn ui_font(cx: &App) -> String {
    cx.global::<UiTheme>().theme.typography.ui_font.clone()
}

/// Monospace font family.
pub fn mono_font(cx: &App) -> String {
    cx.global::<UiTheme>().theme.typography.mono_font.clone()
}

/// Base text size.
pub fn text_size(cx: &App) -> Pixels {
    px(cx.global::<UiTheme>().theme.typography.size)
}
