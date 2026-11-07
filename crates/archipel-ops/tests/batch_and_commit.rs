//! Batch operations, multi-repo commit and partial staging on real repositories.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::similar_names,
    clippy::format_collect
)]

use std::sync::Arc;

use archipel_fixtures::project::{RepoState, generate_project};
use archipel_fixtures::{TestRepo, git};
use archipel_git::GitCli;
use archipel_model::RepoPath;
use archipel_model::diff::LineOrigin;
use archipel_model::ports::{DiffReader, DiffSide, JournalStore, StatusReader};
use archipel_model::requests::{FetchRequest, PatchDirection, TagRequest};
use archipel_model::settings::Settings;
use archipel_model::testing::InMemoryStore;
use archipel_ops::batch::BatchTarget;
use archipel_ops::patch::{hunk_patch, lines_patch, selection_patch};
use archipel_ops::{
    BatchOps, CommitOptions, Direction, GitPorts, Journal, MultiCommit, OpError, Outcome,
    RepoActions, Scheduler, UndoService,
};

struct Ops {
    rt: tokio::runtime::Runtime,
    batches: Arc<BatchOps>,
    commit: MultiCommit,
    undo: UndoService,
    actions: RepoActions,
    store: Arc<InMemoryStore>,
}

fn ops() -> Ops {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let cli = Arc::new(GitCli::default());
    let store = Arc::new(InMemoryStore::new());
    let ports = GitPorts::from_backend(cli.clone());
    let journal = Journal::new(store.clone(), cli.clone(), cli, 200);
    let actions = RepoActions::new(ports.clone(), journal.clone());
    let scheduler = Arc::new(Scheduler::new(rt.handle().clone(), &Settings::default()));
    let batches = Arc::new(BatchOps::new(scheduler.clone(), actions.clone()));
    Ops {
        commit: MultiCommit::new(scheduler, actions.clone(), batches.clone()),
        undo: UndoService::new(ports, journal),
        batches,
        actions,
        store,
        rt,
    }
}

fn add_failing_hook(path: &std::path::Path) {
    let hook = path.join(".git/hooks/pre-commit");
    std::fs::write(
        &hook,
        "#!/bin/sh\necho 'pre-commit: tests failed' >&2\nexit 1\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

#[test]
fn multi_repo_commit_reports_partial_failure_and_retries() {
    let o = ops();
    let repos: Vec<TestRepo> = (0..3).map(|_| TestRepo::new().unwrap()).collect();
    for (i, r) in repos.iter().enumerate() {
        r.write("a.txt", "a\n").unwrap();
        r.commit_all("init").unwrap();
        if i < 2 {
            r.write("a.txt", &format!("change {i}\n")).unwrap();
            r.git(&["add", "a.txt"]).unwrap();
        }
    }
    add_failing_hook(repos[1].path());
    let paths: Vec<RepoPath> = repos.iter().map(|r| RepoPath::new(r.path())).collect();
    let mut plan = o.rt.block_on(o.commit.prepare(&paths));
    let selected: Vec<bool> = plan.targets.iter().map(|t| t.selected).collect();
    assert_eq!(
        selected,
        [true, true, false],
        "repos with staged changes are pre-checked"
    );
    assert!(!plan.branch_mismatch());

    let report =
        o.rt.block_on(
            o.commit
                .run(&plan, "JIRA-1 shared change", &CommitOptions::default()),
        )
        .unwrap();
    assert_eq!(report.counts(), (1, 1, 0));
    match report.outcome(paths[1].id()).unwrap() {
        Outcome::Failed(OpError::Git(archipel_model::GitError::CommandFailed {
            stderr, ..
        })) => {
            assert!(stderr.contains("tests failed"));
        }
        other => panic!("{other:?}"),
    }
    // Retry only the failed repo once the hook is fixed.
    std::fs::remove_file(repos[1].path().join(".git/hooks/pre-commit")).unwrap();
    for t in &mut plan.targets {
        t.selected = report.failed().contains(&t.id);
    }
    let retry =
        o.rt.block_on(
            o.commit
                .run(&plan, "JIRA-1 shared change", &CommitOptions::default()),
        )
        .unwrap();
    assert_eq!(retry.counts(), (1, 0, 0));
    assert_eq!(
        repos[1].git(&["log", "-1", "--format=%s"]).unwrap(),
        "JIRA-1 shared change"
    );

    // Empty message and required same branch are refused up front.
    assert!(
        o.rt.block_on(o.commit.run(&plan, "  ", &CommitOptions::default()))
            .is_err()
    );
    repos[2].git(&["checkout", "-q", "-b", "other"]).unwrap();
    let mut plan = o.rt.block_on(o.commit.prepare(&paths));
    for t in &mut plan.targets {
        t.selected = true;
    }
    assert!(plan.branch_mismatch());
    let strict = CommitOptions {
        require_same_branch: true,
        ..CommitOptions::default()
    };
    assert!(o.rt.block_on(o.commit.run(&plan, "x", &strict)).is_err());
}

#[test]
fn batch_operations_on_a_project() {
    let o = ops();
    let dir = tempfile::tempdir().unwrap();
    let repos = generate_project(dir.path(), 8, 3).unwrap();
    let targets: Vec<BatchTarget> = repos
        .iter()
        .map(|r| BatchTarget::new(RepoPath::new(&r.path)))
        .collect();

    let fetch =
        o.rt.block_on(o.batches.fetch_all(&targets, FetchRequest::default()));
    assert_eq!(fetch.counts(), (8, 0, 0));

    // Pull: fast-forward only; the diverged repo fails, the dirty one may succeed.
    let pull = o.rt.block_on(o.batches.pull_all(
        &targets,
        archipel_model::requests::PullMode::FastForwardOnly,
    ));
    let diverged = repos
        .iter()
        .position(|r| r.state == RepoState::Diverged)
        .unwrap();
    assert!(matches!(pull.per_repo[diverged].1, Outcome::Failed(_)));
    let behind = repos
        .iter()
        .position(|r| r.state == RepoState::Behind)
        .unwrap();
    assert!(pull.per_repo[behind].1.is_done());
    assert_eq!(pull.failed(), vec![targets[diverged].id]);

    // Create a branch everywhere: it is checked out at once, so checking it
    // out again is skipped.
    let created =
        o.rt.block_on(o.batches.create_branch_all(&targets, "release/1.0", None));
    assert_eq!(created.counts(), (8, 0, 0));
    let checkout =
        o.rt.block_on(o.batches.checkout_all(&targets, "release/1.0"));
    assert_eq!(checkout.counts(), (0, 0, 8));
    let missing =
        o.rt.block_on(o.batches.checkout_all(&targets, "does-not-exist"));
    assert_eq!(missing.counts(), (0, 0, 8));

    // The create-and-checkout batch is undone as one unit.
    let entries = o.store.recent(targets[0].id, 1).unwrap();
    let batch = entries[0].batch.unwrap();
    let locate = |id| targets.iter().find(|t| t.id == id).map(|t| t.path.clone());
    let undone = o
        .undo
        .execute_batch(batch, Direction::Undo, &locate)
        .unwrap();
    assert!(undone.counts().0 >= 1);
    let cli = GitCli::default();
    assert_eq!(
        cli.summary(&targets[0].path).unwrap().head.branch(),
        Some("main")
    );

    let tags = o.rt.block_on(o.batches.create_tag_all(
        &targets,
        TagRequest {
            name: "v2".into(),
            target: None,
            message: None,
        },
    ));
    assert_eq!(tags.counts(), (8, 0, 0));
}

#[test]
fn line_and_hunk_staging_match_git() {
    let o = ops();
    let repo = TestRepo::new().unwrap();
    let body: String = (1..=30).map(|i| format!("line {i}\n")).collect();
    repo.write("f.txt", &body).unwrap();
    repo.commit_all("init").unwrap();
    let changed = body
        .replace("line 2\n", "LINE 2\n")
        .replace("line 3\n", "LINE 3\n")
        .replace("line 25\n", "LINE 25\n");
    repo.write("f.txt", &changed).unwrap();
    let path = RepoPath::new(repo.path());
    let cli = GitCli::default();
    let diff = &cli.worktree_diff(&path, DiffSide::Unstaged, None).unwrap()[0];
    assert_eq!(diff.hunks.len(), 2);

    // Stage only "line 2 -> LINE 2" from the first hunk.
    let hunk = &diff.hunks[0];
    let picks: Vec<usize> = hunk
        .lines
        .iter()
        .enumerate()
        .filter(|(_, l)| l.content.ends_with(" 2") || l.content.ends_with(" 2\r"))
        .map(|(i, _)| i)
        .collect();
    let patch = lines_patch(diff, 0, &picks, PatchDirection::Forward).unwrap();
    o.actions
        .apply_patch(&path, &patch, PatchDirection::Forward)
        .unwrap();
    let cached = repo.git(&["diff", "--cached", "-U0"]).unwrap();
    assert!(cached.contains("-line 2\n+LINE 2"), "{cached}");
    assert!(
        !cached.contains("LINE 3") && !cached.contains("LINE 25"),
        "{cached}"
    );

    // Stage the second hunk whole.
    let patch = hunk_patch(diff, 1).unwrap();
    o.actions
        .apply_patch(&path, &patch, PatchDirection::Forward)
        .unwrap();
    assert!(
        repo.git(&["diff", "--cached"])
            .unwrap()
            .contains("+LINE 25")
    );

    // Unstage "LINE 25" again from the staged diff (reverse).
    let staged = &cli.worktree_diff(&path, DiffSide::Staged, None).unwrap()[0];
    let last = staged.hunks.len() - 1;
    let picks: Vec<usize> = staged.hunks[last]
        .lines
        .iter()
        .enumerate()
        .filter(|(_, l)| l.content.contains("25"))
        .map(|(i, _)| i)
        .collect();
    let patch = lines_patch(staged, last, &picks, PatchDirection::Reverse).unwrap();
    o.actions
        .apply_patch(&path, &patch, PatchDirection::Reverse)
        .unwrap();
    let cached = repo.git(&["diff", "--cached"]).unwrap();
    assert!(
        cached.contains("+LINE 2") && !cached.contains("LINE 25"),
        "{cached}"
    );
}

#[test]
fn line_staging_in_crlf_file() {
    let o = ops();
    let repo = TestRepo::new().unwrap();
    repo.write("w.txt", "a\r\nb\r\nc\r\n").unwrap();
    repo.commit_all("init").unwrap();
    repo.write("w.txt", "A\r\nb\r\nC\r\n").unwrap();
    let path = RepoPath::new(repo.path());
    let diff = &GitCli::default()
        .worktree_diff(&path, DiffSide::Unstaged, None)
        .unwrap()[0];
    let picks: Vec<usize> = diff.hunks[0]
        .lines
        .iter()
        .enumerate()
        .filter(|(_, l)| l.content.starts_with('a') || l.content.starts_with('A'))
        .map(|(i, _)| i)
        .collect();
    let patch = lines_patch(diff, 0, &picks, PatchDirection::Forward).unwrap();
    o.actions
        .apply_patch(&path, &patch, PatchDirection::Forward)
        .unwrap();
    let index = git(repo.path(), &["show", ":w.txt"]).unwrap();
    // `git()` trims trailing whitespace, including the final `\r\n`.
    assert_eq!(index, "A\r\nb\r\nc");
}

#[test]
fn selection_across_hunks_and_line_discard() {
    let o = ops();
    let repo = TestRepo::new().unwrap();
    let original: String = (1..=30).map(|i| format!("line {i}\n")).collect();
    repo.write("f.txt", &original).unwrap();
    repo.commit_all("init").unwrap();
    let changed = original
        .replace("line 2\n", "LINE 2\nextra\n")
        .replace("line 25\n", "LINE 25\n");
    repo.write("f.txt", &changed).unwrap();
    let path = RepoPath::new(repo.path());
    let cli = GitCli::default();
    let diff = &cli.worktree_diff(&path, DiffSide::Unstaged, None).unwrap()[0];
    assert_eq!(diff.hunks.len(), 2);
    let pick = |h: usize, f: &dyn Fn(&str) -> bool| -> Vec<usize> {
        diff.hunks[h]
            .lines
            .iter()
            .enumerate()
            .filter(|(_, l)| l.origin != LineOrigin::Context && f(&l.content))
            .map(|(i, _)| i)
            .collect()
    };

    // Stage "extra" (first hunk) and "LINE 25" (second hunk) in one patch.
    let selection = [
        (0, pick(0, &|c| c == "extra")),
        (1, pick(1, &|c| c.ends_with("25"))),
    ];
    let patch = selection_patch(diff, &selection, PatchDirection::Forward).unwrap();
    o.actions
        .apply_patch(&path, &patch, PatchDirection::Forward)
        .unwrap();
    let cached = repo.git(&["diff", "--cached", "-U0"]).unwrap();
    assert!(
        cached.contains("+extra") && cached.contains("+LINE 25"),
        "{cached}"
    );
    assert!(!cached.contains("LINE 2\n"), "{cached}");

    // Discard "line 2 -> LINE 2" from the worktree, keep the rest.
    let diff = &cli.worktree_diff(&path, DiffSide::Unstaged, None).unwrap()[0];
    let picks: Vec<usize> = diff.hunks[0]
        .lines
        .iter()
        .enumerate()
        .filter(|(_, l)| l.origin != LineOrigin::Context && l.content.ends_with(" 2"))
        .map(|(i, _)| i)
        .collect();
    let patch = lines_patch(diff, 0, &picks, PatchDirection::Reverse).unwrap();
    o.actions.discard_patch(&path, "f.txt", &patch).unwrap();
    let worktree = std::fs::read_to_string(repo.path().join("f.txt")).unwrap();
    assert_eq!(
        worktree,
        original
            .replace("line 2\n", "line 2\nextra\n")
            .replace("line 25\n", "LINE 25\n")
    );
    // Nothing left unstaged.
    assert!(repo.git(&["diff"]).unwrap().is_empty());
}

#[test]
fn saving_an_edited_file_refuses_a_change_made_meanwhile() {
    let o = ops();
    let repo = TestRepo::new().unwrap();
    repo.write("src/e.txt", "one\r\ntwo\r\n").unwrap();
    repo.commit_all("init").unwrap();
    let path = RepoPath::new(repo.path());
    let opened = o.actions.read_file(&path, "src/e.txt").unwrap();
    assert_eq!(opened, b"one\r\ntwo\r\n");

    o.actions
        .save_file(&path, "src/e.txt", &opened, b"one\r\nTWO\r\n")
        .unwrap();
    let on_disk = std::fs::read(repo.path().join("src/e.txt")).unwrap();
    assert_eq!(on_disk, b"one\r\nTWO\r\n");
    // The edit shows as an unstaged change.
    assert!(!repo.git(&["diff", "--name-only"]).unwrap().is_empty());

    // Saved again over the old contents: changed meanwhile, refused.
    let err = o
        .actions
        .save_file(&path, "src/e.txt", &opened, b"lost\r\n")
        .unwrap_err();
    assert!(matches!(err, OpError::Refused(_)), "{err}");
    let on_disk = std::fs::read(repo.path().join("src/e.txt")).unwrap();
    assert_eq!(on_disk, b"one\r\nTWO\r\n");
}

#[test]
fn deleting_branches_skips_moved_ones_and_undoes_as_one_batch() {
    use archipel_model::ports::RefReader;
    use std::collections::HashMap;

    let o = ops();
    let repo = TestRepo::new().unwrap();
    repo.write("a.txt", "a").unwrap();
    repo.commit_all("first").unwrap();
    repo.git(&["branch", "done"]).unwrap();
    repo.git(&["branch", "moved"]).unwrap();
    repo.git(&["branch", "gone"]).unwrap();
    let path = RepoPath::new(repo.path());
    let cli = GitCli::default();
    let tip = cli.resolve(&path, "main").unwrap().unwrap();

    // Listed, then changed before the batch runs.
    repo.git(&["checkout", "-q", "moved"]).unwrap();
    repo.write("b.txt", "b").unwrap();
    repo.commit_all("more").unwrap();
    repo.git(&["checkout", "-q", "main"]).unwrap();
    repo.git(&["branch", "-D", "gone"]).unwrap();

    let target = BatchTarget::new(path.clone());
    let wanted = ["done", "moved", "gone", "main"]
        .map(|n| (n.to_owned(), tip))
        .to_vec();
    let report = o.rt.block_on(o.batches.delete_branches_all(
        std::slice::from_ref(&target),
        HashMap::from([(target.id, wanted)]),
    ));
    assert_eq!(report.outcome(target.id), Some(&Outcome::Done(1)));
    assert!(cli.resolve(&path, "refs/heads/done").unwrap().is_none());
    assert!(cli.resolve(&path, "refs/heads/moved").unwrap().is_some());
    assert!(cli.resolve(&path, "refs/heads/main").unwrap().is_some());

    // Only skips: reported as such, with the reasons.
    let again = o.rt.block_on(o.batches.delete_branches_all(
        std::slice::from_ref(&target),
        HashMap::from([(target.id, vec![("moved".to_owned(), tip)])]),
    ));
    assert!(matches!(
        again.outcome(target.id),
        Some(Outcome::Skipped(r)) if r.contains("moved since")
    ));

    let batch = o.store.recent(target.id, 1).unwrap()[0].batch.unwrap();
    let locate = |id| (id == target.id).then(|| path.clone());
    o.undo
        .execute_batch(batch, Direction::Undo, &locate)
        .unwrap();
    assert_eq!(cli.resolve(&path, "done").unwrap(), Some(tip));
}
