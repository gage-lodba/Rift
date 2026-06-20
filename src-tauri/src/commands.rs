//! Tauri commands invoked from the Yew frontend.
//!
//! All commands use `rename_all = "snake_case"` so argument names match the
//! snake_case keys the Rust/WASM frontend serializes.

use rift_types::{
    events, AlbumPage, AlbumSummary, ArtistPage, ArtistSummary, Bootstrap, DownloadState, Library,
    PlaybackState, Playlist, Progress, Track, YtDlpStatus,
};
use rustypipe::model::{AlbumItem, AlbumType, Thumbnail, TrackItem};
use tauri::{AppHandle, Emitter, Manager, State};
use tracing::{info, warn};

use crate::audio::AudioCmd;
use crate::player::{self, snapshot};
use crate::util::LockExt;
use crate::AppState;

fn thumb(thumbs: &[Thumbnail]) -> String {
    // Prefer the smallest thumbnail at least TARGET wide — crisp for headers
    // without pulling the full-resolution original for tiny list rows/cards.
    // Fall back to the largest available when nothing meets the target.
    const TARGET: u32 = 512;
    thumbs
        .iter()
        .filter(|t| t.width >= TARGET)
        .min_by_key(|t| t.width)
        .or_else(|| thumbs.iter().max_by_key(|t| t.width))
        .map(|t| t.url.clone())
        .unwrap_or_default()
}

fn join_artists(artists: &[rustypipe::model::ArtistId]) -> String {
    let joined = artists
        .iter()
        .map(|a| a.name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    if joined.is_empty() {
        "Unknown Artist".into()
    } else {
        joined
    }
}

fn album_type_label(t: AlbumType) -> String {
    match t {
        AlbumType::Album => "Album".into(),
        AlbumType::Ep => "EP".into(),
        AlbumType::Single => "Single".into(),
        other => format!("{other:?}"),
    }
}

fn convert_artists(artists: &[rustypipe::model::ArtistId]) -> Vec<rift_types::ArtistRef> {
    artists
        .iter()
        .map(|a| rift_types::ArtistRef {
            id: a.id.clone(),
            name: a.name.clone(),
        })
        .collect()
}

/// Heuristic: does this title look like a music-video upload rather than an
/// audio track? YouTube Music's "Songs" filter occasionally surfaces the
/// "Official Video" version of a track (typically when there's no separate
/// audio-only upload), and these come back typed as a plain track, so the
/// `TrackType` filter alone doesn't catch them.
///
/// Matches specific marker *phrases* (not the bare word "video") so legitimate
/// songs whose names happen to contain "video" — e.g. "Video Games" — aren't
/// dropped. Audio-leaning variants ("official audio", "lyric video",
/// "visualizer") are deliberately not matched.
fn looks_like_video_title(title: &str) -> bool {
    let t = title.to_lowercase();
    const MARKERS: [&str; 5] = [
        "official video",
        "music video",
        "official hd video",
        "official 4k video",
        "video clip",
    ];
    MARKERS.iter().any(|m| t.contains(m))
}

fn convert(item: TrackItem) -> Track {
    Track {
        title: item.name,
        artist: join_artists(&item.artists),
        artists: convert_artists(&item.artists),
        album: item.album.as_ref().map(|a| a.name.clone()),
        album_id: item.album.map(|a| a.id),
        duration: item.duration,
        cover: thumb(&item.cover),
        id: item.id,
    }
}

fn convert_album_item(item: AlbumItem) -> AlbumSummary {
    AlbumSummary {
        artist: join_artists(&item.artists),
        artist_id: item
            .artist_id
            .or_else(|| item.artists.iter().find_map(|a| a.id.clone())),
        name: item.name,
        cover: thumb(&item.cover),
        year: item.year,
        album_type: album_type_label(item.album_type),
        id: item.id,
    }
}

// ---------------------------------------------------------------- search

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
        .filter(|t| t.track_type.is_track() && !looks_like_video_title(&t.name))
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
        .filter(|t| t.track_type.is_track() && !looks_like_video_title(&t.name))
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

    let pl = shared
        .rp
        .query()
        .music_playlist(&pl_id)
        .await
        .map_err(|e| format!("could not load songs: {e}"))?;
    let mut songs: Vec<Track> = pl
        .tracks
        .items
        .into_iter()
        .filter(|t| t.track_type.is_track() && !looks_like_video_title(&t.name))
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

// -------------------------------------------------------------- playback

/// Replace the queue with `tracks` and start playing at `start`.
#[tauri::command(rename_all = "snake_case")]
pub async fn play_tracks(
    tracks: Vec<Track>,
    start: usize,
    source: Option<String>,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    if tracks.is_empty() || start >= tracks.len() {
        return Err("nothing to play".into());
    }
    {
        let mut core = state.player.core.lock_safe();
        core.queue = tracks;
        core.source = source;
        core.shuffle_history.clear();
        core.shuffle_cursor = 0;
        core.epoch += 1;
    }
    player::play_index(&app, start);
    Ok(())
}

/// Play a single track. With `radio`, the queue is then filled with YouTube
/// Music's related-tracks radio for endless playback.
#[tauri::command(rename_all = "snake_case")]
pub async fn play_track(
    track: Track,
    radio: bool,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let epoch = {
        let mut core = state.player.core.lock_safe();
        core.queue = vec![track.clone()];
        core.source = None;
        core.shuffle_history.clear();
        core.shuffle_cursor = 0;
        core.epoch += 1;
        core.epoch
    };
    player::play_index(&app, 0);

    if radio {
        let shared = state.player.clone();
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            match shared.rp.query().music_radio_track(&track.id).await {
                Ok(paginator) => {
                    {
                        let mut core = shared.core.lock_safe();
                        if core.epoch != epoch {
                            return; // queue was replaced meanwhile
                        }
                        let have: std::collections::HashSet<String> =
                            core.queue.iter().map(|t| t.id.clone()).collect();
                        core.queue.extend(
                            paginator
                                .items
                                .into_iter()
                                .filter(|t| {
                                    t.track_type.is_track() && !looks_like_video_title(&t.name)
                                })
                                .map(convert)
                                .filter(|t| !have.contains(&t.id)),
                        );
                    }
                    player::emit_queue(&app, &shared);
                }
                Err(e) => warn!("radio fill failed: {e}"),
            }
        });
    }
    Ok(())
}

#[tauri::command(rename_all = "snake_case")]
pub fn toggle_play(app: AppHandle) {
    player::toggle_playback(&app);
}

#[tauri::command(rename_all = "snake_case")]
pub fn next_track(app: AppHandle) {
    player::play_next(&app, true);
}

#[tauri::command(rename_all = "snake_case")]
pub fn prev_track(app: AppHandle) {
    player::play_prev(&app);
}

#[tauri::command(rename_all = "snake_case")]
pub fn seek(position: f64, app: AppHandle, state: State<'_, AppState>) {
    let (progress, pstate) = {
        let mut core = state.player.core.lock_safe();
        if core.state != PlaybackState::Playing && core.state != PlaybackState::Paused {
            return;
        }
        core.position = position.clamp(0.0, core.duration.max(0.0));
        // Re-evaluate the crossfade arm from the new position: a backward seek
        // out of the overlap window should let the crossfade arm again.
        core.crossfade_armed_for = None;
        (
            Progress {
                position: core.position,
                duration: core.duration,
            },
            core.state,
        )
    };
    let _ = state.player.audio.send(AudioCmd::Seek(progress.position));
    let _ = app.emit(events::PROGRESS, progress);
    // Re-sync Discord's elapsed/remaining bar to the new position.
    state.discord.set_state(pstate, progress.position);
}

/// Live volume change (fires continuously while the slider is dragged): updates
/// the audio thread and in-memory state but does not touch disk.
#[tauri::command(rename_all = "snake_case")]
pub fn set_volume(volume: f32, state: State<'_, AppState>) {
    let volume = volume.clamp(0.0, 1.0);
    state.player.core.lock_safe().volume = volume;
    let _ = state.player.audio.send(AudioCmd::Volume(volume));
}

/// Persist the volume (fired once when the slider is released) so it survives
/// a restart. Kept separate from [`set_volume`] to avoid a disk write per tick.
#[tauri::command(rename_all = "snake_case")]
pub fn save_volume(volume: f32, state: State<'_, AppState>) {
    state.settings.lock_safe().set_volume(volume);
}

/// Enable or disable Discord Rich Presence and persist the choice. The Discord
/// thread retains the last-known track, so toggling on immediately re-advertises
/// whatever is currently playing (and toggling off clears the presence).
#[tauri::command(rename_all = "snake_case")]
pub fn set_discord_rpc(enabled: bool, state: State<'_, AppState>) {
    state.settings.lock_safe().set_discord_rpc(enabled);
    state.discord.set_enabled(enabled);
}

/// Set the crossfade overlap (in seconds; 0 disables it) and persist it. Takes
/// effect on the next track transition.
#[tauri::command(rename_all = "snake_case")]
pub fn set_crossfade(secs: f32, state: State<'_, AppState>) {
    let clamped = state.settings.lock_safe().set_crossfade(secs);
    state.player.core.lock_safe().crossfade = clamped as f64;
}

#[tauri::command(rename_all = "snake_case")]
pub fn toggle_shuffle(app: AppHandle, state: State<'_, AppState>) {
    {
        let mut core = state.player.core.lock_safe();
        core.shuffle = !core.shuffle;
        // Anchor the shuffle history on the current track so it isn't
        // immediately repeated and Previous has a sane starting point.
        core.shuffle_history = core.current.into_iter().collect();
        core.shuffle_cursor = 0;
    }
    player::emit_queue(&app, &state.player);
}

#[tauri::command(rename_all = "snake_case")]
pub fn cycle_repeat(app: AppHandle, state: State<'_, AppState>) {
    use rift_types::RepeatMode::*;
    {
        let mut core = state.player.core.lock_safe();
        core.repeat = match core.repeat {
            Off => All,
            All => One,
            One => Off,
        };
    }
    player::emit_queue(&app, &state.player);
}

// ----------------------------------------------------------------- queue

#[tauri::command(rename_all = "snake_case")]
pub fn queue_add(track: Track, app: AppHandle, state: State<'_, AppState>) {
    let start_playing = {
        let mut core = state.player.core.lock_safe();
        if core.queue.iter().any(|t| t.id == track.id) {
            None
        } else {
            core.queue.push(track);
            // Nothing playing: start with the track we just added.
            (core.current.is_none()).then(|| core.queue.len() - 1)
        }
    };
    if let Some(i) = start_playing {
        player::play_index(&app, i);
    } else {
        player::emit_queue(&app, &state.player);
    }
}

#[tauri::command(rename_all = "snake_case")]
pub fn queue_remove(index: usize, app: AppHandle, state: State<'_, AppState>) {
    let outcome = {
        let mut core = state.player.core.lock_safe();
        player::remove_from_queue(&mut core, index)
    };
    match outcome {
        player::RemoveOutcome::None => {}
        player::RemoveOutcome::EmitOnly => player::emit_queue(&app, &state.player),
        player::RemoveOutcome::PlayIndex(i) => player::play_index(&app, i),
        player::RemoveOutcome::Stop => {
            player::stop(&app);
            player::emit_queue(&app, &state.player);
        }
    }
}

#[tauri::command(rename_all = "snake_case")]
pub fn queue_jump(index: usize, app: AppHandle) {
    player::play_index(&app, index);
}

#[tauri::command(rename_all = "snake_case")]
pub fn queue_clear(app: AppHandle, state: State<'_, AppState>) {
    {
        let mut core = state.player.core.lock_safe();
        core.queue.clear();
        core.current = None;
        core.source = None;
        core.shuffle_history.clear();
        core.shuffle_cursor = 0;
        core.epoch += 1;
    }
    player::stop(&app);
    player::emit_queue(&app, &state.player);
}

// --------------------------------------------------------------- library

fn emit_library(app: &AppHandle, library: &Library) {
    let _ = app.emit(events::LIBRARY, library);
}

#[tauri::command(rename_all = "snake_case")]
pub fn toggle_like(track: Track, app: AppHandle, state: State<'_, AppState>) {
    let mut lib = state.library.lock_safe();
    lib.toggle_like(track);
    emit_library(&app, &lib.data);
}

#[tauri::command(rename_all = "snake_case")]
pub fn create_playlist(
    name: String,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<Playlist, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("playlist name cannot be empty".into());
    }
    let mut lib = state.library.lock_safe();
    let playlist = lib.create_playlist(name.to_string());
    emit_library(&app, &lib.data);
    Ok(playlist)
}

#[tauri::command(rename_all = "snake_case")]
pub fn delete_playlist(id: String, app: AppHandle, state: State<'_, AppState>) {
    let mut lib = state.library.lock_safe();
    lib.delete_playlist(&id);
    emit_library(&app, &lib.data);
}

#[tauri::command(rename_all = "snake_case")]
pub fn rename_playlist(id: String, name: String, app: AppHandle, state: State<'_, AppState>) {
    let mut lib = state.library.lock_safe();
    lib.rename_playlist(&id, name);
    emit_library(&app, &lib.data);
}

#[tauri::command(rename_all = "snake_case")]
pub fn add_to_playlist(id: String, track: Track, app: AppHandle, state: State<'_, AppState>) {
    // A playlist whose every track is already downloaded is treated as "kept
    // offline": adding a new track should pull it down too, so the playlist
    // stays fully available offline. Evaluated before the add, and only for a
    // non-empty playlist that doesn't already contain this track.
    let keep_offline = {
        let lib = state.library.lock_safe();
        lib.data
            .playlists
            .iter()
            .find(|p| p.id == id)
            .is_some_and(|p| {
                !p.tracks.is_empty()
                    && !p.tracks.iter().any(|t| t.id == track.id)
                    && p.tracks
                        .iter()
                        .all(|t| state.downloads.is_downloaded(&t.id))
            })
    };

    {
        let mut lib = state.library.lock_safe();
        lib.add_to_playlist(&id, track.clone());
        emit_library(&app, &lib.data);
    }

    if keep_offline {
        start_downloads(
            vec![track],
            app,
            state.downloads.clone(),
            state.player.clone(),
        );
    }
}

#[tauri::command(rename_all = "snake_case")]
pub fn remove_from_playlist(
    id: String,
    track_id: String,
    app: AppHandle,
    state: State<'_, AppState>,
) {
    let mut lib = state.library.lock_safe();
    lib.remove_from_playlist(&id, &track_id);
    emit_library(&app, &lib.data);
}

// ------------------------------------------------------------- bootstrap

#[tauri::command(rename_all = "snake_case")]
pub fn bootstrap(state: State<'_, AppState>) -> Bootstrap {
    let core = state.player.core.lock_safe();
    let lib = state.library.lock_safe();
    // Read all settings under a single lock: two `lock_safe()` calls in the same
    // struct literal would both stay alive until the literal is built and
    // deadlock on the non-reentrant mutex.
    let (discord_rpc, crossfade, yt_dlp_path, update_notifications) = {
        let s = state.settings.lock_safe();
        (
            s.data.discord_rpc,
            s.data.crossfade,
            s.data.yt_dlp_path.clone(),
            s.data.update_notifications,
        )
    };
    Bootstrap {
        library: lib.data.clone(),
        queue: snapshot(&core),
        state: core.state,
        volume: core.volume,
        discord_rpc,
        crossfade,
        yt_dlp_path,
        update_notifications,
        track: core.current.and_then(|i| core.queue.get(i).cloned()),
        progress: Progress {
            position: core.position,
            duration: core.duration,
        },
        downloads: state.downloads.state(),
    }
}

// ------------------------------------------------------------- downloads

fn emit_downloads(app: &AppHandle, state: &DownloadState) {
    let _ = app.emit(events::DOWNLOADS, state);
}

/// Download a set of tracks for offline listening. Already-downloaded and
/// already-in-flight tracks are skipped; the rest are fetched sequentially with
/// progress emitted after each one.
#[tauri::command(rename_all = "snake_case")]
pub fn download_tracks(tracks: Vec<Track>, app: AppHandle, state: State<'_, AppState>) {
    start_downloads(tracks, app, state.downloads.clone(), state.player.clone());
}

/// Spawn a background job that downloads `tracks`, skipping ones already on disk
/// or in flight. Shared by the explicit Download action and the automatic
/// "keep a fully-downloaded playlist offline" path.
fn start_downloads(
    tracks: Vec<Track>,
    app: AppHandle,
    downloads: std::sync::Arc<crate::downloads::Downloads>,
    player: std::sync::Arc<crate::player::PlayerShared>,
) {
    // `begin` atomically claims each id and returns false if one was already in
    // flight, so concurrent or repeated calls never fetch the same track twice.
    let pending: Vec<Track> = tracks
        .into_iter()
        .filter(|t| !downloads.is_downloaded(&t.id) && downloads.begin(&t.id))
        .collect();
    if pending.is_empty() {
        return;
    }
    emit_downloads(&app, &downloads.state());

    tauri::async_runtime::spawn(async move {
        for track in pending {
            let dest = downloads.path(&track.id);
            match rift::fetch::fetch_bytes(&player.rp, &player.http, &track.id).await {
                Ok((data, _)) => match tokio::fs::write(&dest, &data).await {
                    Ok(()) => downloads.finish(&track.id),
                    Err(e) => {
                        warn!("could not write download {}: {e}", track.id);
                        downloads.fail(&track.id);
                        let _ = app.emit(
                            events::ERROR,
                            format!("Could not save \u{201c}{}\u{201d}: {e}", track.title),
                        );
                    }
                },
                Err(e) => {
                    warn!("download failed for {}: {e:#}", track.id);
                    downloads.fail(&track.id);
                    let _ = app.emit(
                        events::ERROR,
                        format!("Could not download \u{201c}{}\u{201d}: {e:#}", track.title),
                    );
                }
            }
            emit_downloads(&app, &downloads.state());
        }
    });
}

/// Remove offline copies of the given tracks.
#[tauri::command(rename_all = "snake_case")]
pub fn remove_downloads(ids: Vec<String>, app: AppHandle, state: State<'_, AppState>) {
    for id in &ids {
        state.downloads.remove(id);
    }
    emit_downloads(&app, &state.downloads.state());
}

// ------------------------------------------------------------- diagnostics

/// Probe the system for yt-dlp, the load-bearing streaming fallback. Used by
/// the Settings view to confirm playback will work.
#[tauri::command(rename_all = "snake_case")]
pub async fn check_ytdlp() -> YtDlpStatus {
    rift::fetch::detect_ytdlp().await
}

/// Set (or clear, with a blank value) a custom yt-dlp location, persist it,
/// apply it immediately, and re-probe so the caller sees the new status.
#[tauri::command(rename_all = "snake_case")]
pub async fn set_yt_dlp_path(
    path: Option<String>,
    state: State<'_, AppState>,
) -> Result<YtDlpStatus, String> {
    let configured = {
        let mut settings = state.settings.lock_safe();
        settings.set_yt_dlp_path(path);
        settings.data.yt_dlp_path.clone()
    };
    rift::fetch::set_ytdlp_override(configured.map(std::path::PathBuf::from));
    Ok(rift::fetch::detect_ytdlp().await)
}

/// Download yt-dlp for the current platform into the app data dir, set it as the
/// active binary, and re-probe. Used by Settings when yt-dlp isn't found.
#[tauri::command(rename_all = "snake_case")]
pub async fn download_ytdlp(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<YtDlpStatus, String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("no app data dir: {e}"))?
        .join("bin");
    let http = state.player.http.clone();
    match rift::fetch::download_ytdlp(&dir, &http).await {
        Ok(path) => {
            let path_str = path.to_string_lossy().to_string();
            state.settings.lock_safe().set_yt_dlp_path(Some(path_str));
            rift::fetch::set_ytdlp_override(Some(path));
        }
        Err(e) => {
            let msg = format!("Could not download yt-dlp: {e:#}");
            warn!("{msg}");
            let _ = app.emit(events::ERROR, msg.clone());
            return Err(msg);
        }
    }
    Ok(rift::fetch::detect_ytdlp().await)
}

// ------------------------------------------------------------- updates

/// GitHub repo (owner/name) whose releases back the update check.
const RELEASE_REPO: &str = "gage-lodba/Rift";

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

/// Check GitHub for a newer Rift release. Compares the running version against
/// the latest published release's tag; surfaced in Settings. Network/parse
/// failures degrade to "latest unknown" (still reporting the running version)
/// rather than erroring, so the UI can always show the current version.
#[tauri::command(rename_all = "snake_case")]
pub async fn check_update(state: State<'_, AppState>) -> Result<rift_types::UpdateStatus, String> {
    let current = env!("CARGO_PKG_VERSION").to_string();
    let mut status = rift_types::UpdateStatus {
        current: current.clone(),
        ..Default::default()
    };

    let url = format!("https://api.github.com/repos/{RELEASE_REPO}/releases/latest");
    let json: Option<serde_json::Value> = async {
        state
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
            .ok()?
            .json()
            .await
            .ok()
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
            .map(|l| is_newer(l, &current))
            .unwrap_or(false);
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
#[tauri::command(rename_all = "snake_case")]
pub fn set_update_notifications(enabled: bool, state: State<'_, AppState>) {
    state.settings.lock_safe().set_update_notifications(enabled);
}

/// Open an http(s) URL in the user's default browser. Used by the "Download"
/// action on an available update (the webview itself shouldn't navigate away).
#[tauri::command(rename_all = "snake_case")]
pub fn open_url(url: String) -> Result<(), String> {
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err("refusing to open non-http(s) url".into());
    }
    #[cfg(target_os = "linux")]
    let mut cmd = {
        let mut c = std::process::Command::new("xdg-open");
        c.arg(&url);
        c
    };
    #[cfg(target_os = "macos")]
    let mut cmd = {
        let mut c = std::process::Command::new("open");
        c.arg(&url);
        c
    };
    #[cfg(windows)]
    let mut cmd = {
        use std::os::windows::process::CommandExt;
        // `start` is a cmd builtin; the empty "" is its window-title argument.
        let mut c = std::process::Command::new("cmd");
        c.args(["/C", "start", "", &url]);
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        c
    };
    cmd.spawn()
        .map(|_| ())
        .map_err(|e| format!("could not open browser: {e}"))
}

// ---------------------------------------------------------------- window

#[tauri::command(rename_all = "snake_case")]
pub fn window_minimize(window: tauri::Window) {
    let _ = window.minimize();
}

#[tauri::command(rename_all = "snake_case")]
pub fn window_toggle_maximize(window: tauri::Window) {
    if window.is_maximized().unwrap_or(false) {
        let _ = window.unmaximize();
    } else {
        let _ = window.maximize();
    }
}

#[tauri::command(rename_all = "snake_case")]
pub fn window_close(window: tauri::Window) {
    let _ = window.close();
}

#[cfg(test)]
mod tests {
    use super::{is_newer, looks_like_video_title};

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

    #[test]
    fn flags_official_video_uploads() {
        for t in [
            "Song Title (Official Video)",
            "Song Title (Official Music Video)",
            "Song Title [Music Video]",
            "Song Title (Official HD Video)",
            "Artist - Song (Video Clip)",
        ] {
            assert!(looks_like_video_title(t), "should flag: {t}");
        }
    }

    #[test]
    fn keeps_audio_tracks_and_video_named_songs() {
        for t in [
            "Video Games",                 // a real song name, not a video upload
            "Song Title (Official Audio)", // audio variant
            "Song Title (Lyric Video)",    // lyric videos are audio-leaning
            "Song Title (Visualizer)",
            "Plain Song Title",
        ] {
            assert!(!looks_like_video_title(t), "should keep: {t}");
        }
    }
}
