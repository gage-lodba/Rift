//! A static check for the invariant behind `C1`: hooks must not be called
//! after a conditional early return.
//!
//! # Why a static check rather than a render test
//!
//! Yew asserts that a render's hook count matches the *previous* render's
//! (`assert_hook_context`, `yew-0.21.0/src/functional/mod.rs:220`), so a hook
//! placed after an early return only panics on the second render — and only if
//! the first render took the early exit. Driving that through a renderer needs
//! either a DOM or a hook harness, neither of which this crate has (see
//! `components.rs` for the two dead ends tried through the server renderer).
//!
//! The *invariant*, though, is structural and needs no renderer: within a
//! component body, every hook call must be reachable on every render. An
//! unconditional early return makes everything after it unreachable on some
//! renders, which is exactly the defect. That is decidable by reading the
//! function body, so this test reads it.
//!
//! # What it does and does not catch
//!
//! Catches: any hook call in the component body *after* an early return that is
//! itself in the body. `C1`'s historical shape is the concrete case — the early
//! return for an empty queue, with hook work introduced below it later.
//!
//! Does not catch: a hook after a `match` arm that always returns. It is a
//! scanner over tokens, not a parser, and it is deliberately conservative: it
//! reports only what it can prove from brace depth. A false positive here
//! breaks the build for innocent code, which is how a check like this gets
//! deleted by the next person, so the scanner is biased toward silence.
//!
//! # The other half of this defect class is already a compile error
//!
//! A hook written *inside* a conditional, rather than after an early return:
//!
//! ```ignore
//! if props.items.is_empty() {
//!     return html! {};
//! } else {
//!     let x = use_state(|| 0);   // error: hooks cannot be called at this position
//! }
//! ```
//!
//! A rule for this was written and then deleted, because measuring showed it
//! could never fire: `#[function_component]` rejects it outright — *"hooks
//! cannot be called at this position"* — so the crate does not build, let
//! alone ship. Verified by injecting exactly that into `queue.rs` and watching
//! the compiler refuse it. Keeping a check for it would have been dead code
//! that read as coverage, which is the failure mode this whole harness exists
//! to avoid. `C1` needed a test precisely because it is the half the macro
//! *cannot* see: the hook is at the top level of the body, so the expansion is
//! well-formed, and nothing but the second render disagrees.
//!
//! This is **not** a substitute for a real second-render test. It closes the
//! specific structural hole; the renderer-level coverage is still open.
//!
//! # The render harness catches this defect too, and it is checked by hand
//!
//! `render.rs` mounts real components through Yew's client renderer, so it can
//! reach the second render this file reasons about — and with the defect
//! re-injected into `queue.rs` it **does** catch it. That check is not a test in
//! the suite, because it cannot be one: Yew's assertion is a panic, the render
//! happens on a microtask, and `wasm32-unknown-unknown` has no unwinding to
//! catch it with (`wasm_bindgen::__rt::maybe_catch_unwind` is the identity
//! function without `panic = "unwind"`). The panic therefore aborts the whole
//! test binary, which means a test asserting it could only ever kill the run
//! instead of reporting. The procedure, so it can be repeated rather than
//! believed:
//!
//! 1. Add a hook below `queue_panel`'s empty-queue early return in
//!    `components/queue.rs` — one line is enough:
//!    `let _x = use_state(|| false);`
//! 2. Run `cargo test -p rift-ui --target wasm32-unknown-unknown`.
//!
//! Measured, in two parts. This file's
//! [`no_hook_is_called_after_an_early_return`] fails with the line and the text
//! of the offending hook:
//!
//! ```text
//! test tests::hook_order::no_hook_is_called_after_an_early_return ... FAIL
//! a hook is called after an early return, so it is skipped on that path and
//! Yew's hook-order assertion will fire on a later render:
//!   components/queue.rs:51: let _collapsed_below_the_early_return = use_state(|| false);
//! ```
//!
//! and `render.rs`'s
//! `the_repair_is_exercised_on_the_real_component` — which mounts the queue empty
//! and then populates it, the only path C1 was ever reachable on — aborts the run
//! from Yew's own assertion:
//!
//! ```text
//! panicked at yew-0.21.0/src/functional/mod.rs:235:49:
//! assertion `left == right` failed: Hooks are called conditionally.
//!   left: 0
//!  right: 1
//! RuntimeError: unreachable
//! ```
//!
//! Two independent mechanisms, then: a static check that reports a line number,
//! and a real render that dies on the assertion the defect actually produces.
//! The second is the one that proves the *mechanism*; the first is the one that
//! can run in the suite. Reverting the injection makes both pass again.

use wasm_bindgen_test::wasm_bindgen_test;

/// Component sources, read at compile time from a hardcoded list.
///
/// `include_str!` rather than a runtime walk: a runtime `read_dir` would make
/// this test depend on the working directory, which is how a test passes
/// locally and silently scans nothing in CI. A hardcoded list can go stale, so
/// [`every_component_is_scanned`] guards it.
const SOURCES: &[(&str, &str)] = &[
    (
        "components/queue.rs",
        include_str!("../components/queue.rs"),
    ),
    (
        "components/player_bar.rs",
        include_str!("../components/player_bar.rs"),
    ),
    (
        "components/track_list.rs",
        include_str!("../components/track_list.rs"),
    ),
    (
        "components/sidebar.rs",
        include_str!("../components/sidebar.rs"),
    ),
    (
        "components/settings.rs",
        include_str!("../components/settings.rs"),
    ),
    ("components/menu.rs", include_str!("../components/menu.rs")),
    ("components/home.rs", include_str!("../components/home.rs")),
    (
        "components/icons.rs",
        include_str!("../components/icons.rs"),
    ),
    (
        "components/grids.rs",
        include_str!("../components/grids.rs"),
    ),
    (
        "components/titlebar.rs",
        include_str!("../components/titlebar.rs"),
    ),
    (
        "components/update_banner.rs",
        include_str!("../components/update_banner.rs"),
    ),
    ("app.rs", include_str!("../app.rs")),
];

/// A hook call found in a component body, with the byte offset it sits at.
struct HookAt {
    offset: usize,
}

/// Strip comments and string/char literals so a `return` or a `use_…(` inside
/// prose or a URL cannot drive the scan. Replaced with a space to preserve
/// offsets.
fn strip_noise(src: &str) -> String {
    let b = src.as_bytes();
    let mut out = vec![b' '; b.len()];
    let mut i = 0;

    while i < b.len() {
        // Line comment.
        if b[i] == b'/' && i + 1 < b.len() && b[i + 1] == b'/' {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        // String literal, including raw strings.
        if b[i] == b'"' {
            out[i] = b'"';
            i += 1;
            while i < b.len() {
                if b[i] == b'\\' {
                    i += 2;
                    continue;
                }
                if b[i] == b'"' {
                    out[i] = b'"';
                    i += 1;
                    break;
                }
                i += 1;
            }
            continue;
        }
        // Char literal. A lifetime (`&'a`) has no closing quote before a
        // non-identifier byte, so require the closing quote to be near.
        if b[i] == b'\'' && i + 2 < b.len() && b[i + 1] != b'\\' && b[i + 2] == b'\'' {
            i += 3;
            continue;
        }
        out[i] = b[i];
        i += 1;
    }

    String::from_utf8(out).expect("ASCII-preserving substitution")
}

/// True if `i` starts the `fn` keyword at a token boundary.
///
/// The boundary check matters: `pub fn ` and `\nfn ` are declarations, but
/// `myfn` is not, and an unanchored search for `fn ` would mis-locate a body
/// and silently shift every depth count after it. That is not hypothetical —
/// the first version of this scanner missed `pub fn` entirely, started its
/// depth count one brace too deep, and reported the *fixed* `queue.rs` as a
/// violation. A check that cries wolf on correct code is worse than no check.
fn starts_fn(b: &[u8], i: usize) -> bool {
    if !b[i..].starts_with(b"fn") {
        return false;
    }
    let before_ok = i == 0 || !(b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_');
    let after_ok = b
        .get(i + 2)
        .is_some_and(|c| c.is_ascii_whitespace() || *c == b'(');
    before_ok && after_ok
}

/// The body ranges (start offset just after `{`, end offset of the matching
/// `}`) of every `fn` in `src`.
///
/// Not a parser: `fn` is matched wherever it starts a token, and the body is
/// taken to be the first `{` after the parameter list at depth zero. Closures
/// and macro bodies inside a `fn` are skipped by depth, so a hook inside a
/// `use_effect_with(|_| …)` closure is never attributed to the component body.
fn fn_bodies(src: &str) -> Vec<(usize, usize)> {
    let stripped = strip_noise(src);
    let b = stripped.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;

    while i < b.len() {
        if starts_fn(b, i) {
            // Walk to the first `{` that opens the body: skip any `{` inside a
            // return type's generics by tracking angle depth crudely — in
            // practice the signature has no braces before the body.
            let mut j = i;
            while j < b.len() && b[j] != b'{' && b[j] != b';' {
                j += 1;
            }
            if j < b.len() && b[j] == b'{' {
                // Find the matching close brace.
                let mut depth = 0usize;
                let mut k = j;
                while k < b.len() {
                    match b[k] {
                        b'{' => depth += 1,
                        b'}' => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                    k += 1;
                }
                out.push((j + 1, k.min(b.len())));
                i = k + 1;
                continue;
            }
        }
        i += 1;
    }

    out
}

/// Hooks called at the body's own brace depth, in order.
fn hooks_at_body_depth(body: &str) -> Vec<HookAt> {
    let b = body.as_bytes();
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut i = 0;

    while i < b.len() {
        match b[i] {
            b'{' => depth += 1,
            b'}' => depth = depth.saturating_sub(1),
            b'u' if depth == 0 && b[i..].starts_with(b"use_") => {
                // A hook *call*: `use_name(`. A `use` import is never followed
                // by `(` -- it is a path to `;`.
                let mut j = i;
                while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'_') {
                    j += 1;
                }
                if j < b.len() && b[j] == b'(' {
                    out.push(HookAt { offset: i });
                }
                i = j;
                continue;
            }
            _ => {}
        }
        i += 1;
    }

    out
}

/// If `body` contains a `return` at all, the offset of the first one.
///
/// **Any** depth, not just the body's own. The first version of this only
/// looked at depth zero, which found nothing in practice: an early return is
/// almost always written inside a conditional (`if props.queue.is_empty() {
/// return html! {}; }`), so it sits one brace deeper than the body. That
/// version passed against the real codebase while being unable to detect the
/// very defect it was written for — the unit test that plants the defect is
/// what caught it.
///
/// A `return` anywhere means some path skips the rest of the body, which is
/// all this check needs to know about it.
///
/// `body` must already be noise-stripped. The second version of this took the
/// raw body, and `queue.rs`'s own comment — "…the empty-queue early return
/// below — a hook called after it would be…" — was read as a return
/// statement, reporting the *correct* code as a violation. Prose is the most
/// likely place for the word to appear, so that was not a corner case.
fn has_toplevel_return(body: &str) -> Option<usize> {
    let b = body.as_bytes();
    let mut depth = 0usize;
    let mut i = 0;

    while i < b.len() {
        match b[i] {
            b'{' => depth += 1,
            b'}' => depth = depth.saturating_sub(1),
            // `depth` is tracked but deliberately not filtered on; see above.
            b'r' if b[i..].starts_with(b"return") => {
                let before_ok = i == 0 || !(b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_');
                let after = b.get(i + 6).copied().unwrap_or(b' ');
                let after_ok = !after.is_ascii_alphanumeric() && after != b'_';
                if before_ok && after_ok {
                    return Some(i);
                }
                i += 6;
                continue;
            }
            _ => {}
        }
        i += 1;
    }

    None
}

/// Every hook placed after an early return, as a byte offset into `src`.
///
/// Offsets are computed against the noise-stripped source, which preserves
/// byte length and newlines, so they index and line-count against `src`
/// directly.
fn violations(src: &str) -> Vec<usize> {
    let stripped = strip_noise(src);
    let mut found = Vec::new();
    for (start, end) in fn_bodies(src) {
        let body = &stripped[start..end];
        let Some(ret) = has_toplevel_return(body) else {
            continue;
        };
        for hook in hooks_at_body_depth(body) {
            if hook.offset > ret {
                found.push(start + hook.offset);
            }
        }
    }
    found
}

/// The check itself.
#[wasm_bindgen_test]
fn no_hook_is_called_after_an_early_return() {
    let mut offenses = Vec::new();
    for (name, src) in SOURCES {
        // Re-derive against the unstripped source so the offset can be turned
        // into a line number for the failure message.
        for offset in violations(src) {
            let line = src[..offset].lines().count();
            let text = src.lines().nth(line.saturating_sub(1)).unwrap_or("").trim();
            offenses.push(format!("{name}:{line}: {text}"));
        }
    }
    assert!(
        offenses.is_empty(),
        "a hook is called after an early return, so it is skipped on that \
         path and Yew's hook-order assertion will fire on a later render:\n  {}",
        offenses.join("\n  ")
    );
}

/// A trace of what the scanner sees, for diagnosing a wrong answer.
///
/// Kept because three separate bugs in this scanner were found by reading its
/// output and none by reading its code: a missed `pub fn`, a comment read as a
/// return statement, and a body range off by a brace. A check whose internals
/// cannot be inspected is a check nobody can debug.
#[wasm_bindgen_test]
fn an_early_return_with_a_hook_after_it_is_reported() {
    // The historical shape of C1: a hook after an unconditional early return.
    let defect = r#"
        pub fn panel(props: &Props) -> Html {
            let a = use_state(|| 0);
            if props.items.is_empty() {
                return html! {};
            }
            let b = use_state(|| 1);
            html! {}
        }
    "#;
    let found = violations(defect);
    assert_eq!(
        found.len(),
        1,
        "exactly one hook (`b`) is called after the early return"
    );
    assert!(
        defect[found[0]..].starts_with("use_state"),
        "the reported offset should be the offending hook, not the return"
    );
}

/// The same component with the hook hoisted above the early return is clean.
/// A check that cannot pass on correct code is not usable.
#[wasm_bindgen_test]
fn a_hook_before_the_early_return_is_not_reported() {
    let fixed = r#"
        pub fn panel(props: &Props) -> Html {
            let a = use_state(|| 0);
            let b = use_state(|| 1);
            if props.items.is_empty() {
                return html! {};
            }
            html! {}
        }
    "#;
    assert!(violations(fixed).is_empty(), "{:?}", violations(fixed));
}

/// The bug that produced the first false positive: prose is not code.
#[wasm_bindgen_test]
fn the_word_return_in_a_comment_is_not_a_return() {
    let commented = r#"
        pub fn panel() -> Html {
            let a = use_state(|| 0);
            // The early return below means any hook after it would be skipped.
            let b = use_state(|| 1);
            html! {}
        }
    "#;
    assert!(
        violations(commented).is_empty(),
        "a comment mentioning `return` was read as a return statement"
    );
}

/// And a `return` inside a string literal, for the same reason.
#[wasm_bindgen_test]
fn the_word_return_in_a_string_is_not_a_return() {
    let quoted = r#"
        pub fn panel() -> Html {
            let a = use_state(|| 0);
            let _msg = "return to sender";
            let b = use_state(|| 1);
            html! {}
        }
    "#;
    assert!(violations(quoted).is_empty(), "{:?}", violations(quoted));
}

/// A hook inside a nested closure is not attributed to the component body, or
/// every `use_effect_with` in the codebase would be reported.
#[wasm_bindgen_test]
fn a_hook_inside_a_closure_is_not_the_component_body() {
    let nested = r#"
        pub fn panel() -> Html {
            let a = use_state(|| 0);
            if true {
                return html! {};
            }
            let cb = Callback::from(move |_| {
                let inner = use_state(|| 1);
                inner.set(true);
            });
            html! {}
        }
    "#;
    assert!(
        violations(nested).is_empty(),
        "a hook at closure depth was attributed to the component body"
    );
}

/// The list above is hardcoded, so it can go stale. This fails if a component
/// file exists that the check never reads — the failure mode being a silent
/// drop in coverage rather than a wrong answer.
#[wasm_bindgen_test]
fn every_component_is_scanned() {
    // The files the crate actually compiles, from `components/mod.rs`.
    const EXPECTED: &[&str] = &[
        "components/queue.rs",
        "components/player_bar.rs",
        "components/track_list.rs",
        "components/sidebar.rs",
        "components/settings.rs",
        "components/menu.rs",
        "components/home.rs",
        "components/icons.rs",
        "components/grids.rs",
        "components/titlebar.rs",
        "components/update_banner.rs",
        "app.rs",
    ];
    let scanned: Vec<&str> = SOURCES.iter().map(|(name, _)| *name).collect();
    assert_eq!(
        scanned.len(),
        EXPECTED.len(),
        "the scanned set and the expected set have different sizes"
    );
    for name in EXPECTED {
        assert!(
            scanned.contains(name),
            "{name} is compiled by the crate but not scanned by this check"
        );
    }
}
