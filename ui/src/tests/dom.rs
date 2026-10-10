//! The DOM half of the harness: a real Yew **client** renderer, driven under
//! Node with a DOM supplied by `jsdom`.
//!
//! # Why this exists
//!
//! `harness.rs` covers everything above the IPC bridge by replacing the bridge.
//! This module covers the one thing that replacement cannot: what a component
//! does on its **second** render. Yew's server renderer drives a component
//! instance exactly once and never runs an effect (measured — the dead ends are
//! recorded in `components.rs`), and that is precisely where two defects live:
//!
//! - **`C1`** — a hook below an early return. The first render (empty queue)
//!   skips it; the second demands it, and Yew's `assert_hook_context` panics
//!   with *"Hooks are called conditionally."*
//! - **`H4`** — PlayerBar's live progress surviving a track change, which is a
//!   re-seed effect that has to run on the second render to be correct at all.
//!
//! # How the DOM gets here
//!
//! `ui/tests/dom-shim.cjs` installs jsdom's globals into Node before the test
//! module loads, via `NODE_OPTIONS=--require`. That is the only injection point
//! the runner offers, and it is enough: `web-sys` looks globals up lazily, so a
//! `document` that exists before the first component mounts is all the renderer
//! needs. See `ui/README.md` for the invocation, and for why this is a *Node*
//! dependency rather than a Cargo one.
//!
//! # Why a microtask flush is not a race
//!
//! Yew delays its scheduler to the end of the microtask queue
//! (`yew-0.21.0/src/scheduler.rs:229`, `spawn_local(async { start_now() })`), so
//! a `Renderer` mount returns *before* anything is on screen. [`flush`] yields
//! to that queue a fixed number of times, which drains the scheduler — including
//! the renders its own effects queue, because the scheduler loops until its
//! queues are empty. It is deterministic: no timers, no frames, no sleeping. A
//! test that changes its answer between two identical runs is a bug in the test,
//! not flake to be retried.
//!
//! # What is still not covered
//!
//! **Computed style.** jsdom has no layout engine, so `getComputedStyle` here
//! answers from inline and stylesheet declarations only, and the white
//! `buttonface` album text — a value *inherited* from a `<button>` through a
//! real cascade — is not observable. `ui/README.md` says so and this module does
//! not imply otherwise. Catching that still needs a browser.

use std::cell::RefCell;
use std::rc::Rc;

use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{Element, HtmlElement};
use yew::prelude::*;

/// How many microtask turns [`flush`] yields for.
///
/// Comfortably more than the scheduler needs — a mount, an effect, and the
/// re-render that effect triggers is about three turns — and each turn is cheap,
/// so the margin covers a render cascade rather than guessing at the usual
/// depth.
const FLUSH_TURNS: usize = 16;

/// A mounted component and the element it renders into.
///
/// Holding the handle keeps the component mounted; dropping it unmounts, and
/// [`Drop`] removes the container so one test's DOM cannot be seen by the next.
pub(crate) struct Mount<C>
where
    C: BaseComponent + 'static,
{
    root: HtmlElement,
    /// `AppHandle` unmounts its component when dropped, so this exists to keep
    /// the render alive for as long as `Mount` does.
    _handle: AppHandle<C>,
}

/// Render `C` with `props` into a fresh container appended to the document body.
///
/// The container is a bare `div`, so a test cannot accidentally depend on
/// styling or an id the app applies elsewhere.
pub(crate) fn mount<C>(props: C::Properties) -> Mount<C>
where
    C: BaseComponent + 'static,
{
    let root = container();
    // `Renderer::render` installs `console_error_panic_hook` (once per process),
    // which is deliberately left in place: it is what puts a component's panic —
    // including Yew's own "Hooks are called conditionally." — on stderr next to
    // the failing test rather than only in the `catch_unwind` payload.
    let handle = yew::Renderer::<C>::with_root_and_props(root.clone().into(), props).render();
    Mount {
        root,
        _handle: handle,
    }
}

/// A `div` appended to the document body.
pub(crate) fn container() -> HtmlElement {
    let body = web_sys::window()
        .expect("the DOM shim installs `window` before any test runs")
        .document()
        .expect("the DOM shim installs `document` before any test runs")
        .body()
        .expect("the shim's document has a body");
    let root = body
        .owner_document()
        .expect("a body belongs to a document")
        .create_element("div")
        .expect("div is a valid element name")
        .dyn_into::<HtmlElement>()
        .expect("create_element('div') yields an HtmlElement");
    body.append_child(&root).expect("append the container");
    root
}

/// Let the Yew scheduler run to quiescence.
///
/// Yields to the microtask queue [`FLUSH_TURNS`] times. Each turn lets whatever
/// the previous one queued run, so a mount, its effects, and the re-renders
/// those effects cause have all completed before this resolves.
///
/// # Panics during a flush
///
/// A panic raised by a render happens on a microtask, and that is the one place
/// it cannot be caught: `wasm32-unknown-unknown` has no unwinding
/// (`wasm_bindgen::__rt::maybe_catch_unwind` is the identity function without
/// `panic = "unwind"` — `wasm-bindgen-0.2.128/src/rt/mod.rs:1090`), so the
/// process aborts and Node reports `RuntimeError: unreachable` with no test
/// results. [`install_panic_reporter`] is what makes that survivable *as
/// information*: the message reaches the log even though the run is over. No
/// test may depend on catching a panic here.
pub(crate) async fn flush() {
    install_panic_reporter();
    for _ in 0..FLUSH_TURNS {
        hop_once().await;
    }
}

/// One turn of the JS job queue.
///
/// [`flush`] is the right default — it waits for the scheduler to be quiescent.
/// This is for the tests that have to *count* turns instead of outwaiting them,
/// which is the only way to ask how coarse a scheduler pass is. See
/// `rerender.rs`, where that measurement is what bounds `H4`'s open residual.
pub(crate) async fn hop() {
    install_panic_reporter();
    hop_once().await;
}

/// A single `await` on an already-resolved promise: one microtask drain.
async fn hop_once() {
    let resolved = js_sys::Promise::resolve(&wasm_bindgen::JsValue::UNDEFINED);
    let _ = JsFuture::from(resolved).await;
}

/// A panic hook, as `std::panic::take_hook` hands one over.
type Hook = Box<dyn Fn(&std::panic::PanicHookInfo<'_>) + Sync + Send>;

/// Report a panic message through `console.error` the moment it happens.
///
/// Measured need: with the hook installed, a component that panics mid-flush
/// still aborts the process, but the log says
/// `panicked at … Hooks are called conditionally.` instead of only
/// `RuntimeError: unreachable`. Nothing asserts on the message — there is no
/// point at which a test could, since the process is already gone — so this
/// exists purely so a red CI run says what happened.
///
/// Installed once, for the process: `set_hook` must not be called while a panic
/// is in flight, and `Yew` installs `console_error_panic_hook` on the first
/// render, so swapping per test would race it. The previous hook is chained so
/// the framework's own reporting still happens.
fn install_panic_reporter() {
    use std::sync::{Once, OnceLock};
    static PREVIOUS: OnceLock<Hook> = OnceLock::new();
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let _ = PREVIOUS.set(std::panic::take_hook());
        std::panic::set_hook(Box::new(|info| {
            let one_line = info
                .to_string()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            web_sys::console::error_1(&one_line.into());
            if let Some(previous) = PREVIOUS.get() {
                previous(info);
            }
        }));
    });
}

impl<C> Mount<C>
where
    C: BaseComponent + 'static,
{
    /// The rendered HTML, exactly as the DOM sees it.
    pub(crate) fn inner_html(&self) -> String {
        self.root.inner_html()
    }

    /// `querySelector` scoped to this container: one element or none.
    pub(crate) fn query(&self, selector: &str) -> Option<Element> {
        self.root.query_selector(selector).ok().flatten()
    }

    /// The `textContent` of every element matching `selector`, in document
    /// order.
    ///
    /// A single-element read is `texts(..)` with the caller asserting the length
    /// it expects, which is better than a helper that silently takes the first
    /// match when a selector was meant to be unique.
    pub(crate) fn texts(&self, selector: &str) -> Vec<String> {
        self.elements(selector)
            .into_iter()
            .map(|el| el.text_content().unwrap_or_default())
            .collect()
    }

    /// An attribute of the single element matching `selector`.
    pub(crate) fn attr(&self, selector: &str, name: &str) -> String {
        self.attr_nth(selector, 0, name)
    }

    /// An attribute of the `index`th element matching `selector`.
    ///
    /// For the same reason [`Mount::nth`] exists: a list's rows share their
    /// selector, and "this row carries this attribute" is a per-row question.
    pub(crate) fn attr_nth(&self, selector: &str, index: usize, name: &str) -> String {
        let el = self.nth(selector, index);
        el.get_attribute(name).unwrap_or_else(|| {
            panic!(
                "`{selector}` at index {index} has no `{name}` attribute:\n{}",
                self.inner_html()
            )
        })
    }

    /// The `class` attribute of every element matching `selector`, in document
    /// order, as an empty string where an element has none.
    ///
    /// For a second-render assertion about *which* of several similar rows
    /// changed: `texts(..)` cannot express that when the rows share their text,
    /// and a list of class strings localises the change to a row without a
    /// per-row query helper.
    pub(crate) fn classes(&self, selector: &str) -> Vec<String> {
        self.elements(selector)
            .into_iter()
            .map(|el| el.get_attribute("class").unwrap_or_default())
            .collect()
    }

    /// The single element matching `selector`, or a panic naming the selector and
    /// the rendered HTML.
    ///
    /// [`Mount::query`] is the "or none" version. This is the "exactly one, or the
    /// test is asking a different question than it thinks" version, and every
    /// interaction helper below is built on it so that a selector which
    /// accidentally matches two rows fails loudly instead of driving the first.
    pub(crate) fn one(&self, selector: &str) -> Element {
        let mut found = self.elements(selector);
        assert_eq!(
            found.len(),
            1,
            "expected exactly one element matching `{selector}`, found {}:\n{}",
            found.len(),
            self.inner_html()
        );
        found.pop().expect("checked len == 1")
    }

    /// The `index`th (0-based) element matching `selector`, in document order.
    pub(crate) fn nth(&self, selector: &str, index: usize) -> Element {
        let found = self.elements(selector);
        let count = found.len();
        found.into_iter().nth(index).unwrap_or_else(|| {
            panic!(
                "no element {index} matching `{selector}` ({count} found):\n{}",
                self.inner_html()
            )
        })
    }

    /// Click the single element matching `selector`.
    ///
    /// The element's own `click()`, not a hand-built `MouseEvent`: that is the
    /// real, bubbling, cancelable event a user's click produces, and it is the
    /// same thing `check-dom-shim.cjs` proves works under this shim.
    pub(crate) fn click(&self, selector: &str) {
        click(&self.one(selector));
    }

    /// Click the `index`th element matching `selector`.
    ///
    /// For a list whose rows share a selector — a queue row and its remove
    /// button — which is how a row-specific callback is told apart from a
    /// row-agnostic one.
    pub(crate) fn click_nth(&self, selector: &str, index: usize) {
        click(&self.nth(selector, index));
    }

    /// Dispatch a bubbling `keydown` for `key` at the single element matching
    /// `selector`.
    pub(crate) fn keydown(&self, selector: &str, key: &str) {
        keydown_at(&self.one(selector), key);
    }

    /// Dispatch a bubbling `keydown` for `key` at the `index`th element matching
    /// `selector`.
    pub(crate) fn keydown_nth(&self, selector: &str, index: usize, key: &str) {
        keydown_at(&self.nth(selector, index), key);
    }

    /// Dispatch a bubbling `contextmenu` at the single element matching
    /// `selector` — a right-click, which is how a row's own kebab menu opens.
    pub(crate) fn contextmenu(&self, selector: &str) {
        contextmenu_at(&self.one(selector));
    }

    /// Dispatch a bubbling `keydown` for `key` **with Alt held** at the single
    /// element matching `selector`.
    pub(crate) fn keydown_alt(&self, selector: &str, key: &str) {
        keydown_alt_at(&self.one(selector), key);
    }

    /// Focus the `index`th element matching `selector`.
    ///
    /// The indexed form rather than a single-element one because the elements a
    /// test needs to focus are usually a set — a menu's items — and `one(..)`
    /// would refuse to pick.
    pub(crate) fn focus_nth(&self, selector: &str, index: usize) {
        focus(&self.nth(selector, index));
    }

    /// The document's focused element, if the document has one.
    pub(crate) fn active(&self) -> Option<Element> {
        web_sys::window()?.document()?.active_element()
    }

    /// Whether the document's focused element is one of the elements matching
    /// `selector`.
    ///
    /// Deliberately not [`Mount::one`]: "is focus inside this group" is the
    /// question every menu test asks, and the group is usually more than one
    /// element. A selector that matches nothing is `false`, not a panic, so this
    /// can be used to assert that focus left somewhere.
    pub(crate) fn active_is(&self, selector: &str) -> bool {
        let Some(active) = self.active() else {
            return false;
        };
        self.elements(selector)
            .iter()
            .any(|el| el.is_same_node(Some(&active)))
    }

    /// The focused element's text.
    pub(crate) fn active_text(&self) -> String {
        self.active()
            .map(|el| el.text_content().unwrap_or_default())
            .unwrap_or_default()
    }

    /// What is focused, for the message of a failed focus assertion.
    ///
    /// jsdom answers `activeElement` and `focus()` properly but has no layout, so
    /// a bare `assert!(false)` would leave the actual destination unrecorded —
    /// and where focus went is the entire subject of these tests.
    ///
    /// The "not inside this mount" note is not decoration, and the reason is not
    /// jsdom: measured, jsdom moves `activeElement` to `<body>` when the focused
    /// element — or the container holding it — is removed. So a test that passes
    /// cannot inherit focus. A test that **panics** is different:
    /// `wasm32-unknown-unknown` has no unwinding, so the panic never runs `Drop`
    /// for [`Mount`], its container stays in the document, and whatever it had
    /// focused stays focused. The next test then reads a foreign node that looks
    /// exactly like the one it was looking for. That only happens down a red run,
    /// which is when a misleading message costs the most.
    pub(crate) fn active_description(&self) -> String {
        match self.active() {
            None => "nothing (document.activeElement is null)".to_string(),
            Some(el) => format!(
                "<{}> class={:?} text={:?}{}",
                el.tag_name().to_lowercase(),
                el.get_attribute("class").unwrap_or_default(),
                el.text_content().unwrap_or_default().trim(),
                if self.root.contains(Some(&el)) {
                    String::new()
                } else {
                    " — NOT inside this mount: left behind by an earlier test, \
                     whose mount a panic kept from being dropped"
                        .to_string()
                },
            ),
        }
    }

    fn elements(&self, selector: &str) -> Vec<Element> {
        let found = self
            .root
            .query_selector_all(selector)
            .expect("a valid CSS selector");
        (0..found.length())
            .map(|i| {
                found
                    .item(i)
                    .expect("index below length")
                    .dyn_into::<Element>()
                    .expect("a NodeList of elements contains Elements")
            })
            .collect()
    }
}

/// Click `el` through the element's own `click()`.
fn click(el: &Element) {
    el.clone()
        .dyn_into::<HtmlElement>()
        .unwrap_or_else(|_| {
            panic!(
                "cannot click a <{}>: `click()` is an HtmlElement method",
                el.tag_name().to_lowercase()
            )
        })
        .click();
}

/// Focus `el`, as a browser would when Tab or a click reaches it.
fn focus(el: &Element) {
    el.clone()
        .dyn_into::<HtmlElement>()
        .unwrap_or_else(|_| {
            panic!(
                "cannot focus a <{}>: `focus()` is an HtmlElement method",
                el.tag_name().to_lowercase()
            )
        })
        .focus()
        .expect("focus() on an element that is in the document");
}

/// Dispatch a real, bubbling, cancelable `keydown` for `key` at `el`.
///
/// # Why the DOM's constructor, and not `KeyboardEventInit`
///
/// `web_sys::KeyboardEvent::new_with_keyboard_event_init_dict` needs the
/// `KeyboardEventInit` web-sys feature, and nothing in this crate's graph turns it
/// on — Yew enables `KeyboardEvent`, not its init dictionary. Turning it on would
/// mean editing `ui/Cargo.toml`, which is outside this test tree, and web-sys
/// features are global rather than test-scoped: the app build would link the same
/// bindings, so "the shipped wasm is unchanged" would stop being something that
/// was measured. Reaching the DOM's own constructor through `Reflect` costs
/// nothing, adds no dependency, and yields an event that `instanceof
/// KeyboardEvent` accepts — which a plain `Event` carrying a `key` property is
/// not, and Yew's `onkeydown` casts.
///
/// `bubbles: true` is load-bearing rather than decorative: Yew 0.21 delegates
/// events to the subtree root, so a non-bubbling event dispatched on a row never
/// reaches the handler that is listening for it.
pub(crate) fn keydown_at(el: &Element, key: &str) {
    el.dispatch_event(&dom_event("KeyboardEvent", "keydown", Some(key), false))
        .expect("dispatching a keydown at an element in the document");
}

/// Dispatch a bubbling `keydown` for `key` **with Alt held** at `el`.
///
/// The keyboard reorder shortcut is Alt+Up/Down, and its handler refuses a
/// keystroke without `altKey` (`reorder.rs`), so a plain [`keydown_at`] cannot
/// reach it at all. Kept as a separate entry point rather than a flag on the
/// common one so that "this test needs a modifier" is visible at the call site.
pub(crate) fn keydown_alt_at(el: &Element, key: &str) {
    el.dispatch_event(&dom_event("KeyboardEvent", "keydown", Some(key), true))
        .expect("dispatching an Alt-keydown at an element in the document");
}

/// Dispatch a real, bubbling, cancelable `contextmenu` at `el` — a right-click.
///
/// The same constructor route as [`keydown_at`], and the third event kind this
/// shim is now proven to carry: `click`, `keydown`, `contextmenu`. `contextmenu`
/// is not incidental — it is the row-level way into a kebab menu, so a test that
/// needs a menu open *inside a row* has to dispatch one rather than press a
/// button.
pub(crate) fn contextmenu_at(el: &Element) {
    el.dispatch_event(&dom_event("MouseEvent", "contextmenu", None, false))
        .expect("dispatching a contextmenu at an element in the document");
}

/// A bubbling, cancelable event of `type_`, built by the DOM's own `constructor`.
///
/// `key` and `alt` are set only for keyboard events; both constructors accept the
/// same `{ key?, altKey?, bubbles, cancelable }` init object, and one helper is
/// what keeps the three above from drifting.
fn dom_event(constructor: &str, type_: &str, key: Option<&str>, alt: bool) -> web_sys::Event {
    let init = js_sys::Object::new();
    let fields: [(&str, JsValue); 2] = [("bubbles", JsValue::TRUE), ("cancelable", JsValue::TRUE)];
    for (name, value) in fields {
        js_sys::Reflect::set(&init, &JsValue::from_str(name), &value)
            .expect("a fresh object accepts a boolean init field");
    }
    if let Some(key) = key {
        js_sys::Reflect::set(&init, &JsValue::from_str("key"), &JsValue::from_str(key))
            .expect("a fresh object accepts `key`");
    }
    if alt {
        js_sys::Reflect::set(&init, &JsValue::from_str("altKey"), &JsValue::TRUE)
            .expect("a fresh object accepts `altKey`");
    }

    let constructor = js_sys::Reflect::get(&js_sys::global(), &JsValue::from_str(constructor))
        .expect("reading a DOM constructor off the global object cannot fail")
        .dyn_into::<js_sys::Function>()
        .expect(
            "the DOM shim mirrors jsdom's DOM constructors onto the global object; \
             if this fails, `check-dom-shim.cjs` should have said so",
        );
    js_sys::Reflect::construct(
        &constructor,
        &js_sys::Array::of2(&JsValue::from_str(type_), &init),
    )
    .expect("`new <Constructor>(type, init)` cannot fail for a valid init")
    .unchecked_into::<web_sys::Event>()
}

// ---------------------------------------------------------------------------
// Driving a component from above
// ---------------------------------------------------------------------------

/// A closure a mounted component publishes so the test can drive it.
///
/// `use_state` is per-instance and a test cannot reach into it. This is the
/// other direction: the component hands out a `Callback` when it mounts and the
/// test calls it — the same thing the app does to a component when state above
/// it changes, not a way of poking a hook.
///
/// Lives here rather than in `render.rs` because it is now used by two files, and
/// a second copy of a helper is how two harnesses drift apart.
pub(crate) type Setter<T> = Rc<RefCell<Option<Callback<T>>>>;

/// A `Setter` plus the callback that fills it, for a component's props.
pub(crate) fn setter_channel<T: 'static>() -> (Setter<T>, Callback<Callback<T>>) {
    let cell: Setter<T> = Rc::new(RefCell::new(None));
    let publish = cell.clone();
    (
        cell,
        Callback::from(move |cb: Callback<T>| *publish.borrow_mut() = Some(cb)),
    )
}

/// The closure `setter` holds, or a failure saying the component never published
/// one.
pub(crate) fn take_setter<T: Clone + 'static>(setter: &Setter<T>) -> Callback<T> {
    setter
        .borrow()
        .clone()
        .expect("the mounted component never published its setter")
}

impl<C> Drop for Mount<C>
where
    C: BaseComponent + 'static,
{
    fn drop(&mut self) {
        if let Some(parent) = self.root.parent_node() {
            let _ = parent.remove_child(&self.root);
        }
    }
}
