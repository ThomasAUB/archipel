//! Event path → [`Scope`] classification.
//!
//! [`classify`] is a pure function over a path relative to one of the
//! repository areas ([`Area`]). [`RepoClassifier`] wraps it for one repository:
//! it finds the area of an absolute event path and drops worktree paths that
//! Git ignores (`.gitignore` files, `.git/info/exclude`, global excludes).

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

use archipel_model::ports::Scope;
use ignore::Match;
use ignore::gitignore::{Gitignore, GitignoreBuilder};

use crate::layout::RepoLayout;

/// Where an event path lives, which decides how it is read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Area {
    /// The working tree (paths relative to its root).
    Worktree,
    /// The Git directory of a regular repository: per-worktree and shared
    /// files in one place.
    GitDir,
    /// The per-worktree Git directory of a linked worktree
    /// (`<common>/worktrees/<name>`).
    LinkedGitDir,
    /// The common directory shared with other worktrees (seen from a linked
    /// worktree): only refs, config and stash matter there.
    CommonDir,
}

/// Name of the Git directory inside a working tree.
const DOT_GIT: &str = ".git";
/// Suffix of Git's lock files (`index.lock`, `refs/heads/main.lock`, …).
const LOCK_SUFFIX: &str = ".lock";
/// Ref holding the stash.
const STASH_REF: &str = "refs/stash";
/// Reflog of the stash (the list of stash entries).
const STASH_LOG: &str = "logs/refs/stash";
/// Per-repository exclude file, relative to the Git directory.
const INFO_EXCLUDE: &str = "info/exclude";
/// Sparse-checkout patterns, relative to the Git directory.
const INFO_SPARSE: &str = "info/sparse-checkout";
/// Per-directory ignore file name.
const GITIGNORE: &str = ".gitignore";

/// State files whose presence means an operation is in progress.
const OP_STATE_FILES: &[&str] = &[
    "MERGE_HEAD",
    "CHERRY_PICK_HEAD",
    "REVERT_HEAD",
    "REBASE_HEAD",
    "BISECT_LOG",
];
/// State directories of in-progress operations.
const OP_STATE_DIRS: &[&str] = &["rebase-merge", "rebase-apply", "sequencer"];

/// Classifies a path relative to `area`.
///
/// Pure: looks only at the path text. Worktree paths are not checked against
/// ignore rules here (see [`RepoClassifier`]).
#[must_use]
pub fn classify(area: Area, rel: &Path) -> Scope {
    let Some(parts) = normal_parts(rel) else {
        return Scope::NONE;
    };
    if parts.is_empty() {
        return Scope::NONE;
    }
    match area {
        Area::Worktree => classify_worktree(&parts),
        Area::GitDir => classify_git(&parts, true, true),
        Area::LinkedGitDir => classify_git(&parts, true, false),
        Area::CommonDir => classify_git(&parts, false, true),
    }
}

fn classify_worktree(parts: &[&str]) -> Scope {
    // `.git` anywhere: our own Git dir (handled as a Git area by the caller),
    // the `.git` file of a linked worktree, or the internals of a nested repo.
    if parts.contains(&DOT_GIT) {
        Scope::NONE
    } else {
        Scope::STATUS
    }
}

/// `per_worktree`: the area holds `HEAD`, `index` and operation state of the
/// watched worktree. `shared`: it holds refs, config and stash.
fn classify_git(parts: &[&str], per_worktree: bool, shared: bool) -> Scope {
    let joined = parts.join("/");
    if joined.ends_with(LOCK_SUFFIX) {
        return Scope::NONE;
    }
    let first = parts[0];
    let op_state = Scope::OP_STATE | Scope::STATUS;
    let only_if = |cond: bool, scope: Scope| if cond { scope } else { Scope::NONE };
    match first {
        "refs" if joined == STASH_REF => Scope::STASH,
        // Shared refs, plus per-worktree refs (bisect, worktree, rewritten).
        "refs" => Scope::REFS,
        "logs" => only_if(shared && joined == STASH_LOG, Scope::STASH),
        "packed-refs" if parts.len() == 1 => only_if(shared, Scope::REFS),
        "config" if parts.len() == 1 => only_if(shared, Scope::CONFIG | Scope::REFS),
        "config.worktree" if parts.len() == 1 => only_if(per_worktree, Scope::CONFIG | Scope::REFS),
        "info" => only_if(
            shared && (joined == INFO_EXCLUDE || joined == INFO_SPARSE),
            Scope::STATUS,
        ),
        "HEAD" if parts.len() == 1 => only_if(per_worktree, Scope::REFS | Scope::STATUS),
        "index" if parts.len() == 1 => only_if(per_worktree, Scope::STATUS),
        "FETCH_HEAD" if parts.len() == 1 => Scope::FETCH | Scope::REFS,
        f if parts.len() == 1 && OP_STATE_FILES.contains(&f) => only_if(per_worktree, op_state),
        f if OP_STATE_DIRS.contains(&f) => only_if(per_worktree, op_state),
        // objects/**, hooks/**, worktrees/**, modules/**, logs/HEAD, ORIG_HEAD,
        // COMMIT_EDITMSG, gc.pid, temp files, …
        _ => Scope::NONE,
    }
}

/// Normal components as UTF-8, `None` if the path escapes (`..`), is rooted,
/// or is not valid UTF-8 (cannot be a Git state file; worktree files are
/// handled conservatively by the caller).
fn normal_parts(rel: &Path) -> Option<Vec<&str>> {
    let mut parts = Vec::new();
    for c in rel.components() {
        match c {
            Component::Normal(s) => parts.push(s.to_str()?),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(parts)
}

/// Classifies absolute event paths for one repository, applying ignore rules.
#[derive(Debug)]
pub struct RepoClassifier {
    layout: RepoLayout,
    /// Global excludes (`core.excludesFile`), matched with worktree-relative paths.
    global: Gitignore,
    /// Per-directory matchers, keyed by absolute directory; `None` = no rules.
    /// The worktree root's entry also holds `.git/info/exclude`.
    dirs: HashMap<PathBuf, Option<Gitignore>>,
}

impl RepoClassifier {
    /// Creates a classifier for `layout`. Ignore files are read lazily.
    #[must_use]
    pub fn new(layout: RepoLayout) -> Self {
        Self::with_global(layout, global_excludes())
    }

    /// Like [`RepoClassifier::new`] without global excludes (deterministic tests).
    #[must_use]
    pub fn without_global_excludes(layout: RepoLayout) -> Self {
        Self::with_global(layout, Gitignore::empty())
    }

    pub(crate) fn with_global(layout: RepoLayout, global: Gitignore) -> Self {
        Self {
            layout,
            global,
            dirs: HashMap::new(),
        }
    }

    /// The repository layout.
    #[must_use]
    pub fn layout(&self) -> &RepoLayout {
        &self.layout
    }

    /// Scope invalidated by a change at absolute path `path`
    /// ([`Scope::NONE`] when the path is not part of this repository).
    pub fn classify(&mut self, path: &Path) -> Scope {
        let linked = self.layout.git_dir != self.layout.common_dir;
        if let Ok(rel) = path.strip_prefix(&self.layout.git_dir) {
            let area = if linked {
                Area::LinkedGitDir
            } else {
                Area::GitDir
            };
            return self.classify_git_area(area, rel);
        }
        if linked {
            if let Ok(rel) = path.strip_prefix(&self.layout.common_dir) {
                return self.classify_git_area(Area::CommonDir, rel);
            }
        }
        let Some(worktree) = &self.layout.worktree else {
            return Scope::NONE;
        };
        let Ok(rel) = path.strip_prefix(worktree) else {
            return Scope::NONE;
        };
        let scope = classify(Area::Worktree, rel);
        if scope.is_empty() {
            return scope;
        }
        if path.file_name().is_some_and(|n| n == GITIGNORE) {
            // Rules changed: forget the cached matcher; the change itself
            // may change the status.
            if let Some(dir) = path.parent() {
                self.dirs.remove(dir);
            }
            return scope;
        }
        if self.is_ignored(path, rel) {
            Scope::NONE
        } else {
            scope
        }
    }

    fn classify_git_area(&mut self, area: Area, rel: &Path) -> Scope {
        let scope = classify(area, rel);
        if !scope.is_empty() && normal_parts(rel).is_some_and(|p| p.join("/") == INFO_EXCLUDE) {
            if let Some(worktree) = &self.layout.worktree {
                self.dirs.remove(worktree);
            }
        }
        scope
    }

    /// Whether Git ignores the worktree path (`rel` relative to the root).
    fn is_ignored(&mut self, path: &Path, rel: &Path) -> bool {
        let as_file = self.ignored_as(path, rel, false);
        let as_dir = self.ignored_as(path, rel, true);
        if as_file == as_dir {
            // Common case: no directory-only rule decides, no `stat` needed.
            as_file
        } else if path.is_dir() {
            // A deleted path cannot be stat'ed: it counts as a file; its
            // parents are still matched as directories.
            as_dir
        } else {
            as_file
        }
    }

    fn ignored_as(&mut self, path: &Path, rel: &Path, is_dir: bool) -> bool {
        let Some(worktree) = self.layout.worktree.clone() else {
            return false;
        };
        // Deepest directory first: the closest `.gitignore` wins.
        let mut dir = path.parent();
        while let Some(d) = dir {
            if !d.starts_with(&worktree) {
                break;
            }
            if let Ok(sub) = path.strip_prefix(d) {
                if let Some(m) = self.matcher(d, &worktree) {
                    match m.matched_path_or_any_parents(sub, is_dir) {
                        Match::Ignore(_) => return true,
                        Match::Whitelist(_) => return false,
                        Match::None => {}
                    }
                }
            }
            if d == worktree {
                break;
            }
            dir = d.parent();
        }
        self.global
            .matched_path_or_any_parents(rel, is_dir)
            .is_ignore()
    }

    fn matcher(&mut self, dir: &Path, worktree: &Path) -> Option<&Gitignore> {
        if !self.dirs.contains_key(dir) {
            let mut builder = GitignoreBuilder::new(dir);
            let mut any = false;
            if dir == worktree {
                // Lowest priority first: later rules win inside one matcher.
                any |= add_if_exists(&mut builder, &self.layout.common_dir.join(INFO_EXCLUDE));
            }
            any |= add_if_exists(&mut builder, &dir.join(GITIGNORE));
            let built = if any {
                builder
                    .build()
                    .map_err(|err| tracing::debug!(%err, dir = %dir.display(), "bad ignore rules"))
                    .ok()
            } else {
                None
            };
            self.dirs.insert(dir.to_path_buf(), built);
        }
        self.dirs.get(dir).and_then(Option::as_ref)
    }
}

/// The user's global excludes (`core.excludesFile`), empty when unreadable.
pub(crate) fn global_excludes() -> Gitignore {
    let (global, err) = Gitignore::global();
    if let Some(err) = err {
        tracing::debug!(%err, "global excludes partially unreadable");
    }
    global
}

fn add_if_exists(builder: &mut GitignoreBuilder, file: &Path) -> bool {
    if !file.is_file() {
        return false;
    }
    if let Some(err) = builder.add(file) {
        tracing::debug!(%err, file = %file.display(), "ignore file partially unreadable");
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: Scope = Scope::STATUS;
    const R: Scope = Scope::REFS;
    const N: Scope = Scope::NONE;

    fn op() -> Scope {
        Scope::OP_STATE | Scope::STATUS
    }

    #[test]
    fn classification_table() {
        let table: Vec<(Area, &str, Scope)> = vec![
            // Worktree
            (Area::Worktree, "src/main.rs", S),
            (Area::Worktree, "README.md", S),
            (Area::Worktree, ".gitignore", S),
            (Area::Worktree, "", N),
            (Area::Worktree, ".git", N),
            (Area::Worktree, "vendor/lib/.git/index", N),
            // Regular Git dir
            (Area::GitDir, "index", S),
            (Area::GitDir, "index.lock", N),
            (Area::GitDir, "HEAD", R | S),
            (Area::GitDir, "HEAD.lock", N),
            (Area::GitDir, "refs/heads/main", R),
            (Area::GitDir, "refs/heads/feature/x", R),
            (Area::GitDir, "refs/heads/main.lock", N),
            (Area::GitDir, "refs/tags/v1", R),
            (Area::GitDir, "refs/remotes/origin/main", R),
            (Area::GitDir, "refs", R),
            (Area::GitDir, "packed-refs", R),
            (Area::GitDir, "packed-refs.lock", N),
            (Area::GitDir, "refs/stash", Scope::STASH),
            (Area::GitDir, "logs/refs/stash", Scope::STASH),
            (Area::GitDir, "logs/HEAD", N),
            (Area::GitDir, "logs/refs/heads/main", N),
            (Area::GitDir, "FETCH_HEAD", Scope::FETCH | R),
            (Area::GitDir, "MERGE_HEAD", op()),
            (Area::GitDir, "CHERRY_PICK_HEAD", op()),
            (Area::GitDir, "REVERT_HEAD", op()),
            (Area::GitDir, "REBASE_HEAD", op()),
            (Area::GitDir, "BISECT_LOG", op()),
            (Area::GitDir, "rebase-merge", op()),
            (Area::GitDir, "rebase-merge/done", op()),
            (Area::GitDir, "rebase-apply/next", op()),
            (Area::GitDir, "sequencer/todo", op()),
            (Area::GitDir, "config", Scope::CONFIG | R),
            (Area::GitDir, "config.lock", N),
            (Area::GitDir, "info/exclude", S),
            (Area::GitDir, "info/refs", N),
            (Area::GitDir, "objects/ab/cdef0123", N),
            (Area::GitDir, "objects/pack/pack-1.pack", N),
            (Area::GitDir, "objects/incoming-xyz/tmp_obj_1", N),
            (Area::GitDir, "ORIG_HEAD", N),
            (Area::GitDir, "COMMIT_EDITMSG", N),
            (Area::GitDir, "gc.pid", N),
            (Area::GitDir, "hooks/pre-commit", N),
            (Area::GitDir, "worktrees/wt/HEAD", N),
            (Area::GitDir, "modules/sub/HEAD", N),
            (Area::GitDir, "description", N),
            (Area::GitDir, "", N),
            // Linked worktree: own Git dir
            (Area::LinkedGitDir, "HEAD", R | S),
            (Area::LinkedGitDir, "index", S),
            (Area::LinkedGitDir, "MERGE_HEAD", op()),
            (Area::LinkedGitDir, "rebase-merge/done", op()),
            (Area::LinkedGitDir, "refs/bisect/bad", R),
            (Area::LinkedGitDir, "config.worktree", Scope::CONFIG | R),
            (Area::LinkedGitDir, "FETCH_HEAD", Scope::FETCH | R),
            (Area::LinkedGitDir, "commondir", N),
            (Area::LinkedGitDir, "gitdir", N),
            // Linked worktree: common dir (main worktree's per-worktree files ignored)
            (Area::CommonDir, "HEAD", N),
            (Area::CommonDir, "index", N),
            (Area::CommonDir, "MERGE_HEAD", N),
            (Area::CommonDir, "rebase-merge/done", N),
            (Area::CommonDir, "refs/heads/main", R),
            (Area::CommonDir, "refs/stash", Scope::STASH),
            (Area::CommonDir, "logs/refs/stash", Scope::STASH),
            (Area::CommonDir, "packed-refs", R),
            (Area::CommonDir, "config", Scope::CONFIG | R),
            (Area::CommonDir, "worktrees/wt/HEAD", N),
            (Area::CommonDir, "objects/ab/cd", N),
            (Area::CommonDir, "info/exclude", S),
        ];
        for (area, path, expected) in table {
            assert_eq!(
                classify(area, Path::new(path)),
                expected,
                "{area:?} {path:?}"
            );
        }
    }

    #[test]
    fn escaping_paths_are_ignored() {
        assert_eq!(classify(Area::Worktree, Path::new("../x")), N);
        assert_eq!(classify(Area::GitDir, Path::new("./HEAD")), R | S);
    }

    fn layout_for(root: &Path) -> RepoLayout {
        std::fs::create_dir_all(root.join(".git/info")).unwrap();
        RepoLayout {
            worktree: Some(root.to_path_buf()),
            git_dir: root.join(".git"),
            common_dir: root.join(".git"),
        }
    }

    #[test]
    fn ignore_rules_drop_worktree_events() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let layout = layout_for(root);
        std::fs::write(root.join(".gitignore"), "*.log\ntarget/\n!keep.log\n").unwrap();
        std::fs::write(root.join(".git/info/exclude"), "secret.txt\n").unwrap();
        std::fs::create_dir_all(root.join("sub/deep")).unwrap();
        std::fs::write(root.join("sub/.gitignore"), "local.tmp\n").unwrap();
        std::fs::create_dir_all(root.join("target/debug")).unwrap();
        let mut c = RepoClassifier::without_global_excludes(layout);

        let table: Vec<(&str, Scope)> = vec![
            ("src/main.rs", S),
            ("app.log", N),
            ("sub/deep/app.log", N),
            ("keep.log", S),
            ("target", N),
            ("target/debug/out.bin", N),
            ("secret.txt", N),
            ("sub/local.tmp", N),
            ("sub/deep/local.tmp", N),
            ("local.tmp", S),
            (".gitignore", S),
            (".git/index", S),
            (".git/index.lock", N),
            (".git/refs/heads/main", R),
        ];
        for (rel, expected) in table {
            assert_eq!(c.classify(&root.join(rel)), expected, "{rel}");
        }
        assert_eq!(c.classify(Path::new("/elsewhere/file")), N);
    }

    #[test]
    fn ignore_cache_is_refreshed_when_rules_change() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let mut c = RepoClassifier::without_global_excludes(layout_for(root));
        assert_eq!(c.classify(&root.join("a.tmp")), S);
        std::fs::write(root.join(".gitignore"), "*.tmp\n").unwrap();
        assert_eq!(c.classify(&root.join(".gitignore")), S);
        assert_eq!(c.classify(&root.join("a.tmp")), N);
        std::fs::write(root.join(".git/info/exclude"), "b.txt\n").unwrap();
        assert_eq!(c.classify(&root.join(".git/info/exclude")), S);
        assert_eq!(c.classify(&root.join("b.txt")), N);
        std::fs::write(root.join(".git/info/exclude"), "").unwrap();
        assert_eq!(c.classify(&root.join(".git/info/exclude")), S);
        assert_eq!(c.classify(&root.join("b.txt")), S);
    }

    #[test]
    fn linked_worktree_areas() {
        let dir = tempfile::tempdir().unwrap();
        let common = dir.path().join("main/.git");
        let git_dir = common.join("worktrees/wt");
        let wt = dir.path().join("wt");
        let mut c = RepoClassifier::without_global_excludes(RepoLayout {
            worktree: Some(wt.clone()),
            git_dir: git_dir.clone(),
            common_dir: common.clone(),
        });
        assert_eq!(c.classify(&git_dir.join("HEAD")), R | S);
        assert_eq!(c.classify(&common.join("HEAD")), N);
        assert_eq!(c.classify(&common.join("refs/heads/main")), R);
        assert_eq!(c.classify(&common.join("worktrees/other/HEAD")), N);
        assert_eq!(c.classify(&wt.join("file.txt")), S);
        assert_eq!(c.classify(&wt.join(".git")), N);
    }
}
