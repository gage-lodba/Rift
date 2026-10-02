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

**Rendering and computed style.** Nothing here drives the Yew renderer. That
needs a DOM, and no test in this tree may depend on a `window` — the same rule
that makes the backend's client tests use a loopback `TcpListener` rather than a
mock crate. Two consequences worth stating plainly:

1. `C1` from `docs/code-review-2026-09.md` — `use_state` after an early return in
   the queue panel — could not have been caught by this harness either. It needs
   a component mounted and re-rendered, not a function called. The invariant
   stays documented at the call site and nowhere else.
2. The white album-text regression (`buttonface` inherited from a `<button>`)
   remains untestable here. It is a computed-style question: the correct answer
   requires a real browser, `getComputedStyle`, and a layout engine. That is a
   *different* purchase from this one — a browser runner (chromedriver, or
   `wasm-bindgen-test`'s in-browser mode) plus its installation and version
   pinning on three operating systems — and it is not included. The gap is open,
   not closed by accident.

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
