//! Playback control commands: load/play tracks, transport, volume, shuffle,
//! repeat, and related persisted settings.

use rift_types::{events, PlaybackState, Progress, Track};
use tauri::{AppHandle, Emitter, State};
use tracing::warn;

use super::convert::{convert, is_audio_track};
use crate::audio::AudioCmd;
use crate::player;
use crate::util::LockExt;
use crate::AppState;

/// Replace the queue with `tracks` and start playing at `start`.
#[tauri::command(rename_all = "snake_case")]
pub async fn play_tracks(
    tracks: Vec<Track>,
    start: usize,
    source: Option<String>,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    // Preview rows carry placeholder ids that can't be fetched; ignore so a
    // screenshot session isn't interrupted by failed-fetch errors.
    if crate::settings::preview_mode() {
        return Ok(());
    }
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
    // Preview rows carry placeholder ids that can't be fetched; ignore so a
    // screenshot session isn't interrupted by failed-fetch errors.
    if crate::settings::preview_mode() {
        return Ok(());
    }
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
                                .filter(is_audio_track)
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
