//! A multi-repo project covering every dashboard state.

use std::path::{Path, PathBuf};
use std::thread;

use anyhow::{Result, anyhow};

use crate::{configure, git, write_file};

/// The state a generated repository is left in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RepoState {
    /// Clean, in sync with its remote.
    Clean,
    /// Modified tracked files.
    Dirty,
    /// Staged changes.
    Staged,
    /// Untracked files only.
    Untracked,
    /// Local commits not pushed.
    Ahead,
    /// Remote commits not pulled (fetched).
    Behind,
    /// Both ahead and behind.
    Diverged,
    /// Detached `HEAD`.
    Detached,
    /// One stash entry, clean worktree.
    Stashed,
    /// A merge stopped on a conflict.
    MergeConflict,
}

impl RepoState {
    /// The cycle of states used by [`generate_project`].
    pub const CYCLE: [Self; 10] = [
        Self::Clean,
        Self::Dirty,
        Self::Staged,
        Self::Untracked,
        Self::Ahead,
        Self::Behind,
        Self::Diverged,
        Self::Detached,
        Self::Stashed,
        Self::MergeConflict,
    ];
}

/// A generated repository.
#[derive(Clone, Debug)]
pub struct GeneratedRepo {
    /// Working tree.
    pub path: PathBuf,
    /// State it was left in.
    pub state: RepoState,
}

/// Generates `count` repositories under `root/repos`, with bare remotes under
/// `root/remotes`, cycling through [`RepoState::CYCLE`]. Repos are built in
/// parallel. Each repo gets `commits` commits and a tag.
pub fn generate_project(root: &Path, count: usize, commits: usize) -> Result<Vec<GeneratedRepo>> {
    std::fs::create_dir_all(root.join("repos"))?;
    std::fs::create_dir_all(root.join("remotes"))?;
    let handles: Vec<_> = (0..count)
        .map(|i| {
            let root = root.to_path_buf();
            let state = RepoState::CYCLE[i % RepoState::CYCLE.len()];
            thread::spawn(move || generate_repo(&root, i, state, commits))
        })
        .collect();
    handles
        .into_iter()
        .map(|h| h.join().map_err(|_| anyhow!("fixture thread panicked"))?)
        .collect()
}

fn generate_repo(root: &Path, i: usize, state: RepoState, commits: usize) -> Result<GeneratedRepo> {
    let name = format!("repo-{i:02}");
    let remote = root.join("remotes").join(format!("{name}.git"));
    let work = root.join("repos").join(&name);
    git(
        root,
        &["init", "-q", "--bare", "-b", "main", &path_str(&remote)?],
    )?;
    git(root, &["init", "-q", "-b", "main", &path_str(&work)?])?;
    configure(&work)?;
    git(&work, &["remote", "add", "origin", &path_str(&remote)?])?;
    for c in 0..commits.max(1) {
        write_file(
            &work,
            &format!("src/file{}.txt", c % 5),
            &format!("{name} line {c}\n"),
        )?;
        git(&work, &["add", "-A"])?;
        git(
            &work,
            &["commit", "-q", "-m", &format!("{name}: commit {c}")],
        )?;
        if c == commits / 2 {
            git(&work, &["tag", "-a", "v1.0.0", "-m", "release 1.0.0"])?;
        }
    }
    git(&work, &["push", "-q", "-u", "origin", "main"])?;
    apply_state(root, &work, &remote, state)?;
    Ok(GeneratedRepo { path: work, state })
}

fn apply_state(root: &Path, work: &Path, remote: &Path, state: RepoState) -> Result<()> {
    match state {
        RepoState::Clean => {}
        RepoState::Dirty => write_file(work, "src/file0.txt", "changed\n")?,
        RepoState::Staged => {
            write_file(work, "src/new.txt", "new\n")?;
            git(work, &["add", "src/new.txt"])?;
        }
        RepoState::Untracked => write_file(work, "notes.txt", "todo\n")?,
        RepoState::Ahead => local_commit(work, "local work")?,
        RepoState::Behind => push_from_other_clone(root, work, remote)?,
        RepoState::Diverged => {
            push_from_other_clone(root, work, remote)?;
            local_commit(work, "local diverging work")?;
        }
        RepoState::Detached => {
            git(work, &["checkout", "-q", "--detach", "HEAD~1"])?;
        }
        RepoState::Stashed => {
            write_file(work, "src/file1.txt", "stashed change\n")?;
            git(work, &["stash", "push", "-q", "-m", "wip"])?;
        }
        RepoState::MergeConflict => {
            git(work, &["checkout", "-q", "-b", "topic"])?;
            write_file(work, "src/file0.txt", "topic side\n")?;
            git(work, &["commit", "-q", "-am", "topic change"])?;
            git(work, &["checkout", "-q", "main"])?;
            write_file(work, "src/file0.txt", "main side\n")?;
            git(work, &["commit", "-q", "-am", "main change"])?;
            // Expected to fail with a conflict.
            let _ = crate::git_output(work, &["merge", "topic"])?;
        }
    }
    Ok(())
}

fn local_commit(work: &Path, message: &str) -> Result<()> {
    write_file(work, "local.txt", message)?;
    git(work, &["add", "-A"])?;
    git(work, &["commit", "-q", "-m", message])?;
    Ok(())
}

fn push_from_other_clone(root: &Path, work: &Path, remote: &Path) -> Result<()> {
    let name = work
        .file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    let other = root.join("others").join(name);
    std::fs::create_dir_all(root.join("others"))?;
    git(
        root,
        &["clone", "-q", &path_str(remote)?, &path_str(&other)?],
    )?;
    configure(&other)?;
    write_file(&other, "upstream.txt", "from a teammate\n")?;
    git(&other, &["add", "-A"])?;
    git(&other, &["commit", "-q", "-m", "teammate commit"])?;
    git(&other, &["push", "-q", "origin", "main"])?;
    git(work, &["fetch", "-q", "origin"])?;
    Ok(())
}

fn path_str(path: &Path) -> Result<String> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| anyhow!("non UTF-8 path {}", path.display()))
}
