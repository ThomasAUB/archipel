//! # archipel-theme
//!
//! **Responsibility:** the look-and-feel extension point (ARCHITECTURE §5,
//! PLAN §3.5). It defines the semantic token set UI code reads instead of
//! literal colors/fonts/sizes, derives a full theme from three base colors,
//! ships the built-in themes, parses/exports TOML theme files with `extends`,
//! and provides the pure logic behind the theme editor. It is UI-toolkit
//! agnostic: the frontend maps [`Color`] and the settings to its own types.
//!
//! **Allowed dependencies:** `serde`, `toml`, `thiserror` only; no workspace
//! crates. The only I/O is reading theme files in [`ThemeRegistry`].
//!
//! **Main types:**
//! - [`Color`], [`Oklch`], [`Oklab`]: sRGB color, perceptual spaces, WCAG contrast.
//! - [`ColorToken`], [`TokenGroup`], [`ColorSet`]: typed tokens with stable string ids.
//! - [`BaseColors`], [`derive_colors`]: derivation from background, text and accent.
//! - [`Theme`]: a resolved theme (colors + [`Typography`], [`Density`], radius,
//!   [`AnimationSpeed`]); [`builtin`] holds light, dark and high-contrast.
//! - [`ThemeFile`], [`export_theme`]: theme files (lenient parsing, minimal export).
//! - [`ThemeRegistry`]: built-ins plus `themes/*.toml`, `extends` resolution, reload.
//! - [`ThemeSelection`]: fixed theme or "follow OS".
//! - [`editor`]: contrast warnings, reset to parent, duplicate.
//! - [`ThemeError`] (fatal, strict APIs) and [`ThemeWarning`] (non-fatal).

pub mod builtin;
pub mod color;
pub mod derive;
pub mod editor;
pub mod error;
pub mod file;
pub mod project;
pub mod registry;
pub mod settings;
pub mod theme;
pub mod token;

pub use color::{
    Color, ColorParseError, Oklab, Oklch, WCAG_AA_LARGE_TEXT, WCAG_AA_NORMAL_TEXT,
    WCAG_AAA_NORMAL_TEXT,
};
pub use derive::{BaseColors, derive_colors, ensure_contrast};
pub use editor::{ContrastWarning, contrast_warnings};
pub use error::{ThemeError, ThemeWarning};
pub use file::{ThemeFile, export_theme};
pub use project::{accent_for_key, with_background_tint, with_project_accent};
pub use registry::ThemeRegistry;
pub use settings::{AnimationSpeed, Appearance, Density, Spacing, ThemeSelection, Typography};
pub use theme::Theme;
pub use token::{ColorSet, ColorToken, LANE_COUNT, TokenGroup, TokenInfo, grouped_tokens};
