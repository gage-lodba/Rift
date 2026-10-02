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

use crate::components::{PlayerBar, PlayerBarProps, QueuePanel, QueuePanelProps};
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
