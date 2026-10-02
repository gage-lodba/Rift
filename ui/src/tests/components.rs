//! Component tests: real components, rendered through Yew's server renderer.
//!
//! No DOM, no browser, no network — `LocalServerRenderer` drives the actual
//! component renderer (and its hook accounting) to a string, so these run under
//! the same Node runner as everything else in this tree.
//!
//! # What these do and do not cover
//!
//! These are **first-render** tests. SSR renders each component instance
//! exactly once, so they cannot see anything that only happens on a *second*
//! render — which includes the hook-order defect (`C1`) this file exists to
//! eventually cover. Two things were measured and are recorded so nobody
//! re-derives them:
//!
//! - Effects do not run during SSR. A wrapper whose `use_effect_with` sets
//!   state renders only its first pass; nothing re-renders it.
//! - Yew's hook-order assertion fires when a *second successful* render
//!   disagrees with the first about how many hooks ran. One pass cannot
//!   produce that pair, and the suspense retry path compares a suspended pass
//!   against a successful one, which is a different branch.
//!
//! So: what is covered here is that each component renders at all, and renders
//! the content its props describe. That is worth having — a panic in a first
//! render is a blank screen — but it is *not* hook-order coverage, and the
//! distinction is deliberate.

use rift_types::{PlaybackState, Progress, QueueSnapshot, RepeatMode, Track};
use wasm_bindgen_test::wasm_bindgen_test;
use yew::prelude::*;

use crate::components::{track_key, PlayerBar, PlayerBarProps, QueuePanel, QueuePanelProps};
use crate::tests::harness::track;

/// A queue of `n` tracks, `t0..tn`.
fn queue_of(n: usize) -> QueueSnapshot {
    QueueSnapshot {
        tracks: (0..n).map(|i| track(&format!("t{i}"))).collect(),
        current: if n == 0 { None } else { Some(0) },
        ..QueueSnapshot::default()
    }
}

fn queue_props(n: usize) -> QueuePanelProps {
    QueuePanelProps {
        queue: queue_of(n),
        on_jump: Callback::noop(),
        on_remove: Callback::noop(),
        on_clear: Callback::noop(),
        on_move: Callback::noop(),
    }
}

fn player_props(track: Option<Track>) -> PlayerBarProps {
    PlayerBarProps {
        track,
        state: PlaybackState::Stopped,
        progress: Progress::default(),
        volume: 1.0,
        shuffle: false,
        repeat: RepeatMode::Off,
        liked: false,
        on_open_artist: Callback::noop(),
        on_volume: Callback::noop(),
    }
}

/// Render a component's first pass to HTML.
async fn first_render<C>(props: C::Properties) -> String
where
    C: BaseComponent + 'static,
{
    yew::LocalServerRenderer::<C>::with_props(props)
        .render()
        .await
}

// ---------------------------------------------------------------------------
// QueuePanel
// ---------------------------------------------------------------------------

/// The empty queue is `App`'s *first* render state (`app.rs` seeds
/// `QueueSnapshot::default()`), and `QueuePanel` returns early for it. That
/// early return is what made `C1` possible, so the state it guards is worth
/// pinning: an empty queue renders nothing at all, not a stray shell.
#[wasm_bindgen_test]
async fn queue_panel_renders_nothing_for_an_empty_queue() {
    let html = first_render::<QueuePanel>(queue_props(0)).await;
    assert!(
        !html.contains("queue-panel"),
        "an empty queue should render no panel at all: {html:?}"
    );
}

/// The non-empty path renders the section and its rows. This is the state the
/// early return hands over to, and the one that tripped the hook assertion.
#[wasm_bindgen_test]
async fn queue_panel_renders_its_rows() {
    let html = first_render::<QueuePanel>(queue_props(3)).await;
    assert!(html.contains("queue-panel"), "no panel: {html}");
    for i in 0..3 {
        assert!(
            html.contains(&format!("Title t{i}")),
            "row t{i} missing from: {html}"
        );
    }
}

/// A one-track queue is the boundary between "renders nothing" and "renders a
/// list", and the boundary is where an off-by-one would show up as a blank
/// panel.
#[wasm_bindgen_test]
async fn queue_panel_renders_a_single_row() {
    let html = first_render::<QueuePanel>(queue_props(1)).await;
    assert!(html.contains("queue-panel"), "no panel: {html}");
    assert!(html.contains("Title t0"), "the one row is missing: {html}");
    assert!(
        !html.contains("Title t1"),
        "a one-track queue rendered a second row: {html}"
    );
}

// ---------------------------------------------------------------------------
// PlayerBar
// ---------------------------------------------------------------------------

/// With nothing loaded the bar still has to render — it is always on screen,
/// even before playback starts.
#[wasm_bindgen_test]
async fn player_bar_renders_with_no_track() {
    let html = first_render::<PlayerBar>(player_props(None)).await;
    assert!(!html.is_empty(), "the bar rendered nothing at all");
    assert!(
        !html.contains("Title t0"),
        "no track is loaded, so no track should be shown: {html}"
    );
}

/// With a track loaded, its metadata reaches the bar.
#[wasm_bindgen_test]
async fn player_bar_renders_the_loaded_track() {
    let html = first_render::<PlayerBar>(player_props(Some(track("abc")))).await;
    assert!(
        html.contains("Title abc"),
        "the loaded track's title is missing: {html}"
    );
    assert!(
        html.contains("Artist"),
        "the loaded track's artist is missing: {html}"
    );
}

// ---------------------------------------------------------------------------
// H4's dependency: the re-seed effect must be able to notice a track change
// ---------------------------------------------------------------------------

/// The half of H4 that can be checked without a renderer.
///
/// H4 was a stale progress bar: the re-seed effect was keyed on
/// `props.progress` alone, and `App` deliberately does not subscribe to
/// PROGRESS ticks, so that value never moved after bootstrap. Switching tracks
/// therefore left the bar showing the *previous* track's elapsed time and
/// slider `max` for the whole fetch window. The fix added the track's identity
/// to the effect's dependency list — which only works if two different tracks
/// produce two different keys.
///
/// So this asserts the property the fix rests on rather than the effect
/// running, which needs a second render. If `track_key` ever stopped
/// distinguishing tracks, this fails and the effect would silently go back to
/// never re-running — the exact regression, restored invisibly.
#[wasm_bindgen_test]
fn track_key_distinguishes_tracks_so_the_reseed_effect_can_rerun() {
    let a = track_key(&Some(track("a")));
    let b = track_key(&Some(track("b")));
    assert_ne!(
        a, b,
        "two different tracks produced the same key, so the re-seed effect \
         would never re-run on a track change (H4)"
    );
}

/// Nothing loaded is its own identity, distinct from any track — so stopping
/// re-seeds the bar instead of leaving the last track's position on screen.
#[wasm_bindgen_test]
fn track_key_separates_no_track_from_a_track() {
    let idle = track_key(&None);
    assert_eq!(idle, None);
    assert_ne!(
        idle,
        track_key(&Some(track("a"))),
        "`None` must be distinguishable from a loaded track"
    );
}

/// The key is the track's *id*, not a title or cover URL — metadata for the
/// same song can be refreshed and re-sent during playback (a like, a download
/// completing), and if that re-seeded the bar the position would jump back to
/// the seed mid-song.
#[wasm_bindgen_test]
fn track_key_ignores_metadata_that_can_change_mid_playback() {
    let original = track("abc");
    let mut refreshed = original.clone();
    refreshed.title = "Title abc (remastered)".to_string();
    refreshed.cover = "https://example.invalid/other.jpg".to_string();
    refreshed.duration = Some(999);

    assert_eq!(
        track_key(&Some(original)),
        track_key(&Some(refreshed)),
        "the same track id with refreshed metadata must keep its key, or the \
         bar re-seeds and the position jumps mid-song"
    );
}
