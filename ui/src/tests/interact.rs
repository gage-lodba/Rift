//! Interaction tests: what a click and a keystroke actually do to a mounted
//! component.
//!
//! # Why this file exists
//!
//! `components.rs` renders each component once, through the server renderer, and
//! asserts the HTML it produces. `render.rs` reaches second renders by driving
//! props. Neither ever *dispatches an event*, which leaves an entire class of
//! defect invisible: a control wired to the wrong callback, a callback emitted
//! with the wrong index, a row that swallows a key aimed at a button inside it,
//! a menu whose keyboard path does not move focus. Those are not properties of
//! the rendered markup; they only exist while something is being clicked.
//!
//! So this is the third layer: the real Yew client renderer (as `render.rs`), in
//! the jsdom DOM (as `dom.rs`), driven by real `click()` and a real bubbling
//! `keydown`. `dom.rs` carries the helpers; the rule they follow is that a
//! selector which matches more than one element is a test bug and panics.
//!
//! # What is real here, and what is not
//!
//! - **Real:** the element's own `click()` — a bubbling, cancelable `MouseEvent`,
//!   the same thing a user produces and the same thing `check-dom-shim.cjs`
//!   proves works under this shim; a real `KeyboardEvent` built by the DOM's own
//!   constructor; Yew's delegated listener, its renderer, its scheduler.
//! - **Not real: default activation.** jsdom implements no default action for
//!   Enter or Space on a `<button>`, so pressing a key on a real button does
//!   *nothing* here. That is why every button-activation assertion below uses
//!   `click()` and every keydown assertion is about the *handler* that saw it.
//!   A test that expected Enter on a `<button>` to fire its `onclick` would be
//!   asserting a browser behaviour this harness does not have.
//! - **Not real: layout, and therefore computed style.** Unchanged from
//!   `dom.rs`: an inherited value from a real cascade is not observable.
//!
//! # The defects these tests guard
//!
//! Each of the first three groups pins something that shipped broken or was
//! fixed in the previous session, and each was mutation-tested (the fix reverted
//! and the test watched to fail):
//!
//! - `Menu`'s ArrowRight read a submenu's position among *submenus* instead of
//!   its index in `props.actions`, so with a plain item in front of it — every
//!   menu in the app — it expanded nothing.
//! - A keydown aimed at a row's own nested control bubbled to the row's handler,
//!   which activated the row *and* called `prevent_default`, cancelling the
//!   control. Enter on a track's kebab played the track instead of opening the
//!   menu; Enter on the queue's "Clear queue" collapsed the panel instead of
//!   clearing it. `components::key_on_nested_control` is the guard.

use std::cell::RefCell;
use std::rc::Rc;

use rift_types::{ArtistRef, QueueSnapshot, SongIndex, Track};
use wasm_bindgen_test::wasm_bindgen_test;
use yew::prelude::*;

use crate::components::{
    MenuAction, MenuButton, QueuePanel, QueuePanelProps, Sidebar, TrackList, TrackListCtx, View,
};
use crate::tests::dom::{flush, mount};
use crate::tests::harness::{playlist, track};

// ---------------------------------------------------------------------------
// Recording a callback
// ---------------------------------------------------------------------------

/// A callback that records every value it is emitted with, and the log behind it.
///
/// These tests are about *wiring* — which control reaches which callback, and
/// with what — so the callback itself is the observable. Nothing here crosses the
/// IPC bridge: `harness::test`/`settle` covers the `api.rs` call sites, and this
/// sits one level below them, where a component's props are the boundary.
fn record<T: 'static>() -> (Rc<RefCell<Vec<T>>>, Callback<T>) {
    let seen: Rc<RefCell<Vec<T>>> = Rc::new(RefCell::new(Vec::new()));
    let sink = seen.clone();
    (
        seen,
        Callback::from(move |value: T| sink.borrow_mut().push(value)),
    )
}

/// Everything recorded so far, as a value that can be compared.
fn seen<T: Clone>(log: &Rc<RefCell<Vec<T>>>) -> Vec<T> {
    log.borrow().clone()
}

// ---------------------------------------------------------------------------
// The popup menu
// ---------------------------------------------------------------------------

/// `MenuButton`'s props type is not re-exported outside `components` (only the
/// component and its action enum are), and `html!` is happy to name the
/// component without naming its props. So the test mounts this instead, which
/// passes nothing but the actions — everything under test, including the open
/// state, stays where it lives.
#[derive(Properties, PartialEq)]
struct MenuHarnessProps {
    actions: Vec<MenuAction>,
}

#[function_component]
fn MenuHarness(props: &MenuHarnessProps) -> Html {
    html! { <MenuButton actions={props.actions.clone()} /> }
}

/// The kebab that opens the menu, and the only `.ibtn` inside the harness.
const TRIGGER: &str = "button.ibtn[title=\"More\"]";

/// A plain menu item that records its own label when activated.
fn item(label: &'static str, log: &Rc<RefCell<Vec<&'static str>>>) -> MenuAction {
    let log = log.clone();
    MenuAction::Item {
        icon: "queue",
        label: label.to_string(),
        danger: false,
        cb: Callback::from(move |_: ()| log.borrow_mut().push(label)),
    }
}

/// A submenu whose options are `(id, name)`, recording the id that was chosen.
fn sub(label: &str, options: &[(&str, &str)], log: &Rc<RefCell<Vec<String>>>) -> MenuAction {
    let log = log.clone();
    MenuAction::Sub {
        icon: "plus",
        label: label.to_string(),
        options: options
            .iter()
            .map(|(id, name)| ((*id).to_string(), (*name).to_string()))
            .collect(),
        cb: Callback::from(move |id: String| log.borrow_mut().push(id)),
    }
}

/// The two plain items most menus in the app start with.
fn two_items(log: &Rc<RefCell<Vec<&'static str>>>) -> Vec<MenuAction> {
    vec![item("Add to queue", log), item("Play next", log)]
}

/// **A click on the kebab opens the menu and moves focus into it.**
///
/// Without the focus effect the menu opens with focus still on the trigger, so a
/// keyboard user has no way in — and, because every arrow key is handled by the
/// menu's own handler, none of the rest of this file would work either.
#[wasm_bindgen_test]
async fn a_click_opens_the_menu_and_focuses_its_first_item() {
    let fired = Rc::new(RefCell::new(Vec::new()));
    let case = mount::<MenuHarness>(MenuHarnessProps {
        actions: two_items(&fired),
    });
    flush().await;

    assert!(case.query(".menu").is_none(), "the menu must start closed");
    assert_eq!(case.attr(TRIGGER, "aria-expanded"), "false");

    case.click(TRIGGER);
    flush().await;

    assert_eq!(
        case.texts(".menu-item"),
        vec!["Add to queue".to_string(), "Play next".to_string()],
        "the menu did not render its actions in order:\n{}",
        case.inner_html()
    );
    assert_eq!(case.attr(TRIGGER, "aria-expanded"), "true");
    assert!(
        case.active_is(".menu-item"),
        "opening the menu must put focus on its first item; focus is on {}",
        case.active_description()
    );
    assert_eq!(case.active_text().trim(), "Add to queue");
}

/// **The arrow keys walk the items and wrap at both ends.**
///
/// The wrap is not decoration: without it, ArrowDown past the last item leaves
/// focus where it is and the menu looks frozen.
#[wasm_bindgen_test]
async fn the_arrow_keys_walk_the_menu_items_and_wrap_at_both_ends() {
    let fired = Rc::new(RefCell::new(Vec::new()));
    let case = mount::<MenuHarness>(MenuHarnessProps {
        actions: two_items(&fired),
    });
    flush().await;
    case.click(TRIGGER);
    flush().await;

    // Each keydown is dispatched at the element that currently has focus, which
    // is what a browser does and what the handler reads back through
    // `document.activeElement`.
    case.keydown_nth(".menu-item", 0, "ArrowDown");
    flush().await;
    assert_eq!(
        case.active_text().trim(),
        "Play next",
        "ArrowDown did not move to the next item; focus is on {}",
        case.active_description()
    );

    case.keydown_nth(".menu-item", 1, "ArrowDown");
    flush().await;
    assert_eq!(
        case.active_text().trim(),
        "Add to queue",
        "ArrowDown on the last item must wrap to the first; focus is on {}",
        case.active_description()
    );

    case.keydown_nth(".menu-item", 0, "ArrowUp");
    flush().await;
    assert_eq!(
        case.active_text().trim(),
        "Play next",
        "ArrowUp on the first item must wrap to the last; focus is on {}",
        case.active_description()
    );

    case.keydown_nth(".menu-item", 1, "Home");
    flush().await;
    assert_eq!(case.active_text().trim(), "Add to queue", "Home");

    case.keydown_nth(".menu-item", 0, "End");
    flush().await;
    assert_eq!(case.active_text().trim(), "Play next", "End");
}

/// **Escape closes the menu and hands focus back to what opened it.**
///
/// The focus half is the one that is easy to lose and invisible in markup: a
/// menu that closes without restoring focus drops a keyboard user at the top of
/// the document. `MenuButton` passes its kebab as the `anchor` for exactly this.
#[wasm_bindgen_test]
async fn escape_closes_the_menu_and_returns_focus_to_its_trigger() {
    let fired = Rc::new(RefCell::new(Vec::new()));
    let case = mount::<MenuHarness>(MenuHarnessProps {
        actions: two_items(&fired),
    });
    flush().await;
    case.click(TRIGGER);
    flush().await;
    assert!(
        case.active_is(".menu-item"),
        "the menu did not take focus on open, so there is nothing to escape from: {}",
        case.active_description()
    );

    case.keydown_nth(".menu-item", 0, "Escape");
    flush().await;

    assert!(
        case.query(".menu").is_none(),
        "Escape did not close the menu:\n{}",
        case.inner_html()
    );
    assert!(
        case.active_is(TRIGGER),
        "Escape must return focus to the element that opened the menu; focus is on {}",
        case.active_description()
    );
}

/// **ArrowRight expands the focused submenu when plain items precede it.**
///
/// This is the shape every menu in the app has, and the shape the defect needed:
/// `focused_submenu` used to return the focused element's position among
/// `[aria-haspopup="true"]` elements, which equals its `props.actions` index only
/// when no plain item comes first. Here the submenu is the *third* action, so the
/// buggy version read `0`, expanded action 0 — an item, which renders no
/// submenu — and this test sees no options at all.
#[wasm_bindgen_test]
async fn arrow_right_expands_the_focused_submenu_even_with_items_before_it() {
    let fired = Rc::new(RefCell::new(Vec::new()));
    let chosen = Rc::new(RefCell::new(Vec::new()));
    let case = mount::<MenuHarness>(MenuHarnessProps {
        actions: vec![
            item("Add to queue", &fired),
            item("Play next", &fired),
            sub(
                "Add to playlist",
                &[("p1", "Chill"), ("p2", "Focus")],
                &chosen,
            ),
        ],
    });
    flush().await;
    case.click(TRIGGER);
    flush().await;

    // Walk from the first item down to the submenu header.
    case.keydown_nth(".menu-item", 0, "ArrowDown");
    flush().await;
    case.keydown_nth(".menu-item", 1, "ArrowDown");
    flush().await;
    assert!(
        case.active_is("[aria-haspopup=\"true\"]"),
        "the submenu header should have focus before ArrowRight; focus is on {}",
        case.active_description()
    );

    case.keydown_nth(".menu-item", 2, "ArrowRight");
    flush().await;

    assert_eq!(
        case.texts(".menu-sub-option"),
        vec!["Chill".to_string(), "Focus".to_string()],
        "ArrowRight did not expand the focused submenu. A stale `expanded` index \
         that points at an item instead of the submenu produces exactly this:\n{}",
        case.inner_html()
    );
    assert_eq!(
        case.attr("[aria-haspopup=\"true\"]", "aria-expanded"),
        "true",
        "the header does not report itself expanded"
    );

    case.keydown_nth(".menu-item", 2, "ArrowLeft");
    flush().await;

    assert!(
        case.query(".menu-sub-list").is_none(),
        "ArrowLeft did not collapse the submenu:\n{}",
        case.inner_html()
    );
    assert_eq!(
        case.attr("[aria-haspopup=\"true\"]", "aria-expanded"),
        "false"
    );
}

/// **Enter activates the focused item and the menu closes.**
///
/// The close is part of the contract: an action menu that stays open after its
/// action ran leaves the user staring at a stale menu.
#[wasm_bindgen_test]
async fn enter_on_a_menu_item_fires_its_callback_and_closes_the_menu() {
    let fired = Rc::new(RefCell::new(Vec::new()));
    let case = mount::<MenuHarness>(MenuHarnessProps {
        actions: two_items(&fired),
    });
    flush().await;
    case.click(TRIGGER);
    flush().await;

    case.keydown_nth(".menu-item", 1, "Enter");
    flush().await;

    assert_eq!(
        seen(&fired),
        vec!["Play next"],
        "Enter on the second item must fire that item's callback, once"
    );
    assert!(
        case.query(".menu").is_none(),
        "activating an item must close the menu:\n{}",
        case.inner_html()
    );
}

/// **The backdrop closes the menu.**
///
/// The panel and the backdrop are siblings, and the panel stops propagation, so a
/// click on an item — or on the panel itself — can never be read as a click
/// outside. That second half is deliberately *not* asserted: with the current
/// structure there is no single-line change that would make "a click inside does
/// not close it" fail (moving the panel inside the backdrop is not enough, because
/// the panel's own `stop_propagation` runs first), and an assertion that cannot be
/// made to fail is the vacuous shape this harness exists to avoid. If the panel is
/// ever nested inside the backdrop *and* its guard dropped, this is the assertion
/// to add back.
#[wasm_bindgen_test]
async fn the_backdrop_closes_the_menu() {
    let fired = Rc::new(RefCell::new(Vec::new()));
    let case = mount::<MenuHarness>(MenuHarnessProps {
        actions: two_items(&fired),
    });
    flush().await;
    case.click(TRIGGER);
    flush().await;
    assert!(
        case.query(".menu").is_some(),
        "the trigger did not open the menu, so there is no backdrop to click:\n{}",
        case.inner_html()
    );

    case.click(".menu-backdrop");
    flush().await;
    assert!(
        case.query(".menu").is_none(),
        "the backdrop did not close the menu:\n{}",
        case.inner_html()
    );
}

// ---------------------------------------------------------------------------
// The queue panel
// ---------------------------------------------------------------------------

/// A queue of `n` tracks, optionally with one of them current.
fn queue_of(n: usize, current: Option<usize>) -> QueueSnapshot {
    QueueSnapshot {
        tracks: playlist("q", n).tracks,
        current,
        ..QueueSnapshot::default()
    }
}

/// A mounted `QueuePanel`, plus the logs it is asserted through — in this order:
/// jumps, removes, clears.
///
/// The order is spelled out because two of the three are `Vec<usize>` and a
/// swapped read would still compile; the tests unpack with `_` for the ones they
/// do not assert.
type PanelLogs = (
    crate::tests::dom::Mount<QueuePanel>,
    Rc<RefCell<Vec<usize>>>,
    Rc<RefCell<Vec<usize>>>,
    Rc<RefCell<Vec<()>>>,
);

/// Mount a `QueuePanel` over `queue`, with every callback recording.
fn panel(queue: QueueSnapshot) -> PanelLogs {
    let (jumps, on_jump) = record::<usize>();
    let (removes, on_remove) = record::<usize>();
    let (clears, on_clear) = record::<()>();
    let case = mount::<QueuePanel>(QueuePanelProps {
        queue,
        on_jump,
        on_remove,
        on_clear,
        on_move: Callback::noop(),
    });
    (case, jumps, removes, clears)
}

/// **A row's click jumps to that row, not to the first one.**
#[wasm_bindgen_test]
async fn clicking_a_queue_row_jumps_to_that_row() {
    let (case, jumps, _, _) = panel(queue_of(3, Some(0)));
    flush().await;

    case.click_nth(".qrow", 1);
    flush().await;

    assert_eq!(
        seen(&jumps),
        vec![1],
        "clicking the second row must jump to index 1:\n{}",
        case.inner_html()
    );
}

/// **A row's remove button removes that row and does not also jump to it.**
///
/// The row's `onclick` is on the whole row, so the remove button has to stop the
/// event; without that, one click both removes a track and starts playing it.
#[wasm_bindgen_test]
async fn clicking_remove_removes_that_row_and_does_not_jump() {
    let (case, jumps, removes, _) = panel(queue_of(3, Some(0)));
    flush().await;

    case.click_nth(".qrow .ibtn", 1);
    flush().await;

    assert_eq!(
        seen(&removes),
        vec![1],
        "the second row's remove button must remove index 1"
    );
    assert!(
        seen(&jumps).is_empty(),
        "the remove button's click reached the row as well, so it both removed \
         and jumped: `stop_propagation` is missing ({:?})",
        seen(&jumps)
    );
}

/// **Enter on a row jumps; Enter on the row's own remove button does not.**
///
/// The second half is `key_on_nested_control`. Without it the row's handler both
/// jumps and calls `prevent_default`, which in a browser cancels the button's own
/// Enter activation — the control becomes unreachable by keyboard. jsdom performs
/// no default activation, so nothing is removed here either way; what this pins
/// is that the *row* did not act.
#[wasm_bindgen_test]
async fn enter_on_a_queue_row_jumps_but_enter_on_its_remove_button_does_not() {
    let (case, jumps, removes, _) = panel(queue_of(3, Some(0)));
    flush().await;

    case.keydown_nth(".qrow", 1, "Enter");
    flush().await;
    assert_eq!(
        seen(&jumps),
        vec![1],
        "Enter on the row itself must jump to it:\n{}",
        case.inner_html()
    );

    case.keydown_nth(".qrow .ibtn", 1, "Enter");
    flush().await;
    assert_eq!(
        seen(&jumps),
        vec![1],
        "Enter on the row's remove button was handled by the row as well, so the \
         nested control is unreachable by keyboard: {:?}",
        seen(&jumps)
    );
    assert!(
        seen(&removes).is_empty(),
        "jsdom performs no default activation for Enter on a button, so this \
         harness cannot see the click a browser would produce: {:?}",
        seen(&removes)
    );
}

/// **The header collapses and re-expands on click.**
#[wasm_bindgen_test]
async fn clicking_the_queue_header_collapses_and_re_expands_the_list() {
    let (case, _, _, _) = panel(queue_of(3, Some(0)));
    flush().await;
    assert!(
        case.query(".queue-list").is_some(),
        "the queue starts expanded"
    );

    case.click(".queue-head");
    flush().await;
    assert!(
        case.query(".queue-list").is_none(),
        "clicking the header did not collapse the list:\n{}",
        case.inner_html()
    );
    assert_eq!(case.attr(".queue-head", "aria-expanded"), "false");

    case.click(".queue-head");
    flush().await;
    assert!(
        case.query(".queue-list").is_some(),
        "clicking the collapsed header did not expand the list again"
    );
    assert_eq!(case.attr(".queue-head", "aria-expanded"), "true");
}

/// **Enter on "Clear queue" does not collapse the panel, and clicking it clears.**
///
/// The Enter half is the shipped defect: the header toggles collapse on keydown
/// and the clear button is *inside* the header, so without
/// `key_on_nested_control` the keystroke collapsed the panel instead of reaching
/// the control. The click half then pins the other direction — the button stops
/// the event, so clearing does not also collapse.
#[wasm_bindgen_test]
async fn enter_on_clear_does_not_collapse_the_panel_and_a_click_clears_it() {
    let (case, _, _, clears) = panel(queue_of(3, Some(0)));
    flush().await;
    let clear = "button[title=\"Clear queue\"]";

    case.keydown(clear, "Enter");
    flush().await;
    assert!(
        case.query(".queue-list").is_some(),
        "Enter on the clear button collapsed the panel, so the header handled a \
         key that belonged to its own control:\n{}",
        case.inner_html()
    );
    assert!(
        seen(&clears).is_empty(),
        "Enter alone must not clear the queue — jsdom does not activate buttons \
         from the keyboard, and this harness must not pretend it does"
    );

    case.click(clear);
    flush().await;
    assert_eq!(
        seen(&clears).len(),
        1,
        "clicking the clear button must fire `on_clear` exactly once"
    );
    assert!(
        case.query(".queue-list").is_some(),
        "the clear button's click also collapsed the panel, so its \
         `stop_propagation` is missing"
    );
}

// ---------------------------------------------------------------------------
// A track row
// ---------------------------------------------------------------------------

/// One track's row, with every callback recording.
///
/// `TrackList`'s props type is not re-exported (only the component and its
/// context are), so the same wrapper trick as the menu applies.
#[derive(Properties, PartialEq)]
struct ListHarnessProps {
    ctx: TrackListCtx,
    tracks: Vec<Track>,
    on_play: Callback<usize>,
}

#[function_component]
fn ListHarness(props: &ListHarnessProps) -> Html {
    html! {
        <TrackList ctx={props.ctx.clone()} tracks={props.tracks.clone()}
                   on_play={props.on_play.clone()} />
    }
}

/// A mounted `TrackList` and the logs for the callbacks asserted below.
struct Rows {
    case: crate::tests::dom::Mount<ListHarness>,
    played: Rc<RefCell<Vec<usize>>>,
    liked: Rc<RefCell<Vec<String>>>,
    opened_artist: Rc<RefCell<Vec<String>>>,
    /// Callbacks that are only reachable through the row's own kebab menu, by
    /// name, so a test can see a menu item activate at all.
    menu: Rc<RefCell<Vec<&'static str>>>,
}

/// Mount `tracks` as a `TrackList`, `online` or not.
fn rows(tracks: Vec<Track>, online: bool) -> Rows {
    let (played, on_play) = record::<usize>();
    let (opened_artist, on_open_artist) = record::<String>();
    // The like callback records the id rather than the whole track: the id is
    // what the assertion is about, and a `Track` in a failure message buries it.
    let liked: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let sink = liked.clone();
    let on_like = Callback::from(move |t: Track| sink.borrow_mut().push(t.id));
    // The menu's first entry is "Play next", which is `on_play_next`. Recording it
    // by name is what lets `enter_on_a_menu_item_inside_a_row_does_not_play_it`
    // tell "the menu item fired" from "the row fired".
    let menu: Rc<RefCell<Vec<&'static str>>> = Rc::new(RefCell::new(Vec::new()));
    let sink = menu.clone();
    let on_play_next = Callback::from(move |_: Track| sink.borrow_mut().push("PLAY_NEXT"));

    let ctx = TrackListCtx {
        liked: Default::default(),
        playing_id: None,
        downloads: Default::default(),
        pinned_ids: Default::default(),
        online,
        playlists: Vec::new(),
        on_like,
        on_queue: Callback::noop(),
        on_add_to_playlist: Callback::noop(),
        on_open_artist,
        on_open_album: Callback::noop(),
        on_download: Callback::noop(),
        on_remove_download: Callback::noop(),
        on_play_next,
    };
    let case = mount::<ListHarness>(ListHarnessProps {
        ctx,
        tracks,
        on_play,
    });
    Rows {
        case,
        played,
        liked,
        opened_artist,
        menu,
    }
}

/// **Enter on a row's kebab does not play the track; Enter on the row does.**
///
/// This is the shipped defect in its original form. The kebab is a real button
/// inside a `role="button"` row; without `key_on_nested_control` the row's
/// handler played the track and cancelled the button's own activation, so a
/// keyboard user could not open the row menu at all.
#[wasm_bindgen_test]
async fn enter_on_a_rows_kebab_does_not_play_the_track_but_enter_on_the_row_does() {
    let r = rows(vec![track("a")], true);
    flush().await;

    r.case.keydown("button.ibtn[title=\"More\"]", "Enter");
    flush().await;
    assert!(
        seen(&r.played).is_empty(),
        "Enter on the row's kebab was handled by the row and played the track:\n{}",
        r.case.inner_html()
    );

    r.case.keydown("div.trow", "Enter");
    flush().await;
    assert_eq!(
        seen(&r.played),
        vec![0],
        "Enter on the row itself must play it:\n{}",
        r.case.inner_html()
    );
}

/// **Enter on a menu item *inside* a row must not also activate the row.**
///
/// `key_on_nested_control` decides by the keydown's **target tag**, against
/// `BUTTON | INPUT | SELECT | TEXTAREA | A`. A row's kebab menu renders *inside*
/// the row, and its items are `<div role="menuitem" tabindex="0">` — a `DIV`. So
/// the guard says "not a nested control" for a keystroke aimed at a menu item, the
/// row's own `onkeydown` runs, and Enter both runs the menu action and plays the
/// track. The tag list cannot see a role.
///
/// Found by the verifier against the previous session's `f19e2ab` and adopted
/// here, control case included. The item's `tag_name()` is asserted to be `DIV`
/// too, so the day menu items stop being DIVs this test says so instead of
/// quietly passing.
///
/// The control is what makes the negative below worth anything: the same dispatch
/// path, aimed at the row itself, must fire. Without it, a dispatcher that
/// delivered nothing at all would "pass" the real assertion.
#[wasm_bindgen_test]
async fn enter_on_a_menu_item_inside_a_row_does_not_play_the_row() {
    let r = rows(vec![track("a")], true);
    flush().await;

    // Control.
    r.case.keydown("div.trow", "Enter");
    flush().await;
    assert_eq!(
        seen(&r.played),
        vec![0],
        "control failed: Enter on the row itself must activate it, so the result \
         below would prove nothing:\n{}",
        r.case.inner_html()
    );
    r.played.borrow_mut().clear();

    // The real case: right-click the row to open its menu, focus the first item,
    // press Enter.
    r.case.contextmenu("div.trow");
    flush().await;

    let item = r.case.nth("[role='menuitem']", 0);
    assert_eq!(
        item.tag_name(),
        "DIV",
        "this test is only meaningful while a menu item is a DIV, which is the \
         thing the tag-based guard cannot see"
    );
    r.case.focus_nth("[role='menuitem']", 0);
    r.case.keydown_nth("[role='menuitem']", 0, "Enter");
    flush().await;

    assert!(
        seen(&r.menu).contains(&"PLAY_NEXT"),
        "the menu item's own action did not fire, so the interesting state was \
         never reached ({:?}):\n{}",
        seen(&r.menu),
        r.case.inner_html()
    );
    assert!(
        seen(&r.played).is_empty(),
        "the ROW also activated for a keystroke aimed at its own menu item — the \
         target is a `role=\"menuitem\"` DIV, which `key_on_nested_control`'s tag \
         list does not cover: {:?}",
        seen(&r.played)
    );
}

/// **Clicking the like button likes the track and does not play it.**
///
/// The mouse half of the same separation: `.trow-actions` stops the click, so
/// a button inside the row is not also a row click.
#[wasm_bindgen_test]
async fn clicking_like_likes_the_track_and_does_not_play_it() {
    let r = rows(vec![track("a")], true);
    flush().await;

    r.case.click("button.ibtn[title=\"Like\"]");
    flush().await;

    assert_eq!(
        seen(&r.liked),
        vec!["a".to_string()],
        "clicking Like must like the track it is on"
    );
    assert!(
        seen(&r.played).is_empty(),
        "clicking Like also played the track, so the row's click handler saw it: {:?}",
        seen(&r.played)
    );
}

/// **The album page's row shows the heart for a song liked from the artist
/// page.**
///
/// YouTube Music names one song with two video ids: the artist page's Top songs
/// row is the art track, the album page's row is the official music video, and
/// neither carries the other's id (recorded live — "Around the World" is
/// `Jb6gcoR266U` on Daft Punk's page and `K0HSD_i2DvA` on *Homework*). So a like
/// stored from one surface has an id the other surface's row never renders, and
/// the heart has to follow the *song*. Against the id-only set this replaced,
/// the row below rendered unliked — which is the bug this test exists for.
#[wasm_bindgen_test]
async fn a_row_liked_from_another_surface_shows_the_heart() {
    // Liked from the artist page: the art track, with the credit and no
    // duration, which is how that surface renders the row.
    let artist_page_row = Track {
        title: "Around the World".to_string(),
        artist: "Daft Punk".to_string(),
        album: Some("Homework".to_string()),
        duration: None,
        artists: vec![ArtistRef {
            id: Some("UCRr1xG_2WIDs18a6cIiCxeA".to_string()),
            name: "Daft Punk".to_string(),
        }],
        album_id: Some("MPREb_dMHztPsrDgN".to_string()),
        ..track("Jb6gcoR266U")
    };
    // ...and the album page renders the same song as the music video, with the
    // duration and no credit (album rows leave the byline to the page header).
    let album_page_row = Track {
        title: "Around the World".to_string(),
        artist: String::new(),
        album: Some("Homework".to_string()),
        duration: Some(430),
        artists: Vec::new(),
        album_id: Some("MPREb_dMHztPsrDgN".to_string()),
        ..track("K0HSD_i2DvA")
    };

    let ctx = TrackListCtx {
        liked: Rc::new(SongIndex::new(&[artist_page_row])),
        playing_id: None,
        downloads: Default::default(),
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
    let case = mount::<ListHarness>(ListHarnessProps {
        ctx,
        tracks: vec![album_page_row],
        on_play: Callback::noop(),
    });
    flush().await;

    assert_eq!(
        case.classes(".trow-actions button.ibtn"),
        vec!["ibtn liked".to_string(), "ibtn".to_string()],
        "the album page's row must show the heart the artist page's like set:\n{}",
        case.inner_html()
    );
    assert_eq!(
        case.attr(".trow-actions button.ibtn", "title"),
        "Unlike",
        "and the heart has to be the *un-like* affordance, not decoration:\n{}",
        case.inner_html()
    );
}

/// **An offline, undownloaded row is inert, and says so.**
///
/// The row renders and is focusable, so the only thing stopping playback is the
/// `available` check in its activation callback; `aria-disabled` is what tells a
/// screen reader the same thing.
#[wasm_bindgen_test]
async fn an_offline_undownloaded_row_plays_nothing() {
    let r = rows(vec![track("a")], false);
    flush().await;

    assert_eq!(
        r.case.attr("div.trow", "aria-disabled"),
        "true",
        "an unavailable row must report itself disabled"
    );

    r.case.keydown("div.trow", "Enter");
    flush().await;
    r.case.click("div.trow");
    flush().await;

    assert!(
        seen(&r.played).is_empty(),
        "an offline, undownloaded row played anyway: {:?}",
        seen(&r.played)
    );
}

/// **An artist credit is a real button and opens the artist.**
///
/// `icons::artist_links` used to emit a `span.link`, which is not focusable and
/// not activatable; it is a `button` now, and this pins both the tag and the
/// wiring in one place. The album column next to it is a `button.link` for the
/// same reason.
#[wasm_bindgen_test]
async fn an_artist_credit_is_a_button_that_opens_the_artist() {
    let mut t = track("a");
    t.artists = vec![ArtistRef {
        id: Some("artist-1".to_string()),
        name: "The Artist".to_string(),
    }];
    let r = rows(vec![t], true);
    flush().await;

    assert_eq!(
        r.case.texts(".trow-artist button.link"),
        vec!["The Artist".to_string()],
        "the artist credit is not a button:\n{}",
        r.case.inner_html()
    );

    r.case.click(".trow-artist button.link");
    flush().await;

    assert_eq!(
        seen(&r.opened_artist),
        vec!["artist-1".to_string()],
        "clicking an artist credit must open that artist"
    );
    assert!(
        seen(&r.played).is_empty(),
        "clicking an artist credit also played the track: {:?}",
        seen(&r.played)
    );
}

/// **Row artwork is decorative.**
///
/// `alt=""` is what tells a screen reader to skip an image; without the attribute
/// the track's cover URL is read out as the track's name. Cheap to assert, and
/// invisible in every other kind of test.
#[wasm_bindgen_test]
async fn row_artwork_is_decorative() {
    let (case, _, _, _) = panel(queue_of(1, Some(0)));
    flush().await;

    assert_eq!(
        case.attr("img.qrow-cover", "alt"),
        "",
        "the queue row's cover needs `alt=\"\"`"
    );
}

// ---------------------------------------------------------------------------
// The rest of the menu's keyboard surface
// ---------------------------------------------------------------------------

/// **Enter on a submenu header expands it, and chooses nothing.**
///
/// The header's own Enter arm is separate from its click handler, and expanding
/// is not choosing: without this arm a keyboard user can reach the submenu and
/// not open it, which is the same class as the ArrowRight defect next door.
#[wasm_bindgen_test]
async fn enter_on_a_submenu_header_expands_it_without_choosing_anything() {
    let chosen = Rc::new(RefCell::new(Vec::new()));
    let case = mount::<MenuHarness>(MenuHarnessProps {
        actions: vec![sub(
            "Add to playlist",
            &[("p1", "Chill"), ("p2", "Focus")],
            &chosen,
        )],
    });
    flush().await;
    case.click(TRIGGER);
    flush().await;

    assert!(
        case.active_is("[aria-haspopup=\"true\"]"),
        "the only action is the submenu header, so opening should have focused \
         it; focus is on {}",
        case.active_description()
    );

    case.keydown_nth("[role='menuitem']", 0, "Enter");
    flush().await;

    assert_eq!(
        case.texts(".menu-sub-option"),
        vec!["Chill".to_string(), "Focus".to_string()],
        "Enter on the submenu header did not expand it:\n{}",
        case.inner_html()
    );
    assert!(
        seen(&chosen).is_empty(),
        "expanding a submenu must not choose one of its options: {:?}",
        seen(&chosen)
    );
}

/// **Enter on a sub-option chooses it, closes the menu, and restores focus.**
///
/// The whole point of the submenu: the id that reaches the callback has to be the
/// option that was chosen, not the submenu that contains it.
#[wasm_bindgen_test]
async fn enter_on_a_sub_option_chooses_it_and_closes_the_menu() {
    let chosen = Rc::new(RefCell::new(Vec::new()));
    let case = mount::<MenuHarness>(MenuHarnessProps {
        actions: vec![sub(
            "Add to playlist",
            &[("p1", "Chill"), ("p2", "Focus")],
            &chosen,
        )],
    });
    flush().await;
    case.click(TRIGGER);
    flush().await;
    case.click("[aria-haspopup=\"true\"]");
    flush().await;
    assert_eq!(
        case.texts(".menu-sub-option").len(),
        2,
        "the submenu did not expand"
    );

    case.keydown_nth(".menu-sub-option", 1, "Enter");
    flush().await;

    assert_eq!(
        seen(&chosen),
        vec!["p2".to_string()],
        "the *second* option is the one that was activated"
    );
    assert!(
        case.query(".menu").is_none(),
        "choosing an option must close the menu:\n{}",
        case.inner_html()
    );
    assert!(
        case.active_is(TRIGGER),
        "choosing an option must hand focus back to the kebab that opened the \
         menu; focus is on {}",
        case.active_description()
    );
}

/// **ArrowLeft from a sub-option collapses the submenu and returns focus to its
/// header.**
///
/// Collapsing without moving focus would leave a keyboard user focused on an
/// element that no longer exists, and the next Tab would start from the top of
/// the document.
#[wasm_bindgen_test]
async fn arrow_left_from_a_sub_option_collapses_it_and_focuses_the_header() {
    let chosen = Rc::new(RefCell::new(Vec::new()));
    let case = mount::<MenuHarness>(MenuHarnessProps {
        actions: vec![sub(
            "Add to playlist",
            &[("p1", "Chill"), ("p2", "Focus")],
            &chosen,
        )],
    });
    flush().await;
    case.click(TRIGGER);
    flush().await;
    case.click("[aria-haspopup=\"true\"]");
    flush().await;

    case.focus_nth(".menu-sub-option", 0);
    case.keydown_nth(".menu-sub-option", 0, "ArrowLeft");
    flush().await;

    assert!(
        case.query(".menu-sub-list").is_none(),
        "ArrowLeft did not collapse the submenu:\n{}",
        case.inner_html()
    );
    assert!(
        case.active_is("[aria-haspopup=\"true\"]"),
        "ArrowLeft must put focus back on the submenu's header; focus is on {}",
        case.active_description()
    );
}

/// **Moving focus out of the menu closes it.**
///
/// The popup is dismissed when focus leaves its subtree. Without that, tabbing
/// away from an open menu leaves it floating over the page with nothing focused
/// inside it, and Escape has nothing to act on.
#[wasm_bindgen_test]
async fn moving_focus_out_of_the_menu_closes_it() {
    let fired = Rc::new(RefCell::new(Vec::new()));
    let case = mount::<MenuHarness>(MenuHarnessProps {
        actions: two_items(&fired),
    });
    flush().await;
    case.click(TRIGGER);
    flush().await;
    assert!(case.query(".menu").is_some(), "the menu did not open");

    // The trigger is outside the menu, so this is exactly the "Tab to the next
    // control" case.
    case.focus_nth(TRIGGER, 0);
    flush().await;

    assert!(
        case.query(".menu").is_none(),
        "focus left the menu and it stayed open:\n{}",
        case.inner_html()
    );
}

// ---------------------------------------------------------------------------
// Keyboard reorder (Alt+Up / Alt+Down)
// ---------------------------------------------------------------------------

/// **Alt+ArrowDown on a focused queue row moves that row down.**
///
/// The handler reads the row's `data-index` from the DOM at key time and requires
/// `altKey`, so this needs both the attribute and [`crate::tests::dom::keydown_alt_at`].
/// It is the first mounted coverage of the keyboard reorder path; the index math
/// behind it is table-tested in `reorder.rs`.
#[wasm_bindgen_test]
async fn alt_arrow_down_on_a_focused_queue_row_moves_it() {
    let (moves, on_move) = record::<(usize, usize)>();
    let case = mount::<QueuePanel>(QueuePanelProps {
        queue: queue_of(3, Some(0)),
        on_jump: Callback::noop(),
        on_remove: Callback::noop(),
        on_clear: Callback::noop(),
        on_move,
    });
    flush().await;

    case.focus_nth(".qrow", 0);
    case.keydown_alt(".qrow[data-index=\"0\"]", "ArrowDown");
    flush().await;

    assert_eq!(
        seen(&moves),
        vec![(0, 1)],
        "Alt+ArrowDown on the first row must move it to index 1"
    );

    // And the boundary: there is nothing above the first row, so a move up is a
    // no-op rather than a move to index 0.
    case.keydown_alt(".qrow[data-index=\"0\"]", "ArrowUp");
    flush().await;
    assert_eq!(
        seen(&moves),
        vec![(0, 1)],
        "Alt+ArrowUp on the first row must emit nothing: {:?}",
        seen(&moves)
    );

    // Without Alt the list must ignore the keystroke — the plain arrow key is
    // how a user moves their place in the list, not their position in the order.
    case.keydown(".qrow[data-index=\"0\"]", "ArrowDown");
    flush().await;
    assert_eq!(
        seen(&moves),
        vec![(0, 1)],
        "ArrowDown without Alt must not reorder: {:?}",
        seen(&moves)
    );
}

/// **Every queue row advertises the shortcut and carries the index it reorders.**
///
/// `data-index` is what the handler reads (a stale captured index would move
/// whichever row took its place), and `aria-keyshortcuts` is how a screen reader
/// knows the shortcut exists at all.
#[wasm_bindgen_test]
async fn queue_rows_expose_the_keyboard_shortcut_and_their_index() {
    let (case, _, _, _) = panel(queue_of(3, Some(0)));
    flush().await;

    for i in 0..3 {
        assert_eq!(
            case.attr_nth(".qrow", i, "data-index"),
            i.to_string(),
            "row {i} carries the wrong index"
        );
        assert_eq!(
            case.attr_nth(".qrow", i, "aria-keyshortcuts"),
            "Alt+ArrowUp Alt+ArrowDown",
            "row {i} does not advertise the reorder shortcut"
        );
    }
}
// ---------------------------------------------------------------------------
// The keyboard reorder's live region
// ---------------------------------------------------------------------------

/// A `QueuePanel` whose queue is in state, and whose `on_move` therefore applies
/// the move.
///
/// Both halves matter. The panel only *asks* for a reorder (`on_move`), and the
/// reorder's focus handoff and announcement run in an effect that waits for the
/// order it computed against to actually change — so a test that records the
/// callback without re-rendering with the new order leaves that whole branch
/// unexecuted, and the announcement and the focus move both stay unproven.
#[derive(Properties, PartialEq)]
struct QueueRootProps {
    initial: QueueSnapshot,
    moves: Rc<RefCell<Vec<(usize, usize)>>>,
}

#[function_component]
fn QueueRoot(props: &QueueRootProps) -> Html {
    let queue = use_state(|| props.initial.clone());
    let on_move = {
        let queue = queue.clone();
        let moves = props.moves.clone();
        Callback::from(move |(from, to): (usize, usize)| {
            moves.borrow_mut().push((from, to));
            let mut next = (*queue).clone();
            if from < next.tracks.len() {
                let track = next.tracks.remove(from);
                let to = to.min(next.tracks.len());
                next.tracks.insert(to, track);
            }
            queue.set(next);
        })
    };
    html! {
        <QueuePanel queue={(*queue).clone()}
                    on_jump={Callback::noop()} on_remove={Callback::noop()}
                    on_clear={Callback::noop()} on_move={on_move} />
    }
}

/// **A keyboard reorder is announced, once it has landed, and focus follows it.**
///
/// The region is asserted **empty at mount** first, and that is the point rather
/// than a formality: a `role="status"` element that appears already carrying its
/// first message is the one case screen readers routinely miss, because there is
/// no change for them to observe. It is rendered empty from the start for exactly
/// that reason, and this is the assertion that keeps it that way.
///
/// The order swap is what makes the rest reachable: the announcement and the
/// focus handoff both live in an effect keyed on the row ids, so a test that only
/// records `on_move` never executes either of them.
#[wasm_bindgen_test]
async fn a_keyboard_reorder_is_announced_once_it_has_landed_and_focus_follows_it() {
    let moves: Rc<RefCell<Vec<(usize, usize)>>> = Rc::new(RefCell::new(Vec::new()));
    let case = mount::<QueueRoot>(QueueRootProps {
        initial: queue_of(3, Some(0)),
        moves: moves.clone(),
    });
    flush().await;

    let region = ".sr-only[role='status']";
    let text = || case.texts(region).join("").trim().to_string();
    assert_eq!(
        case.texts(region).len(),
        1,
        "the queue needs exactly one live region:\n{}",
        case.inner_html()
    );
    assert!(
        text().is_empty(),
        "the live region must be mounted empty — a status element that appears \
         already carrying its first message is the case screen readers miss; it \
         reads {:?} at mount",
        text()
    );

    case.focus_nth(".qrow", 0);
    case.keydown_alt(".qrow[data-index=\"0\"]", "ArrowDown");
    flush().await;

    assert_eq!(
        seen(&moves),
        vec![(0, 1)],
        "Alt+ArrowDown on the first row must ask for a move to index 1"
    );
    assert_eq!(
        text(),
        "Title q0 moved to position 2 of 3",
        "the landed move was not announced (or was announced wrong):\n{}",
        case.inner_html()
    );
    assert!(
        case.active_is(".qrow[data-index=\"1\"]"),
        "focus must follow the moved row, or the next Alt+ArrowDown moves the row \
         that took its place; focus is on {}",
        case.active_description()
    );
}
// ---------------------------------------------------------------------------
// The sidebar host: the same rule, on a different row type
// ---------------------------------------------------------------------------

/// `SidebarProps` is not re-exported outside `components` (only the component and
/// its `View` are), and `html!` builds a component's props without naming their
/// type — the same wrapper trick as `MenuHarness` and `ListHarness`. Only the two
/// inputs the test varies are props; everything else is a no-op or a recorder.
#[derive(Properties, PartialEq)]
struct SidebarHarnessProps {
    playlists: Vec<(String, String, usize)>,
    log: Rc<RefCell<Vec<String>>>,
}

#[function_component]
fn SidebarHarness(props: &SidebarHarnessProps) -> Html {
    let record = |name: &'static str| {
        let log = props.log.clone();
        Callback::from(move |_: String| log.borrow_mut().push(name.to_owned()))
    };
    let on_nav = {
        let log = props.log.clone();
        Callback::from(move |_: View| log.borrow_mut().push("NAV".to_owned()))
    };
    html! {
        <Sidebar view={View::Home}
                 playlists={props.playlists.clone()}
                 {on_nav}
                 on_new_playlist={Callback::noop()}
                 on_import_playlist={Callback::noop()}
                 on_import_file={Callback::noop()}
                 on_rename_playlist={record("MENU_RENAME")}
                 on_delete_playlist={record("MENU_DELETE")}
                 on_export_playlist={record("MENU_EXPORT")}
                 on_move_playlist={Callback::noop()}
                 playing_source={Option::<String>::None}
                 is_playing={false} />
    }
}

/// **The sidebar half of the nested-control finding: the same rule, a different
/// row type.**
///
/// `SidebarPlaylist` renders its `Menu` inside a `role="button"` row whose
/// `onkeydown` calls the same tag-based `key_on_nested_control`, so the sidebar is
/// the second host of the defect the verifier found on `TrackRow` — and the only
/// one this suite never mounted. Adopted from the verifier's scratch test, control
/// case included: Enter aimed at the row itself must navigate, or the negative
/// below would pass for a dispatcher that delivered nothing.
#[wasm_bindgen_test]
async fn enter_on_a_playlist_menu_item_does_not_navigate() {
    let log: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let case = mount::<SidebarHarness>(SidebarHarnessProps {
        playlists: vec![("pl-1".to_string(), "My Playlist".to_string(), 3)],
        log: log.clone(),
    });
    flush().await;

    // `.side-item[data-index="0"]`, not the verifier's first `.side-item`: the
    // sidebar's own nav entries share the class, and only a playlist row carries
    // `data-index`, so this selects the row under test exactly.
    let row = ".side-item[data-index=\"0\"]";

    // Control.
    case.keydown(row, "Enter");
    flush().await;
    assert_eq!(
        seen(&log),
        vec!["NAV".to_string()],
        "control failed: Enter on the playlist row itself must open the \
         playlist:\n{}",
        case.inner_html()
    );
    log.borrow_mut().clear();

    // The real case: open the row's own menu from its kebab, focus the first item,
    // press Enter.
    case.click(".side-item[data-index=\"0\"] button[title=\"More\"]");
    flush().await;
    assert_eq!(
        case.attr(
            ".side-item[data-index=\"0\"] button[title=\"More\"]",
            "aria-expanded"
        ),
        "true",
        "the kebab did not open the row's menu:\n{}",
        case.inner_html()
    );

    let item = case.nth("[role='menuitem']", 0);
    assert_eq!(
        item.tag_name(),
        "DIV",
        "this test is only meaningful while a menu item is a DIV, which is the \
         thing the tag-based guard cannot see"
    );
    case.focus_nth("[role='menuitem']", 0);
    case.keydown_nth("[role='menuitem']", 0, "Enter");
    flush().await;

    let entries = seen(&log);
    assert!(
        entries.iter().any(|e| e.starts_with("MENU_")),
        "the menu item's own action did not fire, so the interesting state was \
         never reached: {entries:?}"
    );
    assert!(
        !entries.iter().any(|e| e == "NAV"),
        "the sidebar ROW also navigated for a keystroke aimed at its own menu item \
         — the same role=\"menuitem\"-DIV blind spot as `TrackRow`, on a different \
         row type: {entries:?}"
    );
    assert!(
        case.query(".menu").is_none(),
        "activating an item must close the menu:\n{}",
        case.inner_html()
    );
}
