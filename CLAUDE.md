# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

Rift is an ad-free YouTube Music desktop player. It's a Cargo workspace of three crates:

- `types/` (`rift-types`) — serde data models shared by backend and frontend, plus the
  `events` module of IPC event channel names. Both sides depend on this; changing a struct
  here changes the wire format across the Tauri IPC boundary.
- `src-tauri/` (`rift`) — the Tauri 2 backend: search/metadata, playback, library, OS
  integration. Native code.
- `ui/` (`rift-ui`) — the Yew (Rust → WASM) frontend, built with Trunk.

The backend embeds the built frontend from `./dist` at compile time via `generate_context!`,
so **the frontend must be built before the backend**.

## Build & run

```sh
# 1. Build the WASM frontend (outputs to ./dist, which the backend embeds)
cd ui && trunk build --release && cd ..

# 2. Run the app
cd src-tauri && cargo run --release
```

For dev iteration on the frontend, `trunk serve` (port 1420) + `cargo tauri dev` works, but
the shipped app loads pre-built files from `../dist` — it does not serve them. For a
distributable bundle: `cargo install tauri-cli` then `cargo tauri build`.

Requires the `wasm32-unknown-unknown` target, Trunk, `webkit2gtk-4.1`, ALSA, and `yt-dlp`
(see "Streaming" below). See `README.md` for the full dependency table.

## Test, lint, probe

CI (`.github/workflows/ci.yml`) runs these on Linux/macOS/Windows; match it locally:

```sh
cargo fmt --all --check
cargo clippy -p rift-ui --target wasm32-unknown-unknown --locked -- -D warnings   # frontend
cargo clippy --workspace --exclude rift-ui --all-targets --locked -- -D warnings  # backend
cargo test --workspace --exclude rift-ui --locked                                 # backend
```

`rift-ui` is always excluded from backend clippy/test because it only compiles for the wasm
target. A single test: `cargo test --workspace --exclude rift-ui <test_name>`.

Verify the streaming pipeline end-to-end (search → resolve → download → decode) without
launching the UI:

```sh
cd src-tauri && cargo run --example probe -- your search terms
```

The probe works because `src-tauri/src/lib.rs` re-exports `fetch` as a library target, so
examples and tests can reuse the streaming logic.

## Architecture

### IPC contract

Frontend ↔ backend communicate over Tauri IPC. Two halves, both anchored in `rift-types`:

- **Commands** (frontend → backend): `#[tauri::command(rename_all = "snake_case")]` in
  `src-tauri/src/commands.rs`, registered in `generate_handler!` in `main.rs`, invoked from
  `ui/src/api.rs` with snake_case argument names. The snake_case convention must match on
  both sides or arguments silently fail to deserialize.
- **Events** (backend → frontend): channel names are constants in `rift_types::events`
  (e.g. `rift://track`, `rift://queue`). The backend `Emitter::emit`s; the frontend
  subscribes via `api::listen_event`. Subscriptions are intentionally leaked (live for the
  page lifetime).

`api::invoke` uses `serde_wasm_bindgen::Serializer::json_compatible()` deliberately — the
default serializer emits JS `Map`s, which the IPC layer rejects.

### Backend threading model

State lives in `AppState` (`main.rs`), managed by Tauri. The design is built around the fact
that several resources are **not `Send`** and so each lives on its own dedicated OS thread,
addressed through a cheap `Send + Sync` handle:

- **Audio** (`audio.rs`): rodio's output stream isn't `Send`, so it owns a thread driven by
  an `AudioCmd` channel; playback events flow back via a tokio channel. Tracks are downloaded
  fully into memory and handed over as bytes.
- **Media keys / now-playing** (`media.rs`): souvlaki's `MediaControls` isn't `Send` on
  Windows/macOS — owns a thread, addressed via `MediaHandle`.
- **Discord Rich Presence** (`discord.rs`): mirrors the `media.rs` design with
  `DiscordHandle`. Toggleable; advertises "Listening to Rift".
- **Disk persistence** (`util.rs` `Persister`): serializes best-effort writes (e.g. the
  playback snapshot) onto one background thread so callers never block on I/O.

`player.rs` is the orchestrator: `PlayerCore` (behind a `Mutex` in `PlayerShared`) holds
queue/playback state; `player::event_loop` consumes `AudioEvent`s and drives auto-advance,
radio-queue fill, and event emission. Note the **generation** counter (guards against a
skip racing an in-flight download) and **epoch** counter (guards stale radio fills after the
queue is replaced) in `PlayerCore`.

Mutexes are locked via `util::LockExt::lock_safe()`, which recovers poisoned guards instead
of panicking — the guarded data is plain values with no broken-on-panic invariants, so a
panicked thread shouldn't brick the whole app.

### Streaming (load-bearing yt-dlp fallback)

`fetch.rs` resolves a video ID to audio bytes via two strategies tried in order:

1. **rustypipe** (pure Rust) — preferred, kept first so playback returns to pure Rust if
   upstream recovers.
2. **yt-dlp** (subprocess) — fallback.

As of mid-2026, rustypipe 0.11.4 can't fetch full streams (YouTube caps tokenless clients to
~1 MB/URL and rustypipe's signature deobfuscator is broken; upstream dormant since mid-2025).
So **the yt-dlp fallback is currently load-bearing** — `yt-dlp` must be installed. A rustypipe
failure backs off to yt-dlp for the next `RUSTYPIPE_RETRY_AFTER` (50) fetches, then re-probes,
so a transient failure doesn't pin the whole session to the subprocess. rustypipe is still
used for search, metadata, and radio queues regardless.

### Persistence

User data dir is the Tauri `app_data_dir` (`~/.local/share/dev.jerimiah.rift/`):

- `library.json` — liked songs, playlists, recently played (`library.rs`). Durable user data,
  written synchronously.
- `playback.json` — queue snapshot, restored (stopped, not auto-playing) on next launch.
  Best-effort via `Persister`.
- `settings.json` — volume, Discord RPC toggle (`settings.rs`).
- `downloads/<id>.m4a` — offline audio. On-disk file presence is the source of truth for
  "downloaded" (`downloads.rs`).

Logs follow `RUST_LOG`, default `rift=debug,rustypipe=info`.

### Frontend

`ui/src/app.rs` is the root `App` component — it owns essentially all state, subscribes to
backend events on mount, and routes between views. `components.rs` holds presentational
components; `api.rs` is the IPC bridge. State flows down as props, actions go up as `fire`
(fire-and-forget command) calls.
