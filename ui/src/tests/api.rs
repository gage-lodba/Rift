//! The IPC boundary (`api.rs`).
//!
//! What these cover that `src-tauri/tests/ipc_contract.rs` cannot: that test
//! reads both sides as *text* and compares command names and argument key
//! names. It cannot see a value's type, and it cannot see a call site it fails
//! to parse. These run the real call path and assert on the JS object the IPC
//! layer actually receives.
//!
//! The defect that motivates the harness — `play_track` sending a `"radio"` key
//! the backend had stopped accepting — is caught by
//! [`play_track_sends_only_the_track_argument`].

use std::cell::RefCell;
use std::rc::Rc;

use serde_json::json;
use wasm_bindgen_test::wasm_bindgen_test;
use yew::Callback;

use crate::api;
use crate::tests::harness::{assert_shape, keys, track, type_of};

/// The regression guard for the defect this harness exists for.
///
/// `play_track`'s `radio` parameter was removed from the backend as dead code
/// (L4) while this call site still sent `"radio": false`. Both sides compiled;
/// the command failed at runtime for every single-track play. The argument set
/// is asserted exactly, so re-adding a key the backend does not accept fails
/// here even though the call site still compiles.
///
/// Deliberately drives `app::fire_play_track` — the function the row and Home
/// handlers call — rather than restating the `json!` literal. Restating it
/// would make this test pass while the real call site was broken; that is the
/// exact way the original defect survived its own review.
#[wasm_bindgen_test]
fn play_track_sends_only_the_track_argument() {
    let t = crate::tests::harness::test(&["play_track"]);

    crate::app::fire_play_track(&track("abc"));

    let call = t.only_call_of("play_track");
    assert_shape(&call, "play_track", &["track"]);
    assert_eq!(type_of(&call, "track"), "object");
    let sent = &call.args.as_ref().expect("args")["track"];
    assert_eq!(
        sent.get("id").and_then(|v| v.as_str()),
        Some("abc"),
        "the track has to cross IPC intact: {sent}"
    );
}

/// `play_tracks` is built in a shared helper (`app::fire_play_tracks`) rather
/// than inline at each call site — exactly the shape the backend's text-level
/// parser cannot see through (it looks for `json!` on the same statement as the
/// command literal). Here it is the real call path.
#[wasm_bindgen_test]
fn play_tracks_sends_tracks_start_and_source() {
    let t = crate::tests::harness::test(&["play_tracks"]);

    crate::app::fire_play_tracks(
        &[track("a"), track("b")],
        &Some("playlist:p1".to_string()),
        1,
    );

    let call = t.only_call_of("play_tracks");
    assert_shape(&call, "play_tracks", &["source", "start", "tracks"]);
    assert_eq!(type_of(&call, "tracks"), "array");
    assert_eq!(type_of(&call, "start"), "number");
    assert_eq!(type_of(&call, "source"), "string");
}

/// `source` is `Option<String>` on both sides. `None` has to reach the backend
/// as an explicit `null`: a *missing* key is a different thing to Tauri's
/// argument deserializer, which requires every declared parameter by name.
#[wasm_bindgen_test]
fn play_tracks_sends_an_explicit_null_source() {
    let t = crate::tests::harness::test(&["play_tracks"]);

    crate::app::fire_play_tracks(&[track("a")], &None, 0);

    let call = t.only_call_of("play_tracks");
    assert!(
        call.args
            .as_ref()
            .expect("args")
            .get("source")
            .is_some_and(serde_json::Value::is_null),
        "a `None` source must cross as an explicit null, not an absent key: {:?}",
        call.args
    );
}

/// Numbers must stay numbers. A `usize` that crossed as a string would be
/// rejected by the backend's deserializer at runtime, and nowhere earlier.
#[wasm_bindgen_test]
fn numeric_arguments_cross_as_numbers() {
    let t =
        crate::tests::harness::test(&["queue_jump", "set_volume", "set_crossfade", "queue_move"]);

    api::fire("queue_jump", json!({ "index": 3usize }));
    api::fire("set_volume", json!({ "volume": 0.5f32 }));
    api::fire("set_crossfade", json!({ "secs": 6.0f32 }));
    api::fire("queue_move", json!({ "from": 1usize, "to": 4usize }));

    let calls = t.settle();
    // The whole observed sequence as one value, so the failure message carries
    // all of it — two separate assertions would hide which call was wrong.
    let seen: Vec<(String, Vec<&'static str>)> = calls
        .iter()
        .map(|c| {
            let ks = keys(c);
            let types = ks.iter().map(|k| type_of(c, k)).collect();
            (c.cmd.clone(), types)
        })
        .collect();
    assert_eq!(
        seen,
        vec![
            ("queue_jump".to_string(), vec!["number"]),
            ("set_volume".to_string(), vec!["number"]),
            ("set_crossfade".to_string(), vec!["number"]),
            ("queue_move".to_string(), vec!["number", "number"]),
        ]
    );
    assert_eq!(keys(&calls[3]), vec!["from".to_string(), "to".to_string()]);
}

/// `queue_move`'s two indices are what the reorder preview promised the user.
/// They must arrive unswapped — the frontend half of the drag-and-drop bug
/// class, whose backend half is covered by the queue tests in `src-tauri`.
#[wasm_bindgen_test]
fn queue_move_preserves_index_order() {
    let t = crate::tests::harness::test(&["queue_move"]);

    api::fire("queue_move", json!({ "from": 1usize, "to": 3usize }));

    let args = t.only_call_of("queue_move").args.expect("args");
    assert_eq!(args["from"], json!(1));
    assert_eq!(args["to"], json!(3));
}

/// A command with no parameters must still send an empty argument object:
/// Tauri's `invoke` distinguishes a missing payload from `{}`.
#[wasm_bindgen_test]
fn no_argument_commands_send_an_empty_object() {
    let t = crate::tests::harness::test(&["toggle_play"]);

    api::fire("toggle_play", json!({}));

    assert_shape(&t.only_call_of("toggle_play"), "toggle_play", &[]);
}

/// A rejected command is reported through the error sink with the command name
/// attached.
///
/// The sink is what 41 of this app's 55 IPC calls route through; before it
/// existed a rejected command reached nobody (clicking play on an empty
/// collection did nothing, and `create_playlist` with a blank name silently
/// closed the dialog). This is the only place that wiring is exercised without
/// a running app.
#[wasm_bindgen_test]
fn fire_routes_a_rejected_command_to_the_error_sink() {
    let t = crate::tests::harness::test(&["create_playlist"]);
    t.fail("create_playlist", "name must not be blank");
    let seen = Rc::new(RefCell::new(Vec::<String>::new()));
    t.on_error({
        let seen = seen.clone();
        Callback::from(move |msg: String| seen.borrow_mut().push(msg))
    });

    api::fire("create_playlist", json!({ "name": "  " }));
    t.settle();

    let seen = seen.borrow();
    assert_eq!(seen.len(), 1, "expected one toast, got {seen:?}");
    assert!(
        seen[0].contains("create_playlist") && seen[0].contains("name must not be blank"),
        "the toast must name the command and the reason: {:?}",
        seen[0]
    );
}

/// The other half: a command that succeeds must not reach the sink, or every
/// play would raise a phantom toast.
#[wasm_bindgen_test]
fn fire_is_silent_when_the_command_succeeds() {
    let t = crate::tests::harness::test(&["queue_add"]);
    t.respond("queue_add", json!(null));
    let seen = Rc::new(RefCell::new(0u32));
    t.on_error({
        let seen = seen.clone();
        Callback::from(move |_: String| *seen.borrow_mut() += 1)
    });

    api::fire("queue_add", json!({ "track": track("a") }));
    let calls = t.settle();

    assert_eq!(calls.len(), 1, "the command should have run");
    assert_eq!(*seen.borrow(), 0, "a successful command must stay quiet");
}

/// The failure has to survive the whole path: transport rejects, `invoke` maps
/// it to a `String`, `fire` prefixes the command, the sink shows it. Every
/// other test here checks one link; this one checks the chain, because a break
/// anywhere in it is a rejected command the user never hears about.
#[wasm_bindgen_test]
fn a_rejected_command_reaches_the_user_with_its_reason() {
    let t = crate::tests::harness::test(&["download_tracks"]);
    t.fail("download_tracks", "yt-dlp is not installed");
    let seen = Rc::new(RefCell::new(Vec::<String>::new()));
    t.on_error({
        let seen = seen.clone();
        Callback::from(move |msg: String| seen.borrow_mut().push(msg))
    });

    api::fire("download_tracks", json!({ "tracks": [track("a")] }));
    t.settle();

    assert_eq!(
        seen.borrow().as_slice(),
        ["download_tracks: yt-dlp is not installed"],
        "the toast text is what the user reads"
    );
}

/// `invoke` returns the backend's value, deserialized into the type the caller
/// asked for — not a raw `JsValue`, and not a `null` stand-in.
///
/// The command is a real one on purpose: `src-tauri/tests/ipc_contract.rs` reads
/// every `api::fire`/`invoke` command literal out of this directory as text and
/// fails if it is not registered, so a made-up name here would break the backend
/// suite. A synthetic name would also be weaker evidence — `bootstrap` is the
/// first call the app makes.
#[wasm_bindgen_test]
fn invoke_returns_the_deserialized_response() {
    let t = crate::tests::harness::test(&["bootstrap"]);
    t.respond(
        "bootstrap",
        json!({
            "library": { "liked": [], "playlists": [], "recently_played": [] },
            "queue": { "tracks": [], "current": null, "shuffle": false, "repeat": "off" },
            // `state` is an internally tagged enum, so it crosses as
            // `{"kind": "..."}` rather than a bare string.
            "state": { "kind": "stopped" },
            "volume": 0.75,
            "track": null,
            "progress": { "position": 0.0, "duration": 0.0 },
            "downloads": { "downloaded": [], "downloading": [], "failed": [] },
            "crossfade": 3.0,
        }),
    );

    let b = block_on(api::invoke::<rift_types::Bootstrap>(
        "bootstrap",
        &json!({}),
    ))
    .expect("resolves");

    assert_eq!(b.volume, 0.75);
    assert_eq!(b.crossfade, 3.0);
    assert_eq!(t.settle()[0].cmd, "bootstrap");
}

/// A failed command surfaces as `Err` carrying the backend's message, which the
/// callers that show a toast print verbatim.
#[wasm_bindgen_test]
fn invoke_surfaces_the_backend_error_message() {
    let t = crate::tests::harness::test(&["bootstrap"]);
    t.fail("bootstrap", "state is not ready");

    let err = block_on(api::invoke::<rift_types::Bootstrap>(
        "bootstrap",
        &json!({}),
    ))
    .expect_err("rejects");

    assert_eq!(err, "state is not ready");
    t.settle();
}

/// A rejection that is not a string still has to become a usable message.
/// `js_err`'s `format!("{e:?}")` fallback is otherwise dead in tests, and a
/// webview is not obliged to reject with a string.
#[wasm_bindgen_test]
fn invoke_stringifies_a_non_string_js_error() {
    let t = crate::tests::harness::test(&["bootstrap"]);
    t.fail_with_js_error("bootstrap", "boom");

    let err = block_on(api::invoke::<rift_types::Bootstrap>(
        "bootstrap",
        &json!({}),
    ))
    .expect_err("rejects");

    assert!(
        err.contains("boom"),
        "the fallback formatting should still name the failure: {err:?}"
    );
    t.settle();
}

/// A response the caller cannot deserialize is an `Err`, not a panic: a backend
/// shape change should surface as a toast, not take the app down.
#[wasm_bindgen_test]
fn invoke_reports_a_response_that_does_not_match_its_type() {
    let t = crate::tests::harness::test(&["bootstrap"]);
    t.respond("bootstrap", "not a bootstrap payload");

    let err = block_on(api::invoke::<rift_types::Bootstrap>(
        "bootstrap",
        &json!({}),
    ))
    .expect_err("rejects");

    assert!(
        !err.is_empty(),
        "a deserialization failure must carry a message to show the user"
    );
    t.settle();
}

// ---------------------------------------------------------------------------
// Driving a single future to completion
// ---------------------------------------------------------------------------

/// Poll `future` to completion on this stack.
///
/// For the tests that call `api::invoke` directly rather than through
/// `api::fire` — there is no queue to drain, only one future, and the stubbed
/// transport is ready on its first poll so this cannot spin. A `Pending` result
/// here is a harness bug (the stub started waiting for something), so it panics
/// rather than looping.
fn block_on<F: std::future::Future>(future: F) -> F::Output {
    use std::task::{Context, Poll};

    let mut future = Box::pin(future);
    let waker = crate::tests::harness::noop_waker();
    let mut cx = Context::from_waker(&waker);
    match future.as_mut().poll(&mut cx) {
        Poll::Ready(out) => out,
        Poll::Pending => panic!(
            "the stubbed transport returned Pending; `invoke` is waiting on \
             something this harness cannot drive"
        ),
    }
}
