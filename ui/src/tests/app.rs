//! App-level logic that a user can reach and a diff cannot show
//! (`app.rs`, `components/queue.rs`).
//!
//! These are the pure decisions `App` makes — which collection counts as fully
//! downloaded, where a shuffled play starts, how a duration reads. The state
//! machine they feed (queue, shuffle, crossfade) lives in the backend and has
//! its own tests in `src-tauri/src/player/tests.rs`; what is checked here is the
//! frontend's half of each decision, which had none.
//!
//! Rendering is out of scope by construction: driving Yew needs a DOM, and no
//! test in this tree may depend on one. That is the recorded gap — see
//! `docs/code-review-2026-09.md`.

use std::collections::HashSet;

use rift_types::Library;
use wasm_bindgen_test::wasm_bindgen_test;

use crate::app::{fmt_total_duration, liked_index, playlist_fully_downloaded, shuffled_start};
use crate::tests::harness::{ids, playlist, track};

// ---------------------------------------------------------------------------
// shuffled_start — the Play button's first track
// ---------------------------------------------------------------------------

/// The bug this exists for: the backend rejects `start >= tracks.len()` with
/// "nothing to play", so a start index equal to the length makes the Play
/// button a no-op. `Math::random()` cannot return exactly 1, but the
/// multiplication can round up to it, and the original `as usize` cast had no
/// bound at all.
#[wasm_bindgen_test]
fn a_shuffled_start_is_always_inside_the_collection() {
    for len in 1..=64usize {
        // 1.0 is outside `random()`'s range and is included anyway: the
        // invariant has to hold for the value the arithmetic can produce, not
        // just for the values the generator documents.
        for r in [0.0, 1e-9, 0.5, 0.999_999_999, 1.0 - f64::EPSILON, 1.0] {
            let start = shuffled_start(r, len);
            assert!(
                start < len,
                "len {len}, random {r}: start {start} is out of range, which the \
                 backend rejects as \"nothing to play\""
            );
        }
    }
}

/// The bound is the only thing being fixed; the distribution must still spread
/// across the whole collection, or "shuffle" would always start at the same row.
#[wasm_bindgen_test]
fn a_shuffled_start_covers_every_row() {
    let len = 10;
    let seen: HashSet<usize> = (0..1000)
        .map(|i| shuffled_start(i as f64 / 1000.0, len))
        .collect();
    assert_eq!(
        seen,
        (0..len).collect::<HashSet<_>>(),
        "every row should be reachable as a shuffle start"
    );
}

/// Boundaries, asserted exactly. `0.5 * 4 == 2` and the top of the range lands
/// on the last row rather than rolling over to the first.
#[wasm_bindgen_test]
fn shuffled_start_boundaries() {
    assert_eq!(shuffled_start(0.0, 4), 0);
    assert_eq!(shuffled_start(0.24, 4), 0);
    assert_eq!(shuffled_start(0.25, 4), 1);
    assert_eq!(shuffled_start(0.5, 4), 2);
    assert_eq!(shuffled_start(0.999_999_999, 4), 3);
    assert_eq!(shuffled_start(1.0, 4), 3, "clamped, not wrapped to 0");
    assert_eq!(shuffled_start(0.0, 1), 0);
    assert_eq!(shuffled_start(1.0, 1), 0);
}

/// An empty collection cannot be started. The call site guards on
/// `len() > 1` first, but this is the function's own contract — returning 0 for
/// an empty list is what keeps it total rather than panicking on `len - 1`.
#[wasm_bindgen_test]
fn an_empty_collection_has_no_shuffle_start() {
    assert_eq!(shuffled_start(0.0, 0), 0);
    assert_eq!(shuffled_start(0.99, 0), 0);
}

// ---------------------------------------------------------------------------
// fmt_total_duration — the collection subtitle
// ---------------------------------------------------------------------------

/// The collection header's duration text. Pinned as a table rather than by
/// cases, so the hour/minute boundary — where "59 min" and "1 hr 0 min" meet —
/// is visible in one place.
#[wasm_bindgen_test]
fn total_duration_reads_the_way_the_header_shows_it() {
    let cases: &[(u32, &str)] = &[
        (0, "0 sec"),
        (1, "1 sec"),
        (59, "59 sec"),
        (60, "1 min"),
        (61, "1 min"),
        (119, "1 min"),
        (3599, "59 min"),
        (3600, "1 hr 0 min"),
        (3661, "1 hr 1 min"),
        (7325, "2 hr 2 min"),
    ];
    for (secs, want) in cases {
        assert_eq!(
            fmt_total_duration(*secs),
            *want,
            "{secs} seconds should read {want:?}"
        );
    }
}

/// Seconds are dropped once there is a minute to show, and minutes once there
/// is an hour: the header states a magnitude, and a per-second value going
/// stale would be worse than an approximate one.
#[wasm_bindgen_test]
fn total_duration_never_shows_seconds_alongside_minutes() {
    for secs in [60u32, 125, 3599, 3600, 7200] {
        let text = fmt_total_duration(secs);
        assert!(
            !text.contains("sec"),
            "{secs} seconds should not mention seconds once it has a minute: {text:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// playlist_fully_downloaded — the offline pin
// ---------------------------------------------------------------------------

/// The single shared predicate behind two user-visible behaviours: the
/// "Downloaded" button that cannot be pressed, and the refusal to delete a
/// download that a playlist is keeping offline. Those two must agree, which is
/// why they share this function rather than each testing the set themselves.
#[wasm_bindgen_test]
fn a_playlist_is_pinned_only_when_every_track_is_downloaded() {
    let p = playlist("p1", 3);
    let all: HashSet<String> = ids(&p.tracks).into_iter().collect();

    assert!(
        playlist_fully_downloaded(&p, &all),
        "all three downloaded means pinned"
    );

    for missing in ids(&p.tracks) {
        let mut some = all.clone();
        some.remove(&missing);
        assert!(
            !playlist_fully_downloaded(&p, &some),
            "missing {missing} means not pinned, however many others are present"
        );
    }

    assert!(
        !playlist_fully_downloaded(&p, &HashSet::new()),
        "nothing downloaded means not pinned"
    );
}

/// An empty playlist is never pinned. Vacuous truth would make it pinned, which
/// would show an un-pressable "Downloaded" button on a playlist with no songs
/// and block deletions for no reason.
#[wasm_bindgen_test]
fn an_empty_playlist_is_never_pinned() {
    let empty = playlist("p1", 0);
    assert!(
        !playlist_fully_downloaded(&empty, &HashSet::new()),
        "an empty playlist must not report itself as fully downloaded"
    );
    // Even with unrelated downloads present.
    let others: HashSet<String> = ids(&[track("x"), track("y")]).into_iter().collect();
    assert!(!playlist_fully_downloaded(&empty, &others));
}

/// Downloads of tracks that are *not* in the playlist do not pin it. The
/// predicate is about the playlist's own tracks, and a set-membership slip
/// (testing `downloaded.len() >= p.tracks.len()`, say) would pass the
/// all-present case and fail here.
#[wasm_bindgen_test]
fn unrelated_downloads_do_not_pin_a_playlist() {
    let p = playlist("p1", 2);
    let unrelated: HashSet<String> = ids(&[track("x"), track("y"), track("z")])
        .into_iter()
        .collect();
    assert!(
        !playlist_fully_downloaded(&p, &unrelated),
        "downloads of other tracks must not count"
    );
}

/// Two playlists sharing a track: each is pinned on its own tracks, so a shared
/// track being downloaded pins both — and a track of `b` missing from the set
/// unpins `b` alone. The `pinned_ids` union in `App` rests on this.
#[wasm_bindgen_test]
fn playlists_are_evaluated_independently() {
    let a = playlist("a", 2);
    let mut b = playlist("b", 2);
    // `b` shares `a`'s first track and keeps one of its own, whose id is
    // distinct so it is absent from a set built only from `a`.
    b.tracks[0] = a.tracks[0].clone();

    let only_a: HashSet<String> = ids(&a.tracks).into_iter().collect();
    assert!(playlist_fully_downloaded(&a, &only_a));
    assert!(
        !playlist_fully_downloaded(&b, &only_a),
        "b has a track that is not downloaded, so b is not pinned"
    );

    let both: HashSet<String> = only_a
        .union(&ids(&b.tracks).into_iter().collect())
        .cloned()
        .collect();
    assert!(playlist_fully_downloaded(&a, &both));
    assert!(playlist_fully_downloaded(&b, &both));
    assert!(
        both.len() == 3,
        "the two playlists should share exactly one track: {both:?}"
    );
}

// ---------------------------------------------------------------------------
// liked_index — the library → hearts wiring
// ---------------------------------------------------------------------------

/// **The one line that connects the library to the hearts.**
///
/// The renderer tests hand-build their `TrackListCtx`, so an `App` that fed the
/// index the wrong list — or never built it — would leave them all green. This
/// drives the real helper: the reported cross-surface pair resolves, a song that
/// is only in Recently played does not, and an empty library indexes nothing.
#[wasm_bindgen_test]
fn the_liked_index_is_built_from_the_liked_songs_by_song() {
    // Liked from the artist page: the art track, credit and no duration.
    let mut liked_row = track("Jb6gcoR266U");
    liked_row.title = "Around the World".to_string();
    liked_row.artist = "Daft Punk".to_string();
    liked_row.album_id = Some("MPREb_dMHztPsrDgN".to_string());
    liked_row.duration = None;
    // The album page's row for the same song: the music video, duration and no
    // credit. It is in Recently played, not in liked.
    let mut album_row = track("K0HSD_i2DvA");
    album_row.title = "Around the World".to_string();
    album_row.artist = String::new();
    album_row.album_id = Some("MPREb_dMHztPsrDgN".to_string());
    album_row.duration = Some(430);

    let library = Library {
        liked: vec![liked_row.clone()],
        recently_played: vec![album_row.clone()],
        ..Library::default()
    };
    let liked = liked_index(&library);
    assert!(
        liked.contains(&album_row),
        "the album page's row must be liked through the artist page's row"
    );
    assert!(liked.contains(&liked_row));

    // Built from `liked`, not from every stored list: a song only in Recently
    // played must not light a heart.
    let recent_only = track("recent-only");
    let library = Library {
        liked: vec![liked_row],
        recently_played: vec![recent_only.clone()],
        ..Library::default()
    };
    assert!(!liked_index(&library).contains(&recent_only));

    // And nothing liked indexes nothing, rather than matching by accident.
    assert!(!liked_index(&Library::default()).contains(&album_row));
}
