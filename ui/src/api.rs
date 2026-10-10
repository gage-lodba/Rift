//! Bindings to the Tauri IPC bridge (`window.__TAURI__`, enabled by
//! `withGlobalTauri`). Commands are invoked with snake_case argument names,
//! matching `#[tauri::command(rename_all = "snake_case")]` on the backend.

use std::cell::RefCell;

use serde::de::DeserializeOwned;
use serde::Serialize;
use wasm_bindgen::prelude::*;
use yew::Callback;

// The bridge itself is the one part of this module the test harness does not
// run: it needs a live `window.__TAURI__`, and no test may depend on a running
// app. Everything above it — the JSON-compatible serialization, the error
// stringification, `fire`'s error routing — is exercised for real; see
// `crate::tests::harness`, which replaces these two imports with a recording
// stub under `cfg(test)`.
//
// What that costs, stated rather than glossed: under `cfg(test)` these two
// declarations are not compiled, so nothing checks the stub against them. The
// stub must nonetheless be callable from `invoke` and `listen_event` below —
// a change to *those* call sites breaks the build — but a change to these two
// signatures alone would not. If you edit either signature, edit
// `tests/harness.rs` in the same commit.
//
// The stub is hand-written rather than a mock crate's, mirroring how the
// backend's client tests use a loopback `TcpListener` instead of a mocking
// library: the seam is at the transport, and there is no third party to agree
// with.
#[cfg(not(test))]
mod bridge {
    use wasm_bindgen::prelude::*;

    #[wasm_bindgen]
    extern "C" {
        #[wasm_bindgen(js_namespace = ["window", "__TAURI__", "core"], js_name = invoke, catch)]
        pub async fn tauri_invoke(cmd: &str, args: JsValue) -> Result<JsValue, JsValue>;

        #[wasm_bindgen(js_namespace = ["window", "__TAURI__", "event"], js_name = listen)]
        fn listen(event: &str, handler: &Closure<dyn FnMut(JsValue)>);
    }

    /// Subscribe `handler` to `event` for the lifetime of the page.
    ///
    /// The `Closure` is built here and leaked, which is what the real binding
    /// needs: a `Closure` is a handle to a JS function, so the callback has to
    /// stay alive as long as the page does.
    pub(super) fn tauri_listen(event: &'static str, handler: impl FnMut(JsValue) + 'static) {
        let closure = Closure::<dyn FnMut(JsValue)>::new(handler);
        listen(event, &closure);
        // The subscription lives as long as the page; intentionally leaked.
        closure.forget();
    }
}

#[cfg(test)]
pub(crate) use crate::tests::harness::tauri_invoke;

#[cfg(not(test))]
pub use bridge::tauri_invoke;

// The executor seam, alongside the bridge above: production runs the future on
// the page's executor, the test build queues it for `Test::settle` to drive.
//
// Command call sites use `api::spawn` rather than
// `wasm_bindgen_futures::spawn_local` for that reason. A future spawned by hand
// never runs under the harness, so everything it awaited was unreachable by any
// test — the Settings yt-dlp probes, the artist/album fetches, the search tabs,
// the launch-time update check. Pure UI timers (`show_toast`'s dismiss delay)
// deliberately stay on `spawn_local`: there is nothing to assert about them, and
// leaving them undriven is what keeps a toast on screen for a test to read.
#[cfg(test)]
pub(crate) use crate::tests::harness::spawn;
#[cfg(not(test))]
pub use wasm_bindgen_futures::spawn_local as spawn;

/// Subscribe to a backend event for the lifetime of the page.
///
/// A thin wrapper over [`deliver`], which is where the payload handling lives and
/// which is compiled in *both* configurations — so the shape this depends on is
/// the shape the tests exercise. Only the transport differs between a real build
/// and a test build.
#[cfg(not(test))]
pub fn listen_event<T: DeserializeOwned + 'static>(event: &'static str, cb: Callback<T>) {
    // `Callback` is not itself `Fn`, so the `deliver` call goes through a
    // closure that emits — the same shape the test build uses.
    bridge::tauri_listen(event, move |raw: JsValue| {
        deliver(raw, |value: T| cb.emit(value));
    });
}

/// The test build's subscription: the same function the app calls, with the
/// transport replaced by a recorder that a test can deliver events through. See
/// `tests/harness.rs`'s `listen_event`.
#[cfg(test)]
pub(crate) use crate::tests::harness::listen_event;

fn js_err(e: JsValue) -> String {
    e.as_string().unwrap_or_else(|| format!("{e:?}"))
}

pub async fn invoke<R: DeserializeOwned>(cmd: &str, args: &impl Serialize) -> Result<R, String> {
    // json_compatible() serializes maps as plain JS objects (the default
    // serializer produces `Map`s, which the IPC layer rejects).
    let ser = serde_wasm_bindgen::Serializer::json_compatible();
    let js_args = args.serialize(&ser).map_err(|e| e.to_string())?;
    let out = tauri_invoke(cmd, js_args).await.map_err(js_err)?;
    serde_wasm_bindgen::from_value(out).map_err(|e| e.to_string())
}

/// Fire-and-forget command invocation.
///
/// "Fire-and-forget" is about the return value, not about the failure: 42 of
/// this app's 55 IPC calls go through here, and a bare `console.error` meant
/// every `Err` not mirrored by a backend `rift://error` event reached nobody.
/// Concretely, clicking play on an empty collection did nothing at all, and
/// `create_playlist` with a blank name silently closed the dialog. A rejected
/// command now surfaces through the same toast the backend's own errors use.
pub fn fire(cmd: &'static str, args: impl Serialize + 'static) {
    spawn(async move {
        if let Err(e) = invoke::<serde_json::Value>(cmd, &args).await {
            let msg = format!("{cmd}: {e}");
            web_sys::console::error_1(&msg.clone().into());
            report_error(msg);
        }
    });
}

/// Await a command, reporting a rejection to the installed sink and falling
/// back to `T::default()`.
///
/// For the call sites that need the command's *result*, so [`fire`] is not an
/// option, and that have a placeholder status to fall back on. Swallowing the
/// failure there is not the same as ignoring it: a rejected `set_yt_dlp_path`
/// fell back to `YtDlpStatus::default()`, which renders as "✗ Not found — Not
/// found on your PATH or in common install locations" — telling the user their
/// path does not exist when the backend had in fact rejected it, with the
/// reason ("not a file", "not absolute") discarded.
pub async fn invoke_or_default<T>(cmd: &'static str, args: &impl Serialize) -> T
where
    T: DeserializeOwned + Default,
{
    match invoke::<T>(cmd, args).await {
        Ok(v) => v,
        Err(e) => {
            report_error(format!("{cmd}: {e}"));
            T::default()
        }
    }
}

thread_local! {
    /// Where a failed command sends its message: `fire`, `invoke_or_default`,
    /// and `App`'s bootstrap, which reports by hand. `App` installs its toast
    /// here on mount; until then failures only reach the console.
    static ERROR_SINK: RefCell<Option<Callback<String>>> = const { RefCell::new(None) };
}

/// Install the sink that frontend-side command failures report to.
pub fn set_error_sink(sink: Callback<String>) {
    ERROR_SINK.with(|s| *s.borrow_mut() = Some(sink));
}

/// Remove the installed sink.
///
/// Production installs one for the life of the page and never removes it. The
/// test harness clears it around each test, because the thread-local outlives
/// any single test (wasm is single-threaded, so "thread-local" is process-wide
/// here) and a sink left installed would let one test's toast assertion see
/// another test's message.
#[cfg(test)]
pub(crate) fn unset_error_sink() {
    ERROR_SINK.with(|s| *s.borrow_mut() = None);
}

/// Report a frontend-side IPC failure to the installed sink, if any.
///
/// Public (crate-internal in practice — `api` is a private module) because
/// [`fire`] and [`invoke_or_default`] are not the only way a command can fail:
/// `App`'s bootstrap and "Check now" await their result themselves and used to
/// leave a rejection in the console, where a user whose snapshot never loaded
/// could not see it.
pub fn report_error(msg: String) {
    ERROR_SINK.with(|s| {
        if let Some(cb) = s.borrow().as_ref() {
            cb.emit(msg);
        }
    });
}

/// Unwrap one event payload and hand it to `cb`, logging a payload the event's
/// type cannot read.
///
/// Split out of [`listen_event`] so it is not behind `cfg(test)`: it is real
/// behaviour — the `payload` key is the bridge's envelope, not the event's own
/// data — and the test build has to run *this* code rather than a copy of it for
/// `emit_for_test` to be worth anything.
///
/// The callback borrows instead of owning (`Fn(T) + '_` rather than
/// `+ 'static`) so both callers can pass one: the real path passes an owned
/// value, the test path passes a borrow of the recorded handler, and neither
/// should have to change shape to satisfy the other.
pub fn deliver<T, C>(raw: JsValue, cb: C)
where
    T: DeserializeOwned,
    C: Fn(T),
{
    let Ok(payload) = js_sys::Reflect::get(&raw, &JsValue::from_str("payload")) else {
        return;
    };
    match serde_wasm_bindgen::from_value::<T>(payload) {
        Ok(v) => cb(v),
        Err(e) => web_sys::console::error_1(&format!("event payload: {e}").into()),
    }
}
