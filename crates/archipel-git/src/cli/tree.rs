//! The files of a commit or of the working copy (the repo view's file
//! browser): `git ls-tree`, `git ls-files` and `git cat-file`.

use std::collections::{BTreeMap, HashSet};

use archipel_model::ports::TreeReader;
use archipel_model::tree::{EntryKind, TreeEntry};
use archipel_model::{GitError, Oid, RepoPath};

use super::GitCli;
use super::runner::{Cmd, CmdKind};

impl TreeReader for GitCli {
    fn list_files(&self, repo: &RepoPath, commit: Option<Oid>) -> Result<Vec<TreeEntry>, GitError> {
        let run = |args: &[&str]| {
            self.runner
                .run(Cmd::new(repo.as_path(), CmdKind::Read, args))
                .map(|out| out.stdout)
        };
        let mut files: BTreeMap<String, EntryKind> = BTreeMap::new();
        if let Some(c) = commit {
            let hex = c.to_hex();
            let listed = run(&["ls-tree", "-r", "-z", "--full-tree", &hex])?;
            files.extend(parse_ls_tree(&listed));
        } else {
            let deleted: HashSet<String> =
                records(&run(&["ls-files", "-z", "--deleted"])?).collect();
            let tracked = run(&["ls-files", "-z", "-s"])?;
            files.extend(parse_ls_files_stage(&tracked).filter(|(p, _)| !deleted.contains(p)));
            let untracked = run(&["ls-files", "-z", "--others", "--exclude-standard"])?;
            files.extend(records(&untracked).map(|p| (p, EntryKind::File)));
        }
        Ok(files
            .into_iter()
            .map(|(path, kind)| TreeEntry { path, kind })
            .collect())
    }

    fn read_blob(&self, repo: &RepoPath, commit: Oid, path: &str) -> Result<Vec<u8>, GitError> {
        let spec = format!("{}:{path}", commit.to_hex());
        Ok(self
            .runner
            .run(Cmd::new(
                repo.as_path(),
                CmdKind::Read,
                &["cat-file", "blob", &spec],
            ))?
            .stdout)
    }
}

/// NUL-separated records of `bytes`.
fn records(bytes: &[u8]) -> impl Iterator<Item = String> + '_ {
    bytes
        .split(|&b| b == 0)
        .filter(|r| !r.is_empty())
        .map(|r| String::from_utf8_lossy(r).into_owned())
}

/// `git ls-tree -r -z`: `<mode> <type> <oid>\t<path>`.
fn parse_ls_tree(bytes: &[u8]) -> impl Iterator<Item = (String, EntryKind)> + '_ {
    records(bytes).filter_map(|r| {
        let (meta, path) = r.split_once('\t')?;
        let mode = meta.split(' ').next()?;
        Some((path.to_owned(), EntryKind::from_mode(mode)))
    })
}

/// `git ls-files -s -z`: `<mode> <oid> <stage>\t<path>`; a conflicted
/// path shows once per stage.
fn parse_ls_files_stage(bytes: &[u8]) -> impl Iterator<Item = (String, EntryKind)> + '_ {
    // Same layout up to the tab: the mode comes first.
    parse_ls_tree(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tree_and_index_listings() {
        let tree: &[u8] = &[
            &b"100644 blob aaaa	src/a.rs"[..],
            b"100755 blob bbbb	run.sh",
            b"160000 commit cccc	vendor/lib",
            b"120000 blob dddd	link",
        ]
        .join(&0u8);
        let parsed: Vec<_> = parse_ls_tree(tree).collect();
        assert_eq!(
            parsed,
            [
                ("src/a.rs".to_owned(), EntryKind::File),
                ("run.sh".to_owned(), EntryKind::Executable),
                ("vendor/lib".to_owned(), EntryKind::Submodule),
                ("link".to_owned(), EntryKind::Symlink),
            ]
        );
        let index: &[u8] = &[
            &b"100644 aaaa 1	c.txt"[..],
            b"100644 bbbb 2	c.txt",
            b"100644 cccc 0	name with	tab",
        ]
        .join(&0u8);
        let parsed: Vec<_> = parse_ls_files_stage(index).map(|(p, _)| p).collect();
        assert_eq!(parsed, ["c.txt", "c.txt", "name with\ttab"]);
    }
}
