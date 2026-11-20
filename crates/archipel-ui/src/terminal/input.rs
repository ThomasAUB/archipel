//! Keys as the bytes a terminal program expects (xterm conventions).

use archipel_model::terminal::TerminalModes;

/// A key press, as the terminal sees it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Key<'a> {
    /// GPUI's key name (`a`, `enter`, `up`, `f5`…).
    pub name: &'a str,
    /// The text the key types, if any (with Shift applied).
    pub typed: Option<&'a str>,
    /// Control.
    pub ctrl: bool,
    /// Alt / Option.
    pub alt: bool,
    /// Shift.
    pub shift: bool,
}

/// The bytes `key` sends, `None` when it sends nothing (a lone modifier, a
/// key the terminal leaves to the app).
#[must_use]
pub fn encode(key: Key<'_>, modes: TerminalModes) -> Option<Vec<u8>> {
    if let Some(bytes) = named(key, modes) {
        return Some(bytes);
    }
    let esc = |mut bytes: Vec<u8>| {
        if key.alt {
            bytes.insert(0, 0x1b);
        }
        bytes
    };
    if key.ctrl {
        return control(key.name, key.shift).map(|b| esc(vec![b]));
    }
    let typed = key.typed.filter(|t| !t.is_empty())?;
    Some(esc(typed.as_bytes().to_vec()))
}

/// Ctrl + `key`: the C0 control character.
fn control(key: &str, shift: bool) -> Option<u8> {
    let mut chars = key.chars();
    let (Some(c), None) = (chars.next(), chars.next()) else {
        return (key == "space").then_some(0);
    };
    match c.to_ascii_lowercase() {
        c @ 'a'..='z' => Some(c as u8 & 0x1f),
        '@' | '2' | ' ' => Some(0),
        '[' | '3' => Some(0x1b),
        '\\' | '4' => Some(0x1c),
        ']' | '5' => Some(0x1d),
        '^' | '6' => Some(0x1e),
        '_' | '7' | '/' => Some(0x1f),
        '-' if shift => Some(0x1f),
        '8' | '?' => Some(0x7f),
        _ => None,
    }
}

/// The xterm modifier parameter (`1 + shift + 2·alt + 4·ctrl`), `None`
/// without modifiers.
fn modifier(key: Key<'_>) -> Option<u8> {
    let m = u8::from(key.shift) + 2 * u8::from(key.alt) + 4 * u8::from(key.ctrl);
    (m != 0).then_some(m + 1)
}

/// Keys that are not text: Enter, arrows, function keys…
fn named(key: Key<'_>, modes: TerminalModes) -> Option<Vec<u8>> {
    let alt = |bytes: &[u8]| {
        let mut out = Vec::with_capacity(bytes.len() + 1);
        if key.alt {
            out.push(0x1b);
        }
        out.extend_from_slice(bytes);
        Some(out)
    };
    // `ESC [ 1 ; m X` with modifiers, else `ESC O X` or `ESC [ X`.
    let letter = |x: u8, application: bool| match modifier(key) {
        Some(m) => Some(format!("\x1b[1;{m}{}", char::from(x)).into_bytes()),
        None if application => Some(vec![0x1b, b'O', x]),
        None => Some(vec![0x1b, b'[', x]),
    };
    // `ESC [ n ; m ~` with modifiers, else `ESC [ n ~`.
    let tilde = |n: u8| match modifier(key) {
        Some(m) => Some(format!("\x1b[{n};{m}~").into_bytes()),
        None => Some(format!("\x1b[{n}~").into_bytes()),
    };
    match key.name {
        "enter" => alt(b"\r"),
        "tab" if key.shift => Some(b"\x1b[Z".to_vec()),
        "tab" => alt(b"\t"),
        "escape" => alt(b"\x1b"),
        "backspace" if key.ctrl => alt(b"\x08"),
        "backspace" => alt(b"\x7f"),
        "up" => letter(b'A', modes.app_cursor),
        "down" => letter(b'B', modes.app_cursor),
        "right" => letter(b'C', modes.app_cursor),
        "left" => letter(b'D', modes.app_cursor),
        "home" => letter(b'H', modes.app_cursor),
        "end" => letter(b'F', modes.app_cursor),
        "insert" => tilde(2),
        "delete" => tilde(3),
        "pageup" => tilde(5),
        "pagedown" => tilde(6),
        "f1" => letter(b'P', true),
        "f2" => letter(b'Q', true),
        "f3" => letter(b'R', true),
        "f4" => letter(b'S', true),
        "f5" => tilde(15),
        "f6" => tilde(17),
        "f7" => tilde(18),
        "f8" => tilde(19),
        "f9" => tilde(20),
        "f10" => tilde(21),
        "f11" => tilde(23),
        "f12" => tilde(24),
        "space" if !key.ctrl => alt(b" "),
        _ => None,
    }
}

/// Text pasted into the terminal: line breaks as Enter, wrapped in
/// bracketed-paste markers when the program asked for them (so a shell
/// does not run each pasted line).
#[must_use]
pub fn paste(text: &str, modes: TerminalModes) -> Vec<u8> {
    let text = text.replace("\r\n", "\r").replace('\n', "\r");
    if modes.bracketed_paste {
        // A pasted end marker would end the paste early.
        let text = text.replace("\x1b[201~", "");
        format!("\x1b[200~{text}\x1b[201~").into_bytes()
    } else {
        text.into_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(name: &str) -> Key<'_> {
        Key {
            name,
            ..Key::default()
        }
    }

    fn plain(k: Key<'_>) -> Option<Vec<u8>> {
        encode(k, TerminalModes::default())
    }

    #[test]
    fn text_and_alt_text() {
        let a = Key {
            typed: Some("a"),
            ..key("a")
        };
        assert_eq!(plain(a), Some(b"a".to_vec()));
        assert_eq!(plain(Key { alt: true, ..a }), Some(b"\x1ba".to_vec()));
        let e = Key {
            typed: Some("é"),
            ..key("é")
        };
        assert_eq!(plain(e), Some("é".as_bytes().to_vec()));
        // A lone modifier types nothing.
        assert_eq!(plain(key("shift")), None);
    }

    #[test]
    fn control_characters() {
        let ctrl = |k: &'static str| Key {
            ctrl: true,
            ..key(k)
        };
        assert_eq!(plain(ctrl("c")), Some(vec![3]));
        assert_eq!(plain(ctrl("z")), Some(vec![0x1a]));
        assert_eq!(plain(ctrl("[")), Some(vec![0x1b]));
        assert_eq!(plain(ctrl("space")), Some(vec![0]));
        assert_eq!(
            plain(Key {
                alt: true,
                ..ctrl("b")
            }),
            Some(vec![0x1b, 2])
        );
        assert_eq!(plain(ctrl("f13")), None);
    }

    #[test]
    fn cursor_keys_follow_the_mode_and_modifiers() {
        assert_eq!(plain(key("up")), Some(b"\x1b[A".to_vec()));
        let app = TerminalModes {
            app_cursor: true,
            ..TerminalModes::default()
        };
        assert_eq!(encode(key("up"), app), Some(b"\x1bOA".to_vec()));
        let ctrl_right = Key {
            ctrl: true,
            ..key("right")
        };
        assert_eq!(encode(ctrl_right, app), Some(b"\x1b[1;5C".to_vec()));
        assert_eq!(
            plain(Key {
                shift: true,
                ..key("delete")
            }),
            Some(b"\x1b[3;2~".to_vec())
        );
        assert_eq!(plain(key("f1")), Some(b"\x1bOP".to_vec()));
        assert_eq!(plain(key("f12")), Some(b"\x1b[24~".to_vec()));
    }

    #[test]
    fn editing_keys() {
        assert_eq!(plain(key("enter")), Some(b"\r".to_vec()));
        assert_eq!(plain(key("backspace")), Some(vec![0x7f]));
        assert_eq!(
            plain(Key {
                shift: true,
                ..key("tab")
            }),
            Some(b"\x1b[Z".to_vec())
        );
        assert_eq!(plain(key("escape")), Some(vec![0x1b]));
    }

    #[test]
    fn pastes_are_bracketed_when_asked() {
        let modes = TerminalModes::default();
        assert_eq!(paste("a\r\nb\n", modes), b"a\rb\r".to_vec());
        let bracketed = TerminalModes {
            bracketed_paste: true,
            ..modes
        };
        assert_eq!(
            paste("rm x\x1b[201~", bracketed),
            b"\x1b[200~rm x\x1b[201~".to_vec()
        );
    }
}
