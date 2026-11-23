//! Cover-art / image download for the UI.
//!
//! The app crate renders `YouTube` thumbnails directly, so it needs the bytes
//! without building a client per request. This module owns a dedicated image
//! client, built from the same TLS backend and timeout policy as the backend's
//! JSON client, but with a thumbnail allow-list and a redirect policy that
//! re-validates every hop. Metadata and connectivity keep their own client and
//! therefore their behaviour.
//!
//! The fetch is deliberately defensive: HTTPS only, a host allow-list, no
//! credentials or non-default ports, a hard size cap, a checked `Content-Type`,
//! and redirects that are re-validated on every hop. A malformed or hostile
//! cover URL should cost a failed image, never a local-network request, an
//! unbounded allocation or a process launch.

use std::time::Duration;

use reqwest::Url;

use crate::backend::BackendHandle;

/// Largest image body the backend will return, in bytes.
///
/// Cover art is typically 20–200 KiB; 16 MiB is a generous ceiling for a
/// high-resolution album image and still small enough that a bad URL cannot
/// exhaust memory.
pub const MAX_IMAGE_BYTES: usize = 16 * 1024 * 1024;

/// The only hosts cover art may come from, as registrable-domain suffixes.
///
/// These are exactly the hosts the metadata parsers put in `Track::cover` /
/// `AlbumSummary::cover` (the recorded fixtures and live responses):
/// `i.ytimg.com` video thumbnails; `yt3.googleusercontent.com`,
/// `lh3.googleusercontent.com` and `yt3.ggpht.com` channel/album artwork; and
/// `www.gstatic.com` for the YouTube Music artist-avatar images
/// (`/youtube/media/ytm/images/artist_avatar…`). A suffix matches the bare
/// domain and any subdomain, and **only** at a label boundary (`notytimg.com`
/// and `ytimg.com.evil.example` do not match). `google.com` itself is
/// deliberately not here: nothing the app consumes is served from it.
const IMAGE_HOST_SUFFIXES: &[&str] = &[
    "ytimg.com",
    "ggpht.com",
    "googleusercontent.com",
    "gstatic.com",
];

/// Maximum number of redirects the image client will follow.
///
/// A cover URL is expected to be final; the small budget exists only because
/// Google's image CDN occasionally answers with one hop. Every hop is
/// re-validated by [`image_url_problem`], so a redirect cannot escape the
/// allow-list.
const MAX_IMAGE_REDIRECTS: usize = 3;

/// Connect timeout for the dedicated image client (same as the backend's JSON
/// client). Kept separate so the stricter redirect policy cannot change
/// metadata/connectivity behaviour.
const IMAGE_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// Total timeout for one image GET, bounds time rather than bytes (the body cap
/// is separate and enforced both up front and while streaming).
const IMAGE_TOTAL_TIMEOUT: Duration = Duration::from_secs(30);

impl BackendHandle {
    /// Download an image (typically `Track::cover`/album/artist artwork) and
    /// return the raw encoded bytes (PNG/JPEG/WebP as served — the caller
    /// decodes them).
    ///
    /// **Async, network I/O.** Run it from a spawned task via `backend.spawn` /
    /// the controller's task pool, never on the UI thread. Uses the backend's
    /// dedicated image client (5 s connect / 30 s total timeout, TLS backend
    /// already initialized, redirects re-validated per hop), so cover loading
    /// never builds a client per request or relaxes the JSON client's policy.
    ///
    /// Errors are human-readable `String`s:
    /// * a URL that is not absolute HTTPS, carries credentials, uses a
    ///   non-default port, or names a host outside `IMAGE_HOST_SUFFIXES` →
    ///   `"refusing to fetch an image: …"`
    /// * transport/HTTP error → `"image fetch failed: …"`
    /// * a response whose `Content-Type` is not `image/*` →
    ///   `"refusing to fetch an image: …"`
    /// * body larger than [`MAX_IMAGE_BYTES`] (declared or actual) →
    ///   `"image too large …"`
    ///
    /// # Errors
    ///
    /// Returns one of the messages above: the URL did not pass the HTTPS / host
    /// / credentials / port gate, the request failed or the server returned an
    /// error status or a non-image body, or the body exceeded the cap. A missing
    /// cover is non-fatal to the caller.
    pub async fn fetch_image(&self, url: String) -> Result<Vec<u8>, String> {
        let trimmed = url.trim();
        let parsed = Url::parse(trimmed)
            .map_err(|e| format!("refusing to fetch an image: malformed url: {e}"))?;
        if let Some(problem) = image_url_problem(&parsed) {
            return Err(format!("refusing to fetch an image: {problem}"));
        }
        download_image_with_limit(&self.inner.http_image, &parsed, MAX_IMAGE_BYTES).await
    }
}

/// Build the dedicated cover-art HTTP client.
///
/// It shares the backend's timeout policy but not its redirect policy: the JSON
/// client follows ordinary redirects, while every image hop must still be an
/// allowed HTTPS thumbnail URL. Kept as a function so the policy is exercised by
/// the same builder a release uses.
pub(crate) fn build_image_client() -> Result<reqwest::Client, reqwest::Error> {
    reqwest::Client::builder()
        .connect_timeout(IMAGE_CONNECT_TIMEOUT)
        .timeout(IMAGE_TOTAL_TIMEOUT)
        .redirect(image_redirect_policy())
        .build()
}

/// The redirect policy for the image client: re-validate every hop, cap the
/// chain, and fail the request rather than returning a `3xx` to the caller.
///
/// Re-validation is the security property: an allow-listed thumbnail host could
/// otherwise redirect a request to `127.0.0.1` or a private address, which is
/// the SSRF this gate exists to prevent.
fn image_redirect_policy() -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(|attempt| {
        // `previous()` includes the initial URL, so `> MAX` follows at most
        // `MAX_IMAGE_REDIRECTS` hops (the same accounting reqwest's built-in
        // limit uses).
        if attempt.previous().len() > MAX_IMAGE_REDIRECTS {
            return attempt.error("image fetch redirected too many times");
        }
        if let Some(problem) = image_url_problem(attempt.url()) {
            return attempt.error(format!("refusing image redirect: {problem}"));
        }
        attempt.follow()
    })
}

/// Why `url` may not be fetched as cover art, or `None` when it may.
///
/// Split out so the gate is testable without a network round trip and so the
/// same rule is applied to the initial request and to every redirect target.
fn image_url_problem(url: &Url) -> Option<&'static str> {
    if url.scheme() != "https" {
        return Some("only https urls are allowed");
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Some("credentials are not allowed in the url");
    }
    // The URL parser drops a scheme-default port, so `Some` here means an
    // explicitly non-default port. Remote services that need one are not
    // thumbnail hosts.
    if url.port().is_some() {
        return Some("only the default https port is allowed");
    }
    let Some(host) = url.host_str() else {
        return Some("url has no host");
    };
    if !is_allowed_image_host(host) {
        return Some("host is not an allowed thumbnail host");
    }
    None
}

/// Whether `host` is one of [`IMAGE_HOST_SUFFIXES`] or a subdomain of one.
///
/// The label boundary matters: a bare `ends_with("ytimg.com")` also accepts
/// `notytimg.com` and `ytimg.com.example.net`, which an attacker controls.
fn is_allowed_image_host(host: &str) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    IMAGE_HOST_SUFFIXES.iter().any(|suffix| {
        host.strip_suffix(suffix)
            .is_some_and(|prefix| prefix.is_empty() || prefix.ends_with('.'))
    })
}

/// GET `url` and return at most `limit` bytes of body.
async fn download_image_with_limit(
    http: &reqwest::Client,
    url: &Url,
    limit: usize,
) -> Result<Vec<u8>, String> {
    let mut response = http
        .get(url.clone())
        .send()
        .await
        .map_err(|e| format!("image fetch failed: {e}"))?;
    if !response.status().is_success() {
        return Err(format!("image fetch failed: HTTP {}", response.status()));
    }
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    if !content_type.starts_with("image/") {
        // Deliberately do not echo an unbounded server header into the error.
        return Err("refusing to fetch an image: response Content-Type is not image/*".to_string());
    }
    // Reject an oversized body before allocating anything when the server
    // declares its length.
    if let Some(length) = response.content_length() {
        if length > limit as u64 {
            return Err(image_too_large(limit));
        }
    }
    // A server may omit the length (chunked) or lie about it, so the cap is
    // enforced on the actual bytes too.
    let mut data = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| format!("image fetch failed: {e}"))?
    {
        if data.len() + chunk.len() > limit {
            return Err(image_too_large(limit));
        }
        data.extend_from_slice(&chunk);
    }
    Ok(data)
}

fn image_too_large(limit: usize) -> String {
    format!("image too large (over {} MiB)", limit / (1024 * 1024))
}

#[cfg(test)]
mod tests {
    use reqwest::Url;

    use super::{
        download_image_with_limit, image_redirect_policy, image_url_problem, is_allowed_image_host,
        IMAGE_CONNECT_TIMEOUT, IMAGE_TOTAL_TIMEOUT, MAX_IMAGE_BYTES,
    };

    fn parses(url: &str) -> Url {
        Url::parse(url).unwrap_or_else(|e| panic!("test URL {url:?} must parse: {e}"))
    }

    fn allowed(url: &str) -> bool {
        image_url_problem(&parses(url)).is_none()
    }

    /// The host gate is what keeps a cover URL from becoming a request to the
    /// user's machine, their LAN, or a cloud metadata endpoint.
    #[test]
    fn only_allowlisted_https_thumbnail_urls_are_fetchable() {
        for allowed_url in [
            "https://i.ytimg.com/vi/dQw4w9WgXcQ/hqdefault.jpg",
            "https://yt3.googleusercontent.com/abc=w544-h544-l90-rj",
            "https://lh3.googleusercontent.com/qLhu6Py_4_xo=w544-h544-l90-rj",
            "https://yt3.ggpht.com/abc=w226-h226",
            "https://www.gstatic.com/youtube/media/ytm/images/artist_avatar@1200.png?sqp=x",
            "https://ytimg.com/cover.jpg",
            "https://googleusercontent.com/cover.jpg",
            "https://i.ytimg.com:443/vi/x/hqdefault.jpg",
        ] {
            assert!(
                allowed(allowed_url),
                "{allowed_url:?} is a URL the app consumes and must be allowed"
            );
        }

        for refused in [
            // Plain HTTP is not acceptable for cover art.
            "http://i.ytimg.com/vi/x/hqdefault.jpg",
            // Loopback, LAN, link-local metadata and localhost names.
            "https://127.0.0.1/cover.jpg",
            "https://[::1]/cover.jpg",
            "https://192.168.1.10/cover.jpg",
            "https://10.0.0.1/cover.jpg",
            "https://169.254.169.254/latest/meta-data/",
            "https://localhost/cover.jpg",
            // Credentials, explicit ports, and unrelated hosts.
            "https://user:pass@i.ytimg.com/cover.jpg",
            "https://user@i.ytimg.com/cover.jpg",
            "https://i.ytimg.com:8443/cover.jpg",
            "https://example.com/cover.jpg",
            "https://www.google.com/cover.jpg",
            // Suffix lookalikes: the label boundary is the point.
            "https://notytimg.com/cover.jpg",
            "https://i.ytimg.com.evil.example/cover.jpg",
            "https://i.ytimg.com@evil.example/cover.jpg",
            "https://evil.example/?next=https://i.ytimg.com/cover.jpg",
        ] {
            assert!(
                !allowed(refused),
                "{refused:?} must not be fetchable as cover art"
            );
        }
    }

    #[test]
    fn the_host_suffix_check_respects_label_boundaries() {
        assert!(is_allowed_image_host("i.ytimg.com"));
        assert!(is_allowed_image_host("ytimg.com"));
        assert!(is_allowed_image_host("YT3.GOOGLEUSERCONTENT.COM"));
        assert!(!is_allowed_image_host("notytimg.com"));
        assert!(!is_allowed_image_host("ytimg.com.evil.example"));
        assert!(!is_allowed_image_host("google.com"));
    }

    #[test]
    fn the_default_cap_is_generous_but_bounded() {
        assert_eq!(MAX_IMAGE_BYTES, 16 * 1024 * 1024);
    }

    /// A one-request loopback server returning `body` with a `Content-Type`.
    fn serve_once_with_type(body: Vec<u8>, content_type: &str) -> String {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("bind loopback");
        let port = listener.local_addr().expect("bound address").port();
        let content_type = content_type.to_owned();
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
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\
                 Connection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(&body);
            let _ = stream.flush();
        });
        format!("http://127.0.0.1:{port}/cover.jpg")
    }

    fn serve_once(body: Vec<u8>) -> String {
        serve_once_with_type(body, "image/jpeg")
    }

    /// A one-request loopback server that redirects to `target`.
    fn serve_redirect(target: String) -> String {
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
            let _ = write!(
                stream,
                "HTTP/1.1 302 Found\r\nLocation: {target}\r\nContent-Length: 0\r\n\
                 Connection: close\r\n\r\n"
            );
            let _ = stream.flush();
        });
        format!("http://127.0.0.1:{port}/redirect.jpg")
    }

    /// A body the server does not declare a length for (chunked or
    /// close-delimited) can only be stopped by the streamed byte count.
    fn serve_once_without_length(body: Vec<u8>) -> String {
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
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: image/jpeg\r\nConnection: close\r\n\r\n"
            );
            let _ = stream.write_all(&body);
            let _ = stream.flush();
        });
        format!("http://127.0.0.1:{port}/cover.jpg")
    }

    fn test_client() -> reqwest::Client {
        reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("build client")
    }

    /// Loopback never passes the production host policy, so these tests drive
    /// the download function directly on a test client: the cap and
    /// `Content-Type` rules are what is under test, not the allow-list (which
    /// has its own pure tests above).
    #[tokio::test]
    async fn a_normal_image_round_trips() {
        let body = vec![0xFF, 0xD8, 0xFF, 0xE0, 1, 2, 3, 4];
        let url = parses(&serve_once(body.clone()));
        let got = download_image_with_limit(&test_client(), &url, MAX_IMAGE_BYTES)
            .await
            .expect("a small image downloads");
        assert_eq!(got, body);
    }

    /// The cap is enforced against the declared length, so an oversized body is
    /// refused before it is buffered.
    #[tokio::test]
    async fn an_oversized_declared_body_is_refused() {
        let body = vec![0u8; 64];
        let url = parses(&serve_once(body));
        let err = download_image_with_limit(&test_client(), &url, 32)
            .await
            .expect_err("a body over the cap must be refused");
        assert!(err.contains("too large"), "unexpected error: {err}");
    }

    /// A server that omits the length (or lies about it) is still bounded by the
    /// streamed byte count.
    #[tokio::test]
    async fn an_oversized_streamed_body_is_refused() {
        let body = vec![0u8; 64];
        let url = parses(&serve_once_without_length(body));
        let err = download_image_with_limit(&test_client(), &url, 32)
            .await
            .expect_err("a streamed body over the cap must be refused");
        assert!(err.contains("too large"), "unexpected error: {err}");
    }

    /// A cover host that answers with HTML (a login page, a proxy error) must
    /// not be handed to the image decoder as if it were artwork.
    #[tokio::test]
    async fn a_non_image_content_type_is_refused() {
        let url = parses(&serve_once_with_type(
            b"<html>not an image</html>".to_vec(),
            "text/html",
        ));
        let err = download_image_with_limit(&test_client(), &url, MAX_IMAGE_BYTES)
            .await
            .expect_err("an HTML body must be refused");
        assert!(err.contains("Content-Type"), "unexpected error: {err}");
    }

    /// The security property of the redirect policy: even a request that starts
    /// at an allowed URL (or, here, a direct test request) cannot be bounced to
    /// loopback by a `Location` header.
    #[tokio::test]
    async fn a_redirect_to_loopback_is_refused_by_the_image_client() {
        let target = serve_once(vec![0xFF, 0xD8, 0xFF, 0xE0]);
        // The production policy may not know this is a test, but the redirect
        // target is loopback and therefore rejected exactly as it would be in a
        // release build. The initial URL is fetched with the client directly,
        // not through `fetch_image`'s initial-URL gate.
        assert!(
            image_url_problem(&parses(&target)).is_some(),
            "the redirect target must be outside the production allow-list"
        );
        let redirect = parses(&serve_redirect(target));

        // Use the production redirect policy and timeouts, but no ambient proxy:
        // a developer/CI HTTP(S)_PROXY must not turn this policy test into a
        // proxy test. The image policy itself is what is under test.
        let client = reqwest::Client::builder()
            .redirect(image_redirect_policy())
            .connect_timeout(IMAGE_CONNECT_TIMEOUT)
            .timeout(IMAGE_TOTAL_TIMEOUT)
            .no_proxy()
            .build()
            .expect("build the redirect-test client");
        let err = client
            .get(redirect)
            .send()
            .await
            .expect_err("a loopback redirect must not be followed");
        assert!(
            err.is_redirect(),
            "the redirect policy, not a timeout, must reject the hop: {err:?}"
        );
        let source = std::error::Error::source(&err)
            .map(ToString::to_string)
            .unwrap_or_default();
        assert!(
            source.contains("refusing image redirect"),
            "the rejection should name the hop: {source:?}"
        );
    }
}
