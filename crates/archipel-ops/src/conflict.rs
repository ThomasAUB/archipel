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

/// The lines of `text` within `range`, each with its line break.
#[must_use]
pub fn line_ranges(text: &str, range: Range<usize>) -> Vec<Range<usize>> {
    let base = range.start;
    text.get(range)
        .unwrap_or_default()
        .split_inclusive('\n')
        .scan(base, |at, line| {
            let start = *at;
            *at += line.len();
            Some(start..*at)
        })
        .collect()
}

/// The lines picked from each side of one conflict, line by line: the
/// conflict is resolved with the picked lines of one side then of the
/// other, each in file order. Nothing picked: the conflict stays.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinePicks {
    /// Per line of the current side, picked.
    pub ours: Vec<bool>,
    /// Per line of the incoming side, picked.
    pub theirs: Vec<bool>,
    /// The incoming side's lines come first: the side picked first leads.
    pub theirs_first: bool,
}

impl LinePicks {
    /// Nothing picked, for `conflict` of `text`.
    #[must_use]
    pub fn new(conflict: &Conflict, text: &str) -> Self {
        let count = |r: &Range<usize>| line_ranges(text, r.clone()).len();
        Self {
            ours: vec![false; count(&conflict.ours)],
            theirs: vec![false; count(&conflict.theirs)],
            theirs_first: false,
        }
    }

    /// The picks of `side`.
    #[must_use]
    pub fn of(&self, side: Side) -> &[bool] {
        match side {
            Side::Ours => &self.ours,
            Side::Theirs => &self.theirs,
        }
    }

    /// Whether any line is picked.
    #[must_use]
    pub fn any(&self) -> bool {
        self.ours.iter().chain(&self.theirs).any(|&p| p)
    }

    /// Picks (or drops) `lines` of `side`. The first lines picked set the
    /// order: that side leads.
    pub fn set(&mut self, side: Side, lines: impl IntoIterator<Item = usize>, picked: bool) {
        let was_empty = !self.any();
        let picks = match side {
            Side::Ours => &mut self.ours,
            Side::Theirs => &mut self.theirs,
        };
        for i in lines {
            if let Some(p) = picks.get_mut(i) {
                *p = picked;
            }
        }
        if picked && was_empty {
            self.theirs_first = side == Side::Theirs;
        }
    }

    /// Picks every line of `side` when not all are, else drops them all.
    pub fn toggle_side(&mut self, side: Side) {
        let all = self.of(side).iter().all(|&p| p);
        let n = self.of(side).len();
        self.set(side, 0..n, !all);
    }

    /// Only `pick`'s lines picked (both sides, in its order); nothing for
    /// the base, which has no lines to pick here.
    pub fn pick(&mut self, pick: Pick) {
        let (ours, theirs) = match pick {
            Pick::Ours => (true, false),
            Pick::Theirs => (false, true),
            Pick::OursThenTheirs | Pick::TheirsThenOurs => (true, true),
            Pick::Base => (false, false),
        };
        self.ours.fill(ours);
        self.theirs.fill(theirs);
        self.theirs_first = pick == Pick::TheirsThenOurs;
    }

    /// The text `conflict` (a block of `text`) is resolved with; `None`
    /// when nothing is picked.
    #[must_use]
    pub fn compose(&self, conflict: &Conflict, text: &str) -> Option<String> {
        if !self.any() {
            return None;
        }
        let lines = |r: &Range<usize>, picks: &[bool]| {
            line_ranges(text, r.clone())
                .into_iter()
                .zip(picks.to_vec())
                .filter(|(_, p)| *p)
                .map(|(l, _)| text.get(l).unwrap_or_default())
                .collect::<Vec<_>>()
        };
        let ours = lines(&conflict.ours, &self.ours);
        let theirs = lines(&conflict.theirs, &self.theirs);
        let (first, second) = if self.theirs_first {
            (theirs, ours)
        } else {
            (ours, theirs)
        };
        let eol = if text
            .get(conflict.range.clone())
            .unwrap_or_default()
            .contains("\r\n")
        {
            "\r\n"
        } else {
            "\n"
        };
        let mut out = String::new();
        for line in first.into_iter().chain(second) {
            if !out.is_empty() && !out.ends_with('\n') {
                out.push_str(eol);
            }
            out.push_str(line);
        }
        Some(out)
    }
}

/// Follows the edit that turned `old` into `new` in `ranges` (ranges of
/// `old`, in order, not overlapping): a range after the edit moves with
/// it, one the edit touches stretches or shrinks with it, an empty one
/// typed into holds what was typed. The edit is
/// taken as the one span between the texts' common start and end, as
/// typing or a paste makes.
pub fn track_edit(old: &str, new: &str, ranges: &mut [Range<usize>]) {
    if old == new {
        return;
    }
    let (a, b) = (old.as_bytes(), new.as_bytes());
    let mut prefix = a.iter().zip(b).take_while(|(x, y)| x == y).count();
    while !old.is_char_boundary(prefix) || !new.is_char_boundary(prefix) {
        prefix -= 1;
    }
    let room = a.len().min(b.len()) - prefix;
    let mut suffix = a
        .iter()
        .rev()
        .zip(b.iter().rev())
        .take(room)
        .take_while(|(x, y)| x == y)
        .count();
    while !old.is_char_boundary(a.len() - suffix) || !new.is_char_boundary(b.len() - suffix) {
        suffix -= 1;
    }
    let (old_end, new_end) = (a.len() - suffix, b.len() - suffix);
    let moved = |at: usize| at + new_end - old_end;
    for r in ranges {
        if r.start == r.end && r.start == prefix && old_end == prefix {
            // Typed into an emptied range: it holds what was typed.
            *r = prefix..new_end;
        } else if r.end <= prefix {
            // Before the edit (typing right after a range leaves it be).
        } else if r.start >= old_end {
            *r = moved(r.start)..moved(r.end);
        } else {
            let end = if r.end >= old_end {
                moved(r.end)
            } else {
                new_end
            };
            *r = r.start.min(prefix)..end;
        }
    }
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

    #[test]
    fn line_picks_compose_in_the_order_picked() {
        let text = "<<<<<<< a\no1\no2\n=======\nt1\nt2\n>>>>>>> b\n";
        let c = &parse(text)[0];
        let mut p = LinePicks::new(c, text);
        assert_eq!((p.ours.len(), p.theirs.len()), (2, 2));
        assert_eq!(p.compose(c, text), None);
        p.set(Side::Theirs, [1], true);
        p.set(Side::Ours, [0], true);
        assert!(p.theirs_first);
        assert_eq!(p.compose(c, text).unwrap(), "t2\no1\n");
        p.theirs_first = false;
        assert_eq!(p.compose(c, text).unwrap(), "o1\nt2\n");
        p.toggle_side(Side::Ours);
        assert_eq!(p.compose(c, text).unwrap(), "o1\no2\nt2\n");
        p.toggle_side(Side::Ours);
        assert_eq!(p.compose(c, text).unwrap(), "t2\n");
        p.pick(Pick::TheirsThenOurs);
        assert_eq!(p.compose(c, text).unwrap(), "t1\nt2\no1\no2\n");
        p.pick(Pick::Base);
        assert_eq!(p.compose(c, text), None);
    }

    #[test]
    fn line_picks_add_a_break_after_a_last_line_without_one() {
        let text = "<<<<<<< a\r\no\r\n=======\r\nt\r\n>>>>>>> b\r\n";
        let c = &parse(text)[0];
        let mut p = LinePicks::new(c, text);
        p.pick(Pick::OursThenTheirs);
        assert_eq!(p.compose(c, text).unwrap(), "o\r\nt\r\n");
        // A side ending the file without a line break.
        let c = Conflict {
            range: 0..text.len(),
            ours: 11..12,
            base: None,
            theirs: 23..24,
            ours_label: String::new(),
            theirs_label: String::new(),
        };
        assert_eq!(p.compose(&c, text).unwrap(), "o\r\nt");
    }

    #[test]
    fn tracked_ranges_follow_edits() {
        let old = "aa[XX]bb[YY]cc";
        let mut r = [2..6, 8..12];
        // Typing before both.
        track_edit(old, "aaz[XX]bb[YY]cc", &mut r);
        assert_eq!(r, [3..7, 9..13]);
        // Replacing the first block's inside.
        let mut r = [2..6, 8..12];
        track_edit(old, "aa[hello]bb[YY]cc", &mut r);
        assert_eq!(r, [2..9, 11..15]);
        // Deleting between them.
        let mut r = [2..6, 8..12];
        track_edit(old, "aa[XX][YY]cc", &mut r);
        assert_eq!(r, [2..6, 6..10]);
        // Typing after both.
        let mut r = [2..6, 8..12];
        track_edit(old, "aa[XX]bb[YY]ccdd", &mut r);
        assert_eq!(r, [2..6, 8..12]);
        // Emptying a block, then filling it again.
        let mut r = [2..6, 6..6, 8..12];
        track_edit("aa[XX]bb[YY]cc", "aabb[YY]cc", &mut r);
        assert_eq!(r, [2..2, 2..2, 4..8]);
        let mut r = [2..2, 4..8];
        track_edit("aabb[YY]cc", "aaQ\nbb[YY]cc", &mut r);
        assert_eq!(r, [2..4, 6..10]);
        // Multi-byte text keeps char boundaries.
        let mut r = [0..2, 2..3];
        track_edit("éa", "èa", &mut r);
        assert_eq!(r, [0..2, 2..3]);
    }

    #[test]
    fn lines_of_a_range() {
        assert_eq!(line_ranges("ab\ncd\nef", 3..8), [3..6, 6..8]);
        assert!(line_ranges("ab", 1..1).is_empty());
    }
}
