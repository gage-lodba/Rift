# Rift

[![CI](https://github.com/gage-lodba/Rift/actions/workflows/ci.yml/badge.svg)](https://github.com/gage-lodba/Rift/actions/workflows/ci.yml)
[![License: Apache-2.0](https://img.shields.io/badge/License-Apache--2.0-blue.svg)](LICENSE)
![Rust 1.88](https://img.shields.io/badge/rust-1.88-orange.svg)

A minimal, ad-free desktop music player for YouTube Music — written entirely in
Rust and rendered directly with **Vulkan**. No webview, no browser engine, no
JavaScript.

![Rift — home screen](Previews/image.png)

## Features

- Search songs, artists and albums; browse artist pages (top songs, full
  discography split into albums and singles) and album pages.
- Shuffle and repeat playback modes.
- Like songs, build and manage playlists, and resume your session on next launch.
- Download tracks for offline listening, with a determinate progress bar under
  the song while it transfers.
- Native audio streaming: a clean-room, pure-Rust, JS-less innertube resolver and
  ranged downloader. No external media binary is needed
  ([design](docs/native-streaming.md)).
- Crossfade between tracks, OS media-key / now-playing integration, and Discord
  Rich Presence.
- Filters out music-video versions so results stay audio-only.
- Built-in update checker.

## Download

Prebuilt binaries are attached to the
[latest GitHub release](https://github.com/gage-lodba/Rift/releases/latest):
Windows (`rift-music-windows-x86_64.exe`), macOS (`rift-music-macos-universal`)
and Linux (`rift-music-linux-x86_64`). The app needs a Vulkan-capable GPU driver;
audio streams natively.

## Requirements

| Platform | Requirements |
|---|---|
| **Windows** | Windows 10/11, an up-to-date GPU driver with Vulkan, and the [Visual Studio C++ Build Tools](https://visualstudio.microsoft.com/visual-cpp-build-tools/) when building from source |
| **macOS** | macOS 13+, an up-to-date GPU driver, and Xcode Command Line Tools when building from source |
| **Linux** | A Vulkan driver, ALSA and the X11/Wayland/xkbcommon development headers |

Also required: **Rust 1.88** (pinned by [`rust-toolchain.toml`](rust-toolchain.toml)).

Linux build dependencies:

```sh
sudo apt install libasound2-dev libvulkan-dev libxkbcommon-dev libwayland-dev \
  libx11-dev libx11-xcb-dev libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev
```

## Build & run

```sh
git clone https://github.com/gage-lodba/Rift.git
cd Rift
cargo run -p rift-desktop              # debug build, live backend
cargo build --release -p rift-desktop  # release binary: target/release/rift-music
```

## Development

```sh
cargo test -p rift -p rift-types -p rift-ytmusic -p rift-discord -p rift-desktop
cargo clippy -p rift -p rift-types -p rift-ytmusic -p rift-discord -p rift-desktop --all-targets -- -D warnings
cargo fmt --all -- --check
```

Audio is resolved and downloaded by the native path
(`rift_ytmusic::player` → `rift::stream`) and decoded with rodio; there is no
external downloader, runtime switch or fallback in the pipeline. Upgrading from a
build that used yt-dlp is a no-op: an existing `<data dir>/bin/yt-dlp[.exe]` (and
a stale `yt_dlp_path` key in `settings.json`) is ignored and left untouched.

Manual, network-requiring probes back the native-streaming work
([design](docs/native-streaming.md)):

- `cargo run -q -p rift-ytmusic --example resolve_stream` — runs a 20-id
  coverage sample (pass video ids to override) and prints which innertube client
  served each stream after a ranged-GET check.
- `cargo run -p rift --example native_probe` — fetches a sample through the
  playback path (`fetch::fetch_bytes`), decodes each result with the playback
  decoder and prints bytes/timing/duration.
- `cargo run -p rift --example download_probe` — drives the real backend's
  download worker against a throwaway data directory, prints the progress
  snapshots the UI receives, then proves a restarted backend discovers the copy
  and replays it offline through `fetch::fetch_track`.

The end-to-end app path (controller/events) is covered by ignored live tests:

```sh
cargo test -p rift-desktop --features backend --test live_backend -- \
  --ignored --nocapture --test-threads=1 live_native_playback_download_progress_and_offline_restart
```

`cargo run` pays Cargo's freshness check plus the loader cost of the (much
larger) debug executable; for the fastest dev-mode launch, run the built binary
directly. The startup timeline (see [`docs/architecture.md`](docs/architecture.md#startup-path-and-timing))
is logged with `RUST_LOG=rift=debug,rift_desktop=debug`, and dev builds keep line
tables for backtraces without full DWARF.

The full local acceptance gate (formatting, lints, docs, both feature
configurations, release build, offscreen screenshot comparison, dependency guards)
runs on Windows:

```powershell
pwsh tools/ci-local.ps1
```

For UI work without real data, development builds support deterministic preview
mode (placeholder library, no backend, no network). Preview and screenshot
support is dev-only and is not compiled into shipped release binaries:

```sh
cargo run -p rift-desktop -- --preview --screenshot artifacts/screens/home.png --size 1160x740 --view home
RIFT_PREVIEW=1 cargo run -p rift-desktop
```

## Architecture

One `cargo build`, one binary. Everything is plain Rust:

| Path | Crate | What it is |
|---|---|---|
| [`crates/rift-desktop/`](crates/rift-desktop/) | `rift-desktop` | **The application.** `winit` window, `vulkano` renderer and a hand-written immediate-mode GUI (layout, text/font atlas, widgets, screens). Owns the event loop, window and controller. |
| [`crates/rift/`](crates/rift/) | `rift` | **Native backend library.** Audio thread (rodio), playback queue and crossfade, persisted library, offline downloads, settings, native streaming, OS media keys / now-playing, Discord Rich Presence and the YouTube Music client wiring. |
| [`crates/rift-discord/`](crates/rift-discord/) | `rift-discord` | In-repo Discord Rich Presence IPC client: the subset of the local RPC protocol Rift uses (handshake plus `SET_ACTIVITY`; no OAuth, voice or chat RPC). |
| [`crates/rift-ytmusic/`](crates/rift-ytmusic/) | `rift-ytmusic` | Clean-room client for the anonymous YouTube Music innertube API (search, artist/album/playlist pages) plus the JS-less native audio-stream resolver. |
| [`crates/rift-types/`](crates/rift-types/) | `rift-types` | Shared data models (`Track`, `Library`, `QueueSnapshot`, …). |

The immediate-mode UI repaints every frame from application state: no retained
widget tree and no IPC boundary. The app drains backend events once per frame and
calls backend commands directly.

## Documentation

Developer documentation lives in [`docs/README.md`](docs/README.md):

- [`architecture.md`](docs/architecture.md) — module layout, render model, paint/UI contracts, windowing.
- [`backend-api.md`](docs/backend-api.md) — backend construction, command/event APIs, data directories.
- [`native-streaming.md`](docs/native-streaming.md) — native resolver/downloader, progress contract, client table and phase record.
- [`imgui-api.md`](docs/imgui-api.md) and [`ui-spec.md`](docs/ui-spec.md) — immediate-mode GUI API and the screen-by-screen visual spec.

## Contributing

Issues and pull requests are welcome. Please run `pwsh tools/ci-local.ps1`
(or the equivalent `cargo` steps above) before opening a PR; all new code must
pass `cargo fmt`, clippy with `-D warnings`, and the test suites.

## License

[Apache-2.0](LICENSE). Rift is an independent project and is not affiliated with
YouTube or Google.
