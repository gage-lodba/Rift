//! Bindings to the Tauri IPC bridge (`window.__TAURI__`, enabled by
//! `withGlobalTauri`). Commands are invoked with snake_case argument names,
//! matching `#[tauri::command(rename_all = "snake_case")]` on the backend.

use std::cell::RefCell;

use serde::de::DeserializeOwned;
use serde::Serialize;
use wasm_bindgen::prelude::*;
use yew::Callback;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = ["window", "__TAURI__", "core"], js_name = invoke, catch)]
    async fn tauri_invoke(cmd: &str, args: JsValue) -> Result<JsValue, JsValue>;

    #[wasm_bindgen(js_namespace = ["window", "__TAURI__", "event"], js_name = listen)]
    fn tauri_listen(event: &str, handler: &Closure<dyn FnMut(JsValue)>) -> js_sys::Promise;
}

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
/// "Fire-and-forget" is about the return value, not about the failure: 41 of
/// this app's 55 IPC calls go through here, and a bare `console.error` meant
/// every `Err` not mirrored by a backend `rift://error` event reached nobody.
/// Concretely, clicking play on an empty collection did nothing at all, and
/// `create_playlist` with a blank name silently closed the dialog. A rejected
/// command now surfaces through the same toast the backend's own errors use.
pub fn fire(cmd: &'static str, args: impl Serialize + 'static) {
    wasm_bindgen_futures::spawn_local(async move {
        if let Err(e) = invoke::<serde_json::Value>(cmd, &args).await {
            let msg = format!("{cmd}: {e}");
            web_sys::console::error_1(&msg.clone().into());
            report_error(msg);
        }
    });
}

thread_local! {
    /// Where a failed `fire` sends its message. `App` installs its toast here on
    /// mount; until then failures only reach the console.
    static ERROR_SINK: RefCell<Option<Callback<String>>> = const { RefCell::new(None) };
}

/// Install the sink that [`fire`] reports failures to.
pub fn set_error_sink(sink: Callback<String>) {
    ERROR_SINK.with(|s| *s.borrow_mut() = Some(sink));
}

/// Report a frontend-side failure to the installed sink, if any.
fn report_error(msg: String) {
    ERROR_SINK.with(|s| {
        if let Some(cb) = s.borrow().as_ref() {
            cb.emit(msg);
        }
    });
}

/// Subscribe to a backend event for the lifetime of the page.
pub fn listen_event<T: DeserializeOwned + 'static>(event: &'static str, cb: Callback<T>) {
    let closure = Closure::<dyn FnMut(JsValue)>::new(move |raw: JsValue| {
        let Ok(payload) = js_sys::Reflect::get(&raw, &JsValue::from_str("payload")) else {
            return;
        };
        match serde_wasm_bindgen::from_value::<T>(payload) {
            Ok(v) => cb.emit(v),
            Err(e) => web_sys::console::error_1(&format!("event {event}: {e}").into()),
        }
    });
    let _ = tauri_listen(event, &closure);
    // The subscription lives as long as the app; intentionally leaked.
    closure.forget();
}
