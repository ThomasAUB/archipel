//! The conflict forecast badge (spec §3): "will conflict · 2" (or
//! "touches 2 modified") as a small pill, the files on hover. Drawn in the
//! dashboard's Sync column, the Quick Edit Header and the Repo Toolbar.

use archipel_app::dashboard::CellBadge;
use archipel_app::forecast::ConflictForecast;
use archipel_model::RepoSummary;
use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, Div, ElementId, div};

use crate::theme::{radius, tone};
use crate::widgets::TextTooltip;

/// A badge as a pill: its text in its tone, framed in the same tone.
pub fn pill(badge: &CellBadge, cx: &gpui_kit::App) -> Div {
    let fg = tone(cx, badge.tone);
    div()
        .flex_none()
        .px_1()
        .rounded(radius(cx))
        .border_1()
        .border_color(fg.opacity(0.6))
        .bg(fg.opacity(0.12))
        .text_xs()
        .text_color(fg)
        .whitespace_nowrap()
        .child(badge.text.clone())
}

/// The forecast badge of a repo with its files on hover; `None` when there
/// is nothing to show.
pub fn forecast_badge(
    id: impl Into<ElementId>,
    forecast: &ConflictForecast,
    summary: Option<&RepoSummary>,
    cx: &gpui_kit::App,
) -> Option<AnyElement> {
    let badge = forecast.badge(summary)?;
    let lines = forecast.tooltip(summary);
    Some(
        pill(&badge, cx)
            .id(id.into())
            .tooltip(move |_, cx| TextTooltip::view(&lines, cx))
            .into_any_element(),
    )
}
