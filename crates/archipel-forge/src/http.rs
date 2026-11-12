//! The HTTP transport behind the providers, and request helpers shared by
//! them (status mapping, JSON decoding, pagination).
//!
//! Providers only build [`HttpRequest`]s and parse [`HttpResponse`]s, so their
//! logic is tested against a fake transport with no network.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use archipel_model::{ForgeError, Timestamp, forge::ForgeToken};
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
    /// DELETE.
    Delete,
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
            Method::Get | Method::Delete => {
                let mut b = if request.method == Method::Get {
                    self.agent.get(url)
                } else {
                    self.agent.delete(url)
                };
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
        // CI logs run past ureq's default 10 MB limit.
        let body = response
            .body_mut()
            .with_config()
            .limit(MAX_BODY_BYTES)
            .read_to_string()
            .map_err(|e| e.to_string())?;
        Ok(HttpResponse {
            status,
            headers,
            body,
        })
    }
}

/// Largest answer body read (a CI log).
const MAX_BODY_BYTES: u64 = 64 * 1024 * 1024;

/// Entries kept at most by [`CachingTransport`]; the cache is emptied when
/// full.
const MAX_CACHED: usize = 1024;

/// Larger answers (CI logs) are not kept by [`CachingTransport`].
const MAX_CACHED_BODY: usize = 256 * 1024;

/// Makes repeated GETs conditional: the `ETag` of each answer is kept and
/// sent back as `If-None-Match`, and a `304 Not Modified` is served from the
/// kept answer. GitHub does not count 304s against the rate limit, so the
/// periodic status reads of unchanged repos cost nothing.
pub struct CachingTransport {
    inner: Arc<dyn HttpTransport>,
    /// Kept answers by request key (URL and credentials).
    cache: Mutex<HashMap<u64, (String, HttpResponse)>>,
}

impl CachingTransport {
    /// Caches the GETs sent through `inner`.
    #[must_use]
    pub fn new(inner: Arc<dyn HttpTransport>) -> Self {
        Self {
            inner,
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// The cache key of a GET: its URL and headers (the token among them,
    /// so accounts never share answers), hashed to keep no token around.
    fn key(request: &HttpRequest) -> u64 {
        let mut hasher = DefaultHasher::new();
        request.url.hash(&mut hasher);
        request.headers.hash(&mut hasher);
        hasher.finish()
    }

    fn cache(&self) -> std::sync::MutexGuard<'_, HashMap<u64, (String, HttpResponse)>> {
        self.cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Keeps a successful answer that has an `ETag`; forgets the key
    /// otherwise.
    fn keep(&self, key: u64, response: &HttpResponse) {
        let mut cache = self.cache();
        match response.header("etag") {
            Some(etag)
                if (200..=299).contains(&response.status)
                    && response.body.len() <= MAX_CACHED_BODY =>
            {
                if cache.len() >= MAX_CACHED && !cache.contains_key(&key) {
                    cache.clear();
                }
                cache.insert(key, (etag.to_owned(), response.clone()));
            }
            _ => {
                cache.remove(&key);
            }
        }
    }
}

impl HttpTransport for CachingTransport {
    fn send(&self, request: &HttpRequest) -> Result<HttpResponse, String> {
        if request.method != Method::Get {
            return self.inner.send(request);
        }
        let key = Self::key(request);
        let cached = self.cache().get(&key).cloned();
        let Some((etag, kept)) = cached else {
            let response = self.inner.send(request)?;
            self.keep(key, &response);
            return Ok(response);
        };
        let mut conditional = request.clone();
        conditional.headers.push(("If-None-Match".into(), etag));
        let response = self.inner.send(&conditional)?;
        if response.status == 304 {
            return Ok(kept);
        }
        self.keep(key, &response);
        Ok(response)
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
            // GitHub answers 403 when the rate limit is used up, with
            // `retry-after` for its secondary (abuse) limit.
            429 => Err(ForgeError::RateLimited {
                host: self.host.to_owned(),
                retry_at: retry_at(&response, Timestamp::now()),
            }),
            // Its secondary limit may come without either header, only
            // saying so in the message.
            403 if response.header("x-ratelimit-remaining") == Some("0")
                || response.header("retry-after").is_some()
                || error_message(&response.body)
                    .to_ascii_lowercase()
                    .contains("rate limit") =>
            {
                Err(ForgeError::RateLimited {
                    host: self.host.to_owned(),
                    retry_at: retry_at(&response, Timestamp::now()),
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

    /// GET `path` as text (a log).
    pub fn get_text(&self, path: &str, what: &str) -> Result<String, ForgeError> {
        Ok(self.send(Method::Get, self.url(path), None, what)?.body)
    }

    /// Sends a JSON body to `path`; the answer body is ignored.
    pub fn write_unit(
        &self,
        method: Method,
        path: &str,
        body: serde_json::Value,
        what: &str,
    ) -> Result<(), ForgeError> {
        self.send(method, self.url(path), Some(body), what)
            .map(drop)
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

    /// DELETE `path`; the answer body is ignored.
    pub fn delete(&self, path: &str, what: &str) -> Result<(), ForgeError> {
        self.send(Method::Delete, self.url(path), None, what)
            .map(drop)
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

/// For a write the user may not be allowed to do (merge): a 401 / 403
/// then means a missing permission more often than a bad token.
pub fn permission<T>(result: Result<T, ForgeError>, action: &str) -> Result<T, ForgeError> {
    scoped(
        result,
        action,
        "no permission on the repository, or the token lacks write access",
    )
}

/// Like [`permission`], naming what the token needs (`hint`): a 401 / 403
/// becomes "not allowed to `action` (`hint`)".
pub fn scoped<T>(result: Result<T, ForgeError>, action: &str, hint: &str) -> Result<T, ForgeError> {
    result.map_err(|e| match e {
        ForgeError::Unauthorized { host, status } => ForgeError::Http {
            host,
            status,
            message: format!("not allowed to {action} ({hint})"),
        },
        e => e,
    })
}

/// When a rate-limited forge accepts calls again: `retry-after` (seconds),
/// else the reset time of the limit (GitHub `x-ratelimit-reset`, GitLab
/// `ratelimit-reset`, epoch seconds).
fn retry_at(response: &HttpResponse, now: Timestamp) -> Option<Timestamp> {
    let number = |name| response.header(name)?.trim().parse::<i64>().ok();
    number("retry-after")
        .map(|secs| Timestamp(now.0 + secs))
        .or_else(|| {
            number("x-ratelimit-reset")
                .or_else(|| number("ratelimit-reset"))
                .map(Timestamp)
        })
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
    fn retry_time_of_a_rate_limit() {
        let now = Timestamp(1_000);
        let answer = |headers: &[(&str, &str)]| HttpResponse {
            status: 403,
            headers: headers
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
            body: String::new(),
        };
        assert_eq!(
            retry_at(&answer(&[("retry-after", "60")]), now),
            Some(Timestamp(1_060))
        );
        assert_eq!(
            retry_at(&answer(&[("x-ratelimit-reset", "5000")]), now),
            Some(Timestamp(5_000))
        );
        assert_eq!(retry_at(&answer(&[]), now), None);
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

    /// Answers 200 with an `ETag` unless the request carries it, then 304.
    #[derive(Default)]
    struct EtagServer {
        seen: Mutex<Vec<HttpRequest>>,
    }

    impl HttpTransport for EtagServer {
        fn send(&self, request: &HttpRequest) -> Result<HttpResponse, String> {
            self.seen.lock().unwrap().push(request.clone());
            let matches = request
                .headers
                .iter()
                .any(|(k, v)| k == "If-None-Match" && v == "\"v1\"");
            Ok(if matches {
                HttpResponse {
                    status: 304,
                    ..HttpResponse::default()
                }
            } else {
                HttpResponse {
                    status: 200,
                    headers: vec![("etag".into(), "\"v1\"".into())],
                    body: "[1]".into(),
                }
            })
        }
    }

    fn get(url: &str, token: &str) -> HttpRequest {
        HttpRequest {
            method: Method::Get,
            url: url.into(),
            headers: vec![("Authorization".into(), format!("Bearer {token}"))],
            body: None,
        }
    }

    #[test]
    fn repeated_gets_are_conditional() {
        let server = Arc::new(EtagServer::default());
        let http = CachingTransport::new(server.clone());
        let first = http.send(&get("u", "a")).unwrap();
        let second = http.send(&get("u", "a")).unwrap();
        assert_eq!((second.status, second.body.as_str()), (200, "[1]"));
        assert_eq!(first, second, "a 304 serves the kept answer");
        // Another token does not share the answer.
        http.send(&get("u", "b")).unwrap();
        let seen = server.seen.lock().unwrap();
        let conditional = |r: &HttpRequest| r.headers.iter().any(|(k, _)| k == "If-None-Match");
        assert_eq!(
            seen.iter().map(conditional).collect::<Vec<_>>(),
            [false, true, false]
        );
    }
}
