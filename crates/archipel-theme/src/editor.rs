//! Pure logic behind the in-app theme editor: contrast warnings, per-token
//! reset and "duplicate and edit".

use crate::color::{WCAG_AA_LARGE_TEXT, WCAG_AA_NORMAL_TEXT};
use crate::theme::Theme;
use crate::token::{ColorSet, ColorToken};

/// A foreground/background token pair that must meet a minimum contrast.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ContrastPair {
    /// Foreground token.
    pub fg: ColorToken,
    /// Background token it is drawn on.
    pub bg: ColorToken,
    /// Required WCAG contrast ratio.
    pub min_ratio: f64,
}

const fn pair(fg: ColorToken, bg: ColorToken, min_ratio: f64) -> ContrastPair {
    ContrastPair { fg, bg, min_ratio }
}

const NORMAL: f64 = WCAG_AA_NORMAL_TEXT;
const LARGE: f64 = WCAG_AA_LARGE_TEXT;

/// Pairs the editor checks: normal text at 4.5:1, large text / UI
/// components / graphics at 3:1 (WCAG 2.x AA).
pub const CONTRAST_PAIRS: &[ContrastPair] = {
    use ColorToken as T;
    &[
        pair(T::FgDefault, T::BgApp, NORMAL),
        pair(T::FgDefault, T::BgPanel, NORMAL),
        pair(T::FgDefault, T::BgElevated, NORMAL),
        pair(T::FgDefault, T::BgInput, NORMAL),
        pair(T::FgDefault, T::BgHover, NORMAL),
        pair(T::FgDefault, T::BgSelected, NORMAL),
        pair(T::FgMuted, T::BgApp, NORMAL),
        pair(T::FgMuted, T::BgPanel, NORMAL),
        pair(T::FgSubtle, T::BgApp, LARGE),
        pair(T::FgSubtle, T::BgPanel, LARGE),
        pair(T::AccentFg, T::Accent, NORMAL),
        pair(T::Accent, T::BgApp, LARGE),
        pair(T::BorderFocus, T::BgApp, LARGE),
        pair(T::StatusDirty, T::BgApp, NORMAL),
        pair(T::StatusAhead, T::BgApp, NORMAL),
        pair(T::StatusBehind, T::BgApp, NORMAL),
        pair(T::StatusConflict, T::BgApp, NORMAL),
        pair(T::StatusSuccess, T::BgApp, NORMAL),
        pair(T::StatusError, T::BgApp, NORMAL),
        pair(T::StatusWarning, T::BgApp, NORMAL),
        pair(T::FgDefault, T::DiffAddBg, NORMAL),
        pair(T::FgDefault, T::DiffDelBg, NORMAL),
        pair(T::FgDefault, T::DiffAddWordBg, NORMAL),
        pair(T::FgDefault, T::DiffDelWordBg, NORMAL),
        pair(T::DiffAddFg, T::DiffAddBg, NORMAL),
        pair(T::DiffDelFg, T::DiffDelBg, NORMAL),
        pair(T::GraphLane0, T::BgApp, LARGE),
        pair(T::GraphLane1, T::BgApp, LARGE),
        pair(T::GraphLane2, T::BgApp, LARGE),
        pair(T::GraphLane3, T::BgApp, LARGE),
        pair(T::GraphLane4, T::BgApp, LARGE),
        pair(T::GraphLane5, T::BgApp, LARGE),
        pair(T::GraphLane6, T::BgApp, LARGE),
        pair(T::GraphLane7, T::BgApp, LARGE),
        pair(T::GraphLane8, T::BgApp, LARGE),
        pair(T::GraphLane9, T::BgApp, LARGE),
        pair(T::SyntaxKeyword, T::BgApp, NORMAL),
        pair(T::SyntaxString, T::BgApp, NORMAL),
        pair(T::SyntaxComment, T::BgApp, NORMAL),
        pair(T::SyntaxFunction, T::BgApp, NORMAL),
        pair(T::SyntaxType, T::BgApp, NORMAL),
        pair(T::SyntaxNumber, T::BgApp, NORMAL),
        pair(T::SyntaxConstant, T::BgApp, NORMAL),
        pair(T::SyntaxOperator, T::BgApp, NORMAL),
    ]
};

/// A pair below its required contrast.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ContrastWarning {
    /// Foreground token.
    pub fg: ColorToken,
    /// Background token.
    pub bg: ColorToken,
    /// Actual contrast ratio (foreground composited over background).
    pub ratio: f64,
    /// Required ratio.
    pub required: f64,
}

/// Contrast ratio of `pair` in `colors` (foreground alpha-composited over an
/// opaque background).
#[must_use]
pub fn pair_ratio(colors: &ColorSet, pair: &ContrastPair) -> f64 {
    let bg = colors.get(pair.bg).with_alpha(255);
    colors.get(pair.fg).over(bg).contrast_ratio(bg)
}

/// Every [`CONTRAST_PAIRS`] entry below its WCAG AA target.
#[must_use]
pub fn contrast_warnings(colors: &ColorSet) -> Vec<ContrastWarning> {
    CONTRAST_PAIRS
        .iter()
        .filter_map(|p| {
            let ratio = pair_ratio(colors, p);
            (ratio < p.min_ratio).then_some(ContrastWarning {
                fg: p.fg,
                bg: p.bg,
                ratio,
                required: p.min_ratio,
            })
        })
        .collect()
}

/// Resets one token to what the parent's rules give: the parent's override
/// for it if it has one, otherwise the value derived from `theme`'s base.
pub fn reset_token(theme: &mut Theme, parent: &Theme, token: ColorToken) {
    match parent.overrides().get(&token) {
        Some(&c) => theme.set_token(token, c),
        None => theme.clear_override(token),
    }
}

/// Resets every token (keeps base colors and settings).
pub fn reset_all_tokens(theme: &mut Theme, parent: &Theme) {
    theme.set_overrides(parent.overrides().clone());
}

/// "Duplicate and edit": a copy of `source` under a new id and name that
/// extends `source`, so exporting it initially yields just name + extends.
#[must_use]
pub fn duplicate(source: &Theme, new_id: impl Into<String>, new_name: impl Into<String>) -> Theme {
    let mut copy = source.clone();
    copy.id = new_id.into();
    copy.name = new_name.into();
    copy.extends = Some(source.id.clone());
    copy
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builtin;
    use crate::color::Color;

    #[test]
    fn low_contrast_override_is_reported_and_reset_clears_it() {
        let parent = builtin::dark();
        let mut t = duplicate(&parent, "mine", "Mine");
        assert!(contrast_warnings(t.colors()).is_empty());
        t.set_token(ColorToken::FgDefault, t.color(ColorToken::BgApp));
        let w = contrast_warnings(t.colors());
        assert!(
            w.iter()
                .any(|w| w.fg == ColorToken::FgDefault && w.bg == ColorToken::BgApp)
        );
        reset_token(&mut t, &parent, ColorToken::FgDefault);
        assert_eq!(t.colors(), parent.colors());
        t.set_token(ColorToken::Accent, Color::rgb(9, 9, 9));
        reset_all_tokens(&mut t, &parent);
        assert_eq!(t.colors(), parent.colors());
    }
}
