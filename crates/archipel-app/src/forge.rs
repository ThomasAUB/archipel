//! "Open in browser" links derived from remote URLs (spec: resolved questions).
//!
//! Extension point: [`ForgeProvider`], registered in a [`ForgeRegistry`]. The
//! built-ins (GitHub, GitLab, Bitbucket, Azure DevOps) are registered exactly
//! as a fork would register its own provider (e.g. a self-hosted forge).

use std::fmt::Write as _;
use std::sync::Arc;

/// A repository on a forge, parsed from a remote URL.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteLocation {
    /// Host name (`github.com`).
    pub host: String,
    /// Path segments after the host, without `.git` (`["owner", "repo"]`).
    pub path: Vec<String>,
}

impl RemoteLocation {
    /// Parses `https://host/a/b(.git)`, `ssh://user@host[:port]/a/b(.git)` and
    /// scp-like `user@host:a/b(.git)` URLs.
    #[must_use]
    pub fn parse(url: &str) -> Option<Self> {
        let url = url.trim();
        let (host, path) = if let Some(rest) = url.split_once("://").map(|(_, r)| r) {
            let (authority, path) = rest.split_once('/')?;
            let host = authority.rsplit('@').next()?;
            let host = host.split(':').next()?;
            (host, path)
        } else {
            let (authority, path) = url.split_once(':')?;
            // A local path: `./a:b`, or a Windows drive letter like `C:/x`.
            if authority.contains('/') || authority.len() == 1 {
                return None;
            }
            (authority.rsplit('@').next()?, path)
        };
        let path: Vec<String> = path
            .trim_end_matches('/')
            .trim_end_matches(".git")
            .split('/')
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect();
        if host.is_empty() || path.len() < 2 {
            return None;
        }
        Some(Self {
            host: host.to_ascii_lowercase(),
            path,
        })
    }
}

/// Links into a forge web UI for one repository.
pub trait ForgeLinks: Send + Sync {
    /// The repository home page.
    fn repo(&self) -> String;
    /// A branch page.
    fn branch(&self, branch: &str) -> String;
    /// A commit page.
    fn commit(&self, oid: &str) -> String;
    /// "Create pull request" page comparing `head` into `base`.
    fn compare(&self, base: &str, head: &str) -> String;
}

/// A forge: recognizes remote URLs and builds links.
pub trait ForgeProvider: Send + Sync {
    /// Stable id (`github`).
    fn id(&self) -> &str;
    /// Links for this remote, if the provider recognizes it.
    fn links(&self, remote: &RemoteLocation) -> Option<Box<dyn ForgeLinks>>;
}

struct UrlLinks {
    base: String,
    branch: &'static str,
    commit: &'static str,
    compare: fn(&str, &str, &str) -> String,
}

impl ForgeLinks for UrlLinks {
    fn repo(&self) -> String {
        self.base.clone()
    }
    fn branch(&self, branch: &str) -> String {
        format!("{}{}{}", self.base, self.branch, encode(branch))
    }
    fn commit(&self, oid: &str) -> String {
        format!("{}{}{oid}", self.base, self.commit)
    }
    fn compare(&self, base: &str, head: &str) -> String {
        (self.compare)(&self.base, &encode(base), &encode(head))
    }
}

/// Percent-encodes what is unsafe in a URL path segment (keeps `/`).
fn encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~/".contains(&b) {
            out.push(char::from(b));
        } else {
            let _ = write!(out, "%{b:02X}");
        }
    }
    out
}

/// A provider matching hosts by name. `build` returns `None` when the
/// URL shape is not one the forge uses (e.g. Azure needs org/project/repo).
struct HostProvider {
    id: &'static str,
    hosts: Vec<String>,
    build: fn(&RemoteLocation) -> Option<UrlLinks>,
}

impl ForgeProvider for HostProvider {
    fn id(&self) -> &str {
        self.id
    }
    fn links(&self, remote: &RemoteLocation) -> Option<Box<dyn ForgeLinks>> {
        let matches = self
            .hosts
            .iter()
            .any(|h| remote.host == *h || remote.host.ends_with(&format!(".{h}")));
        if !matches {
            return None;
        }
        (self.build)(remote).map(|l| Box::new(l) as Box<dyn ForgeLinks>)
    }
}

#[allow(clippy::unnecessary_wraps)] // same signature as `azure`, stored as fn pointer
fn github(r: &RemoteLocation) -> Option<UrlLinks> {
    Some(UrlLinks {
        base: format!("https://{}/{}", r.host, r.path.join("/")),
        branch: "/tree/",
        commit: "/commit/",
        compare: |base, b, h| format!("{base}/compare/{b}...{h}?expand=1"),
    })
}

#[allow(clippy::unnecessary_wraps)] // same signature as `azure`, stored as fn pointer
fn gitlab(r: &RemoteLocation) -> Option<UrlLinks> {
    Some(UrlLinks {
        base: format!("https://{}/{}", r.host, r.path.join("/")),
        branch: "/-/tree/",
        commit: "/-/commit/",
        compare: |base, b, h| {
            format!(
                "{base}/-/merge_requests/new?merge_request[source_branch]={h}&merge_request[target_branch]={b}"
            )
        },
    })
}

#[allow(clippy::unnecessary_wraps)] // same signature as `azure`, stored as fn pointer
fn bitbucket(r: &RemoteLocation) -> Option<UrlLinks> {
    Some(UrlLinks {
        base: format!("https://{}/{}", r.host, r.path.join("/")),
        branch: "/branch/",
        commit: "/commits/",
        compare: |base, b, h| format!("{base}/pull-requests/new?source={h}&dest={b}"),
    })
}

fn azure(r: &RemoteLocation) -> Option<UrlLinks> {
    // https://dev.azure.com/org/project/_git/repo
    // git@ssh.dev.azure.com:v3/org/project/repo
    let segs: Vec<&str> = r
        .path
        .iter()
        .map(String::as_str)
        .filter(|s| *s != "_git" && *s != "v3")
        .collect();
    let [org, project, repo] = segs[..] else {
        return None;
    };
    Some(UrlLinks {
        base: format!("https://dev.azure.com/{org}/{project}/_git/{repo}"),
        branch: "?version=GB",
        commit: "/commit/",
        compare: |base, b, h| format!("{base}/pullrequestcreate?sourceRef={h}&targetRef={b}"),
    })
}

/// Holds forge providers; the first matching provider wins.
#[derive(Default)]
pub struct ForgeRegistry {
    providers: Vec<Arc<dyn ForgeProvider>>,
}

impl ForgeRegistry {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A registry with GitHub, GitLab, Bitbucket and Azure DevOps.
    #[must_use]
    pub fn with_builtins() -> Self {
        let mut r = Self::new();
        let host = |id, hosts: &[&str], build| {
            Arc::new(HostProvider {
                id,
                hosts: hosts.iter().map(|h| (*h).to_owned()).collect(),
                build,
            })
        };
        r.register(host("github", &["github.com"], github));
        r.register(host("gitlab", &["gitlab.com"], gitlab));
        r.register(host("bitbucket", &["bitbucket.org"], bitbucket));
        r.register(host(
            "azure",
            &["dev.azure.com", "ssh.dev.azure.com", "visualstudio.com"],
            azure,
        ));
        r
    }

    /// Registers a provider (checked before the ones registered earlier, so
    /// a fork can override a built-in for a host).
    pub fn register(&mut self, provider: Arc<dyn ForgeProvider>) {
        self.providers.insert(0, provider);
    }

    /// Links for a remote URL, from the first provider recognizing it.
    #[must_use]
    pub fn links(&self, url: &str) -> Option<Box<dyn ForgeLinks>> {
        let remote = RemoteLocation::parse(url)?;
        self.providers.iter().find_map(|p| p.links(&remote))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_remote_url_shapes() {
        for url in [
            "https://github.com/owner/repo.git",
            "git@github.com:owner/repo.git",
            "ssh://git@github.com:22/owner/repo",
            "https://user@github.com/owner/repo/",
        ] {
            let r = RemoteLocation::parse(url).unwrap();
            assert_eq!(r.host, "github.com", "{url}");
            assert_eq!(r.path, ["owner", "repo"], "{url}");
        }
        assert!(RemoteLocation::parse("C:/local/repo").is_none());
        assert!(RemoteLocation::parse("/srv/git/repo.git").is_none());
    }

    #[test]
    fn builds_links_for_each_forge() {
        let reg = ForgeRegistry::with_builtins();
        let gh = reg.links("git@github.com:o/r.git").unwrap();
        assert_eq!(gh.branch("feat/x"), "https://github.com/o/r/tree/feat/x");
        assert_eq!(
            gh.compare("main", "feat/x"),
            "https://github.com/o/r/compare/main...feat/x?expand=1"
        );
        let gl = reg.links("https://gitlab.com/group/sub/proj.git").unwrap();
        assert_eq!(
            gl.commit("abc"),
            "https://gitlab.com/group/sub/proj/-/commit/abc"
        );
        let bb = reg.links("git@bitbucket.org:team/repo.git").unwrap();
        assert_eq!(bb.repo(), "https://bitbucket.org/team/repo");
        let az = reg.links("git@ssh.dev.azure.com:v3/org/proj/repo").unwrap();
        assert_eq!(az.repo(), "https://dev.azure.com/org/proj/_git/repo");
        let az2 = reg
            .links("https://org@dev.azure.com/org/proj/_git/repo")
            .unwrap();
        assert_eq!(
            az2.branch("main"),
            "https://dev.azure.com/org/proj/_git/repo?version=GBmain"
        );
        assert!(reg.links("https://example.com/a/b.git").is_none());
    }

    #[test]
    fn forks_can_register_a_self_hosted_forge() {
        struct Gitea;
        impl ForgeProvider for Gitea {
            fn id(&self) -> &str {
                "gitea"
            }
            fn links(&self, remote: &RemoteLocation) -> Option<Box<dyn ForgeLinks>> {
                (remote.host == "git.example.com")
                    .then(|| github(remote).map(|l| Box::new(l) as Box<dyn ForgeLinks>))?
            }
        }
        let mut reg = ForgeRegistry::with_builtins();
        reg.register(Arc::new(Gitea));
        assert_eq!(
            reg.links("git@git.example.com:a/b.git").unwrap().repo(),
            "https://git.example.com/a/b"
        );
    }
}
