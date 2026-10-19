# Rift

[![CI](https://github.com/gage-lodba/Rift/actions/workflows/ci.yml/badge.svg)](https://github.com/gage-lodba/Rift/actions/workflows/ci.yml)
[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)
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
- Download tracks for offline listening.
- Crossfade between tracks, OS media-key / now-playing integration, and Discord
  Rich Presence.
- Filters out music-video versions so results stay audio-only.
- Built-in yt-dlp detection with one-click install, plus an update checker.

## Download

Prebuilt binaries are attached to the
[latest GitHub release](https://github.com/gage-lodba/Rift/releases/latest):
Windows (`rift-music-windows-x86_64.exe`), macOS (`rift-music-macos-universal`)
and Linux (`rift-music-linux-x86_64`). The app needs a Vulkan-capable GPU driver
and [yt-dlp](https://github.com/yt-dlp/yt-dlp) for streaming.

## Requirements

| Platform | Requirements |
|---|---|
| **Windows** | Windows 10/11, an up-to-date GPU driver with Vulkan, and the [Visual Studio C++ Build Tools](https://visualstudio.microsoft.com/visual-cpp-build-tools/) when building from source |
| **macOS** | macOS 13+, an up-to-date GPU driver, and Xcode Command Line Tools when building from source |
| **Linux** | A Vulkan driver, ALSA and the X11/Wayland/xkbcommon development headers |

Also required: **Rust 1.88** (pinned by [`rust-toolchain.toml`](rust-toolchain.toml))
and **yt-dlp** (`winget install yt-dlp.yt-dlp`, `brew install yt-dlp`,
`pacman -S yt-dlp`, or install it from **Settings → yt-dlp** inside the app).

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
cargo test -p rift -p rift-types -p rift-ytmusic -p rift-desktop
cargo clippy -p rift -p rift-types -p rift-ytmusic -p rift-desktop --all-targets -- -D warnings
cargo fmt --all -- --check
```

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
| [`crates/rift/`](crates/rift/) | `rift` | **Native backend library.** Audio thread (rodio), playback queue and crossfade, persisted library, offline downloads, settings, yt-dlp resolution and install, OS media keys / now-playing, Discord Rich Presence and the YouTube Music client wiring. |
| [`crates/rift-ytmusic/`](crates/rift-ytmusic/) | `rift-ytmusic` | Clean-room client for the anonymous YouTube Music innertube API (search, artist/album/playlist pages, stream metadata). |
| [`crates/rift-types/`](crates/rift-types/) | `rift-types` | Shared data models (`Track`, `Library`, `QueueSnapshot`, …). |

The immediate-mode UI repaints every frame from application state: no retained
widget tree and no IPC boundary. The app drains backend events once per frame and
calls backend commands directly.

## Documentation

Developer documentation lives in [`docs/vulkan-migration/README.md`](docs/vulkan-migration/README.md):

- [`architecture.md`](docs/vulkan-migration/architecture.md) — module ownership, render model, paint/UI contracts, windowing.
- [`backend-api.md`](docs/vulkan-migration/backend-api.md) — backend construction, command/event APIs, data directories.
- [`imgui-api.md`](docs/vulkan-migration/imgui-api.md) and [`ui-spec.md`](docs/vulkan-migration/ui-spec.md) — immediate-mode GUI API and the screen-by-screen visual spec.
- [`vulkano-recipe.md`](docs/vulkan-migration/vulkano-recipe.md) — practical Vulkano 0.35 renderer notes.

## Contributing

Issues and pull requests are welcome. Please run `pwsh tools/ci-local.ps1`
(or the equivalent `cargo` steps above) before opening a PR; all new code must
pass `cargo fmt`, clippy with `-D warnings`, and the test suites.

## License

[Apache-2.0](LICENSE). Rift is an independent project and is not affiliated with
YouTube or Google.
