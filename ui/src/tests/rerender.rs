//! Second-render coverage beyond the two components that had it, and the
//! measurement that bounds what a second render can ever see.
//!
//! `render.rs` covers the two defects (`C1`, `H4`) on the components they lived
//! in: `QueuePanel` going from empty to populated, and `PlayerBar` re-seeding
//! across a track change. This file covers what those two rest on, in both
//! directions.
//!
//! - **A keyed list re-decorated from above.** `TrackList` is driven through Yew's
//!   diffing with one row's decoration moved to the other row, so a decoration
//!   computed from a stale index or pinned to the first track fails here. It is
//!   `H4`'s bug one level down — state keyed on identity, updated when the
//!   identity above it changes — and unlike `QueuePanel` it has more than one row,
//!   so "the right one changed" is a real question.
//! - **How coarse one scheduler pass is.** The residual `H4` cannot reach is
//!   stated in `render.rs` and `README.md` as "a single render is not
//!   observable". That is a claim about Yew's scheduler, so it is measured here,
//!   hop by hop, instead of asserted in prose: see
//!   [`one_microtask_hop_carries_a_render_and_its_effects`]. If a Yew upgrade ever
//!   splits a render from its effects across two hops, that test fails and says
//!   the window `H4` lives in has become samplable — which is the point of
//!   writing it down as a test rather than a paragraph.

use std::cell::RefCell;
use std::rc::Rc;

use rift_types::{DownloadState, Track};
use wasm_bindgen_test::wasm_bindgen_test;
use yew::prelude::*;

use crate::components::{TrackList, TrackListCtx};
use crate::tests::dom::{flush, hop, mount, setter_channel, take_setter};
use crate::tests::harness::track;

// ---------------------------------------------------------------------------
// How coarse is one scheduler pass?
// ---------------------------------------------------------------------------

/// A component that records the two halves of a scheduler pass: one line when its
/// body runs, one when its effect runs.
///
/// Instrumentation, not a component under test. The question below is whether a
/// test can ever observe the first line without the second.
#[derive(Properties, PartialEq)]
struct PassProps {
    dep: u32,
    log: Rc<RefCell<Vec<String>>>,
}

#[function_component]
fn PassProbe(props: &PassProps) -> Html {
    props.log.borrow_mut().push(format!("render {}", props.dep));
    {
        let log = props.log.clone();
        use_effect_with(props.dep, move |dep| {
            log.borrow_mut().push(format!("effect {dep}"));
            || ()
        });
    }
    html! { <span class="probe">{ props.dep.to_string() }</span> }
}

/// Holds the probe's input in state, because a prop can only change from above.
#[derive(Properties, PartialEq)]
struct PassRootProps {
    log: Rc<RefCell<Vec<String>>>,
    exposed: Callback<Callback<u32>>,
}

#[function_component]
fn PassRoot(props: &PassRootProps) -> Html {
    let dep = use_state(|| 0u32);
    {
        let dep = dep.clone();
        let exposed = props.exposed.clone();
        use_effect_with((), move |_| {
            exposed.emit(Callback::from(move |next: u32| dep.set(next)));
            || ()
        });
    }
    html! { <PassProbe dep={*dep} log={props.log.clone()} /> }
}

/// **The measurement behind `H4`'s open residual: one hop is a whole pass.**
///
/// `H4`'s defect and its fix differ for exactly one render — the render after the
/// track changes and before the re-seed effect has run — so a test could catch the
/// mutation that removes the effect's dependency if it could sample that render.
/// `flush()` cannot: it waits for quiescence by construction. The narrower
/// question is whether *any* sampling is possible, and it reduces to how much work
/// one turn of the JS job queue carries.
///
/// Measured here: it carries the render, the DOM update, and the effects that
/// render queued, all of them. A test can only resume between turns of the job
/// queue, so there is no point at which it can run while "rendered but effects not
/// yet run" is true — which is why the `H4` detector is a documented manual
/// procedure in `hook_order.rs` rather than a test, and why seeing that frame
/// needs a real browser (an animation frame is not a microtask).
///
/// Written as a test rather than a comment because it is a claim about *Yew*, not
/// about this repository: if a future version commits the render and runs effects
/// on a later hop, this fails, and the failure message is the instruction to go
/// and write the `H4` detector that just became possible.
#[wasm_bindgen_test]
async fn one_microtask_hop_carries_a_render_and_its_effects() {
    let log: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let (setter, exposed) = setter_channel::<u32>();
    let case = mount::<PassRoot>(PassRootProps {
        log: log.clone(),
        exposed,
    });
    flush().await;

    assert_eq!(
        case.texts(".probe"),
        vec!["0".to_string()],
        "the probe did not mount:\n{}",
        case.inner_html()
    );

    // The change is made from a *settled* state, so the only work in flight is the
    // one the change schedules. The two assertions before the hop are what make
    // this a measurement rather than a restatement: without them, a `hop()` that
    // accidentally waited for quiescence would pass the rest of the test.
    log.borrow_mut().clear();
    take_setter(&setter).emit(1);
    assert!(
        log.borrow().is_empty(),
        "the state update was processed synchronously, so nothing below is \
         measuring the scheduler: {:?}",
        log.borrow().clone()
    );
    assert_eq!(
        case.texts(".probe"),
        vec!["0".to_string()],
        "the document changed without yielding, so nothing below is measuring the \
         scheduler:\n{}",
        case.inner_html()
    );

    hop().await;
    let after_one_hop = log.borrow().clone();

    assert!(
        after_one_hop.contains(&"effect 1".to_string()),
        "one turn of the job queue did not carry the render *and* its effect, so \
         there is now a sample point between them and the H4 detector this \
         harness records as impossible has become possible. Observed after one \
         hop: {after_one_hop:?}"
    );
    assert_eq!(
        case.texts(".probe"),
        vec!["1".to_string()],
        "the re-render did not reach the document within one hop:\n{}",
        case.inner_html()
    );
    assert_eq!(
        after_one_hop,
        vec!["render 1".to_string(), "effect 1".to_string()],
        "a prop change should produce exactly one render and one effect run"
    );
}

// ---------------------------------------------------------------------------
// A keyed list re-decorated from above
// ---------------------------------------------------------------------------

/// What the rows are told about themselves, held above the list so a test can
/// move it from one track to the other.
#[derive(Clone, PartialEq)]
struct Decor {
    /// The track id currently playing.
    playing: String,
    /// The track id currently liked.
    liked: String,
    /// The track id with an offline copy; empty means none.
    downloaded: String,
}

impl Decor {
    fn new(playing: &str, liked: &str, downloaded: &str) -> Self {
        Self {
            playing: playing.to_string(),
            liked: liked.to_string(),
            downloaded: downloaded.to_string(),
        }
    }
}

#[derive(Properties, PartialEq)]
struct ListRootProps {
    tracks: Vec<Track>,
    exposed: Callback<Callback<Decor>>,
}

#[function_component]
fn ListRoot(props: &ListRootProps) -> Html {
    let decor = use_state(|| Decor::new("a", "a", ""));
    {
        let decor = decor.clone();
        let exposed = props.exposed.clone();
        use_effect_with((), move |_| {
            exposed.emit(Callback::from(move |next: Decor| decor.set(next)));
            || ()
        });
    }
    let mut downloaded = std::collections::HashSet::new();
    if !decor.downloaded.is_empty() {
        downloaded.insert(decor.downloaded.clone());
    }
    let ctx = TrackListCtx {
        liked_ids: [decor.liked.clone()].into_iter().collect(),
        playing_id: Some(decor.playing.clone()),
        downloads: DownloadState {
            downloaded,
            ..DownloadState::default()
        },
        pinned_ids: Default::default(),
        online: true,
        playlists: Vec::new(),
        on_like: Callback::noop(),
        on_queue: Callback::noop(),
        on_add_to_playlist: Callback::noop(),
        on_open_artist: Callback::noop(),
        on_open_album: Callback::noop(),
        on_download: Callback::noop(),
        on_remove_download: Callback::noop(),
        on_play_next: Callback::noop(),
    };
    html! {
        <TrackList ctx={ctx} tracks={props.tracks.clone()} on_play={Callback::noop()} />
    }
}

/// **Every decoration follows the props to the other row, in one update.**
///
/// Two rows, decorated from above, then all three decorations moved from the
/// first track to the second at once. Asserting on class lists rather than on
/// text is deliberate: both rows render the same shape, so which row changed is
/// the assertion, and a single-element `attr(..)` could not express it.
///
/// The defect it guards is a decoration derived from a *position* instead of the
/// track's identity — the same mistake as `H4`'s, one level down, where the
/// component below is keyed on a track and the value above it is not.
#[wasm_bindgen_test]
async fn every_row_decoration_follows_the_props_across_a_re_render() {
    let (setter, exposed) = setter_channel::<Decor>();
    let case = mount::<ListRoot>(ListRootProps {
        tracks: vec![track("a"), track("b")],
        exposed,
    });
    flush().await;

    assert_eq!(
        case.classes(".trow"),
        vec!["trow playing".to_string(), "trow".to_string()],
        "the first row should start out playing:\n{}",
        case.inner_html()
    );
    assert_eq!(
        case.classes(".trow-actions button.ibtn"),
        vec![
            "ibtn liked".to_string(),
            "ibtn".to_string(),
            "ibtn".to_string(),
            "ibtn".to_string(),
        ],
        "the first row's like button should start out liked (one like button and \
         one kebab per row, in document order):\n{}",
        case.inner_html()
    );
    assert_eq!(
        case.classes(".trow-dl button"),
        vec!["ibtn".to_string(), "ibtn".to_string()],
        "neither row starts out downloaded:\n{}",
        case.inner_html()
    );

    take_setter(&setter).emit(Decor::new("b", "b", "a"));
    flush().await;

    assert_eq!(
        case.classes(".trow"),
        vec!["trow".to_string(), "trow playing".to_string()],
        "the playing marker did not move to the second row:\n{}",
        case.inner_html()
    );
    assert_eq!(
        case.classes(".trow-actions button.ibtn"),
        vec![
            "ibtn".to_string(),
            "ibtn".to_string(),
            "ibtn liked".to_string(),
            "ibtn".to_string(),
        ],
        "the liked marker did not move to the second row:\n{}",
        case.inner_html()
    );
    assert_eq!(
        case.classes(".trow-dl button"),
        vec!["ibtn accent".to_string(), "ibtn".to_string()],
        "the download affordance did not follow the track: the first row should \
         now offer to *remove* an offline copy (accent) and the second should \
         still offer to download it:\n{}",
        case.inner_html()
    );
}
