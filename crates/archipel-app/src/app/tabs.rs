//! Tab commands (open, close, cycle, move) applied to the UI state. Tab 0,
//! the dashboard, never closes or moves.

use archipel_model::ui_state::{TabState, UiState};

use crate::actions::Command;

/// Applies a tab command (open, close, cycle, move) to the UI state.
pub(super) fn tab_command(ui: &mut UiState, command: &Command) {
    match *command {
        Command::OpenRepoTab(repo) => {
            let tab = TabState::Repo(repo);
            ui.active_tab = ui.tabs.iter().position(|t| *t == tab).unwrap_or_else(|| {
                ui.tabs.push(tab);
                ui.tabs.len() - 1
            });
        }
        Command::CloseTab(index) => {
            if index > 0 && index < ui.tabs.len() {
                ui.tabs.remove(index);
                if ui.active_tab >= index {
                    ui.active_tab = ui.active_tab.saturating_sub(1);
                }
            }
        }
        Command::CloseOtherTabs(index) => {
            if index < ui.tabs.len() {
                ui.active_tab = index;
                close_tabs(ui, |i| i == index, index);
            }
        }
        Command::CloseTabsToRight(index) => close_tabs(ui, |i| i <= index, index),
        Command::CloseAllTabs => close_tabs(ui, |_| false, 0),
        Command::CycleTab(forward) => {
            let n = ui.tabs.len().max(1);
            ui.active_tab = if forward {
                (ui.active_tab + 1) % n
            } else {
                (ui.active_tab + n - 1) % n
            };
        }
        Command::MoveTab { from, to } => {
            let len = ui.tabs.len();
            if from == 0 || from >= len || from == to {
                return;
            }
            let to = to.clamp(1, len - 1);
            let active = ui.tabs.get(ui.active_tab).cloned();
            let tab = ui.tabs.remove(from);
            ui.tabs.insert(to, tab);
            ui.active_tab = active
                .and_then(|t| ui.tabs.iter().position(|x| *x == t))
                .unwrap_or(0);
        }
        _ => {}
    }
}

/// Closes the tabs whose index fails `keep` (never the dashboard). The active
/// tab stays active when kept; otherwise tab `focus` becomes active when kept,
/// else the dashboard.
fn close_tabs(ui: &mut UiState, keep: impl Fn(usize) -> bool, focus: usize) {
    let active = ui.tabs.get(ui.active_tab).cloned();
    let focus = ui.tabs.get(focus).cloned();
    let mut i = 0;
    ui.tabs.retain(|_| {
        let kept = i == 0 || keep(i);
        i += 1;
        kept
    });
    let position = |tab: Option<TabState>| tab.and_then(|t| ui.tabs.iter().position(|x| *x == t));
    ui.active_tab = position(active).or_else(|| position(focus)).unwrap_or(0);
}
