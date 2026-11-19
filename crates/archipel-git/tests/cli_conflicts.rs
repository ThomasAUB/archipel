//! (Test file: unwrap/expect/panic are the expected failure mode in tests.)
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The conflict port of the CLI adapter (the merge view), on real
//! temporary repositories: reading the three versions of a conflicted
//! file, resolving it, and bringing the conflict back.

use archipel_fixtures::TestRepo;
use archipel_git::GitCli;
use archipel_model::RepoPath;
use archipel_model::changes::FileState;
use archipel_model::ports::{ConflictResolver, HistoryRewriter, StatusReader, WorktreeSnapshots};
use archipel_model::requests::MergeMode;

/// A merge of `side` into `main` stopped on conflicts: `a.txt` edited on
/// both sides, `gone.txt` edited on main and deleted on side.
fn conflicted() -> (TestRepo, RepoPath, GitCli) {
    let repo = TestRepo::new().unwrap();
    repo.write("a.txt", "one\ntwo\nthree\n").unwrap();
    repo.write("gone.txt", "kept?\n").unwrap();
    repo.commit_all("initial").unwrap();
    repo.git(&["checkout", "-q", "-b", "side"]).unwrap();
    repo.write("a.txt", "one\nside\nthree\n").unwrap();
    repo.git(&["rm", "-q", "gone.txt"]).unwrap();
    repo.commit_all("side").unwrap();
    repo.git(&["checkout", "-q", "main"]).unwrap();
    repo.write("a.txt", "one\nmain\nthree\n").unwrap();
    repo.write("gone.txt", "edited on main\n").unwrap();
    repo.commit_all("main").unwrap();
    let path = RepoPath::new(repo.path());
    let cli = GitCli::default();
    assert!(cli.merge(&path, "side", MergeMode::NoFastForward).is_err());
    (repo, path, cli)
}

fn state(cli: &GitCli, path: &RepoPath, file: &str) -> Option<FileState> {
    cli.changes(path)
        .unwrap()
        .into_iter()
        .find(|c| c.path == file)
        .map(|c| c.state)
}

#[test]
fn reads_the_three_versions_of_a_conflicted_file() {
    let (_repo, path, cli) = conflicted();
    let sides = cli.conflict_sides(&path, "a.txt").unwrap();
    assert_eq!(sides.base.as_deref(), Some(&b"one\ntwo\nthree\n"[..]));
    assert_eq!(sides.ours.as_deref(), Some(&b"one\nmain\nthree\n"[..]));
    assert_eq!(sides.theirs.as_deref(), Some(&b"one\nside\nthree\n"[..]));

    // Deleted on their side: no stage 3.
    let sides = cli.conflict_sides(&path, "gone.txt").unwrap();
    assert_eq!(sides.ours.as_deref(), Some(&b"edited on main\n"[..]));
    assert_eq!(sides.theirs, None);

    assert!(cli.conflict_sides(&path, "missing.txt").is_err());
}

#[test]
fn resolves_with_contents_or_by_deleting() {
    let (repo, path, cli) = conflicted();
    cli.resolve_conflict(&path, "a.txt", Some(b"one\nboth\nthree\n"))
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(repo.path().join("a.txt")).unwrap(),
        "one\nboth\nthree\n"
    );
    assert!(matches!(
        state(&cli, &path, "a.txt"),
        Some(FileState::Tracked { .. })
    ));
    assert!(cli.conflict_sides(&path, "a.txt").is_err());

    cli.resolve_conflict(&path, "gone.txt", None).unwrap();
    assert!(!repo.path().join("gone.txt").exists());
    assert!(matches!(
        state(&cli, &path, "gone.txt"),
        Some(FileState::Tracked { .. })
    ));
}

#[test]
fn brings_a_resolved_conflict_back_as_it_was() {
    let (repo, path, cli) = conflicted();
    // Half edited before resolving: the undo brings back that text.
    let edited = std::fs::read_to_string(repo.path().join("a.txt"))
        .unwrap()
        .replace("three", "THREE");
    repo.write("a.txt", &edited).unwrap();
    let paths = ["a.txt".to_owned(), "gone.txt".to_owned()];
    let snapshot = cli.snapshot_paths(&path, &paths).unwrap().unwrap();
    cli.resolve_conflict(&path, "a.txt", Some(b"resolved\n"))
        .unwrap();
    cli.resolve_conflict(&path, "gone.txt", None).unwrap();

    cli.unresolve_conflicts(&path, &paths, snapshot).unwrap();
    assert_eq!(state(&cli, &path, "a.txt"), Some(FileState::Conflicted));
    assert_eq!(state(&cli, &path, "gone.txt"), Some(FileState::Conflicted));
    assert_eq!(
        std::fs::read_to_string(repo.path().join("a.txt")).unwrap(),
        edited
    );
    assert_eq!(
        std::fs::read_to_string(repo.path().join("gone.txt")).unwrap(),
        "edited on main\n"
    );
    let sides = cli.conflict_sides(&path, "a.txt").unwrap();
    assert_eq!(sides.theirs.as_deref(), Some(&b"one\nside\nthree\n"[..]));

    // Conflicted again: nothing to bring back.
    assert!(cli.unresolve_conflicts(&path, &paths, snapshot).is_err());
}

#[test]
fn refuses_to_bring_back_a_conflict_edited_since() {
    let (repo, path, cli) = conflicted();
    let paths = ["a.txt".to_owned()];
    let snapshot = cli.snapshot_paths(&path, &paths).unwrap().unwrap();
    cli.resolve_conflict(&path, "a.txt", Some(b"resolved\n"))
        .unwrap();
    repo.write("a.txt", "edited after\n").unwrap();
    assert!(cli.unresolve_conflicts(&path, &paths, snapshot).is_err());
    assert_eq!(
        std::fs::read_to_string(repo.path().join("a.txt")).unwrap(),
        "edited after\n"
    );
}
