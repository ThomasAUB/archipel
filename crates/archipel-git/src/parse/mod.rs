//! Pure parsers of `git` CLI outputs. No I/O: every function takes text or
//! bytes and returns model types, so they are unit-tested on recorded outputs.

pub mod diff;
pub mod numstat;
pub mod progress;
pub mod refs;
pub mod status;
pub mod submodule;

/// Removes C-style quoting from a path printed by Git (`"a\tb"` → `a<TAB>b`).
///
/// Paths are only quoted when they contain special characters; unquoted
/// input is returned unchanged.
#[must_use]
pub fn unquote_path(raw: &str) -> String {
    let Some(inner) = raw.strip_prefix('"').and_then(|s| s.strip_suffix('"')) else {
        return raw.to_owned();
    };
    let mut bytes = Vec::with_capacity(inner.len());
    let mut it = inner.bytes().peekable();
    while let Some(b) = it.next() {
        if b != b'\\' {
            bytes.push(b);
            continue;
        }
        match it.next() {
            Some(b'n') => bytes.push(b'\n'),
            Some(b't') => bytes.push(b'\t'),
            Some(b'r') => bytes.push(b'\r'),
            Some(b'a') => bytes.push(7),
            Some(b'b') => bytes.push(8),
            Some(b'f') => bytes.push(12),
            Some(b'v') => bytes.push(11),
            Some(d @ b'0'..=b'7') => {
                let mut value = u32::from(d - b'0');
                for _ in 0..2 {
                    match it.peek() {
                        Some(n @ b'0'..=b'7') => {
                            value = value * 8 + u32::from(n - b'0');
                            it.next();
                        }
                        _ => break,
                    }
                }
                bytes.push(u8::try_from(value).unwrap_or(b'?'));
            }
            Some(other) => bytes.push(other),
            None => bytes.push(b'\\'),
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

#[cfg(test)]
mod tests {
    use super::unquote_path;

    #[test]
    fn unquotes_c_style_paths() {
        assert_eq!(unquote_path("plain.txt"), "plain.txt");
        assert_eq!(unquote_path(r#""a\tb""#), "a\tb");
        assert_eq!(unquote_path(r#""q\"uote""#), "q\"uote");
        // UTF-8 "é" escaped as octal bytes.
        assert_eq!(unquote_path(r#""caf\303\251""#), "café");
    }
}
