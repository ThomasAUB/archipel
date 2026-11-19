//! The image diff: a one-file diff of an image shows the image before and
//! after side by side, read and decoded off the UI thread, on canvases of
//! one size (the larger of the two, images aligned top-left) so both
//! share one scale. An added or deleted image shows alone.

use archipel_model::RepoPath;
use archipel_model::changes::ChangeKind;
use archipel_model::diff::FileDiff;
use archipel_model::ports::DiffSide;
use archipel_ops::RepoActions;
use archipel_theme::ColorToken as T;
use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, Context, ImageFormat, SvgRenderer, Window, div};

use super::stage::{Layer, Zoom, stage};
use super::viewer::{info, zoom_chips};
use super::{Picture, decode, image_format, release};
use crate::diff_view::Revs;
use crate::theme::color;

/// Where one version of a file is read from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Source {
    /// The tree of a revision (`HEAD`, `<oid>^`, an id).
    Rev(String),
    /// The index (staged).
    Index,
    /// The working copy.
    Worktree,
}

/// One version of a file: where it is read from, and its path there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Version {
    pub(crate) source: Source,
    pub(crate) path: String,
}

/// The two versions an image diff compares; `None`: the file is not on
/// that side (added, deleted).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Sides {
    pub(crate) old: Option<Version>,
    pub(crate) new: Option<Version>,
}

/// Whether `path` names an image file.
pub(crate) fn is_image(path: &str) -> bool {
    image_format(path).is_some()
}

/// The versions to compare for `file`, shown as images: a binary file
/// with an image path, or an SVG when `svg_images`. `side`: a working-copy
/// diff (unstaged: index → disk; staged: `HEAD` → index); else a commit
/// diff between `revs`. `None`: not shown as images, or the versions are
/// unknown.
pub(crate) fn sides(
    file: &FileDiff,
    side: Option<DiffSide>,
    revs: Option<&Revs>,
    svg_images: bool,
) -> Option<Sides> {
    let paths = [file.old_path.as_deref(), file.new_path.as_deref()];
    let shown = paths.into_iter().flatten().any(|p| match image_format(p) {
        Some(ImageFormat::Svg) => file.binary || svg_images,
        Some(_) => file.binary,
        None => false,
    });
    if !shown {
        return None;
    }
    let (old, new) = match (side, revs) {
        (Some(DiffSide::Unstaged), _) => (Source::Index, Source::Worktree),
        (Some(DiffSide::Staged), _) => (Source::Rev("HEAD".into()), Source::Index),
        (None, Some(revs)) => (
            Source::Rev(revs.old.clone()),
            Source::Rev(revs.new.to_hex()),
        ),
        (None, None) => return None,
    };
    let version = |source, path: Option<&String>, absent: ChangeKind| {
        path.filter(|_| file.kind != absent).map(|p| Version {
            source,
            path: p.clone(),
        })
    };
    Some(Sides {
        old: version(old, file.old_path.as_ref(), ChangeKind::Added),
        new: version(new, file.new_path.as_ref(), ChangeKind::Deleted),
    })
}

/// One side, as read.
enum Loaded {
    Loading,
    /// Not on this side.
    Absent,
    Failed(String),
    Ready(Picture),
}

impl Loaded {
    fn picture(&self) -> Option<&Picture> {
        match self {
            Self::Ready(p) => Some(p),
            _ => None,
        }
    }
}

/// Reads and decodes `version` (off the UI thread).
fn load(
    actions: &RepoActions,
    repo: &RepoPath,
    version: Option<&Version>,
    svg: &SvgRenderer,
) -> Loaded {
    let Some(Version { source, path }) = version else {
        return Loaded::Absent;
    };
    let bytes = match source {
        Source::Rev(rev) => actions.read_file_at_rev(repo, rev, path),
        Source::Index => actions.read_index_file(repo, path).map(Some),
        Source::Worktree => actions.read_file(repo, path).map(Some),
    };
    let bytes = match bytes {
        Ok(Some(b)) => b,
        Ok(None) => return Loaded::Absent,
        Err(e) => return Loaded::Failed(e.to_string()),
    };
    let Some(format) = image_format(path) else {
        return Loaded::Failed("Not an image".into());
    };
    match decode(format, &bytes, svg) {
        Ok(p) => Loaded::Ready(p),
        Err(e) => Loaded::Failed(format!("Not decoded: {e}")),
    }
}

/// The image diff's state.
pub(crate) struct ImageDiff {
    repo: RepoPath,
    sides: Sides,
    /// The diff it was read for (its address): a reloaded diff reads the
    /// images again.
    doc: usize,
    old: Loaded,
    new: Loaded,
    zoom: Zoom,
}

impl ImageDiff {
    /// The image diff of `sides`, read for diff `doc`; its textures are
    /// freed with it.
    pub(crate) fn new(
        actions: RepoActions,
        repo: RepoPath,
        sides: Sides,
        doc: usize,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.on_release(|v: &mut Self, cx| v.release(cx)).detach();
        let mut diff = Self {
            repo,
            sides,
            doc,
            old: Loaded::Loading,
            new: Loaded::Loading,
            zoom: Zoom::Fit,
        };
        diff.load(actions, cx);
        diff
    }

    /// Whether it shows `sides` of `repo`.
    pub(crate) fn shows(&self, repo: &RepoPath, sides: &Sides) -> bool {
        self.repo == *repo && self.sides == *sides
    }

    /// Reads the images again for diff `doc` (reloaded), keeping the ones
    /// shown until then.
    pub(crate) fn refresh(&mut self, actions: RepoActions, doc: usize, cx: &mut Context<Self>) {
        if self.doc != doc {
            self.doc = doc;
            self.load(actions, cx);
        }
    }

    fn load(&mut self, actions: RepoActions, cx: &mut Context<Self>) {
        let (repo, sides, doc) = (self.repo.clone(), self.sides.clone(), self.doc);
        let svg = cx.svg_renderer();
        let task = cx.background_spawn(async move {
            let old = load(&actions, &repo, sides.old.as_ref(), &svg);
            let new = load(&actions, &repo, sides.new.as_ref(), &svg);
            (old, new)
        });
        cx.spawn(async move |this, cx| {
            let (old, new) = task.await;
            let _ = this.update(cx, |v, cx| {
                if v.doc != doc {
                    return; // reloaded meanwhile
                }
                v.release(cx);
                (v.old, v.new) = (old, new);
                cx.notify();
            });
        })
        .detach();
    }

    /// Frees the textures shown.
    fn release(&self, cx: &mut gpui_kit::App) {
        for p in [self.old.picture(), self.new.picture()]
            .into_iter()
            .flatten()
        {
            release(p.render.clone(), cx);
        }
    }

    /// The canvas both images are shown on: wide and tall enough for each.
    fn canvas(&self) -> (u32, u32) {
        [self.old.picture(), self.new.picture()]
            .into_iter()
            .flatten()
            .fold((1, 1), |(w, h), p| (w.max(p.width), h.max(p.height)))
    }

    /// One side's pane: its label and size, then its image.
    fn pane(&self, before: bool, cx: &Context<Self>) -> AnyElement {
        let (loaded, name) = if before {
            (&self.old, "Before")
        } else {
            (&self.new, "After")
        };
        let other = if before { &self.new } else { &self.old };
        let name = match other {
            Loaded::Absent if before => "Deleted",
            Loaded::Absent => "Added",
            _ => name,
        };
        let (label, body) = match loaded {
            Loaded::Ready(p) => (
                format!("{name} · {}", p.info()),
                stage(
                    if before { "image-old" } else { "image-new" },
                    self.canvas(),
                    vec![Layer::whole(p)],
                    self.zoom,
                    None,
                    cx,
                )
                .into_any_element(),
            ),
            Loaded::Loading => (name.to_owned(), note("Loading…", cx)),
            Loaded::Absent => (name.to_owned(), note("Not in this version", cx)),
            Loaded::Failed(e) => (name.to_owned(), note(e.clone(), cx)),
        };
        div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .child(info(label, cx).px_2().py_1().truncate())
            .child(body)
            .into_any_element()
    }
}

/// A muted note in place of an image.
fn note(text: impl Into<gpui_kit::SharedString>, cx: &Context<ImageDiff>) -> AnyElement {
    div()
        .p_3()
        .text_color(color(cx, T::FgMuted))
        .child(text.into())
        .into_any_element()
}

impl Render for ImageDiff {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let bar = div()
            .flex()
            .flex_none()
            .items_center()
            .gap_2()
            .px_2()
            .py_1()
            .border_b_1()
            .border_color(color(cx, T::BorderSubtle))
            .child(div().flex_1())
            .child(zoom_chips(self.zoom, cx, |v: &mut Self, z| v.zoom = z));
        let both = !matches!(self.old, Loaded::Absent) && !matches!(self.new, Loaded::Absent);
        let panes = div().flex_1().min_h_0().flex();
        let panes = if both {
            panes
                .child(self.pane(true, cx))
                .child(div().w(gpui_kit::px(1.0)).bg(color(cx, T::BorderSubtle)))
                .child(self.pane(false, cx))
        } else {
            panes.child(self.pane(matches!(self.new, Loaded::Absent), cx))
        };
        div()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .flex()
            .flex_col()
            .child(bar)
            .child(panes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use archipel_model::Oid;

    fn file(old: Option<&str>, new: Option<&str>, kind: ChangeKind, binary: bool) -> FileDiff {
        FileDiff {
            old_path: old.map(Into::into),
            new_path: new.map(Into::into),
            kind,
            binary,
            hunks: Vec::new(),
            truncated: false,
        }
    }

    fn version(source: Source, path: &str) -> Version {
        Version {
            source,
            path: path.into(),
        }
    }

    #[test]
    fn working_copy_sides_read_the_index() {
        let f = file(Some("a.png"), Some("a.png"), ChangeKind::Modified, true);
        let unstaged = sides(&f, Some(DiffSide::Unstaged), None, false).unwrap();
        assert_eq!(unstaged.old, Some(version(Source::Index, "a.png")));
        assert_eq!(unstaged.new, Some(version(Source::Worktree, "a.png")));
        let staged = sides(&f, Some(DiffSide::Staged), None, false).unwrap();
        assert_eq!(
            staged.old,
            Some(version(Source::Rev("HEAD".into()), "a.png"))
        );
        assert_eq!(staged.new, Some(version(Source::Index, "a.png")));
    }

    #[test]
    fn a_commit_diff_reads_its_parent_and_itself() {
        let oid = Oid::from_hex(&"ab".repeat(20)).unwrap();
        let revs = Revs::span(oid, oid);
        let f = file(Some("old.png"), Some("new.png"), ChangeKind::Renamed, true);
        let s = sides(&f, None, Some(&revs), false).unwrap();
        assert_eq!(
            s.old,
            Some(version(
                Source::Rev(format!("{}^", oid.to_hex())),
                "old.png"
            ))
        );
        assert_eq!(s.new, Some(version(Source::Rev(oid.to_hex()), "new.png")));
        assert_eq!(sides(&f, None, None, false), None);
    }

    #[test]
    fn added_and_deleted_images_have_one_side() {
        let added = file(None, Some("a.gif"), ChangeKind::Added, true);
        let s = sides(&added, Some(DiffSide::Staged), None, false).unwrap();
        assert!(s.old.is_none() && s.new.is_some());
        let deleted = file(Some("a.gif"), None, ChangeKind::Deleted, true);
        let s = sides(&deleted, Some(DiffSide::Unstaged), None, false).unwrap();
        assert!(s.old.is_some() && s.new.is_none());
    }

    #[test]
    fn text_files_and_text_svgs_stay_text_diffs() {
        let text = file(Some("a.rs"), Some("a.rs"), ChangeKind::Modified, false);
        assert_eq!(sides(&text, Some(DiffSide::Unstaged), None, true), None);
        let bin = file(Some("a.bin"), Some("a.bin"), ChangeKind::Modified, true);
        assert_eq!(sides(&bin, Some(DiffSide::Unstaged), None, true), None);
        let svg = file(Some("a.svg"), Some("a.svg"), ChangeKind::Modified, false);
        assert_eq!(sides(&svg, Some(DiffSide::Unstaged), None, false), None);
        assert!(sides(&svg, Some(DiffSide::Unstaged), None, true).is_some());
    }
}
