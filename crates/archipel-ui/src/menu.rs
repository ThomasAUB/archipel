//! The macOS application menu.
//!
//! A Mac app without one has no Quit (Cmd+Q), Hide or Services. Linux and
//! Windows windows have no menu bar: everything stays in the action palette.

use gpui_kit::{App, KeyBinding, Menu, MenuItem, SystemMenuType};

use crate::workspace::NewWindow;

gpui_kit::actions!(archipel, [Quit, Hide, HideOthers, ShowAll]);

/// Sets the menu bar and its actions (macOS only).
pub fn install(cx: &mut App) {
    if !cfg!(target_os = "macos") {
        return;
    }
    cx.on_action(|_: &Quit, cx| cx.quit());
    cx.on_action(|_: &Hide, cx| cx.hide());
    cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
    cx.on_action(|_: &ShowAll, cx| cx.unhide_other_apps());
    cx.bind_keys([
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("cmd-alt-h", HideOthers, None),
    ]);
    cx.set_menus([Menu::new("Archipel").items([
        MenuItem::action("New Window", NewWindow),
        MenuItem::separator(),
        MenuItem::os_submenu("Services", SystemMenuType::Services),
        MenuItem::separator(),
        MenuItem::action("Hide Archipel", Hide),
        MenuItem::action("Hide Others", HideOthers),
        MenuItem::action("Show All", ShowAll),
        MenuItem::separator(),
        MenuItem::action("Quit Archipel", Quit),
    ])]);
}
