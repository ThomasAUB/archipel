//! Parser of `git --progress` lines written on stderr.

use archipel_model::Progress;

/// Parses one progress line, e.g.
/// `Receiving objects:  45% (450/1000), 1.20 MiB | 1.00 MiB/s` or
/// `remote: Counting objects: 12`.
#[must_use]
pub fn parse_progress(line: &str) -> Option<Progress> {
    let line = line.trim().trim_start_matches("remote:").trim();
    let (phase, rest) = line.split_once(':')?;
    if phase.is_empty() || phase.len() > 40 {
        return None;
    }
    let rest = rest.trim();
    if let Some(open) = rest.find('(') {
        let inner = &rest[open + 1..rest[open..].find(')').map(|i| open + i)?];
        let (cur, total) = inner.split_once('/')?;
        return Some(Progress {
            phase: phase.to_owned(),
            current: cur.trim().parse().ok()?,
            total: Some(total.trim().parse().ok()?),
        });
    }
    let first = rest.split([',', ' ']).next()?;
    Some(Progress {
        phase: phase.to_owned(),
        current: first.parse().ok()?,
        total: None,
    })
}

/// Splits a stderr chunk into lines on both `\r` and `\n` (progress lines are
/// rewritten in place with carriage returns).
pub fn split_progress_lines(chunk: &str) -> impl Iterator<Item = &str> {
    chunk.split(['\r', '\n']).filter(|l| !l.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_counted_progress() {
        let p =
            parse_progress("Receiving objects:  45% (450/1000), 1.20 MiB | 1.00 MiB/s").unwrap();
        assert_eq!(p.phase, "Receiving objects");
        assert_eq!((p.current, p.total), (450, Some(1000)));
        let p = parse_progress("remote: Compressing objects: 100% (5/5), done.").unwrap();
        assert_eq!(p.phase, "Compressing objects");
        assert_eq!((p.current, p.total), (5, Some(5)));
    }

    #[test]
    fn parses_uncounted_progress() {
        let p = parse_progress("remote: Enumerating objects: 12, done.").unwrap();
        assert_eq!((p.current, p.total), (12, None));
    }

    #[test]
    fn ignores_other_lines() {
        assert!(parse_progress("From github.com:x/y").is_none());
        assert!(parse_progress("fatal: could not read Username").is_none());
        let lines: Vec<_> = split_progress_lines("a: 1%\rb: 2%\n\n").collect();
        assert_eq!(lines, ["a: 1%", "b: 2%"]);
    }
}
