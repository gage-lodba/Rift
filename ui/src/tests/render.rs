//! Second-render tests: the real Yew **client** renderer, in a real DOM.
//!
//! This is the half of the frontend harness that was missing. Everything else in
//! `src/tests/` covers either pure decisions or a *first* render; Yew's server
//! renderer drives a component instance exactly once and never runs an effect, so
//! the two defects below are invisible to it:
//!
//! - **`C1`** — a `use_state` below the queue panel's empty-queue early return.
//!   The first render (empty queue) skips it, the second demands it, and Yew's
//!   `assert_hook_context` fires with *"Hooks are called conditionally."* The
//!   static check in `hook_order.rs` covers that defect's *shape*; this covers
//!   the behaviour, on the real component, through the real scheduler.
//! - **`H4`** — PlayerBar's live progress surviving a track change. The fix made
//!   the re-seed effect depend on the track's identity, and `components.rs`
//!   pins the half that is decidable without a renderer (`track_key` must
//!   distinguish two tracks). What it could not pin is the effect actually
//!   re-running — which is what
//!   [`player_bar_reseeds_the_seek_bar_when_the_track_changes`] now drives.
//!
//! # Two detectors, on purpose
//!
//! [`a_hook_below_an_early_return_is_caught_on_the_second_render`] asserts that
//! Yew's assertion fires on a planted defect, and
//! [`the_repair_is_exercised_on_the_real_component`] asserts that the shipped
//! `QueuePanel` survives the transition that defect broke. They fail in opposite
//! directions and both are needed: the first is what makes the harness
//! trustworthy (this defect class *is* detectable here), the second is what
//! makes the fix verified (it is not merely undetected). A harness whose
//! detector has never been seen firing is the `hook_order.rs` lesson — three
//! bugs in that scanner passed against the whole codebase while being unable to
//! detect the defect they existed for.
//!
//! # What is deliberately not here
//!
//! **Computed style.** No layout engine, so inherited style is unobservable and
//! the white `buttonface` album text stays open. See `dom.rs` and `README.md`;
//! nothing in this file should be read as covering it.

use std::cell::RefCell;
use std::rc::Rc;

use rift_types::{events, PlaybackState, Progress, QueueSnapshot, RepeatMode, Track};
use wasm_bindgen_test::wasm_bindgen_test;
use yew::prelude::*;

use crate::components::{PlayerBar, PlayerBarProps, QueuePanel};
use crate::tests::dom::{flush, mount};
use crate::tests::harness::{emit_for_test, playlist, track};

// ---------------------------------------------------------------------------
// Driving a mounted component from a test
// ---------------------------------------------------------------------------

/// A closure a mounted component publishes so the test can drive it.
///
/// `use_state` is per-instance and a test cannot reach into it. This is the
/// other direction: the component hands out a `Callback` when it mounts and the
/// test calls it — the same thing the app does to a component when state above
/// it changes, not a way of poking a hook.
type Setter<T> = Rc<RefCell<Option<Callback<T>>>>;

/// A `Setter` plus the callback that fills it, for a component's props.
fn setter_channel<T: 'static>() -> (Setter<T>, Callback<Callback<T>>) {
    let cell: Setter<T> = Rc::new(RefCell::new(None));
    let publish = cell.clone();
    (
        cell,
        Callback::from(move |cb: Callback<T>| *publish.borrow_mut() = Some(cb)),
    )
}

/// The closure `setter` holds, or a failure saying the component never published
/// one.
fn take_setter<T: Clone + 'static>(setter: &Setter<T>) -> Callback<T> {
    setter
        .borrow()
        .clone()
        .expect("the mounted component never published its setter")
}

// ---------------------------------------------------------------------------
// The DOM the shim provides
// ---------------------------------------------------------------------------

/// The DOM is not optional here, and its absence has exactly one cause.
///
/// Without the shim, `web_sys::window()` returns `None` and the first mount
/// panics inside a helper — which says what is wrong but not how it usually came
/// to be wrong. This fails first, and says the fix.
#[wasm_bindgen_test]
fn the_dom_shim_is_loaded() {
    let window = web_sys::window().expect(
        "no DOM: the wasm tests were run without ui/tests/dom-shim.cjs.\n\
         Run `npm install` in ui/ first, then run the tests through\n\
         `cargo test -p rift-ui --target wasm32-unknown-unknown`, which sets\n\
         NODE_OPTIONS for you (.cargo/config.toml). See ui/README.md.",
    );
    assert!(
        window.document().is_some(),
        "`window` exists but `document` does not — the shim loaded partially"
    );
}

// ---------------------------------------------------------------------------
// C1 — a hook below an early return
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// The harness's own proof of life
// ---------------------------------------------------------------------------

/// A counter whose value the test drives, so the harness can be shown to run a
/// **second** render, an effect, and a callback — none of which the server
/// renderer does.
///
/// The value lives in an `Rc<Cell>` the test owns rather than in a `use_state`,
/// deliberately: a closure built on one render captures *that* render's state
/// handle, so incrementing through a stale clone counts from the same base twice
/// and looks like a missed re-render. A shared cell has one value, so a wrong
/// count here means a genuinely missed render. (That is not hypothetical — the
/// first version of this probe did exactly that and read 1 after two bumps.)
#[function_component]
fn Counter(props: &ProbeProps) -> Html {
    let value = use_state(|| props.source.get());
    {
        let value = value.clone();
        let source = props.source.clone();
        let exposed = props.exposed.clone();
        use_effect_with((), move |_| {
            exposed.emit(Callback::from(move |_: ()| value.set(source.get())));
            || ()
        });
    }
    html! { <span class="count">{ value.to_string() }</span> }
}

#[derive(Properties, PartialEq)]
struct ProbeProps {
    /// What the component should render. The test mutates it and then tells the
    /// component to re-read it.
    source: Rc<std::cell::Cell<usize>>,
    /// Handed the component's "re-read" closure when it mounts.
    exposed: Callback<Callback<()>>,
}

/// **The harness's proof that it can see what it claims to see.**
///
/// Every other test in this file rests on three things the server renderer
/// cannot do: mount into a document, run an effect, and re-render in place. If
/// this passes, all three work, so a passing render test below is evidence about
/// a component rather than about the harness.
///
/// It is deliberately not the C1 detector. Planting a hook-order defect and
/// checking that Yew's assertion fires is the right check — and it *was*
/// written and run — but on this target the panic it produces takes down the
/// whole test binary instead of failing one test, because a render happens on a
/// microtask and `wasm32-unknown-unknown` has no unwinding to catch it with. So
/// the detector is a documented manual procedure in `hook_order.rs`, with its
/// measured output, rather than a test that can never report.
#[wasm_bindgen_test]
async fn the_harness_runs_effects_and_second_renders() {
    let source = Rc::new(std::cell::Cell::new(0usize));
    let (setter, exposed) = setter_channel::<()>();
    let case = mount::<Counter>(ProbeProps {
        source: source.clone(),
        exposed,
    });
    flush().await;

    assert_eq!(
        case.texts(".count"),
        vec!["0".to_string()],
        "the component did not reach its first render:\n{}",
        case.inner_html()
    );

    // The effect published this while mounting, which the server renderer never
    // does — `components.rs` records two attempts to get an effect to run
    // through SSR, both dead ends.
    let reread = take_setter(&setter);
    source.set(1);
    reread.emit(());
    flush().await;
    assert_eq!(
        case.texts(".count"),
        vec!["1".to_string()],
        "the component did not re-render into the document:\n{}",
        case.inner_html()
    );

    source.set(2);
    reread.emit(());
    flush().await;
    assert_eq!(
        case.texts(".count"),
        vec!["2".to_string()],
        "a second state update did not reach the DOM, so this harness sees one \
         render and not a sequence of them:\n{}",
        case.inner_html()
    );
}

// ---------------------------------------------------------------------------
// C1 — the repair, on the real component
// ---------------------------------------------------------------------------

/// The real `QueuePanel`, with the queue in state so a test can make the exact
/// transition that broke it.
#[derive(Properties, PartialEq)]
struct PanelProps {
    initial: QueueSnapshot,
    exposed: Callback<Callback<QueueSnapshot>>,
}

#[function_component]
fn PanelRoot(props: &PanelProps) -> Html {
    let queue = use_state(|| props.initial.clone());
    {
        let queue = queue.clone();
        let exposed = props.exposed.clone();
        use_effect_with((), move |_| {
            exposed.emit(Callback::from(move |next: QueueSnapshot| queue.set(next)));
            || ()
        });
    }
    html! {
        <QueuePanel queue={(*queue).clone()}
                    on_jump={Callback::noop()} on_remove={Callback::noop()}
                    on_clear={Callback::noop()} on_move={Callback::noop()} />
    }
}

/// The state the defect needed and no existing test ever produced: a queue that
/// is **empty on the first render and populated on the second**.
///
/// That is `App`'s actual startup — `app.rs` seeds `QueueSnapshot::default()` and
/// the first snapshot arrives over IPC — and it is the only path on which C1 was
/// reachable, which is why it shipped. With a hook below the early return this
/// test panics with Yew's hook-order assertion; with the hook hoisted, the panel
/// appears and it passes. The two states are asserted separately so a regression
/// cannot hide behind "the panel rendered something".
#[wasm_bindgen_test]
async fn the_repair_is_exercised_on_the_real_component() {
    let (setter, exposed) = setter_channel::<QueueSnapshot>();
    let case = mount::<PanelRoot>(PanelProps {
        initial: QueueSnapshot::default(),
        exposed,
    });
    flush().await;

    assert_eq!(
        case.inner_html(),
        "",
        "the empty queue must render nothing at all — this early return is the \
         one C1 hid behind, and the state the app starts in"
    );

    let populate = take_setter(&setter);
    let queue = QueueSnapshot {
        tracks: playlist("q", 3).tracks,
        current: Some(0),
        ..QueueSnapshot::default()
    };
    populate.emit(queue);
    flush().await;

    assert_eq!(
        case.texts(".qrow").len(),
        3,
        "the panel did not render the rows it was handed:\n{}",
        case.inner_html()
    );
    assert!(
        case.inner_html().contains("UP NEXT (3)"),
        "the panel header does not reflect the queue:\n{}",
        case.inner_html()
    );
}

// ---------------------------------------------------------------------------
// H4 — PlayerBar progress on a track change
// ---------------------------------------------------------------------------

/// The track the wrapper is currently rendering, reachable from the test.
type TrackCell = Rc<RefCell<Option<Track>>>;

/// The real `PlayerBar`, rendering whatever track the shared cell holds.
///
/// `bar` is the props the *app* holds, and they are deliberately not the props
/// the bar re-seeds from: on a track change the app sends a new `track` but
/// `progress` stays at the bootstrap seed, which is the fact that made H4 a bug.
/// Keeping both — the live props in `state`, the original seed in a clone — is
/// what stops this wrapper from accidentally handing the bar a *correct* seed.
#[derive(Properties, PartialEq)]
struct BarProps {
    current: TrackCell,
    bar: PlayerBarProps,
    seed: PlayerBarProps,
    exposed: Callback<Callback<Option<Track>>>,
}

#[function_component]
fn BarRoot(props: &BarProps) -> Html {
    let current = use_state(|| props.current.borrow().clone());
    {
        let current = current.clone();
        let exposed = props.exposed.clone();
        use_effect_with((), move |_| {
            exposed.emit(Callback::from(move |next: Option<Track>| current.set(next)));
            || ()
        });
    }
    html! {
        <PlayerBar track={(*current).clone()}
                   state={props.bar.state}
                   progress={props.seed.progress}
                   volume={props.bar.volume}
                   shuffle={props.bar.shuffle}
                   repeat={props.bar.repeat}
                   liked={props.bar.liked}
                   on_open_artist={props.bar.on_open_artist.clone()}
                   on_volume={props.bar.on_volume.clone()} />
    }
}

fn bar_props() -> PlayerBarProps {
    PlayerBarProps {
        track: None,
        state: PlaybackState::Playing,
        // The bootstrap seed, at its default and never updated: `App`
        // deliberately does not subscribe to PROGRESS, and that is the fact that
        // made H4 a bug rather than a delayed update.
        progress: Progress::default(),
        volume: 1.0,
        shuffle: false,
        repeat: RepeatMode::Off,
        liked: false,
        on_open_artist: Callback::noop(),
        on_volume: Callback::noop(),
    }
}

/// **H4's behaviour, to the limit this harness can see it.**
///
/// `H4` was the seek bar showing the previous track's elapsed time and slider
/// range after a track change. `components.rs` pins the property the fix rests
/// on (`track_key` must distinguish two tracks); this drives the real component
/// through the transition and asserts what the bar does with it.
///
/// # What this test proves, and what it does not
///
/// It proves the bar **re-seeds**: the app's seed stays at its bootstrap value
/// across the change (that is the fact that made H4 a bug — `App` never
/// subscribes to PROGRESS), and after the change the bar settles on the *seed's*
/// numbers rather than the previous track's. That is why the seed is a clone
/// held across the whole test instead of being folded into the prop update: with
/// a fresh seed the re-seed would be indistinguishable from no re-seed at all.
///
/// It does **not** catch the mutation that removes the
/// track key from the effect's dependency list, and that is recorded here rather
/// than papered over. Measured, by building both: the stale `live` values are
/// masked on every render by `track_changed`, which stays true forever when the
/// effect never re-runs, so the bar falls back to the new track's own duration
/// and zeroed position — the same thing it shows for the one render before the
/// effect fires. The defect and the fix therefore differ for exactly one render,
/// and this harness cannot sample it: Yew renders and runs effects in the *same*
/// scheduler pass, which one microtask drains in full (measured, hop by hop).
/// Seeing that frame needs a real frame boundary — a browser runner — which is
/// the same purchase as the computed-style work, not this one.
#[wasm_bindgen_test]
async fn player_bar_reseeds_the_seek_bar_when_the_track_changes() {
    let mut a = track("a");
    a.duration = Some(200);
    let mut b = track("b");
    b.duration = Some(180);

    let cell: TrackCell = Rc::new(RefCell::new(Some(a.clone())));
    let (setter, exposed) = setter_channel::<Option<Track>>();

    let mut bar = bar_props();
    bar.track = Some(a.clone());
    let case = mount::<BarRoot>(BarProps {
        current: cell.clone(),
        bar: bar.clone(),
        // The bootstrap seed, held fixed: `App` sends a new `track` on a track
        // change and never touches `progress`.
        seed: bar.clone(),
        exposed,
    });
    flush().await;

    assert_eq!(
        case.texts(".seek-row .time"),
        vec!["0:00".to_string(), "0:00".to_string()],
        "the bar did not seed itself from the props it was given:\n{}",
        case.inner_html()
    );

    // What the PROGRESS subscription does while that track plays: the bar's own
    // local state, not a prop. This is the state that used to survive a track
    // change.
    let delivered = emit_for_test(
        events::PROGRESS,
        &Progress {
            position: 123.0,
            duration: 200.0,
        },
    );
    assert_eq!(
        delivered, 1,
        "expected the bar to be the only PROGRESS listener; {delivered} were \
         registered, so a component from an earlier test is still subscribed"
    );
    flush().await;

    assert_eq!(
        case.texts(".seek-row .time"),
        vec!["2:03".to_string(), "3:20".to_string()],
        "live progress did not reach the bar, so there is nothing for a track \
         change to strand:\n{}",
        case.inner_html()
    );

    let switch = take_setter(&setter);
    *cell.borrow_mut() = Some(b.clone());
    switch.emit(Some(b.clone()));
    flush().await;

    assert!(
        case.inner_html().contains("Title b"),
        "the new track never reached the bar, so nothing was tested:\n{}",
        case.inner_html()
    );
    assert_eq!(
        case.texts(".seek-row .time"),
        vec!["0:00".to_string(), "0:00".to_string()],
        "the bar is still showing per-track values from the track that just \
         ended (H4). It should have re-seeded from the props it was handed, \
         which is what makes the previous track's elapsed time and slider range \
         disappear:\n{}",
        case.inner_html()
    );
    assert_eq!(
        case.attr(".seek", "max"),
        "1",
        "the slider's range is still derived from something other than the seed \
         the app is sending"
    );
}
