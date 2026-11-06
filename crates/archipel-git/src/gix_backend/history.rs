//! History walking with `gix`.

use std::ops::ControlFlow;

use archipel_model::history::{CommitMeta, TopologyNode, WalkOrder};
use archipel_model::ports::HistoryReader;
use archipel_model::{CancelToken, GitError, Oid, RepoPath, Timestamp};
use gix::bstr::ByteSlice;
use gix::traverse::commit::topo;

use super::{GixReader, backend, to_gix, to_oid};

/// Tips of the graph: every ref except the stash, plus `HEAD`.
fn tips(repo: &gix::Repository) -> Result<Vec<gix::ObjectId>, GitError> {
    let mut tips = Vec::new();
    if let Ok(head) = repo.head_id() {
        tips.push(head.detach());
    }
    for r in repo.references().map_err(backend)?.all().map_err(backend)? {
        let mut r = r.map_err(backend)?;
        if r.name().as_bstr() == "refs/stash" {
            continue;
        }
        let Ok(id) = r.peel_to_id() else { continue };
        // Only commits start a walk (tags may point to trees or blobs).
        if id
            .object()
            .is_ok_and(|o| o.kind == gix::object::Kind::Commit)
        {
            tips.push(id.detach());
        }
    }
    tips.sort_unstable();
    tips.dedup();
    Ok(tips)
}

impl HistoryReader for GixReader {
    fn walk_topology(
        &self,
        path: &RepoPath,
        order: WalkOrder,
        sink: &mut dyn FnMut(TopologyNode) -> ControlFlow<()>,
        cancel: &CancelToken,
    ) -> Result<(), GitError> {
        let repo = self.open(path)?;
        let tips = tips(&repo)?;
        if tips.is_empty() {
            return Ok(());
        }
        let sorting = match order {
            WalkOrder::Topological => topo::Sorting::TopoOrder,
            WalkOrder::Date => topo::Sorting::DateOrder,
        };
        let walk = topo::Builder::from_iters(&repo.objects, tips, None::<Vec<gix::ObjectId>>)
            .sorting(sorting)
            .with_commit_graph(repo.commit_graph_if_enabled().ok().flatten())
            .build()
            .map_err(backend)?;
        for (i, info) in walk.enumerate() {
            if i % 1024 == 0 {
                cancel.check()?;
            }
            let info = info.map_err(backend)?;
            let node = TopologyNode {
                oid: to_oid(info.id)?,
                parents: info
                    .parent_ids
                    .iter()
                    .map(to_oid)
                    .collect::<Result<_, _>>()?,
            };
            if sink(node).is_break() {
                break;
            }
        }
        Ok(())
    }

    fn commit_meta(&self, path: &RepoPath, oids: &[Oid]) -> Result<Vec<CommitMeta>, GitError> {
        let repo = self.open(path)?;
        oids.iter()
            .map(|oid| {
                let commit = repo.find_commit(to_gix(*oid)?).map_err(backend)?;
                let decoded = commit.decode().map_err(backend)?;
                let author = decoded.author().map_err(backend)?;
                let committer = decoded.committer().map_err(backend)?;
                Ok(CommitMeta {
                    oid: *oid,
                    summary: decoded.message_summary().to_str_lossy().into_owned(),
                    author_name: author.name.to_str_lossy().into_owned(),
                    author_email: author.email.to_str_lossy().into_owned(),
                    author_time: Timestamp(author.time().map_err(backend)?.seconds),
                    commit_time: Timestamp(committer.time().map_err(backend)?.seconds),
                })
            })
            .collect()
    }

    fn commit_message(&self, path: &RepoPath, commit: Oid) -> Result<String, GitError> {
        let repo = self.open(path)?;
        let commit = repo.find_commit(to_gix(commit)?).map_err(backend)?;
        let decoded = commit.decode().map_err(backend)?;
        Ok(decoded.message.to_str_lossy().trim_end().to_owned())
    }

    fn is_ancestor(
        &self,
        path: &RepoPath,
        ancestor: Oid,
        descendant: Oid,
    ) -> Result<bool, GitError> {
        if ancestor == descendant {
            return Ok(true);
        }
        let repo = self.open(path)?;
        let ancestor = to_gix(ancestor)?;
        // "No merge base" is reported as an error: unrelated histories.
        let base = repo.merge_base(ancestor, to_gix(descendant)?);
        Ok(matches!(base, Ok(Some(b)) if b.detach() == ancestor))
    }
}
