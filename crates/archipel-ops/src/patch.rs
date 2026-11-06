//! Patches for hunk and line staging (PLAN Phase 4 step 4).
//!
//! The working copy view stages part of a file by building a patch from the
//! displayed diff and applying it with `git apply --cached`:
//!
//! * **staging** selected lines of an *unstaged* diff (index → worktree):
//!   [`PatchDirection::Forward`];
//! * **unstaging** selected lines of a *staged* diff (`HEAD` → index): the
//!   patch is applied with `--reverse`, [`PatchDirection::Reverse`].
//!
//! Unselected lines are turned into context or dropped so the patch applies
//! exactly to the index. Binary, truncated, added/deleted/renamed files and
//! selections touching a missing final newline are refused: the caller falls
//! back to file-level staging with the returned message.

use std::fmt::Write as _;

use archipel_model::changes::ChangeKind;
use archipel_model::diff::{FileDiff, Hunk, LineOrigin};
use archipel_model::requests::PatchDirection;

use crate::error::OpError;

fn refuse(reason: &str) -> OpError {
    OpError::Refused(format!("{reason}; stage the whole file instead"))
}

fn header(file: &FileDiff) -> Result<String, OpError> {
    if file.binary {
        return Err(refuse("binary files cannot be staged partially"));
    }
    if file.truncated {
        return Err(refuse("the diff is too large to be staged partially"));
    }
    if file.kind != ChangeKind::Modified {
        return Err(refuse(
            "added, deleted or renamed files cannot be staged partially",
        ));
    }
    let path = file.display_path();
    Ok(format!(
        "diff --git a/{path} b/{path}\n--- a/{path}\n+++ b/{path}\n"
    ))
}

/// Patch staging (or unstaging) one whole hunk.
pub fn hunk_patch(file: &FileDiff, hunk: usize) -> Result<String, OpError> {
    let h = file
        .hunks
        .get(hunk)
        .ok_or_else(|| OpError::Refused(format!("no hunk {hunk}")))?;
    let all: Vec<usize> = (0..h.lines.len()).collect();
    lines_patch(file, hunk, &all, PatchDirection::Forward)
}

/// Patch restricted to the `selected` line indices of hunk `hunk`.
pub fn lines_patch(
    file: &FileDiff,
    hunk: usize,
    selected: &[usize],
    direction: PatchDirection,
) -> Result<String, OpError> {
    selection_patch(file, &[(hunk, selected.to_vec())], direction)
}

/// Patch restricted to selected lines of several hunks: `(hunk, line
/// indices)` pairs. Hunks without a selected change are skipped.
///
/// Besides staging and unstaging, a [`PatchDirection::Reverse`] patch of an
/// *unstaged* diff discards the selected lines when applied in reverse to
/// the worktree (it matches the worktree, the new side).
pub fn selection_patch(
    file: &FileDiff,
    selection: &[(usize, Vec<usize>)],
    direction: PatchDirection,
) -> Result<String, OpError> {
    let mut out = header(file)?;
    let mut picks: Vec<&(usize, Vec<usize>)> = selection.iter().collect();
    picks.sort_by_key(|(h, _)| *h);
    // Lines added minus lines removed by the hunks already written, which
    // shifts the free side of the following ones.
    let mut shift = 0i64;
    let mut any = false;
    for (hunk, selected) in picks {
        let h: &Hunk = file
            .hunks
            .get(*hunk)
            .ok_or_else(|| OpError::Refused(format!("no hunk {hunk}")))?;
        let Some(part) = hunk_part(h, selected, direction)? else {
            continue;
        };
        any = true;
        let start = |anchor: i64, count: u32| if count == 0 { anchor - 1 } else { anchor }.max(0);
        let anchor = i64::from(part.anchor);
        let (old_start, new_start) = match direction {
            PatchDirection::Forward => (
                start(anchor, part.old_count),
                start(anchor + shift, part.new_count),
            ),
            PatchDirection::Reverse => (
                start(anchor - shift, part.old_count),
                start(anchor, part.new_count),
            ),
        };
        shift += i64::from(part.new_count) - i64::from(part.old_count);
        let _ = writeln!(
            out,
            "@@ -{old_start},{} +{new_start},{} @@",
            part.old_count, part.new_count
        );
        out.push_str(&part.body);
    }
    if !any {
        return Err(OpError::Refused("no changed line selected".into()));
    }
    Ok(out)
}

/// One hunk of a selection patch.
struct HunkPart {
    body: String,
    old_count: u32,
    new_count: u32,
    /// Exact start on the side matched against the target.
    anchor: u32,
}

/// The patch lines of hunk `h` restricted to `selected`, `None` when no
/// changed line is selected.
fn hunk_part(
    h: &Hunk,
    selected: &[usize],
    direction: PatchDirection,
) -> Result<Option<HunkPart>, OpError> {
    let mut body = String::new();
    let (mut old_count, mut new_count, mut changes) = (0u32, 0u32, 0usize);
    for i in ordered_lines(h, selected, direction) {
        let line = &h.lines[i];
        let picked = selected.contains(&i);
        // What this line becomes in the patch.
        let prefix = match (line.origin, picked, direction) {
            (LineOrigin::Added, true, _) => Some('+'),
            (LineOrigin::Removed, true, _) => Some('-'),
            // Staging: an unselected removal stays in the index (context),
            // an unselected addition is not staged (dropped).
            (LineOrigin::Context, _, _)
            | (LineOrigin::Removed, false, PatchDirection::Forward)
            | (LineOrigin::Added, false, PatchDirection::Reverse) => Some(' '),
            (LineOrigin::Added, false, PatchDirection::Forward)
            | (LineOrigin::Removed, false, PatchDirection::Reverse) => None,
        };
        let Some(prefix) = prefix else { continue };
        if line.no_newline_at_eof && prefix == ' ' && line.origin != LineOrigin::Context {
            return Err(refuse("the selection touches a line without final newline"));
        }
        match prefix {
            ' ' => {
                old_count += 1;
                new_count += 1;
            }
            '+' => {
                new_count += 1;
                changes += 1;
            }
            _ => {
                old_count += 1;
                changes += 1;
            }
        }
        body.push(prefix);
        body.push_str(&line.content);
        body.push('\n');
        if line.no_newline_at_eof {
            body.push_str("\\ No newline at end of file\n");
        }
    }
    if changes == 0 {
        return Ok(None);
    }
    // The side matched against the target keeps its exact position: the
    // old side when staging, the new side when unstaging (applied in
    // reverse).
    let (start, lines) = match direction {
        PatchDirection::Forward => (h.old_start, h.old_lines),
        PatchDirection::Reverse => (h.new_start, h.new_lines),
    };
    let anchor = if lines == 0 { start + 1 } else { start };
    Ok(Some(HunkPart {
        body,
        old_count,
        new_count,
        anchor,
    }))
}

/// Order in which hunk lines are emitted.
///
/// Within a change block (removals followed by additions), the side matched
/// against the index must keep its order, but the other side may move. When
/// staging, selected additions are placed right after the last selected
/// removal, so a partially staged replacement lands where the old line was
/// (unselected removals after it stay after it). When unstaging, the hunk
/// order is already right: restored removals go before the kept additions.
fn ordered_lines(h: &Hunk, selected: &[usize], direction: PatchDirection) -> Vec<usize> {
    if direction == PatchDirection::Reverse {
        return (0..h.lines.len()).collect();
    }
    let mut order = Vec::with_capacity(h.lines.len());
    let mut i = 0;
    while i < h.lines.len() {
        if h.lines[i].origin == LineOrigin::Context {
            order.push(i);
            i += 1;
            continue;
        }
        let start = i;
        while i < h.lines.len() && h.lines[i].origin == LineOrigin::Removed {
            i += 1;
        }
        let removed = start..i;
        while i < h.lines.len() && h.lines[i].origin == LineOrigin::Added {
            i += 1;
        }
        let added = removed.end..i;
        let split = removed
            .clone()
            .filter(|r| selected.contains(r))
            .max()
            .map_or(removed.end, |r| r + 1);
        order.extend(removed.start..split);
        order.extend(added);
        order.extend(split..removed.end);
    }
    order
}

#[cfg(test)]
mod tests {
    use archipel_model::diff::DiffLine;

    use super::*;

    fn line(origin: LineOrigin, content: &str) -> DiffLine {
        DiffLine {
            origin,
            content: content.into(),
            old_lineno: None,
            new_lineno: None,
            no_newline_at_eof: false,
        }
    }

    fn file(lines: Vec<DiffLine>) -> FileDiff {
        FileDiff {
            old_path: Some("f.txt".into()),
            new_path: Some("f.txt".into()),
            kind: ChangeKind::Modified,
            binary: false,
            truncated: false,
            hunks: vec![Hunk {
                old_start: 1,
                old_lines: 3,
                new_start: 1,
                new_lines: 3,
                header: String::new(),
                lines,
            }],
        }
    }

    #[test]
    fn forward_selection_turns_unselected_removals_into_context() {
        let f = file(vec![
            line(LineOrigin::Context, "a"),
            line(LineOrigin::Removed, "b"),
            line(LineOrigin::Removed, "c"),
            line(LineOrigin::Added, "B"),
            line(LineOrigin::Added, "C"),
        ]);
        let p = lines_patch(&f, 0, &[1, 3], PatchDirection::Forward).unwrap();
        // The staged replacement lands where `b` was, before the kept `c`.
        assert!(p.ends_with("@@ -1,3 +1,3 @@\n a\n-b\n+B\n c\n"), "{p}");
    }

    #[test]
    fn reverse_selection_turns_unselected_additions_into_context() {
        let f = file(vec![
            line(LineOrigin::Context, "a"),
            line(LineOrigin::Removed, "b"),
            line(LineOrigin::Added, "B"),
            line(LineOrigin::Added, "X"),
        ]);
        let p = lines_patch(&f, 0, &[3], PatchDirection::Reverse).unwrap();
        assert!(p.ends_with("@@ -1,2 +1,3 @@\n a\n B\n+X\n"), "{p}");
    }

    #[test]
    fn multi_hunk_selection_shifts_the_free_side() {
        let mut f = file(vec![
            line(LineOrigin::Context, "a"),
            line(LineOrigin::Added, "x"),
            line(LineOrigin::Added, "y"),
        ]);
        f.hunks[0].old_lines = 1;
        f.hunks.push(Hunk {
            old_start: 10,
            old_lines: 2,
            new_start: 12,
            new_lines: 1,
            header: String::new(),
            lines: vec![
                line(LineOrigin::Context, "k"),
                line(LineOrigin::Removed, "z"),
            ],
        });
        let p = selection_patch(
            &f,
            &[(1, vec![1]), (0, vec![1, 2])],
            PatchDirection::Forward,
        )
        .unwrap();
        assert!(
            p.ends_with("@@ -1,1 +1,3 @@\n a\n+x\n+y\n@@ -10,2 +12,1 @@\n k\n-z\n"),
            "{p}"
        );
        // Only the second hunk: no shift.
        let p = selection_patch(&f, &[(1, vec![1])], PatchDirection::Forward).unwrap();
        assert!(p.ends_with("@@ -10,2 +10,1 @@\n k\n-z\n"), "{p}");
        // Reverse (unstage / discard) anchors on the new side.
        let p =
            selection_patch(&f, &[(0, vec![2]), (1, vec![1])], PatchDirection::Reverse).unwrap();
        assert!(
            p.ends_with("@@ -1,2 +1,3 @@\n a\n x\n+y\n@@ -11,2 +12,1 @@\n k\n-z\n"),
            "{p}"
        );
    }

    #[test]
    fn refusals() {
        let mut f = file(vec![line(LineOrigin::Context, "a")]);
        assert!(lines_patch(&f, 0, &[0], PatchDirection::Forward).is_err());
        f.binary = true;
        assert!(hunk_patch(&f, 0).is_err());
        let mut eof = line(LineOrigin::Removed, "z");
        eof.no_newline_at_eof = true;
        let f = file(vec![eof, line(LineOrigin::Added, "y")]);
        assert!(lines_patch(&f, 0, &[1], PatchDirection::Forward).is_err());
    }
}
