# Rift

[![CI](https://github.com/gage-lodba/Rift/actions/workflows/ci.yml/badge.svg)](https://github.com/gage-lodba/Rift/actions/workflows/ci.yml)

A minimal, ad-free desktop music player, written (almost) entirely in Rust.

![Preview](Previews/image.png)

- **Backend:** Tauri 2, tokio, rodio (playback), rift-ytmusic (metadata), serde, tracing
- **Frontend:** Yew (Rust → WASM), built with Trunk

No ads, no tracking, no account. Features:

- Search songs, artists, and albums; browse artist pages (top songs, full
  discography split into albums and singles) and album pages.
- Shuffle and repeat playback modes.
- Like songs, build and manage playlists, and resume your session on next launch.
- Download tracks for offline listening.
- Crossfade between tracks, OS media-key / now-playing integration, and Discord
  Rich Presence.
- Filters out music-video versions so results stay audio-only.
- Built-in yt-dlp detection (with one-click install) and an update checker.

## Download

Grab the installer for your platform from the
[latest release](https://github.com/gage-lodba/Rift/releases/latest) — `.AppImage`,
`.deb` or `.rpm` for Linux, `.dmg` for macOS, `.msi` or `.exe` for Windows.
Playback needs [yt-dlp](https://github.com/yt-dlp/yt-dlp) (see below); Rift
detects it automatically and can install it for you from **Settings** if it's
missing.

To build from source instead, read on.

## Requirements

Common to all platforms:

| Dependency | Why | Install |
|---|---|---|
| Rust + cargo | everything | [rustup.rs](https://rustup.rs), or `pacman -S rustup` / `winget install Rustlang.Rustup` |
| `wasm32-unknown-unknown` target | Yew frontend | `rustup target add wasm32-unknown-unknown` |
| [Trunk](https://trunkrs.dev) | WASM bundler | `cargo binstall trunk` (prebuilt) or `cargo install trunk` |
| [yt-dlp](https://github.com/yt-dlp/yt-dlp) | stream fetching (see below) | `pacman -S yt-dlp` / `winget install yt-dlp.yt-dlp`, or one-click from **Settings** |

Linux (Arch shown; use your distro's equivalents):

| Dependency | Why | Install |
|---|---|---|
| webkit2gtk-4.1 | Tauri webview | `pacman -S webkit2gtk-4.1` |
| ALSA | audio output | preinstalled on most systems |

Windows:

| Dependency | Why | Install |
|---|---|---|
| Visual Studio C++ Build Tools | MSVC linker for Rust | `winget install Microsoft.VisualStudio.2022.BuildTools` (select the "Desktop development with C++" workload) |
| WebView2 runtime | Tauri webview | preinstalled on Windows 10/11 |

Audio output on Windows uses WASAPI directly — nothing extra to install.

## Build & run

The frontend must be built first: the Tauri backend embeds `./dist` at compile time,
so `cargo run` picks up whatever the last `trunk build` produced.

```sh
# 1. build the frontend (outputs to ./dist)
cd ui && trunk build --release && cd ..

# 2. run the app
cd src-tauri && cargo run --release
```

The same commands work on Windows in PowerShell 7 or `cmd` (Windows
PowerShell 5 doesn't support `&&` — run the commands one at a time there).

For a distributable bundle, install `tauri-cli` (`cargo install tauri-cli`) and run
`cargo tauri build`.

## How streaming works (and why yt-dlp is needed)

Rift resolves its audio through [yt-dlp](https://github.com/yt-dlp/yt-dlp):
the subprocess fetches the m4a, which Rift reads back into memory and decodes
with rodio. yt-dlp is actively maintained against YouTube's changes and owns
the signature/`n` deobfuscation and proof-of-origin token handling that a
pure-Rust client would otherwise have to chase continuously — there is no
pure-Rust PO-token path to depend on today. The reasoning, with the evidence,
is written up in [`docs/innertube-research.md`](docs/innertube-research.md).

Search, metadata, artist/album pages, and playlist import are handled in Rust
by `crates/ytmusic` (`rift-ytmusic`), a clean-room client for the anonymous
YouTube Music innertube API. Everything else — UI, queue, playback, library —
also stays in Rust.

You can verify the streaming pipeline from the command line:

```sh
cd src-tauri && cargo run --example probe -- your search terms
```

The metadata client has its own parser tests against recorded responses:

```sh
cargo test -p rift-ytmusic
```

## Data

Your library (liked songs, playlists, recently played) is stored as JSON in
`~/.local/share/dev.jerimiah.rift/library.json` on Linux, or
`%APPDATA%\dev.jerimiah.rift\library.json` on Windows. Logs follow `RUST_LOG`
(default `rift=debug,rift_music=debug`).

## Discord Rich Presence

Rift advertises the currently playing track as a "Listening to Rift" status on
Discord. This uses a Discord application ID baked into the build. **Forks** need
their own Discord application: create one at the
[Discord Developer Portal](https://discord.com/developers/applications), then
replace `APP_ID` in `src-tauri/src/discord.rs` with your application's ID.

## License

Apache-2.0 — see [LICENSE](LICENSE).
