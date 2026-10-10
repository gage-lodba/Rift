//! Unit tests for the pure navigation and queue-mutation logic.

use rift_types::{PlaybackState, QueueSnapshot, RepeatMode, Track};

use super::nav::{anchor_playing, cycle_repeat, pick_next, pick_prev, seed_shuffle_anchor};
use super::playback::{commit_crossfade, preseed_duration};
use super::queue::{
    append_tracks, insert_next, move_in_queue, remove_from_queue, AddOutcome, RemoveOutcome,
};
use super::state::{session_changed, PlayerCore};

fn dummy_track(i: usize) -> Track {
    Track {
        id: i.to_string(),
        title: String::new(),
        artist: String::new(),
        album: None,
        duration: None,
        cover: String::new(),
        artists: Vec::new(),
        album_id: None,
    }
}

/// A core with `n` tracks, shuffle on, anchored on `start` as `play_index`
/// would leave it.
fn shuffled_core(n: usize, start: usize) -> PlayerCore {
    PlayerCore {
        queue: (0..n).map(dummy_track).collect(),
        current: Some(start),
        shuffle: true,
        shuffle_history: vec![start],
        shuffle_cursor: 0,
        ..PlayerCore::default()
    }
}

/// Apply a chosen index the way `start_playback` would (sets `current`).
fn advance(core: &mut PlayerCore, idx: usize) {
    core.current = Some(idx);
}

#[test]
fn shuffle_covers_every_track_once_before_repeating() {
    let mut core = shuffled_core(4, 2);
    let mut seen = vec![2];
    for _ in 0..3 {
        let n = pick_next(&mut core, true).expect("a next track");
        advance(&mut core, n);
        seen.push(n);
    }
    seen.sort_unstable();
    assert_eq!(seen, vec![0, 1, 2, 3], "every track plays exactly once");

    // Cycle complete: auto-advance with repeat off stops.
    assert_eq!(pick_next(&mut core, false), None);
}

#[test]
fn shuffle_auto_advance_stops_at_the_end_of_the_cycle_when_repeat_is_off() {
    let mut core = shuffled_core(3, 0);
    // Play out the rest of the cycle.
    for _ in 0..2 {
        let n = pick_next(&mut core, true).expect("a next track");
        advance(&mut core, n);
    }
    // Every track has played, repeat is off: auto-advance must stop here rather
    // than looping the queue forever.
    assert_eq!(pick_next(&mut core, false), None);
}

#[test]
fn shuffle_repeat_one_auto_advance_replays_the_current_track() {
    let mut core = shuffled_core(4, 1);
    core.repeat = RepeatMode::One;
    assert_eq!(pick_next(&mut core, false), Some(1));
}

#[test]
fn shuffle_repeat_all_auto_advance_starts_a_fresh_cycle() {
    let mut core = shuffled_core(3, 0);
    core.repeat = RepeatMode::All;
    for _ in 0..2 {
        let n = pick_next(&mut core, false).expect("a next track");
        advance(&mut core, n);
    }
    let n = pick_next(&mut core, false).expect("a fresh cycle");
    assert_ne!(n, core.current.unwrap(), "doesn't replay the current track");
}

#[test]
fn shuffle_manual_next_wraps_at_the_end_of_the_cycle_when_repeat_is_off() {
    let mut core = shuffled_core(3, 0);
    core.shuffle_history = vec![0, 2, 1];
    core.shuffle_cursor = 2;
    // A user-initiated Next still responds, mirroring queue-order mode.
    let n = pick_next(&mut core, true).expect("manual next wraps");
    assert_ne!(n, core.current.unwrap(), "doesn't replay the current track");
}

/// The shuffle bookkeeping `play_index` applies to a direct jump: the anchor
/// moves the cursor to the jumped-to track, but the tracks already played this
/// cycle stay in the history (so they aren't drawn again, and Previous can walk
/// back through them).
fn jump(core: &mut PlayerCore, index: usize) {
    seed_shuffle_anchor(core, index);
    core.current = Some(index);
}

#[test]
fn a_jump_keeps_already_played_tracks_out_of_the_running() {
    let mut core = shuffled_core(4, 0);
    core.shuffle_history = vec![0, 2];
    core.shuffle_cursor = 1;
    // Jump back to a track already played this cycle: it must not be redrawn as
    // an upcoming track.
    jump(&mut core, 2);
    let n = pick_next(&mut core, false).expect("a next track");
    assert_ne!(n, 0, "a track already played this cycle");
    assert_ne!(n, 2, "the current track");
}

#[test]
fn a_jump_to_a_new_track_enters_the_cycle_but_keeps_the_history() {
    let mut core = shuffled_core(4, 0);
    core.shuffle_history = vec![0];
    core.shuffle_cursor = 0;
    jump(&mut core, 3);
    // A few seconds in, Previous restarts the jumped-to track…
    core.position = 5.0;
    assert_eq!(
        pick_prev(&mut core),
        Some(3),
        "restarts the jumped-to track"
    );
    // …and stepping back right after the jump walks into the pre-jump history.
    core.position = 0.0;
    assert_eq!(pick_prev(&mut core), Some(0), "back to the pre-jump track");
    // Next then replays the existing order instead of redrawing.
    core.position = 0.0;
    assert_eq!(
        pick_next(&mut core, true),
        Some(3),
        "replays the jump target"
    );
}

/// Simulate a queue playing to completion: returns the played order and whether
/// playback stopped on its own, or `None` if it was still finding next tracks
/// after 50 advances (i.e. it never stops).
fn play_out(
    core: &mut PlayerCore,
    shuffle: bool,
    repeat: RepeatMode,
) -> Option<(Vec<usize>, bool)> {
    core.shuffle = shuffle;
    core.repeat = repeat;
    let mut played = vec![core.current.unwrap()];
    for _ in 0..50 {
        match pick_next(core, false) {
            Some(i) => {
                advance(core, i);
                played.push(i);
            }
            None => return Some((played, true)),
        }
    }
    None
}

#[test]
fn a_finished_queue_stops_unless_repeat_asks_for_more() {
    // Queue order.
    let (played, stopped) = {
        let mut core = queue_core(3, Some(0));
        play_out(&mut core, false, RepeatMode::Off).unwrap()
    };
    assert_eq!(played, vec![0, 1, 2]);
    assert!(stopped);

    // Shuffle, repeat off: exactly one pass over the queue, then stop.
    let mut core = queue_core(4, Some(0));
    core.shuffle_history = vec![0];
    let (played, stopped) = play_out(&mut core, true, RepeatMode::Off).unwrap();
    assert_eq!(played.len(), 4, "one pass: {played:?}");
    let mut sorted = played.clone();
    sorted.sort_unstable();
    assert_eq!(sorted, vec![0, 1, 2, 3], "every track once: {played:?}");
    assert!(stopped);

    // Shuffle with repeat all keeps going indefinitely.
    let mut core = queue_core(4, Some(0));
    core.shuffle_history = vec![0];
    assert_eq!(play_out(&mut core, true, RepeatMode::All), None);

    // Shuffle with repeat one never leaves the current track.
    let mut core = queue_core(4, Some(2));
    assert_eq!(play_out(&mut core, true, RepeatMode::One), None);
    assert_eq!(core.current, Some(2));
}

#[test]
fn shuffle_previous_retraces_then_next_replays_forward() {
    let mut core = shuffled_core(5, 0);

    // Build a history by skipping forward.
    let mut played = vec![0usize];
    for _ in 0..3 {
        let n = pick_next(&mut core, true).unwrap();
        advance(&mut core, n);
        played.push(n);
    }
    assert_eq!(core.shuffle_history, played);
    assert_eq!(core.shuffle_cursor, played.len() - 1);

    // Previous steps back through the *actual* play order.
    let p1 = pick_prev(&mut core).unwrap();
    advance(&mut core, p1);
    assert_eq!(p1, played[2]);
    let p2 = pick_prev(&mut core).unwrap();
    advance(&mut core, p2);
    assert_eq!(p2, played[1]);

    // Next now replays forward through the existing order — no new draws.
    let f1 = pick_next(&mut core, true).unwrap();
    advance(&mut core, f1);
    assert_eq!(f1, played[2]);
    let f2 = pick_next(&mut core, true).unwrap();
    advance(&mut core, f2);
    assert_eq!(f2, played[3]);
    assert_eq!(core.shuffle_history, played, "replaying must not redraw");
}

#[test]
fn previous_near_start_of_track_restarts_it() {
    let mut core = shuffled_core(5, 0);
    let n = pick_next(&mut core, true).unwrap();
    advance(&mut core, n);
    // More than a few seconds in: Previous restarts the current track.
    core.position = 5.0;
    assert_eq!(pick_prev(&mut core), Some(n));
}

#[test]
fn repeat_cycles_through_off_all_and_one() {
    let mut core = queue_core(3, Some(1));
    assert_eq!(cycle_repeat(&mut core), RepeatMode::All);
    assert_eq!(cycle_repeat(&mut core), RepeatMode::One);
    assert_eq!(cycle_repeat(&mut core), RepeatMode::Off);
}

#[test]
fn changing_repeat_mode_drops_a_prefetched_crossfade_pick() {
    let mut core = queue_core(3, Some(1));
    // A crossfade drew the next track before the mode changed (here: while the
    // user cycles Off → All → One mid-prefetch).
    core.pending_next = Some(2);
    core.crossfade_armed_for = Some(core.generation);
    assert_eq!(cycle_repeat(&mut core), RepeatMode::All);
    assert_eq!(core.pending_next, None, "stale prefetch dropped");
    assert_eq!(core.crossfade_armed_for, None, "crossfade un-armed");
    // The end-of-track path therefore advances normally, not onto the stale pick
    // (with repeat off this is exactly the track it would have crossfaded to).
    assert_eq!(pick_next(&mut core, false), Some(2));
}

#[test]
fn sequential_next_prev_walk_queue_order() {
    let mut core = PlayerCore {
        queue: (0..3).map(dummy_track).collect(),
        current: Some(0),
        ..PlayerCore::default()
    };
    assert_eq!(pick_next(&mut core, false), Some(1));
    advance(&mut core, 1);
    assert_eq!(pick_prev(&mut core), Some(0));
    // End of queue, repeat off, auto-advance stops.
    advance(&mut core, 2);
    assert_eq!(pick_next(&mut core, false), None);
    // Manual next from the end wraps.
    assert_eq!(pick_next(&mut core, true), Some(0));
}

fn queue_core(n: usize, current: Option<usize>) -> PlayerCore {
    PlayerCore {
        queue: (0..n).map(dummy_track).collect(),
        current,
        ..PlayerCore::default()
    }
}

/// Step forward `n` times the way a manual Next does, returning the ids played.
fn skip_forward(core: &mut PlayerCore, n: usize) -> Vec<String> {
    let mut played = Vec::new();
    for _ in 0..n {
        let next = pick_next(core, true).expect("a next track");
        advance(core, next);
        played.push(core.queue[next].id.clone());
    }
    played
}

/// Auto-advance through the rest of the cycle (repeat off) and return the ids
/// played, sorted. The cycle has to end: one that keeps handing out tracks after
/// every remaining track has had its turn is the defect these tests pin down.
fn play_rest(core: &mut PlayerCore) -> Vec<String> {
    let mut played = Vec::new();
    while let Some(next) = pick_next(core, false) {
        advance(core, next);
        played.push(core.queue[next].id.clone());
        assert!(played.len() < 64, "auto-advance never stopped: {played:?}");
    }
    played.sort_unstable();
    played
}

/// The queue ids that have not played this cycle: neither in `played` nor the
/// track playing now.
fn unplayed_ids(core: &PlayerCore, played: &[String]) -> Vec<String> {
    let current = core.current.map(|c| core.queue[c].id.clone());
    let mut ids: Vec<String> = core
        .queue
        .iter()
        .map(|t| t.id.clone())
        .filter(|id| !played.contains(id) && Some(id) != current.as_ref())
        .collect();
    ids.sort_unstable();
    ids
}

#[test]
fn pending_next_is_honoured_once_then_falls_through() {
    let mut core = PlayerCore {
        queue: (0..3).map(dummy_track).collect(),
        current: Some(0),
        pending_next: Some(2),
        ..PlayerCore::default()
    };
    // A crossfade-prefetched pick is returned and consumed.
    assert_eq!(pick_next(&mut core, false), Some(2));
    assert_eq!(core.pending_next, None);
    // The next call falls through to normal sequential order.
    advance(&mut core, 2);
    assert_eq!(pick_next(&mut core, true), Some(0));
}

#[test]
fn stale_out_of_range_pending_next_is_ignored() {
    let mut core = PlayerCore {
        queue: (0..2).map(dummy_track).collect(),
        current: Some(0),
        pending_next: Some(9),
        ..PlayerCore::default()
    };
    // An index left over from a longer queue is dropped, not played.
    assert_eq!(pick_next(&mut core, false), Some(1));
    assert_eq!(core.pending_next, None);
}

#[test]
fn remove_before_current_shifts_current_down() {
    let mut core = queue_core(5, Some(3));
    assert!(matches!(
        remove_from_queue(&mut core, 1),
        RemoveOutcome::EmitOnly
    ));
    assert_eq!(core.current, Some(2));
    assert_eq!(core.queue.len(), 4);
}

#[test]
fn remove_after_current_keeps_current() {
    let mut core = queue_core(5, Some(1));
    assert!(matches!(
        remove_from_queue(&mut core, 3),
        RemoveOutcome::EmitOnly
    ));
    assert_eq!(core.current, Some(1));
}

#[test]
fn remove_current_plays_the_one_that_took_its_slot() {
    let mut core = queue_core(5, Some(2));
    assert!(matches!(
        remove_from_queue(&mut core, 2),
        RemoveOutcome::PlayIndex(2)
    ));
}

#[test]
fn remove_current_at_end_clamps_to_new_last() {
    let mut core = queue_core(3, Some(2));
    assert!(matches!(
        remove_from_queue(&mut core, 2),
        RemoveOutcome::PlayIndex(1)
    ));
}

#[test]
fn remove_last_remaining_current_stops() {
    let mut core = queue_core(1, Some(0));
    core.shuffle_history = vec![0];
    assert!(matches!(
        remove_from_queue(&mut core, 0),
        RemoveOutcome::Stop
    ));
    assert_eq!(core.current, None);
    assert!(core.queue.is_empty());
    assert!(
        core.shuffle_history.is_empty(),
        "the played set goes with the queue it indexed"
    );
}

#[test]
fn remove_clears_a_pending_crossfade_pick() {
    let mut core = queue_core(5, Some(1));
    core.pending_next = Some(3);
    // Removal shifts indices, so a prefetched pick would land on the wrong
    // track if it survived.
    assert!(matches!(
        remove_from_queue(&mut core, 2),
        RemoveOutcome::EmitOnly
    ));
    assert_eq!(core.pending_next, None);
}

#[test]
fn remove_out_of_range_is_a_noop() {
    let mut core = queue_core(2, Some(0));
    assert!(matches!(
        remove_from_queue(&mut core, 9),
        RemoveOutcome::None
    ));
    assert_eq!(core.queue.len(), 2);
}

fn ids(core: &PlayerCore) -> Vec<String> {
    core.queue.iter().map(|t| t.id.clone()).collect()
}

#[test]
fn append_adds_at_end_and_skips_duplicates() {
    let mut core = queue_core(2, Some(0)); // ids "0","1"
    let outcome = append_tracks(&mut core, vec![dummy_track(1), dummy_track(5)]);
    assert!(matches!(outcome, AddOutcome::EmitOnly));
    assert_eq!(
        ids(&core),
        vec!["0", "1", "5"],
        "dup '1' skipped, '5' appended"
    );
    assert_eq!(core.current, Some(0), "append doesn't move current");
}

#[test]
fn append_to_empty_queue_starts_playback() {
    let mut core = PlayerCore::default();
    let outcome = append_tracks(&mut core, vec![dummy_track(7), dummy_track(8)]);
    assert!(matches!(outcome, AddOutcome::PlayIndex(0)));
}

#[test]
fn insert_next_places_after_current() {
    let mut core = queue_core(3, Some(1)); // "0","1","2", playing "1"
    core.shuffle_history = vec![0, 1];
    core.shuffle_cursor = 1;
    let outcome = insert_next(&mut core, vec![dummy_track(9)]);
    assert!(matches!(outcome, AddOutcome::EmitOnly));
    assert_eq!(ids(&core), vec!["0", "1", "9", "2"]);
    assert_eq!(core.current, Some(1), "still playing the same track");
    assert_eq!(
        core.shuffle_history,
        vec![0, 1],
        "the insertion is after both played entries, so neither moved"
    );
}

#[test]
fn insert_next_with_nothing_playing_starts_first() {
    let mut core = PlayerCore::default();
    let outcome = insert_next(&mut core, vec![dummy_track(4)]);
    assert!(matches!(outcome, AddOutcome::PlayIndex(0)));
    assert_eq!(ids(&core), vec!["4"]);
}

#[test]
fn move_follows_the_playing_track() {
    let mut core = queue_core(4, Some(2)); // playing "2"
    assert!(move_in_queue(&mut core, 0, 3)); // "0" to the end
    assert_eq!(ids(&core), vec!["1", "2", "3", "0"]);
    assert_eq!(core.current, Some(1), "current follows track '2'");
}

#[test]
fn move_out_of_range_or_noop_is_rejected() {
    let mut core = queue_core(3, Some(0));
    assert!(!move_in_queue(&mut core, 0, 0), "no-op move");
    assert!(!move_in_queue(&mut core, 5, 1), "out of range");
}

/// The shuffle cycle is about *tracks*, not slots: a queue edit renumbers what
/// has already played instead of forgetting it. After four of six tracks have
/// played, removing one of them must leave exactly the two that never played
/// before the cycle ends — the old code cleared the history here, so the draw
/// pool was four tracks wide again and tracks heard moments ago came straight
/// back.
#[test]
fn removing_a_played_track_leaves_the_rest_of_the_cycle_intact() {
    let mut core = shuffled_core(6, 0);
    let mut played = vec![core.queue[0].id.clone()];
    played.extend(skip_forward(&mut core, 3));
    // Index 0 always holds the anchor, which played first and is never the
    // current track, so this is a removal before `current`.
    assert!(matches!(
        remove_from_queue(&mut core, 0),
        RemoveOutcome::EmitOnly
    ));
    let expected = unplayed_ids(&core, &played);
    assert_eq!(expected.len(), 2, "four of six tracks had played");
    assert_eq!(play_rest(&mut core), expected);
}

/// A removal *after* the current track leaves its index alone but still
/// renumbers the entries that shifted, so the played set survives it.
#[test]
fn removing_after_the_current_track_keeps_the_played_set() {
    // Queue ids "0".."5" (the ids match their indices, so the history reads as
    // tracks): this cycle played "3", then "5", then jumped back to "1".
    let mut core = queue_core(6, Some(1));
    core.shuffle = true;
    core.shuffle_history = vec![3, 5, 1];
    core.shuffle_cursor = 2;
    assert!(matches!(
        remove_from_queue(&mut core, 3),
        RemoveOutcome::EmitOnly
    ));
    assert_eq!(core.current, Some(1), "current is unchanged");
    assert_eq!(ids(&core), vec!["0", "1", "2", "4", "5"]);
    let played = vec!["3".to_owned(), "5".to_owned(), "1".to_owned()];
    let expected = unplayed_ids(&core, &played);
    assert_eq!(play_rest(&mut core), expected);
}

/// Removing the playing track hands the caller the index that took its slot,
/// which `play_index` anchors. The already-played tracks stay in the cycle, so
/// only the replacement and the never-played tracks are left.
#[test]
fn removing_the_playing_track_replaces_it_without_restarting_the_cycle() {
    // Played "0", then "2", then jumped to "4".
    let mut core = queue_core(6, Some(4));
    core.shuffle = true;
    core.shuffle_history = vec![0, 2, 4];
    core.shuffle_cursor = 2;
    let RemoveOutcome::PlayIndex(i) = remove_from_queue(&mut core, 4) else {
        panic!("removing the playing track must name a replacement");
    };
    assert_eq!(i, 4, "the track that took its slot");
    jump(&mut core, i); // what `queue_remove`'s PlayIndex arm does
    assert_eq!(core.queue[i].id, "5");
    let played = vec!["0".to_owned(), "2".to_owned(), "4".to_owned()];
    let expected = unplayed_ids(&core, &played);
    assert_eq!(play_rest(&mut core), expected);
}

/// The one case where removing the playing track can put an already-played
/// track back on: the replacement is the new last track, and that has played. It
/// re-anchors at its place in the order, so Next replays the track that followed
/// it before drawing the never-played ones — the same rule a direct jump follows
/// — and the cycle still ends after those two.
#[test]
fn removing_the_last_playing_track_replays_from_its_place_in_the_order() {
    let mut core = queue_core(6, Some(5));
    core.shuffle = true;
    core.shuffle_history = vec![0, 4, 1, 5];
    core.shuffle_cursor = 3;
    let RemoveOutcome::PlayIndex(i) = remove_from_queue(&mut core, 5) else {
        panic!("removing the playing track must name a replacement");
    };
    assert_eq!(i, 4, "clamped to the new last track");
    jump(&mut core, i); // "4" already played, so the cursor steps back to it
    assert_eq!(play_rest(&mut core), vec!["1", "2", "3"]);
}

/// Insert-next renumbers everything past the insertion point, so the played set
/// has to move with it; only the never-played tracks and the insert are left.
#[test]
fn inserting_next_keeps_the_rest_of_the_cycle() {
    let mut core = shuffled_core(6, 0);
    let mut played = vec![core.queue[0].id.clone()];
    played.extend(skip_forward(&mut core, 3));
    let cur = core.current.unwrap();
    assert!(matches!(
        insert_next(&mut core, vec![dummy_track(9)]),
        AddOutcome::EmitOnly
    ));
    assert_eq!(
        core.queue[cur + 1].id,
        "9",
        "lands right after the current track"
    );
    let expected = unplayed_ids(&core, &played);
    assert_eq!(expected.len(), 3, "two never played plus the insert");
    assert_eq!(play_rest(&mut core), expected);
}

/// A played track that sits *after* the insertion point is renumbered with the
/// queue, so Previous still walks back to the track that actually played.
#[test]
fn insert_next_shifts_played_entries_past_the_current_track() {
    // Played "4", then jumped back to "2"; "4" sits at index 4, past the insert.
    let mut core = queue_core(6, Some(2));
    core.shuffle = true;
    core.shuffle_history = vec![4, 2];
    core.shuffle_cursor = 1;
    assert!(matches!(
        insert_next(&mut core, vec![dummy_track(9)]),
        AddOutcome::EmitOnly
    ));
    assert_eq!(ids(&core), vec!["0", "1", "2", "9", "3", "4", "5"]);
    core.position = 0.0;
    let prev = pick_prev(&mut core).expect("a previous track");
    assert_eq!(
        core.queue[prev].id, "4",
        "the track that played before the current one"
    );
}

/// A move permutes the queue; the cycle is about tracks, so it survives every
/// reordering — the rest of the cycle is still exactly the tracks that never
/// played, whichever way the queue was reshuffled.
#[test]
fn moving_any_track_keeps_the_rest_of_the_cycle_intact() {
    for (from, to) in [(0, 5), (5, 0), (3, 1), (1, 4)] {
        let mut core = shuffled_core(6, 0);
        let mut played = vec![core.queue[0].id.clone()];
        played.extend(skip_forward(&mut core, 3));
        assert!(move_in_queue(&mut core, from, to));
        let expected = unplayed_ids(&core, &played);
        assert_eq!(expected.len(), 2, "four of six tracks had played");
        assert_eq!(play_rest(&mut core), expected, "move {from} → {to}");
    }

    // Moving the playing track itself: it stays current (by identity, wherever
    // it lands) and the cycle is still intact around it.
    let mut core = shuffled_core(6, 0);
    let mut played = vec![core.queue[0].id.clone()];
    played.extend(skip_forward(&mut core, 3));
    let cur = core.current.unwrap();
    assert!(move_in_queue(&mut core, cur, 0));
    assert_eq!(core.current, Some(0), "current follows the moved track");
    let expected = unplayed_ids(&core, &played);
    assert_eq!(play_rest(&mut core), expected, "the current track moved");
}

/// Previous walks back through the tracks that really played, in order, even
/// when the queue was reordered under it.
#[test]
fn a_move_keeps_previous_walking_the_real_play_order() {
    let mut core = shuffled_core(6, 0);
    let mut played = vec![core.queue[0].id.clone()];
    played.extend(skip_forward(&mut core, 3));
    // Reorder the queue out from under the history: last track to the front.
    assert!(move_in_queue(&mut core, 5, 0));
    let mut back = Vec::new();
    for _ in 0..3 {
        core.position = 0.0;
        let prev = pick_prev(&mut core).expect("a previous track");
        advance(&mut core, prev);
        back.push(core.queue[prev].id.clone());
    }
    assert_eq!(
        back,
        vec![played[2].clone(), played[1].clone(), played[0].clone()],
        "Previous retraces the played order in reverse"
    );
}

/// The seek bar's length while a track's stream is being fetched. `fetch_track`
/// reports 0.0 for an unknown length (the offline path always does, since it
/// hands back the file without probing it), and that zero must not become the
/// bar's range.
#[test]
fn preseed_duration_prefers_metadata_and_never_zeroes_a_known_length() {
    // The normal case: the new track's own length is shown immediately, so the
    // slider's range tracks the track actually selected.
    assert_eq!(preseed_duration(Some(200), 0.0), 200.0);
    // Offline/unknown length, but the bar already had a length to show: keep it
    // rather than collapsing to 0 for the whole fetch window.
    assert_eq!(preseed_duration(None, 180.0), 180.0);
    // Nothing known at all stays zero; the decoder's Duration is authoritative.
    assert_eq!(preseed_duration(None, 0.0), 0.0);
    // A metadata length of zero is treated as "known to be zero-length" only
    // because it is `Some`; it must not fall through to the stale previous.
    assert_eq!(preseed_duration(Some(0), 180.0), 0.0);
}

/// The redundant-write guard. A track's first play emits the queue twice (once
/// for the change, once after credit enrichment); the second carries an
/// identical snapshot and must not queue another disk write.
#[test]
fn an_unchanged_snapshot_is_not_persisted_again() {
    let snap = QueueSnapshot {
        tracks: vec![dummy_track(1), dummy_track(2)],
        current: Some(1),
        ..QueueSnapshot::default()
    };
    let mut last = None;

    assert!(
        session_changed(&mut last, &snap),
        "the first snapshot writes"
    );
    assert!(
        !session_changed(&mut last, &snap),
        "an identical snapshot must be skipped"
    );

    // Any real change writes again: current, modes, source, or the tracks.
    let moved = QueueSnapshot {
        current: Some(0),
        ..snap.clone()
    };
    assert!(session_changed(&mut last, &moved), "a new current writes");
    let reordered = QueueSnapshot {
        tracks: vec![dummy_track(2), dummy_track(1)],
        ..moved.clone()
    };
    assert!(session_changed(&mut last, &reordered), "a new order writes");
    let shuffled = QueueSnapshot {
        shuffle: true,
        ..reordered.clone()
    };
    assert!(
        session_changed(&mut last, &shuffled),
        "a mode change writes"
    );
    let resourced = QueueSnapshot {
        source: Some("liked".into()),
        ..shuffled.clone()
    };
    assert!(
        session_changed(&mut last, &resourced),
        "a source change writes"
    );

    // ...and the just-written snapshot is itself deduped.
    assert!(!session_changed(&mut last, &resourced));
}

// ---------------------------------------------------------------------------
// Seeded property test over sequences of commands
// ---------------------------------------------------------------------------
//
// Every test above drives one operation and checks the state it leaves. M10
// (the shuffle cycle forgetting what it had played across a queue edit) lived
// through two review sessions because that is all they did: the invariant is a
// property of *sequences*, so these drive the real functions the command layer
// calls, thousands of steps at a time, and assert the bookkeeping after every
// one. The seed is fixed so any failure is reproducible, and each message names
// the seed and step.

/// xorshift64. A fixed-seed generator is enough here and keeps the test free of
/// a dependency; it is never asked for cryptographic quality, only for a long,
/// reproducible walk over the state space.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    /// A value in `0..n` (`0` when `n` is zero).
    fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next() % n as u64) as usize
        }
    }

    fn chance(&mut self, one_in: u64) -> bool {
        self.next() % one_in == 0
    }
}

/// The state `start_playback` commits for a hard start: the track becomes
/// current, the prefetched crossfade pick is dropped, and the position resets.
/// The fetch/decode that follows is modelled as succeeding (`Playing`), since
/// only the queue bookkeeping is under test here.
fn start_like(core: &mut PlayerCore, index: usize) {
    core.current = Some(index);
    core.generation += 1;
    core.pending_next = None;
    core.state = PlaybackState::Playing;
    core.position = 0.0;
}

/// `play_index` as the commands run it: an out-of-range jump is ignored, a real
/// one clears the failure budget, anchors the cycle under shuffle, and starts.
fn jump_like(core: &mut PlayerCore, index: usize) {
    if index >= core.queue.len() {
        return;
    }
    core.failures = 0;
    anchor_playing(core, index);
    start_like(core, index);
}

/// One to three tracks, occasionally reusing an id that is already queued so the
/// real add paths' de-duplication is exercised by a rejection, not assumed.
fn random_tracks(core: &PlayerCore, rng: &mut Rng, next_id: &mut usize, max: usize) -> Vec<Track> {
    let n = 1 + rng.below(max);
    (0..n)
        .map(|_| {
            if !core.queue.is_empty() && rng.chance(4) {
                core.queue[rng.below(core.queue.len())].clone()
            } else {
                let id = *next_id;
                *next_id += 1;
                dummy_track(id)
            }
        })
        .collect()
}

/// What one random command did, for the invariants that only apply to some of
/// them.
struct Step {
    /// The command, for failure messages.
    label: String,
    /// A queue *edit*: the operations that must not lose the played set (M10).
    edit: bool,
    /// The command ended by making a specific track current through an anchored
    /// path (a jump, a Next/auto-advance, a committed crossfade, a shuffle
    /// toggle), so the cursor must name it afterwards. A prefetch is not one of
    /// these: it deliberately runs the cycle ahead of playback.
    settles: bool,
}

impl Step {
    fn new(label: String, edit: bool, settles: bool) -> Self {
        Self {
            label,
            edit,
            settles,
        }
    }
}

/// Apply one random command the way the command layer does: call the real pure
/// function, then perform the state updates its caller performs (`play_index`,
/// `play_next`, `queue_remove`, `queue_clear`, `play_tracks`, …).
fn apply_random_step(core: &mut PlayerCore, rng: &mut Rng, next_id: &mut usize) -> Step {
    match rng.below(13) {
        0 => {
            let index = rng.below(core.queue.len() + 2);
            jump_like(core, index);
            Step::new(
                format!("play_index({index})"),
                false,
                index < core.queue.len(),
            )
        }
        1 => {
            let manual = rng.chance(2);
            // An automatic advance under repeat-one hands back the track that is
            // already playing without touching the cycle, so it settles nothing.
            let replaying = !manual && core.repeat == RepeatMode::One;
            let advanced = match pick_next(core, manual) {
                Some(i) => {
                    start_like(core, i);
                    true
                }
                // `play_next` stops when the cycle ends; `stop` leaves the
                // current track in place.
                None => {
                    core.position = 0.0;
                    false
                }
            };
            Step::new(
                format!("play_next(manual={manual})"),
                false,
                advanced && !replaying,
            )
        }
        2 => {
            // A Previous pressed part-way through restarts the track instead of
            // stepping back (`pick_prev` reads `position`), and a restart can
            // leave the cursor on a reservation the prefetch made — see the
            // cursor note in `assert_bookkeeping` — so this step does not
            // promise the strong form.
            core.position = if rng.chance(3) { 5.0 } else { 0.0 };
            if let Some(i) = pick_prev(core) {
                start_like(core, i);
            }
            Step::new("play_prev".to_owned(), false, false)
        }
        3 => {
            let index = rng.below(core.queue.len() + 2);
            let mut settles = false;
            match remove_from_queue(core, index) {
                RemoveOutcome::PlayIndex(i) => {
                    jump_like(core, i);
                    settles = true;
                }
                RemoveOutcome::Stop => {
                    core.state = PlaybackState::Stopped;
                    core.position = 0.0;
                }
                RemoveOutcome::None | RemoveOutcome::EmitOnly => {}
            }
            Step::new(format!("remove({index})"), true, settles)
        }
        4 => {
            let tracks = random_tracks(core, rng, next_id, 3);
            let started = matches!(insert_next(core, tracks), AddOutcome::PlayIndex(_));
            if started {
                // The only insert that starts playback is the empty-queue one,
                // which always starts index 0.
                jump_like(core, 0);
            }
            Step::new("insert_next".to_owned(), true, started)
        }
        5 => {
            let tracks = random_tracks(core, rng, next_id, 3);
            let start = match append_tracks(core, tracks) {
                AddOutcome::PlayIndex(i) => Some(i),
                AddOutcome::EmitOnly => None,
            };
            if let Some(i) = start {
                jump_like(core, i);
            }
            Step::new("append_tracks".to_owned(), true, start.is_some())
        }
        6 => {
            let from = rng.below(core.queue.len() + 1);
            let to = rng.below(core.queue.len() + 1);
            move_in_queue(core, from, to);
            Step::new(format!("move({from},{to})"), true, false)
        }
        7 => {
            // `toggle_shuffle`: flips the mode and re-anchors the history on the
            // current track, so the cursor names it again afterwards. Like the
            // command, it does *not* drop a prefetched crossfade pick — the
            // asymmetry with `cycle_repeat` is real, and the assertions below
            // hold the bookkeeping to it.
            core.shuffle = !core.shuffle;
            core.shuffle_history = core.current.into_iter().collect();
            core.shuffle_cursor = 0;
            Step::new("toggle_shuffle".to_owned(), false, core.current.is_some())
        }
        8 => {
            cycle_repeat(core);
            Step::new("cycle_repeat".to_owned(), false, false)
        }
        9 => {
            // The pure step of `begin_crossfade`: draw ahead of time and pin the
            // pick. The position/state gates that decide *when* it arms don't
            // change this transition. This is the one command that legitimately
            // leaves the cursor ahead of the playing track.
            if core.pending_next.is_none() && core.repeat != RepeatMode::One {
                if let Some(next) = pick_next(core, false) {
                    core.pending_next = Some(next);
                }
            }
            Step::new("prefetch_crossfade".to_owned(), false, false)
        }
        10 => {
            // The pure step of the commit once the fetch lands and nothing has
            // advanced: the real function, so a missing anchor is caught here.
            let committed = core.pending_next.is_some_and(|i| i < core.queue.len());
            if committed {
                let index = core.pending_next.expect("checked above");
                commit_crossfade(core, index);
            }
            Step::new("commit_crossfade".to_owned(), false, committed)
        }
        11 => {
            // `queue_clear`: the queue and the bookkeeping go together.
            core.queue.clear();
            core.current = None;
            core.source = None;
            core.shuffle_history.clear();
            core.shuffle_cursor = 0;
            core.pending_next = None;
            core.state = PlaybackState::Stopped;
            core.position = 0.0;
            Step::new("queue_clear".to_owned(), false, false)
        }
        _ => {
            // `play_tracks`: a new queue starts a fresh cycle, then plays the
            // requested index (`start >= len` is rejected before anything).
            let n = 1 + rng.below(6);
            let start = rng.below(n + 1);
            let tracks: Vec<Track> = (0..n)
                .map(|_| {
                    let id = *next_id;
                    *next_id += 1;
                    dummy_track(id)
                })
                .collect();
            if start < n {
                core.queue = tracks;
                core.source = Some("random-queue".into());
                core.shuffle_history.clear();
                core.shuffle_cursor = 0;
                jump_like(core, start);
                Step::new("play_tracks".to_owned(), false, true)
            } else {
                Step::new("play_tracks(rejected)".to_owned(), false, false)
            }
        }
    }
}

/// The ids the history names, in order: the cycle's played set.
fn played_ids(core: &PlayerCore) -> Vec<String> {
    core.shuffle_history
        .iter()
        .map(|i| core.queue[*i].id.clone())
        .collect()
}

/// The id of the queue index the cursor names.
fn cursor_id(core: &PlayerCore) -> String {
    core.queue[core.shuffle_history[core.shuffle_cursor]]
        .id
        .clone()
}

/// Structural invariants after every command. Each is asserted, not
/// re-established: the property is that the functions maintain them.
fn assert_bookkeeping(core: &PlayerCore, seed: u64, step: usize, what: &str) {
    let at = format!("seed {seed:#x} step {step} after {what}");
    let len = core.queue.len();
    for (pos, index) in core.shuffle_history.iter().enumerate() {
        assert!(
            *index < len,
            "{at}: shuffle_history[{pos}] = {index} is not a live index (queue has {len})"
        );
        // Entries are unique by design: `pick_next` only draws from the pool
        // (and `seed_shuffle_anchor` moves the cursor onto an entry that already
        // exists), so a duplicate would mean a track repeated before its cycle
        // was out.
        assert!(
            !core.shuffle_history[..pos].contains(index),
            "{at}: shuffle_history lists {index} twice: {:?}",
            core.shuffle_history
        );
    }
    if core.shuffle_history.is_empty() {
        assert_eq!(
            core.shuffle_cursor, 0,
            "{at}: an empty history leaves no cursor"
        );
    } else {
        // `pick_prev` reads `history[cursor]` without a bounds check.
        assert!(
            core.shuffle_cursor < core.shuffle_history.len(),
            "{at}: cursor {} is out of range for history {:?}",
            core.shuffle_cursor,
            core.shuffle_history
        );
    }
    if let Some(cur) = core.current {
        assert!(
            cur < len,
            "{at}: current {cur} is not a live index (queue has {len})"
        );
    }
    if let Some(pending) = core.pending_next {
        // A pinned pick is only ever read as `pick_next`'s next track, and that
        // read re-checks the bound, but the command layer only ever pins a live
        // index in the first place.
        assert!(
            pending < len,
            "{at}: pending_next {pending} is not a live index (queue has {len})"
        );
    }
    if core.shuffle {
        match core.current {
            // The playing track must be in the played set. The draw pool is
            // "every index not in the history", so a current track missing from
            // it can be drawn again — Next would restart the track already
            // playing. The cursor is deliberately *not* asserted to name it:
            // a prefetch advances the cursor onto the track it reserves while
            // the previous one is still playing (that is what makes the pick
            // reusable), and a pick dropped without playing leaves the cursor on
            // that track — which then waits for the next cycle — rather than
            // walking it back.
            Some(cur) => assert!(
                core.shuffle_history.contains(&cur),
                "{at}: the playing track {cur} is not in the played set {:?}, so Next can redraw it",
                core.shuffle_history
            ),
            None => assert!(
                core.shuffle_history.is_empty(),
                "{at}: nothing is playing but the history still holds {:?}",
                core.shuffle_history
            ),
        }
    }
}

/// The strong form of the cursor invariant, asserted where it *must* hold: a
/// command that makes a new track current through an anchored path — a jump, a
/// Next/auto-advance, a committed crossfade, a shuffle toggle — has to leave the
/// cursor naming that track. It deliberately does not hold between a prefetch
/// and the command that plays the reserved pick, nor after a Previous restarts a
/// track whose cycle the prefetch had already run ahead of; see the note in
/// `assert_bookkeeping` for exactly which states are permitted.
fn assert_cursor_names_current(core: &PlayerCore, seed: u64, step: usize, what: &str) {
    if !core.shuffle {
        return;
    }
    if let Some(cur) = core.current {
        assert_eq!(
            core.shuffle_history.get(core.shuffle_cursor),
            Some(&cur),
            "seed {seed:#x} step {step} after {what}: the cursor must name the track playback settled on (history {:?}, cursor {}, current {cur})",
            core.shuffle_history,
            core.shuffle_cursor
        );
    }
}

/// M10 as a property: an edit may renumber the played set, but a track that has
/// played this cycle and is still in the queue must not fall out of it — that
/// is exactly what "the edit forgot what it played" looked like.
fn assert_no_played_track_was_lost(
    core: &PlayerCore,
    before: &[String],
    seed: u64,
    step: usize,
    what: &str,
) {
    let after = played_ids(core);
    let lost: Vec<String> = before
        .iter()
        .filter(|id| !after.contains(*id) && core.queue.iter().any(|t| &t.id == *id))
        .cloned()
        .collect();
    assert!(
        lost.is_empty(),
        "seed {seed:#x} step {step} after {what}: the edit dropped played tracks {lost:?} from the cycle (was {before:?}, now {after:?})"
    );
}

/// Play the rest of the cycle out on the bookkeeping alone (state saved and
/// restored, so the surrounding sequence is unaffected): auto-advance must never
/// replay a track, and with repeat off it must stop exactly when every track in
/// the queue has played. That is the invariant the hand-written cycle tests
/// assert, checked here from whatever state the sequence reached.
fn assert_cycle_is_sound(core: &mut PlayerCore, seed: u64, step: usize, what: &str) {
    if !core.shuffle || core.queue.len() < 2 || core.current.is_none() {
        return;
    }
    let at = format!(
        "seed {seed:#x} step {step} after {what} ({} tracks, history {:?}, cursor {}, current {:?}, pending {:?})",
        core.queue.len(),
        core.shuffle_history,
        core.shuffle_cursor,
        core.current,
        core.pending_next
    );
    let saved = (
        core.current,
        core.repeat,
        core.shuffle_history.clone(),
        core.shuffle_cursor,
        core.pending_next,
    );
    core.repeat = RepeatMode::Off;
    let entered_on = core.current;
    let mut played: Vec<String> = Vec::new();
    while let Some(next) = pick_next(core, false) {
        assert!(
            next < core.queue.len(),
            "{at}: pick_next returned {next} for a {}-track queue",
            core.queue.len()
        );
        if played.is_empty() {
            // Auto-advance must never restart the track that just ended: the
            // pool excludes it precisely so a cycle doesn't repeat back-to-back,
            // and the fresh-cycle branch filters it out too.
            assert_ne!(
                Some(next),
                entered_on,
                "{at}: auto-advance restarted the track that was already playing"
            );
        }
        let id = core.queue[next].id.clone();
        assert!(
            !played.contains(&id),
            "{at}: the cycle replayed \"{id}\" before every track had played: {played:?}"
        );
        played.push(id);
        core.current = Some(next);
        assert!(
            played.len() <= core.queue.len(),
            "{at}: auto-advance never stopped after {played:?}"
        );
    }
    assert_eq!(
        core.shuffle_history.len(),
        core.queue.len(),
        "{at}: a completed cycle must have played every track once"
    );
    let (current, repeat, history, cursor, pending) = saved;
    core.current = current;
    core.repeat = repeat;
    core.shuffle_history = history;
    core.shuffle_cursor = cursor;
    core.pending_next = pending;
}

/// The sequence test. Seeds are fixed; each failure message names the seed, the
/// step and the command, so a run is reproducible by rerunning this test.
#[test]
fn random_command_sequences_keep_the_shuffle_bookkeeping_sound() {
    const SEEDS: [u64; 4] = [
        0x9E37_79B9_7F4A_7C15,
        0xDEAD_BEEF_CAFE_BABE,
        0x0123_4567_89AB_CDEF,
        0x0BAD_F00D_DEAD_BEEF,
    ];
    const STEPS: usize = 2_000;

    for seed in SEEDS {
        let mut rng = Rng::new(seed);
        // Vary the starting shape so the four walks don't all leave the same
        // state: 2–8 tracks, shuffle on or off, anchored on a real track.
        let n = 2 + rng.below(7);
        let start = rng.below(n);
        let mut core = PlayerCore {
            queue: (0..n).map(dummy_track).collect(),
            current: Some(start),
            shuffle: rng.chance(2),
            shuffle_history: vec![start],
            shuffle_cursor: 0,
            crossfade: 10.0,
            duration: 180.0,
            ..PlayerCore::default()
        };
        let mut next_id = 1_000;
        for step in 0..STEPS {
            let before = played_ids(&core);
            let applied = apply_random_step(&mut core, &mut rng, &mut next_id);
            assert_bookkeeping(&core, seed, step, &applied.label);
            if applied.settles {
                assert_cursor_names_current(&core, seed, step, &applied.label);
            }
            if applied.edit {
                assert_no_played_track_was_lost(&core, &before, seed, step, &applied.label);
            }
            assert_cycle_is_sound(&mut core, seed, step, &applied.label);
        }
    }
}

// ---------------------------------------------------------------------------
// The prefetch/cursor states the property test settled
// ---------------------------------------------------------------------------

/// The prefetch deliberately runs the cycle ahead of playback: `pick_next`
/// advances the cursor onto the track it draws, and `begin_crossfade` pins that
/// pick so the crossfade commit or the end-of-track advance can reuse it. So
/// between the draw and that advance the cursor names the *reserved* track while
/// `current` is still the previous one. That is the state seed 0x9e3779b97f4a7c15
/// step 15 reached (`history [4, 5, 0], cursor 2, current 5`), and it is by
/// design, not a lost cursor: the playing track stays in the played set
/// throughout, and the advance that plays the pick leaves the cursor naming it.
/// Asserting "the cursor always names the playing track" would be wrong here;
/// `random_command_sequences_keep_the_shuffle_bookkeeping_sound` instead asserts
/// the strong form on the commands that must settle it (a jump, a Next, a
/// committed crossfade, a toggle) and this pins the permitted intermediate state.
#[test]
fn a_prefetched_pick_leads_the_cursor_until_it_is_played() {
    // Played "2", then "0", then stepped back: "0" is playing and "3" is the
    // entry Next would replay.
    let mut core = queue_core(4, Some(0));
    core.shuffle = true;
    core.shuffle_history = vec![2, 0, 3];
    core.shuffle_cursor = 1;
    // The prefetch draws it: the cursor moves onto "3" — the pick it pins —
    // while "0" is still playing.
    let next = pick_next(&mut core, false).expect("a next track");
    core.pending_next = Some(next);
    assert_eq!(next, 3, "the replay the cursor stepped onto");
    assert_eq!(
        cursor_id(&core),
        "3",
        "the cursor leads onto the reserved track"
    );
    assert_eq!(core.current, Some(0), "playback has not moved yet");
    assert!(
        core.shuffle_history.contains(&0),
        "the playing track stays in the played set while the pick waits"
    );

    // The end-of-track advance consumes the pin, and now the cursor must name
    // the track that is playing.
    let played = pick_next(&mut core, false).expect("the pinned pick");
    advance(&mut core, played);
    assert_eq!(core.current, Some(3));
    assert_eq!(
        cursor_id(&core),
        "3",
        "once the pick plays, the cursor names it"
    );
}

/// The property test's other finding (seed 0x9e3779b97f4a7c15 step 686): at the
/// end of a complete cycle a prefetch draws the *next* cycle's first track, and
/// that draw happens while the last track is still playing. If the pick never
/// plays — here the user presses Previous, and a hard start drops the pin — the
/// track that is still playing has to stay in the played set, or the draw pool
/// hands it straight back to the next advance.
#[test]
fn a_cycle_rollover_keeps_the_playing_track_in_the_played_set() {
    // A five-track cycle has played out completely, ending on "4".
    let mut core = queue_core(5, Some(4));
    core.shuffle = true;
    core.repeat = RepeatMode::All;
    core.shuffle_history = vec![1, 3, 0, 2, 4];
    core.shuffle_cursor = 4;
    // The prefetch's draw rolls into a fresh cycle there and then, pinning the
    // first track of the new cycle.
    let pick = pick_next(&mut core, false).expect("a fresh cycle");
    core.pending_next = Some(pick);
    assert_ne!(
        pick, 4,
        "a fresh cycle doesn't start on the track just played"
    );

    // The user presses Previous before it plays; `start_playback` drops the pin.
    core.position = 0.0;
    let prev = pick_prev(&mut core).expect("a previous track");
    advance(&mut core, prev);
    core.pending_next = None;
    assert_eq!(prev, 4, "Previous replays the track that just played");
    assert!(
        core.shuffle_history.contains(&core.current.unwrap()),
        "the playing track stays in the played set even though the pick was dropped"
    );
    // …and the next advance cannot restart it.
    let next = pick_next(&mut core, false).expect("a next track");
    assert_ne!(next, 4, "auto-advance must not restart the playing track");
}

/// The counterpart to the state above, found by the property test at seed
/// 0x9e3779b97f4a7c15 step 241: `toggle_shuffle` re-anchors the shuffle history
/// on the playing track but deliberately keeps a prefetched pick. When the track
/// ends and that pick plays, it has to be anchored as the cycle's current point
/// too — otherwise the track that just started is missing from the played set,
/// stays in the draw pool, and Next can redraw it immediately (the property
/// test's cycle check saw exactly that repeat).
#[test]
fn toggle_shuffle_mid_prefetch_anchors_the_pick_when_the_track_ends() {
    let mut core = queue_core(4, Some(1)); // ids "0".."3", playing "1"
    core.shuffle_history = vec![1]; // "1" has played this cycle
    core.shuffle_cursor = 0;
    core.pending_next = Some(2); // a crossfade drew "2" while shuffle was off
                                 // The user turns shuffle on mid-prefetch: `toggle_shuffle` flips the mode,
                                 // re-anchors the history on the playing track, and leaves the pin alone.
    core.shuffle = !core.shuffle;
    core.shuffle_history = core.current.into_iter().collect();
    core.shuffle_cursor = 0;

    let played = pick_next(&mut core, false).expect("the pinned pick");
    advance(&mut core, played);
    assert_eq!(played, 2, "the prefetched pick still plays");
    assert_eq!(
        cursor_id(&core),
        "2",
        "playing the pick anchors the cursor on it"
    );
    // The rest of the cycle is exactly the two tracks that have never played.
    assert_eq!(play_rest(&mut core), vec!["0", "3"]);
}

/// The same sequence through the other consumer: the crossfade's fetch wins the
/// race and `commit_crossfade` makes the pinned pick current. It needs the same
/// anchor, which is why the commit's pure half is a function of its own.
#[test]
fn toggle_shuffle_mid_prefetch_anchors_the_pick_when_the_crossfade_commits() {
    let mut core = queue_core(4, Some(1));
    core.shuffle_history = vec![1];
    core.shuffle_cursor = 0;
    core.pending_next = Some(2);
    core.shuffle = !core.shuffle; // the toggle above
    core.shuffle_history = core.current.into_iter().collect();
    core.shuffle_cursor = 0;

    commit_crossfade(&mut core, 2);
    assert_eq!(core.current, Some(2));
    assert_eq!(
        cursor_id(&core),
        "2",
        "committing the pick anchors the cursor on it"
    );
    assert_eq!(play_rest(&mut core), vec!["0", "3"]);
}
