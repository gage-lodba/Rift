//! Unit tests for the pure navigation and queue-mutation logic.

use rift_types::{QueueSnapshot, RepeatMode, Track};

use super::nav::{cycle_repeat, pick_next, pick_prev, seed_shuffle_anchor};
use super::playback::preseed_duration;
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
    assert!(matches!(
        remove_from_queue(&mut core, 0),
        RemoveOutcome::Stop
    ));
    assert_eq!(core.current, None);
    assert!(core.queue.is_empty());
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
    let outcome = insert_next(&mut core, vec![dummy_track(9)]);
    assert!(matches!(outcome, AddOutcome::EmitOnly));
    assert_eq!(ids(&core), vec!["0", "1", "9", "2"]);
    assert_eq!(core.current, Some(1), "still playing the same track");
    assert_eq!(core.shuffle_history, vec![1]);
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
