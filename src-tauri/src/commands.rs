//! Tauri commands invoked from the Yew frontend.
//!
//! All commands use `rename_all = "snake_case"` so argument names match the
//! snake_case keys the Rust/WASM frontend serializes.

use rift_types::{
    events, AlbumPage, AlbumSummary, ArtistPage, ArtistSummary, Bootstrap, DownloadState, Library,
    PlaybackState, Playlist, Progress, Track, YtDlpStatus,
};
use rustypipe::model::{AlbumItem, AlbumType, Thumbnail, TrackItem};
use tauri::{AppHandle, Emitter, State};
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
    // Keep official YouTube Music audio tracks only — drop music videos and
    // podcast episodes so results are songs, not video versions.
    Ok(result
        .items
        .items
        .into_iter()
        .filter(|t| t.track_type.is_track())
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
    Ok(ArtistPage {
        image: thumb(&artist.header_image),
        id: artist.id,
        name: artist.name,
        description: artist.description,
        subscribers: artist.subscriber_count,
        tracks: artist.tracks.into_iter().map(convert).collect(),
        albums: artist.albums.into_iter().map(convert_album_item).collect(),
    })
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
                        let have: Vec<String> = core.queue.iter().map(|t| t.id.clone()).collect();
                        core.queue.extend(
                            paginator
                                .items
                                .into_iter()
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
    let mut lib = state.library.lock_safe();
    lib.add_to_playlist(&id, track);
    emit_library(&app, &lib.data);
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
    Bootstrap {
        library: lib.data.clone(),
        queue: snapshot(&core),
        state: core.state,
        volume: core.volume,
        discord_rpc: state.settings.lock_safe().data.discord_rpc,
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
    let downloads = state.downloads.clone();
    let player = state.player.clone();

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
