//! (Test file: unwrap/expect/panic are the expected failure mode in tests.)
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Submodule reads and updates of the CLI adapter on real repositories.

use archipel_fixtures::TestRepo;
use archipel_git::GitCli;
use archipel_model::ports::{
    IndexWriter, StashWriter, StatusReader, SubmoduleReader, SubmoduleWriter,
};
use archipel_model::requests::SubmoduleUpdate;
use archipel_model::{CancelToken, NoProgress, Oid, RepoPath};

/// A parent repository with one submodule at `libs/lib`; returns the
/// parent, the library and the pinned commit.
fn parent_with_submodule() -> (TestRepo, TestRepo, Oid) {
    let lib = TestRepo::new().unwrap();
    lib.write("lib.txt", "v1\n").unwrap();
    let pinned = Oid::from_hex(&lib.commit_all("lib v1").unwrap()).unwrap();
    let parent = TestRepo::new().unwrap();
    parent.write("app.txt", "app\n").unwrap();
    parent.commit_all("app").unwrap();
    let url = lib.path().to_string_lossy().replace('\\', "/");
    parent
        .git(&[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "-q",
            &url,
            "libs/lib",
        ])
        .unwrap();
    parent.commit_all("add lib").unwrap();
    (parent, lib, pinned)
}

#[test]
fn repo_without_submodules_lists_nothing() {
    let repo = TestRepo::new().unwrap();
    repo.commit_all("empty").unwrap();
    let cli = GitCli::default();
    assert!(
        cli.submodules(&RepoPath::new(repo.path()))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn lists_in_sync_then_out_of_sync_submodule() {
    let (parent, _lib, pinned) = parent_with_submodule();
    let cli = GitCli::default();
    let repo = RepoPath::new(parent.path());
    let subs = cli.submodules(&repo).unwrap();
    assert_eq!(subs.len(), 1);
    let s = &subs[0];
    assert_eq!(s.name, "libs/lib");
    assert!(s.initialized);
    assert_eq!(s.pinned, Some(pinned));
    assert_eq!(s.checked_out, Some(pinned));
    assert!(!s.is_out_of_sync());

    // A new commit inside the submodule: checked out ≠ pinned.
    let sub_dir = parent.path().join("libs/lib");
    archipel_fixtures::configure(&sub_dir).unwrap();
    archipel_fixtures::write_file(&sub_dir, "lib.txt", "v2\n").unwrap();
    archipel_fixtures::git(&sub_dir, &["commit", "-q", "-am", "lib v2"]).unwrap();
    let subs = cli.submodules(&repo).unwrap();
    assert!(subs[0].is_out_of_sync());
    assert_eq!(subs[0].pinned, Some(pinned));
    assert_ne!(subs[0].checked_out, Some(pinned));
}

#[test]
fn update_initializes_a_deinitialized_submodule() {
    let (parent, _lib, pinned) = parent_with_submodule();
    // Deinit keeps the module repository in `.git/modules`: no clone needed.
    parent
        .git(&["submodule", "deinit", "-q", "-f", "libs/lib"])
        .unwrap();
    let cli = GitCli::default();
    let repo = RepoPath::new(parent.path());
    let subs = cli.submodules(&repo).unwrap();
    assert!(!subs[0].initialized);
    assert_eq!(subs[0].pinned, Some(pinned));
    assert_eq!(subs[0].checked_out, None);

    let request = SubmoduleUpdate {
        paths: vec!["libs/lib".into()],
        init: true,
        recursive: true,
    };
    cli.update_submodules(&repo, &request, &NoProgress, &CancelToken::new())
        .unwrap();
    let subs = cli.submodules(&repo).unwrap();
    assert!(subs[0].initialized);
    assert_eq!(subs[0].checked_out, Some(pinned));
}

#[test]
fn discard_resets_a_moved_submodule_and_restore_brings_it_back() {
    let (parent, _lib, pinned) = parent_with_submodule();
    let sub_dir = parent.path().join("libs/lib");
    archipel_fixtures::configure(&sub_dir).unwrap();
    archipel_fixtures::write_file(&sub_dir, "lib.txt", "v2\n").unwrap();
    archipel_fixtures::git(&sub_dir, &["commit", "-q", "-am", "lib v2"]).unwrap();
    let cli = GitCli::default();
    let repo = RepoPath::new(parent.path());
    let moved = cli.submodules(&repo).unwrap()[0].checked_out;
    assert_ne!(moved, Some(pinned));

    let paths = vec!["libs/lib".to_owned()];
    let snapshot = cli.snapshot_paths(&repo, &paths).unwrap().unwrap();
    cli.discard(&repo, &paths).unwrap();
    assert_eq!(cli.submodules(&repo).unwrap()[0].checked_out, Some(pinned));
    assert!(!cli.summary(&repo).unwrap().changes.is_dirty());

    cli.restore_paths(&repo, snapshot).unwrap();
    assert_eq!(cli.submodules(&repo).unwrap()[0].checked_out, moved);
}

#[test]
fn discard_drops_changes_inside_a_submodule() {
    let (parent, _lib, pinned) = parent_with_submodule();
    let sub_dir = parent.path().join("libs/lib");
    archipel_fixtures::write_file(&sub_dir, "lib.txt", "edited\n").unwrap();
    archipel_fixtures::write_file(&sub_dir, "new.txt", "new\n").unwrap();
    let cli = GitCli::default();
    let repo = RepoPath::new(parent.path());
    assert!(cli.summary(&repo).unwrap().changes.is_dirty());

    cli.discard(&repo, &["libs/lib".to_owned()]).unwrap();
    assert!(!cli.summary(&repo).unwrap().changes.is_dirty());
    assert_eq!(cli.submodules(&repo).unwrap()[0].checked_out, Some(pinned));
    assert!(!sub_dir.join("new.txt").exists());
}
