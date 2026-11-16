//! Parser of `git worktree list --porcelain -z`.
//!
//! Each worktree is a record of NUL-terminated attributes (`worktree <path>`,
//! `HEAD <oid>`, `branch <ref>`, `detached`, `bare`, `locked [<reason>]`,
//! `prunable [<reason>]`), records separated by an empty attribute.

use std::path::PathBuf;

use archipel_model::journal::HeadRef;
use archipel_model::worktree::WorktreeInfo;
use archipel_model::{GitError, Oid, RepoPath};

/// Parses the output of `git worktree list --porcelain -z`. The first
/// record is the main worktree.
pub fn parse_worktree_list(out: &[u8]) -> Result<Vec<WorktreeInfo>, GitError> {
    let text = String::from_utf8_lossy(out);
    let mut list = Vec::new();
    let mut current: Option<WorktreeInfo> = None;
    let mut detached = false;
    let finish = |w: Option<WorktreeInfo>, detached: bool, list: &mut Vec<WorktreeInfo>| {
        if let Some(mut w) = w {
            if detached {
                w.head = w.head_oid.map(HeadRef::Detached);
            }
            w.is_main = list.is_empty();
            list.push(w);
        }
    };
    for attr in text.split('\0') {
        if attr.is_empty() {
            finish(current.take(), detached, &mut list);
            detached = false;
            continue;
        }
        let (key, value) = attr.split_once(' ').unwrap_or((attr, ""));
        if key == "worktree" {
            finish(current.take(), detached, &mut list);
            detached = false;
            current = Some(WorktreeInfo {
                path: RepoPath::new(native_path(value)),
                head: None,
                head_oid: None,
                is_main: false,
                is_bare: false,
                locked: None,
                prunable: None,
            });
            continue;
        }
        let Some(w) = current.as_mut() else {
            return Err(GitError::Parse {
                what: "git worktree list",
                detail: format!("attribute before any worktree: {attr}"),
            });
        };
        match key {
            "HEAD" => {
                let oid = Oid::from_hex(value).map_err(|_| GitError::Parse {
                    what: "git worktree list",
                    detail: attr.to_owned(),
                })?;
                w.head_oid = (!oid.is_null()).then_some(oid);
            }
            "branch" => w.head = Some(HeadRef::Symbolic(value.to_owned())),
            "detached" => detached = true,
            "bare" => w.is_bare = true,
            "locked" => w.locked = Some(value.to_owned()),
            "prunable" => w.prunable = Some(value.to_owned()),
            // Attributes added by later Git versions.
            _ => {}
        }
    }
    finish(current, detached, &mut list);
    Ok(list)
}

/// Git prints `/`-separated paths, also on Windows.
fn native_path(text: &str) -> PathBuf {
    if cfg!(windows) {
        PathBuf::from(text.replace('/', "\\"))
    } else {
        PathBuf::from(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "1111111111111111111111111111111111111111";
    const B: &str = "2222222222222222222222222222222222222222";

    fn record(attrs: &[&str]) -> String {
        let mut s = String::new();
        for a in attrs {
            s.push_str(a);
            s.push('\0');
        }
        s.push('\0');
        s
    }

    #[test]
    fn main_linked_detached_locked_prunable() {
        let out = [
            record(&[
                "worktree /w/app",
                &format!("HEAD {A}"),
                "branch refs/heads/main",
            ]),
            record(&[
                "worktree /w/app.worktrees/feat",
                &format!("HEAD {B}"),
                "branch refs/heads/feat/parser",
                "locked on usb drive",
            ]),
            record(&["worktree /w/det", &format!("HEAD {A}"), "detached"]),
            record(&[
                "worktree /w/gone",
                &format!("HEAD {B}"),
                "branch refs/heads/old",
                "prunable gitdir file points to non-existent location",
            ]),
            record(&[
                "worktree /w/plain-lock",
                &format!("HEAD {B}"),
                "detached",
                "locked",
            ]),
        ]
        .concat();
        let list = parse_worktree_list(out.as_bytes()).unwrap();
        assert_eq!(list.len(), 5);
        assert!(list[0].is_main);
        assert_eq!(list[0].branch(), Some("main"));
        assert!(!list[1].is_main);
        assert_eq!(list[1].branch(), Some("feat/parser"));
        assert_eq!(list[1].locked.as_deref(), Some("on usb drive"));
        assert_eq!(
            list[2].head,
            Some(HeadRef::Detached(Oid::from_hex(A).unwrap()))
        );
        assert!(list[3].is_missing());
        assert_eq!(list[4].locked.as_deref(), Some(""));
        assert_eq!(list[1].path.as_path(), native_path("/w/app.worktrees/feat"));
    }

    #[test]
    fn bare_main_and_unborn_branch() {
        let zero = "0".repeat(40);
        let out = [
            record(&["worktree /w/repo.git", "bare"]),
            record(&[
                "worktree /w/new",
                &format!("HEAD {zero}"),
                "branch refs/heads/fresh",
            ]),
        ]
        .concat();
        let list = parse_worktree_list(out.as_bytes()).unwrap();
        assert!(list[0].is_main && list[0].is_bare);
        assert_eq!(list[0].head, None);
        assert_eq!(list[1].head_oid, None);
        assert_eq!(list[1].branch(), Some("fresh"));
    }

    #[test]
    fn garbage_is_an_error() {
        assert!(parse_worktree_list(b"HEAD 12\0\0").is_err());
        assert!(parse_worktree_list(b"").unwrap().is_empty());
    }
}
