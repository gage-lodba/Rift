# `rift-ui` — the frontend test harness

```sh
cargo test -p rift-ui --target wasm32-unknown-unknown --locked
```

The tests live in `src/tests/`, compiled only under `cfg(test)` — `trunk build`
and the shipped wasm never carry any of them. `src/tests/harness.rs` documents
what is real, what is stubbed, and why the seam is where it is; `src/tests/dom.rs`
does the same for the DOM half.

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

- **`jsdom`, installed with `npm install` in `ui/`.** The renderer tests mount
  real components into a real DOM, and `ui/tests/dom-shim.cjs` supplies it —
  see [The DOM shim](#the-dom-shim). This is a *Node* dependency, not a Cargo
  one: it is never linked into the wasm, because it never touches it. `ui/package.json`
  pins it (`jsdom@26.1.0`, chosen for a `node >= 18` floor so the CI runners'
  preinstalled Node cannot fall below it) and `ui/package-lock.json` pins what
  that pulls in. Without it the renderer tests fail with an explicit message
  telling you to run `npm install`; nothing else is affected.

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

### The DOM shim

`ui/tests/dom-shim.cjs` builds a `jsdom` document and installs its globals into
Node *before* the test module loads, so `web-sys` finds a `window` when the first
component mounts. Node's `--require` is the only injection point the runner
offers, and `.cargo/config.toml`'s `[env]` table supplies it:

```toml
[env]
NODE_OPTIONS = { value = "--require ui/tests/dom-shim.cjs", relative = true }
```

Two things about that file are load-bearing and were each found by a broken run:

- **The shim must define `pre#output`.** `wasm-bindgen-test` decides between its
  Node and browser output formatters by looking for a `self` property on the
  global object. A DOM has one, so the *browser* formatter is what runs, and it
  writes every result line into `document.getElementById("output")`. Without
  that element the run dies on `Cannot read properties of null` before a single
  test executes. With it, the shim mirrors those writes to stdout, so a CI log
  shows `test result: ok. 53 passed; …` rather than nothing at all.
- **The shim must not shadow `performance`, `crypto`, `fetch` and friends.**
  jsdom's own implementations delegate to the global one —
  `Performance-impl.js` is literally `performance.now() - this._nowAtOrigin` —
  so mirroring jsdom's wrapper over Node's built-in makes it call itself and the
  run dies with `Maximum call stack size exceeded` before any test executes.

`node scripts/check-dom-shim.cjs` checks the shim on its own, in about a second,
without compiling anything: it loads the shim, builds elements, dispatches an
event, and reads a style property back. Run it first if a renderer test fails in
a way that mentions a missing global.

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

(WSL needs its own `wasm-bindgen-cli`, Node and `jsdom`; the toolchain comes from
`rust-toolchain.toml`.)

**One wrinkle when running under WSL against a Windows checkout.**
`.cargo/config.toml` resolves the shim path relative to itself, so from WSL it is
the *Windows* path `C:\Users\…\ui\tests\dom-shim.cjs`, which WSL's Node cannot
load. The four renderer tests then fail — with the message in
`the_dom_shim_is_loaded`, which says exactly this — while the other 49 pass. Set
`NODE_OPTIONS` explicitly for that run:

```sh
wsl -e bash -lc 'cd /mnt/c/.../Rift && \
  NODE_OPTIONS="--require $PWD/ui/tests/dom-shim.cjs" \
  cargo test -p rift-ui --target wasm32-unknown-unknown'
```

A WSL-native checkout, and every other platform, needs no override.

## What is covered

| Area | Tests | What they pin |
|---|---|---|
| `api.rs` — the IPC boundary | `src/tests/api.rs` | The exact JS argument object each priority command sends: key names, key sets, and value **types**, read back through `serde_wasm_bindgen` so they are observed rather than restated. Plus the error sink, response deserialization, and the non-string-rejection fallback. |
| `components/reorder.rs` | `src/tests/reorder.rs` | `slot_at_offset` (which slot the pointer is over) and `drag_shift_style` (which way each row slides), including the last row's bottom edge, exact slot boundaries across a range of lengths and row heights, scroll offset, and that a preview is a permutation of the slots. |
| `app.rs` | `src/tests/app.rs` | `shuffled_start` (a shuffled Play must not start out of range), `fmt_total_duration`, `playlist_fully_downloaded` (the offline pin behind two user-visible behaviours). |
| Components, first render | `src/tests/components.rs` | That `QueuePanel` and `PlayerBar` render at all, and render what their props describe — the empty queue rendering nothing, a one-track queue rendering exactly one row, a loaded track's title and artist reaching the bar. A panic in a first render is a blank screen, so this is worth having. Also `track_key`, the dependency `H4`'s re-seed effect is keyed on: distinct tracks must produce distinct keys, or the effect stops re-running on a track change. |
| **Components, second render** | `src/tests/render.rs` | The real Yew **client** renderer in a real DOM: that the harness runs effects and re-renders at all; that the queue panel survives **empty-then-populated**, the transition `C1` broke and the one the app actually starts on; and that `PlayerBar` re-seeds from the app's props across a **track change**, using the PROGRESS subscription path (`harness::emit_for_test`) rather than calling a callback directly. What this does *not* catch is listed below. |
| Hook order, statically | `src/tests/hook_order.rs` | That no hook is called after an early return, in any component source. This is the defect class behind `C1`. A token scanner over the component files, biased toward silence; it reports only what it can prove from brace depth, and its blind spots are listed in the module doc. The same module documents the **manual** check that a re-injected defect is caught by the render harness — it cannot be a test, for the reason in the next section. |
### How components are rendered

Two renderers, on purpose:

- **Yew's `ssr` feature** drives the real component renderer to a string with no
  DOM at all, for `components.rs`. It is declared as a **dev-dependency**, so the
  shipped bundle never links it — measured, not assumed: `trunk build` with the
  line, without it, and with it again produced a byte-identical wasm each time,
  and neither crate the feature adds (`html-escape`, `base64ct`) appears in the
  non-test dependency tree.
- **Yew's CSR renderer, in jsdom**, for `render.rs`. This is the only way to see
  a second render, an effect, or a callback. Nothing it uses reaches the shipped
  bundle either: `csr` is already on through the app's own dependency, and the
  DOM is a Node-side package (`ui/package.json`), so `trunk build` is unaffected
  in both directions.

Every one of these has been mutation-tested: the fix was reverted and the test
watched to fail. Three mutations *survived* and are recorded here rather than
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
- **Removing the track key from `PlayerBar`'s re-seed effect does not fail any
  test in `render.rs`.** Measured, by building it both ways. The stale `live`
  values are masked on every render by `track_changed`, which stays `true`
  forever when the effect never re-runs — so the bar falls back to the new
  track's own duration and a zeroed position, which is what it shows for the one
  render *before* the effect fires anyway. The defect and the fix therefore
  differ for exactly one render, and this harness cannot sample it: Yew renders
  and runs effects in the same scheduler pass, which a single microtask drains
  in full (measured, hop by hop). That is recorded as open below, not papered
  over.

## What is *not* covered, and why

**A single render is not observable.** `flush()` yields to the microtask queue
until the scheduler is quiescent, and Yew renders a component and runs its
effects inside one scheduler pass — so there is no point at which a test can
observe "rendered but effects not yet run". That window is exactly where `H4`'s
remaining difference lives (see the third surviving mutation above). Seeing it
needs a real frame boundary, i.e. a browser, which is the computed-style
purchase below rather than a separate one.

**Nothing can assert that a panic happened.** On `wasm32-unknown-unknown` there
is no unwinding: `panic = "abort"` is the only strategy and
`wasm_bindgen::__rt::maybe_catch_unwind` is the identity function without
`panic = "unwind"`. A panic raised by a render happens on a microtask, so the
process dies with `RuntimeError: unreachable` and no test results at all. The
harness installs a panic hook that prints the message before the abort
(`dom.rs`), which is a diagnostic and not a detector. This is why the C1
detector is a manual procedure in `hook_order.rs` and why no test here asserts on
a panic.

**Second-render behaviour, generally.** The component tests in `components.rs`
render each component **once**, through SSR, which drives a component instance a
single time and does not run effects — two attempts to force a second pass are
recorded as dead ends in that file. `render.rs` does reach second renders, but
only through the one `flush()` granularity described above.

**Computed style — the class of bug that already shipped.** The white album text
came from a `<button>` inheriting `buttonface`, and nothing here can see it.
jsdom has no layout engine, so `getComputedStyle` answers from inline and
stylesheet declarations only, and an *inherited* value is not observable. This
needs a real browser (`getComputedStyle` with a layout engine): chromedriver or
`wasm-bindgen-test`'s in-browser mode, plus its installation and version pinning
on three operating systems. **This gap is open, and the DOM shim does not close
it** — it is the stepping stone toward it, not the thing itself. The harness is
deliberately not described as covering it.

**Other call sites' argument shapes.** `src-tauri/tests/ipc_contract.rs` parses
the frontend's `api::fire("cmd", json!({...}))` calls as text and compares key
names against the backend's parameter lists. It is the only guard on the call
sites this harness does not reach, and it is textual: it cannot see value types,
and it does not see through a helper that builds its payload somewhere else
(`fire_play_tracks` was invisible to it). The frontend tests here cover the two
`play_*` commands — the ones with a history — end to end; the rest of the 55 IPC
call sites have the text check and nothing else.

**Component props and callback wiring.** Which callback a button is bound to, and
whether it emits the right value, is renderer territory — but no test drives a
click yet. The DOM makes it possible (`dispatch_event` works under the shim, and
`check-dom-shim.cjs` proves it); nothing does it. Recorded as open.

