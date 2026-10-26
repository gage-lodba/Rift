//! Clean-room native audio stream resolution via JS-less innertube clients.
//!
//! `YouTube`'s player endpoint normally wants the signature and `n` parameters
//! solved by running the video player's JavaScript. A few innertube clients are
//! served direct `url` fields instead (no `signatureCipher`, no `n` throttling
//! parameter, no PO token), and those URLs are directly downloadable with a
//! plainly ranged HTTP GET (an un-ranged GET is throttled to a trickle; see
//! `docs/native-streaming.md` §2.3).
//!
//! Requests are built from observed traffic only: see
//! `docs/native-streaming.md` §2.2 and the clean-room policy in §5.
//!
//! Clients are a small data table (`ClientConfig`) tried in order by
//! [`resolve`], so a client reshuffle is a table edit plus a loopback test
//! rather than a new request path. Today the table has one entry — `VISIONOS` —
//! because it is the only JS-less client empirically shown to return *fully
//! downloadable* audio URLs; §3.1 records the probe of the alternatives and the
//! revisit trigger.
//!
//! This module resolves one [`AudioStream`] at a time. Downloading it (ranged
//! GETs, retries, progress) belongs to the caller — `rift::stream` in the app.

use std::sync::OnceLock;
use std::time::Duration;

use reqwest::header;
use serde_json::{json, Value};
use tracing::debug;

use crate::error::{Error, Result};
use crate::identity;

/// `Origin` header the player endpoint is asked with.
const ORIGIN: &str = "https://www.youtube.com";
/// The player base URL compiled into release builds.
///
/// [`player_base`] is the only reader. Under `cfg(test)` a test may substitute a
/// loopback address for it (see [`PlayerBaseOverride`]), so transport tests drive
/// the shipped request path without touching the real site. The seam is
/// compile-time gated, like `context::home`: a user's environment cannot redirect
/// stream resolution, and a release build contains no override at all.
const PLAYER_BASE: &str = "https://www.youtube.com";
/// The desktop Firefox user agent the visitor pool is scraped with.
///
/// [`identity::visitor_data`] takes the user agent as a parameter, and the
/// metadata client's constant of the same value is private to `client.rs`; this
/// mirrors it so the resolver scrapes the anonymous session exactly the way the
/// existing, verified metadata flow does.
const IDENTITY_USER_AGENT: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:128.0) Gecko/20100101 Firefox/128.0";
/// The `VISIONOS` Safari user agent: part of that client's wire identity.
const VISIONOS_USER_AGENT: &str =
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 15_7_3) AppleWebKit/605.1.15 \
     (KHTML, like Gecko) Version/26.0 Safari/605.1.15";

/// Hard ceiling on the player JSON body the resolver will buffer.
///
/// Player responses for music videos are a few hundred kilobytes; this is a
/// deliberately generous multiple of that, because [`reqwest::Response`] has no
/// ceiling of its own and a broken (or hostile) endpoint could otherwise grow a
/// buffer for as long as it keeps sending. Parsing walks a complete
/// [`serde_json::Value`], so the body is read whole by design — bounding it, not
/// streaming it, is the mitigation.
const MAX_PLAYER_BODY_BYTES: u64 = 8 * 1024 * 1024;

/// Longest excerpt of a non-JSON error body copied into an error message.
const MAX_ERROR_EXCERPT_CHARS: usize = 200;

/// One JS-less innertube client the resolver can ask for a player response.
///
/// This is the client table from `docs/native-streaming.md` §3.1: a row is data,
/// not a request path. Adding a client means a new constant, a context builder
/// for any client-specific fields, and a loopback test asserting the request it
/// produces — the chain below then tries it automatically. The fields are the
/// protocol facts recorded from observed traffic; `client_name`/`client_version`
/// and the `X-YouTube-Client-*` headers must describe the same client.
struct ClientConfig {
    /// Stable name used in logs and [`AudioStream::client`]; matches
    /// `client_name` today, but kept separate so a table row can be named after
    /// a variant if a client ever needs two configurations.
    name: &'static str,
    /// `context.client.clientName`.
    client_name: &'static str,
    /// `context.client.clientVersion` and `X-YouTube-Client-Version`.
    client_version: &'static str,
    /// `X-YouTube-Client-Name`.
    client_name_id: &'static str,
    /// `User-Agent` header (some clients also repeat it in the context).
    user_agent: &'static str,
    /// Client-specific extras merged into `context.client` (device fields,
    /// platform details, ...). [`merge_object`] applies them over the common
    /// fields, so a builder cannot silently drop `clientName` and friends.
    extra_context_client: fn() -> Value,
}

/// The `VISIONOS` client configuration: the single entry the shipped chain uses.
///
/// This reproduces the recorded request from `docs/native-streaming.md` §2.2
/// (client id 101, version 1.02, Apple device context, Safari visionOS user
/// agent). It is the only client in the 2026-10-26 probe that returned
/// *downloadable* direct audio URLs — see [`CHAIN`].
const VISIONOS: ClientConfig = ClientConfig {
    name: "VISIONOS",
    client_name: "VISIONOS",
    client_version: "1.02",
    client_name_id: "101",
    user_agent: VISIONOS_USER_AGENT,
    extra_context_client: visionos_context_fields,
};

/// The `context.client` fields only the `VISIONOS` client sends.
fn visionos_context_fields() -> Value {
    json!({
        "deviceMake": "Apple",
        "deviceModel": "RealityDevice17,1",
        "osName": "visionOS",
        "osVersion": "26.5.23O471",
    })
}

/// The clients [`resolve`] tries, in preference order.
///
/// Only clients empirically shown to serve **fully downloadable** direct audio
/// URLs belong here; a client that resolves but whose URLs are gated is worse
/// than no fallback, because the downloader would start a transfer and then hit
/// a 403 instead of falling back immediately. The 2026-10-26 live probe
/// (`docs/native-streaming.md` §3.1) found exactly one such client:
///
/// * `VISIONOS` — direct URLs, 20/20 ids downloaded with ranged GETs.
/// * `ANDROID_VR` — resolves and returns direct URLs, but googlevideo serves
///   only the first ~1 MiB of those URLs and rejects any later/offset range
///   without a GVS PO token; deliberately not shipped.
/// * `TVHTML5_SIMPLY`, `MWEB` — playable only with a `signatureTimestamp`, and
///   then every audio format carries `signatureCipher` (needs JS solving).
/// * `TVHTML5`, `WEB` — unplayable from a clean-room request.
/// * `WEB_EMBEDDED_PLAYER` — needs JS-generated encrypted host flags.
/// * `IOS` — SABR-only: audio formats have no URL at all.
const CHAIN: &[ClientConfig] = &[VISIONOS];

/// One directly downloadable audio stream resolved from the player endpoint.
///
/// The URL is signed and time-limited: it must be used (and re-resolved) by the
/// downloader rather than stored. `itag`/`mime_type` describe the format the
/// selection rule picked, `content_length` — when the response reported it — is
/// what a downloader can verify its byte count against, and `client` names the
/// innertube client that answered (useful when the preferred one was not the one
/// that succeeded, and for the live probes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioStream {
    /// Direct `googlevideo` URL (already signed/authorized; expires).
    pub url: String,
    /// Format id, e.g. `140`.
    pub itag: u32,
    /// MIME type as reported, e.g. `audio/mp4; codecs="mp4a.40.2"`.
    pub mime_type: String,
    /// Total bytes as reported by the format (`Content-Length` equivalent).
    pub content_length: Option<u64>,
    /// Name of the client configuration that served this stream (a
    /// `ClientConfig::name`, e.g. `"VISIONOS"`).
    pub client: &'static str,
}

/// Resolve the preferred directly downloadable audio stream for a video id.
///
/// Tries the JS-less innertube `CHAIN` in order (preferred client first),
/// sending each client's recorded player request to `www.youtube.com` with an
/// anonymous visitor id from the crate-wide pool, then picks a format from
/// `streamingData.adaptiveFormats`. The selection order is itag `140` (AAC
/// 128 kbit/s, the format the pre-native app streamed) → `141` →
/// `139` → any other `audio/mp4` entry → any audio-only entry. Entries with
/// `signatureCipher` or without a direct `url` are skipped: solving them would
/// need the JavaScript this resolver deliberately does without.
///
/// One visitor rotation is attempted per resolve when a client's request is
/// rejected with HTTP 403 or the player answers
/// `playabilityStatus.status == "LOGIN_REQUIRED"` (the "confirm you're not a
/// bot" rejection): the rejected id is evicted from the pool and the request is
/// retried once with the id the pool then hands out. If a client still cannot
/// serve the video, the next entry in the chain is tried; a transport-level
/// failure ends the chain because it says nothing about the client.
///
/// No `playbackContext`/`signatureTimestamp` is sent: it was verified unnecessary
/// for this client family (`docs/native-streaming.md` §2.2).
///
/// # Errors
///
/// Returns the error of the first (preferred) client that failed when the whole
/// chain is exhausted: [`Error::Http`] on a transport failure (DNS, TLS,
/// timeout, reset — this also ends the chain); [`Error::Bootstrap`] when no
/// anonymous visitor identity can be established; [`Error::Api`] when the player
/// endpoint answers with a non-success status; [`Error::ResponseTooLarge`] for a
/// response body over the resolver's ceiling; [`Error::Parse`] when the body is
/// not JSON or carries no `playabilityStatus.status`; [`Error::Unavailable`]
/// when the player reports the video itself as not playable (with the server's
/// reason); and [`Error::NoAudioFormat`] when the response offers no directly
/// downloadable audio format, in which case the fetch surfaces that error
/// directly (there is no fallback backend).
pub async fn resolve(video_id: &str) -> Result<AudioStream> {
    let http = shared_http_client()?;
    resolve_with_chain(&http, video_id, CHAIN).await
}

/// Resolve through an explicit client chain, returning the first success.
///
/// Split out from [`resolve`] so the fall-through rules can be tested with a
/// synthetic chain (the shipped chain has one entry, and a committed test may
/// not depend on whichever clients exist in the wild that day).
async fn resolve_with_chain(
    http: &reqwest::Client,
    video_id: &str,
    chain: &[ClientConfig],
) -> Result<AudioStream> {
    // One rotation budget for the whole resolve, whichever client triggers it.
    let mut rotated = false;
    let mut failure: Option<Error> = None;
    for client in chain {
        match resolve_with_client(http, video_id, client, &mut rotated).await {
            Ok(stream) => return Ok(stream),
            // A transport/bootstrap/size failure is about the endpoint or the
            // process, not this client: trying the next row would only repeat
            // it. Parse/API/unavailable/no-audio verdicts can be client-specific
            // (a video may be playable only through one client), so those fall
            // through to the next client.
            Err(error) if is_terminal(&error) => return Err(error),
            Err(error) => {
                debug!(client = client.name, %error, "client did not resolve the stream");
                if failure.is_none() {
                    failure = Some(error);
                }
            }
        }
    }
    // The chain is a non-empty constant, so this arm is only reachable through
    // a misconfigured empty slice (or a future caller); report it rather than
    // panicking inside a library.
    Err(failure.unwrap_or_else(|| Error::Unavailable {
        reason: "no player client to try".to_owned(),
    }))
}

/// Whether `error` describes the endpoint/process rather than this client.
fn is_terminal(error: &Error) -> bool {
    matches!(
        error,
        Error::Http(_) | Error::Bootstrap(_) | Error::ResponseTooLarge { .. }
    )
}

/// Try one client, applying the shared one-rotation visitor rule.
async fn resolve_with_client(
    http: &reqwest::Client,
    video_id: &str,
    client: &ClientConfig,
    rotated: &mut bool,
) -> Result<AudioStream> {
    loop {
        let visitor = identity::visitor_data(http, IDENTITY_USER_AGENT).await?;

        let response = match post_player(http, client, video_id, &visitor).await {
            Ok(response) => response,
            Err(Error::Api {
                status: 403,
                message: _,
            }) => {
                // Budget spent: report the give-up without evicting a visitor
                // this resolve never got to try (the chain's next row would
                // otherwise rotate a healthy id away).
                if *rotated {
                    return Err(Error::Api {
                        status: 403,
                        message: "forbidden after rotating visitor data".to_owned(),
                    });
                }
                identity::ban(&visitor);
                debug!(
                    client = client.name,
                    "player rejected the visitor data; rotating once"
                );
                *rotated = true;
                continue;
            }
            Err(error) => return Err(error),
        };

        // A LOGIN_REQUIRED verdict is usually the anonymous visitor being
        // flagged, not the video; rotate once before believing it. Any other
        // non-OK status is the video's (or this client's) verdict and is left
        // for the chain to interpret.
        if playability_status(&response) == Some("LOGIN_REQUIRED") && !*rotated {
            identity::ban(&visitor);
            debug!(
                client = client.name,
                "player demanded sign-in; rotating visitor data once"
            );
            *rotated = true;
            continue;
        }

        check_playability(&response)?;
        let stream = select_audio(&response, client.name)?;
        debug!(
            video_id,
            client = client.name,
            itag = stream.itag,
            content_length = stream.content_length,
            "resolved native audio stream"
        );
        return Ok(stream);
    }
}

/// The player base URL in use: [`PLAYER_BASE`] unless a test installed a
/// loopback override.
fn player_base() -> String {
    #[cfg(test)]
    if let Some(base) = TEST_PLAYER_BASE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
    {
        return base;
    }
    PLAYER_BASE.to_owned()
}

/// Test-only loopback override for [`player_base`]; see its docs.
#[cfg(test)]
static TEST_PLAYER_BASE: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// Test-only RAII handle: points [`player_base`] at `base` for as long as it
/// lives, so a failing assertion cannot strand the override for later tests.
///
/// The override is process-global, so only one test at a time may resolve
/// streams: every test in this module holds the visitor-pool gate, which is what
/// serialises them (the pool would need the same exclusivity anyway, since tests
/// seed it).
#[cfg(test)]
struct PlayerBaseOverride;

#[cfg(test)]
impl PlayerBaseOverride {
    fn at(base: &str) -> Self {
        *TEST_PLAYER_BASE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(base.to_owned());
        Self
    }
}

#[cfg(test)]
impl Drop for PlayerBaseOverride {
    fn drop(&mut self) {
        *TEST_PLAYER_BASE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    }
}

/// Build the resolver's HTTP client.
///
/// The visitor scrape sets its own user agent ([`IDENTITY_USER_AGENT`]) and the
/// player request sends the client's own one explicitly, so the client itself
/// sets no default. Timeouts mirror the metadata client: a bounded connect, and
/// a total ceiling that is generous for one small JSON response but finite, so a
/// half-open socket cannot wedge the resolver forever.
fn build_http_client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .gzip(true)
        .brotli(true)
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(30))
        .build()?)
}

/// The process-wide resolver client, built on first use.
///
/// [`resolve`] runs on the playback path and is called again on a re-resolve
/// seconds later; a fresh client each time would throw away the connection
/// pool and repeat the TCP+TLS handshake for every call. `reqwest::Client`
/// clones share their pool (it is an `Arc` internally), so one client serves all
/// calls. A failed build is not cached: the next call retries instead of pinning
/// a transient error forever.
fn shared_http_client() -> Result<reqwest::Client> {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    if let Some(client) = CLIENT.get() {
        return Ok(client.clone());
    }
    let client = build_http_client()?;
    Ok(CLIENT.get_or_init(|| client).clone())
}

/// The recorded player request body for `client` — the VISIONOS context from
/// `docs/native-streaming.md` §2.2, generalised over the table row.
///
/// `playbackContext`/`signatureTimestamp` is deliberately absent: the capture
/// proved the response is the same without it, and the clients that require it
/// are not usable clean-room anyway (§3.1).
fn player_request_body(client: &ClientConfig, video_id: &str) -> Value {
    let mut context_client = json!({
        "clientName": client.client_name,
        "clientVersion": client.client_version,
        "userAgent": client.user_agent,
        "hl": "en",
        "timeZone": "UTC",
        "utcOffsetMinutes": 0,
    });
    merge_object(&mut context_client, (client.extra_context_client)());
    json!({
        "context": { "client": context_client },
        "videoId": video_id,
        "contentCheckOk": true,
        "racyCheckOk": true,
    })
}

/// Merge one JSON object's keys into another.
///
/// Used for the client-specific context fields, which are applied *over* the
/// common ones so a builder cannot drop `clientName`/`clientVersion`. A builder
/// that returns a non-object adds nothing rather than corrupting the context.
fn merge_object(target: &mut Value, source: Value) {
    if let (Some(target), Some(source)) = (target.as_object_mut(), source.as_object()) {
        for (key, value) in source {
            target.insert(key.clone(), value.clone());
        }
    }
}

/// POST the player request once and return the parsed JSON response.
///
/// Error mapping mirrors `client::post` so the two transports behave the same
/// way for callers: a 403 is reported for the rotation retry to act on before
/// the body is read (a flagged visitor can get an HTML block page, and "the body
/// is not JSON" must not turn rotation into a parse error), a non-success status
/// keeps the status and the server's own words, and only a successful response
/// is parsed as JSON.
async fn post_player(
    http: &reqwest::Client,
    client: &ClientConfig,
    video_id: &str,
    visitor: &str,
) -> Result<Value> {
    let base = player_base();
    let resp = http
        .post(format!("{base}/youtubei/v1/player?prettyPrint=false"))
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::USER_AGENT, client.user_agent)
        .header("X-YouTube-Client-Name", client.client_name_id)
        .header("X-YouTube-Client-Version", client.client_version)
        .header("X-Goog-Visitor-Id", visitor)
        .header(header::ORIGIN, ORIGIN)
        .json(&player_request_body(client, video_id))
        .send()
        .await?;

    let status = resp.status();
    if status == reqwest::StatusCode::FORBIDDEN {
        return Err(Error::Api {
            status: 403,
            message: "player rejected the visitor data".to_owned(),
        });
    }

    // Reject an oversized *declared* body before reading anything; the received
    // bytes are counted while streaming too, because a chunked or decompressed
    // response declares no length at all.
    if let Some(declared) = resp.content_length() {
        if declared > MAX_PLAYER_BODY_BYTES {
            return Err(Error::ResponseTooLarge { size: declared });
        }
    }
    let raw = read_body(resp).await?;

    if !status.is_success() {
        let message = serde_json::from_slice::<Value>(&raw)
            .ok()
            .map(|parsed| api_message(&parsed))
            .filter(|message| !message.is_empty())
            .unwrap_or_else(|| body_excerpt(&raw));
        return Err(Error::Api {
            status: status.as_u16(),
            message,
        });
    }

    serde_json::from_slice(&raw)
        .map_err(|e| Error::Parse(format!("player response was not JSON: {e}")))
}

/// Read a response body, refusing to buffer more than [`MAX_PLAYER_BODY_BYTES`].
async fn read_body(mut resp: reqwest::Response) -> Result<Vec<u8>> {
    let mut body = Vec::new();
    while let Some(chunk) = resp.chunk().await? {
        let received = body.len() as u64 + chunk.len() as u64;
        if received > MAX_PLAYER_BODY_BYTES {
            return Err(Error::ResponseTooLarge { size: received });
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// The server-provided `error.message` from a parsed error body, if any.
fn api_message(parsed: &Value) -> String {
    parsed
        .get("error")
        .and_then(|error| error.get("message"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

/// A short, single-line excerpt of a non-JSON response body, for an error
/// message. Collapses whitespace and truncates on a character boundary.
fn body_excerpt(body: &[u8]) -> String {
    let lossy = String::from_utf8_lossy(body);
    let flat = lossy.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = flat.chars();
    let excerpt: String = chars.by_ref().take(MAX_ERROR_EXCERPT_CHARS).collect();
    if chars.next().is_some() {
        format!("{excerpt}…")
    } else {
        excerpt
    }
}

/// The `playabilityStatus.status` string, when the response carries one.
fn playability_status(response: &Value) -> Option<&str> {
    response
        .pointer("/playabilityStatus/status")
        .and_then(Value::as_str)
}

/// Reject a response the player marked as not playable.
///
/// A missing `playabilityStatus.status` is a parse failure rather than an
/// "unavailable" verdict: an unplayable response always carries a status, and
/// guessing would hide a changed response shape.
fn check_playability(response: &Value) -> Result<()> {
    let status = playability_status(response).ok_or_else(|| {
        Error::Parse("player response carried no playabilityStatus.status".to_owned())
    })?;
    if status == "OK" {
        return Ok(());
    }
    let reason = response
        .pointer("/playabilityStatus/reason")
        .and_then(Value::as_str)
        .filter(|reason| !reason.is_empty())
        .unwrap_or(status)
        // The reason is server text copied into a user-facing toast; cap it so
        // a hostile/broken response cannot balloon the UI payload.
        .chars()
        .take(300)
        .collect::<String>();
    debug!(status, %reason, "player reported an unplayable video");
    Err(Error::Unavailable { reason })
}

/// Pick the preferred audio stream out of one player response, tagging it with
/// the client that served it.
///
/// Tolerant by construction: unknown fields are ignored, unusable format entries
/// are skipped rather than failing the whole response, and only "there is no
/// directly downloadable audio at all" is an error.
fn select_audio(response: &Value, client: &'static str) -> Result<AudioStream> {
    let formats = audio_formats(response, client);

    // The known itags first, in the order the app's playback format has always
    // been requested (140 is the historical audio format). Then the container
    // and finally "any audio-only format" as a last resort.
    for itag in [140, 141, 139] {
        if let Some(stream) = formats.iter().find(|stream| stream.itag == itag) {
            return Ok(stream.clone());
        }
    }
    if let Some(stream) = formats
        .iter()
        .find(|stream| stream.mime_type.starts_with("audio/mp4"))
    {
        return Ok(stream.clone());
    }
    if let Some(stream) = formats.first() {
        return Ok(stream.clone());
    }
    debug!(
        client,
        "player response offered no directly downloadable audio format"
    );
    Err(Error::NoAudioFormat)
}

/// Every directly downloadable audio format in a player response, in response
/// order, tagged with the client that served it.
fn audio_formats(response: &Value, client: &'static str) -> Vec<AudioStream> {
    response
        .pointer("/streamingData/adaptiveFormats")
        .and_then(Value::as_array)
        .map(|formats| {
            formats
                .iter()
                .filter_map(|entry| audio_format(entry, client))
                .collect()
        })
        .unwrap_or_default()
}

/// Parse one `adaptiveFormats` entry into an [`AudioStream`], or `None` when it
/// is not a directly downloadable audio stream.
///
/// Skipped without error: entries carrying a `signatureCipher` (they need the
/// JavaScript signature solving this resolver deliberately avoids), entries with
/// no direct `url`, entries with no `mimeType` (nothing could classify them),
/// non-audio entries, and entries with no usable `itag` (there is no honest value
/// to report as theirs).
fn audio_format(entry: &Value, client: &'static str) -> Option<AudioStream> {
    if entry
        .get("signatureCipher")
        .is_some_and(|cipher| !cipher.is_null())
    {
        return None;
    }
    let mime_type = entry.get("mimeType").and_then(Value::as_str)?;
    if !mime_type.starts_with("audio/") {
        return None;
    }
    let url = entry
        .get("url")
        .and_then(Value::as_str)
        .filter(|url| !url.is_empty())?;
    let itag = entry
        .get("itag")
        .and_then(json_u64)
        .and_then(|itag| u32::try_from(itag).ok())?;
    Some(AudioStream {
        url: url.to_owned(),
        itag,
        mime_type: mime_type.to_owned(),
        content_length: entry.get("contentLength").and_then(json_u64),
        client,
    })
}

/// Read a non-negative integer that may arrive as a JSON number or a numeric
/// string.
///
/// The player response reports `contentLength` as a string today and has been
/// seen as a number elsewhere; both are the same fact.
fn json_u64(value: &Value) -> Option<u64> {
    match value {
        Value::Number(number) => number.as_u64(),
        Value::String(text) => text.trim().parse().ok(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    //! Resolver tests: pure parsing/selection rules, the recorded fixture, and
    //! the request path against a loopback HTTP server.
    //!
    //! Nothing here touches the network. The transport tests substitute
    //! [`PlayerBaseOverride::at`] for [`player_base`] and seed the visitor pool,
    //! so the only host contacted is a `std::net::TcpListener` on 127.0.0.1 —
    //! [`identity::visitor_data`] would otherwise scrape `context::home()`, which
    //! is exactly what the compile-time seam exists to keep these tests out of.

    use std::collections::HashMap;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::{Arc, Mutex, OnceLock};

    use serde_json::json;

    use super::*;

    /// The visitor the fake server rejects, so the rotation is visible.
    const REJECTED_VISITOR: &str = "visitor-that-gets-403";
    /// A second rejected visitor: the permanent-403 test needs the retried
    /// request to carry a *different* id, or the "one rotation" claim is weak.
    const ALSO_REJECTED_VISITOR: &str = "visitor-that-also-gets-403";
    /// A visitor the fake server accepts. Tests seed it and expect it in the
    /// request, so the pool's contents are never left to other tests' choices.
    const GOOD_VISITOR: &str = "visitor-the-server-accepts";

    /// Name tagged onto [`AudioStream::client`] by the pure selection tests.
    const TEST_CLIENT: &str = "test-client";

    /// A synthetic preferred client for the chain tests.
    ///
    /// The shipped [`CHAIN`] has one entry (see its docs); these rows exist so
    /// the fall-through rules — a client-specific failure moves to the next row,
    /// a transport failure ends the chain, the one rotation budget is shared —
    /// are pinned without shipping a client that cannot serve full downloads.
    const TEST_CLIENT_A: ClientConfig = ClientConfig {
        name: "TEST_A",
        client_name: "TEST_A",
        client_version: "1.0",
        client_name_id: "9901",
        user_agent: "rift-test-agent-a",
        extra_context_client: no_extra_client_fields,
    };

    /// The synthetic fallback client: what a working second row would look like.
    const TEST_CLIENT_B: ClientConfig = ClientConfig {
        name: "TEST_B",
        client_name: "TEST_B",
        client_version: "2.0",
        client_name_id: "9902",
        user_agent: "rift-test-agent-b",
        extra_context_client: no_extra_client_fields,
    };

    /// Context builder for the synthetic chain clients: no client-specific fields.
    fn no_extra_client_fields() -> Value {
        json!({})
    }

    /// `audio/mp4` MIME type, as the recorded response reports it.
    const AAC_MIME: &str = "audio/mp4; codecs=\"mp4a.40.2\"";
    /// `audio/webm` MIME type, the other audio container in the response.
    const OPUS_MIME: &str = "audio/webm; codecs=\"opus\"";

    /// A sanitized player response as the fake server (and pure tests) use it.
    fn player_ok_response() -> Value {
        json!({
            "playabilityStatus": { "status": "OK" },
            "streamingData": { "adaptiveFormats": [
                { "itag": 137, "mimeType": "video/mp4; codecs=\"avc1.640028\"",
                  "url": "https://example.invalid/videoplayback?itag=137",
                  "contentLength": "83977646" },
                { "itag": 251, "mimeType": OPUS_MIME,
                  "url": "https://example.invalid/videoplayback?itag=251",
                  "contentLength": "4917841" },
                { "itag": 141, "mimeType": AAC_MIME,
                  "url": "https://example.invalid/videoplayback?itag=141",
                  "contentLength": "6953417" },
                { "itag": 140, "mimeType": AAC_MIME,
                  "url": "https://example.invalid/videoplayback?itag=140",
                  "contentLength": "3911684" },
                { "itag": 139, "mimeType": "audio/mp4; codecs=\"mp4a.40.5\"",
                  "url": "https://example.invalid/videoplayback?itag=139",
                  "contentLength": 1474802_u64 },
            ]}
        })
    }

    /// A player response shaped like the one for an invalid video id: the exact
    /// status/reason pair the live sample recorded.
    fn player_unavailable_response() -> Value {
        json!({
            "playabilityStatus": {
                "status": "ERROR",
                "reason": "This video is unavailable",
            },
        })
    }

    /// The bot-check rejection: HTTP 200, `LOGIN_REQUIRED`.
    fn player_login_required_response() -> Value {
        json!({
            "playabilityStatus": {
                "status": "LOGIN_REQUIRED",
                "reason": "Sign in to confirm you're not a bot",
            },
        })
    }

    /// A playable response with no directly downloadable audio: video formats
    /// only, plus an audio entry that would need JavaScript signature solving.
    ///
    /// This is what a client whose formats have gone JS-gated looks like (the
    /// 2026-10-26 probe recorded exactly this shape for `TVHTML5_SIMPLY` and
    /// `MWEB`), so the chain must fall through to the next client rather than
    /// fail the whole resolve.
    fn player_video_only_response() -> Value {
        json!({
            "playabilityStatus": { "status": "OK" },
            "streamingData": { "adaptiveFormats": [
                { "itag": 137, "mimeType": "video/mp4; codecs=\"avc1.640028\"",
                  "url": "https://example.invalid/videoplayback?itag=137" },
                { "itag": 140, "mimeType": AAC_MIME,
                  "signatureCipher": "s=abc&sp=sig&url=https%3A%2F%2Fexample.invalid%2F140" },
            ]}
        })
    }

    /// One request the fake server received.
    #[derive(Clone)]
    struct Request {
        method: String,
        path: String,
        headers: HashMap<String, String>,
        body: Value,
    }

    impl Request {
        fn header(&self, name: &str) -> &str {
            self.headers.get(name).map_or("", String::as_str)
        }
        fn client_name_id(&self) -> &str {
            self.header("x-youtube-client-name")
        }
        fn video_id(&self) -> &str {
            self.body
                .get("videoId")
                .and_then(Value::as_str)
                .unwrap_or("")
        }
    }

    /// One canned response, with its status and raw body.
    struct Response {
        status: u16,
        body: String,
    }

    impl Response {
        fn json(status: u16, body: Value) -> Self {
            Self {
                status,
                body: body.to_string(),
            }
        }
    }

    /// The loopback player server: one per test binary, routes keyed by video id.
    struct FakePlayer {
        addr: String,
        requests: Arc<Mutex<Vec<Request>>>,
    }

    /// Bind a loopback server and hand back its address plus the request log.
    fn spawn() -> FakePlayer {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = format!("http://{}", listener.local_addr().expect("local addr"));
        let requests: Arc<Mutex<Vec<Request>>> = Arc::new(Mutex::new(Vec::new()));

        let log = requests.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let log = log.clone();
                // One connection per thread: a panicking route must not take
                // the accept loop (and every later test) down with it.
                let _ = std::thread::spawn(move || serve(stream, &log));
            }
        });

        FakePlayer { addr, requests }
    }

    /// The server every transport test in this process talks to.
    fn server() -> &'static FakePlayer {
        static SERVER: OnceLock<FakePlayer> = OnceLock::new();
        SERVER.get_or_init(spawn)
    }

    /// Read the requests seen so far, surviving a poisoned log.
    fn logged(requests: &Mutex<Vec<Request>>) -> Vec<Request> {
        requests
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .map(|request| Request {
                method: request.method.clone(),
                path: request.path.clone(),
                headers: request.headers.clone(),
                body: request.body.clone(),
            })
            .collect()
    }

    /// Read one request, write one response. Always answers.
    fn serve(mut stream: TcpStream, log: &Mutex<Vec<Request>>) -> std::io::Result<()> {
        let mut reader = BufReader::new(stream.try_clone()?);
        let mut request_line = String::new();
        reader.read_line(&mut request_line)?;
        let mut parts = request_line.split_whitespace();
        let method = parts.next().unwrap_or("").to_owned();
        let path = parts.next().unwrap_or("").to_owned();

        let mut headers = HashMap::new();
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line)? == 0 {
                break;
            }
            let line = line.trim_end();
            if line.is_empty() {
                break;
            }
            if let Some((name, value)) = line.split_once(':') {
                headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_owned());
            }
        }

        let mut raw = String::new();
        if let Some(len) = headers.get("content-length").and_then(|v| v.parse().ok()) {
            let mut buf = vec![0u8; len];
            reader.read_exact(&mut buf)?;
            raw = String::from_utf8_lossy(&buf).into_owned();
        }

        let request = Request {
            method,
            path,
            headers,
            body: serde_json::from_str(&raw).unwrap_or(Value::Null),
        };
        let response = route(&request);
        log.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(request);

        let reason = match response.status {
            200 => "OK",
            403 => "Forbidden",
            500 => "Internal Server Error",
            _ => "Error",
        };
        let head = format!(
            "HTTP/1.1 {} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
             Connection: close\r\n\r\n",
            response.status,
            response.body.len()
        );
        stream.write_all(head.as_bytes())?;
        stream.write_all(response.body.as_bytes())?;
        stream.flush()
    }

    /// The routes the transport tests exercise, selected by `videoId`.
    fn route(request: &Request) -> Response {
        let visitor = request.header("x-goog-visitor-id");
        match request.video_id() {
            "unavailable" => Response::json(200, player_unavailable_response()),
            // Rejected on the first visitor, fine on the rotated one.
            "login-once" => {
                if visitor == REJECTED_VISITOR {
                    Response::json(200, player_login_required_response())
                } else {
                    Response::json(200, player_ok_response())
                }
            }
            // Rejected for every visitor: the rotation is spent and the call
            // must end in `Unavailable`, not loop.
            "login-always" => Response::json(200, player_login_required_response()),
            // A permanent HTTP refusal, for the give-up path.
            "forbidden" => Response::json(
                403,
                json!({ "error": { "message": "rejected by the test server" } }),
            ),
            // A non-success status with a non-JSON body: an HTTP error, not a
            // parse failure.
            "server-error" => Response {
                status: 500,
                body: "upstream failed: please retry later".to_owned(),
            },
            // Chain machinery: the preferred synthetic client answers with a
            // response that has no downloadable audio, so the chain must fall
            // through to the next row.
            "chain-no-audio" if request.client_name_id() == TEST_CLIENT_A.client_name_id => {
                Response::json(200, player_video_only_response())
            }
            // Chain machinery: a client-specific HTTP failure falls through too.
            "chain-api-error" if request.client_name_id() == TEST_CLIENT_A.client_name_id => {
                Response::json(400, json!({ "error": { "message": "client rejected" } }))
            }
            // Chain machinery: the preferred synthetic client is rejected for
            // the flagged visitor, so the shared rotation budget is spent there;
            // it then fails on the rotated visitor with a client-specific
            // verdict, and the fallback succeeds with that same rotated visitor.
            "chain-rotation"
                if request.client_name_id() == TEST_CLIENT_A.client_name_id
                    && visitor == REJECTED_VISITOR =>
            {
                Response::json(
                    403,
                    json!({ "error": { "message": "rejected by the test server" } }),
                )
            }
            "chain-rotation" if request.client_name_id() == TEST_CLIENT_A.client_name_id => {
                Response::json(200, player_video_only_response())
            }
            // Chain machinery: every client says the video is unavailable, so
            // the first (preferred) client's verdict is what the caller gets.
            "chain-unavailable" => Response::json(200, player_unavailable_response()),
            // Everything else behaves like a healthy player, but rejects the
            // flagged visitor so the rotation path is visible on the default
            // route too.
            _ if visitor == REJECTED_VISITOR => Response::json(
                403,
                json!({ "error": { "message": "rejected by the test server" } }),
            ),
            _ => Response::json(200, player_ok_response()),
        }
    }

    /// Point [`player_base`] at the fake server for the length of a test, after
    /// seeding `visitors` as the whole pool.
    ///
    /// Keeps the process-global test server override alive for a test, plus the
    /// visitor-pool gate that serialises those tests.
    ///
    /// Struct fields are dropped in declaration order: the base override must be
    /// cleared *before* the gate is released, otherwise the next test can start
    /// with a stale override pointing at this test's loopback server (or clear
    /// it to the real site). A tuple return would drop in the opposite order.
    struct Arrangement {
        /// Held only for its `Drop` (clearing the process-global base override).
        #[allow(dead_code)]
        base: PlayerBaseOverride,
        /// Held only for its `Drop` (serialising the process-global overrides);
        /// never read, hence the allow.
        #[allow(dead_code)]
        gate: tokio::sync::MutexGuard<'static, ()>,
    }

    async fn arrange(visitors: &[&str]) -> Arrangement {
        let gate = identity::pool_gate().lock().await;
        identity::seed_pool_for_test(visitors);
        let base = PlayerBaseOverride::at(&server().addr);
        Arrangement { base, gate }
    }

    /// The visitors the fake server saw on requests for `video_id`, in order.
    fn visitors_for(video_id: &str) -> Vec<String> {
        logged(&server().requests)
            .iter()
            .filter(|request| request.video_id() == video_id)
            .map(|request| request.header("x-goog-visitor-id").to_owned())
            .collect()
    }

    /// The `X-YouTube-Client-Name` ids the fake server saw for `video_id`, in
    /// order: the chain-order evidence.
    fn clients_for(video_id: &str) -> Vec<String> {
        logged(&server().requests)
            .iter()
            .filter(|request| request.video_id() == video_id)
            .map(|request| request.client_name_id().to_owned())
            .collect()
    }

    /// Resolve through an explicit client chain against the fake server.
    ///
    /// The shipped entry point goes through [`CHAIN`] (one row today); the chain
    /// tests inject their own rows so the fall-through rules are pinned without
    /// depending on which clients happen to work in the wild.
    async fn resolve_chain(video_id: &str, chain: &[ClientConfig]) -> Result<AudioStream> {
        let http = build_http_client().expect("http client");
        resolve_with_chain(&http, video_id, chain).await
    }

    // -----------------------------------------------------------------------
    // Transport: the request we build, and the retry paths
    // -----------------------------------------------------------------------

    /// The whole shipped request, asserted against the recorded contract: path,
    /// method, headers, VISIONOS context, and the deliberate absence of
    /// `playbackContext`.
    #[tokio::test]
    async fn the_request_is_the_validated_visionos_player_request() {
        let _arrangement = arrange(&[GOOD_VISITOR]).await;

        let stream = resolve("request-shape").await.expect("resolve");

        assert_eq!(stream.itag, 140, "the recorded response's preferred format");
        assert_eq!(stream.mime_type, AAC_MIME);
        assert_eq!(stream.content_length, Some(3_911_684));
        assert_eq!(stream.url, "https://example.invalid/videoplayback?itag=140");
        assert_eq!(
            stream.client, VISIONOS.name,
            "the stream names the client that served it"
        );

        let requests = logged(&server().requests);
        let sent: Vec<&Request> = requests
            .iter()
            .filter(|request| request.video_id() == "request-shape")
            .collect();
        assert_eq!(sent.len(), 1, "one attempt for a healthy response");
        let request = sent[0];

        assert_eq!(request.method, "POST");
        assert_eq!(request.path, "/youtubei/v1/player?prettyPrint=false");
        assert_eq!(request.header("content-type"), "application/json");
        assert_eq!(request.header("x-youtube-client-name"), "101");
        assert_eq!(request.header("x-youtube-client-version"), "1.02");
        assert_eq!(request.header("x-goog-visitor-id"), GOOD_VISITOR);
        assert_eq!(request.header("origin"), "https://www.youtube.com");
        assert_eq!(
            request.header("user-agent"),
            VISIONOS_USER_AGENT,
            "the VISIONOS Safari user agent identifies the client"
        );

        let client = request
            .body
            .pointer("/context/client")
            .expect("context.client");
        assert_eq!(client["clientName"], "VISIONOS");
        assert_eq!(client["clientVersion"], "1.02");
        assert_eq!(client["deviceMake"], "Apple");
        assert_eq!(client["deviceModel"], "RealityDevice17,1");
        assert_eq!(client["userAgent"], VISIONOS_USER_AGENT);
        assert_eq!(client["osName"], "visionOS");
        assert_eq!(client["osVersion"], "26.5.23O471");
        assert_eq!(client["hl"], "en");
        assert_eq!(client["timeZone"], "UTC");
        assert_eq!(client["utcOffsetMinutes"], 0);

        assert_eq!(request.body["videoId"], "request-shape");
        assert_eq!(request.body["contentCheckOk"], true);
        assert_eq!(request.body["racyCheckOk"], true);
        assert!(
            request.body.get("playbackContext").is_none(),
            "playbackContext/signatureTimestamp was verified unnecessary"
        );
        assert!(!request.body.to_string().contains("signatureTimestamp"));
    }

    /// A rejected visitor is evicted and the retried request carries the id the
    /// pool then hands out.
    #[tokio::test]
    async fn a_403_rotates_the_visitor_and_retries_once() {
        let _arrangement = arrange(&[REJECTED_VISITOR, GOOD_VISITOR]).await;

        let stream = resolve("rotation-ok")
            .await
            .expect("the rotated visitor must succeed");

        assert_eq!(stream.itag, 140);
        assert_eq!(
            visitors_for("rotation-ok"),
            [REJECTED_VISITOR, GOOD_VISITOR],
            "the seeded id, then the pooled id after the ban"
        );
    }

    /// A 403 on every visitor is terminal after exactly one rotation.
    #[tokio::test]
    async fn a_403_on_every_attempt_reports_forbidden() {
        let _arrangement = arrange(&[REJECTED_VISITOR, ALSO_REJECTED_VISITOR]).await;

        let error = resolve("forbidden")
            .await
            .expect_err("a permanent 403 must fail");
        match error {
            Error::Api { status, message } => {
                assert_eq!(status, 403);
                assert!(
                    message.contains("rotating"),
                    "the give-up path should say the rotation was exhausted: {message}"
                );
            }
            other => panic!("expected Error::Api, got {other:?}"),
        }

        assert_eq!(
            visitors_for("forbidden"),
            [REJECTED_VISITOR, ALSO_REJECTED_VISITOR],
            "exactly one extra attempt, with a genuinely different visitor"
        );
    }

    /// `LOGIN_REQUIRED` (the anonymous-visitor bot check) rotates once and
    /// succeeds on the fresh visitor.
    #[tokio::test]
    async fn a_login_required_response_rotates_the_visitor_and_retries_once() {
        let _arrangement = arrange(&[REJECTED_VISITOR, GOOD_VISITOR]).await;

        let stream = resolve("login-once")
            .await
            .expect("the rotated visitor must succeed");

        assert_eq!(stream.itag, 140);
        assert_eq!(visitors_for("login-once"), [REJECTED_VISITOR, GOOD_VISITOR]);
    }

    /// A `LOGIN_REQUIRED` that persists is the caller's answer, not a loop: the
    /// rotation is spent and the server's reason is reported.
    #[tokio::test]
    async fn a_login_required_that_persists_is_unavailable() {
        let _arrangement = arrange(&[REJECTED_VISITOR, GOOD_VISITOR]).await;

        let error = resolve("login-always")
            .await
            .expect_err("a persistent LOGIN_REQUIRED must fail");
        match error {
            Error::Unavailable { reason } => {
                assert_eq!(reason, "Sign in to confirm you're not a bot")
            }
            other => panic!("expected Error::Unavailable, got {other:?}"),
        }

        assert_eq!(
            visitors_for("login-always"),
            [REJECTED_VISITOR, GOOD_VISITOR],
            "one rotation is the whole budget"
        );
    }

    /// An unplayable video keeps the player's own reason and is not retried.
    #[tokio::test]
    async fn an_unplayable_video_reports_the_player_reason() {
        let _arrangement = arrange(&[GOOD_VISITOR]).await;

        let error = resolve("unavailable")
            .await
            .expect_err("an ERROR status must fail");
        match error {
            Error::Unavailable { reason } => {
                assert_eq!(
                    reason, "This video is unavailable",
                    "the exact reason string from the live sample"
                );
            }
            other => panic!("expected Error::Unavailable, got {other:?}"),
        }

        assert_eq!(
            visitors_for("unavailable").len(),
            1,
            "an unavailable video is not a visitor problem, so no rotation"
        );
    }

    /// A non-success status with a non-JSON body is an HTTP-level refusal; it
    /// keeps the status and the server's words, and is not retried.
    #[tokio::test]
    async fn a_non_json_error_body_is_reported_as_an_api_error() {
        let _arrangement = arrange(&[GOOD_VISITOR]).await;

        let error = resolve("server-error").await.expect_err("a 500 must fail");
        match error {
            Error::Api { status, message } => {
                assert_eq!(status, 500);
                assert!(
                    message.contains("upstream failed"),
                    "the body excerpt should reach the caller: {message:?}"
                );
            }
            other => panic!("expected Error::Api, got {other:?}"),
        }

        assert_eq!(
            visitors_for("server-error").len(),
            1,
            "only a 403 or LOGIN_REQUIRED is retried"
        );
    }

    // -----------------------------------------------------------------------
    // Chain: table order, fall-through, and the shared rotation budget
    // -----------------------------------------------------------------------

    /// The shipped chain is the verified single client.
    ///
    /// Only a client whose URLs have been shown to serve *full* ranged downloads
    /// belongs in [`CHAIN`]; the 2026-10-26 probe (§3.1) found exactly one. This
    /// test is the deliberate speed bump for adding another: it fails until the
    /// table's request contract is asserted here too.
    #[test]
    fn the_shipped_chain_is_the_verified_visionos_client() {
        assert_eq!(CHAIN.len(), 1, "only the verified client is shipped");
        let client = &CHAIN[0];
        assert_eq!(client.name, "VISIONOS");
        assert_eq!(client.client_name, "VISIONOS");
        assert_eq!(client.client_version, "1.02");
        assert_eq!(client.client_name_id, "101");
        assert_eq!(client.user_agent, VISIONOS_USER_AGENT);
        assert_eq!(
            player_request_body(client, "K0HSD_i2DvA"),
            json!({
                "context": { "client": {
                    "clientName": "VISIONOS",
                    "clientVersion": "1.02",
                    "userAgent": VISIONOS_USER_AGENT,
                    "hl": "en",
                    "timeZone": "UTC",
                    "utcOffsetMinutes": 0,
                    "deviceMake": "Apple",
                    "deviceModel": "RealityDevice17,1",
                    "osName": "visionOS",
                    "osVersion": "26.5.23O471",
                }},
                "videoId": "K0HSD_i2DvA",
                "contentCheckOk": true,
                "racyCheckOk": true,
            }),
            "the shipped row must reproduce the recorded request from §2.2 exactly"
        );
    }

    /// When the preferred client cannot produce a downloadable format, the next
    /// row is tried — that is the whole point of the table.
    #[tokio::test]
    async fn the_chain_falls_through_when_the_preferred_client_offers_no_audio() {
        let _arrangement = arrange(&[GOOD_VISITOR]).await;

        let stream = resolve_chain("chain-no-audio", &[TEST_CLIENT_A, TEST_CLIENT_B])
            .await
            .expect("the fallback must serve the stream");

        assert_eq!(stream.client, TEST_CLIENT_B.name);
        assert_eq!(stream.itag, 140);
        assert_eq!(
            clients_for("chain-no-audio"),
            [TEST_CLIENT_A.client_name_id, TEST_CLIENT_B.client_name_id],
            "the preferred client, then the fallback"
        );

        // The fallback request is built from *its* row, not from the preferred
        // one: per-client headers and context are table data.
        let requests = logged(&server().requests);
        let fallback = requests
            .iter()
            .find(|request| {
                request.video_id() == "chain-no-audio"
                    && request.client_name_id() == TEST_CLIENT_B.client_name_id
            })
            .expect("the fallback request");
        assert_eq!(
            fallback.header("x-youtube-client-version"),
            TEST_CLIENT_B.client_version
        );
        assert_eq!(fallback.header("user-agent"), TEST_CLIENT_B.user_agent);
        assert_eq!(
            fallback
                .body
                .pointer("/context/client/clientName")
                .and_then(Value::as_str),
            Some(TEST_CLIENT_B.client_name)
        );
        assert_eq!(
            fallback
                .body
                .pointer("/context/client/clientVersion")
                .and_then(Value::as_str),
            Some(TEST_CLIENT_B.client_version)
        );
    }

    /// A client-specific HTTP rejection falls through to the next row too.
    #[tokio::test]
    async fn the_chain_falls_through_on_a_client_specific_api_error() {
        let _arrangement = arrange(&[GOOD_VISITOR]).await;

        let stream = resolve_chain("chain-api-error", &[TEST_CLIENT_A, TEST_CLIENT_B])
            .await
            .expect("the fallback must serve the stream");

        assert_eq!(stream.client, TEST_CLIENT_B.name);
        assert_eq!(
            clients_for("chain-api-error"),
            [TEST_CLIENT_A.client_name_id, TEST_CLIENT_B.client_name_id]
        );
    }

    /// The one-rotation budget is shared by the whole chain, so a run of client
    /// rejections cannot turn into a visitor-rotation loop.
    #[tokio::test]
    async fn the_chain_spends_one_visitor_rotation_then_uses_the_next_client() {
        let _arrangement = arrange(&[REJECTED_VISITOR, GOOD_VISITOR]).await;

        let stream = resolve_chain("chain-rotation", &[TEST_CLIENT_A, TEST_CLIENT_B])
            .await
            .expect("the fallback must serve the stream after the rotation");

        assert_eq!(stream.client, TEST_CLIENT_B.name);
        assert_eq!(
            visitors_for("chain-rotation"),
            [REJECTED_VISITOR, GOOD_VISITOR, GOOD_VISITOR],
            "the preferred client rotated once and was still rejected; the \
             fallback then used the rotated visitor"
        );
        assert_eq!(
            clients_for("chain-rotation"),
            [
                TEST_CLIENT_A.client_name_id,
                TEST_CLIENT_A.client_name_id,
                TEST_CLIENT_B.client_name_id,
            ],
            "one retry of the preferred client, then the fallback — no second rotation"
        );
    }

    /// When every row fails, the call reports the *preferred* client's verdict,
    /// which is the most representative failure for the caller.
    #[tokio::test]
    async fn the_chain_reports_the_preferred_clients_error_when_every_row_fails() {
        let _arrangement = arrange(&[GOOD_VISITOR]).await;

        let error = resolve_chain("chain-unavailable", &[TEST_CLIENT_A, TEST_CLIENT_B])
            .await
            .expect_err("every client says the video is unavailable");
        match error {
            Error::Unavailable { reason } => assert_eq!(reason, "This video is unavailable"),
            other => panic!("expected Error::Unavailable, got {other:?}"),
        }
        assert_eq!(
            clients_for("chain-unavailable").len(),
            2,
            "both rows were tried before giving up"
        );
    }

    /// Transport-level failures are about the endpoint, not a client, so they end
    /// the chain instead of retrying the same network with the next row.
    #[test]
    fn a_transport_level_failure_is_terminal_for_the_chain() {
        assert!(is_terminal(&Error::Bootstrap("no visitor".to_owned())));
        assert!(is_terminal(&Error::ResponseTooLarge { size: 1 }));
        assert!(!is_terminal(&Error::NoAudioFormat));
        assert!(!is_terminal(&Error::Parse("body".to_owned())));
        assert!(!is_terminal(&Error::Unavailable {
            reason: "This video is unavailable".to_owned()
        }));
        assert!(!is_terminal(&Error::Api {
            status: 400,
            message: "client rejected".to_owned()
        }));
    }

    // -----------------------------------------------------------------------
    // Pure parsing and selection rules
    // -----------------------------------------------------------------------

    /// `select_audio` with the test client name: the pure selection rules do not
    /// care which client served the response, and the transport/chain tests
    /// assert the tag.
    fn selected(response: &Value) -> Result<AudioStream> {
        select_audio(response, TEST_CLIENT)
    }

    /// Wrap `formats` in a minimal playable player response.
    fn response_with(formats: &[Value]) -> Value {
        json!({
            "playabilityStatus": { "status": "OK" },
            "streamingData": { "adaptiveFormats": formats },
        })
    }

    /// One audio format entry for the selection tests.
    fn audio(itag: u64, mime_type: &str) -> Value {
        json!({
            "itag": itag,
            "mimeType": mime_type,
            "url": format!("https://example.invalid/videoplayback?itag={itag}"),
            "contentLength": "1234",
        })
    }

    #[test]
    fn itag_140_wins_whatever_the_response_order() {
        // The recorded response lists the audio formats in a different order; the
        // rule must not depend on it.
        let response = response_with(&[
            audio(251, OPUS_MIME),
            audio(141, AAC_MIME),
            audio(139, "audio/mp4; codecs=\"mp4a.40.5\""),
            audio(140, AAC_MIME),
        ]);
        assert_eq!(selected(&response).expect("audio").itag, 140);
    }

    #[test]
    fn itag_141_is_the_second_choice_and_139_the_third() {
        let response = response_with(&[audio(251, OPUS_MIME), audio(141, AAC_MIME)]);
        assert_eq!(selected(&response).expect("audio").itag, 141);

        let response = response_with(&[audio(251, OPUS_MIME), audio(139, AAC_MIME)]);
        assert_eq!(selected(&response).expect("audio").itag, 139);
    }

    #[test]
    fn any_audio_mp4_is_preferred_over_other_audio_containers() {
        // An unknown/renumbered itag in the right container still beats a known
        // Opus format, because the app's decoder path is m4a.
        let response = response_with(&[audio(251, OPUS_MIME), audio(999, AAC_MIME)]);
        assert_eq!(selected(&response).expect("audio").itag, 999);
    }

    #[test]
    fn any_audio_only_format_is_the_last_resort() {
        let response = response_with(&[
            json!({ "itag": 137, "mimeType": "video/mp4", "url": "https://example.invalid/137" }),
            audio(251, OPUS_MIME),
        ]);
        assert_eq!(selected(&response).expect("audio").itag, 251);
    }

    #[test]
    fn ciphered_or_urlless_formats_are_skipped() {
        let response = response_with(&[
            // Needs JavaScript signature solving: unusable here, however much
            // the itag is preferred.
            json!({
                "itag": 140,
                "mimeType": AAC_MIME,
                "signatureCipher": "s=abc&sp=sig&url=https%3A%2F%2Fexample.invalid%2F140",
            }),
            // No direct URL at all.
            json!({ "itag": 141, "mimeType": AAC_MIME }),
            audio(139, AAC_MIME),
        ]);
        assert_eq!(
            selected(&response).expect("audio").itag,
            139,
            "the first usable format, skipping the ciphered and urlless ones"
        );
    }

    #[test]
    fn no_downloadable_audio_is_a_distinct_error() {
        // Only video formats: a caller must fall back rather than stream video.
        let response = response_with(&[json!({
            "itag": 137,
            "mimeType": "video/mp4",
            "url": "https://example.invalid/137",
        })]);
        assert!(matches!(selected(&response), Err(Error::NoAudioFormat)));

        // No streaming data at all (e.g. a playable-but-audio-less response).
        assert!(matches!(
            selected(&json!({ "playabilityStatus": { "status": "OK" } })),
            Err(Error::NoAudioFormat)
        ));
    }

    #[test]
    fn content_length_accepts_a_number_or_a_numeric_string() {
        let mut numeric_string = audio(140, AAC_MIME);
        numeric_string["contentLength"] = json!("3911684");
        assert_eq!(
            selected(&response_with(&[numeric_string]))
                .expect("audio")
                .content_length,
            Some(3_911_684)
        );

        let mut number = audio(140, AAC_MIME);
        number["contentLength"] = json!(3_911_684_u64);
        assert_eq!(
            selected(&response_with(&[number]))
                .expect("audio")
                .content_length,
            Some(3_911_684)
        );

        // Absent or unparseable: unknown, not a failure.
        let mut absent = audio(140, AAC_MIME);
        absent
            .as_object_mut()
            .expect("object")
            .remove("contentLength");
        assert_eq!(
            selected(&response_with(&[absent]))
                .expect("audio")
                .content_length,
            None
        );
        let mut garbled = audio(140, AAC_MIME);
        garbled["contentLength"] = json!("not a number");
        assert_eq!(
            selected(&response_with(&[garbled]))
                .expect("audio")
                .content_length,
            None
        );
    }

    #[test]
    fn a_format_entry_without_a_usable_itag_is_skipped() {
        let response = response_with(&[
            json!({
                "itag": "not-a-number",
                "mimeType": AAC_MIME,
                "url": "https://example.invalid/broken",
            }),
            audio(139, AAC_MIME),
        ]);
        assert_eq!(selected(&response).expect("audio").itag, 139);

        // A numeric string is a usable itag (the same tolerance as contentLength).
        let mut numeric_string_itag = audio(141, AAC_MIME);
        numeric_string_itag["itag"] = json!("141");
        assert_eq!(
            selected(&response_with(&[numeric_string_itag]))
                .expect("audio")
                .itag,
            141
        );
    }

    #[test]
    fn non_ok_playability_is_unavailable_with_the_player_reason() {
        let error =
            check_playability(&player_unavailable_response()).expect_err("ERROR is not playable");
        match error {
            Error::Unavailable { reason } => assert_eq!(reason, "This video is unavailable"),
            other => panic!("expected Error::Unavailable, got {other:?}"),
        }
        assert_eq!(
            playability_status(&player_ok_response()),
            Some("OK"),
            "the control response is playable"
        );
    }

    #[test]
    fn a_missing_playability_status_is_a_parse_error() {
        // A changed response shape must be reported as such, not guessed at: an
        // empty `{}` is not evidence that the video is unavailable.
        assert!(matches!(
            check_playability(&json!({})),
            Err(Error::Parse(_))
        ));
    }

    // -----------------------------------------------------------------------
    // The recorded fixture
    // -----------------------------------------------------------------------

    /// Read a committed fixture. The fixture is a real VISIONOS player response
    /// compacted down to the keys the resolver reads, with every signed URL
    /// replaced by an inert `example.invalid` placeholder.
    fn fixture(name: &str) -> String {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name);
        std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("could not read {}: {e}", path.display()))
    }

    #[test]
    fn the_recorded_visionos_response_resolves_to_itag_140() {
        let text = fixture("player_visionos.json");
        let response: Value =
            serde_json::from_str(&text).unwrap_or_else(|e| panic!("fixture is not JSON: {e}"));

        let stream = selected(&response).expect("the recorded response must resolve");
        assert_eq!(stream.itag, 140, "the format the pre-native app streamed");
        assert_eq!(stream.mime_type, "audio/mp4; codecs=\"mp4a.40.2\"");
        assert_eq!(
            stream.content_length,
            Some(3_911_684),
            "contentLength is a numeric string in the real response"
        );
        // The URL in the fixture is the sanitized placeholder the sanitizer
        // wrote, not a live signed URL.
        assert_eq!(stream.url, "https://example.invalid/videoplayback?itag=140");
    }

    /// The fixture is committed to the repository, so it must stay clean: a
    /// signed `googlevideo` URL or a visitor id in it would be a credential leak
    /// as well as an unstable test asset.
    #[test]
    fn the_recorded_fixture_contains_no_signed_urls_or_visitor_data() {
        let text = fixture("player_visionos.json");
        for needle in [
            "googlevideo",
            "signatureCipher",
            "visitorData",
            "X-Goog-Visitor-Id",
        ] {
            assert!(
                !text.contains(needle),
                "the committed fixture must not contain {needle:?}"
            );
        }
        // Every format URL the resolver could return is the inert placeholder.
        let response: Value = serde_json::from_str(&text).expect("fixture is JSON");
        let urls = audio_formats(&response, TEST_CLIENT);
        assert!(
            !urls.is_empty(),
            "the fixture must actually exercise audio selection"
        );
        assert!(
            urls.iter()
                .all(|stream| stream.url.starts_with("https://example.invalid/")),
            "every fixture URL must be sanitized"
        );
    }
}
