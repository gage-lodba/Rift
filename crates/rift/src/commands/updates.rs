//! Update-check commands: query GitHub releases, toggle launch notifications,
//! and open the release page in the default browser.

use tracing::warn;

use crate::backend::BackendHandle;
use crate::util::LockExt;

/// GitHub repo (owner/name) whose releases back the update check.
const RELEASE_REPO: &str = "gage-lodba/Rift";

/// Largest update-check response body accepted, in bytes.
///
/// `/releases/latest` is a few KiB (the release notes dominate); 1 MiB is a
/// generous ceiling and keeps a misbehaving API/proxy from making the app buffer
/// an unbounded remote body just to read two fields out of it.
const MAX_UPDATE_JSON_BYTES: usize = 1024 * 1024;

impl BackendHandle {
    /// Check GitHub for a newer Rift release. Compares the running version
    /// against the latest published release's tag; surfaced in Settings.
    /// Network/parse failures degrade to "latest unknown" (still reporting the
    /// running version) rather than erroring, so the UI can always show the
    /// current version.
    ///
    /// Network I/O: run the future from a spawned task.
    ///
    /// # Errors
    ///
    /// Currently never fails: a network/too-large-body/parse problem degrades to
    /// "latest unknown" (logged) so Settings can still show the running version.
    /// The `Result` keeps the preserved command signature.
    pub async fn check_update(&self) -> Result<rift_types::UpdateStatus, String> {
        let current = env!("CARGO_PKG_VERSION").to_string();
        let mut status = rift_types::UpdateStatus {
            current: current.clone(),
            ..Default::default()
        };

        let url = format!("https://api.github.com/repos/{RELEASE_REPO}/releases/latest");
        let json: Option<serde_json::Value> = async {
            let response = self
                .inner
                .player
                .http
                .get(&url)
                // GitHub's API rejects requests without a User-Agent.
                .header(reqwest::header::USER_AGENT, "rift-update-check")
                .header(reqwest::header::ACCEPT, "application/vnd.github+json")
                .send()
                .await
                .ok()?
                .error_for_status()
                .ok()?;
            let body = body_bounded(response, MAX_UPDATE_JSON_BYTES).await?;
            serde_json::from_slice(&body).ok()
        }
        .await;

        if let Some(json) = json {
            status.latest = json
                .get("tag_name")
                .and_then(|v| v.as_str())
                .map(|s| s.trim_start_matches('v').to_string());
            status.update_available = status
                .latest
                .as_deref()
                .is_some_and(|l| is_newer(l, &current));
            status.url = json
                .get("html_url")
                .and_then(|v| v.as_str())
                .map(str::to_string);
        } else {
            warn!("update check failed to reach GitHub");
        }
        Ok(status)
    }

    /// Enable or disable the launch-time update check/notification and persist it.
    pub fn set_update_notifications(&self, enabled: bool) {
        self.inner
            .settings
            .lock_safe()
            .set_update_notifications(enabled);
    }

    /// Open an http(s) URL in the user's default browser.
    ///
    /// Used by the "Download" action on an available update. The URL is
    /// validated first: only absolute `http`/`https` URLs are opened (never an
    /// arbitrary file or custom protocol), and only in a shape an external
    /// program cannot re-parse into something other than a URL. It is then
    /// handed to the platform opener as a single argv element.
    ///
    /// # Errors
    ///
    /// Returns `"refusing to open non-http(s) url"` for anything that is not a
    /// well-formed absolute `http(s)` URL, or a `"could not open …"` message when
    /// the platform opener cannot be spawned. The URL is never passed through a
    /// shell.
    pub fn open_url(&self, url: String) -> Result<(), String> {
        if !is_openable_url(&url) {
            return Err("refusing to open non-http(s) url".into());
        }
        // Remote JSON could carry `user:password@`; strip it instead of handing
        // credentials to the OS opener (they are not used by the destination and
        // would surface in the process listing). The rest of the URL still opens.
        open_external(&strip_url_credentials(&url))
    }
}

/// Whether `url` is safe to hand to the OS opener: only `http` and `https`
/// URLs, and only in the shape a URL can actually take.
///
/// The scheme prefix is the security boundary: it keeps a backend-driven
/// `open_url` from being a generic "launch anything" primitive (`file:`,
/// `ms-settings:`, a bare executable path, a `javascript:` payload).
///
/// The rest of the check is about what happens *after* the gate: `open_url` is
/// fed URLs that came out of remote JSON (the GitHub releases API), so a value
/// with embedded whitespace or control characters must not be handed to an
/// external program at all. An empty authority is refused too — `https:///…`
/// names no host and is not a URL a browser can open. Ordinary URL characters
/// (`&`, `?`, `#`, `%`) are fine: the opener receives one argv element and the
/// Windows path never routes through a shell (see [`open_external`]).
fn is_openable_url(url: &str) -> bool {
    let Some(rest) = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
    else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    !authority.is_empty()
        && !url
            .chars()
            .any(|c| c.is_ascii_control() || c.is_whitespace())
}

/// Strip `user:password@` from a URL that passed [`is_openable_url`].
///
/// Update URLs come out of remote JSON. Nothing the browser is asked to open
/// uses HTTP basic credentials, and handing them to the platform opener would
/// expose them in the process listing; dropping the credential part keeps the
/// link working without leaking it. Falls back to the input when it cannot be
/// re-parsed (the gate above already proved the shape is `http(s)` with an
/// authority).
fn strip_url_credentials(url: &str) -> String {
    let Ok(mut parsed) = reqwest::Url::parse(url) else {
        return url.to_owned();
    };
    if parsed.username().is_empty() && parsed.password().is_none() {
        return url.to_owned();
    }
    // Both setters only fail for cannot-be-a-base URLs, which `is_openable_url`
    // rules out; restoring the original is still the safe fallback.
    if parsed.set_username("").is_err() || parsed.set_password(None).is_err() {
        return url.to_owned();
    }
    parsed.to_string()
}

/// Hand `url` to the platform's default browser.
///
/// Split out (and pure of backend state) so the gate above is the only thing an
/// external URL decision depends on.
fn open_external(url: &str) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("xdg-open")
            .arg(url)
            .spawn()
            .map(|_| ())
            .map_err(|e| format!("could not open browser: {e}"))
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(url)
            .spawn()
            .map(|_| ())
            .map_err(|e| format!("could not open browser: {e}"))
    }
    #[cfg(windows)]
    {
        // `explorer.exe` hands the URL straight to the default handler via
        // ShellExecute, so shell metacharacters such as `&` in the URL are not
        // re-parsed into a command. It's a GUI process, so no console window
        // flashes.
        //
        // There is deliberately **no** `cmd /C start "" <url>` fallback. That
        // form re-enters cmd.exe's own parser, and Rust's `Command` only quotes
        // arguments containing spaces — so a URL with no spaces but with `&`,
        // `|`, `^` or `%VAR%` would arrive as cmd syntax rather than as data.
        // The URL is remote-supplied (GitHub releases JSON), which makes that a
        // shell-injection path, and explorer.exe exists on every supported
        // Windows install, so the fallback was dead code in practice.
        std::process::Command::new("explorer.exe")
            .arg(url)
            .spawn()
            .map(|_| ())
            .map_err(|e| format!("could not open the default browser: {e}"))
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        let _ = url;
        Err("opening a browser is not supported on this platform".into())
    }
}

/// Read at most `limit` bytes of a response body, or `None` when the body is
/// larger or the read fails.
///
/// The update check reads a remote JSON document and then two fields out of it,
/// so the body is bounded before `serde_json` ever sees it: a declared length
/// over the ceiling is refused from the header, and an undeclared/chunked body
/// is cut off as soon as the bytes received pass the ceiling.
async fn body_bounded(mut response: reqwest::Response, limit: usize) -> Option<Vec<u8>> {
    if response
        .content_length()
        .is_some_and(|len| len > limit as u64)
    {
        return None;
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.ok()? {
        if body.len() + chunk.len() > limit {
            return None;
        }
        body.extend_from_slice(&chunk);
    }
    Some(body)
}

/// Compare two dotted version strings (e.g. "0.2.0" vs "0.1.3"), returning true
/// if `latest` is strictly newer. Non-numeric/missing components count as 0, so
/// pre-release suffixes are treated leniently rather than crashing the check.
fn is_newer(latest: &str, current: &str) -> bool {
    fn parts(v: &str) -> Vec<u32> {
        v.trim_start_matches('v')
            .split('.')
            .map(|p| {
                p.split(|c: char| !c.is_ascii_digit())
                    .next()
                    .unwrap_or("")
                    .parse()
                    .unwrap_or(0)
            })
            .collect()
    }
    let (l, c) = (parts(latest), parts(current));
    for i in 0..l.len().max(c.len()) {
        let (a, b) = (
            l.get(i).copied().unwrap_or(0),
            c.get(i).copied().unwrap_or(0),
        );
        if a != b {
            return a > b;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::{body_bounded, is_newer, is_openable_url, strip_url_credentials};

    /// A one-request loopback server answering with `body`, optionally
    /// declaring its `Content-Length` (without one the reply is EOF-delimited,
    /// which exercises the cap on the bytes received).
    fn serve_json(body: Vec<u8>, declare_length: bool) -> String {
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
            let headers = if declare_length {
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
            } else {
                "HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n".to_owned()
            };
            let _ = stream.write_all(headers.as_bytes());
            let _ = stream.write_all(&body);
            let _ = stream.flush();
        });
        format!("http://127.0.0.1:{port}/release")
    }

    /// The update check parses a remote JSON body and must not buffer an
    /// unbounded one: the cap holds for a declared length and for a body that is
    /// only delimited by the connection closing.
    #[tokio::test]
    async fn the_update_check_bounds_the_body_it_reads() {
        let http = reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("build client");

        let body = br#"{"tag_name":"v0.6.0"}"#.to_vec();
        let url = serve_json(body.clone(), true);
        let response = http
            .get(&url)
            .send()
            .await
            .expect("send")
            .error_for_status()
            .expect("status");
        assert_eq!(
            body_bounded(response, 1024).await.expect("under the cap"),
            body
        );

        let url = serve_json(vec![b'x'; 4096], true);
        let response = http
            .get(&url)
            .send()
            .await
            .expect("send")
            .error_for_status()
            .expect("status");
        assert!(
            body_bounded(response, 1024).await.is_none(),
            "a declared body over the cap must be refused"
        );

        let url = serve_json(vec![b'x'; 4096], false);
        let response = http
            .get(&url)
            .send()
            .await
            .expect("send")
            .error_for_status()
            .expect("status");
        assert!(
            body_bounded(response, 1024).await.is_none(),
            "a streamed body over the cap must be refused"
        );
    }

    #[test]
    fn detects_newer_versions() {
        assert!(is_newer("0.2.0", "0.1.0"));
        assert!(is_newer("0.1.1", "0.1.0"));
        assert!(is_newer("1.0.0", "0.9.9"));
        assert!(is_newer("v0.2.0", "0.1.0")); // leading v tolerated
        assert!(is_newer("0.2", "0.1.9")); // uneven lengths
    }

    #[test]
    fn ignores_same_or_older_versions() {
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("0.1.0", "0.2.0"));
        assert!(!is_newer("0.1.0", "0.1.0")); // identical
        assert!(!is_newer("0.1.0-rc1", "0.1.0")); // pre-release suffix -> 0
    }

    /// The scheme gate is what keeps a backend-driven `open_url` from being a
    /// generic "launch anything" primitive. `file:`, custom protocols and shell
    /// metacharacter payloads must be refused before any process is spawned.
    #[test]
    fn only_http_and_https_urls_are_opened() {
        assert!(is_openable_url(
            "https://github.com/gage-lodba/Rift/releases"
        ));
        assert!(is_openable_url("http://example.com"));
        for refused in [
            "file:///C:/Windows/System32/calc.exe",
            "javascript:alert(1)",
            "ms-settings:",
            "C:\\Windows\\System32\\calc.exe",
            "",
            " https://example.com",
        ] {
            assert!(!is_openable_url(refused), "must refuse {refused:?}");
        }
    }

    /// The URL comes out of remote JSON, so it has to be *a URL*, not just a
    /// string with an `http(s)://` prefix: a value carrying a newline, a space
    /// or another control character must never reach an external program, and an
    /// empty authority names no host. The second half pins the opposite rule:
    /// ordinary URL syntax (`&`, `?`, `#`, `%`) is legal and still opens.
    /// Embedded credentials pass this shape gate but are stripped by
    /// [`strip_url_credentials`] before the opener sees them (next test).
    #[test]
    fn url_shaped_strings_that_could_confuse_an_opener_are_refused() {
        for refused in [
            "https://",
            "http://",
            "https:///etc/passwd",
            "http://?next=evil",
            "https://example.com/a\nb",
            "https://example.com/a b",
            "https://example.com/a\tb",
            "https://example.com/a\u{0}b",
            "https://example.com/a\r\nb",
        ] {
            assert!(!is_openable_url(refused), "must refuse {refused:?}");
        }

        for allowed in [
            "https://github.com/gage-lodba/Rift/releases/download/v0.5.0/x?y=1&z=2#frag",
            "http://127.0.0.1:8080/health",
            "https://user:pass@example.com/",
        ] {
            assert!(is_openable_url(allowed), "must allow {allowed:?}");
        }
    }

    /// Remote JSON could carry credentials; they are stripped from what the OS
    /// opener receives, while the rest of the URL is preserved exactly.
    #[test]
    fn embedded_credentials_are_stripped_before_opening() {
        assert_eq!(
            strip_url_credentials("https://user:pass@example.com/a?b=1#frag"),
            "https://example.com/a?b=1#frag"
        );
        assert_eq!(
            strip_url_credentials("https://example.com/a?b=1"),
            "https://example.com/a?b=1"
        );
        assert_eq!(
            strip_url_credentials("not a url"),
            "not a url",
            "non-URLs are the caller's gate, not this helper's"
        );
    }
}
