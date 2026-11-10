//! The bridge between `archipel-theme` tokens and GPUI colors.
//!
//! This is the **only** module of the UI allowed to deal with concrete
//! colors (`cargo xtask check-colors`); views ask for semantic tokens.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use archipel_app::dashboard::Tone;
use archipel_app::settings::{load_theme_selection, save_theme_selection};
use archipel_theme::{
    Appearance, Color, ColorToken, Oklab, Theme, ThemeRegistry, ThemeSelection, builtin,
};
use gpui_kit::component::highlighter::{HighlightTheme, SyntaxColors};
use gpui_kit::component::scroll::ScrollbarMode;
use gpui_kit::component::{Theme as KitTheme, ThemeMode};
use gpui_kit::{App, FontWeight, Global, Hsla, Pixels, Rgba, WindowAppearance, px};

/// The active theme, as a GPUI global.
///
/// `theme` is what views read: the user's theme, re-derived with the
/// accent and background tint of the project of the window being drawn (spec §2
/// "Recognizing a window at a glance"). Each window applies its colors at
/// the start of its render ([`apply_window_colors`]); windows are drawn one
/// at a time on the main thread, so every view of a window sees that
/// window's colors.
pub struct UiTheme {
    /// Built-in and user themes.
    pub registry: ThemeRegistry,
    /// The effective theme (user theme + window colors).
    pub theme: Theme,
    /// The theme the user chose.
    selected: Theme,
    /// Accent applied to `theme`, if any.
    accent: Option<Color>,
    /// Background tint (percent) applied to `theme`, if any.
    background_tint: Option<u8>,
    /// Font families installed on this system (lowercase).
    installed: HashSet<String>,
    /// UI font actually used (the theme's, or the platform fallback).
    ui_family: String,
    /// Monospace font actually used.
    mono_family: String,
    /// Where the theme choice is remembered (`None`: not remembered).
    selection_file: Option<PathBuf>,
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
    pub fn load(dir: &Path, id: &str, cx: &App) -> Self {
        let mut registry = ThemeRegistry::new();
        if let Err(e) = registry.load_dir(dir) {
            tracing::warn!(error = %e, "cannot load user themes");
        }
        let theme = registry.get(id).cloned().unwrap_or_else(builtin::dark);
        let mut installed = cx
            .text_system()
            .all_font_names()
            .into_iter()
            .map(|n| n.to_lowercase())
            .collect();
        crate::fonts::register_missing(&mut installed, cx);
        let mut ui = Self {
            registry,
            selected: theme.clone(),
            theme,
            accent: None,
            background_tint: None,
            installed,
            ui_family: String::new(),
            mono_family: String::new(),
            selection_file: None,
        };
        ui.resolve_fonts();
        ui
    }

    /// Loads the theme remembered in `selection_file` (the default theme
    /// when there is none) and remembers later choices there. Returns a
    /// warning when the file cannot be read.
    pub fn load_remembered(
        dir: &Path,
        selection_file: PathBuf,
        cx: &App,
    ) -> (Self, Option<String>) {
        let (selection, warning) = load_theme_selection(&selection_file);
        let os = match cx.window_appearance() {
            WindowAppearance::Light | WindowAppearance::VibrantLight => Appearance::Light,
            WindowAppearance::Dark | WindowAppearance::VibrantDark => Appearance::Dark,
        };
        let id = selection
            .as_ref()
            .map_or(builtin::DEFAULT_ID, |s| s.theme_id(os));
        let mut ui = Self::load(dir, id, cx);
        ui.selection_file = Some(selection_file);
        (ui, warning)
    }

    /// Remembers the current theme as the user's fixed choice.
    pub fn remember(&self) -> Result<(), String> {
        let Some(path) = &self.selection_file else {
            return Ok(());
        };
        let selection = ThemeSelection::Fixed {
            id: self.selected.id.clone(),
        };
        save_theme_selection(path, &selection)
    }

    /// Activates another theme (live: every view re-renders).
    pub fn set(&mut self, id: &str) {
        if let Some(t) = self.registry.get(id) {
            self.selected = t.clone();
            self.theme = self.derive(self.accent, self.background_tint);
            self.resolve_fonts();
        }
    }

    fn derive(&self, accent: Option<Color>, background_tint: Option<u8>) -> Theme {
        let theme = accent.map_or_else(
            || self.selected.clone(),
            |a| archipel_theme::with_project_accent(&self.selected, a),
        );
        background_tint.map_or(theme.clone(), |t| {
            archipel_theme::with_background_tint(&theme, t)
        })
    }
}

/// Projects the user's theme onto the gpui-component theme, which colors
/// the toolkit widgets (text inputs, text areas, scrollbars, the window
/// surface).
///
/// Left alone, that theme stays in its default *light* mode while Archipel
/// draws with its own (usually dark) tokens: inputs then got a white
/// background under Archipel's light-gray text, unreadable. Call it once the
/// [`UiTheme`] global is set and after every theme change. It uses the
/// user's theme without the window accent: the toolkit theme is global, so
/// a per-window value would flip at every frame of a multi-window session.
pub fn sync_toolkit_theme(cx: &mut App) {
    let ui = cx.global::<UiTheme>();
    let t = &ui.selected;
    let c = |token| to_hsla(t.color(token));
    let dark = t.appearance() == Appearance::Dark;
    let ui_family = ui.ui_family.clone();
    let mono_family = ui.mono_family.clone();
    let radius = px(t.radius);
    let (bg_app, bg_panel, bg_elevated, bg_hover, bg_selected) = (
        c(ColorToken::BgApp),
        c(ColorToken::BgPanel),
        c(ColorToken::BgElevated),
        c(ColorToken::BgHover),
        c(ColorToken::BgSelected),
    );
    let (fg, fg_muted, fg_subtle) = (
        c(ColorToken::FgDefault),
        c(ColorToken::FgMuted),
        c(ColorToken::FgSubtle),
    );
    let (accent, accent_hover, accent_fg, accent_subtle) = (
        c(ColorToken::Accent),
        c(ColorToken::AccentHover),
        c(ColorToken::AccentFg),
        c(ColorToken::AccentSubtle),
    );
    let (border, border_subtle, focus) = (
        c(ColorToken::Border),
        c(ColorToken::BorderSubtle),
        c(ColorToken::BorderFocus),
    );
    let (error, success, warning) = (
        c(ColorToken::StatusError),
        c(ColorToken::StatusSuccess),
        c(ColorToken::StatusWarning),
    );
    let syntax = syntax_colors(&c);
    if KitTheme::global(cx).is_dark() != dark {
        KitTheme::change(
            if dark {
                ThemeMode::Dark
            } else {
                ThemeMode::Light
            },
            None,
            cx,
        );
    }
    KitTheme::update(cx, |k| {
        k.font_family = ui_family.into();
        k.mono_font_family = mono_family.into();
        k.radius = radius;
        // Shown whenever a list overflows, not only on hover or scroll.
        k.scrollbar_mode = ScrollbarMode::Always;
        // Code in the editor and the file viewer takes the theme's syntax
        // colors, as diffs do.
        if let Some(syntax) = syntax {
            k.highlight_theme = Arc::new(highlight_theme(&k.highlight_theme, syntax));
        }
        let k = &mut k.colors;
        k.background = bg_app;
        k.foreground = fg;
        k.border = border;
        k.input = border;
        k.ring = focus;
        k.caret = fg;
        k.selection = accent_subtle;
        k.muted = bg_panel;
        k.muted_foreground = fg_subtle;
        k.popover = bg_elevated;
        k.popover_foreground = fg;
        k.accent = bg_hover;
        k.accent_foreground = fg;
        k.primary = accent;
        k.primary_hover = accent_hover;
        k.primary_active = accent_hover;
        k.primary_foreground = accent_fg;
        k.secondary = bg_elevated;
        k.secondary_hover = bg_hover;
        k.secondary_active = bg_selected;
        k.secondary_foreground = fg;
        k.list = bg_panel;
        k.list_hover = bg_hover;
        k.list_active = bg_selected;
        k.list_active_border = accent;
        k.scrollbar_thumb = border;
        k.scrollbar_thumb_hover = fg_muted;
        k.danger = error;
        k.success = success;
        k.warning = warning;
        k.link = accent;
        k.link_hover = accent_hover;
        k.title_bar = bg_panel;
        k.title_bar_border = border_subtle;
        k.window_border = border;
        k.tab_bar = bg_panel;
        k.tab = bg_panel;
        k.tab_active = bg_app;
        k.tab_foreground = fg_muted;
        k.tab_active_foreground = fg;
        k.sidebar = bg_panel;
        k.sidebar_foreground = fg;
        k.sidebar_border = border_subtle;
    });
}

/// `base` with `syntax` colors.
fn highlight_theme(base: &HighlightTheme, syntax: SyntaxColors) -> HighlightTheme {
    let mut theme = base.clone();
    theme.style.syntax = syntax;
    theme
}

/// The toolkit's highlight names in the theme's syntax colors; the names
/// left out stay plain, as they do in a diff (see [`crate::syntax`]).
fn syntax_colors(c: &impl Fn(ColorToken) -> Hsla) -> Option<SyntaxColors> {
    use ColorToken as T;
    const NAMES: [(&str, ColorToken); 20] = [
        ("keyword", T::SyntaxKeyword),
        ("preproc", T::SyntaxKeyword),
        ("tag", T::SyntaxKeyword),
        ("string", T::SyntaxString),
        ("string.escape", T::SyntaxString),
        ("string.regex", T::SyntaxString),
        ("string.special", T::SyntaxString),
        ("comment", T::SyntaxComment),
        ("comment.doc", T::SyntaxComment),
        ("function", T::SyntaxFunction),
        ("attribute", T::SyntaxFunction),
        ("type", T::SyntaxType),
        ("constructor", T::SyntaxType),
        ("enum", T::SyntaxType),
        ("number", T::SyntaxNumber),
        ("boolean", T::SyntaxConstant),
        ("constant", T::SyntaxConstant),
        ("variant", T::SyntaxConstant),
        ("variable.special", T::SyntaxConstant),
        ("operator", T::SyntaxOperator),
    ];
    // The toolkit's style type cannot be built field by field outside it,
    // but it reads from the theme file format.
    let map = NAMES
        .iter()
        .map(|&(name, token)| (name.to_owned(), serde_json::json!({ "color": c(token) })))
        .collect();
    serde_json::from_value(serde_json::Value::Object(map)).ok()
}

/// Re-derives the theme with the accent and background tint of the window
/// about to be drawn (`None`: the user's as is). Cheap when they are
/// unchanged.
pub fn apply_window_colors(cx: &mut App, accent: Option<Color>, background_tint: Option<u8>) {
    let ui = cx.global::<UiTheme>();
    if ui.accent == accent && ui.background_tint == background_tint {
        return;
    }
    let ui = cx.global_mut::<UiTheme>();
    ui.theme = ui.derive(accent, background_tint);
    ui.accent = accent;
    ui.background_tint = background_tint;
}

impl UiTheme {
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
pub(crate) fn to_hsla(c: Color) -> Hsla {
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
    to_hsla(cx.global::<UiTheme>().theme.color(token))
}

/// Fill of the "clean" status dot in the sidebar: a fixed green, not derived
/// from the theme.
pub fn clean_dot() -> Hsla {
    to_hsla(Color::rgb(0x30, 0x98, 0x5e))
}

/// The color of graph lane `index` (wraps around the palette).
pub fn lane(cx: &App, index: u32) -> Hsla {
    let i = usize::try_from(index).unwrap_or(0);
    to_hsla(cx.global::<UiTheme>().theme.colors().lane(i))
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
            Tone::Success => ColorToken::StatusSuccess,
            Tone::Warning => ColorToken::StatusWarning,
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

/// Weight of regular UI text.
pub fn ui_weight(cx: &App) -> FontWeight {
    FontWeight(cx.global::<UiTheme>().theme.typography.weight)
}

/// Monospace font family (installed).
pub fn mono_font(cx: &App) -> String {
    cx.global::<UiTheme>().mono_family.clone()
}

/// Base text size.
pub fn text_size(cx: &App) -> Pixels {
    px(cx.global::<UiTheme>().theme.typography.size)
}

/// How far the repository panes sit from `bg.app`, in `bg.panel` steps.
const REPO_SURFACE_STEP: f64 = 1.8;
/// How far the staging panes sit from `bg.app`, in `bg.panel` steps.
const STAGING_SURFACE_STEP: f64 = 2.1;
/// Share of the accent's hue given to the staging panes.
const STAGING_ACCENT_TINT: f64 = 0.18;

/// `bg.app` moved `t` times the way to `bg.panel` (past it when `t > 1`),
/// so the side panes keep the direction of the theme's own panel surface.
fn beyond_panel(theme: &Theme, t: f64) -> Oklab {
    let (app, panel) = (
        theme.color(ColorToken::BgApp).to_oklab(),
        theme.color(ColorToken::BgPanel).to_oklab(),
    );
    Oklab {
        l: app.l + (panel.l - app.l) * t,
        a: app.a + (panel.a - app.a) * t,
        b: app.b + (panel.b - app.b) * t,
        ..app
    }
}

/// Surface of the repository side panes (projects sidebar, refs pane):
/// neutral, clearly apart from the central pane.
pub fn repo_surface(cx: &App) -> Hsla {
    let theme = &cx.global::<UiTheme>().theme;
    to_hsla(beyond_panel(theme, REPO_SURFACE_STEP).to_color())
}

/// Surface of the staging panes (repo tab details, quick edit): a little
/// further from the central pane than [`repo_surface`], with a faint accent
/// tint so the two read apart. Stays below `bg.hover` so rows still show it.
pub fn staging_surface(cx: &App) -> Hsla {
    let theme = &cx.global::<UiTheme>().theme;
    let s = beyond_panel(theme, STAGING_SURFACE_STEP);
    let accent = theme.color(ColorToken::Accent).to_oklab();
    to_hsla(
        Oklab {
            a: s.a + (accent.a - s.a) * STAGING_ACCENT_TINT,
            b: s.b + (accent.b - s.b) * STAGING_ACCENT_TINT,
            ..s
        }
        .to_color(),
    )
}
