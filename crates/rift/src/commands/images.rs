//! Cover-art / image download for the UI.
//!
//! The app crate renders `YouTube` thumbnails directly, so it needs the bytes
//! without creating a second HTTP client (and a second TLS stack, connection
//! pool and timeout policy). This module exposes the backend's shared
//! [`reqwest::Client`] for exactly that: one bounded GET.
//!
//! The fetch is deliberately defensive: only `http(s)` URLs, a hard size cap,
//! and the client's existing 30 s total timeout. A malformed or hostile cover
//! URL should cost a failed image, never an unbounded allocation or a process
//! launch.

use crate::backend::BackendHandle;

/// Largest image body the backend will return, in bytes.
///
/// Cover art is typically 20–200 KiB; 16 MiB is a generous ceiling for a
/// high-resolution album image and still small enough that a bad URL cannot
/// exhaust memory.
pub const MAX_IMAGE_BYTES: usize = 16 * 1024 * 1024;

impl BackendHandle {
    /// Download an image (typically `Track::cover`/album/artist artwork) and
    /// return the raw encoded bytes (PNG/JPEG/WebP as served — the caller
    /// decodes them).
    ///
    /// **Async, network I/O.** Run it from a spawned task via `backend.spawn` /
    /// the controller's task pool, never on the UI thread. Uses the backend's
    /// shared `reqwest::Client` (5 s connect / 30 s total timeout, TLS backend
    /// already initialized), so cover loading does not create a second client.
    ///
    /// Errors are human-readable `String`s:
    /// * non-`http(s)` scheme → `"refusing to fetch a non-http(s) image url"`
    /// * transport/HTTP error → `"image fetch failed: …"`
    /// * body larger than [`MAX_IMAGE_BYTES`] (declared or actual) →
    ///   `"image too large …"`
    pub async fn fetch_image(&self, url: String) -> Result<Vec<u8>, String> {
        let trimmed = url.trim();
        if !is_fetchable_image_url(trimmed) {
            return Err("refusing to fetch a non-http(s) image url".to_string());
        }
        download_image_with_limit(&self.inner.player.http, trimmed, MAX_IMAGE_BYTES).await
    }
}

/// Whether a URL may be fetched as an image: absolute `http(s)` only.
///
/// Split out so the gate is testable without a network round trip (and so the
/// rule is one expression rather than duplicated at call sites).
fn is_fetchable_image_url(url: &str) -> bool {
    url.starts_with("https://") || url.starts_with("http://")
}

/// GET `url` and return at most `limit` bytes of body.
async fn download_image_with_limit(
    http: &reqwest::Client,
    url: &str,
    limit: usize,
) -> Result<Vec<u8>, String> {
    let mut response = http
        .get(url)
        .send()
        .await
        .map_err(|e| format!("image fetch failed: {e}"))?;
    if !response.status().is_success() {
        return Err(format!("image fetch failed: HTTP {}", response.status()));
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
    use super::{download_image_with_limit, is_fetchable_image_url, MAX_IMAGE_BYTES};

    /// The scheme gate is what keeps a cover URL from becoming a local-file read
    /// or a process launch.
    #[test]
    fn only_http_and_https_urls_are_fetchable() {
        assert!(is_fetchable_image_url(
            "https://i.ytimg.com/vi/dQw4w9WgXcQ/hqdefault.jpg"
        ));
        assert!(is_fetchable_image_url("http://example.com/cover.png"));
        for refused in [
            "",
            "file:///C:/Windows/System32/calc.exe",
            "data:image/png;base64,AAAA",
            "C:\\covers\\a.png",
            "javascript:alert(1)",
            " https://example.com/cover.png",
        ] {
            assert!(!is_fetchable_image_url(refused), "must refuse {refused:?}");
        }
    }

    #[test]
    fn the_default_cap_is_generous_but_bounded() {
        assert_eq!(MAX_IMAGE_BYTES, 16 * 1024 * 1024);
    }

    /// A one-request loopback server returning `body` with a `Content-Length`.
    fn serve_once(body: Vec<u8>) -> String {
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
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(&body);
            let _ = stream.flush();
        });
        format!("http://127.0.0.1:{port}/cover.jpg")
    }

    #[tokio::test]
    async fn a_normal_image_round_trips() {
        let body = vec![0xFF, 0xD8, 0xFF, 0xE0, 1, 2, 3, 4];
        let url = serve_once(body.clone());
        let http = reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("build client");
        let got = download_image_with_limit(&http, &url, MAX_IMAGE_BYTES)
            .await
            .expect("a small image downloads");
        assert_eq!(got, body);
    }

    /// The cap is enforced against the declared length, so an oversized body is
    /// refused before it is buffered.
    #[tokio::test]
    async fn an_oversized_declared_body_is_refused() {
        let body = vec![0u8; 64];
        let url = serve_once(body);
        let http = reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("build client");
        let err = download_image_with_limit(&http, &url, 32)
            .await
            .expect_err("a body over the cap must be refused");
        assert!(err.contains("too large"), "unexpected error: {err}");
    }
}
