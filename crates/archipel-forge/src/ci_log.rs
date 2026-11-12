//! Error excerpts of CI job logs (spec §8): the end of the log is kept, and
//! reduced to the lines that look like errors with a few lines around them.
//!
//! The rules are simple on purpose (lines with `error`, `FAILED`,
//! `panicked`, a non-zero exit code); a provider can replace them through
//! [`crate::provider::CiApi::log_excerpt`]. When no line matches, the raw
//! end of the log is the excerpt.

use archipel_model::forge::CiJobLog;

/// Bytes kept from the end of a log.
pub const TAIL_BYTES: usize = 64 * 1024;

/// Most lines in an excerpt, gaps included.
pub const MAX_EXCERPT_LINES: usize = 30;

/// Lines kept before an error line.
const BEFORE: usize = 2;

/// Lines kept after an error line.
const AFTER: usize = 3;

/// Lines of the raw end of the log when no line looks like an error.
const FALLBACK_LINES: usize = 15;

/// Longer lines are cut.
const MAX_LINE_CHARS: usize = 200;

/// The last `bytes` of `log`, starting on a whole line.
#[must_use]
pub fn tail(log: &str, bytes: usize) -> &str {
    if log.len() <= bytes {
        return log;
    }
    let mut start = log.len() - bytes;
    while !log.is_char_boundary(start) {
        start += 1;
    }
    let rest = &log[start..];
    // The first line is most likely cut: start after it.
    rest.find('\n').map_or(rest, |i| &rest[i + 1..])
}

/// Removes ANSI escape sequences (colors, cursor moves).
fn strip_ansi(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        match chars.next() {
            // CSI: parameters, then one final byte in `@`..=`~`.
            Some('[') => {
                for c in chars.by_ref() {
                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }
            }
            // OSC: up to BEL or ST (`ESC \`).
            Some(']') => {
                while let Some(c) = chars.next() {
                    if c == '\u{7}' {
                        break;
                    }
                    if c == '\u{1b}' && chars.peek() == Some(&'\\') {
                        chars.next();
                        break;
                    }
                }
            }
            // Any other two-character sequence.
            _ => {}
        }
    }
    out
}

/// Whether `word` is a GitHub log timestamp (`2024-05-01T10:00:00.1234567Z`).
fn is_timestamp(word: &str) -> bool {
    let b = word.as_bytes();
    b.len() >= 20
        && b[..4].iter().all(u8::is_ascii_digit)
        && b[4] == b'-'
        && b[10] == b'T'
        && word.ends_with('Z')
}

/// One log line as shown: no colors, no timestamp, no overwritten progress
/// text, forge markers turned into text. `None` for lines that carry
/// nothing (empty, end of a group).
#[must_use]
pub fn clean_line(raw: &str) -> Option<String> {
    let raw = raw.trim_end_matches(['\r', '\n']);
    // A carriage return rewrites the line (progress bars, GitLab section
    // markers): what follows the last one is what shows.
    let raw = raw.rsplit('\r').next().unwrap_or(raw);
    let line = strip_ansi(raw);
    let line = match line.split_once(' ') {
        Some((first, rest)) if is_timestamp(first) => rest.to_owned(),
        _ if is_timestamp(line.trim_end()) => String::new(),
        _ => line,
    };
    let line = if let Some(rest) = line.strip_prefix("##[") {
        let (marker, text) = rest.split_once(']').unwrap_or((rest, ""));
        match marker {
            "endgroup" => String::new(),
            "error" => format!("Error: {text}"),
            "warning" => format!("Warning: {text}"),
            _ => text.to_owned(),
        }
    } else {
        line
    };
    let line = line.trim_end();
    if line.trim().is_empty() {
        return None;
    }
    Some(if line.chars().count() > MAX_LINE_CHARS {
        let mut cut: String = line.chars().take(MAX_LINE_CHARS).collect();
        cut.push('…');
        cut
    } else {
        line.to_owned()
    })
}

/// The number after `key` in `lower` (`exit code: 2` → 2), if any.
fn number_after(lower: &str, key: &str) -> Option<u64> {
    let start = lower.find(key)? + key.len();
    let rest = lower[start..].trim_start_matches([' ', ':', '=', '(']);
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// Whether a cleaned line looks like an error.
#[must_use]
pub fn is_error_line(line: &str) -> bool {
    if line.contains("FAILED") || line.contains("panicked") {
        return true;
    }
    let lower = line.to_ascii_lowercase();
    for key in ["exit code", "exit status", "exited with"] {
        if let Some(code) = number_after(&lower, key) {
            return code != 0;
        }
    }
    // `error` as the start of a word (`error:`, `Error`, `errors`), not
    // inside one (`stderror`), and not a count of none (`0 errors`).
    lower.match_indices("error").any(|(i, _)| {
        let before = lower[..i].chars().next_back();
        let word_start = before.is_none_or(|c| !c.is_ascii_alphanumeric() && c != '_');
        let none = lower[..i].trim_end().ends_with(" 0") || lower[..i].trim_end() == "0";
        word_start && !none
    })
}

/// The excerpt of a log (the end of it): the error lines with
/// [`BEFORE`] / [`AFTER`] lines around them, the last groups first kept
/// within [`MAX_EXCERPT_LINES`]; else the last lines of the log.
#[must_use]
pub fn excerpt(log: &str) -> CiJobLog {
    let lines: Vec<String> = log.lines().filter_map(clean_line).collect();
    // Windows around the error lines, merged when they touch.
    let mut groups: Vec<(usize, usize)> = Vec::new();
    for (i, _) in lines.iter().enumerate().filter(|(_, l)| is_error_line(l)) {
        let (from, to) = (i.saturating_sub(BEFORE), (i + AFTER).min(lines.len() - 1));
        match groups.last_mut() {
            Some(last) if from <= last.1 + 1 => last.1 = to,
            _ => groups.push((from, to)),
        }
    }
    if groups.is_empty() {
        let from = lines.len().saturating_sub(FALLBACK_LINES);
        return CiJobLog {
            excerpt: lines[from..].to_vec(),
            matched: false,
            url: None,
        };
    }
    // The last errors are the closest to the failure: keep groups from the
    // end while they fit.
    let mut kept: Vec<&[String]> = Vec::new();
    let mut used = 0;
    for &(from, to) in groups.iter().rev() {
        let size = to - from + 1;
        let gap = usize::from(!kept.is_empty());
        if used + gap + size > MAX_EXCERPT_LINES {
            if kept.is_empty() {
                // One group larger than the budget: its end.
                kept.push(&lines[to + 1 - MAX_EXCERPT_LINES..=to]);
            }
            break;
        }
        used += gap + size;
        kept.push(&lines[from..=to]);
    }
    let mut excerpt = Vec::with_capacity(used);
    for (k, group) in kept.iter().rev().enumerate() {
        if k > 0 {
            excerpt.push(CiJobLog::GAP.to_owned());
        }
        excerpt.extend(group.iter().cloned());
    }
    CiJobLog {
        excerpt,
        matched: true,
        url: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `"{prefix} {i}\n"` for each `i` of `range`.
    fn numbered(prefix: &str, range: std::ops::Range<usize>) -> String {
        range
            .map(|i| format!("{prefix} {i}\n"))
            .collect::<Vec<_>>()
            .concat()
    }

    #[test]
    fn cleans_forge_markup() {
        assert_eq!(
            clean_line("2024-05-01T10:00:00.1234567Z \u{1b}[31merror\u{1b}[0m: boom\r").as_deref(),
            Some("error: boom")
        );
        assert_eq!(
            clean_line("2024-05-01T10:00:00.1234567Z ##[error]Process completed with exit code 1.")
                .as_deref(),
            Some("Error: Process completed with exit code 1.")
        );
        assert_eq!(
            clean_line("2024-05-01T10:00:00.1234567Z ##[endgroup]"),
            None
        );
        assert_eq!(
            clean_line("##[group]Run cargo test").as_deref(),
            Some("Run cargo test")
        );
        // GitLab sections and progress overwritten by `\r`.
        assert_eq!(
            clean_line("section_start:1700000000:build\r\u{1b}[0KBuilding").as_deref(),
            Some("Building")
        );
        assert_eq!(clean_line("section_end:1700000000:build\r\u{1b}[0K"), None);
        assert_eq!(clean_line("10%\r50%\r100%").as_deref(), Some("100%"));
        assert_eq!(clean_line("   "), None);
        let long = "x".repeat(300);
        assert_eq!(
            clean_line(&long).unwrap().chars().count(),
            MAX_LINE_CHARS + 1
        );
    }

    #[test]
    fn recognizes_error_lines() {
        for yes in [
            "error[E0308]: mismatched types",
            "Error: Process completed with exit code 1.",
            "test tests::it_works ... FAILED",
            "thread 'main' panicked at src/main.rs:2:5",
            "make: *** [all] Error 2",
            "ERROR: Job failed: exit code 2",
            "npm ERR! code ELIFECYCLE; command exited with 1",
            "2 errors found",
        ] {
            assert!(is_error_line(yes), "{yes}");
        }
        for no in [
            "Compiling foo v0.1.0",
            "finished with 0 errors",
            "Process completed with exit code 0.",
            "reading stderror.log",
            "test result: ok. 3 passed; 0 failed",
        ] {
            assert!(!is_error_line(no), "{no}");
        }
    }

    #[test]
    fn excerpt_keeps_errors_with_context() {
        let log = [
            numbered("step", 0..20),
            "error: first\n".into(),
            numbered("step", 20..40),
            "error: second\nstep 40\n".into(),
        ]
        .concat();
        let e = excerpt(&log);
        assert!(e.matched);
        assert_eq!(
            e.excerpt,
            [
                "step 18",
                "step 19",
                "error: first",
                "step 20",
                "step 21",
                "step 22",
                "…",
                "step 38",
                "step 39",
                "error: second",
                "step 40"
            ]
        );
    }

    #[test]
    fn excerpt_keeps_the_last_errors_within_budget() {
        let log = numbered("error", 0..100);
        let e = excerpt(&log);
        assert_eq!(e.excerpt.len(), MAX_EXCERPT_LINES);
        assert_eq!(e.excerpt.last().map(String::as_str), Some("error 99"));
    }

    #[test]
    fn excerpt_falls_back_to_the_end_of_the_log() {
        let log = numbered("line", 0..40);
        let e = excerpt(&log);
        assert!(!e.matched);
        assert_eq!(e.excerpt.len(), FALLBACK_LINES);
        assert_eq!(e.excerpt[0], "line 25");
        assert!(excerpt("").excerpt.is_empty());
    }

    #[test]
    fn tail_starts_on_a_whole_line() {
        let log = "aaaa\nbbbb\ncccc\n";
        assert_eq!(tail(log, 100), log);
        assert_eq!(tail(log, 7), "cccc\n");
        // Never in the middle of a character.
        assert_eq!(tail("ééé\nx", 4), "x");
    }
}
