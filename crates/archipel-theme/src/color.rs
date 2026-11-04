//! sRGB colors with alpha, hex parsing/formatting, Oklab/OKLCH conversion
//! (with gamut mapping) and WCAG 2.x luminance / contrast.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Minimum contrast ratio for normal-size text (WCAG 2.x AA, SC 1.4.3).
pub const WCAG_AA_NORMAL_TEXT: f64 = 4.5;
/// Minimum contrast ratio for large text and UI components / graphical
/// objects (WCAG 2.x AA, SC 1.4.3 large text and SC 1.4.11).
pub const WCAG_AA_LARGE_TEXT: f64 = 3.0;
/// Minimum contrast ratio for normal-size text at level AAA (SC 1.4.6).
pub const WCAG_AAA_NORMAL_TEXT: f64 = 7.0;

/// Number of bisection steps used when reducing chroma to fit the sRGB gamut.
/// 24 steps resolve chroma to ~2e-8, far below one 8-bit sRGB step.
const GAMUT_SEARCH_STEPS: u32 = 24;
/// Tolerance on linear sRGB channels when testing gamut membership.
const GAMUT_EPSILON: f64 = 1e-7;
/// Chroma below which a color is treated as achromatic (hue reported as 0).
const ACHROMATIC_CHROMA: f64 = 1e-6;

/// An 8-bit-per-channel sRGB color with straight (non-premultiplied) alpha.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Color {
    /// Red channel.
    pub r: u8,
    /// Green channel.
    pub g: u8,
    /// Blue channel.
    pub b: u8,
    /// Alpha channel (255 = opaque).
    pub a: u8,
}

/// Why a hex color string was rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ColorParseError {
    /// The string does not start with `#`.
    #[error("color must start with `#`")]
    MissingHash,
    /// The string has a digit count other than 6 (`#rrggbb`) or 8 (`#rrggbbaa`).
    #[error("expected 6 or 8 hex digits, found {0}")]
    InvalidLength(usize),
    /// The string contains a non-hexadecimal character.
    #[error("invalid hex digit")]
    InvalidDigit,
}

impl Color {
    /// Opaque black.
    pub const BLACK: Self = Self::rgb(0, 0, 0);
    /// Opaque white.
    pub const WHITE: Self = Self::rgb(255, 255, 255);
    /// Fully transparent black.
    pub const TRANSPARENT: Self = Self::rgba(0, 0, 0, 0);

    /// Opaque color from 8-bit channels.
    #[must_use]
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }

    /// Color from 8-bit channels and alpha.
    #[must_use]
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    /// Same color with a different alpha.
    #[must_use]
    pub const fn with_alpha(self, a: u8) -> Self {
        Self { a, ..self }
    }

    /// Parses `#rrggbb` or `#rrggbbaa` (case-insensitive).
    pub fn from_hex(s: &str) -> Result<Self, ColorParseError> {
        let digits = s
            .trim()
            .strip_prefix('#')
            .ok_or(ColorParseError::MissingHash)?;
        if !digits.is_ascii() {
            return Err(ColorParseError::InvalidDigit);
        }
        let len = digits.len();
        if len != 6 && len != 8 {
            return Err(ColorParseError::InvalidLength(len));
        }
        let channel = |i: usize| {
            u8::from_str_radix(&digits[i..i + 2], 16).map_err(|_| ColorParseError::InvalidDigit)
        };
        let a = if len == 8 { channel(6)? } else { 255 };
        Ok(Self::rgba(channel(0)?, channel(2)?, channel(4)?, a))
    }

    /// Formats as `#rrggbb` when opaque, `#rrggbbaa` otherwise (lowercase).
    #[must_use]
    pub fn to_hex(self) -> String {
        if self.a == 255 {
            format!("#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
        } else {
            format!("#{:02x}{:02x}{:02x}{:02x}", self.r, self.g, self.b, self.a)
        }
    }

    /// Alpha as a fraction in `[0, 1]`.
    #[must_use]
    pub fn alpha(self) -> f64 {
        f64::from(self.a) / 255.0
    }

    /// Linear-light sRGB channels in `[0, 1]` (alpha ignored).
    #[must_use]
    pub fn to_linear_rgb(self) -> [f64; 3] {
        [
            srgb_to_linear(self.r),
            srgb_to_linear(self.g),
            srgb_to_linear(self.b),
        ]
    }

    /// Color from linear-light sRGB channels (clipped to `[0, 1]`).
    #[must_use]
    pub fn from_linear_rgb(rgb: [f64; 3], alpha: f64) -> Self {
        Self::rgba(
            linear_to_srgb(rgb[0]),
            linear_to_srgb(rgb[1]),
            linear_to_srgb(rgb[2]),
            unit_to_u8(alpha),
        )
    }

    /// Converts to Oklab.
    #[must_use]
    pub fn to_oklab(self) -> Oklab {
        let [l, a, b] = linear_srgb_to_oklab(self.to_linear_rgb());
        Oklab {
            l,
            a,
            b,
            alpha: self.alpha(),
        }
    }

    /// Converts to OKLCH.
    #[must_use]
    pub fn to_oklch(self) -> Oklch {
        self.to_oklab().to_oklch()
    }

    /// WCAG 2.x relative luminance of the opaque color, in `[0, 1]`.
    #[must_use]
    pub fn relative_luminance(self) -> f64 {
        let [r, g, b] = self.to_linear_rgb();
        0.2126 * r + 0.7152 * g + 0.0722 * b
    }

    /// WCAG 2.x contrast ratio between two opaque colors, in `[1, 21]`.
    /// Alpha is ignored; composite with [`Color::over`] first if needed.
    #[must_use]
    pub fn contrast_ratio(self, other: Self) -> f64 {
        let l1 = self.relative_luminance();
        let l2 = other.relative_luminance();
        let (hi, lo) = if l1 >= l2 { (l1, l2) } else { (l2, l1) };
        (hi + 0.05) / (lo + 0.05)
    }

    /// Alpha-composites `self` over an opaque `background` (in sRGB space,
    /// as browsers and most toolkits do). The result is opaque.
    #[must_use]
    pub fn over(self, background: Self) -> Self {
        let t = self.alpha();
        let blend =
            |f: u8, b: u8| unit_to_u8((f64::from(f) * t + f64::from(b) * (1.0 - t)) / 255.0);
        Self::rgb(
            blend(self.r, background.r),
            blend(self.g, background.g),
            blend(self.b, background.b),
        )
    }

    /// Interpolates towards `other` in Oklab (`t = 0` → `self`, `t = 1` → `other`).
    #[must_use]
    pub fn mix(self, other: Self, t: f64) -> Self {
        let t = t.clamp(0.0, 1.0);
        let (p, q) = (self.to_oklab(), other.to_oklab());
        Oklab {
            l: p.l + (q.l - p.l) * t,
            a: p.a + (q.a - p.a) * t,
            b: p.b + (q.b - p.b) * t,
            alpha: p.alpha + (q.alpha - p.alpha) * t,
        }
        .to_color()
    }

    /// Euclidean distance in Oklab (ΔE<sub>OK</sub>); ~0.02 is a just-noticeable difference.
    #[must_use]
    pub fn delta_e(self, other: Self) -> f64 {
        let (p, q) = (self.to_oklab(), other.to_oklab());
        ((p.l - q.l).powi(2) + (p.a - q.a).powi(2) + (p.b - q.b).powi(2)).sqrt()
    }
}

impl fmt::Display for Color {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl FromStr for Color {
    type Err = ColorParseError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::from_hex(s)
    }
}

impl Serialize for Color {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for Color {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Self::from_hex(&s).map_err(serde::de::Error::custom)
    }
}

/// A color in the Oklab perceptual space.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Oklab {
    /// Perceptual lightness, `0` (black) to `1` (white).
    pub l: f64,
    /// Green–red axis.
    pub a: f64,
    /// Blue–yellow axis.
    pub b: f64,
    /// Alpha in `[0, 1]`.
    pub alpha: f64,
}

impl Oklab {
    /// Polar form.
    #[must_use]
    pub fn to_oklch(self) -> Oklch {
        let c = self.a.hypot(self.b);
        let h = if c < ACHROMATIC_CHROMA {
            0.0
        } else {
            self.b.atan2(self.a).to_degrees().rem_euclid(360.0)
        };
        Oklch {
            l: self.l,
            c,
            h,
            alpha: self.alpha,
        }
    }

    /// Converts to sRGB, gamut-mapped by reducing chroma (see [`Oklch::to_color`]).
    #[must_use]
    pub fn to_color(self) -> Color {
        self.to_oklch().to_color()
    }
}

/// A color in OKLCH: Oklab lightness, chroma and hue angle (degrees).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Oklch {
    /// Perceptual lightness, `0` (black) to `1` (white).
    pub l: f64,
    /// Chroma, `0` (gray) to ~`0.37` (most saturated sRGB colors).
    pub c: f64,
    /// Hue angle in degrees, `[0, 360)`.
    pub h: f64,
    /// Alpha in `[0, 1]`.
    pub alpha: f64,
}

impl Oklch {
    /// Opaque OKLCH color.
    #[must_use]
    pub const fn new(l: f64, c: f64, h: f64) -> Self {
        Self {
            l,
            c,
            h,
            alpha: 1.0,
        }
    }

    /// Cartesian form.
    #[must_use]
    pub fn to_oklab(self) -> Oklab {
        let rad = self.h.to_radians();
        Oklab {
            l: self.l,
            a: self.c * rad.cos(),
            b: self.c * rad.sin(),
            alpha: self.alpha,
        }
    }

    /// Converts to sRGB. Lightness is clamped to `[0, 1]`; colors outside the
    /// sRGB gamut keep their lightness and hue while chroma is reduced
    /// (bisection) until they fit, as in CSS Color 4 gamut mapping.
    #[must_use]
    pub fn to_color(self) -> Color {
        let l = self.l.clamp(0.0, 1.0);
        let c = self.c.max(0.0);
        let rgb_at = |chroma: f64| {
            let lab = Self {
                l,
                c: chroma,
                ..self
            }
            .to_oklab();
            oklab_to_linear_srgb([lab.l, lab.a, lab.b])
        };
        let rgb = rgb_at(c);
        if in_gamut(rgb) {
            return Color::from_linear_rgb(rgb, self.alpha);
        }
        let (mut lo, mut hi) = (0.0, c);
        for _ in 0..GAMUT_SEARCH_STEPS {
            let mid = f64::midpoint(lo, hi);
            if in_gamut(rgb_at(mid)) {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        Color::from_linear_rgb(rgb_at(lo), self.alpha)
    }
}

fn in_gamut(rgb: [f64; 3]) -> bool {
    rgb.iter()
        .all(|&v| (-GAMUT_EPSILON..=1.0 + GAMUT_EPSILON).contains(&v))
}

/// Rounds a `[0, 1]` fraction to an 8-bit channel.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "value is clamped to [0, 255] and rounded before the cast"
)]
fn unit_to_u8(v: f64) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0).round() as u8
}

fn srgb_to_linear(c: u8) -> f64 {
    let c = f64::from(c) / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_to_srgb(c: f64) -> u8 {
    let c = c.clamp(0.0, 1.0);
    let v = if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    };
    unit_to_u8(v)
}

// Matrices from Björn Ottosson, "A perceptual color space for image processing" (2020).
#[allow(
    clippy::unreadable_literal,
    clippy::many_single_char_names,
    reason = "published reference matrix and its conventional channel names"
)]
fn linear_srgb_to_oklab([r, g, b]: [f64; 3]) -> [f64; 3] {
    let l = 0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b;
    let m = 0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b;
    let s = 0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b;
    let (l, m, s) = (l.cbrt(), m.cbrt(), s.cbrt());
    [
        0.2104542553 * l + 0.7936177850 * m - 0.0040720468 * s,
        1.9779984951 * l - 2.4285922050 * m + 0.4505937099 * s,
        0.0259040371 * l + 0.7827717662 * m - 0.8086757660 * s,
    ]
}

#[allow(
    clippy::unreadable_literal,
    clippy::many_single_char_names,
    reason = "published reference matrix and its conventional channel names"
)]
fn oklab_to_linear_srgb([l, a, b]: [f64; 3]) -> [f64; 3] {
    let l_ = l + 0.3963377774 * a + 0.2158037573 * b;
    let m_ = l - 0.1055613458 * a - 0.0638541728 * b;
    let s_ = l - 0.0894841775 * a - 1.2914855480 * b;
    let (l, m, s) = (l_.powi(3), m_.powi(3), s_.powi(3));
    [
        4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s,
        -1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s,
        -0.0041960863 * l - 0.7034186147 * m + 1.7076147010 * s,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_parse_and_format() {
        assert_eq!(Color::from_hex("#FF8000"), Ok(Color::rgb(255, 128, 0)));
        assert_eq!(
            Color::from_hex("#ff800080"),
            Ok(Color::rgba(255, 128, 0, 128))
        );
        assert_eq!(Color::rgb(1, 2, 3).to_hex(), "#010203");
        assert_eq!(Color::rgba(1, 2, 3, 4).to_hex(), "#01020304");
        assert_eq!(Color::from_hex("ff8000"), Err(ColorParseError::MissingHash));
        assert_eq!(
            Color::from_hex("#fff"),
            Err(ColorParseError::InvalidLength(3))
        );
        assert_eq!(
            Color::from_hex("#gg0000"),
            Err(ColorParseError::InvalidDigit)
        );
        assert_eq!(
            Color::from_hex("#é00000"),
            Err(ColorParseError::InvalidDigit)
        );
    }

    #[test]
    fn composite_over() {
        let half = Color::rgba(255, 255, 255, 128);
        let c = half.over(Color::BLACK);
        assert_eq!(c, Color::rgb(128, 128, 128));
    }
}
