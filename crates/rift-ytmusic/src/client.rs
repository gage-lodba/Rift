//! High-level client: request transport + typed metadata endpoints.

use std::sync::Arc;

use reqwest::header;
use rift_types::{AlbumPage, AlbumSummary, ArtistPage, ArtistRef, ArtistSummary, Playlist, Track};
use serde_json::{json, Value};

use crate::context::{self, CLIENT_NAME_ID};
use crate::error::{Error, Result};
use crate::identity;
use crate::params::SearchFilter;
use crate::parse;

const USER_AGENT: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:128.0) Gecko/20100101 Firefox/128.0";

/// The largest innertube response body the client will buffer.
///
/// Real metadata pages are a few megabytes at most, so this is a deliberately
/// generous ceiling; it exists because [`reqwest::Response`] has no ceiling of
/// its own, and a broken (or hostile) endpoint could otherwise grow a buffer for
/// as long as it keeps sending. Innertube JSON is read whole by design — the
/// parsers walk a complete [`serde_json::Value`] — so "do not buffer it at all"
/// is not an option here; bounding the buffer is.
///
/// Tests shrink the constant: the streaming bound can only be exercised
/// end-to-end if a response is allowed to exceed it, and moving tens of
/// megabytes per test through loopback would cost the suite more than the
/// bound is worth. The shipped value is 32 MiB; only the constant is
/// substituted under `cfg(test)`.
#[cfg(not(test))]
const MAX_RESPONSE_BYTES: u64 = 32 * 1024 * 1024;
#[cfg(test)]
const MAX_RESPONSE_BYTES: u64 = 16 * 1024;

/// Longest excerpt of a non-JSON error body copied into an error message.
const MAX_ERROR_EXCERPT_CHARS: usize = 200;

/// Read a response body, refusing to buffer more than [`MAX_RESPONSE_BYTES`].
///
/// The ceiling is enforced by counting the bytes actually received, not by
/// trusting `Content-Length`: a chunked or automatically decompressed response
/// has no declared length (`Response::content_length` returns `None` for it),
/// and the request timeout bounds how *long* a slow server may trickle, not how
/// much it can make the client buffer. Callers still reject an oversized
/// declared length before calling this, so that case never starts reading.
async fn read_body(mut resp: reqwest::Response) -> Result<Vec<u8>> {
    let mut body = Vec::new();
    while let Some(chunk) = resp.chunk().await? {
        let received = body.len() as u64 + chunk.len() as u64;
        if received > MAX_RESPONSE_BYTES {
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
/// message. Collapses whitespace so an HTML error page cannot put newlines in a
/// toast, and truncates on a character boundary.
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

/// A `YouTube` Music innertube client.
///
/// Cheap to clone: clones share the HTTP connection pool. The scraped client
/// version and the visitor-data pool are process-wide caches in the crate's
/// private `context` and `identity` modules rather than per-client state, so every
/// clone and every separate `Client` in the process sees the same session.
#[derive(Clone)]
pub struct Client {
    inner: Arc<Inner>,
}

struct Inner {
    http: reqwest::Client,
}

impl Client {
    /// Build an anonymous client. Does no network I/O; the session bootstraps
    /// lazily on the first request.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Http`] only if the underlying [`reqwest::Client`] cannot
    /// be constructed (for example a TLS backend that fails to initialize). No
    /// request is made here, so there is no network error to report yet.
    pub fn new() -> Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .gzip(true)
            .brotli(true)
            // reqwest's default is no timeout at all, which turns a half-open
            // socket into a permanently hung metadata command (the UI's only
            // affordance is a spinner with no cancel). Innertube responses are
            // small, so a short ceiling costs nothing legitimate.
            .connect_timeout(std::time::Duration::from_secs(5))
            .timeout(std::time::Duration::from_secs(30))
            .build()?;
        Ok(Self {
            inner: Arc::new(Inner { http }),
        })
    }

    /// Search `YouTube` Music for audio tracks.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Http`] on a transport failure (DNS, TLS, timeout,
    /// reset), [`Error::Api`] when innertube answers with a non-success status,
    /// [`Error::Bootstrap`] when no anonymous visitor identity can be
    /// established, [`Error::ResponseTooLarge`] for an oversized response, and
    /// [`Error::Parse`] when the body is not JSON.
    pub async fn search_tracks(&self, query: &str) -> Result<Vec<Track>> {
        let resp = self.search_raw(SearchFilter::Tracks, query).await?;
        Ok(parse::tracks(&resp))
    }

    /// Search `YouTube` Music for artists.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Http`] on a transport failure, [`Error::Api`] when
    /// innertube answers with a non-success status, [`Error::Bootstrap`] when no
    /// anonymous visitor identity can be established, [`Error::ResponseTooLarge`]
    /// for an oversized response, and [`Error::Parse`] when the body is not JSON.
    pub async fn search_artists(&self, query: &str) -> Result<Vec<ArtistSummary>> {
        let resp = self.search_raw(SearchFilter::Artists, query).await?;
        Ok(parse::artists(&resp))
    }

    /// Search `YouTube` Music for albums.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Http`] on a transport failure, [`Error::Api`] when
    /// innertube answers with a non-success status, [`Error::Bootstrap`] when no
    /// anonymous visitor identity can be established, [`Error::ResponseTooLarge`]
    /// for an oversized response, and [`Error::Parse`] when the body is not JSON.
    pub async fn search_albums(&self, query: &str) -> Result<Vec<AlbumSummary>> {
        let resp = self.search_raw(SearchFilter::Albums, query).await?;
        Ok(parse::albums(&resp))
    }

    /// Fetch a full album page (header + track list) by its browse id.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Http`] on a transport failure, [`Error::Api`] when
    /// innertube answers with a non-success status, [`Error::Bootstrap`] when no
    /// anonymous visitor identity can be established, [`Error::ResponseTooLarge`]
    /// for an oversized response, and [`Error::Parse`] when the body is not JSON
    /// or the page shape is not understood.
    pub async fn album(&self, browse_id: &str) -> Result<AlbumPage> {
        let resp = self.browse_raw(browse_id).await?;
        parse::album_page(&resp, browse_id)
            .ok_or_else(|| Error::Parse("could not parse album page".to_owned()))
    }

    /// Fetch a full artist page (header + top songs + albums) by its browse id.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Http`] on a transport failure, [`Error::Api`] when
    /// innertube answers with a non-success status, [`Error::Bootstrap`] when no
    /// anonymous visitor identity can be established, [`Error::ResponseTooLarge`]
    /// for an oversized response, and [`Error::Parse`] when the body is not JSON
    /// or the page shape is not understood.
    pub async fn artist(&self, browse_id: &str) -> Result<ArtistPage> {
        let resp = self.browse_raw(browse_id).await?;
        parse::artist_page(&resp, browse_id)
            .ok_or_else(|| Error::Parse("could not parse artist page".to_owned()))
    }

    /// Fetch a playlist's name and tracks, following continuations up to `limit`.
    ///
    /// A page that yields no *tracks* is not the end of the playlist: the parser
    /// drops UGC/video/episode rows on purpose, so a page of nothing but videos
    /// still moved the walk along and the next page may hold audio. Stopping
    /// there truncated the import at the first video-only page — the rest of the
    /// playlist silently missing, with no error to show for it.
    ///
    /// What does end the walk, in order: the caller's `limit`; the absence of a
    /// continuation token; the request budget (below), which is what keeps a run
    /// of track-less pages from making the walk unbounded; a page that carried no
    /// items at all; and a token that comes back unchanged, which is the spin the
    /// old track-based guard was actually protecting against — a page with no
    /// rows and a token that never advances. A page whose token does not advance
    /// still contributes the tracks it carried: the guard ends the walk, it does
    /// not invalidate the content already parsed.
    ///
    /// `playlist_id` is a bare playlist id (`PL…`, `OLAK5uy_…`); a leading `VL`
    /// browse prefix is accepted and stripped, so callers that copied a browse
    /// id are not silently broken by a double prefix.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Http`] on a transport failure, [`Error::Api`] when
    /// innertube answers with a non-success status, [`Error::Bootstrap`] when no
    /// anonymous visitor identity can be established, [`Error::ResponseTooLarge`]
    /// for an oversized response, and [`Error::Parse`] when a page body is not
    /// JSON. A failure on the first page or on any continuation request fails the
    /// whole call; only the `limit` and uninterrupted-token bounds return partial
    /// results.
    pub async fn playlist(&self, playlist_id: &str, limit: usize) -> Result<Playlist> {
        // Callers normally pass a bare id (the artist page strips the prefix
        // itself), but a pasted browse id can carry it; the request needs
        // exactly one `VL`.
        let playlist_id = playlist_id.strip_prefix("VL").unwrap_or(playlist_id);
        let first = self.browse_raw(&format!("VL{playlist_id}")).await?;
        let page = parse::playlist_page(&first);
        let name = page.name.unwrap_or_default();
        let mut tracks = page.tracks;
        let mut continuation = page.continuation;
        // Continuation requests this call may still make.
        //
        // A page that yields tracks advances `tracks` by at least one, so those
        // pages are already bounded by `limit`. What that bound does not cover is
        // a page whose rows were all filtered out: it advances nothing, so the
        // walk would keep going for exactly as long as the server keeps handing
        // out *fresh* tokens — one round trip per page, with no ceiling at all.
        // Budgeting `limit` requests for the whole call makes it finite without
        // inventing a constant: a caller asking for N tracks is also a caller
        // agreeing to wait for at most N round trips, and a playlist whose pages
        // each carry real tracks needs far fewer than that. Hitting the budget
        // returns what was found so far, exactly as running out of tokens does.
        let mut requests_left = limit;

        while tracks.len() < limit {
            let Some(token) = continuation else { break };
            if requests_left == 0 {
                break;
            }
            requests_left -= 1;
            let next = self
                .post("browse", json!({ "continuation": token }))
                .await?;
            let page = parse::playlist_page(&next);
            // A page whose rows were all filtered out is explained, not empty:
            // keep following its token, budget permitting.
            if page.items_seen == 0 {
                break;
            }
            // Keep the rows *before* testing the token: a token that does not
            // advance ends the walk, but the page that carried it was a real
            // page and its tracks are not made invalid by the server repeating
            // itself.
            tracks.extend(page.tracks);
            // ... but only continue while the token actually advances. Following
            // a token that comes back unchanged would spin, and it is not the
            // budget's job to notice that.
            if page.continuation.as_deref() == Some(token.as_str()) {
                break;
            }
            continuation = page.continuation;
        }
        tracks.truncate(limit);

        Ok(Playlist {
            id: playlist_id.to_owned(),
            name,
            tracks,
        })
    }

    /// Fetch per-artist credits and album info for a track (for enrichment).
    ///
    /// `video_id` is passed through to the parser, which matches the byline to
    /// that exact queue entry — a substituted video must not contribute another
    /// song's artists.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Http`] on a transport failure, [`Error::Api`] when
    /// innertube answers with a non-success status, [`Error::Bootstrap`] when no
    /// anonymous visitor identity can be established, [`Error::ResponseTooLarge`]
    /// for an oversized response, and [`Error::Parse`] when the body is not JSON
    /// or the response does not carry the requested `video_id`.
    pub async fn track_credits(
        &self,
        video_id: &str,
    ) -> Result<(Vec<ArtistRef>, Option<String>, Option<String>)> {
        let resp = self.next_raw(video_id).await?;
        parse::track_credits(&resp, video_id)
            .ok_or_else(|| Error::Parse("could not parse track credits".to_owned()))
    }

    /// The raw, unparsed search response for a given filter (debugging/tests).
    #[doc(hidden)]
    pub async fn search_raw(&self, filter: SearchFilter, query: &str) -> Result<Value> {
        self.post(
            "search",
            json!({ "query": query, "params": filter.params() }),
        )
        .await
    }

    /// The raw, unparsed `/browse` response for a browse id (debugging/tests).
    #[doc(hidden)]
    pub async fn browse_raw(&self, browse_id: &str) -> Result<Value> {
        self.post("browse", json!({ "browseId": browse_id })).await
    }

    /// The raw, unparsed `/next` response for a video id (debugging/tests).
    #[doc(hidden)]
    pub async fn next_raw(&self, video_id: &str) -> Result<Value> {
        self.post(
            "next",
            json!({
                "videoId": video_id,
                "enablePersistentPlaylistPanel": true,
                "isAudioOnly": true,
                "tunerSettingValue": "AUTOMIX_SETTING_NORMAL",
            }),
        )
        .await
    }

    /// POST to an innertube endpoint, merging the session context into the body.
    ///
    /// Makes at most two attempts. A 403 is the visitor-data rejection the retry
    /// exists for: the rejected id is evicted from the identity pool and the
    /// request is retried once with the id the pool then hands out. Every other
    /// status is final, so the retry is bounded by construction (one rotation,
    /// not a loop).
    ///
    /// The 403 is handled before the body is parsed: `YouTube` has answered a
    /// flagged visitor with an HTML block page as well as JSON, and "the body is
    /// not JSON" must not turn the rotation path into a `Parse` error. For the
    /// same reason a non-JSON body on any other non-success status is reported as
    /// [`Error::Api`] with a short excerpt, not as a parse failure — the status is
    /// what the caller can act on.
    async fn post(&self, endpoint: &str, extra: Value) -> Result<Value> {
        let mut rotated = false;
        loop {
            let version = context::client_version(&self.inner.http, USER_AGENT).await;
            let vdata = identity::visitor_data(&self.inner.http, USER_AGENT).await?;
            let mut body = extra.clone();
            if let Some(obj) = body.as_object_mut() {
                obj.insert(
                    "context".to_owned(),
                    context::context_object(&vdata, &version),
                );
            }

            let base = context::home();
            let resp = self
                .inner
                .http
                .post(format!("{base}/youtubei/v1/{endpoint}?prettyPrint=false"))
                .header(header::CONTENT_TYPE, "application/json")
                .header("X-YouTube-Client-Name", CLIENT_NAME_ID)
                .header("X-YouTube-Client-Version", &version)
                .header("X-Goog-EOM-Visitor-Id", &vdata)
                .header(header::ORIGIN, base)
                .header(header::REFERER, base)
                .header(header::COOKIE, identity::CONSENT_COOKIE)
                .json(&body)
                .send()
                .await?;

            let status = resp.status();
            if status == reqwest::StatusCode::FORBIDDEN {
                // Evict exactly the id this response rejected. If the retried
                // request is rejected too, evict that one as well and give up:
                // one rotation is the whole retry budget.
                identity::ban(&vdata);
                if rotated {
                    return Err(Error::Api {
                        status: 403,
                        message: "forbidden after rotating visitor data".to_owned(),
                    });
                }
                rotated = true;
                continue;
            }

            // Reject an oversized *declared* body before reading anything. The
            // actual received bytes are still counted while streaming, because
            // a chunked or decompressed response declares no length at all.
            if let Some(declared) = resp.content_length() {
                if declared > MAX_RESPONSE_BYTES {
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

            let parsed: Value = serde_json::from_slice(&raw)
                .map_err(|e| Error::Parse(format!("response was not JSON: {e}")))?;
            return Ok(parsed);
        }
    }
}

/// `client.rs` transport tests, against a real HTTP server on loopback.
///
/// A `std::net::TcpListener` on 127.0.0.1 serving canned responses exercises the
/// whole request path, so these need no mock-HTTP crate and no network. What
/// they reach that the parser fixtures cannot: the 403 retry and its visitor-id
/// rotation (H1), the request that actually gets built, pagination and its stop
/// conditions, and error mapping.
///
/// Two pieces of process-global state make these need care, and both are worth
/// knowing before adding another test here:
///
/// * `context::home()` is substituted once per process, so every test in this
///   module talks to the one server below.
/// * the visitor pool outlives a test. A test that needs a *specific* visitor on
///   its first request seeds the pool (`seed_visitor`) rather than trying to
///   arrange ordering, and holds [`visitor_gate`] so no concurrent test's
///   request is affected by that seeding.
#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::{Arc, Mutex, OnceLock};

    use serde_json::{json, Value};

    use super::*;
    use crate::context;
    use crate::identity;

    /// The visitor the fake server rejects, so the retry's rotation is visible.
    const REJECTED_VISITOR: &str = "visitor-that-gets-403";
    /// The visitor the fake home page hands out, which the client caches after
    /// fetching it. Tests that do not seed the pool get this one.
    const HOME_VISITOR: &str = "visitor-from-home-cookie";
    /// Queries that make the fake search endpoint answer with a fixed status,
    /// so "only a 403 is retried" and "a permanent 403 gives up" are testable
    /// without depending on the seeded visitor.
    const ALWAYS_429: &str = "__always_429__";
    const ALWAYS_403: &str = "__always_403__";
    /// A 403 whose body is an HTML block page rather than the usual JSON error
    /// object: the rotation retry must not depend on the body being JSON.
    const ALWAYS_403_HTML: &str = "__always_403_html__";
    /// A non-success status with a non-JSON body: an HTTP error, not a parse
    /// failure.
    const ALWAYS_500_HTML: &str = "__always_500_html__";
    /// A response that declares a body larger than the client's ceiling.
    const DECLARED_TOO_LARGE: &str = "__declared_too_large__";
    /// A chunked response larger than the ceiling, which declares no length.
    const CHUNKED_TOO_LARGE: &str = "__chunked_too_large__";

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
        /// `/youtubei/v1/<endpoint>?...` -> `<endpoint>`.
        fn endpoint(&self) -> &str {
            self.path
                .split("/youtubei/v1/")
                .nth(1)
                .and_then(|rest| rest.split('?').next())
                .unwrap_or("")
        }
        fn query(&self) -> &str {
            self.body.get("query").and_then(Value::as_str).unwrap_or("")
        }
        fn browse_id(&self) -> &str {
            self.body
                .get("browseId")
                .and_then(Value::as_str)
                .unwrap_or("")
        }
        fn continuation(&self) -> &str {
            self.body
                .get("continuation")
                .and_then(Value::as_str)
                .unwrap_or("")
        }
    }

    struct Response {
        status: u16,
        /// Extra headers, e.g. the `Set-Cookie` carrying visitor data.
        headers: Vec<(String, String)>,
        body: String,
        /// Overrides the `Content-Length` the server advertises, so a response
        /// can *declare* a body larger than it sends. Exercises the client's
        /// early size rejection without moving tens of megabytes through
        /// loopback.
        declared_length: Option<u64>,
        /// Send the body with `Transfer-Encoding: chunked` instead of a
        /// `Content-Length`. A chunked response has no declared length, which is
        /// what makes the client's streaming byte count the only bound.
        chunked: bool,
    }

    impl Response {
        fn json(body: Value) -> Self {
            Self {
                status: 200,
                headers: Vec::new(),
                body: body.to_string(),
                declared_length: None,
                chunked: false,
            }
        }
        fn error(status: u16) -> Self {
            Self {
                status,
                headers: Vec::new(),
                body: json!({ "error": { "message": "rejected by the test server" } }).to_string(),
                declared_length: None,
                chunked: false,
            }
        }
        /// A non-JSON body with an explicit status, as an HTML block page or
        /// proxy error would be.
        fn text(status: u16, body: impl Into<String>) -> Self {
            Self {
                status,
                headers: Vec::new(),
                body: body.into(),
                declared_length: None,
                chunked: false,
            }
        }
        /// A chunked body: the client cannot learn its size from the headers.
        fn chunked(body: impl Into<String>) -> Self {
            Self {
                status: 200,
                headers: Vec::new(),
                body: body.into(),
                declared_length: None,
                chunked: true,
            }
        }
        /// A body that *declares* a length different from what it sends.
        fn declared_length(body: impl Into<String>, declared: u64) -> Self {
            Self {
                status: 200,
                headers: Vec::new(),
                body: body.into(),
                declared_length: Some(declared),
                chunked: false,
            }
        }
    }

    type Handler = Box<dyn Fn(&Request) -> Response + Send + Sync>;

    /// Loopback HTTP server, routed by path.
    struct FakeYt {
        addr: String,
        requests: Arc<Mutex<Vec<Request>>>,
    }

    /// Bind a server, move its state into the accept thread, and hand back the
    /// handle tests use to inspect what was sent.
    ///
    /// Each connection is handled on its own thread so a panic in a route cannot
    /// take the accept loop down with it. That is not defensive padding: this
    /// server is shared by every test in the binary, so a died accept loop turns
    /// one handler bug into `ConnectionRefused` in unrelated tests, which is a
    /// far more confusing failure than the original panic.
    fn spawn(routes: HashMap<String, Handler>) -> FakeYt {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = format!("http://{}", listener.local_addr().expect("local addr"));
        let requests: Arc<Mutex<Vec<Request>>> = Arc::new(Mutex::new(Vec::new()));

        let (log, routes) = (requests.clone(), Arc::new(routes));
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let (log, routes) = (log.clone(), routes.clone());
                let _ = std::thread::spawn(move || serve(stream, &log, &routes));
            }
        });

        FakeYt { addr, requests }
    }

    /// Read the requests seen so far, surviving a poisoned log.
    ///
    /// A route that panics while a test holds nothing still leaves the log
    /// poisoned if the panic happened during a `lock()`. Recovering the guard
    /// keeps that from turning into a second, misleading failure.
    fn logged(requests: &Mutex<Vec<Request>>) -> Vec<Request> {
        requests
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .map(|r| Request {
                method: r.method.clone(),
                path: r.path.clone(),
                headers: r.headers.clone(),
                body: r.body.clone(),
            })
            .collect()
    }

    /// Read one request, write one response. Always answers, so a test cannot
    /// hang waiting on a connection that was never replied to.
    fn serve(
        mut stream: TcpStream,
        log: &Mutex<Vec<Request>>,
        routes: &HashMap<String, Handler>,
    ) -> std::io::Result<()> {
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
        let route = request.path.split('?').next().unwrap_or("").to_owned();

        // Log and release before running the handler: this server is shared by
        // every test in the binary, so a handler that panics — or a future one
        // that reaches back into the log — must not be able to poison the lock
        // for everyone else.
        log.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(request.clone());

        let response = match routes.get(&route) {
            Some(handler) => handler(&request),
            None => Response {
                status: 500,
                headers: Vec::new(),
                body: json!({ "error": { "message": format!("no route for {route}") } })
                    .to_string(),
                declared_length: None,
                chunked: false,
            },
        };

        let reason = match response.status {
            200 => "OK",
            403 => "Forbidden",
            429 => "Too Many Requests",
            _ => "Error",
        };
        let mut head = format!(
            "HTTP/1.1 {} {reason}\r\nContent-Type: application/json\r\nConnection: close\r\n",
            response.status
        );
        if response.chunked {
            head.push_str("Transfer-Encoding: chunked\r\n");
        } else {
            let length = response
                .declared_length
                .unwrap_or(response.body.len() as u64);
            head.push_str("Content-Length: ");
            head.push_str(&length.to_string());
            head.push_str("\r\n");
        }
        for (name, value) in &response.headers {
            head.push_str(&format!("{name}: {value}\r\n"));
        }
        head.push_str("\r\n");
        stream.write_all(head.as_bytes())?;
        if response.chunked {
            // Write in chunks so the client has several streaming reads to count;
            // one giant write would also work but would let a reader that only
            // checks the first chunk look correct.
            for chunk in response.body.as_bytes().chunks(8192) {
                stream.write_all(format!("{:X}\r\n", chunk.len()).as_bytes())?;
                stream.write_all(chunk)?;
                stream.write_all(b"\r\n")?;
            }
            stream.write_all(b"0\r\n\r\n")?;
        } else {
            stream.write_all(response.body.as_bytes())?;
        }
        stream.flush()
    }

    /// The version the fake home page advertises; asserted on the innertube
    /// headers, so a change here is visible in one place.
    const CLIENT_VERSION: &str = "1.20250901.00.00";

    /// Home-page requests that should answer without a client version. Set by the
    /// H2 guard and decremented as it is spent; zero for every other test, so the
    /// shared server behaves exactly as it always did.
    static HOME_SCRAPE_FAILURES: std::sync::atomic::AtomicUsize =
        std::sync::atomic::AtomicUsize::new(0);

    fn home_html() -> String {
        format!(
            r#"<script>window["ytcfg"].set({{"INNERTUBE_CONTEXT_CLIENT_VERSION":"{CLIENT_VERSION}"}});</script>"#
        )
    }

    /// One `musicResponsiveListItemRenderer` row.
    fn track_row(id: &str, title: &str, artist: &str) -> Value {
        json!({
            "musicResponsiveListItemRenderer": {
                "playlistItemData": { "videoId": id },
                "flexColumns": [
                    { "musicResponsiveListItemFlexColumnRenderer": {
                        "text": { "runs": [{ "text": title }] } } },
                    { "musicResponsiveListItemFlexColumnRenderer": {
                        "text": { "runs": [
                            { "text": "Song" }, { "text": " • " },
                            { "text": artist, "navigationEndpoint": { "browseEndpoint": { "browseId": "UCartist" } } },
                            { "text": " • " }, { "text": "3:00" }
                        ] } } }
                ],
                "fixedColumns": [{ "musicResponsiveListItemFixedColumnRenderer": {
                    "text": { "runs": [{ "text": "3:00" }] } } }]
            }
        })
    }

    /// The same row typed `MUSIC_VIDEO_TYPE_UGC`: a user upload the parser drops
    /// on the strength of the type alone, however clean its title is.
    fn ugc_row(id: &str) -> Value {
        let mut row = track_row(id, "A User Upload", "Some Channel");
        row["musicResponsiveListItemRenderer"]["overlay"] = json!({
            "musicItemThumbnailOverlayRenderer": { "content": { "musicPlayButtonRenderer": {
                "playNavigationEndpoint": { "watchEndpoint": {
                    "videoId": id,
                    "watchEndpointMusicSupportedConfigs": { "watchEndpointMusicConfig": {
                        "musicVideoType": "MUSIC_VIDEO_TYPE_UGC"
                    } }
                } }
            } } }
        });
        row
    }

    /// A row badged "Video": dropped by the badge, which is the other reason a
    /// page can carry rows and yield no tracks.
    fn video_row(id: &str) -> Value {
        let mut row = track_row(id, "A Music Video", "Some Artist");
        row["musicResponsiveListItemRenderer"]["flexColumns"][1]
            ["musicResponsiveListItemFlexColumnRenderer"]["text"]["runs"][0]["text"] =
            json!("Video");
        row
    }

    /// Requests the "stuck" route has served for its own token.
    static STUCK_SERVED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    /// Identical tokens the "stuck" route serves before it stops cooperating and
    /// hands out a real track. A walk that keeps following a token which never
    /// advances therefore ends holding a track, and the test that says it must
    /// hold nothing fails — rather than hanging the suite until the harness
    /// gives up.
    const STUCK_TOKEN_LIMIT: usize = 5;

    /// Fruitless pages the "endless" route serves, each with a *fresh* token,
    /// before a real track finally arrives. Higher than any cap a `playlist`
    /// call should follow, so a missing budget is visible as that track being
    /// reached — and as this many requests — rather than as a hang.
    const ENDLESS_FILTERED_PAGES: usize = 40;

    /// A playlist page: a named header, some rows, and optionally a continuation
    /// token. Rows and a token coexist on a real page — the shape the guard in
    /// `playlist` exists for is a page with a token but *no* rows, expressed by
    /// passing an empty `rows`.
    fn playlist_page(name: &str, rows: Vec<Value>, continuation: Option<&str>) -> Value {
        let mut section = json!({
            "contents": [{ "musicPlaylistShelfRenderer": { "contents": rows } }]
        });
        if let Some(token) = continuation {
            section["continuations"] =
                json!([{ "nextContinuationData": { "continuation": token } }]);
        }
        json!({
            "header": { "musicResponsiveHeaderRenderer": { "title": { "runs": [{ "text": name }] } } },
            "contents": { "twoColumnBrowseResultsRenderer": { "tabs": [{ "tabRenderer": {
                "content": { "sectionListRenderer": section } } }] } }
        })
    }

    /// A per-call id, so each `playlist` invocation gets its own story. Cargo
    /// runs tests concurrently and the browse route is shared, so two tests
    /// following "the" pagination path would otherwise interleave.
    fn tx() -> u128 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    }

    /// Three pages of rows, tied together by `tx` so the ids are unique to the
    /// call that asked for them.
    fn paginated_playlist(tx: u128) -> Value {
        let row = |n: usize| track_row(&format!("p{tx}-{n}"), &format!("Track {n}"), "A");
        playlist_page(
            "Pagination Test",
            vec![row(1), row(2)],
            Some(&format!("{tx}:page-2")),
        )
    }

    fn paginated_playlist_continuation(tx: u128, token: &str) -> Value {
        let row = |n: usize| track_row(&format!("p{tx}-{n}"), &format!("Track {n}"), "A");
        if token.ends_with("page-2") {
            playlist_page(
                "Pagination Test",
                vec![row(3), row(4), row(5)],
                Some(&format!("{tx}:page-3")),
            )
        } else {
            playlist_page("Pagination Test", vec![row(6), row(7)], None)
        }
    }

    /// The server every client test in this process talks to.
    ///
    /// One instance, because the client-version cache and the visitor pool are
    /// process-global: pointing them at a second host mid-run would be wrong.
    fn server() -> &'static FakeYt {
        static SERVER: OnceLock<FakeYt> = OnceLock::new();
        SERVER.get_or_init(|| {
            let mut routes: HashMap<String, Handler> = HashMap::new();

            routes.insert(
                "/".to_owned(),
                Box::new(|_: &Request| {
                    // A planned failure answers a page with no version in it, so
                    // the scrape returns nothing. Every other test leaves the
                    // counter at zero and gets the versioned page.
                    let failing = HOME_SCRAPE_FAILURES
                        .fetch_update(
                            std::sync::atomic::Ordering::SeqCst,
                            std::sync::atomic::Ordering::SeqCst,
                            |left| left.checked_sub(1),
                        )
                        .is_ok();
                    Response {
                        status: 200,
                        // `identity::fetch` reads visitor data from this cookie
                        // before scraping the HTML, so it is what fills the pool.
                        headers: vec![(
                            "Set-Cookie".to_owned(),
                            format!("__Secure-YEC={HOME_VISITOR}; Path=/; Secure"),
                        )],
                        body: if failing {
                            "<html></html>".to_owned()
                        } else {
                            home_html()
                        },
                        declared_length: None,
                        chunked: false,
                    }
                }),
            );

            routes.insert(
                "/youtubei/v1/search".to_owned(),
                Box::new(|req: &Request| match req.query() {
                    ALWAYS_429 => Response::error(429),
                    ALWAYS_403 => Response::error(403),
                    ALWAYS_403_HTML => Response::text(403, "<html><body>Forbidden</body></html>"),
                    ALWAYS_500_HTML => Response::text(500, "upstream failed: please retry later"),
                    DECLARED_TOO_LARGE => {
                        Response::declared_length("{}", MAX_RESPONSE_BYTES + 1)
                    }
                    CHUNKED_TOO_LARGE => Response::chunked(
                        "x".repeat(usize::try_from(MAX_RESPONSE_BYTES).map_or(0, |cap| cap * 3)),
                    ),
                    _ if req.header("x-goog-eom-visitor-id") == REJECTED_VISITOR => {
                        Response::error(403)
                    }
                    _ => Response::json(json!({
                        "contents": { "tabbedSearchResultsRenderer": { "tabs": [{ "tabRenderer": {
                            "content": { "sectionListRenderer": { "contents": [{
                                "itemSectionRenderer": { "contents": [
                                    track_row("s1", "Blinding Lights", "The Weeknd")
                                ] }
                            }] } } } }] } }
                    })),
                }),
            );

            routes.insert(
                "/youtubei/v1/browse".to_owned(),
                Box::new(|req: &Request| {
                    let id = req.browse_id();
                    let token = req.continuation();

                    // One audio row, then a continuation whose page carries no
                    // rows at all, then a page that does carry one. An itemless
                    // page is a dead end, so the last page must never be reached —
                    // and because it holds a real track, a walk that wrongly keeps
                    // going fails an assertion in
                    // `playlist_pagination_stops_when_a_page_carries_no_items`
                    // instead of looping.
                    if id.contains("degenerate") {
                        return Response::json(playlist_page(
                            "Degenerate",
                            vec![track_row("degenerate-first", "First", "A")],
                            Some("loop"),
                        ));
                    }
                    if token == "loop" {
                        return Response::json(playlist_page(
                            "Degenerate",
                            Vec::new(),
                            Some("loop-2"),
                        ));
                    }
                    if token == "loop-2" {
                        return Response::json(playlist_page(
                            "Degenerate",
                            vec![track_row("degenerate-late", "Late Track", "A")],
                            None,
                        ));
                    }
                    // A page whose rows are all filtered out, sitting between two
                    // pages of audio: the walk must not stop at it.
                    if let Some(tx) = id
                        .strip_prefix("VL")
                        .unwrap_or(id)
                        .strip_prefix("PLtest_filtered_")
                    {
                        return Response::json(playlist_page(
                            "Filtered",
                            vec![
                                track_row(&format!("f{tx}-1"), "Kept", "A"),
                                ugc_row(&format!("f{tx}-ugc-1")),
                            ],
                            Some(&format!("{tx}:filtered-1")),
                        ));
                    }
                    if let Some(tx) = token.strip_suffix(":filtered-1") {
                        return Response::json(playlist_page(
                            "Filtered",
                            vec![
                                ugc_row(&format!("f{tx}-ugc-2")),
                                video_row(&format!("f{tx}-vid-3")),
                            ],
                            Some(&format!("{tx}:filtered-2")),
                        ));
                    }
                    if let Some(tx) = token.strip_suffix(":filtered-2") {
                        return Response::json(playlist_page(
                            "Filtered",
                            vec![track_row(&format!("f{tx}-2"), "Kept Too", "A")],
                            None,
                        ));
                    }
                    // A page whose rows are all filtered *and* whose token does
                    // not advance. After `STUCK_TOKEN_LIMIT` identical tokens the
                    // route hands out a real track, so a missing no-advance guard
                    // fails `playlist_pagination_stops_when_a_filtered_page_does_not_advance`
                    // rather than spinning.
                    if id.contains("PLtest_stuck") {
                        return Response::json(playlist_page(
                            "Stuck",
                            vec![ugc_row("stuck-ugc")],
                            Some("stuck"),
                        ));
                    }
                    if token == "stuck" {
                        let served = STUCK_SERVED.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        return Response::json(if served < STUCK_TOKEN_LIMIT {
                            playlist_page("Stuck", vec![ugc_row("stuck-ugc")], Some("stuck"))
                        } else {
                            playlist_page("Stuck", vec![track_row("stuck-late", "Late", "A")], None)
                        });
                    }
                    // A playlist with an unbounded run of *fruitless* pages: every
                    // one carries a UGC row only, and every one hands out a fresh
                    // token, so neither the itemless guard nor the no-advance guard
                    // can end the walk. The run stops after
                    // `ENDLESS_FILTERED_PAGES`, where a real track arrives, so a
                    // walk that follows every token is caught by an assertion in
                    // `playlist_pagination_bounds_a_run_of_fruitless_pages`
                    // instead of running until the harness gives up.
                    if let Some(tx) = id
                        .strip_prefix("VL")
                        .unwrap_or(id)
                        .strip_prefix("PLtest_endless_")
                    {
                        return Response::json(playlist_page(
                            "Endless",
                            vec![ugc_row(&format!("e{tx}-ugc-0"))],
                            Some(&format!("{tx}:endless-1")),
                        ));
                    }
                    if let Some((tx, n)) = token.split_once(":endless-") {
                        if let Ok(n) = n.parse::<usize>() {
                            return Response::json(if n < ENDLESS_FILTERED_PAGES {
                                playlist_page(
                                    "Endless",
                                    vec![ugc_row(&format!("e{tx}-ugc-{n}"))],
                                    Some(&format!("{tx}:endless-{}", n + 1)),
                                )
                            } else {
                                playlist_page(
                                    "Endless",
                                    vec![track_row(&format!("e{tx}-late"), "Late", "A")],
                                    None,
                                )
                            });
                        }
                    }
                    // A page whose continuation token never advances, but whose
                    // rows are real audio: the walk has to stop *and* keep what
                    // the page already carried. The route hands out one track on
                    // each of its two pages, both under the same token, so a walk
                    // that drops the second page's row fails
                    // `playlist_keeps_the_tracks_of_a_page_whose_token_stops_advancing`.
                    if let Some(tx) = id
                        .strip_prefix("VL")
                        .unwrap_or(id)
                        .strip_prefix("PLtest_same_token_")
                    {
                        return Response::json(playlist_page(
                            "Same Token",
                            vec![track_row(&format!("s{tx}-1"), "First", "A")],
                            Some("same-token"),
                        ));
                    }
                    if token == "same-token" {
                        return Response::json(playlist_page(
                            "Same Token",
                            vec![track_row("s-late", "Second", "A")],
                            Some("same-token"),
                        ));
                    }
                    // `playlist()` requests `VL<id>`, so the prefix is stripped
                    // before matching.
                    if let Some(tx) = id
                        .strip_prefix("VL")
                        .unwrap_or(id)
                        .strip_prefix("PLtest_pagination_")
                    {
                        return Response::json(paginated_playlist(tx.parse().unwrap_or(0)));
                    }
                    // Continuations carry no browseId, so the transaction is
                    // parsed back out of the token this test handed out.
                    if token.contains("page-") {
                        let tx = token
                            .split(':')
                            .next()
                            .and_then(|t| t.parse().ok())
                            .unwrap_or(0);
                        return Response::json(paginated_playlist_continuation(tx, token));
                    }
                    if id.starts_with("MPREb_") {
                        return Response::json(json!({
                            "header": { "musicResponsiveHeaderRenderer": {
                                "title": { "runs": [{ "text": "Test Album" }] } } },
                            "contents": { "twoColumnBrowseResultsRenderer": {
                                "secondaryContents": { "sectionListRenderer": { "contents": [{
                                    "musicShelfRenderer": { "contents": [
                                        track_row("a1", "Album Song", "A")
                                    ] } }] } } } }
                        }));
                    }
                    Response::json(json!({
                        "header": { "musicImmersiveHeaderRenderer": {
                            "title": { "runs": [{ "text": "Test Artist" }] } } },
                        "contents": { "singleColumnBrowseResultsRenderer": { "tabs": [{ "tabRenderer": {
                            "content": { "sectionListRenderer": { "contents": [{
                                "musicShelfRenderer": { "contents": [
                                    track_row("ar1", "Artist Song", "A")
                                ] }
                            }] } } } }] } }
                    }))
                }),
            );

            let fake = spawn(routes);
            // Point the client at it. `set_home_for_test` takes the first value
            // it is given, so this stays consistent for the process.
            context::set_home_for_test(&fake.addr);
            fake
        })
    }

    /// Make `id` the visitor the next request goes out with.
    ///
    /// Seeding the pool rather than overriding the lookup: the retry path is
    /// exactly what these tests are about, and an override would bypass it.
    fn seed_visitor(id: &str) {
        identity::seed_pool_for_test(&[id]);
    }

    /// Serialises the tests in this module. Held by **every** test here.
    ///
    /// Two pieces of process-global state are shared: the request log, and the
    /// visitor pool that decides which id goes out. A test that seeds the pool
    /// therefore cannot run alongside any other test that makes a request —
    /// including one that does not care about the visitor id, because its lookup
    /// would consume the seeded value. That is exactly how
    /// `the_request_carries_the_context_object_and_innertube_headers` first
    /// became an intermittent failure: it seeded an id, and a concurrent test's
    /// request took it, so the assertion saw `HOME_VISITOR` instead.
    ///
    /// So the rule is blunt on purpose — take the gate, and hold it for the whole
    /// test. These tests are milliseconds each and the client is I/O-bound, so
    /// serialising them costs nothing that matters, and it removes a class of
    /// failure that is otherwise very hard to read. Blocks rather than fails:
    /// they are independent, they just each need the shared state to themselves.
    ///
    /// Cargo runs test functions on parallel threads by default, so this is not
    /// hypothetical. The gate itself lives in `identity` because `identity`'s
    /// tests touch the same pool with a different lock; a gate that only covered
    /// this module left the pair green by scheduling luck.
    async fn visitor_gate() -> tokio::sync::MutexGuard<'static, ()> {
        identity::pool_gate().lock().await
    }

    fn client() -> Client {
        server();
        Client::new().expect("client")
    }

    /// H1's regression guard, end to end: the first 403 must produce a *second*
    /// request carrying a different visitor id, and that request must succeed.
    ///
    /// Before the fix, `ban` left the id in the pool *and* `visitor_data` handed
    /// out a pooled id without consulting `banned`, so the retry re-selected the
    /// id it had just banned and the whole metadata surface died for the life of
    /// the process. `identity.rs` has unit tests for the eviction itself; this
    /// one proves the retry loop actually recovers.
    ///
    /// Note the two halves mask each other: reverting *either* alone leaves this
    /// test passing, because eviction alone stops a banned id being handed back
    /// and the `banned` filter alone does too. It takes both to reproduce the
    /// original defect, which is the honest scope of the guard — verified by
    /// reverting both and watching this report
    /// `["visitor-that-gets-403", "visitor-that-gets-403"]`.
    #[tokio::test]
    async fn a_rejected_visitor_is_rotated_and_the_request_retried() {
        let _gate = visitor_gate().await;
        let fake = server();
        let c = client();

        // A query unique to this call, so the assertion below is about *these*
        // requests and stays correct if the test is re-run in the same process
        // (cargo's `--rerun-failed`, or a retry harness).
        let query = format!("rotation-{}", tx());
        seed_visitor(REJECTED_VISITOR);
        // No un-seeding afterwards: the client bans this id on the 403 and
        // fetches a fresh one, which is the behaviour under test.
        let outcome = c.search_tracks(&query).await;

        let sent: Vec<String> = logged(&fake.requests)
            .iter()
            .filter(|r| r.endpoint() == "search" && r.query() == query)
            .map(|r| r.header("x-goog-eom-visitor-id").to_owned())
            .collect();
        // Asserted as one value so a failure reports the whole sequence.
        assert_eq!(
            (
                sent,
                outcome
                    .as_ref()
                    .map(std::vec::Vec::len)
                    .map_err(ToString::to_string)
            ),
            (
                vec![REJECTED_VISITOR.to_owned(), HOME_VISITOR.to_owned()],
                Ok(1)
            ),
            "expected the seeded id, then a freshly fetched one"
        );
    }

    #[tokio::test]
    async fn the_request_carries_the_context_object_and_innertube_headers() {
        let _gate = visitor_gate().await;
        let fake = server();
        let c = client();
        seed_visitor("visitor-for-context-test");

        // A query unique to this test, so its request can be picked out of the
        // shared log. Taking "the latest search" instead is how this test first
        // failed: another test's request landed after it.
        const QUERY: &str = "blinding lights";
        c.search_tracks(QUERY).await.expect("search");

        let reqs = logged(&fake.requests);
        let search = reqs
            .iter()
            .find(|r| r.endpoint() == "search" && r.query() == QUERY)
            .expect("a search request for this test's query");

        assert_eq!(search.method, "POST");
        assert_eq!(search.header("content-type"), "application/json");
        assert_eq!(search.header("x-youtube-client-name"), CLIENT_NAME_ID);
        assert_eq!(
            search.header("x-youtube-client-version"),
            CLIENT_VERSION,
            "the version scraped from the home page"
        );
        assert_eq!(
            search.header("x-goog-eom-visitor-id"),
            "visitor-for-context-test"
        );
        assert!(search.header("origin").starts_with("http://127.0.0.1"));
        assert!(search.header("referer").starts_with("http://127.0.0.1"));
        assert!(
            search.header("cookie").contains("SOCS="),
            "the consent cookie must be sent"
        );

        let client_ctx = search
            .body
            .pointer("/context/client")
            .expect("context.client");
        assert_eq!(client_ctx["clientName"], context::CLIENT_NAME);
        assert_eq!(client_ctx["visitorData"], "visitor-for-context-test");
        assert_eq!(client_ctx["hl"], "en");
        assert_eq!(search.body["query"], QUERY);
        assert!(
            search.body["params"].is_string(),
            "a filtered search must send params"
        );
    }

    #[tokio::test]
    async fn only_a_403_is_retried() {
        let _gate = visitor_gate().await;
        let fake = server();
        let c = client();

        let err = c
            .search_tracks(ALWAYS_429)
            .await
            .expect_err("a 429 must not be treated as success");
        match err {
            Error::Api { status, .. } => assert_eq!(status, 429),
            other => panic!("expected Error::Api, got {other:?}"),
        }

        let attempts = logged(&fake.requests)
            .iter()
            .filter(|r| r.endpoint() == "search" && r.query() == ALWAYS_429)
            .count();
        assert_eq!(attempts, 1, "only a 403 is retried");
    }

    #[tokio::test]
    async fn a_403_on_every_attempt_reports_forbidden() {
        let _gate = visitor_gate().await;
        let fake = server();
        let c = client();

        let err = c
            .search_tracks(ALWAYS_403)
            .await
            .expect_err("a permanent 403 must fail");
        match err {
            Error::Api { status, message } => {
                assert_eq!(status, 403);
                assert!(
                    message.contains("rotating"),
                    "the give-up path should say the rotation was exhausted: {message}"
                );
            }
            other => panic!("expected Error::Api, got {other:?}"),
        }

        let attempts = logged(&fake.requests)
            .iter()
            .filter(|r| r.endpoint() == "search" && r.query() == ALWAYS_403)
            .count();
        assert_eq!(attempts, 2, "the retry budget is one extra attempt");
    }

    #[tokio::test]
    async fn playlist_pagination_follows_continuations_up_to_the_limit() {
        let _gate = visitor_gate().await;
        let c = client();
        let tx = tx();
        let playlist = c
            .playlist(&format!("PLtest_pagination_{tx}"), 7)
            .await
            .expect("playlist");

        assert_eq!(playlist.name, "Pagination Test");
        let expected: Vec<String> = (1..=7).map(|n| format!("p{tx}-{n}")).collect();
        assert_eq!(
            playlist
                .tracks
                .iter()
                .map(|t| t.id.as_str())
                .collect::<Vec<_>>(),
            expected.iter().map(String::as_str).collect::<Vec<_>>(),
            "three pages followed, in order"
        );
    }

    #[tokio::test]
    async fn playlist_pagination_truncates_at_the_limit() {
        let _gate = visitor_gate().await;
        let c = client();
        let tx = tx();
        let playlist = c
            .playlist(&format!("PLtest_pagination_{tx}"), 3)
            .await
            .expect("playlist");
        let expected: Vec<String> = (1..=3).map(|n| format!("p{tx}-{n}")).collect();
        assert_eq!(
            playlist
                .tracks
                .iter()
                .map(|t| t.id.as_str())
                .collect::<Vec<_>>(),
            expected.iter().map(String::as_str).collect::<Vec<_>>(),
            "stops mid-page at the limit rather than taking the whole page"
        );
    }

    /// The walk also ends when a page offers no continuation, even though the
    /// limit is far from reached — the other bound the pagination loop owes its
    /// caller.
    #[tokio::test]
    async fn playlist_pagination_stops_at_the_page_without_a_continuation() {
        let _gate = visitor_gate().await;
        let c = client();
        let tx = tx();
        let playlist = c
            .playlist(&format!("PLtest_pagination_{tx}"), 50)
            .await
            .expect("playlist");

        let expected: Vec<String> = (1..=7).map(|n| format!("p{tx}-{n}")).collect();
        assert_eq!(
            playlist
                .tracks
                .iter()
                .map(|t| t.id.as_str())
                .collect::<Vec<_>>(),
            expected.iter().map(String::as_str).collect::<Vec<_>>(),
            "all three pages; the third hands out no token, so the walk ends there"
        );
    }

    /// The exact regression: a continuation page whose rows were **all filtered
    /// out** is not the end of the playlist.
    ///
    /// The parser drops UGC/video/episode rows on purpose, so "this page yielded
    /// no tracks" and "this page was empty" are different facts. The old loop
    /// broke on the first, which truncated the import at the first video-only
    /// page: the tracks after it were missing, and an empty playlist and a
    /// truncated one look identical to the caller. The route here serves audio,
    /// then a page of nothing but a UGC upload and a "Video"-badged row, then
    /// more audio.
    #[tokio::test]
    async fn playlist_pagination_follows_a_page_whose_rows_were_all_filtered() {
        let _gate = visitor_gate().await;
        let c = client();
        let tx = tx();
        let playlist = c
            .playlist(&format!("PLtest_filtered_{tx}"), 50)
            .await
            .expect("playlist");

        let expected: Vec<String> = [format!("f{tx}-1"), format!("f{tx}-2")].into();
        assert_eq!(
            playlist
                .tracks
                .iter()
                .map(|t| t.id.as_str())
                .collect::<Vec<_>>(),
            expected.iter().map(String::as_str).collect::<Vec<_>>(),
            "the audio on either side of the all-video page, and none of the videos"
        );
    }

    /// A page that carries no items at all is a dead end: the walk stops rather
    /// than following its token.
    ///
    /// This replaces `playlist_pagination_stops_when_a_page_yields_no_tracks`,
    /// which asserted the same outcome with a *track-less* page. That is the
    /// property worth keeping — but it pinned "stop on any page with no tracks",
    /// which is the bug next door: a page whose rows were merely filtered must be
    /// followed. Here the first page has audio, the second has a token but no
    /// rows, and the third has a real track, so a walk that wrongly keeps going
    /// ends holding `degenerate-late` and fails this assertion.
    #[tokio::test]
    async fn playlist_pagination_stops_when_a_page_carries_no_items() {
        let _gate = visitor_gate().await;
        let c = client();
        let playlist = c.playlist("PLtest_degenerate", 50).await.expect("playlist");
        assert_eq!(
            playlist
                .tracks
                .iter()
                .map(|t| t.id.as_str())
                .collect::<Vec<_>>(),
            ["degenerate-first"],
            "the first page's track, and nothing from beyond the itemless page"
        );
    }

    /// Once track-less pages are followed, the token is the only progress signal
    /// left — so a token that comes back unchanged has to end the walk, or the
    /// loop spins forever on a server that stops advancing. The route serves a
    /// real track after a few identical tokens, so a missing guard fails here
    /// rather than hanging the suite.
    #[tokio::test]
    async fn playlist_pagination_stops_when_a_filtered_page_does_not_advance() {
        let _gate = visitor_gate().await;
        STUCK_SERVED.store(0, std::sync::atomic::Ordering::SeqCst);
        let c = client();
        let playlist = c.playlist("PLtest_stuck", 50).await.expect("playlist");
        assert!(
            playlist.tracks.is_empty(),
            "a token that does not advance must end the walk: {:?}",
            playlist.tracks
        );
    }

    /// The walk is finite even when *no* page yields a track.
    ///
    /// Following a filtered page fixed the truncation but on its own removed the
    /// only bound there was: a page whose rows all filter out advances no track,
    /// so the walk ended only when the playlist did — one round trip per page,
    /// for as long as the server kept issuing fresh tokens, with neither the
    /// itemless guard nor the no-advance guard able to fire. The route here
    /// serves [`ENDLESS_FILTERED_PAGES`] such pages and then a real track, so a
    /// missing budget shows up as that track arriving and as a request count of
    /// 40, rather than as a hang.
    #[tokio::test]
    async fn playlist_pagination_bounds_a_run_of_fruitless_pages() {
        let _gate = visitor_gate().await;
        let fake = server();
        let c = client();
        let tx = tx();
        let limit = 5;
        let playlist = c
            .playlist(&format!("PLtest_endless_{tx}"), limit)
            .await
            .expect("playlist");

        // Nothing on any of those pages was audio, so the walk stops with
        // nothing — the point is what it did *not* do: reach the late track.
        assert!(
            playlist.tracks.is_empty(),
            "no page yielded audio, so nothing may come back: {:?}",
            playlist.tracks
        );
        assert!(
            !playlist.tracks.iter().any(|t| t.id.ends_with("-late")),
            "the walk must stop at the budget, not at the late track"
        );

        let prefix = format!("{tx}:endless-");
        let followed = logged(&fake.requests)
            .iter()
            .filter(|r| r.continuation().starts_with(&prefix))
            .count();
        assert_eq!(
            followed, limit,
            "the walk must spend its request budget and no more"
        );
    }

    /// H2's regression guard, at the entry point the app actually calls.
    ///
    /// The unit test in `context.rs` drives `resolve_client_version`, which
    /// restates the caching rule and cannot see this call site — and the defect
    /// the review recorded lived *here*: `client_version` pinning the fallback
    /// when the scrape failed, so one bad home page made every later request in
    /// the process send a stale `clientVersion`, which innertube answers with 400
    /// (a status the 403-only retry does not cover). The plan below fails the
    /// scrape once and then answers properly: the fallback may serve the first
    /// call, but it must not be remembered.
    ///
    /// The installed cache is what makes this test possible at all: the
    /// process-global one cannot be cleared, so sharing it would make the
    /// assertions depend on whether another test had already scraped a version.
    /// It also means this test can never fill the global for anyone else.
    #[tokio::test]
    async fn a_failed_client_version_scrape_is_not_remembered() {
        let _gate = visitor_gate().await;
        // Point `context::home()` at the loopback server before scraping.
        // Without this the scrape would go to the real YouTube Music — the one
        // way this test can pass for a reason that has nothing to do with the
        // code under test.
        let _server = server();
        let _fresh = context::FreshClientVersionCache::install();
        HOME_SCRAPE_FAILURES.store(1, std::sync::atomic::Ordering::SeqCst);

        let http = reqwest::Client::builder().build().expect("http client");
        let first = context::client_version(&http, USER_AGENT).await;
        assert_eq!(
            first,
            context::FALLBACK_VERSION,
            "a home page carrying no version must fall back rather than invent one"
        );
        assert!(
            HOME_SCRAPE_FAILURES.load(std::sync::atomic::Ordering::SeqCst) == 0,
            "the planned bad home page must have been the one this call read"
        );

        let second = context::client_version(&http, USER_AGENT).await;
        assert_ne!(
            second,
            context::FALLBACK_VERSION,
            "a failed scrape must not be cached: the next call has to try again"
        );
        assert_eq!(
            second, CLIENT_VERSION,
            "the second scrape reads the value the page now carries"
        );
    }

    #[tokio::test]
    async fn album_and_artist_pages_route_to_browse_and_parse() {
        let _gate = visitor_gate().await;
        let c = client();

        let album = c.album("MPREb_test").await.expect("album");
        assert_eq!(album.name, "Test Album");
        assert_eq!(album.tracks.len(), 1);

        let artist = c.artist("UCtest").await.expect("artist");
        assert_eq!(artist.name, "Test Artist");
    }

    /// A 403 is the visitor-rotation path whatever the body looks like.
    ///
    /// `YouTube` has answered a flagged visitor with an HTML block page instead of
    /// the usual JSON error object. Parsing the body before looking at the status
    /// turned that into a `Parse` error on the first attempt, so the documented
    /// retry never ran; both attempts must be made, and the second 403 is the
    /// give-up.
    #[tokio::test]
    async fn a_403_with_a_non_json_body_still_rotates_and_retries() {
        let _gate = visitor_gate().await;
        let fake = server();
        let c = client();

        let err = c
            .search_tracks(ALWAYS_403_HTML)
            .await
            .expect_err("a permanent 403 must fail");
        match err {
            Error::Api { status, .. } => assert_eq!(status, 403),
            other => panic!("expected Error::Api, got {other:?}"),
        }

        let attempts = logged(&fake.requests)
            .iter()
            .filter(|r| r.endpoint() == "search" && r.query() == ALWAYS_403_HTML)
            .count();
        assert_eq!(
            attempts, 2,
            "the rotation retry must not depend on the 403 body being JSON"
        );
    }

    /// A non-JSON body on a non-success status is an HTTP-level refusal: keeping
    /// the status and the server's own words is what the caller can act on, where
    /// "could not parse" would hide both. It must not be retried (only a 403 is).
    #[tokio::test]
    async fn a_non_json_error_body_is_reported_as_an_api_error() {
        let _gate = visitor_gate().await;
        let fake = server();
        let c = client();

        let err = c
            .search_tracks(ALWAYS_500_HTML)
            .await
            .expect_err("a 500 must fail");
        match err {
            Error::Api { status, message } => {
                assert_eq!(status, 500);
                assert!(
                    message.contains("upstream failed"),
                    "the body excerpt should reach the caller: {message:?}"
                );
            }
            other => panic!("expected Error::Api, got {other:?}"),
        }

        let attempts = logged(&fake.requests)
            .iter()
            .filter(|r| r.endpoint() == "search" && r.query() == ALWAYS_500_HTML)
            .count();
        assert_eq!(attempts, 1, "only a 403 is retried");
    }

    /// A body the server *declares* larger than the ceiling is rejected from the
    /// header alone, before any of it is buffered.
    #[tokio::test]
    async fn a_declared_oversized_body_is_rejected_without_being_read() {
        let _gate = visitor_gate().await;
        let fake = server();
        let c = client();

        let err = c
            .search_tracks(DECLARED_TOO_LARGE)
            .await
            .expect_err("an oversized body must fail");
        match err {
            Error::ResponseTooLarge { size } => {
                assert_eq!(
                    size,
                    MAX_RESPONSE_BYTES + 1,
                    "the declared size is reported"
                );
            }
            other => panic!("expected Error::ResponseTooLarge, got {other:?}"),
        }

        let attempts = logged(&fake.requests)
            .iter()
            .filter(|r| r.endpoint() == "search" && r.query() == DECLARED_TOO_LARGE)
            .count();
        assert_eq!(attempts, 1);
    }

    /// The ceiling is also enforced by counting received bytes: a chunked body
    /// declares no length at all, so the streaming count is the only bound. The
    /// test body is larger than the (test-only) ceiling on purpose.
    #[tokio::test]
    async fn a_chunked_body_is_bounded_while_it_streams() {
        let _gate = visitor_gate().await;
        let fake = server();
        let c = client();

        let err = c
            .search_tracks(CHUNKED_TOO_LARGE)
            .await
            .expect_err("an oversized chunked body must fail");
        match err {
            Error::ResponseTooLarge { size } => assert!(
                size > MAX_RESPONSE_BYTES,
                "the error must report the byte count that crossed the ceiling: {size}"
            ),
            other => panic!("expected Error::ResponseTooLarge, got {other:?}"),
        }

        let attempts = logged(&fake.requests)
            .iter()
            .filter(|r| r.endpoint() == "search" && r.query() == CHUNKED_TOO_LARGE)
            .count();
        assert_eq!(attempts, 1);
    }

    /// `playlist` takes a bare id but tolerates a `VL`-prefixed browse id: the
    /// artist page strips the prefix itself, while a copied browse id can still
    /// carry it, and a double prefix would silently send the wrong `browseId`.
    #[tokio::test]
    async fn playlist_accepts_a_vl_prefixed_id() {
        let _gate = visitor_gate().await;
        let c = client();
        let tx = tx();

        let playlist = c
            .playlist(&format!("VLPLtest_pagination_{tx}"), 3)
            .await
            .expect("playlist");
        let expected: Vec<String> = (1..=3).map(|n| format!("p{tx}-{n}")).collect();
        assert_eq!(
            playlist
                .tracks
                .iter()
                .map(|t| t.id.as_str())
                .collect::<Vec<_>>(),
            expected.iter().map(String::as_str).collect::<Vec<_>>(),
            "a single VL prefix must reach the pagination route"
        );
    }

    /// A page whose continuation token does not advance still carried real
    /// tracks. The guard exists to stop the *walk*, not to discard content the
    /// server already sent: the unchanged token must end the loop with the page's
    /// rows kept, rather than dropping the last page.
    #[tokio::test]
    async fn playlist_keeps_the_tracks_of_a_page_whose_token_stops_advancing() {
        let _gate = visitor_gate().await;
        let c = client();
        let tx = tx();

        let playlist = c
            .playlist(&format!("PLtest_same_token_{tx}"), 50)
            .await
            .expect("playlist");
        let expected = [format!("s{tx}-1"), "s-late".to_owned()];
        assert_eq!(
            playlist
                .tracks
                .iter()
                .map(|t| t.id.as_str())
                .collect::<Vec<_>>(),
            expected.iter().map(String::as_str).collect::<Vec<_>>(),
            "the page that repeated the token still contributed its track"
        );
    }
}
