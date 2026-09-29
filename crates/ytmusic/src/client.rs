//! High-level client: request transport + typed metadata endpoints.

use std::sync::Arc;

use reqwest::header;
use rift_types::{AlbumPage, AlbumSummary, ArtistPage, ArtistRef, ArtistSummary, Playlist, Track};
use serde_json::{json, Value};

use crate::context::{self, CLIENT_NAME_ID, HOME};
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
    pub async fn track_credits(
        &self,
        video_id: &str,
    ) -> Result<(Vec<ArtistRef>, Option<String>, Option<String>)> {
        let resp = self.next_raw(video_id).await?;
        parse::track_credits(&resp)
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

            let resp = self
                .inner
                .http
                .post(format!("{HOME}/youtubei/v1/{endpoint}?prettyPrint=false"))
                .header(header::CONTENT_TYPE, "application/json")
                .header("X-YouTube-Client-Name", CLIENT_NAME_ID)
                .header("X-YouTube-Client-Version", &version)
                .header("X-Goog-EOM-Visitor-Id", &vdata)
                .header(header::ORIGIN, HOME)
                .header(header::REFERER, HOME)
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
