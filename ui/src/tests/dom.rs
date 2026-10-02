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

use wasm_bindgen::JsCast;
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
        let resolved = js_sys::Promise::resolve(&wasm_bindgen::JsValue::UNDEFINED);
        let _ = JsFuture::from(resolved).await;
    }
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
        let el = self.query(selector).unwrap_or_else(|| {
            panic!(
                "no element matches `{selector}`, so `{name}` cannot be read:\n{}",
                self.inner_html()
            )
        });
        el.get_attribute(name)
            .unwrap_or_else(|| panic!("`{selector}` has no `{name}` attribute"))
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
