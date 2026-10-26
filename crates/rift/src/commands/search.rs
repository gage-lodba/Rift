//! Search and catalog-browsing commands: tracks, artists, albums.

use std::collections::HashMap;

use rift_types::{AlbumPage, AlbumSummary, ArtistPage, ArtistSummary, Track};
use tracing::info;

use super::convert::{is_audio_track, MAX_FETCH_TRACKS};
use crate::backend::BackendHandle;

impl BackendHandle {
    /// Search `YouTube` Music for songs, keeping audio tracks only: drops
    /// "Official Video" uploads that slip through typed as plain tracks.
    ///
    /// Network I/O: fetch the future in a task (`backend.spawn` / the app's
    /// controller). Never call this on the UI thread.
    ///
    /// # Errors
    ///
    /// Returns `"search failed: …"` when the innertube request or its parse
    /// fails (offline, blocked, or an unexpected response shape).
    pub async fn search(&self, query: String) -> Result<Vec<Track>, String> {
        info!("searching: {query}");
        let tracks = self
            .inner
            .player
            .yt
            .search_tracks(&query)
            .await
            .map_err(|e| format!("search failed: {e}"))?;
        Ok(tracks.into_iter().filter(is_audio_track).collect())
    }

    /// Search for artists.
    ///
    /// # Errors
    ///
    /// Returns `"artist search failed: …"` when the innertube request or its
    /// parse fails.
    pub async fn search_artists(&self, query: String) -> Result<Vec<ArtistSummary>, String> {
        self.inner
            .player
            .yt
            .search_artists(&query)
            .await
            .map_err(|e| format!("artist search failed: {e}"))
    }

    /// Search for albums.
    ///
    /// # Errors
    ///
    /// Returns `"album search failed: …"` when the innertube request or its
    /// parse fails.
    pub async fn search_albums(&self, query: String) -> Result<Vec<AlbumSummary>, String> {
        self.inner
            .player
            .yt
            .search_albums(&query)
            .await
            .map_err(|e| format!("album search failed: {e}"))
    }

    /// Load an artist page, keeping only audio tracks.
    ///
    /// # Errors
    ///
    /// Returns `"could not load artist: …"` when the innertube request or its
    /// parse fails.
    pub async fn get_artist(&self, id: String) -> Result<ArtistPage, String> {
        let mut page = self
            .inner
            .player
            .yt
            .artist(&id)
            .await
            .map_err(|e| format!("could not load artist: {e}"))?;
        page.tracks.retain(is_audio_track);
        Ok(page)
    }

    /// Load an artist's full song catalog (the "Show all songs" view), filtered
    /// audio-only. Album attribution is backfilled in two passes.
    ///
    /// # Errors
    ///
    /// Returns `"could not load artist: …"` / `"could not load songs: …"` when a
    /// request or parse fails, and `"artist has no songs list"` when the artist
    /// page carries no songs playlist id. The album/credits backfill passes are
    /// best-effort and simply leave `album` unset on failure.
    pub async fn get_artist_songs(&self, id: String) -> Result<Vec<Track>, String> {
        let page = self
            .inner
            .player
            .yt
            .artist(&id)
            .await
            .map_err(|e| format!("could not load artist: {e}"))?;
        let pl_id = page
            .tracks_playlist_id
            .ok_or_else(|| "artist has no songs list".to_string())?;
        let mut songs = self
            .inner
            .player
            .yt
            .playlist(&pl_id, MAX_FETCH_TRACKS)
            .await
            .map_err(|e| format!("could not load songs: {e}"))?
            .tracks;
        songs.retain(is_audio_track);

        // Pass 1: album pages. The artist page lists their albums, and one album
        // page covers ~a dozen songs, so this attributes most of the catalog for
        // a handful of requests.
        let mut album_of: HashMap<String, (String, String)> = HashMap::new();
        let album_ids: Vec<String> = page.albums.iter().map(|a| a.id.clone()).collect();
        let pages = bounded_for_each(album_ids, |id| {
            let yt = self.inner.player.yt.clone();
            async move { yt.album(&id).await.ok() }
        })
        .await;
        for album in pages.into_iter().flatten() {
            for t in &album.tracks {
                album_of
                    .entry(t.id.clone())
                    .or_insert_with(|| (album.name.clone(), album.id.clone()));
            }
        }
        for s in songs.iter_mut().filter(|s| s.album.is_none()) {
            if let Some((name, album_id)) = album_of.get(&s.id) {
                s.album = Some(name.clone());
                s.album_id = Some(album_id.clone());
            }
        }

        // Pass 2: ask the catalog itself about whatever is still unattributed —
        // singles, features, and anything else that no album page lists. Each
        // straggler costs one `/next` request, so this is bounded; the remainder
        // keeps a `None` album, which the UI already renders without one.
        let stragglers: Vec<(usize, String)> = songs
            .iter()
            .enumerate()
            .filter(|(_, s)| s.album.is_none())
            .map(|(i, s)| (i, s.id.clone()))
            .take(MAX_CREDIT_LOOKUPS)
            .collect();
        if !stragglers.is_empty() {
            info!(
                "backfilling album for {} song(s) via track credits",
                stragglers.len()
            );
        }
        let credits = bounded_for_each(stragglers, |(index, video_id)| {
            let yt = self.inner.player.yt.clone();
            async move { (index, yt.track_credits(&video_id).await.ok()) }
        })
        .await;
        for (index, credit) in credits {
            let Some((artists, album, album_id)) = credit else {
                continue;
            };
            let Some(song) = songs.get_mut(index) else {
                continue;
            };
            song.album = album;
            song.album_id = album_id;
            // A playlist row carries one flattened artist string; the credits
            // carry each artist as a linkable reference, which is what the UI
            // needs to offer per-artist navigation.
            if !artists.is_empty() {
                song.artists = artists;
            }
        }

        Ok(songs)
    }

    /// Load an album page, keeping only audio tracks.
    ///
    /// # Errors
    ///
    /// Returns `"could not load album: …"` when the innertube request or its
    /// parse fails.
    pub async fn get_album(&self, id: String) -> Result<AlbumPage, String> {
        let mut page = self
            .inner
            .player
            .yt
            .album(&id)
            .await
            .map_err(|e| format!("could not load album: {e}"))?;
        page.tracks.retain(is_audio_track);
        Ok(page)
    }
}

/// Cap on per-track credits lookups when backfilling an artist's catalog. Each
/// lookup is one request, so this bounds the worst case for an artist whose
/// back catalogue mostly misses their own album pages.
const MAX_CREDIT_LOOKUPS: usize = 40;

/// Run an async `task` over each item with at most [`MAX_INFLIGHT`] in flight at
/// once, collecting the results (order not preserved). Keeps fan-out network
/// work from overwhelming the upstream API.
async fn bounded_for_each<I, T, F, Fut>(items: I, task: F) -> Vec<T>
where
    I: IntoIterator,
    F: Fn(I::Item) -> Fut,
    Fut: std::future::Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    const MAX_INFLIGHT: usize = 8;
    let mut set = tokio::task::JoinSet::new();
    let mut out = Vec::new();
    let mut items = items.into_iter();
    // Prime up to the limit, then replace each finished task with the next.
    for item in items.by_ref().take(MAX_INFLIGHT) {
        set.spawn(task(item));
    }
    while let Some(res) = set.join_next().await {
        if let Ok(v) = res {
            out.push(v);
        }
        if let Some(item) = items.next() {
            set.spawn(task(item));
        }
    }
    out
}
