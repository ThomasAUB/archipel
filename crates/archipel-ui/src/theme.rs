//! The bridge between `archipel-theme` tokens and GPUI colors.
//!
//! This is the **only** module of the UI allowed to deal with concrete
//! colors (`cargo xtask check-colors`); views ask for semantic tokens.

use std::collections::HashSet;

use archipel_app::dashboard::Tone;
use archipel_theme::{Color, ColorToken, Theme, ThemeRegistry, builtin};
use gpui_kit::{App, Global, Hsla, Pixels, Rgba, px};

/// The active theme, as a GPUI global.
pub struct UiTheme {
    /// Built-in and user themes.
    pub registry: ThemeRegistry,
    /// The active theme.
    pub theme: Theme,
    /// Font families installed on this system (lowercase).
    installed: HashSet<String>,
    /// UI font actually used (the theme's, or the platform fallback).
    ui_family: String,
    /// Monospace font actually used.
    mono_family: String,
}

impl Global for UiTheme {}

/// UI fonts tried, in order, when the theme's font is not installed.
const UI_FALLBACKS: &[&str] = &[
    "Segoe UI",
    ".SystemUIFont",
    "Noto Sans",
    "DejaVu Sans",
    "Ubuntu",
    "Cantarell",
];
/// Monospace fonts tried when the theme's font is not installed.
const MONO_FALLBACKS: &[&str] = &[
    "Cascadia Mono",
    "Consolas",
    "Menlo",
    "DejaVu Sans Mono",
    "Noto Sans Mono",
    "Ubuntu Mono",
];

impl UiTheme {
    /// Loads user themes from `dir` and activates `id` (default theme when unknown).
    ///
    /// A font family that is not installed makes the text system search for
    /// a fallback on every new text run, which cost ~200 ms on the first
    /// frame (ADR 0001): fonts are resolved once here instead.
    pub fn load(dir: &std::path::Path, id: &str, cx: &App) -> Self {
        let mut registry = ThemeRegistry::new();
        if let Err(e) = registry.load_dir(dir) {
            tracing::warn!(error = %e, "cannot load user themes");
        }
        let theme = registry.get(id).cloned().unwrap_or_else(builtin::dark);
        let installed = cx
            .text_system()
            .all_font_names()
            .into_iter()
            .map(|n| n.to_lowercase())
            .collect();
        let mut ui = Self {
            registry,
            theme,
            installed,
            ui_family: String::new(),
            mono_family: String::new(),
        };
        ui.resolve_fonts();
        ui
    }

    /// Activates another theme (live: every view re-renders).
    pub fn set(&mut self, id: &str) {
        if let Some(t) = self.registry.get(id) {
            self.theme = t.clone();
            self.resolve_fonts();
        }
    }

    fn resolve_fonts(&mut self) {
        self.ui_family = self.pick(&self.theme.typography.ui_font, UI_FALLBACKS);
        self.mono_family = self.pick(&self.theme.typography.mono_font, MONO_FALLBACKS);
    }

    fn pick(&self, wanted: &str, fallbacks: &[&str]) -> String {
        std::iter::once(wanted)
            .chain(fallbacks.iter().copied())
            .find(|f| self.installed.contains(&f.to_lowercase()))
            .unwrap_or(wanted)
            .to_owned()
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

/// UI font family (installed).
pub fn ui_font(cx: &App) -> String {
    cx.global::<UiTheme>().ui_family.clone()
}

/// Monospace font family (installed).
pub fn mono_font(cx: &App) -> String {
    cx.global::<UiTheme>().mono_family.clone()
}

/// Base text size.
pub fn text_size(cx: &App) -> Pixels {
    px(cx.global::<UiTheme>().theme.typography.size)
}
