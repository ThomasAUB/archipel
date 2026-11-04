//! Per-project window identity colors (spec §2 "Recognizing a window at a
//! glance").
//!
//! A project gets an accent color derived from a stable key (its path), so
//! the same project has the same color on every launch; the window theme is
//! then the user's theme re-derived with that accent ([`with_project_accent`]),
//! which tints the selection and accent tokens consistently.

use crate::color::{Color, Oklab, Oklch};
use crate::theme::Theme;

/// OKLCH lightness of automatic project accents (readable on light and dark).
pub const PROJECT_ACCENT_LIGHTNESS: f64 = 0.68;
/// OKLCH chroma of automatic project accents: enough to tell windows apart,
/// muted enough to sit with the theme's neutrals.
pub const PROJECT_ACCENT_CHROMA: f64 = 0.12;
/// Hues (OKLCH degrees) automatic project accents are picked from. The
/// olive / lime band (~90–140°) is left out: at this lightness and chroma it
/// reads as muddy next to the slate neutrals.
pub const PROJECT_ACCENT_HUES: [f64; 12] = [
    15.0, 40.0, 65.0, 150.0, 175.0, 200.0, 222.0, 245.0, 268.0, 290.0, 315.0, 340.0,
];

/// FNV-1a 64-bit offset basis.
const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
/// FNV-1a 64-bit prime.
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// Stable 64-bit hash (FNV-1a): unlike `std`'s hasher it never changes
/// between Rust versions, so colors stay the same across upgrades.
fn fnv1a(text: &str) -> u64 {
    text.bytes().fold(FNV_OFFSET, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(FNV_PRIME)
    })
}

/// The automatic accent of a project, derived from `key` (its path).
#[must_use]
pub fn accent_for_key(key: &str) -> Color {
    let n = PROJECT_ACCENT_HUES.len() as u64;
    // The remainder is below the (tiny) table length.
    let i = usize::try_from(fnv1a(key) % n).unwrap_or(0);
    Oklch::new(
        PROJECT_ACCENT_LIGHTNESS,
        PROJECT_ACCENT_CHROMA,
        PROJECT_ACCENT_HUES[i],
    )
    .to_color()
}

/// `theme` re-derived with `accent` as its accent color; explicit token
/// overrides of the theme are kept.
#[must_use]
pub fn with_project_accent(theme: &Theme, accent: Color) -> Theme {
    let mut out = theme.clone();
    let mut base = *theme.base();
    base.accent = accent;
    out.set_base(base);
    out
}

/// `theme` re-derived with its background tinted by its accent: `percent`
/// (0–100, clamped) of the accent's Oklab hue and chroma mixed into the
/// background, whose lightness is kept so text contrast and the light /
/// dark appearance are unchanged. Explicit token overrides are kept.
#[must_use]
pub fn with_background_tint(theme: &Theme, percent: u8) -> Theme {
    let t = f64::from(percent.min(100)) / 100.0;
    let mut out = theme.clone();
    let mut base = *theme.base();
    let (bg, accent) = (base.background.to_oklab(), base.accent.to_oklab());
    base.background = Oklab {
        a: bg.a + (accent.a - bg.a) * t,
        b: bg.b + (accent.b - bg.b) * t,
        ..bg
    }
    .to_color();
    out.set_base(base);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builtin;
    use crate::token::ColorToken;

    #[test]
    fn accent_is_stable_and_spread() {
        let a = accent_for_key("/work/product");
        assert_eq!(a, accent_for_key("/work/product"));
        assert_ne!(a, accent_for_key("/work/other"));
        // Known value: guards against an accidental hash change.
        assert_eq!(fnv1a("a"), 0xaf63_dc4c_8601_ec8c);
    }

    #[test]
    fn project_accent_retints_selection() {
        let dark = builtin::dark();
        let accent = accent_for_key("/x");
        let themed = with_project_accent(&dark, accent);
        assert_eq!(themed.color(ColorToken::Accent), accent);
        assert_ne!(
            themed.color(ColorToken::BgSelected),
            dark.color(ColorToken::BgSelected)
        );
        assert_eq!(
            themed.color(ColorToken::BgApp),
            dark.color(ColorToken::BgApp)
        );
    }

    #[test]
    fn background_tint_keeps_lightness() {
        for theme in [builtin::dark(), builtin::light()] {
            let themed = with_project_accent(&theme, accent_for_key("/x"));
            assert_eq!(with_background_tint(&themed, 0).base(), themed.base());
            let tinted = with_background_tint(&themed, 30);
            let (before, after) = (
                themed.base().background.to_oklch(),
                tinted.base().background.to_oklch(),
            );
            assert!((before.l - after.l).abs() < 0.005);
            assert!(after.c > before.c);
            assert_eq!(tinted.appearance(), themed.appearance());
            assert_eq!(
                tinted.color(ColorToken::Accent),
                themed.color(ColorToken::Accent)
            );
        }
    }
}
