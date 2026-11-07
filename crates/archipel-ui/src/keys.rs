//! Keymap keystrokes as GPUI keystrokes.
//!
//! The keymap (`archipel-app`) has one primary modifier, written `ctrl`. On
//! macOS it is Command, as in every Mac app, except before Tab: Cmd+Tab
//! switches applications, so tab switching stays on Control there.

use archipel_app::keymap::Keystroke;

/// The GPUI form of `stroke` on this OS (also the label shown for it).
pub fn gpui_keystroke(stroke: &Keystroke) -> String {
    for_os(stroke, cfg!(target_os = "macos"))
}

fn for_os(stroke: &Keystroke, mac: bool) -> String {
    let text = stroke.to_string();
    match text.strip_prefix("ctrl-") {
        Some(rest) if mac && stroke.key != "tab" => format!("cmd-{rest}"),
        _ => text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn on(text: &str, mac: bool) -> String {
        for_os(&Keystroke::parse(text).unwrap(), mac)
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
    fn keys_without_control_are_unchanged() {
        assert_eq!(on("f5", true), "f5");
        assert_eq!(on("alt-shift-d", true), "alt-shift-d");
    }
}
