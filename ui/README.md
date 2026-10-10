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
NODE_OPTIONS = "--require ./tests/dom-shim.cjs"
```

The path is relative to the **runner's** working directory, and Cargo sets that to
the package root, not to wherever you invoked Cargo. Measured with a probe runner
from the repo root, from `ui/`, from `ui/src/`, and through `--manifest-path`:
every one of them ran the runner with `cwd = ui/`, so `./tests/dom-shim.cjs` is
`ui/tests/dom-shim.cjs`. The leading `./` is load-bearing — without it Node reads
the value as a bare package specifier, searches `node_modules` and exits with
`MODULE_NOT_FOUND` before the test module loads. Invoked from *outside* the repo
with `--manifest-path`, Cargo does not read that file at all: it finds
`.cargo/config.toml` by walking up from the working directory.

**`relative = true` cannot be used for this value, and it is the trap this file
fell into until 2026-09.** Cargo path-joins the *whole* value onto the config's
base directory, flag included:

```toml
NODE_OPTIONS = { value = "--require ui/tests/dom-shim.cjs", relative = true }
# => NODE_OPTIONS = "C:\Users\...\Rift\--require ui/tests/dom-shim.cjs"
```

Node **silently ignores** `NODE_OPTIONS` tokens that do not begin with `-`, so
that is not a startup error. The shim simply never loads, and the four renderer
tests fail with `the DOM shim installs \`window\` before any test runs` — a message
about a missing global that names neither the variable nor the reason. Measured
both ways: the value above gives `49 passed; 4 failed`, the value in the table
gives all of them passing. **CI has never exercised the fixed table**, because the
branch is unpushed — the Linux and macOS jobs have been running the broken value
too, and their four renderer tests have been failing with it.

`force` is deliberately left unset on the entry, so a `NODE_OPTIONS` you export
yourself wins (Cargo warns that it is skipping this one) rather than being
silently replaced. That is what isolated the bug: exporting the working value was
enough to turn the four failures green.

Two further things about the shim are load-bearing and were each found by a broken
run:

- **The shim must define `pre#output`.** `wasm-bindgen-test` decides between its
  Node and browser output formatters by looking for a `self` property on the
  global object. A DOM has one, so the *browser* formatter is what runs, and it
  writes every result line into `document.getElementById("output")`. Without
  that element the run dies on `Cannot read properties of null` before a single
  test executes. With it, the shim mirrors those writes to stdout, so a CI log
  shows `test result: ok. 53 passed; …` rather than nothing at all.
- **The mirror writes only the new text.** It re-printed the whole element on
  every DOM mutation until 2026-09, which grows the log quadratically: a 53-test
  run logged 369 lines, every one of them a longer prefix of the same report,
  with the one line that matters buried at the end. It now writes the delta
  against what it has already written (a common-prefix diff, so a rewrite of the
  element is still handled), and a 73-test run logs 74 lines.
- **The shim must not shadow `performance`, `crypto`, `fetch` and friends.**
  jsdom's own implementations delegate to the global one —
  `Performance-impl.js` is literally `performance.now() - this._nowAtOrigin` —
  so mirroring jsdom's wrapper over Node's built-in makes it call itself and the
  run dies with `Maximum call stack size exceeded` before any test executes.

Two traps that are about *driving* the DOM rather than supplying it, both found
while writing `interact.rs`:

- **A test that panics leaves its mount in the document.** `Mount` removes its
  container in `Drop`, but `wasm32-unknown-unknown` has no unwinding, so a failing
  assertion never runs `Drop`: the container, its DOM and whatever it had focused
  all survive into the next test. Down a red run, a later focus assertion can
  therefore read a foreign node that looks exactly like the one it wanted.
  `Mount::active_description` says "NOT inside this mount" when that happens. (It
  is *not* jsdom keeping focus on a detached node — measured: jsdom moves
  `activeElement` back to `<body>` when the focused element or its container is
  removed, and `check-dom-shim.cjs` pins that.)
- **Events are built with the DOM's own constructors, not web-sys's init
  dictionaries.** `new_with_keyboard_event_init_dict` needs the
  `KeyboardEventInit` feature, which nothing in this crate's graph enables, and
  web-sys features are global rather than test-scoped — turning it on would put
  those bindings in the app build too. `dom.rs` reaches `new KeyboardEvent(...)`
  and `new MouseEvent(...)` through `Reflect` instead, with `bubbles: true`
  (load-bearing: Yew delegates to the subtree root). `click`, `keydown` and
  `contextmenu` are all proven. The third is not incidental: it is the row-level
  way into a kebab menu, so a test that needs a menu open *inside a row* has to
  dispatch one.

`node scripts/check-dom-shim.cjs` checks the shim on its own, in about a second,
without compiling anything: it loads the shim, builds elements, dispatches all
three event kinds the interaction tests use (`click`, `keydown`, `contextmenu`)
through the DOM's own constructors, reads a style property back, and pins what
`focus()` does — including that removing the focused element puts
`activeElement` back on the body. Run it first if a renderer test fails in a way
that mentions a missing global.

### Two ways this suite dies on Windows, and they are not the same failure

Both were live until 2026-09 and both are fixed in `.cargo/config.toml`; they are
kept here because the messages they produce say nothing about their cause.

1. **The generated script does not parse, so *no* test runs.** The runner writes
   the benchmark baseline path into the Node script it generates, with
   `Path::display()` inside a single-quoted JS string:

   ```js
   const benchmark_import = await fs.readFile('C:\...\Rift\ui\target\wbg_benchmark.json');
   ```

   and `\ui` is an invalid Unicode escape — `\u` must be followed by four hex
   digits — so Node rejects the file while parsing it. (The trigger is `\ui`, not
   `\U`: JS accepts `'\U'` and `'\Users'` as `NonEscapeCharacter`s and the caret
   in Node's error points at the package directory. The earlier version of this
   file blamed `\U` and was wrong about the character, right about the class.)
   The `readFile` sits in an `if (is_bench)` branch a `cargo test` run never
   takes, which changes nothing: a syntax error is a syntax error whether or not
   its branch executes. Fixed by pinning `WASM_BINDGEN_BENCH_RESULT` to the same
   location in a form that always parses — `target/wbg_benchmark.json`, relative
   to the runner's cwd, so it is the path the runner would have chosen itself.
   Only `cargo bench` ever reads it.

2. **The shim does not load, so the four renderer tests fail.** The
   `NODE_OPTIONS` trap above, which is not Windows-specific at all: it was broken
   on every platform and looked like a platform problem because the four failures
   it causes are about a missing DOM.

Neither needs WSL. Earlier revisions of this file recommended running the suite
under WSL and explained the four failures as a Windows path reaching a Linux Node;
that diagnosis was wrong — the path never got as far as Node — and the note has
been removed rather than corrected in place, because "run it somewhere else" was
never the fix.

The suite runs on this Windows machine with the toolchain already on `PATH`:

```sh
cargo test -p rift-ui --target wasm32-unknown-unknown --locked
```

## What is covered

| Area | Tests | What they pin |
|---|---|---|
| `api.rs` — the IPC boundary | `src/tests/api.rs` | The exact JS argument object each priority command sends: key names, key sets, and value **types**, read back through `serde_wasm_bindgen` so they are observed rather than restated. Plus the error sink, response deserialization, and the non-string-rejection fallback. |
| `components/reorder.rs` | `src/tests/reorder.rs` | `slot_at_offset` (which slot the pointer is over) and `drag_shift_style` (which way each row slides), including the last row's bottom edge, exact slot boundaries across a range of lengths and row heights, scroll offset, and that a preview is a permutation of the slots. |
| `app.rs` | `src/tests/app.rs` | `shuffled_start` (a shuffled Play must not start out of range), `fmt_total_duration`, `playlist_fully_downloaded` (the offline pin behind two user-visible behaviours). |
| Components, first render | `src/tests/components.rs` | That `QueuePanel` and `PlayerBar` render at all, and render what their props describe — the empty queue rendering nothing, a one-track queue rendering exactly one row, a loaded track's title and artist reaching the bar. A panic in a first render is a blank screen, so this is worth having. Also `track_key`, the dependency `H4`'s re-seed effect is keyed on: distinct tracks must produce distinct keys, or the effect stops re-running on a track change. |
| **Components, second render** | `src/tests/render.rs` | The real Yew **client** renderer in a real DOM: that the harness runs effects and re-renders at all; that the queue panel survives **empty-then-populated**, the transition `C1` broke and the one the app actually starts on; and that `PlayerBar` re-seeds from the app's props across a **track change**, using the PROGRESS subscription path (`harness::emit_for_test`) rather than calling a callback directly. What this does *not* catch is listed below. |
| **A keyed list re-decorated, and how coarse a pass is** | `src/tests/rerender.rs` | Two rows whose playing / liked / offline decoration is moved from the first to the second in one update, so the *right* row is what changes — `H4`'s bug one level down, where the component below is keyed on a track and the value above it is not. Plus the measurement that bounds the whole file: **one microtask hop carries a render and its effects**, asserted by showing the change has not been applied before the hop and has been applied after it. |
| **Clicks and keystrokes, on real components** | `src/tests/interact.rs` | The menu's keyboard path (open → focus the first item → Arrow keys with wrap, Home/End, ArrowRight/ArrowLeft on a submenu, Escape returning focus to its anchor, Enter activating an item and closing), the backdrop closing it, one control→callback wiring per component (queue rows, the remove button, the clear button, a track row's like button, an artist credit), and the two guards that decide whether a keystroke belongs to a row or to a control inside it. Every assertion here is a real `click()` or a real dispatched, bubbling `keydown`/`contextmenu` through Yew's delegated listeners. |
| Hook order, statically | `src/tests/hook_order.rs` | That no hook is called after an early return, in any component source. This is the defect class behind `C1`. A token scanner over the component files, biased toward silence; it reports only what it can prove from brace depth, and its blind spots are listed in the module doc. Its *own* false positive — a `return` inside a keydown closure read as an early return from the component — is fixed and pinned by two regression tests built from the exact shape that misfired. The same module documents the **manual** check that a re-injected defect is caught by the render harness — it cannot be a test, for the reason in the next section. |
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
watched to fail. Four mutations *survived* and are recorded here rather than
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
- **Removing `.trow-actions`' `stop_propagation` on its own changes nothing
  observable.** Every interactive child inside that container stops the event
  itself — the like button, the kebab, and the menu panel — so the container's
  guard is redundant with theirs, and theirs with it:
  `clicking_like_likes_the_track_and_does_not_play_it` only fails when *both* are
  removed. Measured. The pair is kept, because which one is load-bearing is a
  property of the markup that a future row could change; what the test pins is
  the behaviour, that a click on Like does not also play the track.
- **Removing the track key from `PlayerBar`'s re-seed effect does not fail any
  test in `render.rs`.** Measured, by building it both ways. The stale `live`
  values are masked on every render by `track_changed`, which stays `true`
  forever when the effect never re-runs — so the bar falls back to the new
  track's own duration and a zeroed position, which is what it shows for the one
  render *before* the effect fires anyway. The defect and the fix therefore
  differ for exactly one render, and this harness cannot sample it: Yew renders
  and runs effects in the same scheduler pass, which one microtask drains in
  full. That is now a **test** rather than a note —
  `rerender.rs::one_microtask_hop_carries_a_render_and_its_effects` shows the
  change has not been applied before the hop and has been applied after it — and
  it is recorded as open below, not papered over.

## What is *not* covered, and why

**A single render is not observable, and that is now measured rather than
asserted.** `flush()` yields to the microtask queue until the scheduler is
quiescent, and Yew renders a component and runs its effects inside one scheduler
pass. The narrower question — *can a test sample the DOM after a render but
before that render's effects?* — reduces to how much work one turn of the JS job
queue carries, and `rerender.rs::one_microtask_hop_carries_a_render_and_its_effects`
answers it with both data points in one test: immediately after a state change
nothing has been applied (the log is empty and the DOM still shows the old
value), and after exactly one hop the render **and** its effect have both run. A
test can only resume between turns of the job queue, so there is no point at
which it can observe that window. That window is exactly where `H4`'s remaining
difference lives (see the fourth surviving mutation above). Seeing it needs a
real frame boundary — an animation frame is not a microtask — i.e. a browser,
which is the computed-style purchase below rather than a separate one. If a Yew
upgrade ever splits a render from its effects across two hops, that test fails
and its message says the `H4` detector has become possible.

**Nothing can assert that a panic happened.** On `wasm32-unknown-unknown` there
is no unwinding: `panic = "abort"` is the only strategy and
`wasm_bindgen::__rt::maybe_catch_unwind` is the identity function without
`panic = "unwind"`. A panic raised by a render happens on a microtask, so the
process dies with `RuntimeError: unreachable` and no test results at all. The
harness installs a panic hook that prints the message before the abort
(`dom.rs`), which is a diagnostic and not a detector. This is why the C1
detector is a manual procedure in `hook_order.rs` and why no test here asserts on
a panic.

**Second-render behaviour beyond the three files that have it.**
`components.rs` renders each component **once**, through SSR, which drives a
component instance a single time and does not run effects — two attempts to force
a second pass are recorded as dead ends in that file. `render.rs` covers
`QueuePanel` and `PlayerBar`, `rerender.rs` covers `TrackList`, and
`interact.rs` reaches every re-render a click causes. Every other component
(Sidebar, SettingsView, HomeView, the grids, Titlebar, UpdateBanner) has only
first-render or pure-function coverage. Nothing stops the next one from getting a
second-render test; nobody has needed one yet.

**One CSR `PlayerBar` per test process, and that is a harness limit.** `LISTENERS`
in `harness.rs` records every event subscription for the life of the process —
there is no unsubscribe, because the stub replaces the transport and the real
`listen_event` has no teardown to mirror. `render.rs`'s `H4` test therefore
asserts that `emit_for_test` reaches **exactly one** PROGRESS listener, and any
*second* test that mounts `PlayerBar` through the client renderer makes that
count two and fails it. `interact.rs` deliberately does not mount `PlayerBar`
for that reason, and asserts on components that subscribe to nothing. The honest
fix is a per-test reset in `harness.rs` plus a subscription that can be dropped;
neither was needed for this session's work, so neither is here.

**Real activation of `<button>`s, and CSS-only reveal, are invisible here.**
jsdom performs no default activation, so Enter or Space on a real `<button>`
does nothing: `interact.rs` asserts button behaviour with `click()` and
keyboard behaviour on the *handler's* effect, and says so in each test. The
review's "the sidebar kebab was unreachable by Tab" class is CSS —
`.side-item:focus-within .side-kebab { display: … }` — and jsdom's
`getComputedStyle` does not do `:focus-within` or layout, so the reveal itself is
review-only. What a test *can* pin without a browser is that a `button[role="switch"]`
has a name (`aria-label`) and `aria-checked`, and that a failed download button
has a title; `interact.rs` does the ones it mounts.

**Computed style — the class of bug that already shipped.** The white album text
came from a `<button>` whose colour was left to the UA, and the fix (in
`styles.css`) is `.link { color: inherit; background: none; font: inherit }`.
Measured, on this machine, with the same markup in both engines:

| value | jsdom (through the shim) | Edge, headless |
|---|---|---|
| the album text's `color`, no colour declared (the defect) | `"buttontext"` | `rgb(0, 0, 0)` — and the render shows it invisible on the dark body |
| the same element with a colour declared on the button | `"rgb(230, 230, 230)"` | `rgb(230, 230, 230)` |
| the same element with `color: inherit` (what the fix says) | `"inherit"` | `rgb(230, 230, 230)` |
| the same element with `color: var(--text)` | `"var(--text)"` | `#ececef` |

So the accurate statement is narrower than "nothing here can see it", and wider
than "this is closed": jsdom **does** apply the cascade and inherit declared
values, but it reports unresolved tokens (`buttontext`, `inherit`,
`var(--text)`) rather than resolved colours, and it resolves nothing against a
layout. A test could therefore assert "a colour declaration reached this element"
by loading the real `styles.css` into the shim's document and checking that the
value is not the UA keyword — a proxy for the shipped defect, not a rendering.

**What that would take, concretely** — three steps and one measured caveat, so
the next session does not re-derive it:

1. Inject `ui/styles.css` into the shim's document as a `<style>` node. The shim
   already resolves files through `createRequire`, so this is a `readFileSync`
   plus `document.createElement('style')`; without it `getComputedStyle` has no
   rules to cascade.
2. Mount the component that carries the defect (`TrackList` for the album button,
   `PlayerBar` for its own) and assert the element's computed colour is **not**
   `"buttontext"`. That is the assertable half, and the two states are
   distinguishable here: the shipped fix is `color: inherit` (`styles.css:363`),
   which jsdom reports as `"inherit"`, and the defect reports the UA keyword.
3. Prove it with a mutation: delete `color: inherit` from `.link` in `styles.css`
   and watch the assertion fail.

The caveat is measured rather than assumed: jsdom hands back `var(--text)` and
`inherit` as raw tokens, so assert on "not the UA keyword", never on a specific
colour, and pick an element that does not depend on a custom property being
resolved. The 1139-line stylesheet also gets parsed on every run, which is worth
weighing before wiring it in. That is **not built**, and the measurement is
recorded here so the next person starts from facts instead of the old claim.

The real-browser route is cheaper than it looks, and was used for the numbers
above: `msedge --headless=new --screenshot=<png> --window-size=1000,300 <url>`
renders and screenshots with **no driver and no dependency** (Edge is on Windows,
Chrome is on the CI runners). Reading the value back out of a screenshot is fine
for a one-off measurement and useless for a suite; wiring it into CI still means
a driver and a version pin, which is why the harness does not claim computed
style.

**Other call sites' argument shapes.** `src-tauri/tests/ipc_contract.rs` parses
the frontend's `api::fire("cmd", json!({...}))` calls as text and compares key
names against the backend's parameter lists. It is the only guard on the call
sites this harness does not reach, and it is textual: it cannot see value types,
and it does not see through a helper that builds its payload somewhere else
(`fire_play_tracks` was invisible to it). The frontend tests here cover the two
`play_*` commands — the ones with a history — end to end; the rest of the 55 IPC
call sites have the text check and nothing else.

**A guard that decides by tag cannot see a role.** `key_on_nested_control`
matches the keydown's target tag (`BUTTON | INPUT | SELECT | TEXTAREA | A`). A
row's kebab menu renders *inside* the row and its items are
`<div role="menuitem" tabindex="0">` — a `DIV` — so the guard said "not a nested
control" and Enter on a menu item activated both the menu item and the row.
Found by the verifier against the previous session's commit and now pinned by
`interact.rs::enter_on_a_menu_item_inside_a_row_does_not_play_the_row`, whose
control case (the same dispatch aimed at the row *must* fire) is what makes the
negative non-vacuous. The general lesson is in that test, not in the guard: a
tag list is a heuristic, and only a renderer-level test can see what a role-based
control actually does.

