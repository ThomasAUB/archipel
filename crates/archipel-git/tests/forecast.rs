//! (Test file: unwrap/expect/panic are the expected failure mode in tests.)
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The conflict forecast of the CLI adapter, on real temporary repositories.

use archipel_fixtures::TestRepo;
use archipel_git::GitCli;
use archipel_model::forecast::MergeStrategy;
use archipel_model::ports::MergeForecaster;
use archipel_model::{CancelToken, Oid, RepoPath};

/// A repo with `a.txt` committed on `main`, and branch `other` created there.
fn setup() -> (TestRepo, RepoPath, GitCli) {
    let repo = TestRepo::new().unwrap();
    repo.write("a.txt", "one\ntwo\nthree\n").unwrap();
    repo.commit_all("initial").unwrap();
    repo.git(&["branch", "other"]).unwrap();
    let path = RepoPath::new(repo.path());
    (repo, path, GitCli::default())
}

fn oid(repo: &TestRepo, rev: &str) -> Oid {
    Oid::from_hex(&repo.git(&["rev-parse", rev]).unwrap()).unwrap()
}

/// Commits `content` to `file` on `branch`.
fn commit_on(repo: &TestRepo, branch: &str, file: &str, content: &str) {
    repo.git(&["checkout", "-q", branch]).unwrap();
    repo.write(file, content).unwrap();
    repo.commit_all(&format!("{branch}: {file}")).unwrap();
}

fn forecast(
    repo: &TestRepo,
    cli: &GitCli,
    strategy: MergeStrategy,
) -> (MergeStrategy, Vec<String>) {
    let path = RepoPath::new(repo.path());
    let f = cli
        .forecast(
            &path,
            oid(repo, "main"),
            oid(repo, "other"),
            strategy,
            &CancelToken::new(),
        )
        .unwrap();
    (f.strategy, f.conflicts)
}

#[test]
fn merge_conflicts_are_listed_without_touching_the_repo() {
    let (repo, _, cli) = setup();
    commit_on(&repo, "other", "a.txt", "one\nTWO\nthree\n");
    commit_on(&repo, "other", "b.txt", "theirs\n");
    commit_on(&repo, "main", "a.txt", "one\ndeux\nthree\n");
    commit_on(&repo, "main", "b.txt", "ours\n");
    repo.write("dirty.txt", "local\n").unwrap();
    let refs_before = repo.git(&["for-each-ref"]).unwrap();
    let status_before = repo.git(&["status", "--porcelain"]).unwrap();

    let (strategy, conflicts) = forecast(&repo, &cli, MergeStrategy::Merge);
    assert_eq!(strategy, MergeStrategy::Merge);
    assert_eq!(conflicts, ["a.txt", "b.txt"]);

    assert_eq!(repo.git(&["for-each-ref"]).unwrap(), refs_before);
    assert_eq!(repo.git(&["status", "--porcelain"]).unwrap(), status_before);
    assert_eq!(
        repo.git(&["rev-parse", "--abbrev-ref", "HEAD"]).unwrap(),
        "main"
    );
}

#[test]
fn separate_changes_merge_cleanly() {
    let (repo, _, cli) = setup();
    commit_on(&repo, "other", "b.txt", "theirs\n");
    commit_on(&repo, "main", "c.txt", "ours\n");
    assert_eq!(
        forecast(&repo, &cli, MergeStrategy::Merge).1,
        Vec::<String>::new()
    );
    assert_eq!(
        forecast(&repo, &cli, MergeStrategy::Rebase).1,
        Vec::<String>::new()
    );
}

#[test]
fn a_rebase_can_conflict_where_a_merge_does_not() {
    let (repo, _, cli) = setup();
    commit_on(&repo, "other", "a.txt", "one\nTWO\nthree\n");
    // Changed, then changed back: nothing left for a merge to conflict on,
    // but replaying the first commit conflicts.
    commit_on(&repo, "main", "a.txt", "one\ndeux\nthree\n");
    commit_on(&repo, "main", "a.txt", "one\ntwo\nthree\n");
    assert_eq!(
        forecast(&repo, &cli, MergeStrategy::Merge).1,
        Vec::<String>::new()
    );
    let (strategy, conflicts) = forecast(&repo, &cli, MergeStrategy::Rebase);
    assert_eq!(strategy, MergeStrategy::Rebase);
    assert_eq!(conflicts, ["a.txt"]);
}

#[test]
fn a_rebase_replays_each_commit_on_the_previous_one() {
    let (repo, _, cli) = setup();
    commit_on(&repo, "other", "a.txt", "one\nTWO\nthree\n");
    // The second commit only applies on top of the first.
    commit_on(&repo, "main", "b.txt", "first\n");
    commit_on(&repo, "main", "b.txt", "second\n");
    assert_eq!(
        forecast(&repo, &cli, MergeStrategy::Rebase).1,
        Vec::<String>::new()
    );
}

#[test]
fn pull_strategy_follows_the_config() {
    let (repo, path, cli) = setup();
    assert_eq!(
        cli.pull_strategy(&path, "main").unwrap(),
        MergeStrategy::Merge
    );
    repo.git(&["config", "pull.rebase", "true"]).unwrap();
    assert_eq!(
        cli.pull_strategy(&path, "main").unwrap(),
        MergeStrategy::Rebase
    );
    repo.git(&["config", "branch.main.rebase", "false"])
        .unwrap();
    assert_eq!(
        cli.pull_strategy(&path, "main").unwrap(),
        MergeStrategy::Merge
    );
    assert_eq!(
        cli.pull_strategy(&path, "other").unwrap(),
        MergeStrategy::Rebase
    );
}

#[test]
fn incoming_paths_are_those_changed_on_the_other_side() {
    let (repo, path, cli) = setup();
    commit_on(&repo, "other", "c.txt", "c\n");
    commit_on(&repo, "other", "a.txt", "one\nTWO\nthree\n");
    commit_on(&repo, "main", "b.txt", "ours\n");
    let paths = cli
        .incoming_paths(&path, oid(&repo, "main"), oid(&repo, "other"))
        .unwrap();
    assert_eq!(paths, ["a.txt", "c.txt"]);
}
