//! Frontend test harness.
//!
//! Run with `cargo test -p rift-ui --target wasm32-unknown-unknown` (see
//! `ui/README.md` for the runner and its version pin). Nothing in this tree may
//! depend on a running app, a network, or a real `window` — the same rule the
//! backend's client tests follow by using a loopback `TcpListener` rather than
//! a mock crate.
//!
//! # What is real, and what is stubbed
//!
//! The seam sits at the Tauri bridge, deliberately:
//!
//! ```text
//!   call site -> api::fire / api::invoke -> [ spawn ] -> [ tauri_invoke ] -> backend
//!                ^ real: the serialization   ^ stub      ^ stub: needs
//!                  and the error routing        (queue)     window.__TAURI__
//! ```
//!
//! Stubbing *there* means a test sees the exact JS argument object the real
//! `invoke` would hand the IPC layer — built by the real
//! `serde_wasm_bindgen::Serializer::json_compatible()` call, not by a
//! hand-written expectation. That is the point. Hand-written `json!({...})`
//! argument shapes are what broke: `play_track` sent a `"radio"` key the
//! backend had stopped accepting, and nothing could see it. A harness that
//! restated the expected arguments in the test would have missed it too.
//!
//! # Why the futures are queued rather than spawned
//!
//! `api::fire` hands its future to `spawn`, which is `spawn_local` in a real
//! build. A test cannot wait for `spawn_local`: the executor belongs to the
//! page's event loop, and under `wasm-bindgen-test` nothing drives it — a test
//! that fires a command and yields sees an empty call log forever. (That is not
//! a guess; it is what the first version of this harness did, and every async
//! test failed identically with "expected exactly one IPC call, got 0".)
//!
//! So `spawn` is replaced too: it pushes the future onto a queue that
//! [`Test::settle`] drains to completion, on the test's own stack. No event
//! loop, no yielding, no timers — which also makes the tests synchronous
//! functions, and therefore immune to a runner that interleaves them.
//!
//! # Why the bridge stub is hand-written
//!
//! `api.rs` compiles its two `extern "C"` imports only when *not* testing, and
//! re-exports these instead. There is no mocking crate to agree with, mirroring
//! how the backend's client tests avoid one.

use std::cell::RefCell;

use serde::Serialize;
use serde_json::Value;
use wasm_bindgen::prelude::*;
use yew::Callback;

use crate::api;

// ---------------------------------------------------------------------------
// Recording
// ---------------------------------------------------------------------------

/// What a routed command resolves to.
#[derive(Clone)]
enum Outcome {
    /// Resolve with this JSON value (what the backend command returned).
    Ok(Value),
    /// Reject with this JS value, as `catch` hands it back to `invoke`.
    Err(JsValue),
}

/// One `invoke` call, as the IPC layer would have received it.
#[derive(Clone)]
pub(crate) struct Call {
    pub(crate) cmd: String,
    /// The argument object, read back through the same descriptor path the IPC
    /// layer uses (`serde_wasm_bindgen::from_value`), so a test sees what a JS
    /// consumer sees rather than what Rust believes it wrote.
    pub(crate) args: Option<Value>,
}

thread_local! {
    /// Calls recorded since the last [`Test::settle`] drained its queue.
    static CALLS: RefCell<Vec<Call>> = const { RefCell::new(Vec::new()) };

    /// Routes, by command, set up by the test currently running. Merged rather
    /// than replaced, so a route set by one test cannot be lost mid-run by
    /// another; each test routes only the commands it fires.
    static ROUTES: RefCell<Vec<(String, Outcome)>> = const { RefCell::new(Vec::new()) };

    /// The error sink to install while a test's queue drains.
    static SINK: RefCell<Option<Callback<String>>> = const { RefCell::new(None) };

    /// Futures queued by [`spawn`], in order.
    static QUEUE: RefCell<Vec<Pin<Box<dyn Future<Output = ()>>>>> = const { RefCell::new(Vec::new()) };
}

use std::future::Future;
use std::pin::Pin;

/// A single test's view of the bridge.
pub(crate) struct Test {
    /// Commands this test fires, in order — the filter that keeps one test from
    /// seeing another's calls, and the expectation [`Test::settle`] checks.
    expected: Vec<&'static str>,
    /// The error sink to install while this test's queue drains.
    sink: RefCell<Option<Callback<String>>>,
}

/// Start a test that fires exactly `commands`, in order.
///
/// The list is not decoration. It is what [`Test::settle`] asserts against, so a
/// call site that fires a command the test forgot to expect fails loudly instead
/// of being filtered out of the log and passing.
#[must_use]
pub(crate) fn test(commands: &[&'static str]) -> Test {
    CALLS.with(|c| c.borrow_mut().clear());
    QUEUE.with(|q| q.borrow_mut().clear());
    api::unset_error_sink();
    Test {
        expected: commands.to_vec(),
        sink: RefCell::new(None),
    }
}

impl Test {
    /// Make `cmd` resolve with `value`. Unrouted commands resolve with `null`,
    /// which is what a `Result<(), String>` command's `Ok(())` becomes.
    pub(crate) fn respond<T: Serialize>(&self, cmd: &str, value: T) {
        let v = serde_json::to_value(value).expect("test response serializes");
        route(cmd, Outcome::Ok(v));
    }

    /// Make `cmd` reject with a string message, as a backend `Err` does.
    pub(crate) fn fail(&self, cmd: &str, msg: &str) {
        route(cmd, Outcome::Err(JsValue::from_str(msg)));
    }

    /// Make `cmd` reject with a real JavaScript `Error` rather than a string.
    ///
    /// Tauri stringifies a `Result<_, String>` for the rejection, so this is not
    /// the common case — but `invoke`'s error path has to survive whatever the
    /// webview hands back, and its non-string fallback (`{e:?}`) is otherwise
    /// never executed by any test.
    pub(crate) fn fail_with_js_error(&self, cmd: &str, msg: &str) {
        route(cmd, Outcome::Err(js_sys::Error::new(msg).into()));
    }

    /// Install `sink` as the app's error sink while this test's queue drains.
    pub(crate) fn on_error(&self, sink: Callback<String>) {
        *self.sink.borrow_mut() = Some(sink);
    }

    /// Drain every queued future, then return the calls this test made.
    ///
    /// Runs the futures on the test's own stack, so there is nothing to wait
    /// for and nothing to be late: when this returns, every command the call
    /// sites fired has been recorded. Panics if the test fired a command it did
    /// not declare, or declared one it never fired — both mean the test and the
    /// call sites have drifted apart, and both are silent failures otherwise.
    pub(crate) fn settle(&self) -> Vec<Call> {
        api::unset_error_sink();
        match self.sink.borrow().clone() {
            Some(cb) => api::set_error_sink(cb),
            None => api::unset_error_sink(),
        }

        let depth = DRAIN_DEPTH.with(|d| {
            let mut d = d.borrow_mut();
            *d += 1;
            *d
        });
        // Nested drains would double-poll a future, so a second `settle` on the
        // same stack is a test bug rather than something to accommodate.
        assert_eq!(depth, 1, "settle must not be called re-entrantly");

        let waker = noop_waker();
        let mut cx = Context::from_waker(&waker);
        // Bounded only to turn a runaway future into a failure instead of a
        // hang. Each turn polls everything queued, and the work here is finite:
        // `invoke` completes on its first poll because the stubbed transport is
        // ready, so a healthy test needs exactly one turn.
        for _ in 0..MAX_DRAIN_TURNS {
            let pending: Vec<_> = QUEUE.with(|q| q.borrow_mut().drain(..).collect());
            if pending.is_empty() {
                break;
            }
            for mut fut in pending {
                match fut.as_mut().poll(&mut cx) {
                    Poll::Ready(()) => {}
                    // Re-queue: a future that yields must be polled again.
                    Poll::Pending => QUEUE.with(|q| q.borrow_mut().push(fut)),
                }
            }
        }
        DRAIN_DEPTH.with(|d| *d.borrow_mut() -= 1);

        let calls = CALLS.with(|c| c.borrow().clone());
        let fired: Vec<&str> = calls.iter().map(|c| c.cmd.as_str()).collect();
        assert_eq!(
            fired, self.expected,
            "the commands fired do not match the ones this test declared"
        );
        calls
    }

    /// The single call recorded for `cmd`.
    ///
    /// Panics on zero or on several, with the whole log in the message: a test
    /// that means to check one command's arguments should fail loudly if the
    /// call site fired twice or not at all.
    pub(crate) fn only_call_of(&self, cmd: &str) -> Call {
        let calls = self.settle();
        let mut matching: Vec<Call> = calls.into_iter().filter(|c| c.cmd == cmd).collect();
        assert_eq!(
            matching.len(),
            1,
            "expected exactly one `{cmd}` call, got {} (all calls: {:?})",
            matching.len(),
            self.expected
        );
        matching.pop().expect("checked len == 1")
    }
}

thread_local! {
    /// Guards against a re-entrant [`Test::settle`], which would poll the same
    /// boxed future twice.
    static DRAIN_DEPTH: RefCell<u32> = const { RefCell::new(0) };
}

/// Turn cap for [`Test::settle`]'s drain loop. See its comment.
const MAX_DRAIN_TURNS: u32 = 64;

fn route(cmd: &str, outcome: Outcome) {
    ROUTES.with(|r| {
        let mut r = r.borrow_mut();
        // Replace an existing route for the same command rather than appending,
        // so a test can re-route a command it fires twice with different
        // outcomes.
        if let Some(slot) = r.iter_mut().find(|(name, _)| name == cmd) {
            slot.1 = outcome;
        } else {
            r.push((cmd.to_string(), outcome));
        }
    });
}

// ---------------------------------------------------------------------------
// The stubs `api.rs` re-exports under `cfg(test)`
// ---------------------------------------------------------------------------

/// Stub for `wasm_bindgen_futures::spawn_local`: queue the future for
/// [`Test::settle`] to drive. See the module doc for why this is not the real
/// executor.
pub(crate) fn spawn<F: Future<Output = ()> + 'static>(future: F) {
    QUEUE.with(|q| q.borrow_mut().push(Box::pin(future)));
}

/// Stub for `api.rs`'s `tauri_invoke`.
///
/// Returns a future that is ready on its first poll rather than a real
/// `Promise`: the IPC layer answers on a later turn, but this one has the
/// answer already, and being immediately ready is what lets [`Test::settle`]
/// finish a call chain in one turn with no event loop behind it. `invoke`'s
/// `.await` is identical either way — see [`Immediate`].
pub(crate) fn tauri_invoke(cmd: &str, args: JsValue) -> Immediate<Result<JsValue, JsValue>> {
    let recorded = Call {
        cmd: cmd.to_string(),
        args: serde_wasm_bindgen::from_value::<Value>(args).ok(),
    };
    CALLS.with(|c| c.borrow_mut().push(recorded));

    let outcome = ROUTES.with(|r| {
        r.borrow()
            .iter()
            .rev()
            .find(|(name, _)| name == cmd)
            .map(|(_, o)| o.clone())
    });
    Immediate(Some(match outcome {
        Some(Outcome::Err(e)) => Err(e),
        Some(Outcome::Ok(v)) => Ok(to_js(&v)),
        None => Ok(JsValue::NULL),
    }))
}

/// A future that is already finished.
///
/// Used instead of a hand-rolled `async` block so the value is produced — and
/// the call recorded — when `tauri_invoke` is *called*, not when the returned
/// future is first polled. That ordering is what makes an un-drained queue
/// visible: a test that forgets to `settle` sees an empty log rather than a
/// recorded call it never awaited.
pub(crate) struct Immediate<T>(pub(crate) Option<T>);

// Unconditionally `Unpin`, with no bound on `T`: nothing here is ever pinned
// for a reason. `pin_mut!` may put this future in a `Pin` on the stack, and
// there is no self-reference to break — moving it is always sound.
impl<T> Unpin for Immediate<T> {}

impl<T> std::future::Future for Immediate<T> {
    type Output = T;

    fn poll(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<T> {
        match self.get_mut().0.take() {
            Some(v) => Poll::Ready(v),
            // Polled twice, which means the drain loop re-queued a finished
            // future. Panicking beats handing back a stale value.
            None => panic!("an Immediate future was polled after it completed"),
        }
    }
}

thread_local! {
    /// The event listeners registered since the test binary started, each with a
    /// Rust-side copy of the callback `api::listen_event` was given.
    ///
    /// The copy is necessary rather than convenient: a `Closure` is a handle to
    /// a JS function and **cannot be invoked from Rust**, and in
    /// `wasm-bindgen` 0.2.128 it is not `Clone` either, so a recorded
    /// `&Closure` could never be delivered to.
    ///
    /// Not cleared per test: each entry belongs to a component instance, and a
    /// component from an earlier test has been unmounted, so its callback either
    /// no-ops or targets a dropped root. [`emit_for_test`] reports how many
    /// handlers it reached, so a test that needs exactly one can require that.
    static LISTENERS: RefCell<Vec<(String, Callback<JsValue>)>> = const { RefCell::new(Vec::new()) };
}

/// Stub for `api::listen_event`, replacing the real subscription under
/// `cfg(test)`.
///
/// It records the callback and hands `api::deliver` back unchanged, so the
/// envelope unwrapping and the deserialization of the payload are the real ones
/// — the stub replaces the transport, not the logic above it.
pub(crate) fn listen_event<T>(event: &'static str, cb: Callback<T>)
where
    T: serde::de::DeserializeOwned + 'static,
{
    // The handler is owned by an `Rc` so the callback `deliver` receives can
    // still be `'static`: a plain `RefCell` borrow would not outlive the
    // enclosing closure, and `bridge`'s real subscription requires `'static`.
    let handler = std::rc::Rc::new(RefCell::new(cb));
    LISTENERS.with(|l| {
        l.borrow_mut().push((
            event.to_string(),
            Callback::from(move |raw: JsValue| {
                let handler = handler.clone();
                crate::api::deliver::<T, _>(raw, move |value| handler.borrow().emit(value));
            }),
        ))
    });
}

/// Deliver `payload` to every listener registered for `event`, as the backend's
/// `emit` would, and return how many were reached.
///
/// The payload is wrapped in an object with a `payload` key because that is the
/// envelope `api::deliver` unwraps, and delivering through that function — rather
/// than calling a component's callback directly — is what makes this a test of
/// the real subscription path rather than of a copy of it.
pub(crate) fn emit_for_test<T: Serialize>(event: &str, payload: &T) -> usize {
    let raw = to_js(&serde_json::to_value(payload).expect("test payload serializes"));
    let envelope = js_sys::Object::new();
    js_sys::Reflect::set(&envelope, &JsValue::from_str("payload"), &raw)
        .expect("setting `payload` on a fresh object cannot fail");
    let envelope = JsValue::from(envelope);

    let handlers: Vec<Callback<JsValue>> = LISTENERS.with(|l| {
        l.borrow()
            .iter()
            .filter(|(name, _)| name == event)
            .map(|(_, cb)| cb.clone())
            .collect()
    });
    let delivered = handlers.len();
    for cb in handlers {
        cb.emit(envelope.clone());
    }
    delivered
}

fn to_js(v: &Value) -> JsValue {
    // `json_compatible()` for the same reason `api::invoke` uses it: the
    // default serializer emits `Map`s, which are not what crosses IPC.
    v.serialize(&serde_wasm_bindgen::Serializer::json_compatible())
        .unwrap_or(JsValue::UNDEFINED)
}

/// A waker that does nothing. Nothing in this harness needs to be woken: a
/// future that returns `Pending` is re-polled on the next turn of
/// [`Test::settle`]'s drain loop, and the stubbed transport never actually
/// waits.
pub(crate) fn noop_waker() -> std::task::Waker {
    use std::task::{RawWaker, RawWakerVTable, Waker};

    fn clone(_: *const ()) -> RawWaker {
        RawWaker::new(std::ptr::null(), &VTABLE)
    }
    fn noop(_: *const ()) {}
    static VTABLE: RawWakerVTable = RawWakerVTable::new(clone, noop, noop, noop);
    // SAFETY: the vtable's functions ignore the (null) data pointer entirely.
    unsafe { Waker::from_raw(RawWaker::new(std::ptr::null(), &VTABLE)) }
}

// ---------------------------------------------------------------------------
// Shape assertions
// ---------------------------------------------------------------------------

/// Argument keys of `call`, sorted — what the backend's `rename_all =
/// "snake_case"` parameter list has to match.
pub(crate) fn keys(call: &Call) -> Vec<String> {
    let Some(Value::Object(map)) = &call.args else {
        panic!(
            "{}: arguments were not a JSON object: {:?}",
            call.cmd, call.args
        );
    };
    let mut keys: Vec<String> = map.keys().cloned().collect();
    keys.sort();
    keys
}

/// JSON type name of `call`'s argument `key`, so a shape test fails with
/// "expected number, got string" instead of dumping a value.
pub(crate) fn type_of(call: &Call, key: &str) -> &'static str {
    let Some(Value::Object(map)) = &call.args else {
        panic!("{}: arguments were not a JSON object", call.cmd);
    };
    let Some(v) = map.get(key) else {
        panic!("{}: no argument `{key}`; has {:?}", call.cmd, keys(call));
    };
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// The common "this command, these keys" assertion, so every shape test reports
/// the same way.
pub(crate) fn assert_shape(call: &Call, cmd: &str, expected: &[&str]) {
    assert_eq!(call.cmd, cmd, "wrong command");
    let expected: Vec<String> = expected.iter().map(|s| (*s).to_string()).collect();
    assert_eq!(keys(call), expected, "{cmd} argument keys");
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

use rift_types::{Playlist, Track};

/// A track with every field populated, so a shape test is testing the shape
/// rather than an absent field.
pub(crate) fn track(id: &str) -> Track {
    Track {
        id: id.to_string(),
        title: format!("Title {id}"),
        artist: "Artist".to_string(),
        album: Some("Album".to_string()),
        duration: Some(210),
        cover: "https://example.invalid/cover.jpg".to_string(),
        artists: vec![],
        album_id: Some("album-1".to_string()),
    }
}

/// A playlist of `n` tracks, ids prefixed with the playlist's own id so two
/// playlists never accidentally share a track.
pub(crate) fn playlist(id: &str, n: usize) -> Playlist {
    Playlist {
        id: id.to_string(),
        name: format!("Playlist {id}"),
        tracks: (0..n).map(|i| track(&format!("{id}{i}"))).collect(),
    }
}

/// The ids of `tracks`, for order assertions.
pub(crate) fn ids(tracks: &[Track]) -> Vec<String> {
    tracks.iter().map(|t| t.id.clone()).collect()
}

use std::task::{Context, Poll};
