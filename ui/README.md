# `rift-ui` — the frontend test harness

```sh
cargo test -p rift-ui --target wasm32-unknown-unknown --locked
```

The tests live in `src/tests/`, compiled only under `cfg(test)` — `trunk build`
and the shipped wasm never carry any of them. `src/tests/harness.rs` documents
what is real, what is stubbed, and why the seam is where it is.

## Running it

The test binary is a `.wasm` module, so Cargo cannot exec it. `.cargo/config.toml`
points the wasm32 target at `wasm-bindgen-test-runner`, which generates the JS
glue and runs the module under Node. That needs:

- **`wasm-bindgen-cli`, version-matched to `Cargo.lock`.** The runner refuses a
  test binary built against a different `wasm-bindgen`, and the error names a
  schema version rather than the number to change:

  ```sh
  cargo install wasm-bindgen-cli --version 0.2.128 --locked
  ```

  `.github/scripts/check-wasm-bindgen-version.sh` fails CI if the version the
  workflow installs and the version `Cargo.lock` resolves ever disagree.

- **Node** on `PATH`. Any recently-supported LTS will do; the runner emits
  CommonJS.

- **`NO_COLOR` must be `true` or unset if you build the app locally.** Trunk
  0.21 rejects `NO_COLOR=1` — and because `cargo tauri build` runs
  `trunk build --release` as its `beforeBuildCommand`, with the environment
  inherited, a `NO_COLOR=1` in your shell fails the whole application build with
  a message that names neither the variable nor the value:

  ```
  Error beforeBuildCommand `trunk build --release` failed with exit code 2
  ```

  CI never hits this (it does not set the variable), so it is a local-only trap.
  `$env:NO_COLOR = "true"` on Windows, `export NO_COLOR=true` elsewhere.

### On Windows, run them under WSL

They do not run on Windows, and this is not a gap in the harness.
`wasm-bindgen-test-runner` 0.2.128 formats the benchmark path into the generated
Node script with `Path::display()` and wraps it in single quotes, so a Windows
path becomes `'C:\Users\...'` — and Node rejects `\U` as an invalid Unicode
escape while *parsing* the file, before any test runs:

```
SyntaxError: Invalid Unicode escape sequence
    at .../run.cjs:55
        const benchmark_import = await fs.readFile('C:\Users\...\wbg_benchmark.json');
```

It is upstream, it is unconditional (the `readFile` sits inside `if (false)`,
which is still parsed), and 0.2.129 has not fixed it. `WASM_BINDGEN_BENCH_RESULT`
does not help: the path is re-encoded through `PathBuf` on the way in, so the
backslashes come back. There is no environment variable that avoids it.

CI therefore runs this step on Linux and macOS only. The tests are
platform-independent — the same wasm module, the same assertions — so the
Windows gap is in *reproducing* a failure locally, not in coverage. If you need
to run them on a Windows machine:

```sh
wsl -e bash -lc 'cd /mnt/c/.../Rift && cargo test -p rift-ui --target wasm32-unknown-unknown'
```

(WSL needs its own `wasm-bindgen-cli` and Node; the toolchain comes from
`rust-toolchain.toml`.)

## What is covered

| Area | Tests | What they pin |
|---|---|---|
| `api.rs` — the IPC boundary | `src/tests/api.rs` | The exact JS argument object each priority command sends: key names, key sets, and value **types**, read back through `serde_wasm_bindgen` so they are observed rather than restated. Plus the error sink, response deserialization, and the non-string-rejection fallback. |
| `components/reorder.rs` | `src/tests/reorder.rs` | `slot_at_offset` (which slot the pointer is over) and `drag_shift_style` (which way each row slides), including the last row's bottom edge, exact slot boundaries across a range of lengths and row heights, scroll offset, and that a preview is a permutation of the slots. |
| `app.rs` | `src/tests/app.rs` | `shuffled_start` (a shuffled Play must not start out of range), `fmt_total_duration`, `playlist_fully_downloaded` (the offline pin behind two user-visible behaviours). |
| Components, first render | `src/tests/components.rs` | That `QueuePanel` and `PlayerBar` render at all, and render what their props describe — the empty queue rendering nothing, a one-track queue rendering exactly one row, a loaded track's title and artist reaching the bar. A panic in a first render is a blank screen, so this is worth having. Also `track_key`, the dependency `H4`'s re-seed effect is keyed on: distinct tracks must produce distinct keys, or the effect stops re-running on a track change. |
| Hook order, statically | `src/tests/hook_order.rs` | That no hook is called after an early return, in any component source. This is the defect class behind `C1`. A token scanner over the component files, biased toward silence; it reports only what it can prove from brace depth, and its blind spots are listed in the module doc. |
### How components are rendered without a DOM

`yew`'s `ssr` feature drives the real component renderer to a string, so a
component can be rendered under the same Node runner as everything else. It is
declared as a **dev-dependency**, so the shipped bundle never links it —
measured, not assumed: `trunk build` with the line, without it, and with it
again produced a byte-identical wasm each time, and neither crate the feature
adds (`html-escape`, `base64ct`) appears in the non-test dependency tree.

Every one of these has been mutation-tested: the fix was reverted and the test
watched to fail. Two mutations *survived* and are recorded here rather than
quietly fixed, because they are the honest boundary of the suite:

- **Dropping `.floor()` or `.max(0.0)` from `slot_at_offset` changes nothing.**
  `as usize` on an `f64` truncates toward zero and saturates at 0, so both are
  redundant against the current implementation. They are kept as statements of
  intent, not as behaviour the tests defend. The `min(len - 1)` clamp, by
  contrast, is load-bearing and two tests fail without it.
- **Changing a `play_track` argument inside an `app.rs` closure is not caught by
  this suite.** That is why `fire_play_track` and `fire_play_tracks` exist as
  named functions: a `json!` literal inside a `Callback::from` cannot be reached
  by a test, and the original defect lived in exactly that position. The
  remaining call-site closures are still only guarded by the text-level check in
  `src-tauri/tests/ipc_contract.rs` — see the gap section below.

## What is *not* covered, and why

**Second renders.** The component tests render each component **once**. Yew's
server renderer drives a component instance a single time and does not run
effects, so nothing here sees a second render — and a second render is where
Yew's hook-order assertion lives, since it compares a render against the
*previous* one. Two attempts to force a second pass through SSR are recorded as
dead ends in `src/tests/components.rs` so nobody repeats them.

This is where `hook_order.rs` sits, and the distinction matters: it covers the
*structural* cause of `C1` (a hook below an early return) by reading the source,
which is decidable without a renderer. It does not cover second-render
*behaviour*, and it is not a renderer-level test. Closing that properly needs a
hook harness or a DOM shim driving the real client renderer — see below.

Note which half of the defect class it covers, because measuring showed the
other half is already a compile error. A hook written *inside* a conditional
block is rejected outright by `#[function_component]` ("hooks cannot be called
at this position"), so no test is needed for it — a rule was written and then
deleted for exactly that reason. `C1` is the half the macro cannot see: the hook
sits at the top level of the body, so the expansion is well-formed and nothing
disagrees until a second render happens.

**Rendering and computed style.** Two consequences worth stating plainly:

1. `H4` from `docs/code-review-2026-09.md` — PlayerBar's live progress surviving
   a track change — is a second-render question and is **not** covered as
   behaviour. What *is* covered is the property its fix rests on: `track_key`
   must give two different tracks two different keys, because the re-seed effect
   is keyed on it and a constant key is exactly how H4 happened. That is the
   logic half. The effect actually re-running, and the bar actually re-seeding,
   still need a second render.
2. The white album-text regression (`buttonface` inherited from a `<button>`)
   remains untestable here. It is a computed-style question: the correct answer
   requires a real browser, `getComputedStyle`, and a layout engine. That is a
   *different* purchase from this one — a browser runner (chromedriver, or
   `wasm-bindgen-test`'s in-browser mode) plus its installation and version
   pinning on three operating systems — and it is not included. A DOM shim would
   be the natural first step toward it and would also unlock `H4`.

**Other call sites' argument shapes.** `src-tauri/tests/ipc_contract.rs` parses
the frontend's `api::fire("cmd", json!({...}))` calls as text and compares key
names against the backend's parameter lists. It is the only guard on the call
sites this harness does not reach, and it is textual: it cannot see value types,
and it does not see through a helper that builds its payload somewhere else
(`fire_play_tracks` was invisible to it). The frontend tests here cover the two
`play_*` commands — the ones with a history — end to end; the rest of the 55 IPC
call sites have the text check and nothing else.

**Component props and callback wiring.** Which callback a button is bound to, and
whether it emits the right value, is renderer territory for the same reason.
