#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Folder scan, repository presence and submodule detection on real folders.

use std::fs;
use std::path::{Path, PathBuf};

use archipel_fixtures::{TestRepo, git, write_file};
use archipel_model::cancel::CancelToken;
use archipel_model::error::ProjectError;
use archipel_model::ids::RepoPath;
use archipel_model::ports::project::RepoDiscovery;
use archipel_model::project::{DiscoveredKind, DiscoveredRepo, ProjectDef, ScanOptions};
use archipel_project::{
    FsRepoDiscovery, RepoPresence, ScanStrategy, classify_repos, outermost_repo_root, repo_presence,
};

const STRATEGIES: [ScanStrategy; 2] = [ScanStrategy::Sequential, ScanStrategy::Parallel];

fn init(dir: &Path) {
    fs::create_dir_all(dir).unwrap();
    git(dir, &["init", "-q"]).unwrap();
}

fn fake_git_dir(dir: &Path) {
    fs::create_dir_all(dir.join(".git")).unwrap();
}

fn fake_git_file(dir: &Path) {
    fs::create_dir_all(dir).unwrap();
    fs::write(dir.join(".git"), "gitdir: ../elsewhere/.git/modules/x\n").unwrap();
}

fn scan_with(strategy: ScanStrategy, root: &Path, options: &ScanOptions) -> Vec<DiscoveredRepo> {
    FsRepoDiscovery::with_strategy(strategy)
        .scan(root, options, &CancelToken::new())
        .unwrap()
}

/// Scans with both strategies, checks they agree and returns relative paths.
fn scan(root: &Path, options: &ScanOptions) -> Vec<(String, DiscoveredKind)> {
    let [seq, par] = STRATEGIES.map(|s| scan_with(s, root, options));
    assert_eq!(seq, par, "strategies disagree");
    seq.into_iter()
        .map(|r| {
            let rel = r.path.as_path().strip_prefix(root).unwrap();
            (rel.to_string_lossy().replace('\\', "/"), r.kind)
        })
        .collect()
}

fn rel(path: &str, kind: DiscoveredKind) -> (String, DiscoveredKind) {
    (path.to_owned(), kind)
}

#[test]
fn finds_git_dirs_and_git_files_sorted() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    init(&root.join("b-repo"));
    init(&root.join("group").join("a-repo"));
    fake_git_file(&root.join("group").join("sub"));
    fs::create_dir_all(root.join("plain").join("folder")).unwrap();
    write_file(root, "plain/file.txt", "x").unwrap();

    assert_eq!(
        scan(root, &ScanOptions::default()),
        [
            rel("b-repo", DiscoveredKind::GitDir),
            rel("group/a-repo", DiscoveredKind::GitDir),
            rel("group/sub", DiscoveredKind::GitFile),
        ]
    );
}

#[test]
fn detects_real_linked_worktree() {
    let repo = TestRepo::new().unwrap();
    repo.commit_all("init").unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let wt = tmp.path().join("wt");
    repo.git(&[
        "worktree",
        "add",
        "-q",
        wt.to_str().unwrap(),
        "-b",
        "feature",
    ])
    .unwrap();
    assert_eq!(
        scan(tmp.path(), &ScanOptions::default()),
        [rel("wt", DiscoveredKind::GitFile)]
    );
}

#[test]
fn root_itself_can_be_a_repository() {
    let tmp = tempfile::tempdir().unwrap();
    init(tmp.path());
    init(&tmp.path().join("nested"));
    assert_eq!(
        scan(tmp.path(), &ScanOptions::default()),
        [rel("", DiscoveredKind::GitDir)]
    );
}

#[test]
fn descend_into_repos_finds_nested_repositories() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    init(&root.join("outer"));
    init(&root.join("outer").join("libs").join("inner"));
    fake_git_file(&root.join("outer").join("sub"));

    let default = ScanOptions::default();
    assert_eq!(scan(root, &default), [rel("outer", DiscoveredKind::GitDir)]);

    let descend = ScanOptions {
        descend_into_repos: true,
        ..ScanOptions::default()
    };
    assert_eq!(
        scan(root, &descend),
        [
            rel("outer", DiscoveredKind::GitDir),
            rel("outer/libs/inner", DiscoveredKind::GitDir),
            rel("outer/sub", DiscoveredKind::GitFile),
        ]
    );
}

#[test]
fn skip_dirs_and_hidden_dirs_are_not_entered() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    fake_git_dir(&root.join("node_modules").join("pkg"));
    fake_git_dir(&root.join("app").join("target").join("x"));
    fake_git_dir(&root.join(".hidden").join("repo"));
    fake_git_dir(&root.join("custom").join("repo"));
    fake_git_dir(&root.join("kept"));

    assert_eq!(
        scan(root, &ScanOptions::default()),
        [
            rel("custom/repo", DiscoveredKind::GitDir),
            rel("kept", DiscoveredKind::GitDir)
        ]
    );

    let options = ScanOptions {
        skip_dirs: vec!["custom".to_owned()],
        skip_hidden: false,
        ..ScanOptions::default()
    };
    assert_eq!(
        scan(root, &options),
        [
            rel(".hidden/repo", DiscoveredKind::GitDir),
            rel("app/target/x", DiscoveredKind::GitDir),
            rel("kept", DiscoveredKind::GitDir),
            rel("node_modules/pkg", DiscoveredKind::GitDir),
        ]
    );
}

#[test]
fn max_depth_limits_the_walk() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    fake_git_dir(&root.join("a"));
    fake_git_dir(&root.join("x").join("y").join("deep"));

    let depth = |max_depth| ScanOptions {
        max_depth,
        ..ScanOptions::default()
    };
    assert_eq!(scan(root, &depth(0)), []);
    assert_eq!(scan(root, &depth(1)), [rel("a", DiscoveredKind::GitDir)]);
    assert_eq!(scan(root, &depth(2)), [rel("a", DiscoveredKind::GitDir)]);
    assert_eq!(
        scan(root, &depth(3)),
        [
            rel("a", DiscoveredKind::GitDir),
            rel("x/y/deep", DiscoveredKind::GitDir)
        ]
    );
}

#[test]
fn cancelled_scan_stops_early() {
    let tmp = tempfile::tempdir().unwrap();
    fake_git_dir(&tmp.path().join("a"));
    let cancel = CancelToken::new();
    cancel.cancel();
    for strategy in STRATEGIES {
        let found = FsRepoDiscovery::with_strategy(strategy)
            .scan(tmp.path(), &ScanOptions::default(), &cancel)
            .unwrap();
        assert!(found.is_empty());
    }
}

#[test]
fn streaming_scan_reports_the_same_repositories() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    for name in ["a", "b/c", "d/e/f"] {
        fake_git_dir(&root.join(name));
    }
    fake_git_file(&root.join("g"));
    for strategy in STRATEGIES {
        let discovery = FsRepoDiscovery::with_strategy(strategy);
        let expected = discovery
            .scan(root, &ScanOptions::default(), &CancelToken::new())
            .unwrap();
        let found = std::sync::Mutex::new(Vec::new());
        discovery
            .scan_each(root, &ScanOptions::default(), &CancelToken::new(), &|r| {
                found.lock().unwrap().push(r);
            })
            .unwrap();
        let mut found = found.into_inner().unwrap();
        found.sort_by(|a, b| a.path.as_path().cmp(b.path.as_path()));
        assert_eq!(found, expected);
    }
}

#[test]
fn outermost_repo_root_climbs_out_of_nested_repositories() {
    let tmp = tempfile::tempdir().unwrap();
    let outer = tmp.path().join("outer");
    fake_git_dir(&outer);
    let sub = outer.join("libs").join("sub");
    fake_git_file(&sub);
    let deep = sub.join("src").join("deep");
    fs::create_dir_all(&deep).unwrap();
    assert_eq!(outermost_repo_root(&deep), Some(outer.clone()));
    assert_eq!(outermost_repo_root(&outer), Some(outer));
    let plain = tmp.path().join("plain");
    fs::create_dir_all(&plain).unwrap();
    // The temp dir itself is not inside a repository on CI machines.
    if outermost_repo_root(tmp.path()).is_none() {
        assert_eq!(outermost_repo_root(&plain), None);
    }
}

#[test]
fn missing_root_is_an_error() {
    let tmp = tempfile::tempdir().unwrap();
    let missing = tmp.path().join("missing");
    let result =
        FsRepoDiscovery::new().scan(&missing, &ScanOptions::default(), &CancelToken::new());
    assert!(matches!(result, Err(ProjectError::Io { .. })));
}

#[test]
fn presence_of_repositories() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    init(&root.join("real"));
    fake_git_file(&root.join("linked"));
    fs::create_dir_all(root.join("plain")).unwrap();
    let bare = root.join("bare.git");
    fs::create_dir_all(&bare).unwrap();
    git(&bare, &["init", "-q", "--bare"]).unwrap();

    assert_eq!(repo_presence(&root.join("real")), RepoPresence::Present);
    assert_eq!(repo_presence(&root.join("linked")), RepoPresence::Present);
    assert_eq!(repo_presence(&bare), RepoPresence::Present);
    assert_eq!(
        repo_presence(&root.join("plain")),
        RepoPresence::NotARepository
    );
    assert_eq!(
        repo_presence(&root.join("gone")),
        RepoPresence::PathNotFound
    );

    let entry = |name: &str| archipel_model::project::RepoEntry {
        path: RepoPath::new(root.join(name)),
        alias: None,
        category: None,
    };
    let project = ProjectDef {
        name: "p".to_owned(),
        repos: vec![entry("real"), entry("gone"), entry("plain")],
        settings: archipel_model::project::ProjectSettings::default(),
    };
    let presence: Vec<_> = classify_repos(&project)
        .into_iter()
        .map(|(e, p)| (e.path.as_path().to_path_buf(), p))
        .collect();
    assert_eq!(
        presence,
        [
            (root.join("real"), RepoPresence::Present),
            (root.join("gone"), RepoPresence::PathNotFound),
            (root.join("plain"), RepoPresence::NotARepository),
        ]
    );
    assert!(presence[0].1.is_present());
    assert!(!presence[1].1.is_present());
}

#[test]
fn real_submodule_is_detected_and_scanned() {
    let child = TestRepo::new().unwrap();
    child.commit_all("child").unwrap();
    let parent = TestRepo::new().unwrap();
    parent
        .git(&[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "-q",
            child.path().to_str().unwrap(),
            "deps/child",
        ])
        .unwrap();

    let sub: PathBuf = parent.path().join("deps").join("child");
    assert_eq!(repo_presence(&sub), RepoPresence::Present);

    let options = ScanOptions {
        descend_into_repos: true,
        ..ScanOptions::default()
    };
    assert_eq!(
        scan(parent.path(), &options),
        [
            rel("", DiscoveredKind::GitDir),
            rel("deps/child", DiscoveredKind::GitFile),
        ]
    );
}
