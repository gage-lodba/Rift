//! Queue-mutation commands: add, insert-next, move, remove, jump, clear.

use rift_types::Track;
use tauri::{AppHandle, State};

use crate::player;
use crate::util::LockExt;
use crate::AppState;

/// Apply the result of a queue add: start playback if nothing was playing,
/// otherwise just re-broadcast the queue.
fn apply_add(outcome: player::AddOutcome, app: &AppHandle, state: &State<'_, AppState>) {
    match outcome {
        player::AddOutcome::EmitOnly => player::emit_queue(app, &state.player),
        player::AddOutcome::PlayIndex(i) => player::play_index(app, i),
    }
}

#[tauri::command(rename_all = "snake_case")]
pub fn queue_add(track: Track, app: AppHandle, state: State<'_, AppState>) {
    let outcome = {
        let mut core = state.player.core.lock_safe();
        player::append_tracks(&mut core, vec![track])
    };
    apply_add(outcome, &app, &state);
}

/// Append a whole collection (album/playlist/...) to the end of the queue.
#[tauri::command(rename_all = "snake_case")]
pub fn queue_add_tracks(tracks: Vec<Track>, app: AppHandle, state: State<'_, AppState>) {
    let outcome = {
        let mut core = state.player.core.lock_safe();
        player::append_tracks(&mut core, tracks)
    };
    apply_add(outcome, &app, &state);
}

/// Insert tracks right after the current one so they play next.
#[tauri::command(rename_all = "snake_case")]
pub fn queue_play_next(tracks: Vec<Track>, app: AppHandle, state: State<'_, AppState>) {
    let outcome = {
        let mut core = state.player.core.lock_safe();
        player::insert_next(&mut core, tracks)
    };
    apply_add(outcome, &app, &state);
}

/// Reorder the queue, moving the track at `from` to `to`.
#[tauri::command(rename_all = "snake_case")]
pub fn queue_move(from: usize, to: usize, app: AppHandle, state: State<'_, AppState>) {
    let moved = {
        let mut core = state.player.core.lock_safe();
        player::move_in_queue(&mut core, from, to)
    };
    if moved {
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
