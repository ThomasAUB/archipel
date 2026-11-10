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
pub const PANEL_STEP: f64 = 0.04;
/// Lightness step from `bg.app` to `bg.elevated`.
pub const ELEVATED_STEP: f64 = 0.07;
/// Lightness step from `bg.app` to `bg.input` (negative: away from text).
pub const INPUT_STEP: f64 = -0.015;
/// Lightness step from `bg.app` to `bg.hover`.
pub const HOVER_STEP: f64 = 0.115;
/// Lightness step from `bg.app` to `bg.selected`.
pub const SELECTED_STEP: f64 = 0.15;
/// Lightness step from `bg.app` to `border.subtle`.
pub const BORDER_SUBTLE_STEP: f64 = 0.12;
/// Lightness step from `bg.app` to `border.default`.
pub const BORDER_STEP: f64 = 0.19;
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
pub const FG_HUE_LIGHTNESS_LIGHT: f64 = 0.52;
/// Chroma of status, syntax and diff foreground colors: moderate, so hued
/// text reads as a family of soft tones rather than competing neons.
pub const FG_HUE_CHROMA: f64 = 0.11;

/// Lightness step from `bg.app` to the diff line backgrounds.
pub const DIFF_BG_STEP: f64 = 0.09;
/// Chroma of the diff line backgrounds.
pub const DIFF_BG_CHROMA: f64 = 0.09;
/// Lightness step from `bg.app` to the word-level diff highlights.
pub const DIFF_WORD_STEP: f64 = 0.19;
/// Chroma of the word-level diff highlights.
pub const DIFF_WORD_CHROMA: f64 = 0.14;

/// Graph lane palette, derived from the accent: lanes alternate between the
/// accent's hue family and its complementary family (a split-complementary
/// scheme), so the graph reads as one harmonious set built around the
/// window color. Each entry is (hue step from the family's center, in
/// [`LANE_HUE_STEP`] units; lightness tier, in [`LANE_TIER_STEP`] units);
/// lane `2k` takes entry `k` in the accent family, lane `2k + 1` the same
/// entry in the complementary one.
///
/// The order keeps every pair of consecutive lanes (and the last and first)
/// at least 120 degrees apart, and gives hues that are close within a family
/// opposite lightness tiers. The first lane (usually the main line) has the
/// accent's own hue.
pub const LANE_FAMILY: [(f64, f64); LANE_COUNT / 2] = [
    (0.0, 0.0),
    (2.0, -1.0),
    (1.0, 1.0),
    (-1.0, -1.0),
    (-2.0, 1.0),
];
// Two families of `LANE_FAMILY.len()` lanes cover the palette.
const _: () = assert!(LANE_COUNT == 2 * LANE_FAMILY.len());
/// Hue distance (OKLCH degrees, on the full wheel) between neighbors of a
/// lane family.
pub const LANE_HUE_STEP: f64 = 30.0;
/// Lightness distance between lane tiers.
pub const LANE_TIER_STEP: f64 = 0.06;
/// Lower bound of the lane chroma, which follows the accent's (a muted
/// accent gives a softer graph, a vivid one a livelier one).
pub const LANE_CHROMA_MIN: f64 = 0.11;
/// Upper bound of the lane chroma.
pub const LANE_CHROMA_MAX: f64 = 0.14;
/// Lightness of graph lanes on dark themes (middle tier).
pub const LANE_LIGHTNESS_DARK: f64 = 0.76;
/// Lightness of graph lanes on light themes (middle tier): a little lighter
/// than hued text, so thin lines do not turn brownish (lanes need only 3:1).
pub const LANE_LIGHTNESS_LIGHT: f64 = 0.58;
/// Below this chroma the accent is a gray: its hue means nothing, and lanes
/// are built around [`NEUTRAL_LANE_HUE`] instead.
pub const ACHROMATIC_ACCENT_CHROMA: f64 = 0.03;
/// Center hue of the lanes for a gray accent (a calm blue).
pub const NEUTRAL_LANE_HUE: f64 = 240.0;
/// The yellow-green band (OKLCH degrees, start and end) lanes skip: at lane
/// lightness and chroma it turns muddy olive. Lane hues are laid out on the
/// wheel with this band cut out, so it never shows and no two lanes collide.
pub const MUDDY_HUES: (f64, f64) = (95.0, 135.0);

/// Fraction of the hue distance to the accent that semantic colors (status,
/// diff, syntax) move towards it, so they share the accent's temperature.
pub const HARMONIZE_FACTOR: f64 = 0.25;
/// Upper bound (degrees) of that move: green stays green, red stays red.
pub const HARMONIZE_MAX_SHIFT: f64 = 12.0;

/// Lightness increment used when searching for a color meeting a contrast target.
pub const CONTRAST_SEARCH_STEP: f64 = 0.005;

/// Hues (OKLCH degrees) of semantic colors; status, diff and syntax hues are
/// nudged towards the accent ([`harmonize`]).
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
    d.accent_and_borders(&mut set);
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
        let (add, add_word) = self.diff_bgs(self.semantic(hue::ADD));
        let (del, del_word) = self.diff_bgs(self.semantic(hue::DEL));
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

    fn accent_and_borders(&self, set: &mut ColorSet) {
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
        set.set(ColorToken::AccentFg, text_on(accent));
        let subtle = self.tinted(ACCENT_SUBTLE_STEP, self.accent_tint, self.accent_lch.h);
        set.set(ColorToken::AccentSubtle, subtle);
        // The accent as text (current branch, active chips, menu checks):
        // readable on every surface it is drawn on, whatever accent is chosen.
        let surfaces = [
            self.bg,
            self.bg_panel,
            set[ColorToken::BgElevated],
            set[ColorToken::BgHover],
            set[ColorToken::BgSelected],
            subtle,
        ];
        set.set(
            ColorToken::AccentText,
            ensure_contrast_all(accent, &surfaces, WCAG_AA_NORMAL_TEXT),
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
            (ColorToken::StatusDirty, self.semantic(hue::DIRTY)),
            (ColorToken::StatusAhead, self.semantic(hue::AHEAD)),
            (ColorToken::StatusBehind, self.semantic(hue::BEHIND)),
            (ColorToken::StatusConflict, self.semantic(hue::CONFLICT)),
            (ColorToken::StatusSuccess, self.semantic(hue::SUCCESS)),
            (ColorToken::StatusError, self.semantic(hue::ERROR)),
            (ColorToken::StatusWarning, self.semantic(hue::WARNING)),
            (ColorToken::SyntaxKeyword, self.semantic(hue::KEYWORD)),
            (ColorToken::SyntaxString, self.semantic(hue::STRING)),
            (ColorToken::SyntaxFunction, self.semantic(hue::FUNCTION)),
            (ColorToken::SyntaxType, self.semantic(hue::TYPE)),
            (ColorToken::SyntaxNumber, self.semantic(hue::NUMBER)),
            (ColorToken::SyntaxConstant, self.semantic(hue::CONSTANT)),
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
                self.semantic(hue::ADD),
            ),
            (
                ColorToken::DiffDelBg,
                ColorToken::DiffDelWordBg,
                ColorToken::DiffDelFg,
                self.semantic(hue::DEL),
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

    /// Graph lanes: two hue families around the accent ([`LANE_FAMILY`]),
    /// readable as graphical objects (3:1) on app and panel backgrounds.
    fn lanes(&self, set: &mut ColorSet) {
        let lane_l = if self.toward_text > 0.0 {
            LANE_LIGHTNESS_DARK
        } else {
            LANE_LIGHTNESS_LIGHT
        };
        let chroma = self.accent_lch.c.clamp(LANE_CHROMA_MIN, LANE_CHROMA_MAX);
        let center = self.accent_hue().unwrap_or(NEUTRAL_LANE_HUE);
        for (i, &t) in ColorToken::LANES.iter().enumerate() {
            let (step, tier) = LANE_FAMILY[i / 2];
            let family = if i % 2 == 0 { 0.0 } else { 180.0 };
            let hue = lane_hue(center, family + step * LANE_HUE_STEP);
            let c = Oklch::new(lane_l + tier * LANE_TIER_STEP, chroma, hue).to_color();
            set.set(
                t,
                ensure_contrast_all(c, &[self.bg, self.bg_panel], WCAG_AA_LARGE_TEXT),
            );
        }
    }

    /// The accent's hue, unless the accent is a gray.
    fn accent_hue(&self) -> Option<f64> {
        (self.accent_lch.c >= ACHROMATIC_ACCENT_CHROMA).then_some(self.accent_lch.h)
    }

    /// Semantic `hue` nudged towards the accent's hue (see [`harmonize`]).
    fn semantic(&self, hue: f64) -> f64 {
        self.accent_hue().map_or(hue, |a| harmonize(hue, a))
    }
}

/// `hue` moved towards `target` by [`HARMONIZE_FACTOR`] of their distance,
/// at most [`HARMONIZE_MAX_SHIFT`] degrees, along the shorter way round.
#[must_use]
pub fn harmonize(hue: f64, target: f64) -> f64 {
    let diff = (target - hue + 180.0).rem_euclid(360.0) - 180.0;
    let shift = (diff * HARMONIZE_FACTOR).clamp(-HARMONIZE_MAX_SHIFT, HARMONIZE_MAX_SHIFT);
    (hue + shift).rem_euclid(360.0)
}

/// The hue `turn` degrees from `center` on the wheel with [`MUDDY_HUES`] cut
/// out: angles are measured on the shortened wheel (scaled so a full turn
/// stays a full turn), then mapped back to real hues. A center inside the
/// band snaps to its nearest edge.
#[must_use]
pub fn lane_hue(center: f64, turn: f64) -> f64 {
    let (start, end) = MUDDY_HUES;
    let width = end - start;
    let span = 360.0 - width;
    let center = center.rem_euclid(360.0);
    let (u_center, upper_edge) = if center < start {
        (center, false)
    } else if center < end {
        (start, center >= f64::midpoint(start, end))
    } else {
        (center - width, false)
    };
    let u = (u_center + turn * span / 360.0).rem_euclid(span);
    // `u` at `start` is the band itself: either of its edges.
    let below = if upper_edge { u < start } else { u <= start };
    if below { u } else { u + width }
}

/// The text color on `fill`: white or black, whichever contrasts more (at
/// least 4.58:1 for any opaque fill). The theme's own text and background
/// are not used: tinted greys read poorly on mid-tone fills.
fn text_on(fill: Color) -> Color {
    if fill.contrast_ratio(Color::WHITE) >= fill.contrast_ratio(Color::BLACK) {
        Color::WHITE
    } else {
        Color::BLACK
    }
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
    fn text_on_is_black_or_white() {
        assert_eq!(text_on(Color::rgb(0x1e, 0x3a, 0x8a)), Color::WHITE);
        assert_eq!(text_on(Color::rgb(0xfa, 0xcc, 0x15)), Color::BLACK);
        for fill in [Color::rgb(0x80, 0x80, 0x80), Color::rgb(0xe1, 0x1d, 0x48)] {
            assert!(text_on(fill).contrast_ratio(fill) >= WCAG_AA_NORMAL_TEXT);
        }
    }
}
