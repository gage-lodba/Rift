//! The innertube `context` object and the client-version bootstrap.

use std::sync::OnceLock;

use serde_json::{json, Value};
use tracing::warn;

/// InnerTube client name for the YouTube Music web client.
pub const CLIENT_NAME: &str = "WEB_REMIX";
/// Numeric client id sent in the `X-YouTube-Client-Name` header.
pub const CLIENT_NAME_ID: &str = "67";
/// YouTube Music home / origin URL.
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
const FALLBACK_VERSION: &str = "1.20240101.01.00";

static CLIENT_VERSION: OnceLock<String> = OnceLock::new();

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
    if let Some(v) = CLIENT_VERSION.get() {
        return v.clone();
    }
    let scraped = scrape(http, user_agent).await;
    resolve_client_version(&CLIENT_VERSION, || scraped)
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
    match try_scrape() {
        Some(scraped) => {
            // First writer wins; a concurrent scrape is harmless.
            cache.get_or_init(|| scraped).clone()
        }
        None => {
            warn!(
                "could not scrape the WEB_REMIX client version; using the fallback for this call \
                 only (the next request will try again)"
            );
            FALLBACK_VERSION.to_owned()
        }
    }
}

async fn scrape(http: &reqwest::Client, user_agent: &str) -> Option<String> {
    let body = http
        .get(home())
        .header(reqwest::header::USER_AGENT, user_agent)
        .send()
        .await
        .ok()?
        .error_for_status()
        .ok()?
        .text()
        .await
        .ok()?;
    extract_client_version(&body)
}

fn extract_client_version(html: &str) -> Option<String> {
    let re = regex::Regex::new(r#""INNERTUBE_CONTEXT_CLIENT_VERSION":"([\w.\-]+?)""#).ok()?;
    re.captures(html)
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
}
