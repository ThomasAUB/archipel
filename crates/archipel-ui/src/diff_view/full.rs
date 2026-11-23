//! The whole file around a diff's hunks: the unchanged lines between and
//! around them, read from the file's new version, shown as context rows
//! (see [`DiffRow::Gap`](super::DiffRow::Gap)). The hunks stay as they
//! are, so staging, line picks and Copy hunk work alike in both modes.
//! The view toggles between the changes only and the whole file; the
//! whole-file doc is read off the UI thread, the changes shown meanwhile.

use std::sync::Arc;

use archipel_model::changes::ChangeKind;
use archipel_model::diff::{DiffLine, FileDiff, LineOrigin};
use archipel_model::ports::DiffSide;
use gpui_kit::Context;
use gpui_kit::prelude::*;

use super::{DiffDoc, Revs};
use crate::image_view::compare::Source;
use crate::staging::Staging;

/// Files larger than this are shown with their changes only.
const MAX_FILE_BYTES: usize = 4 * 1024 * 1024;

/// The unchanged lines of one file, each run with the index of the hunk
/// it comes before (the hunk count: after the last one).
pub(super) type Gaps = Vec<(usize, Vec<DiffLine>)>;

/// The unchanged lines of `file` around its hunks, taken from `text`, the
/// file's new version. `None` when `text` does not match the hunks (the
/// file changed since the diff was read).
pub(super) fn gaps(file: &FileDiff, text: &str) -> Option<Gaps> {
    let lines: Vec<&str> = text.split_inclusive('\n').map(trim_newline).collect();
    let (mut old, mut new) = (1u32, 1u32);
    let mut out = Gaps::new();
    let take = |at: usize, old: u32, new: u32, end: u32, out: &mut Gaps| -> Option<()> {
        if end <= new {
            return Some(());
        }
        let run = lines
            .get(new as usize - 1..end as usize - 1)?
            .iter()
            .zip(0..)
            .map(|(content, i)| DiffLine {
                origin: LineOrigin::Context,
                content: (*content).to_owned(),
                old_lineno: Some(old + i),
                new_lineno: Some(new + i),
                no_newline_at_eof: false,
            })
            .collect();
        out.push((at, run));
        Some(())
    };
    for (h, hunk) in file.hunks.iter().enumerate() {
        // A side without lines starts after the line it names.
        let old_first = hunk.old_start + u32::from(hunk.old_lines == 0);
        let new_first = hunk.new_start + u32::from(hunk.new_lines == 0);
        if new_first < new || new_first - new != old_first.checked_sub(old)? {
            return None;
        }
        take(h, old, new, new_first, &mut out)?;
        old = old_first + hunk.old_lines;
        new = new_first + hunk.new_lines;
    }
    let total = u32::try_from(lines.len()).ok()?;
    if total + 1 < new {
        return None;
    }
    take(file.hunks.len(), old, new, total + 1, &mut out)?;
    Some(out)
}

/// A line without its `\n`, keeping a `\r` as the diff's lines do.
fn trim_newline(line: &str) -> &str {
    line.strip_suffix('\n').unwrap_or(line)
}

/// Whether file `file` has unchanged lines to show: a text file on both
/// sides (an added or deleted file's diff is already the whole file).
fn expandable(file: &FileDiff) -> bool {
    !file.binary
        && !file.truncated
        && file.new_path.is_some()
        && !matches!(file.kind, ChangeKind::Added | ChangeKind::Deleted)
}

/// Whether `doc` reads differently as the whole file: the toggle shows.
pub(super) fn offered(doc: &DiffDoc) -> bool {
    doc.files.iter().any(expandable)
}

/// Where the new version of a diff's files is read from: the working copy
/// (unstaged), the index (staged), or the newer commit.
fn new_source(side: Option<DiffSide>, revs: Option<&Revs>) -> Option<Source> {
    match (side, revs) {
        (Some(DiffSide::Unstaged), _) => Some(Source::Worktree),
        (Some(DiffSide::Staged), _) => Some(Source::Index),
        (None, Some(revs)) => Some(Source::Rev(revs.new.to_hex())),
        (None, None) => None,
    }
}

/// The doc to show for `base`: its whole-file version once read, while
/// the view shows whole files; else `base`. Starts reading it.
pub(super) fn shown<V: Staging>(
    v: &mut V,
    base: &Arc<DiffDoc>,
    side: Option<DiffSide>,
    cx: &mut Context<V>,
) -> Arc<DiffDoc> {
    if !v.panel().full_file || !offered(base) {
        return base.clone();
    }
    if let Some((read_for, full)) = &v.panel().full
        && Arc::ptr_eq(read_for, base)
    {
        return full.clone().unwrap_or_else(|| base.clone());
    }
    let Some(source) = new_source(side, base.revs.as_ref()) else {
        return base.clone();
    };
    v.panel().full = Some((base.clone(), None));
    let actions = v.core().repo_actions().clone();
    let repo = v.repo_path().clone();
    let read = base.clone();
    let task = cx.background_spawn(async move {
        let texts = read
            .files
            .iter()
            .map(|file| {
                let path = file.new_path.as_deref().filter(|_| expandable(file))?;
                let bytes = match &source {
                    Source::Rev(rev) => actions.read_file_at_rev(&repo, rev, path).ok()??,
                    Source::Index => actions.read_index_file(&repo, path).ok()?,
                    Source::Worktree => actions.read_file(&repo, path).ok()?,
                };
                (bytes.len() <= MAX_FILE_BYTES)
                    .then(|| gaps(file, &String::from_utf8_lossy(&bytes)))
                    .flatten()
            })
            .collect::<Vec<_>>();
        Arc::new(DiffDoc::with_gaps(read.files.clone(), texts).with_revs(read.revs.clone()))
    });
    let read_for = base.clone();
    cx.spawn(async move |this, cx| {
        let full = task.await;
        let _ = this.update(cx, |v, cx| {
            let panel = v.panel();
            // Another diff opened meanwhile, or the toggle went off.
            if panel
                .full
                .as_ref()
                .is_some_and(|(r, _)| Arc::ptr_eq(r, &read_for))
            {
                panel.full = Some((read_for, Some(full)));
                cx.notify();
            }
        });
    })
    .detach();
    base.clone()
}

/// The doc shown for `base` (its whole-file version when read), without
/// starting a read: for keys on the view.
pub(super) fn cached<V: Staging>(v: &mut V, base: &Arc<DiffDoc>) -> Arc<DiffDoc> {
    let on = v.panel().full_file;
    match &v.panel().full {
        Some((read_for, Some(full))) if on && Arc::ptr_eq(read_for, base) => full.clone(),
        _ => base.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use archipel_model::diff::Hunk;

    fn line(origin: LineOrigin, content: &str, old: Option<u32>, new: Option<u32>) -> DiffLine {
        DiffLine {
            origin,
            content: content.into(),
            old_lineno: old,
            new_lineno: new,
            no_newline_at_eof: false,
        }
    }

    fn modified(hunks: Vec<Hunk>) -> FileDiff {
        FileDiff {
            old_path: Some("a".into()),
            new_path: Some("a".into()),
            kind: ChangeKind::Modified,
            binary: false,
            hunks,
            truncated: false,
        }
    }

    fn hunk(old: (u32, u32), new: (u32, u32), lines: Vec<DiffLine>) -> Hunk {
        Hunk {
            old_start: old.0,
            old_lines: old.1,
            new_start: new.0,
            new_lines: new.1,
            header: String::new(),
            lines,
        }
    }

    #[test]
    fn fills_lines_around_and_between_hunks() {
        use LineOrigin::{Added, Removed};
        // old: 1 2 3 x 5 6 7 8 / new: 1 2 3 y 5 6 7 8 z
        let file = modified(vec![
            hunk(
                (4, 1),
                (4, 1),
                vec![
                    line(Removed, "x", Some(4), None),
                    line(Added, "y", None, Some(4)),
                ],
            ),
            hunk((8, 0), (9, 1), vec![line(Added, "z", None, Some(9))]),
        ]);
        let gaps = gaps(&file, "1\n2\n3\ny\n5\n6\n7\n8\nz\n").unwrap();
        let shape: Vec<_> = gaps
            .iter()
            .map(|(h, run)| {
                let numbers: Vec<_> = run.iter().map(|l| (l.old_lineno, l.new_lineno)).collect();
                (*h, numbers)
            })
            .collect();
        assert_eq!(
            shape,
            vec![
                (
                    0,
                    vec![(Some(1), Some(1)), (Some(2), Some(2)), (Some(3), Some(3))]
                ),
                (
                    1,
                    vec![
                        (Some(5), Some(5)),
                        (Some(6), Some(6)),
                        (Some(7), Some(7)),
                        (Some(8), Some(8)),
                    ]
                ),
            ]
        );
        assert_eq!(gaps[1].1[0].content, "5");
    }

    #[test]
    fn keeps_carriage_returns_and_rejects_a_changed_file() {
        use LineOrigin::Added;
        let file = modified(vec![hunk(
            (1, 0),
            (2, 1),
            vec![line(Added, "b", None, Some(2))],
        )]);
        let gaps = gaps(&file, "a\r\nb\r\nc").unwrap();
        assert_eq!(gaps[0].1[0].content, "a\r");
        assert_eq!(gaps[1].1[0].content, "c");
        assert_eq!(gaps[1].1[0].old_lineno, Some(2));
        // Shorter than the hunks say.
        assert!(super::gaps(&file, "a\n").is_none());
    }
}
