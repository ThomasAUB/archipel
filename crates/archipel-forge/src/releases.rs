//! Archipel's own releases on GitHub, for the update check: the public
//! "latest release" endpoint, no account needed.

use std::sync::Arc;

use archipel_model::ForgeError;
use archipel_model::forge::Release;
use archipel_model::ports::ReleaseFeed;
use serde::Deserialize;

use crate::http::{HttpRequest, HttpTransport, Method};

const HOST: &str = "github.com";

/// The releases of one GitHub repository.
pub struct GitHubReleases {
    http: Arc<dyn HttpTransport>,
    /// `owner/name`.
    repo: String,
}

impl GitHubReleases {
    /// The releases of `repo` (`owner/name`).
    pub fn new(http: Arc<dyn HttpTransport>, repo: impl Into<String>) -> Self {
        Self {
            http,
            repo: repo.into(),
        }
    }

    /// The releases of the repository at `url` (`https://github.com/owner/name`,
    /// e.g. the crate's `repository` field); `None` for any other URL.
    pub fn from_repository_url(http: Arc<dyn HttpTransport>, url: &str) -> Option<Self> {
        let path = url
            .strip_prefix("https://github.com/")?
            .trim_end_matches('/')
            .trim_end_matches(".git");
        let (owner, name) = path.split_once('/')?;
        (!owner.is_empty() && !name.is_empty() && !name.contains('/'))
            .then(|| Self::new(http, path))
    }
}

#[derive(Deserialize)]
struct Latest {
    tag_name: String,
    html_url: String,
}

impl ReleaseFeed for GitHubReleases {
    fn latest_release(&self) -> Result<Option<Release>, ForgeError> {
        let request = HttpRequest {
            method: Method::Get,
            url: format!("https://api.github.com/repos/{}/releases/latest", self.repo),
            headers: vec![
                ("Accept".into(), "application/vnd.github+json".into()),
                ("X-GitHub-Api-Version".into(), "2022-11-28".into()),
            ],
            body: None,
        };
        let response = self
            .http
            .send(&request)
            .map_err(|message| ForgeError::Network {
                host: HOST.into(),
                message,
            })?;
        match response.status {
            200 => {}
            // No release published yet (drafts and pre-releases are not "latest").
            404 => return Ok(None),
            status => {
                return Err(ForgeError::Http {
                    host: HOST.into(),
                    status,
                    message: response.body.chars().take(200).collect(),
                });
            }
        }
        let latest: Latest =
            serde_json::from_str(&response.body).map_err(|e| ForgeError::Decode {
                host: HOST.into(),
                message: e.to_string(),
            })?;
        Ok(Some(Release {
            version: latest.tag_name.trim_start_matches('v').to_owned(),
            url: latest.html_url,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::HttpResponse;

    struct Answer(u16, &'static str);

    impl HttpTransport for Answer {
        fn send(&self, request: &HttpRequest) -> Result<HttpResponse, String> {
            assert_eq!(
                request.url,
                "https://api.github.com/repos/o/r/releases/latest"
            );
            Ok(HttpResponse {
                status: self.0,
                headers: Vec::new(),
                body: self.1.into(),
            })
        }
    }

    fn feed(status: u16, body: &'static str) -> GitHubReleases {
        GitHubReleases::from_repository_url(
            Arc::new(Answer(status, body)),
            "https://github.com/o/r",
        )
        .unwrap()
    }

    #[test]
    fn latest_release_and_none_published() {
        let body =
            r#"{"tag_name":"v1.2.0","html_url":"https://github.com/o/r/releases/tag/v1.2.0"}"#;
        assert_eq!(
            feed(200, body).latest_release().unwrap(),
            Some(Release {
                version: "1.2.0".into(),
                url: "https://github.com/o/r/releases/tag/v1.2.0".into(),
            })
        );
        assert_eq!(feed(404, "").latest_release().unwrap(), None);
        assert!(feed(500, "oops").latest_release().is_err());
    }

    #[test]
    fn repository_urls() {
        let http: Arc<dyn HttpTransport> = Arc::new(Answer(404, ""));
        let repo = |url| GitHubReleases::from_repository_url(http.clone(), url).map(|f| f.repo);
        assert_eq!(repo("https://github.com/o/r/").as_deref(), Some("o/r"));
        assert_eq!(repo("https://github.com/o/r.git").as_deref(), Some("o/r"));
        assert_eq!(repo("https://gitlab.com/o/r"), None);
        assert_eq!(repo("https://github.com/o"), None);
    }
}
