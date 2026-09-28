//! The innertube `context` object and the client-version bootstrap.

use std::sync::OnceLock;

use serde_json::{json, Value};

/// InnerTube client name for the YouTube Music web client.
pub const CLIENT_NAME: &str = "WEB_REMIX";
/// Numeric client id sent in the `X-YouTube-Client-Name` header.
pub const CLIENT_NAME_ID: &str = "67";
/// YouTube Music home / origin URL.
pub const HOME: &str = "https://music.youtube.com";

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
///
/// The version is scraped from the page's own `INNERTUBE_CONTEXT_CLIENT_VERSION`
/// and cached for the process lifetime; on failure it falls back to a
/// date-shaped constant.
pub async fn client_version(http: &reqwest::Client, user_agent: &str) -> String {
    if let Some(v) = CLIENT_VERSION.get() {
        return v.clone();
    }
    let version = scrape(http, user_agent)
        .await
        .unwrap_or_else(|| FALLBACK_VERSION.to_owned());
    // First writer wins; a concurrent scrape is harmless.
    let _ = CLIENT_VERSION.set(version.clone());
    version
}

async fn scrape(http: &reqwest::Client, user_agent: &str) -> Option<String> {
    let body = http
        .get(HOME)
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
