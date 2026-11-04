//! A large synthetic history with merges, generated with `git fast-import`.

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::Path;
use std::process::Stdio;

use anyhow::{Context, Result, bail};

use crate::{base_command, configure, git};

/// Shape of the generated history.
#[derive(Clone, Copy, Debug)]
pub struct HistoryShape {
    /// Total number of commits (approximately; merges included).
    pub commits: usize,
    /// A feature branch is forked every `branch_every` mainline commits.
    pub branch_every: usize,
    /// Commits per feature branch before it is merged back.
    pub branch_len: usize,
    /// Number of distinct files touched.
    pub files: usize,
    /// Number of distinct authors.
    pub authors: usize,
}

impl Default for HistoryShape {
    fn default() -> Self {
        Self {
            commits: 100_000,
            branch_every: 20,
            branch_len: 4,
            files: 200,
            authors: 12,
        }
    }
}

/// Creates a repository at `dir` containing a history of the given shape on
/// `main`, with a few feature branches left open and some tags. Like a
/// repository maintained by `git gc`, it has a commit-graph file.
pub fn generate_history(dir: &Path, shape: HistoryShape) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    git(dir, &["init", "-q", "-b", "main"])?;
    configure(dir)?;
    let stream = build_stream(shape);
    let mut child = base_command(dir)
        .args(["fast-import", "--quiet", "--done"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .context("spawning git fast-import")?;
    child
        .stdin
        .take()
        .context("fast-import stdin")?
        .write_all(stream.as_bytes())?;
    let out = child.wait_with_output()?;
    if !out.status.success() {
        bail!(
            "fast-import failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    git(dir, &["reset", "-q", "--hard", "main"])?;
    git(dir, &["commit-graph", "write", "--reachable"])?;
    Ok(())
}

struct Gen {
    out: String,
    mark: usize,
    time: i64,
    shape: HistoryShape,
}

impl Gen {
    fn commit(
        &mut self,
        branch: &str,
        from: Option<usize>,
        merge: Option<usize>,
        msg: &str,
    ) -> usize {
        self.mark += 1;
        self.time += 600;
        let mark = self.mark;
        let author = mark % self.shape.authors;
        let file = (mark * 7) % self.shape.files;
        let body = format!("line {mark}\n");
        let _ = writeln!(self.out, "commit refs/heads/{branch}\nmark :{mark}");
        let _ = writeln!(
            self.out,
            "author Dev {author} <dev{author}@example.com> {} +0000",
            self.time
        );
        let _ = writeln!(
            self.out,
            "committer Dev {author} <dev{author}@example.com> {} +0000",
            self.time
        );
        let _ = writeln!(self.out, "data {}\n{msg}", msg.len());
        if let Some(from) = from {
            let _ = writeln!(self.out, "from :{from}");
        }
        if let Some(merge) = merge {
            let _ = writeln!(self.out, "merge :{merge}");
        }
        let _ = writeln!(
            self.out,
            "M 644 inline dir{}/file{file}.txt\ndata {}\n{body}",
            file % 10,
            body.len()
        );
        mark
    }
}

fn build_stream(shape: HistoryShape) -> String {
    let mut g = Gen {
        out: String::with_capacity(shape.commits * 220),
        mark: 0,
        time: 1_500_000_000,
        shape,
    };
    let mut tip = g.commit("main", None, None, "initial commit");
    let mut made = 1;
    let mut feature = 0;
    while made < shape.commits {
        for _ in 0..shape.branch_every {
            tip = g.commit("main", Some(tip), None, &format!("main work {made}"));
            made += 1;
        }
        feature += 1;
        let branch = format!("feature/{feature}");
        let mut ftip = tip;
        for k in 0..shape.branch_len {
            ftip = g.commit(&branch, Some(ftip), None, &format!("{branch} step {k}"));
            made += 1;
        }
        // A couple of mainline commits in parallel so the graph has two lanes.
        tip = g.commit("main", Some(tip), None, &format!("main parallel {made}"));
        made += 1;
        // Leave the last 3 feature branches open, merge the others.
        if made + shape.branch_every * 3 < shape.commits {
            tip = g.commit("main", Some(tip), Some(ftip), &format!("Merge {branch}"));
            made += 1;
            let _ = writeln!(
                g.out,
                "reset refs/heads/{branch}\nfrom 0000000000000000000000000000000000000000\n"
            );
        }
        if feature % 50 == 0 {
            let _ = writeln!(g.out, "reset refs/tags/v{}.0\nfrom :{tip}\n", feature / 50);
        }
    }
    g.out.push_str("done\n");
    g.out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_requested_size() {
        let dir = tempfile::tempdir().unwrap();
        let shape = HistoryShape {
            commits: 300,
            ..HistoryShape::default()
        };
        generate_history(dir.path(), shape).unwrap();
        let count: usize = git(dir.path(), &["rev-list", "--all", "--count"])
            .unwrap()
            .parse()
            .unwrap();
        assert!((300..330).contains(&count), "{count}");
        let merges: usize = git(dir.path(), &["rev-list", "--merges", "--count", "main"])
            .unwrap()
            .parse()
            .unwrap();
        assert!(merges > 5);
    }
}
