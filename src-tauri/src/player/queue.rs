//! Pure queue mutations (remove / append / insert-next / move) plus their
//! outcome enums. Kept side-effect-free so the index bookkeeping is testable.

use rift_types::Track;

use super::state::PlayerCore;

/// What the caller should do after [`remove_from_queue`] mutates the core.
pub enum RemoveOutcome {
    /// Index was out of range; nothing changed.
    None,
    /// Queue changed but playback didn't; just re-broadcast.
    EmitOnly,
    /// The playing track was removed; start the one that took its slot.
    PlayIndex(usize),
    /// The playing track was the last one; stop.
    Stop,
}

/// Remove the track at `index`, fixing up `current` and resetting the shuffle
/// history (queue indices shift on removal). Pure so the index math is testable.
pub fn remove_from_queue(core: &mut PlayerCore, index: usize) -> RemoveOutcome {
    if index >= core.queue.len() {
        return RemoveOutcome::None;
    }
    core.queue.remove(index);
    core.shuffle_history.clear();
    core.shuffle_cursor = 0;
    match core.current {
        // A track before the current one went away: current shifts down by one.
        Some(cur) if index < cur => {
            core.current = Some(cur - 1);
            core.shuffle_history = vec![cur - 1];
            RemoveOutcome::EmitOnly
        }
        // The playing track was removed: move to whatever took its place, or
        // stop if it was the last one.
        Some(cur) if index == cur => {
            if core.queue.is_empty() {
                core.current = None;
                RemoveOutcome::Stop
            } else {
                RemoveOutcome::PlayIndex(cur.min(core.queue.len() - 1))
            }
        }
        // Removal was after the current track: its index is unchanged.
        Some(cur) => {
            core.shuffle_history = vec![cur];
            RemoveOutcome::EmitOnly
        }
        None => RemoveOutcome::EmitOnly,
    }
}

/// Outcome of adding tracks to the queue.
pub enum AddOutcome {
    /// Tracks were added to a running queue; just re-broadcast.
    EmitOnly,
    /// Nothing was playing; start the track that landed at this index.
    PlayIndex(usize),
}

/// Drop ids already present in the queue so adds stay idempotent.
fn dedupe_new(core: &PlayerCore, tracks: Vec<Track>) -> Vec<Track> {
    tracks
        .into_iter()
        .filter(|t| !core.queue.iter().any(|q| q.id == t.id))
        .collect()
}

/// Append `tracks` to the end of the queue. Appending never shifts existing
/// indices, so the shuffle history stays valid. If nothing is playing, signals
/// to start at the first newly added track.
pub fn append_tracks(core: &mut PlayerCore, tracks: Vec<Track>) -> AddOutcome {
    let start = core.queue.len();
    let new = dedupe_new(core, tracks);
    if new.is_empty() {
        return AddOutcome::EmitOnly;
    }
    core.queue.extend(new);
    match core.current {
        Some(_) => AddOutcome::EmitOnly,
        None => AddOutcome::PlayIndex(start),
    }
}

/// Insert `tracks` immediately after the current track so they play next. Queue
/// indices after the insertion point shift, so the shuffle history is reset to
/// the current track (mirroring [`remove_from_queue`]). If nothing is playing,
/// behaves like [`append_tracks`] and starts the first one.
pub fn insert_next(core: &mut PlayerCore, tracks: Vec<Track>) -> AddOutcome {
    let new = dedupe_new(core, tracks);
    if new.is_empty() {
        return AddOutcome::EmitOnly;
    }
    match core.current {
        Some(cur) => {
            let at = cur + 1;
            core.queue.splice(at..at, new);
            // Indices past the insertion point moved; drop the now-stale shuffle
            // history and any prefetched crossfade pick.
            core.shuffle_history = vec![cur];
            core.shuffle_cursor = 0;
            core.pending_next = None;
            AddOutcome::EmitOnly
        }
        None => {
            core.queue.splice(0..0, new);
            AddOutcome::PlayIndex(0)
        }
    }
}

/// Move the track at `from` to `to`, keeping the currently-playing track under
/// the cursor. Indices shift, so the shuffle history resets. Pure for testing.
pub fn move_in_queue(core: &mut PlayerCore, from: usize, to: usize) -> bool {
    let len = core.queue.len();
    if from >= len || to >= len || from == to {
        return false;
    }
    // Follow the playing track by identity across the move.
    let playing_id = core
        .current
        .and_then(|c| core.queue.get(c))
        .map(|t| t.id.clone());
    let track = core.queue.remove(from);
    core.queue.insert(to, track);
    if let Some(id) = playing_id {
        core.current = core.queue.iter().position(|t| t.id == id);
    }
    core.shuffle_history = core.current.into_iter().collect();
    core.shuffle_cursor = 0;
    core.pending_next = None;
    true
}
