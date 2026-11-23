//! The innertube `context` object and the client-version bootstrap.

use std::sync::OnceLock;

use serde_json::{json, Value};
use tracing::warn;

/// `InnerTube` client name for the `YouTube` Music web client.
pub const CLIENT_NAME: &str = "WEB_REMIX";
/// Numeric client id sent in the `X-YouTube-Client-Name` header.
pub const CLIENT_NAME_ID: &str = "67";
/// `YouTube` Music home / origin URL.
pub const HOME: &str = "https://music.youtube.com";

/// The base URL to talk to, which is [`HOME`] everywhere except under test.
///
/// The loopback seam is compile-time gated rather than an environment variable
/// on purpose: an env override would ship in the release binary, and "silently
/// send every metadata request somewhere else" is not a capability a music
/// player should have. As a `#[cfg(test)]` substitution it does not exist in
/// anything a user runs.
#[cfg(test)]
static TEST_HOME: OnceLock<String> = OnceLock::new();

pub(crate) fn home() -> &'static str {
    #[cfg(test)]
    if let Some(url) = TEST_HOME.get() {
        return url.as_str();
    }
    HOME
}

/// Point [`home`] at `url` for the rest of the process. Test-only; the first
/// call wins, because the visitor-data and client-version caches are keyed to
/// one host for the process lifetime.
#[cfg(test)]
pub(crate) fn set_home_for_test(url: &str) {
    let _ = TEST_HOME.set(url.to_owned());
}

/// Last-resort client version when the home page can't be scraped.
pub(crate) const FALLBACK_VERSION: &str = "1.20240101.01.00";

static CLIENT_VERSION: OnceLock<String> = OnceLock::new();

/// The cache [`client_version`] reads and writes.
///
/// A shipped build has exactly one, [`CLIENT_VERSION`]. A test may install a
/// private one for the length of its body instead: H2's defect lived in this
/// entry point's *wiring* rather than in the caching rule, so the guard has to
/// drive the entry point itself, and the process-global cell cannot be cleared —
/// any test sharing it would pass or fail on which test ran first. Test-only
/// plumbing rather than a production parameter, for the same reason [`home`] is
/// compile-time gated: nothing a user runs can point the client version
/// somewhere else.
fn client_version_cache() -> &'static OnceLock<String> {
    #[cfg(test)]
    if let Some(installed) = test_cache::installed() {
        return installed;
    }
    &CLIENT_VERSION
}

/// A private client-version cache for one test, restored when it is dropped.
#[cfg(test)]
pub(crate) struct FreshClientVersionCache(Option<&'static OnceLock<String>>);

#[cfg(test)]
impl FreshClientVersionCache {
    /// Install a cache that starts empty, whatever the process-global one holds.
    pub(crate) fn install() -> Self {
        // Leaked because the accessor needs a `&'static`: one cell per test, not
        // one per request.
        let fresh: &'static OnceLock<String> = Box::leak(Box::new(OnceLock::new()));
        Self(test_cache::swap(Some(fresh)))
    }
}

#[cfg(test)]
impl Drop for FreshClientVersionCache {
    fn drop(&mut self) {
        test_cache::swap(self.0);
    }
}

#[cfg(test)]
mod test_cache {
    use super::OnceLock;
    use std::cell::RefCell;

    thread_local! {
        /// The cache installed by the test running on this thread, if any.
        ///
        /// Thread-local rather than a second process-global: a swap that every
        /// test could see would reintroduce exactly the order-dependence this
        /// exists to remove. Safe because the transport tests drive the client
        /// on a current-thread runtime, so the install and the calls it wraps
        /// stay on one thread.
        static INSTALLED: RefCell<Option<&'static OnceLock<String>>> = const { RefCell::new(None) };
    }

    pub(super) fn installed() -> Option<&'static OnceLock<String>> {
        INSTALLED.with(|slot| *slot.borrow())
    }

    /// Point the accessor at `cache`, returning what it pointed at before.
    pub(super) fn swap(
        cache: Option<&'static OnceLock<String>>,
    ) -> Option<&'static OnceLock<String>> {
        INSTALLED.with(|slot| slot.replace(cache))
    }
}

/// Build the `context` object merged into every innertube request body.
pub fn context_object(visitor_data: &str, client_version: &str) -> Value {
    json!({
        "client": {
            "clientName": CLIENT_NAME,
            "clientVersion": client_version,
            "platform": "DESKTOP",
            "visitorData": visitor_data,
            "hl": "en",
            "gl": "US",
        },
        "request": { "useSsl": true, "internalExperimentFlags": [] },
        "user": {},
    })
}

/// Fetch and cache the current `WEB_REMIX` client version from the home page.
pub async fn client_version(http: &reqwest::Client, user_agent: &str) -> String {
    if let Some(v) = client_version_cache().get() {
        return v.clone();
    }
    let scraped = scrape(http, user_agent).await;
    resolve_client_version(client_version_cache(), || scraped)
}

/// Resolve the client version, caching only a *successful* scrape.
///
/// A failed scrape must be retried rather than remembered: caching the fallback
/// would pin a stale `clientVersion` for the process lifetime, and innertube
/// rejects a stale version with 400 — which `Client::post` does not retry, since
/// it rotates visitor data only on 403 — so every metadata command would fail
/// until restart. Note that `OnceLock::get_or_init` alone is *not* enough here:
/// it memoizes the init closure's `None` result too. The resolver is therefore
/// only reachable once it has something worth caching.
fn resolve_client_version(
    cache: &OnceLock<String>,
    try_scrape: impl FnOnce() -> Option<String>,
) -> String {
    if let Some(scraped) = try_scrape() {
        // First writer wins; a concurrent scrape is harmless.
        cache.get_or_init(|| scraped).clone()
    } else {
        warn!(
            "could not scrape the WEB_REMIX client version; using the fallback for this call \
             only (the next request will try again)"
        );
        FALLBACK_VERSION.to_owned()
    }
}

async fn scrape(http: &reqwest::Client, user_agent: &str) -> Option<String> {
    let resp = http
        .get(home())
        .header(reqwest::header::USER_AGENT, user_agent)
        .send()
        .await
        .ok()?
        .error_for_status()
        .ok()?;
    let body = read_capped(resp).await?;
    extract_client_version(&body)
}

/// Largest home-page body the client-version scrape will buffer.
///
/// The same 2 MiB ceiling as the visitor scrape: the home page is a few hundred
/// kilobytes, the request's 30 s timeout bounds *time* rather than *bytes*, and
/// a broken or hostile server must not grow this buffer for as long as it keeps
/// sending. Tests shrink the constant so the streaming bound can be exercised
/// without moving megabytes through loopback.
#[cfg(not(test))]
const CLIENT_VERSION_MAX_BYTES: usize = 2 * 1024 * 1024;
#[cfg(test)]
const CLIENT_VERSION_MAX_BYTES: usize = 4 * 1024;

/// Read the home page with a hard size ceiling, returning `None` if it is
/// larger than [`CLIENT_VERSION_MAX_BYTES`].
///
/// Streaming (rather than `Response::text()`) is what makes the ceiling real:
/// `reqwest` has no body limit of its own, and a chunked response declares no
/// length at all. A declared length over the cap is rejected before the first
/// byte, and the streamed count is checked too because a server can omit or lie
/// about `Content-Length`.
async fn read_capped(mut resp: reqwest::Response) -> Option<String> {
    if let Some(declared) = resp.content_length() {
        if declared > CLIENT_VERSION_MAX_BYTES as u64 {
            return None;
        }
    }
    let mut body = Vec::new();
    while let Some(chunk) = resp.chunk().await.ok()? {
        if body.len() + chunk.len() > CLIENT_VERSION_MAX_BYTES {
            return None;
        }
        body.extend_from_slice(&chunk);
    }
    Some(String::from_utf8_lossy(&body).into_owned())
}

/// The `"INNERTUBE_CONTEXT_CLIENT_VERSION":"…"` literal in the home page.
///
/// Compiled once per process rather than once per call: this is read on every
/// client-version bootstrap, which happens on the request path. The pattern is
/// a literal, so compilation cannot fail in practice, but the helper still
/// returns `Option` rather than panicking: if it ever did fail (`None`), every
/// caller treats it as "version not found" and falls back to the built-in
/// version. The `OnceLock` keeps the fast path (no per-call recompilation).
fn client_version_regex() -> Option<&'static regex::Regex> {
    static RE: OnceLock<Option<regex::Regex>> = OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r#""INNERTUBE_CONTEXT_CLIENT_VERSION":"([\w.\-]+?)""#).ok())
        .as_ref()
}

fn extract_client_version(html: &str) -> Option<String> {
    client_version_regex()?
        .captures(html)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_client_version_reads_the_embedded_constant() {
        let html = r#"<script>window["ytcfg"].set({"INNERTUBE_CONTEXT_CLIENT_VERSION":"1.20250101.00.00","X":"y"});</script>"#;
        assert_eq!(
            extract_client_version(html).as_deref(),
            Some("1.20250101.00.00")
        );
    }

    /// L7's regression guard: the regex is compiled once, not once per scrape.
    ///
    /// `ptr::eq` is the observable consequence of caching — two calls through the
    /// accessor must hand back the *same* compiled object. The tests above pass
    /// either way, which is precisely why this one exists: without it, dropping
    /// the `OnceLock` again would be invisible to the suite.
    #[test]
    fn the_client_version_regex_is_compiled_once() {
        let (first, second) = (client_version_regex(), client_version_regex());
        assert!(
            matches!((first, second), (Some(a), Some(b)) if std::ptr::eq(a, b)),
            "the client-version regex must be cached, not rebuilt on every call"
        );
    }

    #[test]
    fn extract_client_version_is_none_when_absent_or_blank() {
        assert_eq!(extract_client_version("<html></html>"), None);
        assert_eq!(
            extract_client_version(r#""INNERTUBE_CONTEXT_CLIENT_VERSION":"""#),
            None
        );
        // A version containing characters the character class rejects is not
        // matched at all, rather than matched truncated.
        assert_eq!(
            extract_client_version(r#""INNERTUBE_CONTEXT_CLIENT_VERSION":"1.2/3""#),
            None
        );
    }

    /// The regression guard for H2. `resolve_client_version` holds the whole
    /// caching rule and is synchronous, so it is exercised deterministically
    /// here — no network, and no reliance on ordering against the process-wide
    /// cache. (`client_version` short-circuits on `CLIENT_VERSION.get()` before
    /// ever reaching this, which is the read path.)
    #[test]
    fn only_a_successful_scrape_is_cached() {
        let cache = OnceLock::new();

        // A failed scrape yields the fallback and stores nothing, so the next
        // call scrapes again rather than pinning the fallback for the process.
        let mut attempts = 0;
        let mut failing = || {
            attempts += 1;
            None
        };
        assert_eq!(
            resolve_client_version(&cache, &mut failing),
            FALLBACK_VERSION
        );
        assert_eq!(
            resolve_client_version(&cache, &mut failing),
            FALLBACK_VERSION
        );
        assert_eq!(attempts, 2, "a failed scrape must not be memoized");
        assert!(
            cache.get().is_none(),
            "the fallback must never enter the cache"
        );

        // A successful scrape *is* stored, so the read path short-circuits and
        // the value survives later failures.
        assert_eq!(
            resolve_client_version(&cache, || Some("1.20250901.00.00".to_owned())),
            "1.20250901.00.00"
        );
        assert_eq!(cache.get().map(String::as_str), Some("1.20250901.00.00"));
        assert_eq!(
            resolve_client_version(&cache, || Some("1.20251001.00.00".to_owned())),
            "1.20250901.00.00",
            "first writer wins"
        );
    }

    /// Serve `body` once, with an optional declared `Content-Length`.
    ///
    /// With no declared length the body is close-delimited, so `reqwest` cannot
    /// learn its size from the headers: the streamed byte count is the only
    /// bound (the same shape as a chunked response).
    fn serve_body(body: Vec<u8>, declared: Option<u64>) -> String {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("bind loopback");
        let port = listener.local_addr().expect("bound address").port();
        std::thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let mut head = Vec::new();
            let mut buf = [0u8; 1024];
            while head.len() < 16 * 1024 && !head.windows(4).any(|w| w == b"\r\n\r\n") {
                match stream.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => head.extend_from_slice(&buf[..n]),
                }
            }
            let length = declared.map_or_else(String::new, |n| format!("Content-Length: {n}\r\n"));
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\n{length}\
                 Connection: close\r\n\r\n"
            );
            let _ = stream.write_all(&body);
            let _ = stream.flush();
        });
        format!("http://127.0.0.1:{port}/")
    }

    fn test_http() -> reqwest::Client {
        reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("build test client")
    }

    /// The client-version scrape is a `GET` of the home page, so it needs the
    /// same kind of body ceiling the visitor scrape has. A declared length over
    /// the cap is refused before any of the body is buffered.
    #[tokio::test]
    async fn an_oversized_declared_client_version_body_is_refused() {
        let declared = u64::try_from(CLIENT_VERSION_MAX_BYTES).expect("cap fits u64") + 1;
        let url = serve_body(Vec::new(), Some(declared));
        let resp = test_http()
            .get(url)
            .send()
            .await
            .expect("loopback response");
        assert!(
            read_capped(resp).await.is_none(),
            "a declared oversized home page must not be buffered"
        );
    }

    /// A response that declares no length (chunked or close-delimited) is the
    /// case where `Response::text()` used to grow without bound; the streamed
    /// count must stop it instead.
    #[tokio::test]
    async fn an_oversized_streamed_client_version_body_is_refused() {
        let body = vec![b'x'; CLIENT_VERSION_MAX_BYTES + 64];
        let url = serve_body(body, None);
        let resp = test_http()
            .get(url)
            .send()
            .await
            .expect("loopback response");
        assert!(
            read_capped(resp).await.is_none(),
            "the streamed body count must enforce the cap"
        );
    }

    /// A page under the cap is still read whole and parsed exactly as before.
    #[tokio::test]
    async fn a_normal_client_version_body_is_read_and_parsed() {
        let html = r#"<script>window["ytcfg"].set({"INNERTUBE_CONTEXT_CLIENT_VERSION":"1.20250101.00.00"});</script>"#;
        let url = serve_body(html.as_bytes().to_vec(), None);
        let resp = test_http()
            .get(url)
            .send()
            .await
            .expect("loopback response");
        let body = read_capped(resp).await.expect("a small page is read");
        assert_eq!(
            extract_client_version(&body).as_deref(),
            Some("1.20250101.00.00")
        );
    }
}
