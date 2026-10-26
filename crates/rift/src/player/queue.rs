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

/// Carry `shuffle_history`/`shuffle_cursor` through a queue edit: `map` gives an
/// old entry its new index, or `None` when the edit removed that track.
///
/// The history is the cycle's played set *and* the back/forward order, so it has
/// to be rewritten exactly the way the queue was. An entry left untouched would
/// name whichever track moved into its slot, and clearing the pair — what the
/// mutators used to do — threw the cycle away, making tracks heard minutes ago
/// immediately eligible again. The cursor is a *position* in the list rather
/// than an index, so it is recomputed: entries dropped before it shift it left,
/// and a cursor whose own track was removed lands on the entry that took its
/// slot (the caller's `play_index` re-anchors it from there).
fn remap_history(core: &mut PlayerCore, map: impl Fn(usize) -> Option<usize>) {
    let old = std::mem::take(&mut core.shuffle_history);
    let cursor_entry = core.shuffle_cursor.min(old.len().saturating_sub(1));
    let mut remapped = Vec::with_capacity(old.len());
    let mut cursor = 0;
    for (pos, index) in old.into_iter().enumerate() {
        if pos == cursor_entry {
            cursor = remapped.len();
        }
        if let Some(new) = map(index) {
            remapped.push(new);
        }
    }
    // `pick_prev` reads `history[cursor]` without a bounds check, so an empty
    // history must leave the cursor at zero and a non-empty one must keep it in
    // range. The clamp is reachable, not decoration: a crossfade prefetch leaves
    // the cursor naming the track it *reserved* while another one is still
    // playing, so removing that reserved track is an ordinary edit that drops
    // the cursor's entry and never re-seeds it (`play_index` re-anchors only the
    // path that replaces the playing track, and only under shuffle). Without the
    // clamp the cursor would sit one past the end — `pick_prev` happens to
    // decrement before indexing, so it does not panic today, but the invariant
    // the rest of the cycle code assumes would be false.
    core.shuffle_cursor = if remapped.is_empty() {
        0
    } else {
        cursor.min(remapped.len() - 1)
    };
    core.shuffle_history = remapped;
}

/// Remove the track at `index`, fixing up `current` and carrying the shuffle
/// history through the removal. Pure so the index math is testable.
pub fn remove_from_queue(core: &mut PlayerCore, index: usize) -> RemoveOutcome {
    if index >= core.queue.len() {
        return RemoveOutcome::None;
    }
    core.queue.remove(index);
    // Every surviving history entry follows the queue's shift down; the removed
    // track leaves the played set with the queue (it can't come back into the
    // cycle, and keeping a hole where it was would only make the cycle end
    // early). An empty queue therefore ends with an empty history.
    remap_history(core, |i| {
        if i < index {
            Some(i)
        } else if i == index {
            None
        } else {
            Some(i - 1)
        }
    });
    // Indices shifted, so a crossfade pick prefetched before the removal now
    // points at (and would play as) the wrong track. Drop it; the crossfade
    // commit and pick_next both treat a cleared pick as "draw normally".
    core.pending_next = None;
    match core.current {
        // A track before the current one went away: current shifts down by one,
        // and the history entry that named it — the cursor's — followed it.
        Some(cur) if index < cur => {
            core.current = Some(cur - 1);
            RemoveOutcome::EmitOnly
        }
        // The playing track was removed: move to whatever took its place, or
        // stop if it was the last one. The replacement is written into `current`
        // immediately, before the lock is released: the caller starts it through
        // `play_index`, and a snapshot taken in between (the UI's
        // `queue_snapshot` accessor runs on the app thread) must never see an
        // out-of-range `current` naming the truncated queue's old last slot.
        // Under shuffle, `play_index` then anchors the replacement in the
        // surviving order: if it has played this cycle the cursor steps back to
        // it (so Next replays where you came from, exactly as a direct jump
        // does), otherwise it joins as the newest entry.
        Some(cur) if index == cur => {
            if core.queue.is_empty() {
                core.current = None;
                RemoveOutcome::Stop
            } else {
                let replacement = cur.min(core.queue.len() - 1);
                core.current = Some(replacement);
                RemoveOutcome::PlayIndex(replacement)
            }
        }
        // Removal was after the current track: its index is unchanged.
        Some(_) => RemoveOutcome::EmitOnly,
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
/// indices after the insertion point shift, so the shuffle history is carried
/// through the same shift: the entries still name the tracks that played, and
/// the inserted tracks simply join the not-yet-played pool. If nothing is
/// playing, behaves like [`append_tracks`] and starts the first one.
pub fn insert_next(core: &mut PlayerCore, tracks: Vec<Track>) -> AddOutcome {
    let new = dedupe_new(core, tracks);
    if new.is_empty() {
        return AddOutcome::EmitOnly;
    }
    let inserted = new.len();
    if let Some(cur) = core.current {
        // `current` is a live index in every state the command layer leaves
        // behind, but it is not re-validated against the queue here: a
        // concurrent `play_tracks` replaces the queue and bumps `current` a
        // moment later, so clamp the insertion point rather than panic in
        // `splice` on an out-of-range start.
        let at = cur.saturating_add(1).min(core.queue.len());
        core.queue.splice(at..at, new);
        // Indices past the insertion point moved, so a prefetched crossfade
        // pick would now point at the wrong track. Drop it (the crossfade
        // commit and pick_next both treat a cleared pick as "draw normally").
        core.pending_next = None;
        // Inserted after `cur`, so the cursor's own entry is untouched;
        // everything past it moves with its track.
        remap_history(core, |i| Some(if i > cur { i + inserted } else { i }));
        AddOutcome::EmitOnly
    } else {
        core.queue.splice(0..0, new);
        // Nothing was playing, so the whole queue — and the history with it
        // — shifted down by the insert.
        remap_history(core, |i| Some(i + inserted));
        AddOutcome::PlayIndex(0)
    }
}

/// Move the track at `from` to `to`, keeping the currently-playing track under
/// the cursor. A move permutes the queue, so the same permutation is applied to
/// the shuffle history: the played order is still the same tracks in the same
/// order, only their queue indices changed. Pure for testing.
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
    // Indices shifted, so a crossfade pick prefetched before the move now points
    // at (and would play as) the wrong track. Drop it (the crossfade commit and
    // pick_next both treat a cleared pick as "draw normally").
    core.pending_next = None;
    // Nothing is dropped here, so the cursor keeps its position in the list and
    // still names the playing track.
    remap_history(core, |i| {
        if i == from {
            // The moved track, played or not, lands on `to`.
            Some(to)
        } else if from < to && i > from && i <= to {
            // Everything the move stepped over shifts back one.
            Some(i - 1)
        } else if to < from && i >= to && i < from {
            // Everything it stepped over shifts forward one.
            Some(i + 1)
        } else {
            Some(i)
        }
    });
    true
}
