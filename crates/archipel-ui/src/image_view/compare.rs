//! The image diff: a one-file diff of an image shows the image before and
//! after, read and decoded off the UI thread, compared side by side, by
//! swiping, as an onion skin, blinking, or by their pixel difference (see
//! [`view`]). Both sit top-left on a canvas as large as the larger one, so
//! they share one scale. An added or deleted image shows alone.

mod view;

use std::time::Duration;

use archipel_model::RepoPath;
use archipel_model::changes::ChangeKind;
use archipel_model::diff::FileDiff;
use archipel_model::ports::DiffSide;
use archipel_ops::RepoActions;
use gpui_kit::component::slider::{SliderEvent, SliderState};
use gpui_kit::prelude::*;
use gpui_kit::{Context, Entity, ImageFormat, Subscription, SvgRenderer, Task};

use super::stage::{Placed, Zoom};
use super::{Difference, Picture, decode, difference, image_format, release};
use crate::diff_view::Revs;

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
pub(crate) enum Loaded {
    Loading,
    /// Not on this side.
    Absent,
    Failed(String),
    Ready(Picture),
}

impl Loaded {
    pub(super) fn picture(&self) -> Option<&Picture> {
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

/// How the two images are compared.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Mode {
    /// Next to each other.
    #[default]
    SideBySide,
    /// One canvas, before left of a draggable split, after right of it.
    Swipe,
    /// After over before, at an opacity set with a slider.
    Onion,
    /// One at a time: after, before while a button is held or every
    /// [`BLINK`] in turn.
    Blink,
    /// `|before − after|` per pixel.
    Difference,
}

impl Mode {
    /// Every mode, in the order offered.
    pub(crate) const ALL: [Self; 5] = [
        Self::SideBySide,
        Self::Swipe,
        Self::Onion,
        Self::Blink,
        Self::Difference,
    ];

    /// Its name on its chip.
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::SideBySide => "Side by side",
            Self::Swipe => "Swipe",
            Self::Onion => "Onion skin",
            Self::Blink => "Blink",
            Self::Difference => "Difference",
        }
    }
}

/// Time each image shows while blinking on its own.
pub(crate) const BLINK: Duration = Duration::from_millis(500);

/// The choices kept from one image diff to the next in a view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Prefs {
    pub(crate) mode: Mode,
    pub(crate) zoom: Zoom,
    /// Differences shown ×4.
    pub(crate) amplify: bool,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            mode: Mode::SideBySide,
            zoom: Zoom::Fit,
            amplify: true,
        }
    }
}

/// The difference image, for one `amplify` setting.
pub(crate) enum Computed {
    Pending,
    Done(Option<Difference>),
}

/// The image diff's state.
pub(crate) struct ImageDiff {
    repo: RepoPath,
    sides: Sides,
    /// The diff it was read for (its address): a reloaded diff reads the
    /// images again.
    doc: usize,
    pub(super) old: Loaded,
    pub(super) new: Loaded,
    pub(super) prefs: Prefs,
    /// Swipe split, as a fraction of the canvas width.
    pub(super) split: f32,
    /// The swipe split follows the pointer.
    pub(super) dragging: bool,
    /// Where the swipe canvas was painted.
    pub(super) placed: Placed,
    /// Onion skin: the opacity of the image after, 0 – 100.
    pub(super) onion: Entity<SliderState>,
    /// Blink: the button showing the image before is held.
    pub(super) held: bool,
    /// Blink on its own: the image before shows.
    pub(super) flipped: bool,
    /// Blink on its own: the timer, dropped to stop.
    pub(super) auto: Option<Task<()>>,
    /// The difference, with the `amplify` it was computed for.
    pub(super) difference: Option<(bool, Computed)>,
    _onion_sub: Subscription,
}

impl ImageDiff {
    /// The image diff of `sides`, read for diff `doc`, with the choices
    /// `prefs`; its textures are freed with it.
    pub(crate) fn new(
        actions: RepoActions,
        repo: RepoPath,
        sides: Sides,
        doc: usize,
        prefs: Prefs,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.on_release(|v: &mut Self, cx| v.release(cx)).detach();
        let onion = cx.new(|_| {
            SliderState::new()
                .min(0.0)
                .max(100.0)
                .step(1.0)
                .default_value(50.0)
        });
        let onion_sub = cx.subscribe(&onion, |_, _, _: &SliderEvent, cx| cx.notify());
        let mut diff = Self {
            repo,
            sides,
            doc,
            old: Loaded::Loading,
            new: Loaded::Loading,
            prefs,
            split: 0.5,
            dragging: false,
            placed: Placed::default(),
            onion,
            held: false,
            flipped: false,
            auto: None,
            difference: None,
            _onion_sub: onion_sub,
        };
        diff.load(actions, cx);
        diff
    }

    /// Whether it shows `sides` of `repo`.
    pub(crate) fn shows(&self, repo: &RepoPath, sides: &Sides) -> bool {
        self.repo == *repo && self.sides == *sides
    }

    /// Its choices, for the next image diff.
    pub(crate) fn prefs(&self) -> Prefs {
        self.prefs
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
                v.difference = None;
                v.compute_difference(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// Frees the textures shown.
    fn release(&self, cx: &mut gpui_kit::App) {
        let difference = match &self.difference {
            Some((_, Computed::Done(Some(d)))) => Some(&d.picture),
            _ => None,
        };
        for p in [self.old.picture(), self.new.picture(), difference]
            .into_iter()
            .flatten()
        {
            release(p.render.clone(), cx);
        }
    }

    /// Both images, once read; `None` while one is missing.
    pub(super) fn pair(&self) -> Option<(&Picture, &Picture)> {
        self.old.picture().zip(self.new.picture())
    }

    /// Shows the images in `mode`.
    pub(super) fn set_mode(&mut self, mode: Mode, cx: &mut Context<Self>) {
        self.prefs.mode = mode;
        if mode != Mode::Blink {
            self.set_auto(false, cx);
        }
        self.compute_difference(cx);
        cx.notify();
    }

    /// Shows the difference ×4 (`amplify`) or as it is.
    pub(super) fn set_amplify(&mut self, amplify: bool, cx: &mut Context<Self>) {
        self.prefs.amplify = amplify;
        self.compute_difference(cx);
        cx.notify();
    }

    /// Starts or stops blinking on its own.
    pub(super) fn set_auto(&mut self, on: bool, cx: &mut Context<Self>) {
        self.flipped = false;
        self.auto = on.then(|| {
            cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor().timer(BLINK).await;
                    let flipped = this.update(cx, |v, cx| {
                        v.flipped = !v.flipped;
                        cx.notify();
                    });
                    if flipped.is_err() {
                        break;
                    }
                }
            })
        });
        cx.notify();
    }

    /// Computes the difference (off the UI thread) when it shows and is
    /// not computed for the current amplification yet.
    fn compute_difference(&mut self, cx: &mut Context<Self>) {
        let amplify = self.prefs.amplify;
        if self.prefs.mode != Mode::Difference
            || self.difference.as_ref().is_some_and(|(a, _)| *a == amplify)
        {
            return;
        }
        let Some((a, b)) = self.pair().map(|(a, b)| (a.clone(), b.clone())) else {
            return;
        };
        if let Some((_, Computed::Done(Some(d)))) = self.difference.take() {
            release(d.picture.render, cx);
        }
        self.difference = Some((amplify, Computed::Pending));
        let doc = self.doc;
        let task = cx.background_spawn(async move { difference(&a, &b, amplify) });
        cx.spawn(async move |this, cx| {
            let done = task.await;
            let _ = this.update(cx, |v, cx| {
                let current = v.doc == doc
                    && matches!(v.difference, Some((a, Computed::Pending)) if a == amplify);
                if current {
                    v.difference = Some((amplify, Computed::Done(done)));
                    cx.notify();
                } else if let Some(d) = done {
                    release(d.picture.render, cx);
                }
            });
        })
        .detach();
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
