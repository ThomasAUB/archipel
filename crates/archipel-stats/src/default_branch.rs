//! Detection of a repository's default branch.

use archipel_model::ports::RefReader;
use archipel_model::refs::BranchInfo;
use archipel_model::{GitError, Oid, RepoPath};

use crate::config::FALLBACK_DEFAULT_BRANCHES;

/// Full name of the ref telling the remote's default branch.
const ORIGIN_HEAD: &str = "refs/remotes/origin/HEAD";
/// Prefix of the `origin` remote-tracking branches' short names.
const ORIGIN_PREFIX: &str = "origin/";

/// The branch other branches are merged into.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DefaultBranch {
    /// Short name (`main`), local when such a local branch exists.
    pub name: String,
    /// Its tip.
    pub tip: Oid,
}

/// Finds the default branch: the `configured` name when it exists, else the
/// branch `origin/HEAD` points to, else the first existing of
/// [`FALLBACK_DEFAULT_BRANCHES`]. `branches` is the output of
/// [`RefReader::branches`].
pub fn detect_default_branch(
    refs: &dyn RefReader,
    repo: &RepoPath,
    branches: &[BranchInfo],
    configured: Option<&str>,
) -> Result<Option<DefaultBranch>, GitError> {
    let local = |name: &str| {
        branches
            .iter()
            .find(|b| !b.is_remote && b.name == name)
            .map(|b| DefaultBranch {
                name: name.to_owned(),
                tip: b.target,
            })
    };

    if let Some(name) = configured {
        if let Some(found) = local(name) {
            return Ok(Some(found));
        }
        if let Some(tip) = refs.resolve(repo, name)? {
            return Ok(Some(DefaultBranch {
                name: name.to_owned(),
                tip,
            }));
        }
        tracing::debug!(name, "configured default branch not found, auto-detecting");
    }

    if let Some(origin_head) = refs.resolve(repo, ORIGIN_HEAD)? {
        // `origin/HEAD` is symbolic; the port only resolves it to a commit, so
        // pick the `origin/*` branch at that commit (preferring usual names).
        let mut candidates: Vec<&str> = branches
            .iter()
            .filter(|b| b.is_remote && b.target == origin_head)
            .filter_map(|b| b.name.strip_prefix(ORIGIN_PREFIX))
            .filter(|n| *n != "HEAD")
            .collect();
        candidates.sort_by_key(|n| {
            (
                FALLBACK_DEFAULT_BRANCHES
                    .iter()
                    .position(|f| f == n)
                    .unwrap_or(FALLBACK_DEFAULT_BRANCHES.len()),
                *n,
            )
        });
        if let Some(short) = candidates.first() {
            return Ok(Some(local(short).unwrap_or_else(|| DefaultBranch {
                name: (*short).to_owned(),
                tip: origin_head,
            })));
        }
    }

    Ok(FALLBACK_DEFAULT_BRANCHES.iter().find_map(|n| local(n)))
}
