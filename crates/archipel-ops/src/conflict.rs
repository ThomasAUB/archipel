//! Conflict markers of a file left by a merge, rebase, cherry-pick or
//! revert, for the merge view: finding the conflict blocks
//! (`<<<<<<<` ours, optional `|||||||` base, `=======`, theirs `>>>>>>>`,
//! in the `merge`, `diff3` and `zdiff3` styles), resolving one or all of
//! them with a side, and rebuilding the file as one side. Pure text
//! functions; byte ranges index the text given.

use std::ops::Range;

/// The marker length Git uses unless the `conflict-marker-size` attribute
/// says otherwise.
const MIN_MARKER: usize = 7;

/// One conflict block of a file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Conflict {
    /// The whole block, markers included.
    pub range: Range<usize>,
    /// The current side's lines (between `<<<<<<<` and `|||||||` or `=======`).
    pub ours: Range<usize>,
    /// The common ancestor's lines, in the `diff3` / `zdiff3` styles.
    pub base: Option<Range<usize>>,
    /// The incoming side's lines (between `=======` and `>>>>>>>`).
    pub theirs: Range<usize>,
    /// The name after `<<<<<<<` (`HEAD`, a commit), possibly empty.
    pub ours_label: String,
    /// The name after `>>>>>>>` (the merged branch, a commit), possibly empty.
    pub theirs_label: String,
}

/// What a conflict is resolved with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pick {
    /// The current side.
    Ours,
    /// The incoming side.
    Theirs,
    /// Both, the current side first.
    OursThenTheirs,
    /// Both, the incoming side first.
    TheirsThenOurs,
    /// The common ancestor (empty when the markers do not hold it).
    Base,
}

/// A side of a conflicted file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    /// The current side.
    Ours,
    /// The incoming side.
    Theirs,
}

impl Conflict {
    /// The text the conflict is replaced with when resolved with `pick`.
    #[must_use]
    pub fn picked(&self, text: &str, pick: Pick) -> String {
        let part = |r: &Range<usize>| text.get(r.clone()).unwrap_or_default();
        let (ours, theirs) = (part(&self.ours), part(&self.theirs));
        match pick {
            Pick::Ours => ours.to_owned(),
            Pick::Theirs => theirs.to_owned(),
            Pick::OursThenTheirs => joined(ours, theirs),
            Pick::TheirsThenOurs => joined(theirs, ours),
            Pick::Base => self.base.as_ref().map_or("", part).to_owned(),
        }
    }

    /// The lines of `side`.
    #[must_use]
    pub fn side<'a>(&self, text: &'a str, side: Side) -> &'a str {
        let range = match side {
            Side::Ours => &self.ours,
            Side::Theirs => &self.theirs,
        };
        text.get(range.clone()).unwrap_or_default()
    }
}

/// `a` then `b`, with a line break between them when `a` lacks one.
fn joined(a: &str, b: &str) -> String {
    let mut out = String::with_capacity(a.len() + b.len() + 1);
    out.push_str(a);
    if !a.is_empty() && !b.is_empty() && !a.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(b);
    out
}

/// A line of the text: its range with and without its line break.
struct Line<'a> {
    start: usize,
    end: usize,
    content: &'a str,
}

fn lines(text: &str) -> Vec<Line<'_>> {
    let mut out = Vec::new();
    let mut start = 0;
    for line in text.split_inclusive('\n') {
        let end = start + line.len();
        let content = line
            .strip_suffix('\n')
            .map_or(line, |l| l.strip_suffix('\r').unwrap_or(l));
        out.push(Line {
            start,
            end,
            content,
        });
        start = end;
    }
    out
}

/// The length of the marker run of `c` opening `line`, when it is a
/// marker (at least [`MIN_MARKER`] long, then nothing or a space and a
/// label), with that label.
fn marker(line: &str, c: char) -> Option<(usize, &str)> {
    let len = line.chars().take_while(|&x| x == c).count();
    if len < MIN_MARKER {
        return None;
    }
    let rest = &line[len..];
    if rest.is_empty() {
        Some((len, ""))
    } else {
        rest.strip_prefix(' ').map(|label| (len, label))
    }
}

/// Whether `line` is the marker of `c` of length `len`.
fn is_marker(line: &str, c: char, len: usize) -> Option<&str> {
    marker(line, c)
        .filter(|(l, _)| *l == len)
        .map(|(_, label)| label)
}

/// The conflict blocks of `text`, in order. A block whose markers do not
/// close is left out (its lines are plain text).
#[must_use]
pub fn parse(text: &str) -> Vec<Conflict> {
    let lines = lines(text);
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        match block_at(&lines, i) {
            Some((conflict, next)) => {
                out.push(conflict);
                i = next;
            }
            None => i += 1,
        }
    }
    out
}

/// The block opening at line `i`, with the index of the line after it.
fn block_at(lines: &[Line<'_>], i: usize) -> Option<(Conflict, usize)> {
    let open = &lines[i];
    let (len, ours_label) = marker(open.content, '<')?;
    let mut base_at = None;
    let mut sep_at = None;
    for (j, line) in lines.iter().enumerate().skip(i + 1) {
        if is_marker(line.content, '<', len).is_some() {
            return None; // another block opens first: this one is text
        }
        match sep_at {
            None => {
                if base_at.is_none() && is_marker(line.content, '|', len).is_some() {
                    base_at = Some(j);
                } else if line.content.len() == len && line.content.chars().all(|c| c == '=') {
                    sep_at = Some(j);
                }
            }
            Some(sep) => {
                if let Some(theirs_label) = is_marker(line.content, '>', len) {
                    let ours_end = lines[base_at.unwrap_or(sep)].start;
                    let conflict = Conflict {
                        range: open.start..line.end,
                        ours: open.end..ours_end,
                        base: base_at.map(|b| lines[b].end..lines[sep].start),
                        theirs: lines[sep].end..line.start,
                        ours_label: ours_label.to_owned(),
                        theirs_label: theirs_label.to_owned(),
                    };
                    return Some((conflict, j + 1));
                }
            }
        }
    }
    None
}

/// `text` with `conflict` (one of [`parse`]'s blocks of it) resolved with
/// `pick`.
#[must_use]
pub fn resolve(text: &str, conflict: &Conflict, pick: Pick) -> String {
    let mut out = String::with_capacity(text.len());
    out.push_str(&text[..conflict.range.start]);
    out.push_str(&conflict.picked(text, pick));
    out.push_str(&text[conflict.range.end..]);
    out
}

/// `text` with every conflict resolved with `pick`.
#[must_use]
pub fn resolve_all(text: &str, pick: Pick) -> String {
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    for conflict in parse(text) {
        out.push_str(&text[at..conflict.range.start]);
        out.push_str(&conflict.picked(text, pick));
        at = conflict.range.end;
    }
    out.push_str(&text[at..]);
    out
}

/// `text` as `side` alone (every conflict replaced by that side's lines),
/// with the range each conflict takes in it.
#[must_use]
pub fn side_text(text: &str, conflicts: &[Conflict], side: Side) -> (String, Vec<Range<usize>>) {
    let mut out = String::with_capacity(text.len());
    let mut ranges = Vec::with_capacity(conflicts.len());
    let mut at = 0;
    for c in conflicts {
        out.push_str(&text[at..c.range.start]);
        let start = out.len();
        out.push_str(c.side(text, side));
        ranges.push(start..out.len());
        at = c.range.end;
    }
    out.push_str(&text[at..]);
    (out, ranges)
}

/// For each conflict of `current` (blocks of `current_text`), the index of
/// the conflict of `original` (blocks of `original_text`) it still is: the
/// same lines on both sides, in order. Resolved conflicts are missing from
/// `current`; one edited by hand matches nothing.
#[must_use]
pub fn match_conflicts(
    original_text: &str,
    original: &[Conflict],
    current_text: &str,
    current: &[Conflict],
) -> Vec<Option<usize>> {
    let key = |text, c: &Conflict| (c.side(text, Side::Ours), c.side(text, Side::Theirs));
    let mut next = 0;
    current
        .iter()
        .map(|c| {
            let wanted = key(current_text, c);
            let found = original
                .iter()
                .enumerate()
                .skip(next)
                .find(|(_, o)| key(original_text, o) == wanted)
                .map(|(i, _)| i);
            if let Some(i) = found {
                next = i + 1;
            }
            found
        })
        .collect()
}

/// Line index (0-based) of byte `offset` in `text`.
#[must_use]
pub fn line_of(text: &str, offset: usize) -> usize {
    text.get(..offset).unwrap_or(text).matches('\n').count()
}

#[cfg(test)]
mod tests {
    use super::*;

    const MERGE: &str = "top\n<<<<<<< HEAD\nmine\n=======\ntheirs\n>>>>>>> side\nmiddle\n<<<<<<< HEAD\na\nb\n=======\n>>>>>>> side\nend\n";

    #[test]
    fn finds_blocks_with_their_sides_and_labels() {
        let c = parse(MERGE);
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].side(MERGE, Side::Ours), "mine\n");
        assert_eq!(c[0].side(MERGE, Side::Theirs), "theirs\n");
        assert_eq!(c[0].ours_label, "HEAD");
        assert_eq!(c[0].theirs_label, "side");
        assert_eq!(c[0].base, None);
        assert_eq!(
            &MERGE[c[0].range.clone()],
            "<<<<<<< HEAD\nmine\n=======\ntheirs\n>>>>>>> side\n"
        );
        assert_eq!(c[1].side(MERGE, Side::Ours), "a\nb\n");
        assert_eq!(c[1].side(MERGE, Side::Theirs), "");
    }

    #[test]
    fn reads_the_base_of_the_diff3_style() {
        let text = "<<<<<<< ours\nx\n||||||| base\nold\n=======\ny\n>>>>>>> theirs\n";
        let c = parse(text);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].picked(text, Pick::Base), "old\n");
        assert_eq!(c[0].picked(text, Pick::Ours), "x\n");
        assert_eq!(c[0].picked(text, Pick::Theirs), "y\n");
    }

    #[test]
    fn keeps_crlf_lines_and_longer_markers() {
        let text = "<<<<<<<<< a\r\nx\r\n=========\r\ny\r\n>>>>>>>>> b\r\nz\r\n";
        let c = parse(text);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].side(text, Side::Ours), "x\r\n");
        assert_eq!(resolve(text, &c[0], Pick::Theirs), "y\r\nz\r\n");
        // A shorter run inside is content, not a marker.
        let text = "<<<<<<<<< a\n=======\n=========\ny\n>>>>>>>>> b\n";
        assert_eq!(parse(text)[0].side(text, Side::Ours), "=======\n");
    }

    #[test]
    fn leaves_unclosed_or_lookalike_markers_as_text() {
        assert!(parse("<<<<<<< a\nx\n=======\ny\n").is_empty());
        assert!(parse("<<<<<<x\n=======\n>>>>>>>\n").is_empty());
        assert!(parse("<<<<<<<x\n=======\n>>>>>>>\n").is_empty());
        // An unclosed block before a whole one: only the whole one counts.
        let text = "<<<<<<< a\nstray\n<<<<<<< b\nx\n=======\ny\n>>>>>>> c\n";
        let c = parse(text);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].ours_label, "b");
    }

    #[test]
    fn resolves_one_or_every_block() {
        let c = parse(MERGE);
        assert_eq!(
            resolve(MERGE, &c[0], Pick::OursThenTheirs),
            "top\nmine\ntheirs\nmiddle\n<<<<<<< HEAD\na\nb\n=======\n>>>>>>> side\nend\n"
        );
        assert!(
            resolve(MERGE, &c[0], Pick::TheirsThenOurs).starts_with("top\ntheirs\nmine\nmiddle\n")
        );
        assert_eq!(
            resolve_all(MERGE, Pick::Theirs),
            "top\ntheirs\nmiddle\nend\n"
        );
        assert_eq!(
            resolve_all(MERGE, Pick::Ours),
            "top\nmine\nmiddle\na\nb\nend\n"
        );
        // Both sides joined even when the first lacks its line break.
        assert_eq!(joined("a", "b\n"), "a\nb\n");
        assert_eq!(joined("", "b\n"), "b\n");
    }

    #[test]
    fn rebuilds_each_side_with_the_conflicts_ranges() {
        let c = parse(MERGE);
        let (ours, ranges) = side_text(MERGE, &c, Side::Ours);
        assert_eq!(ours, "top\nmine\nmiddle\na\nb\nend\n");
        assert_eq!(&ours[ranges[0].clone()], "mine\n");
        assert_eq!(&ours[ranges[1].clone()], "a\nb\n");
        let (theirs, ranges) = side_text(MERGE, &c, Side::Theirs);
        assert_eq!(theirs, "top\ntheirs\nmiddle\nend\n");
        assert_eq!(ranges[1], 18..18);
        assert_eq!(line_of(&theirs, ranges[1].start), 3);
    }

    #[test]
    fn matches_the_conflicts_left_to_the_original_ones() {
        let original = parse(MERGE);
        // The first resolved: the second remains, as the original's second.
        let now = resolve(MERGE, &original[0], Pick::Ours);
        let current = parse(&now);
        assert_eq!(match_conflicts(MERGE, &original, &now, &current), [Some(1)]);
        // Edited by hand inside: no longer the original conflict.
        let edited = now.replace("a\nb\n", "a\nc\n");
        let current = parse(&edited);
        assert_eq!(match_conflicts(MERGE, &original, &edited, &current), [None]);
    }
}
