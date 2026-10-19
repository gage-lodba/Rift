//! Queue navigation: choosing the next/previous index (shuffle-aware) and the
//! play/pause, next, previous, and stop controls that act on those picks.

use std::sync::Arc;

use rift_types::{PlaybackState, RepeatMode};

use super::playback::{play_index, start_playback};
use super::state::{emit_state, PlayerCore};
use crate::audio::AudioCmd;
use crate::backend::BackendInner;
use crate::util::LockExt;

fn rand_index(len: usize) -> usize {
    (rand::random::<u64>() as usize) % len
}

/// Step the repeat mode Off → All → One → Off, applying it to `core`. Pure so
/// the transition and the crossfade bookkeeping it invalidates are testable.
pub(crate) fn cycle_repeat(core: &mut PlayerCore) -> RepeatMode {
    use RepeatMode::*;
    let next = match core.repeat {
        Off => All,
        All => One,
        One => Off,
    };
    core.repeat = next;
    // A crossfade draws its target within `crossfade` seconds of the track's end
    // and pins it in `pending_next`, which the end-of-track path reuses without
    // consulting the repeat mode. Changing modes therefore invalidates the
    // prefetch: under repeat-one it would drag playback onto another track and
    // skip the repeat the user just asked for. Dropping the pick costs a hard cut
    // instead of a crossfade (the same trade the other stale-pick paths make) and
    // un-arms so a fresh one can be drawn under the new mode.
    core.pending_next = None;
    core.crossfade_armed_for = None;
    next
}

/// Toggle shuffle, re-anchoring the cycle on the current track. Pure so the
/// transition and the crossfade bookkeeping it invalidates are testable; the
/// `toggle_shuffle` command is a lock, this call and a broadcast.
pub(crate) fn toggle_shuffle(core: &mut PlayerCore) {
    core.shuffle = !core.shuffle;
    // Anchor on the playing track either way: it has just played, so it must not
    // be drawn again straight away, and Previous needs a starting point.
    core.shuffle_history = core.current.into_iter().collect();
    core.shuffle_cursor = 0;
    // The same invalidation `cycle_repeat` documents above: a pick drawn under
    // the old mode is pinned in `pending_next`, and the end-of-track path reuses
    // it without consulting the mode — so a shuffled pick would still play after
    // shuffle was switched off, and a queue-order pick after it was switched on.
    // Dropping it costs a hard cut instead of a crossfade (the trade every
    // stale-pick path makes) and un-arms so a fresh target is drawn under the
    // new mode. It also stops the pin outliving the history rewrite above,
    // because a pin the cursor no longer names would put the track that then
    // plays back in the draw pool.
    core.pending_next = None;
    core.crossfade_armed_for = None;
}

/// Re-anchor the shuffle bookkeeping on a direct jump to `index` (a click, a
/// collection start, a resume). The jumped-to track becomes the cursor, but the
/// tracks already played this cycle stay in the history: dropping them would let
/// a track heard a moment ago be drawn again before the cycle is out, and would
/// leave Previous with nothing to walk back through. A track that already played
/// is replayed by moving the cursor rather than appending a duplicate (which
/// would otherwise strand the cursor on a repeat of the same index).
pub(crate) fn seed_shuffle_anchor(core: &mut PlayerCore, index: usize) {
    if let Some(pos) = core.shuffle_history.iter().position(|i| *i == index) {
        core.shuffle_cursor = pos
    } else {
        core.shuffle_history.push(index);
        core.shuffle_cursor = core.shuffle_history.len() - 1;
    }
}

/// Anchor the shuffle cycle on the track playback is about to move to. Every
/// path that makes a new track current — a direct jump, a replayed or drawn
/// pick, a committed crossfade — goes through here, because `pick_next` replays
/// forward from `history[shuffle_cursor]` and `pick_prev` steps back from it: a
/// cursor left naming the *previous* track would put the one that just started
/// back in the draw pool (so Next can redraw it) and make Previous walk back
/// from the wrong place. Normally the entry already exists — the pick was drawn
/// through [`pick_next`], and every command that resets the history also drops
/// any pinned pick — and this only moves the cursor onto it. It is kept as the
/// consumers' own guarantee: whatever rewrites the history underneath a track
/// that is becoming current, that track ends up in the played set. With shuffle
/// off there is no cycle to anchor.
pub(crate) fn anchor_playing(core: &mut PlayerCore, index: usize) {
    if core.shuffle {
        seed_shuffle_anchor(core, index);
    }
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
            // Normally the cursor already names the pick — `pick_next` left it on
            // the entry it reserved when it drew it — and this only moves the
            // cursor onto that entry again. It still has to hold if a history
            // rewrite ever leaves a pin behind (every command that rewrites the
            // history drops the pin today), because the consumer owns the
            // invariant: the track handed to playback must be the cycle's current
            // point, or it is missing from the played set and still in the draw
            // pool, so the next Next can redraw it.
            anchor_playing(core, i);
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
            // Every track has played this cycle. Repeat-all rolls into a fresh
            // cycle, keeping the current track out of the running so it isn't
            // repeated back-to-back, and a user-initiated Next wraps the same
            // way (mirroring queue order). Repeat off ends the cycle here, so
            // auto-advance stops exactly as it does in queue order — checking
            // `manual` instead would loop the queue forever and make the repeat
            // setting look ignored whenever shuffle is on.
            if !manual && core.repeat != RepeatMode::All {
                return None;
            }
            // Carry the playing track into the new cycle as its first entry.
            // It has just played, so it has to stay out of the pool for the
            // whole cycle (a back-to-back repeat is what the `fresh` filter
            // below is about), and it has to stay in the history: this draw is
            // usually a crossfade prefetch, i.e. it happens *before* the track
            // ends, and the pinned pick can be dropped without playing (repeat
            // cycling, a queue edit, a skip). A playing track outside the played
            // set is back in the draw pool, so Next could restart it. It also
            // gives Previous the track it came from.
            core.shuffle_history.clear();
            core.shuffle_history.push(cur);
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
pub(crate) fn toggle_playback(backend: &Arc<BackendInner>) {
    enum Action {
        None,
        Emit(PlaybackState),
        Replay(usize),
        Cancel,
    }
    let action = {
        let mut core = backend.player.core.lock_safe();
        match core.state {
            PlaybackState::Playing => {
                let _ = backend.player.audio.send(AudioCmd::Pause);
                core.state = PlaybackState::Paused;
                Action::Emit(PlaybackState::Paused)
            }
            PlaybackState::Paused => {
                let _ = backend.player.audio.send(AudioCmd::Resume);
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
        Action::Emit(s) => emit_state(backend, s),
        Action::Replay(i) => play_index(backend, i),
        Action::Cancel => {
            let _ = backend.player.audio.send(AudioCmd::Stop);
            emit_state(backend, PlaybackState::Stopped);
        }
        Action::None => {}
    }
}

pub(crate) fn play_next(backend: &Arc<BackendInner>, manual: bool) {
    let next = {
        let mut core = backend.player.core.lock_safe();
        pick_next(&mut core, manual)
    };
    match next {
        Some(i) => start_playback(backend, i),
        None => stop(backend),
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

pub(crate) fn play_prev(backend: &Arc<BackendInner>) {
    let prev = {
        let mut core = backend.player.core.lock_safe();
        pick_prev(&mut core)
    };
    if let Some(i) = prev {
        start_playback(backend, i);
    }
}

pub(crate) fn stop(backend: &Arc<BackendInner>) {
    {
        let mut core = backend.player.core.lock_safe();
        core.state = PlaybackState::Stopped;
        core.position = 0.0;
    }
    let _ = backend.player.audio.send(AudioCmd::Stop);
    emit_state(backend, PlaybackState::Stopped);
}
