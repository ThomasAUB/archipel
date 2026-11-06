//! The HTTP transport behind the providers, and request helpers shared by
//! them (status mapping, JSON decoding, pagination).
//!
//! Providers only build [`HttpRequest`]s and parse [`HttpResponse`]s, so their
//! logic is tested against a fake transport with no network.

use std::fmt::Write as _;
use std::time::Duration;

use archipel_model::{ForgeError, forge::ForgeToken};
use serde::de::DeserializeOwned;

/// HTTP method.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    /// GET.
    Get,
    /// POST.
    Post,
    /// PUT.
    Put,
    /// PATCH.
    Patch,
}

/// A request to a forge API.
#[derive(Clone, PartialEq, Eq)]
pub struct HttpRequest {
    /// Method.
    pub method: Method,
    /// Absolute URL.
    pub url: String,
    /// Headers (the authorization header included).
    pub headers: Vec<(String, String)>,
    /// JSON body.
    pub body: Option<String>,
}

impl std::fmt::Debug for HttpRequest {
    /// Header values are left out: one of them is the token.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpRequest")
            .field("method", &self.method)
            .field("url", &self.url)
            .field("body", &self.body)
            .finish_non_exhaustive()
    }
}

/// A response.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HttpResponse {
    /// Status code.
    pub status: u16,
    /// Headers, names lowercase.
    pub headers: Vec<(String, String)>,
    /// Body text.
    pub body: String,
}

impl HttpResponse {
    /// A header value by lowercase name.
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }
}

/// Sends HTTP requests. Blocking, bounded by a timeout.
pub trait HttpTransport: Send + Sync {
    /// Sends `request`. HTTP error statuses are responses, not errors; `Err`
    /// is a transport failure (DNS, TLS, timeout), as a message.
    fn send(&self, request: &HttpRequest) -> Result<HttpResponse, String>;
}

/// The real transport: `ureq` with rustls and the OS trust store (so
/// self-hosted forges behind a corporate CA work), proxy from the
/// environment.
pub struct UreqTransport {
    agent: ureq::Agent,
}

impl UreqTransport {
    /// A transport whose every request is bounded by `timeout`.
    #[must_use]
    pub fn new(timeout: Duration) -> Self {
        let tls = ureq::tls::TlsConfig::builder()
            .root_certs(ureq::tls::RootCerts::PlatformVerifier)
            .build();
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(timeout))
            .http_status_as_error(false)
            .user_agent(concat!("archipel/", env!("CARGO_PKG_VERSION")))
            .tls_config(tls)
            .build();
        Self {
            agent: ureq::Agent::new_with_config(config),
        }
    }
}

impl HttpTransport for UreqTransport {
    fn send(&self, request: &HttpRequest) -> Result<HttpResponse, String> {
        let url = request.url.as_str();
        let result = match request.method {
            Method::Get => {
                let mut b = self.agent.get(url);
                for (k, v) in &request.headers {
                    b = b.header(k.as_str(), v.as_str());
                }
                b.call()
            }
            Method::Post | Method::Put | Method::Patch => {
                let mut b = match request.method {
                    Method::Post => self.agent.post(url),
                    Method::Put => self.agent.put(url),
                    _ => self.agent.patch(url),
                };
                for (k, v) in &request.headers {
                    b = b.header(k.as_str(), v.as_str());
                }
                b.header("Content-Type", "application/json")
                    .send(request.body.as_deref().unwrap_or("{}"))
            }
        };
        let mut response = result.map_err(|e| e.to_string())?;
        let headers = response
            .headers()
            .iter()
            .filter_map(|(k, v)| Some((k.as_str().to_owned(), v.to_str().ok()?.to_owned())))
            .collect();
        let status = response.status().as_u16();
        let body = response
            .body_mut()
            .read_to_string()
            .map_err(|e| e.to_string())?;
        Ok(HttpResponse {
            status,
            headers,
            body,
        })
    }
}

/// How a provider authenticates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Auth {
    /// `Authorization: Bearer <token>` (GitHub).
    Bearer,
    /// `PRIVATE-TOKEN: <token>` (GitLab).
    PrivateToken,
}

/// Pages followed at most when listing (100 items per page).
pub const MAX_PAGES: usize = 10;

/// One authenticated conversation with a forge API.
pub struct Api<'a> {
    /// Transport.
    pub http: &'a dyn HttpTransport,
    /// Host, for error messages.
    pub host: &'a str,
    /// API base URL, without trailing slash.
    pub base: &'a str,
    /// Token.
    pub token: &'a ForgeToken,
    /// Authentication scheme.
    pub auth: Auth,
    /// Extra headers sent with every request.
    pub extra_headers: &'a [(&'a str, &'a str)],
}

impl Api<'_> {
    fn request(&self, method: Method, url: String, body: Option<String>) -> HttpRequest {
        let mut headers: Vec<(String, String)> = self
            .extra_headers
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        headers.push(match self.auth {
            Auth::Bearer => (
                "Authorization".into(),
                format!("Bearer {}", self.token.expose()),
            ),
            Auth::PrivateToken => ("PRIVATE-TOKEN".into(), self.token.expose().to_owned()),
        });
        HttpRequest {
            method,
            url,
            headers,
            body,
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    /// Sends a request and maps error statuses. `what` names the object for
    /// "not found" messages.
    pub fn send(
        &self,
        method: Method,
        url: String,
        body: Option<serde_json::Value>,
        what: &str,
    ) -> Result<HttpResponse, ForgeError> {
        let request = self.request(method, url, body.map(|b| b.to_string()));
        let response = self
            .http
            .send(&request)
            .map_err(|message| ForgeError::Network {
                host: self.host.to_owned(),
                message,
            })?;
        match response.status {
            200..=299 => Ok(response),
            429 => Err(ForgeError::RateLimited {
                host: self.host.to_owned(),
            }),
            // GitHub answers 403 when the rate limit is used up.
            403 if response.header("x-ratelimit-remaining") == Some("0") => {
                Err(ForgeError::RateLimited {
                    host: self.host.to_owned(),
                })
            }
            401 | 403 => Err(ForgeError::Unauthorized {
                host: self.host.to_owned(),
                status: response.status,
            }),
            404 => Err(ForgeError::NotFound {
                host: self.host.to_owned(),
                what: what.to_owned(),
            }),
            status => Err(ForgeError::Http {
                host: self.host.to_owned(),
                status,
                message: error_message(&response.body),
            }),
        }
    }

    /// Decodes a JSON body.
    pub fn decode<T: DeserializeOwned>(&self, response: &HttpResponse) -> Result<T, ForgeError> {
        serde_json::from_str(&response.body).map_err(|e| ForgeError::Decode {
            host: self.host.to_owned(),
            message: e.to_string(),
        })
    }

    /// GET `path` (relative to the API base) and decode it.
    pub fn get<T: DeserializeOwned>(&self, path: &str, what: &str) -> Result<T, ForgeError> {
        let response = self.send(Method::Get, self.url(path), None, what)?;
        self.decode(&response)
    }

    /// Sends a JSON body to `path` and decodes the answer.
    pub fn write<T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: serde_json::Value,
        what: &str,
    ) -> Result<T, ForgeError> {
        let response = self.send(method, self.url(path), Some(body), what)?;
        self.decode(&response)
    }

    /// GET every page of a list (following `Link: rel="next"`), up to
    /// [`MAX_PAGES`].
    pub fn get_pages<T: DeserializeOwned>(
        &self,
        path: &str,
        what: &str,
    ) -> Result<Vec<T>, ForgeError> {
        let mut out = Vec::new();
        let mut url = Some(self.url(path));
        for _ in 0..MAX_PAGES {
            let Some(current) = url.take() else { break };
            let response = self.send(Method::Get, current, None, what)?;
            out.extend(self.decode::<Vec<T>>(&response)?);
            // The token goes with every page: only follow the API itself.
            url = response
                .header("link")
                .and_then(next_link)
                .filter(|next| next.starts_with(&format!("{}/", self.base)));
        }
        if url.is_some() {
            tracing::warn!(
                host = self.host,
                what,
                "list truncated after {MAX_PAGES} pages"
            );
        }
        Ok(out)
    }
}

/// `Ok(None)` for answers that mean "not available to this token" (a
/// missing scope, a hidden object) when the caller can do without; other
/// errors (network, server, rate limit) still fail.
pub fn optional<T>(result: Result<T, ForgeError>) -> Result<Option<T>, ForgeError> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(ForgeError::Unauthorized { .. } | ForgeError::NotFound { .. }) => Ok(None),
        Err(e) => Err(e),
    }
}

/// The `rel="next"` URL of a `Link` header.
#[must_use]
pub fn next_link(header: &str) -> Option<String> {
    header.split(',').find_map(|part| {
        let (url, params) = part.split_once(';')?;
        params
            .split(';')
            .any(|p| p.trim() == "rel=\"next\"")
            .then(|| {
                url.trim()
                    .trim_start_matches('<')
                    .trim_end_matches('>')
                    .to_owned()
            })
    })
}

/// The `message` of a JSON error body, else the start of the body.
fn error_message(body: &str) -> String {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| {
            v.get("message")
                .or_else(|| v.get("error"))
                .map(|m| m.as_str().map_or_else(|| m.to_string(), str::to_owned))
        })
        .unwrap_or_else(|| body.chars().take(200).collect())
}

/// Percent-encodes everything except unreserved characters (a path
/// segment or query value; `/` is encoded too).
#[must_use]
pub fn encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(char::from(b));
        } else {
            // Writing to a String cannot fail.
            let _ = write!(out, "%{b:02X}");
        }
    }
    out
}

/// Percent-encodes a path, keeping `/` separators.
#[must_use]
pub fn encode_path(path: &str) -> String {
    path.split('/').map(encode).collect::<Vec<_>>().join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn link_header_and_encoding() {
        let link = "<https://api.github.com/x?page=2>; rel=\"next\", <https://api.github.com/x?page=5>; rel=\"last\"";
        assert_eq!(
            next_link(link).as_deref(),
            Some("https://api.github.com/x?page=2")
        );
        assert_eq!(next_link("<a>; rel=\"last\""), None);
        assert_eq!(encode("feat/x y"), "feat%2Fx%20y");
        assert_eq!(encode_path("o/r é"), "o/r%20%C3%A9");
        assert_eq!(error_message("{\"message\":\"Bad\"}"), "Bad");
    }

    #[test]
    fn request_debug_hides_the_token() {
        let req = HttpRequest {
            method: Method::Get,
            url: "u".into(),
            headers: vec![("Authorization".into(), "Bearer secret".into())],
            body: None,
        };
        assert!(!format!("{req:?}").contains("secret"));
    }
}
