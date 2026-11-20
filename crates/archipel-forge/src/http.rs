//! The HTTP transport behind the providers, and request helpers shared by
//! them (status mapping, JSON decoding, pagination).
//!
//! Providers only build [`HttpRequest`]s and parse [`HttpResponse`]s, so their
//! logic is tested against a fake transport with no network.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::{Read as _, Write};
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

    /// Downloads `url` (redirects followed) into `out`, calling `progress`
    /// with the bytes received so far and the total when known. Meant for
    /// large files (release archives): a long timeout instead of the API
    /// one. Any status but 200 is an error.
    fn download(
        &self,
        url: &str,
        out: &mut dyn Write,
        progress: &mut dyn FnMut(u64, Option<u64>),
    ) -> Result<(), String> {
        let _ = (url, out, progress);
        Err("this transport cannot download files".into())
    }
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
        // 204 and 304 never carry a body. GitHub still labels its 304 as
        // `Content-Encoding: gzip`, and decoding that empty body fails, so
        // every conditional GET of an unchanged answer would error out.
        let body = if matches!(status, 204 | 304) {
            String::new()
        } else {
            // CI logs run past ureq's default 10 MB limit.
            response
                .body_mut()
                .with_config()
                .limit(MAX_BODY_BYTES)
                .read_to_string()
                .map_err(|e| e.to_string())?
        };
        Ok(HttpResponse {
            status,
            headers,
            body,
        })
    }

    fn download(
        &self,
        url: &str,
        out: &mut dyn Write,
        progress: &mut dyn FnMut(u64, Option<u64>),
    ) -> Result<(), String> {
        let mut response = self
            .agent
            .get(url)
            .config()
            .timeout_global(Some(DOWNLOAD_TIMEOUT))
            .build()
            .call()
            .map_err(|e| e.to_string())?;
        let status = response.status().as_u16();
        if status != 200 {
            return Err(format!("HTTP {status}"));
        }
        let total = response.body().content_length();
        let mut reader = response
            .body_mut()
            .with_config()
            .limit(MAX_DOWNLOAD_BYTES)
            .reader();
        let mut buffer = vec![0; 64 * 1024];
        let mut received = 0_u64;
        loop {
            let read = reader.read(&mut buffer).map_err(|e| e.to_string())?;
            if read == 0 {
                return Ok(());
            }
            out.write_all(&buffer[..read]).map_err(|e| e.to_string())?;
            received += u64::try_from(read).unwrap_or(u64::MAX);
            progress(received, total);
        }
    }
}

/// Largest answer body read (a CI log).
const MAX_BODY_BYTES: u64 = 64 * 1024 * 1024;

/// Largest file downloaded (a release archive is ~30 MB).
const MAX_DOWNLOAD_BYTES: u64 = 512 * 1024 * 1024;

/// Bound of a whole download.
const DOWNLOAD_TIMEOUT: Duration = Duration::from_mins(15);

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

    fn download(
        &self,
        url: &str,
        out: &mut dyn Write,
        progress: &mut dyn FnMut(u64, Option<u64>),
    ) -> Result<(), String> {
        self.inner.download(url, out, progress)
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

/// The `message` of a JSON error body, else the start of the body. The
/// details go with it: GitHub's `errors` (`name already exists on this
/// account`), GitLab's per-field messages (`path has already been taken`).
fn error_message(body: &str) -> String {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(body) else {
        return body.chars().take(200).collect();
    };
    let Some(message) = value.get("message").or_else(|| value.get("error")) else {
        return body.chars().take(200).collect();
    };
    let text = match message {
        serde_json::Value::String(text) => text.clone(),
        // GitLab: `{"name": ["has already been taken"], …}`.
        serde_json::Value::Object(fields) => fields
            .iter()
            .flat_map(|(field, problems)| {
                let problems = match problems {
                    serde_json::Value::Array(list) => list.clone(),
                    other => vec![other.clone()],
                };
                problems.into_iter().map(move |p| match p {
                    serde_json::Value::String(p) => format!("{field} {p}"),
                    p => format!("{field} {p}"),
                })
            })
            .collect::<Vec<_>>()
            .join("; "),
        other => other.to_string(),
    };
    let details: Vec<&str> = value
        .get("errors")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|e| e.get("message").and_then(serde_json::Value::as_str))
        .collect();
    if details.is_empty() {
        text
    } else {
        format!("{text} ({})", details.join("; "))
    }
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

/// Parses an RFC 3339 / ISO 8601 date and time as the forges write them
/// (`2024-03-01T12:30:00Z`, `2024-03-01T12:30:00.123+01:00`).
#[must_use]
pub fn parse_time(text: &str) -> Option<Timestamp> {
    let text = text.trim();
    let (date, time) = text.split_once(['T', ' '])?;
    let mut d = date.splitn(3, '-').map(str::parse::<i64>);
    let (year, month, day) = (d.next()?.ok()?, d.next()?.ok()?, d.next()?.ok()?);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    // The offset: `Z`, `+hh:mm` or `-hh:mm` after the seconds.
    let (clock, offset) = match time.find(['Z', 'z', '+', '-']) {
        Some(i) => (&time[..i], &time[i..]),
        None => (time, ""),
    };
    let mut c = clock.splitn(3, ':');
    let hour: i64 = c.next()?.parse().ok()?;
    let minute: i64 = c.next()?.parse().ok()?;
    // Fractions of a second dropped.
    let second: i64 = match c.next() {
        Some(s) => s.split('.').next()?.parse().ok()?,
        None => 0,
    };
    let offset = match offset.as_bytes().first() {
        None | Some(b'Z' | b'z') => 0,
        Some(sign) => {
            let (h, m) = offset[1..].split_once(':').unwrap_or((&offset[1..], "0"));
            let secs = h.parse::<i64>().ok()? * 3_600 + m.parse::<i64>().ok()? * 60;
            if *sign == b'-' { -secs } else { secs }
        }
    };
    // Days from the civil date (Howard Hinnant's algorithm).
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(Timestamp(
        days * 86_400 + hour * 3_600 + minute * 60 + second - offset,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forge_dates_parse() {
        assert_eq!(parse_time("1970-01-01T00:00:00Z"), Some(Timestamp(0)));
        assert_eq!(
            parse_time("2024-03-01T12:30:00Z"),
            Some(Timestamp(1_709_296_200))
        );
        assert_eq!(
            parse_time("2024-03-01T13:30:00.123+01:00"),
            Some(Timestamp(1_709_296_200))
        );
        assert_eq!(
            parse_time("2024-03-01T07:30:00-05:00"),
            Some(Timestamp(1_709_296_200))
        );
        assert_eq!(parse_time("yesterday"), None);
        assert_eq!(parse_time("2024-13-01T00:00:00Z"), None);
    }

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
    fn error_messages_keep_their_details() {
        assert_eq!(error_message(r#"{"message":"Not Found"}"#), "Not Found");
        assert_eq!(
            error_message(
                r#"{"message":"Repository creation failed.","errors":[{"resource":"Repository","code":"custom","field":"name","message":"name already exists on this account"}]}"#
            ),
            "Repository creation failed. (name already exists on this account)"
        );
        assert_eq!(
            error_message(
                r#"{"message":{"name":["has already been taken"],"path":["has already been taken"]}}"#
            ),
            "name has already been taken; path has already been taken"
        );
        assert_eq!(error_message("<html>oops"), "<html>oops");
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

    /// GitHub answers a conditional GET with `304`, no body, yet
    /// `Content-Encoding: gzip`: decoding that empty body must not fail.
    #[test]
    fn ureq_reads_a_bodiless_gzip_304() {
        use std::io::{Read as _, Write as _};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/r", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut buf = [0u8; 4096];
            let _ = s.read(&mut buf).unwrap();
            s.write_all(
                b"HTTP/1.1 304 Not Modified\r\nETag: \"v1\"\r\n\
                  Content-Encoding: gzip\r\nConnection: close\r\n\r\n",
            )
            .unwrap();
        });
        let http = UreqTransport::new(Duration::from_secs(5));
        let response = http.send(&get(&url, "t")).unwrap();
        server.join().unwrap();
        assert_eq!((response.status, response.body.as_str()), (304, ""));
    }
}
