//! Keymap keystrokes as GPUI keystrokes, and as the labels shown for them.
//!
//! The keymap (`archipel-app`) has one primary modifier, written `ctrl`. On
//! macOS it is Command, as in every Mac app, except where macOS keeps
//! Command for itself ([`keeps_control`]): those stay on Control there.
//!
//! Every shortcut shown in the UI comes from [`label_for`] (a keymap
//! action, rebindable) or [`fixed_label`] (a key a view handles itself), so
//! it matches the key that works, on every OS.

use archipel_app::keymap::{Keymap, Keystroke};

/// The GPUI form of `stroke` on this OS.
pub fn gpui_keystroke(stroke: &Keystroke) -> String {
    for_os(stroke, MAC)
}

/// How `stroke` is shown on this OS: `Ctrl+Shift+P`, or `⇧⌘P` on macOS.
pub fn label(stroke: &Keystroke) -> String {
    label_on(stroke, MAC)
}

/// The label of the key bound to action `id` (the first one when several
/// are), or `None` when the user's keymap unbound it.
pub fn label_for(keymap: &Keymap, id: &str) -> Option<String> {
    keymap.keys_for(id).first().map(|k| label(k))
}

/// The label of a key a view handles itself rather than through the
/// keymap, written as in the keymap (`ctrl-enter`).
pub fn fixed_label(text: &str) -> String {
    Keystroke::parse(text).map_or_else(|_| text.to_owned(), |k| label(&k))
}

const MAC: bool = cfg!(target_os = "macos");

/// Whether `stroke` keeps Control on macOS rather than becoming Command,
/// as macOS keeps the Command form for itself: Cmd+Tab switches
/// applications, Cmd+H and Cmd+Option+H hide this app and the others
/// (the application menu, see `menu`).
fn keeps_control(stroke: &Keystroke) -> bool {
    stroke.key == "tab" || stroke.key == "h" && !stroke.shift
}

fn for_os(stroke: &Keystroke, mac: bool) -> String {
    let text = stroke.to_string();
    match text.strip_prefix("ctrl-") {
        Some(rest) if mac && !keeps_control(stroke) => format!("cmd-{rest}"),
        _ => text,
    }
}

fn label_on(stroke: &Keystroke, mac: bool) -> String {
    let key = key_name(&stroke.key);
    if mac {
        // In the macOS order: Control, Option, Shift, Command.
        let command = stroke.ctrl && !keeps_control(stroke);
        let mut out = String::new();
        for (on, glyph) in [
            (stroke.ctrl && !command, '⌃'),
            (stroke.alt, '⌥'),
            (stroke.shift, '⇧'),
            (command, '⌘'),
        ] {
            if on {
                out.push(glyph);
            }
        }
        out + &key
    } else {
        let mut parts = Vec::new();
        for (on, name) in [
            (stroke.ctrl, "Ctrl"),
            (stroke.alt, "Alt"),
            (stroke.shift, "Shift"),
        ] {
            if on {
                parts.push(name.to_owned());
            }
        }
        parts.push(key);
        parts.join("+")
    }
}

/// A key as written on the keyboard: `P`, `Enter`, `F5`, `Esc`.
fn key_name(key: &str) -> String {
    match key {
        "escape" => "Esc".to_owned(),
        "pageup" => "Page Up".to_owned(),
        "pagedown" => "Page Down".to_owned(),
        "up" => "↑".to_owned(),
        "down" => "↓".to_owned(),
        "left" => "←".to_owned(),
        "right" => "→".to_owned(),
        _ => {
            let mut chars = key.chars();
            chars.next().map_or_else(String::new, |first| {
                first.to_uppercase().chain(chars).collect()
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn on(text: &str, mac: bool) -> String {
        for_os(&Keystroke::parse(text).unwrap(), mac)
    }

    fn shown(text: &str, mac: bool) -> String {
        label_on(&Keystroke::parse(text).unwrap(), mac)
    }

    #[test]
    fn control_becomes_command_on_macos() {
        assert_eq!(on("ctrl-shift-p", true), "cmd-shift-p");
        assert_eq!(on("ctrl-alt-k", true), "cmd-alt-k");
        assert_eq!(on("ctrl-shift-p", false), "ctrl-shift-p");
    }

    #[test]
    fn tab_switching_keeps_control_on_macos() {
        assert_eq!(on("ctrl-tab", true), "ctrl-tab");
        assert_eq!(on("ctrl-shift-tab", true), "ctrl-shift-tab");
    }

    #[test]
    fn hiding_keeps_control_on_macos() {
        // Cmd+H hides the app, Cmd+Shift+H is free.
        assert_eq!(on("ctrl-h", true), "ctrl-h");
        assert_eq!(on("ctrl-alt-h", true), "ctrl-alt-h");
        assert_eq!(on("ctrl-shift-h", true), "cmd-shift-h");
        assert_eq!(on("ctrl-h", false), "ctrl-h");
    }

    #[test]
    fn keys_without_control_are_unchanged() {
        assert_eq!(on("f5", true), "f5");
        assert_eq!(on("alt-shift-d", true), "alt-shift-d");
    }

    #[test]
    fn labels_read_as_on_the_keyboard() {
        assert_eq!(shown("ctrl-shift-p", false), "Ctrl+Shift+P");
        assert_eq!(shown("ctrl-enter", false), "Ctrl+Enter");
        assert_eq!(shown("f5", false), "F5");
        assert_eq!(shown("ctrl-`", false), "Ctrl+`");
    }

    #[test]
    fn labels_follow_the_macos_key_mapping() {
        assert_eq!(shown("ctrl-shift-p", true), "⇧⌘P");
        assert_eq!(shown("ctrl-j", true), "⌘J");
        assert_eq!(shown("ctrl-h", true), "⌃H");
        // Tab switching stays on Control.
        assert_eq!(shown("ctrl-shift-tab", true), "⌃⇧Tab");
        assert_eq!(shown("alt-shift-d", true), "⌥⇧D");
    }

    #[test]
    fn labels_come_from_the_keymap() {
        let keymap = Keymap::defaults();
        assert_eq!(
            label_for(&keymap, "drawer.toggle").map(|l| l.replace('⌘', "Ctrl+")),
            Some("Ctrl+J".to_owned())
        );
        let (unbound, _) = keymap.with_overrides("\"ctrl-j\" = \"\"");
        assert_eq!(label_for(&unbound, "drawer.toggle"), None);
    }
}
