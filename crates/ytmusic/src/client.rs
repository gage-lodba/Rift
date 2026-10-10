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

/// A YouTube Music innertube client.
///
/// Cheap to clone: shares the HTTP client, cached client version, and
/// visitor-data pool behind an `Arc`.
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

    /// Search YouTube Music for audio tracks.
    pub async fn search_tracks(&self, query: &str) -> Result<Vec<Track>> {
        let resp = self.search_raw(SearchFilter::Tracks, query).await?;
        Ok(parse::tracks(&resp))
    }

    /// Search YouTube Music for artists.
    pub async fn search_artists(&self, query: &str) -> Result<Vec<ArtistSummary>> {
        let resp = self.search_raw(SearchFilter::Artists, query).await?;
        Ok(parse::artists(&resp))
    }

    /// Search YouTube Music for albums.
    pub async fn search_albums(&self, query: &str) -> Result<Vec<AlbumSummary>> {
        let resp = self.search_raw(SearchFilter::Albums, query).await?;
        Ok(parse::albums(&resp))
    }

    /// Fetch a full album page (header + track list) by its browse id.
    pub async fn album(&self, browse_id: &str) -> Result<AlbumPage> {
        let resp = self.browse_raw(browse_id).await?;
        parse::album_page(&resp, browse_id)
            .ok_or_else(|| Error::Parse("could not parse album page".to_owned()))
    }

    /// Fetch a full artist page (header + top songs + albums) by its browse id.
    pub async fn artist(&self, browse_id: &str) -> Result<ArtistPage> {
        let resp = self.browse_raw(browse_id).await?;
        parse::artist_page(&resp, browse_id)
            .ok_or_else(|| Error::Parse("could not parse artist page".to_owned()))
    }

    /// Fetch a playlist's name and tracks, following continuations up to `limit`.
    pub async fn playlist(&self, playlist_id: &str, limit: usize) -> Result<Playlist> {
        let mut tracks = Vec::new();

        let first = self.browse_raw(&format!("VL{playlist_id}")).await?;
        let (name_opt, mut t, mut continuation) = parse::playlist_page(&first);
        let name = name_opt.unwrap_or_default();
        tracks.append(&mut t);

        while tracks.len() < limit {
            let Some(token) = continuation else { break };
            let next = self
                .post("browse", json!({ "continuation": token }))
                .await?;
            let (_, mut t, c) = parse::playlist_page(&next);
            // Guard against a page that yields no tracks but still carries a
            // continuation (would otherwise spin without progress).
            if t.is_empty() {
                break;
            }
            tracks.append(&mut t);
            continuation = c;
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
    /// Retries once after a 403, rotating the visitor id — YouTube occasionally
    /// rejects a stale or flagged visitor data.
    async fn post(&self, endpoint: &str, extra: Value) -> Result<Value> {
        for _ in 0..2 {
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
            let text = resp.text().await?;
            let parsed: Value = serde_json::from_str(&text)
                .map_err(|e| Error::Parse(format!("response was not JSON: {e}")))?;

            if status == reqwest::StatusCode::FORBIDDEN {
                identity::ban(&vdata);
                continue;
            }
            if !status.is_success() {
                let message = parsed
                    .get("error")
                    .and_then(|e| e.get("message"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                return Err(Error::Api {
                    status: status.as_u16(),
                    message,
                });
            }
            return Ok(parsed);
        }
        Err(Error::Api {
            status: 403,
            message: "forbidden after rotating visitor data".to_owned(),
        })
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

    #[derive(Clone)]
    struct Request {
        method: String,
        path: String,
        headers: HashMap<String, String>,
        body: Value,
    }

    impl Request {
        fn header(&self, name: &str) -> &str {
            self.headers.get(name).map(String::as_str).unwrap_or("")
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
    }

    impl Response {
        fn json(body: Value) -> Self {
            Self {
                status: 200,
                headers: Vec::new(),
                body: body.to_string(),
            }
        }
        fn error(status: u16) -> Self {
            Self {
                status,
                headers: Vec::new(),
                body: json!({ "error": { "message": "rejected by the test server" } }).to_string(),
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
            .unwrap_or_else(|p| p.into_inner())
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
            .unwrap_or_else(|p| p.into_inner())
            .push(request.clone());

        let response = match routes.get(&route) {
            Some(handler) => handler(&request),
            None => Response {
                status: 500,
                headers: Vec::new(),
                body: json!({ "error": { "message": format!("no route for {route}") } })
                    .to_string(),
            },
        };

        let reason = match response.status {
            200 => "OK",
            403 => "Forbidden",
            429 => "Too Many Requests",
            _ => "Error",
        };
        let mut head = format!(
            "HTTP/1.1 {} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",
            response.status,
            response.body.len()
        );
        for (name, value) in &response.headers {
            head.push_str(&format!("{name}: {value}\r\n"));
        }
        head.push_str("\r\n");
        stream.write_all(head.as_bytes())?;
        stream.write_all(response.body.as_bytes())?;
        stream.flush()
    }

    /// The version the fake home page advertises; asserted on the innertube
    /// headers, so a change here is visible in one place.
    const CLIENT_VERSION: &str = "1.20250901.00.00";

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
                Box::new(|_: &Request| Response {
                    status: 200,
                    // `identity::fetch` reads visitor data from this cookie
                    // before scraping the HTML, so it is what fills the pool.
                    headers: vec![(
                        "Set-Cookie".to_owned(),
                        format!("__Secure-YEC={HOME_VISITOR}; Path=/; Secure"),
                    )],
                    body: home_html(),
                }),
            );

            routes.insert(
                "/youtubei/v1/search".to_owned(),
                Box::new(|req: &Request| match req.query() {
                    ALWAYS_429 => Response::error(429),
                    ALWAYS_403 => Response::error(403),
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

                    // A page that offers a continuation but yields no rows. The
                    // same route serves the continuation, so a regression in the
                    // guard would loop rather than quietly pass.
                    if id.contains("degenerate") || token == "loop" {
                        return Response::json(playlist_page("Degenerate", Vec::new(), Some("loop")));
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
                    .map(|t| t.len())
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

    #[tokio::test]
    async fn playlist_pagination_stops_when_a_page_yields_no_tracks() {
        let _gate = visitor_gate().await;
        let c = client();
        let playlist = c.playlist("PLtest_degenerate", 50).await.expect("playlist");
        assert!(
            playlist.tracks.is_empty(),
            "a token with no rows must stop the walk, not spin: {:?}",
            playlist.tracks
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
}
