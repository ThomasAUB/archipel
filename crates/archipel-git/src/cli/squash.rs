//! Squashing consecutive commits of the current branch into one.
//!
//! Done with plumbing only (`commit-tree`, `update-ref`): the squashed
//! commit takes the tree of the newest commit of the range, and the commits
//! after it are recreated on top with their own trees. Every tree is kept,
//! so nothing can conflict, and neither the index nor the worktree is
//! touched: a dirty working copy is fine.

use archipel_model::history::SquashPreview;
use archipel_model::ports::RefReader;
use archipel_model::{GitError, Oid, RepoPath};

use super::GitCli;
use super::runner::{Cmd, CmdKind};
use crate::gitdir::GitDirs;

/// A commit of the first-parent history read for a squash.
#[derive(Debug)]
struct Link {
    oid: Oid,
    parents: Vec<Oid>,
}

/// What a commit is recreated with.
struct Recorded {
    tree: String,
    author_name: String,
    author_email: String,
    author_date: String,
    message: String,
}

/// The commits a squash rewrites.
struct SquashPlan {
    head: Oid,
    /// The squashed commits, oldest first.
    squashed: Vec<Link>,
    /// The commits after the range up to `HEAD`, oldest first.
    after: Vec<Link>,
}

fn refuse(why: impl Into<String>) -> GitError {
    GitError::InvalidRequest(why.into())
}

impl GitCli {
    /// Reads and checks the commits from `oldest` to `newest`: consecutive
    /// commits of the first-parent history of `HEAD`, no merge among them.
    fn squash_plan(
        &self,
        repo: &RepoPath,
        oldest: Oid,
        newest: Oid,
    ) -> Result<SquashPlan, GitError> {
        if GitDirs::locate(repo.as_path())?.in_progress().is_some() {
            return Err(refuse("finish or abort the operation in progress first"));
        }
        let head = self
            .resolve(repo, "HEAD")?
            .ok_or_else(|| refuse("HEAD has no commit"))?;
        if oldest == newest {
            return Err(refuse("pick at least two commits to squash"));
        }
        let mut cmd = Cmd::new(
            repo.as_path(),
            CmdKind::Read,
            &["rev-list", "--first-parent", "--parents"],
        )
        .arg(head.to_hex());
        // Stop at the commit before `oldest`: what is left is the
        // first-parent chain from `HEAD` down to `oldest`, when `oldest`
        // is on it.
        let parents_of_oldest = self.parents(repo, oldest)?;
        // (A root commit has none: the walk goes down to it.)
        if !parents_of_oldest.is_empty() {
            cmd = cmd
                .arg("--not")
                .args(parents_of_oldest.iter().map(Oid::to_hex));
        }
        let text = self.runner.run(cmd)?.stdout_text();
        let mut chain = Vec::new();
        for line in text.lines().filter(|l| !l.is_empty()) {
            let mut ids = line.split(' ').map(|h| {
                Oid::from_hex(h).map_err(|_| GitError::Parse {
                    what: "git rev-list",
                    detail: line.to_owned(),
                })
            });
            let oid = ids.next().transpose()?.ok_or_else(|| GitError::Parse {
                what: "git rev-list",
                detail: line.to_owned(),
            })?;
            chain.push(Link {
                oid,
                parents: ids.collect::<Result<_, _>>()?,
            });
        }
        // Newest first: `HEAD` … `newest` … `oldest`.
        if chain.last().map(|l| l.oid) != Some(oldest) {
            return Err(refuse(format!(
                "{} is not in the history of the current branch",
                oldest.short(8)
            )));
        }
        let at = chain.iter().position(|l| l.oid == newest).ok_or_else(|| {
            refuse(format!(
                "{} is not between {} and the tip of the current branch",
                newest.short(8),
                oldest.short(8)
            ))
        })?;
        let mut squashed = chain.split_off(at);
        squashed.reverse();
        let mut after = chain;
        after.reverse();
        if let Some(merge) = squashed.iter().find(|l| l.parents.len() > 1) {
            return Err(refuse(format!(
                "{} is a merge: merges cannot be squashed",
                merge.oid.short(8)
            )));
        }
        Ok(SquashPlan {
            head,
            squashed,
            after,
        })
    }

    fn parents(&self, repo: &RepoPath, commit: Oid) -> Result<Vec<Oid>, GitError> {
        let text = self
            .runner
            .run(
                Cmd::new(
                    repo.as_path(),
                    CmdKind::Read,
                    &["rev-list", "--parents", "-n", "1"],
                )
                .arg(commit.to_hex()),
            )?
            .stdout_text();
        text.split_whitespace()
            .skip(1)
            .map(|h| {
                Oid::from_hex(h).map_err(|_| GitError::Parse {
                    what: "git rev-list",
                    detail: text.clone(),
                })
            })
            .collect()
    }

    /// Tree, author and message of `commits`, in order.
    fn recorded(&self, repo: &RepoPath, commits: &[Oid]) -> Result<Vec<Recorded>, GitError> {
        if commits.is_empty() {
            return Ok(Vec::new());
        }
        // Fields end with NUL, which a commit message cannot hold; git
        // adds a newline after each commit.
        let out = self.runner.run(
            Cmd::new(
                repo.as_path(),
                CmdKind::Read,
                &[
                    "log",
                    "--no-walk=unsorted",
                    "--date=raw",
                    "--format=%T%x00%an%x00%ae%x00%ad%x00%B%x00",
                ],
            )
            .args(commits.iter().map(Oid::to_hex)),
        )?;
        let text = String::from_utf8_lossy(&out.stdout);
        let fields: Vec<&str> = text.split('\0').collect();
        let parsed: Vec<Recorded> = fields
            .as_chunks::<5>()
            .0
            .iter()
            .map(|[tree, name, email, date, message]| Recorded {
                tree: tree.trim_start_matches('\n').to_owned(),
                author_name: (*name).to_owned(),
                author_email: (*email).to_owned(),
                author_date: (*date).to_owned(),
                message: (*message).to_owned(),
            })
            .collect();
        if parsed.len() != commits.len() {
            return Err(GitError::Parse {
                what: "git log",
                detail: format!("{} commits read, {} expected", parsed.len(), commits.len()),
            });
        }
        Ok(parsed)
    }

    /// Writes a commit object; returns its id.
    fn commit_tree(
        &self,
        repo: &RepoPath,
        tree: &str,
        parents: &[Oid],
        author: &Recorded,
        message: &str,
    ) -> Result<Oid, GitError> {
        let mut cmd = Cmd::new(repo.as_path(), CmdKind::Write, &["commit-tree", tree])
            .env("GIT_AUTHOR_NAME", &author.author_name)
            .env("GIT_AUTHOR_EMAIL", &author.author_email)
            .env("GIT_AUTHOR_DATE", &author.author_date)
            .arg("-F")
            .arg("-")
            .stdin(message.as_bytes().to_vec());
        for p in parents {
            cmd = cmd.arg("-p").arg(p.to_hex());
        }
        let text = self.runner.run(cmd)?.stdout_text();
        Oid::from_hex(&text).map_err(|_| GitError::Parse {
            what: "git commit-tree",
            detail: text,
        })
    }

    pub(super) fn squash_preview_impl(
        &self,
        repo: &RepoPath,
        oldest: Oid,
        newest: Oid,
    ) -> Result<SquashPreview, GitError> {
        let plan = self.squash_plan(repo, oldest, newest)?;
        let commits: Vec<Oid> = plan.squashed.iter().map(|l| l.oid).collect();
        let message = combined_message(
            self.recorded(repo, &commits)?
                .iter()
                .map(|r| r.message.as_str()),
        );
        Ok(SquashPreview { commits, message })
    }

    pub(super) fn squash_impl(
        &self,
        repo: &RepoPath,
        oldest: Oid,
        newest: Oid,
        message: &str,
    ) -> Result<Oid, GitError> {
        if message.trim().is_empty() {
            return Err(refuse("write a message for the squashed commit"));
        }
        let plan = self.squash_plan(repo, oldest, newest)?;
        let first = &plan.squashed[0];
        let mut wanted: Vec<Oid> = vec![first.oid, newest];
        wanted.extend(plan.after.iter().map(|l| l.oid));
        let recorded = self.recorded(repo, &wanted)?;
        // The squashed commit: the first commit's author and parent, the
        // last commit's tree.
        let author = &recorded[0];
        let squashed = Recorded {
            tree: recorded[1].tree.clone(),
            author_name: author.author_name.clone(),
            author_email: author.author_email.clone(),
            author_date: author.author_date.clone(),
            message: String::new(),
        };
        let mut tip = self.commit_tree(
            repo,
            &squashed.tree,
            &first.parents,
            &squashed,
            &normalized(message),
        )?;
        // The commits after the range, on top: same trees, first parent
        // replaced, other parents (of merges) kept.
        for (link, rec) in plan.after.iter().zip(&recorded[2..]) {
            let mut parents = vec![tip];
            parents.extend(link.parents.iter().skip(1).copied());
            tip = self.commit_tree(repo, &rec.tree, &parents, rec, &rec.message)?;
        }
        // Follows `HEAD` to the branch it is on; fails if `HEAD` moved.
        self.runner.run(Cmd::new(
            repo.as_path(),
            CmdKind::Write,
            &[
                "update-ref",
                "-m",
                "archipel: squash",
                "HEAD",
                &tip.to_hex(),
                &plan.head.to_hex(),
            ],
        ))?;
        Ok(tip)
    }
}

/// The messages of the squashed commits, oldest first, separated by a
/// blank line.
fn combined_message<'a>(messages: impl Iterator<Item = &'a str>) -> String {
    let parts: Vec<&str> = messages.map(str::trim).filter(|m| !m.is_empty()).collect();
    parts.join("\n\n")
}

/// `message` with trailing whitespace trimmed and one final newline, as
/// `git commit` stores it.
fn normalized(message: &str) -> String {
    let lines: Vec<&str> = message.trim().lines().map(str::trim_end).collect();
    format!("{}\n", lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_are_joined_oldest_first() {
        let joined = combined_message(["add a\n", "  \n", "fix a\n\nbody\n"].into_iter());
        assert_eq!(joined, "add a\n\nfix a\n\nbody");
    }

    #[test]
    fn message_is_normalized() {
        assert_eq!(normalized("  title  \n\nbody  \n\n"), "title\n\nbody\n");
    }
}
