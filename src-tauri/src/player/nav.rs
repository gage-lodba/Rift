//! Queue navigation: choosing the next/previous index (shuffle-aware) and the
//! play/pause, next, previous, and stop controls that act on those picks.

use rift_types::{PlaybackState, RepeatMode};
use tauri::{AppHandle, Manager};

use super::playback::{play_index, start_playback};
use super::state::{emit_state, PlayerCore};
use crate::audio::AudioCmd;
use crate::util::LockExt;
use crate::AppState;

fn rand_index(len: usize) -> usize {
    (rand::random::<u64>() as usize) % len
}

/// Decide the next queue index and advance the shuffle history. `manual` marks
/// a user-initiated Next (wraps at the end and overrides repeat-one); automatic
/// advancement respects repeat mode.
///
/// Under shuffle: if Previous stepped the cursor back into the history, Next
/// walks forward through that existing order; otherwise a track not yet played
/// this cycle is drawn at random and appended, so every track plays once before
/// any repeats.
pub(crate) fn pick_next(core: &mut PlayerCore, manual: bool) -> Option<usize> {
    let len = core.queue.len();
    let cur = core.current?;
    if len == 0 {
        return None;
    }
    // A crossfade may have already drawn the next track (advancing the shuffle
    // bookkeeping as it did). Honour that pick so it isn't drawn twice.
    if let Some(i) = core.pending_next.take() {
        if i < len {
            return Some(i);
        }
    }
    if !manual && core.repeat == RepeatMode::One {
        return Some(cur);
    }
    if core.shuffle && len > 1 {
        // Previous left the cursor behind the front: replay forward.
        if core.shuffle_cursor + 1 < core.shuffle_history.len() {
            core.shuffle_cursor += 1;
            return Some(core.shuffle_history[core.shuffle_cursor]);
        }
        let pool: Vec<usize> = (0..len)
            .filter(|i| !core.shuffle_history.contains(i))
            .collect();
        let pick = if pool.is_empty() {
            // Every track has played this cycle.
            if !manual && core.repeat != RepeatMode::All {
                return None;
            }
            // Start a fresh cycle, keeping the current track out of the running
            // so it isn't repeated back-to-back.
            core.shuffle_history.clear();
            let fresh: Vec<usize> = (0..len).filter(|i| *i != cur).collect();
            fresh[rand_index(fresh.len())]
        } else {
            pool[rand_index(pool.len())]
        };
        core.shuffle_history.push(pick);
        core.shuffle_cursor = core.shuffle_history.len() - 1;
        return Some(pick);
    }
    if cur + 1 < len {
        Some(cur + 1)
    } else if manual || core.repeat == RepeatMode::All {
        Some(0)
    } else {
        None
    }
}

/// Toggle play/pause, resume a stopped track, or cancel an in-flight load.
/// Shared by the `toggle_play` command and the OS media keys.
pub fn toggle_playback(app: &AppHandle) {
    let state = app.state::<AppState>();
    enum Action {
        None,
        Emit(PlaybackState),
        Replay(usize),
        Cancel,
    }
    let action = {
        let mut core = state.player.core.lock_safe();
        match core.state {
            PlaybackState::Playing => {
                let _ = state.player.audio.send(AudioCmd::Pause);
                core.state = PlaybackState::Paused;
                Action::Emit(PlaybackState::Paused)
            }
            PlaybackState::Paused => {
                let _ = state.player.audio.send(AudioCmd::Resume);
                core.state = PlaybackState::Playing;
                Action::Emit(PlaybackState::Playing)
            }
            PlaybackState::Stopped => match core.current {
                Some(i) => Action::Replay(i),
                None => Action::None,
            },
            // Cancel an in-flight load: bump the generation so the pending
            // fetch is discarded when it lands, and stop.
            PlaybackState::Loading => {
                core.generation += 1;
                core.state = PlaybackState::Stopped;
                core.position = 0.0;
                Action::Cancel
            }
        }
    };
    match action {
        Action::Emit(s) => emit_state(app, s),
        Action::Replay(i) => play_index(app, i),
        Action::Cancel => {
            let _ = state.player.audio.send(AudioCmd::Stop);
            emit_state(app, PlaybackState::Stopped);
        }
        Action::None => {}
    }
}

pub fn play_next(app: &AppHandle, manual: bool) {
    let state = app.state::<AppState>();
    let shared = state.player.clone();
    let next = {
        let mut core = shared.core.lock_safe();
        pick_next(&mut core, manual)
    };
    match next {
        Some(i) => start_playback(app, i),
        None => stop(app),
    }
}

/// Pick the previous queue index, mirroring [`pick_next`]. Near the start of a
/// track — or at the very front of the shuffle history — this returns the
/// current track to restart it. Under shuffle it steps back through the actual
/// playback history rather than queue order (mutating `shuffle_cursor`), so a
/// following Next replays the track you came from.
pub(crate) fn pick_prev(core: &mut PlayerCore) -> Option<usize> {
    let cur = core.current?;
    let prev = if core.position > 3.0 {
        cur
    } else if core.shuffle && core.queue.len() > 1 {
        if core.shuffle_cursor > 0 {
            core.shuffle_cursor -= 1;
            core.shuffle_history[core.shuffle_cursor]
        } else {
            cur
        }
    } else if cur > 0 {
        cur - 1
    } else {
        cur
    };
    Some(prev)
}

pub fn play_prev(app: &AppHandle) {
    let state = app.state::<AppState>();
    let shared = state.player.clone();
    let prev = {
        let mut core = shared.core.lock_safe();
        pick_prev(&mut core)
    };
    if let Some(i) = prev {
        start_playback(app, i);
    }
}

pub fn stop(app: &AppHandle) {
    let state = app.state::<AppState>();
    let shared = state.player.clone();
    {
        let mut core = shared.core.lock_safe();
        core.state = PlaybackState::Stopped;
        core.position = 0.0;
    }
    let _ = shared.audio.send(AudioCmd::Stop);
    emit_state(app, PlaybackState::Stopped);
}
