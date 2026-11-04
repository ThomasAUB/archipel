//! Derivation of a full [`ColorSet`] from three base colors (background,
//! text, accent) using OKLCH lightness/chroma steps.
//!
//! All steps move "towards the text": lighter on dark themes, darker on
//! light ones. Text-like tokens are then nudged in lightness until they meet
//! their WCAG target on the surfaces they are drawn on. The result is a pure
//! function of the inputs (deterministic).

use crate::color::{Color, Oklch, WCAG_AA_LARGE_TEXT, WCAG_AA_NORMAL_TEXT};
use crate::settings::Appearance;
use crate::token::{ColorSet, ColorToken, LANE_COUNT};

/// The three colors a theme can be derived from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BaseColors {
    /// Main application background (`bg.app`).
    pub background: Color,
    /// Default text color (`fg.default`, adjusted for contrast if needed).
    pub text: Color,
    /// Accent color (`accent`).
    pub accent: Color,
}

/// OKLCH lightness at or above which a background counts as light.
pub const LIGHT_BACKGROUND_THRESHOLD: f64 = 0.6;

/// Lightness step from `bg.app` to `bg.panel`.
pub const PANEL_STEP: f64 = 0.03;
/// Lightness step from `bg.app` to `bg.elevated`.
pub const ELEVATED_STEP: f64 = 0.055;
/// Lightness step from `bg.app` to `bg.input` (negative: away from text).
pub const INPUT_STEP: f64 = -0.015;
/// Lightness step from `bg.app` to `bg.hover`.
pub const HOVER_STEP: f64 = 0.075;
/// Lightness step from `bg.app` to `bg.selected`.
pub const SELECTED_STEP: f64 = 0.13;
/// Lightness step from `bg.app` to `border.subtle`.
pub const BORDER_SUBTLE_STEP: f64 = 0.09;
/// Lightness step from `bg.app` to `border.default`.
pub const BORDER_STEP: f64 = 0.17;
/// Lightness step from `bg.app` to `accent.subtle`.
pub const ACCENT_SUBTLE_STEP: f64 = 0.10;
/// Lightness step from `accent` to `accent.hover`.
pub const ACCENT_HOVER_STEP: f64 = 0.06;
/// Fraction of the accent chroma used for `bg.selected` / `accent.subtle`.
pub const ACCENT_TINT_FACTOR: f64 = 0.4;
/// Upper bound on the chroma of accent-tinted surfaces.
pub const ACCENT_TINT_MAX_CHROMA: f64 = 0.07;

/// How far (Oklab mix) `fg.muted` moves from the text towards the background.
pub const MUTED_MIX: f64 = 0.30;
/// How far `fg.subtle` moves from the text towards the background.
pub const SUBTLE_MIX: f64 = 0.48;
/// How far `fg.disabled` moves from the text towards the background.
pub const DISABLED_MIX: f64 = 0.62;
/// How far `syntax.comment` moves from the text towards the background.
pub const COMMENT_MIX: f64 = 0.38;
/// How far `syntax.operator` moves from the text towards the background.
pub const OPERATOR_MIX: f64 = 0.12;

/// Lightness of status, syntax and diff foreground colors on dark themes.
pub const FG_HUE_LIGHTNESS_DARK: f64 = 0.78;
/// Lightness of status, syntax and diff foreground colors on light themes.
pub const FG_HUE_LIGHTNESS_LIGHT: f64 = 0.50;
/// Chroma of status, syntax and diff foreground colors.
pub const FG_HUE_CHROMA: f64 = 0.15;

/// Lightness step from `bg.app` to the diff line backgrounds.
pub const DIFF_BG_STEP: f64 = 0.06;
/// Chroma of the diff line backgrounds.
pub const DIFF_BG_CHROMA: f64 = 0.045;
/// Lightness step from `bg.app` to the word-level diff highlights.
pub const DIFF_WORD_STEP: f64 = 0.13;
/// Chroma of the word-level diff highlights.
pub const DIFF_WORD_CHROMA: f64 = 0.09;

/// Hue (degrees) of the first graph lane.
pub const LANE_HUE_START: f64 = 25.0;
/// Hue spacing (degrees) between consecutive lanes; with [`LANE_COUNT`]
/// lanes this spreads them evenly around the hue circle.
pub const LANE_HUE_STEP: f64 = 360.0 / 10.0;
// `LANE_HUE_STEP` and `lane_position` assume ten lanes.
const _: () = assert!(LANE_COUNT == 10);
/// Chroma of graph lane colors.
pub const LANE_CHROMA: f64 = 0.15;
/// Lightness offset alternated between consecutive lanes so neighbors also
/// differ in lightness.
pub const LANE_LIGHTNESS_ALTERNATION: f64 = 0.05;

/// Lightness increment used when searching for a color meeting a contrast target.
pub const CONTRAST_SEARCH_STEP: f64 = 0.005;

/// Hues (OKLCH degrees) of semantic colors.
mod hue {
    pub const ADD: f64 = 150.0;
    pub const DEL: f64 = 25.0;
    pub const DIRTY: f64 = 80.0;
    pub const AHEAD: f64 = 235.0;
    pub const BEHIND: f64 = 300.0;
    pub const CONFLICT: f64 = 350.0;
    pub const SUCCESS: f64 = 150.0;
    pub const ERROR: f64 = 25.0;
    pub const WARNING: f64 = 60.0;
    pub const KEYWORD: f64 = 305.0;
    pub const STRING: f64 = 140.0;
    pub const FUNCTION: f64 = 250.0;
    pub const TYPE: f64 = 190.0;
    pub const NUMBER: f64 = 55.0;
    pub const CONSTANT: f64 = 20.0;
}

impl BaseColors {
    /// Light or dark, from the background lightness.
    #[must_use]
    pub fn appearance(&self) -> Appearance {
        appearance_of(self.background)
    }
}

/// Light or dark appearance of a background color.
#[must_use]
pub fn appearance_of(background: Color) -> Appearance {
    if background.to_oklch().l >= LIGHT_BACKGROUND_THRESHOLD {
        Appearance::Light
    } else {
        Appearance::Dark
    }
}

/// Returns `fg` if it already reaches `min_ratio` against `bg`; otherwise
/// moves its OKLCH lightness away from `bg` (keeping hue, chroma reduced only
/// for gamut) in [`CONTRAST_SEARCH_STEP`] increments until it does, falling
/// back to black or white.
#[must_use]
pub fn ensure_contrast(fg: Color, bg: Color, min_ratio: f64) -> Color {
    if fg.contrast_ratio(bg) >= min_ratio {
        return fg;
    }
    let lighten = bg.contrast_ratio(Color::WHITE) >= bg.contrast_ratio(Color::BLACK);
    let dir = if lighten { 1.0 } else { -1.0 };
    let start = fg.to_oklch();
    let mut l = start.l;
    while (0.0..=1.0).contains(&l) {
        l += dir * CONTRAST_SEARCH_STEP;
        let candidate = Oklch { l, ..start }.to_color();
        if candidate.contrast_ratio(bg) >= min_ratio {
            return candidate;
        }
    }
    let extreme = if lighten { Color::WHITE } else { Color::BLACK };
    extreme.with_alpha(fg.a)
}

/// [`ensure_contrast`] against several backgrounds that share a light/dark
/// side (all surfaces of one theme).
#[must_use]
pub fn ensure_contrast_all(fg: Color, backgrounds: &[Color], min_ratio: f64) -> Color {
    let mut out = fg;
    // Two passes: adjusting for one surface can only move further from the
    // others when they are on the same side, so the second pass is a no-op
    // in practice; it guards against mixed inputs.
    for _ in 0..2 {
        for &bg in backgrounds {
            out = ensure_contrast(out, bg, min_ratio);
        }
    }
    out
}

/// Derives every color token from the three base colors.
#[must_use]
pub fn derive_colors(base: &BaseColors) -> ColorSet {
    let d = Deriver::new(base);
    let mut set = ColorSet::filled(d.bg);
    let fg = d.surfaces_and_text(base.text, &mut set);
    d.accent_and_borders(fg, &mut set);
    d.status_and_syntax(fg, &mut set);
    d.diff(&mut set);
    d.lanes(&mut set);
    set
}

/// Shared state of one derivation.
struct Deriver {
    bg: Color,
    bg_lch: Oklch,
    bg_panel: Color,
    /// `+1` on dark themes (steps lighten), `-1` on light ones.
    toward_text: f64,
    accent: Color,
    accent_lch: Oklch,
    accent_tint: f64,
    /// Lightness of hued foregrounds (status, syntax, diff, lanes).
    hue_l: f64,
}

impl Deriver {
    fn new(base: &BaseColors) -> Self {
        let bg = base.background.with_alpha(255);
        let bg_lch = bg.to_oklch();
        let dark = appearance_of(bg) == Appearance::Dark;
        let accent = base.accent.with_alpha(255);
        let accent_lch = accent.to_oklch();
        let mut d = Self {
            bg,
            bg_lch,
            bg_panel: bg,
            toward_text: if dark { 1.0 } else { -1.0 },
            accent,
            accent_lch,
            accent_tint: (accent_lch.c * ACCENT_TINT_FACTOR).min(ACCENT_TINT_MAX_CHROMA),
            hue_l: if dark {
                FG_HUE_LIGHTNESS_DARK
            } else {
                FG_HUE_LIGHTNESS_LIGHT
            },
        };
        d.bg_panel = d.surface(PANEL_STEP);
        d
    }

    /// `bg.app` moved `step` towards the text, same hue/chroma.
    fn surface(&self, step: f64) -> Color {
        Oklch {
            l: self.bg_lch.l + self.toward_text * step,
            ..self.bg_lch
        }
        .to_color()
    }

    /// `bg.app` moved `step` towards the text, with the given chroma/hue.
    fn tinted(&self, step: f64, chroma: f64, hue: f64) -> Color {
        Oklch::new(self.bg_lch.l + self.toward_text * step, chroma, hue).to_color()
    }

    /// A hued foreground color at the theme's foreground lightness.
    fn hued(&self, hue: f64) -> Color {
        Oklch::new(self.hue_l, FG_HUE_CHROMA, hue).to_color()
    }

    /// Readable as normal text on app and panel backgrounds.
    fn on_surfaces(&self, c: Color) -> Color {
        ensure_contrast_all(c, &[self.bg, self.bg_panel], WCAG_AA_NORMAL_TEXT)
    }

    /// Line and word-highlight backgrounds of a diff side.
    fn diff_bgs(&self, hue: f64) -> (Color, Color) {
        (
            self.tinted(DIFF_BG_STEP, DIFF_BG_CHROMA, hue),
            self.tinted(DIFF_WORD_STEP, DIFF_WORD_CHROMA, hue),
        )
    }

    /// Surfaces and text tokens; returns `fg.default`.
    fn surfaces_and_text(&self, text: Color, set: &mut ColorSet) -> Color {
        let bg = self.bg;
        let bg_hover = self.surface(HOVER_STEP);
        let bg_selected = self.tinted(SELECTED_STEP, self.accent_tint, self.accent_lch.h);
        let bg_elevated = self.surface(ELEVATED_STEP);
        let bg_input = self.surface(INPUT_STEP);
        set.set(ColorToken::BgApp, bg);
        set.set(ColorToken::BgPanel, self.bg_panel);
        set.set(ColorToken::BgElevated, bg_elevated);
        set.set(ColorToken::BgInput, bg_input);
        set.set(ColorToken::BgHover, bg_hover);
        set.set(ColorToken::BgSelected, bg_selected);

        // The default text must be readable on every surface it is drawn on.
        let text_surfaces = [bg, self.bg_panel, bg_hover, bg_selected];
        let (add, add_word) = self.diff_bgs(hue::ADD);
        let (del, del_word) = self.diff_bgs(hue::DEL);
        let mut all_surfaces = vec![bg_elevated, bg_input, add, add_word, del, del_word];
        all_surfaces.extend(text_surfaces);
        let fg = ensure_contrast_all(text.with_alpha(255), &all_surfaces, WCAG_AA_NORMAL_TEXT);
        set.set(ColorToken::FgDefault, fg);
        set.set(
            ColorToken::FgMuted,
            ensure_contrast_all(fg.mix(bg, MUTED_MIX), &text_surfaces, WCAG_AA_NORMAL_TEXT),
        );
        set.set(
            ColorToken::FgSubtle,
            ensure_contrast_all(
                fg.mix(bg, SUBTLE_MIX),
                &[bg, self.bg_panel],
                WCAG_AA_LARGE_TEXT,
            ),
        );
        set.set(ColorToken::FgDisabled, fg.mix(bg, DISABLED_MIX));
        fg
    }

    fn accent_and_borders(&self, fg: Color, set: &mut ColorSet) {
        let accent = self.accent;
        set.set(ColorToken::Accent, accent);
        set.set(
            ColorToken::AccentHover,
            Oklch {
                l: self.accent_lch.l + self.toward_text * ACCENT_HOVER_STEP,
                ..self.accent_lch
            }
            .to_color(),
        );
        set.set(ColorToken::AccentFg, text_on(accent, &[self.bg, fg]));
        set.set(
            ColorToken::AccentSubtle,
            self.tinted(ACCENT_SUBTLE_STEP, self.accent_tint, self.accent_lch.h),
        );
        set.set(ColorToken::BorderSubtle, self.surface(BORDER_SUBTLE_STEP));
        set.set(ColorToken::Border, self.surface(BORDER_STEP));
        set.set(
            ColorToken::BorderFocus,
            ensure_contrast_all(accent, &[self.bg, self.bg_panel], WCAG_AA_LARGE_TEXT),
        );
    }

    /// Status colors (also used as text: counts, labels) and syntax colors.
    fn status_and_syntax(&self, fg: Color, set: &mut ColorSet) {
        for (t, h) in [
            (ColorToken::StatusDirty, hue::DIRTY),
            (ColorToken::StatusAhead, hue::AHEAD),
            (ColorToken::StatusBehind, hue::BEHIND),
            (ColorToken::StatusConflict, hue::CONFLICT),
            (ColorToken::StatusSuccess, hue::SUCCESS),
            (ColorToken::StatusError, hue::ERROR),
            (ColorToken::StatusWarning, hue::WARNING),
            (ColorToken::SyntaxKeyword, hue::KEYWORD),
            (ColorToken::SyntaxString, hue::STRING),
            (ColorToken::SyntaxFunction, hue::FUNCTION),
            (ColorToken::SyntaxType, hue::TYPE),
            (ColorToken::SyntaxNumber, hue::NUMBER),
            (ColorToken::SyntaxConstant, hue::CONSTANT),
        ] {
            set.set(t, self.on_surfaces(self.hued(h)));
        }
        set.set(
            ColorToken::SyntaxComment,
            self.on_surfaces(fg.mix(self.bg, COMMENT_MIX)),
        );
        set.set(
            ColorToken::SyntaxOperator,
            self.on_surfaces(fg.mix(self.bg, OPERATOR_MIX)),
        );
    }

    fn diff(&self, set: &mut ColorSet) {
        for (bg_t, word_t, fg_t, h) in [
            (
                ColorToken::DiffAddBg,
                ColorToken::DiffAddWordBg,
                ColorToken::DiffAddFg,
                hue::ADD,
            ),
            (
                ColorToken::DiffDelBg,
                ColorToken::DiffDelWordBg,
                ColorToken::DiffDelFg,
                hue::DEL,
            ),
        ] {
            let (line_bg, word_bg) = self.diff_bgs(h);
            set.set(bg_t, line_bg);
            set.set(word_t, word_bg);
            set.set(
                fg_t,
                ensure_contrast_all(
                    self.hued(h),
                    &[line_bg, word_bg, self.bg],
                    WCAG_AA_NORMAL_TEXT,
                ),
            );
        }
    }

    /// Graph lanes: evenly spaced hues, alternating lightness, readable as
    /// graphical objects (3:1) on app and panel backgrounds.
    fn lanes(&self, set: &mut ColorSet) {
        for (i, &t) in ColorToken::LANES.iter().enumerate() {
            let offset = if i % 2 == 0 {
                LANE_LIGHTNESS_ALTERNATION / 2.0
            } else {
                -LANE_LIGHTNESS_ALTERNATION / 2.0
            };
            let h = LANE_HUE_START + LANE_HUE_STEP * lane_position(i);
            let c = Oklch::new(self.hue_l + offset, LANE_CHROMA, h).to_color();
            set.set(
                t,
                ensure_contrast_all(c, &[self.bg, self.bg_panel], WCAG_AA_LARGE_TEXT),
            );
        }
    }
}

/// Lane `i`'s position on the hue circle. Consecutive lanes are placed far
/// apart (stride 3 over 10 positions, coprime) so adjacent lanes in the graph
/// never get neighboring hues.
fn lane_position(i: usize) -> f64 {
    const STRIDE: usize = 3;
    let pos = (i * STRIDE) % LANE_COUNT;
    // LANE_COUNT is tiny; the conversion is exact.
    f64::from(u8::try_from(pos).unwrap_or(0))
}

/// Picks a readable text color on `fill`: the first candidate reaching AA,
/// otherwise black or white, whichever contrasts more.
fn text_on(fill: Color, candidates: &[Color]) -> Color {
    candidates
        .iter()
        .copied()
        .find(|c| c.contrast_ratio(fill) >= WCAG_AA_NORMAL_TEXT)
        .unwrap_or_else(|| {
            if fill.contrast_ratio(Color::WHITE) >= fill.contrast_ratio(Color::BLACK) {
                Color::WHITE
            } else {
                Color::BLACK
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ensure_contrast_reaches_target() {
        let bg = Color::rgb(30, 30, 30);
        let fg = Color::rgb(60, 60, 60);
        let out = ensure_contrast(fg, bg, WCAG_AA_NORMAL_TEXT);
        assert!(out.contrast_ratio(bg) >= WCAG_AA_NORMAL_TEXT);
        // Already fine: unchanged.
        assert_eq!(ensure_contrast(Color::WHITE, bg, 4.5), Color::WHITE);
    }

    #[test]
    fn lane_positions_are_a_permutation() {
        let mut p: Vec<_> = (0..LANE_COUNT).map(|i| (i * 3) % LANE_COUNT).collect();
        p.sort_unstable();
        assert_eq!(p, (0..LANE_COUNT).collect::<Vec<_>>());
    }
}
