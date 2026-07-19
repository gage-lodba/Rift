//! Search and catalog-browsing commands (tracks, artists, albums).

use rift_types::{AlbumPage, AlbumSummary, ArtistPage, ArtistSummary, Track};
use tauri::State;
use tracing::{info, warn};

use super::convert::{
    album_type_label, convert, convert_album_item, convert_artists, is_audio_track, join_artists,
    thumb, MAX_FETCH_TRACKS,
};
use crate::AppState;

#[tauri::command(rename_all = "snake_case")]
pub async fn search(query: String, state: State<'_, AppState>) -> Result<Vec<Track>, String> {
    info!("searching: {query}");
    let result = state
        .player
        .rp
        .query()
        .music_search_tracks(&query)
        .await
        .map_err(|e| format!("search failed: {e}"))?;
    // Keep official YouTube Music audio tracks only: drop music videos and
    // podcast episodes by type, then also drop entries that slip through typed
    // as tracks but are titled as the "Official Video" upload.
    Ok(result
        .items
        .items
        .into_iter()
        .filter(is_audio_track)
        .map(convert)
        .collect())
}

#[tauri::command(rename_all = "snake_case")]
pub async fn search_artists(
    query: String,
    state: State<'_, AppState>,
) -> Result<Vec<ArtistSummary>, String> {
    let result = state
        .player
        .rp
        .query()
        .music_search_artists(&query)
        .await
        .map_err(|e| format!("artist search failed: {e}"))?;
    Ok(result
        .items
        .items
        .into_iter()
        .map(|a| ArtistSummary {
            avatar: thumb(&a.avatar),
            id: a.id,
            name: a.name,
            subscribers: a.subscriber_count,
        })
        .collect())
}

#[tauri::command(rename_all = "snake_case")]
pub async fn search_albums(
    query: String,
    state: State<'_, AppState>,
) -> Result<Vec<AlbumSummary>, String> {
    let result = state
        .player
        .rp
        .query()
        .music_search_albums(&query)
        .await
        .map_err(|e| format!("album search failed: {e}"))?;
    Ok(result
        .items
        .items
        .into_iter()
        .map(convert_album_item)
        .collect())
}

#[tauri::command(rename_all = "snake_case")]
pub async fn get_artist(id: String, state: State<'_, AppState>) -> Result<ArtistPage, String> {
    let artist = state
        .player
        .rp
        .query()
        .music_artist(&id, false)
        .await
        .map_err(|e| format!("could not load artist: {e}"))?;

    // Drop "Official Video" uploads so Top songs shows audio tracks, not the
    // duplicate music-video versions (which the artist endpoint mixes in).
    let mut tracks: Vec<Track> = artist
        .tracks
        .into_iter()
        .filter(is_audio_track)
        .map(convert)
        .collect();

    // Artist-page top tracks come back without durations (rustypipe only
    // populates `duration` from search/album/playlist endpoints). Backfill them
    // from the artist's tracks playlist, which does carry durations.
    if tracks.iter().any(|t| t.duration.is_none()) {
        if let Some(pl_id) = &artist.tracks_playlist_id {
            if let Ok(pl) = state.player.rp.query().music_playlist(pl_id).await {
                let durations: std::collections::HashMap<String, u32> = pl
                    .tracks
                    .items
                    .into_iter()
                    .filter_map(|t| t.duration.map(|d| (t.id, d)))
                    .collect();
                for t in tracks.iter_mut().filter(|t| t.duration.is_none()) {
                    t.duration = durations.get(&t.id).copied();
                }
            }
        }
    }

    Ok(ArtistPage {
        image: thumb(&artist.header_image),
        id: artist.id,
        name: artist.name,
        description: artist.description,
        subscribers: artist.subscriber_count,
        tracks,
        albums: artist.albums.into_iter().map(convert_album_item).collect(),
        tracks_playlist_id: artist.tracks_playlist_id,
    })
}

/// Load an artist's full song catalog (the list behind "Show all songs"),
/// filtered the same way as the artist page so it stays audio-tracks-only.
///
/// The catalog playlist YouTube Music exposes carries no per-track album, so we
/// rebuild it in two cheap-to-expensive passes:
///   1. Fetch the artist's albums concurrently and map each track ID to its
///      album — one album page covers ~a dozen songs, so this resolves the bulk.
///   2. For the stragglers (singles, collabs where the artist is featured,
///      soundtrack/bonus cuts whose IDs don't match an album page) fall back to
///      per-track `music_details`, which always reports an album.
#[tauri::command(rename_all = "snake_case")]
pub async fn get_artist_songs(
    id: String,
    state: State<'_, AppState>,
) -> Result<Vec<Track>, String> {
    let shared = state.player.clone();
    let artist = shared
        .rp
        .query()
        .music_artist(&id, false)
        .await
        .map_err(|e| format!("could not load artist: {e}"))?;
    let pl_id = artist
        .tracks_playlist_id
        .ok_or_else(|| "artist has no songs list".to_string())?;

    let q = shared.rp.query();
    let mut pl = q
        .music_playlist(&pl_id)
        .await
        .map_err(|e| format!("could not load songs: {e}"))?;
    // Pull past the first page so prolific artists' catalogs aren't truncated.
    if let Err(e) = pl.tracks.extend_limit(&q, MAX_FETCH_TRACKS).await {
        warn!("catalog continuation failed; showing a partial list: {e}");
    }
    let mut songs: Vec<Track> = pl
        .tracks
        .items
        .into_iter()
        .filter(is_audio_track)
        .map(convert)
        .collect();

    let mut album_of: std::collections::HashMap<String, (String, String)> =
        std::collections::HashMap::new();
    // Seed from the artist's top tracks, which already carry albums — catches
    // singles/EPs that aren't in the main albums list.
    for t in &artist.tracks {
        if let Some(al) = &t.album {
            album_of
                .entry(t.id.clone())
                .or_insert_with(|| (al.name.clone(), al.id.clone()));
        }
    }

    // Pass 1: fetch each album page and index track ID -> (album, id). Bounded
    // so we don't fire dozens of requests at once (YouTube rate-limits, which
    // would silently drop album coverage).
    let album_ids = artist.albums.into_iter().map(|a| a.id);
    bounded_for_each(album_ids, |id| {
        let shared = shared.clone();
        async move { shared.rp.query().music_album(&id).await.ok() }
    })
    .await
    .into_iter()
    .flatten()
    .for_each(|al| {
        for t in al.tracks {
            album_of
                .entry(t.id)
                .or_insert_with(|| (al.name.clone(), al.id.clone()));
        }
    });
    for s in songs.iter_mut().filter(|s| s.album.is_none()) {
        if let Some((name, album_id)) = album_of.get(&s.id) {
            s.album = Some(name.clone());
            s.album_id = Some(album_id.clone());
        }
    }

    // Pass 2: resolve whatever the album pages didn't cover via per-track
    // details (one request each, only for the songs still missing an album),
    // bounded the same way.
    let missing: Vec<String> = songs
        .iter()
        .filter(|s| s.album.is_none())
        .map(|s| s.id.clone())
        .collect();
    bounded_for_each(missing, |id| {
        let shared = shared.clone();
        async move {
            let album = shared
                .rp
                .query()
                .music_details(&id)
                .await
                .ok()
                .and_then(|d| d.track.album);
            album.map(|al| (id, al.name, al.id))
        }
    })
    .await
    .into_iter()
    .flatten()
    .for_each(|(id, name, album_id)| {
        if let Some(s) = songs.iter_mut().find(|s| s.id == id && s.album.is_none()) {
            s.album = Some(name);
            s.album_id = Some(album_id);
        }
    });

    Ok(songs)
}

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

#[tauri::command(rename_all = "snake_case")]
pub async fn get_album(id: String, state: State<'_, AppState>) -> Result<AlbumPage, String> {
    let album = state
        .player
        .rp
        .query()
        .music_album(&id)
        .await
        .map_err(|e| format!("could not load album: {e}"))?;

    let cover = thumb(&album.cover);
    let album_artist = join_artists(&album.artists);
    let artist_id = album
        .artist_id
        .clone()
        .or_else(|| album.artists.iter().find_map(|a| a.id.clone()));

    // Album track items often omit the cover (and sometimes artists);
    // inherit them from the album.
    let tracks = album
        .tracks
        .into_iter()
        .map(|item| {
            let mut t = convert(item);
            if t.cover.is_empty() {
                t.cover = cover.clone();
            }
            if t.artists.is_empty() && !album.by_va {
                t.artist = album_artist.clone();
                t.artists = convert_artists(&album.artists);
            }
            if t.album.is_none() {
                t.album = Some(album.name.clone());
                t.album_id = Some(album.id.clone());
            }
            t
        })
        .collect();

    Ok(AlbumPage {
        id: album.id,
        name: album.name,
        cover,
        artist: album_artist,
        artist_id,
        year: album.year,
        album_type: album_type_label(album.album_type),
        tracks,
    })
}
