//! Unit tests for the pure navigation and queue-mutation logic.

use rift_types::Track;

use super::nav::{pick_next, pick_prev};
use super::queue::{
    append_tracks, insert_next, move_in_queue, remove_from_queue, AddOutcome, RemoveOutcome,
};
use super::state::PlayerCore;

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
