//! Finding the forge repository behind a local repository: provider
//! auto-detected from the remote URL, account matched by host.

use archipel_model::forge::{ForgeAccount, ForgeRepo};
use archipel_model::ports::RemoteInfo;

use super::{ForgeRegistry, RemoteLocation};

/// Where a local repository lives on a forge.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Located {
    /// On a forge the user is signed in to.
    Repo {
        /// The forge repository.
        repo: ForgeRepo,
        /// The remote it was found through.
        remote: String,
    },
    /// On a recognized forge, without an account for its host.
    SignedOut {
        /// The host to sign in to.
        host: String,
    },
    /// No remote, or a remote on no known forge.
    NotOnForge,
}

/// The remote to use: `preferred` (the upstream's remote), else `origin`,
/// else the first one.
#[must_use]
pub fn pick_remote<'a>(
    remotes: &'a [RemoteInfo],
    preferred: Option<&str>,
) -> Option<&'a RemoteInfo> {
    preferred
        .and_then(|p| remotes.iter().find(|r| r.name == p))
        .or_else(|| remotes.iter().find(|r| r.name == "origin"))
        .or_else(|| remotes.first())
}

/// Locates a repository from its remotes and the signed-in accounts. A
/// host counts as a forge when an account exists for it, when a link
/// provider recognizes it, or when its name says `github` / `gitlab`
/// (self-hosted instances).
#[must_use]
pub fn locate(
    remotes: &[RemoteInfo],
    preferred: Option<&str>,
    accounts: &[ForgeAccount],
    links: &ForgeRegistry,
) -> Located {
    let Some(remote) = pick_remote(remotes, preferred) else {
        return Located::NotOnForge;
    };
    let Some(location) = RemoteLocation::parse(&remote.url) else {
        return Located::NotOnForge;
    };
    if let Some(account) = accounts.iter().find(|a| a.host == location.host) {
        return Located::Repo {
            repo: ForgeRepo {
                account: account.clone(),
                path: location.path.join("/"),
            },
            remote: remote.name.clone(),
        };
    }
    let named = location
        .host
        .split('.')
        .any(|part| part == "github" || part == "gitlab");
    if named || links.links(&remote.url).is_some() {
        Located::SignedOut {
            host: location.host,
        }
    } else {
        Located::NotOnForge
    }
}

#[cfg(test)]
mod tests {
    use archipel_model::forge::ForgeKind;

    use super::*;

    fn remote(name: &str, url: &str) -> RemoteInfo {
        RemoteInfo {
            name: name.into(),
            url: url.into(),
        }
    }

    #[test]
    fn locates_through_the_preferred_remote() {
        let links = ForgeRegistry::with_builtins();
        let account = ForgeAccount {
            host: "gitlab.corp.example".into(),
            kind: ForgeKind::gitlab(),
            api_url: "https://gitlab.corp.example/api/v4".into(),
            user: "me".into(),
        };
        let remotes = [
            remote("origin", "git@github.com:me/fork.git"),
            remote(
                "upstream",
                "ssh://git@gitlab.corp.example:2222/team/sub/app.git",
            ),
        ];
        let accounts = std::slice::from_ref(&account);
        match locate(&remotes, Some("upstream"), accounts, &links) {
            Located::Repo { repo, remote } => {
                assert_eq!(repo.path, "team/sub/app");
                assert_eq!(remote, "upstream");
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(
            locate(&remotes, None, accounts, &links),
            Located::SignedOut {
                host: "github.com".into()
            }
        );
        assert_eq!(
            locate(
                &[remote("origin", "https://git.example.org/a/b.git")],
                None,
                accounts,
                &links
            ),
            Located::NotOnForge
        );
        assert_eq!(
            locate(&[remote("o", "/srv/repo.git")], None, &[], &links),
            Located::NotOnForge
        );
        assert_eq!(locate(&[], None, &[], &links), Located::NotOnForge);
    }
}
