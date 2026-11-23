# agents.md — Rift repository guide for AI agents

Rift is a native desktop music player for YouTube Music, written entirely in Rust and
rendered directly with Vulkan. This file is the entry point for any agent (AI or human)
joining the repository with zero context: it explains what the project is, how it is put
together, how to build and verify a change, where the authoritative contracts live, and
which mistakes to avoid.

> **Keep this file true.** It is part of the repository. If your change makes a path,
> command, contract or gate step described here wrong, update this file in the same
> change. When this file and a deeper document disagree, the deeper document wins —
> then fix this file.

---

## 0. How to use this file

### 0.1 Find your section

| You are about to… | Read |
|---|---|
| Understand the product and its hard constraints | §1 |
| Build, run, or see the app | §2 |
| Find where a feature lives | §3 |
| Understand the process/thread/event model | §4 |
| Change the backend library (audio, player, library, downloads, settings, events) | §5.1, §5.3, then `docs/backend-api.md` |
| Touch search/browse parsing or audio-stream resolution/download | §5.2, then `docs/native-streaming.md` |
| Change the app shell, controller, window, preview or screenshots | §5.4, then `docs/controller-notes.md` / `docs/native-platform.md` |
| Change the UI framework, text stack, renderer or screens | §5.5, then `docs/imgui-api.md` / `docs/ui-spec.md` |
| Know the build/feature/env-var matrix | §6 |
| Know how tests are organised and how to run one | §7 |
| Know what "done" means and what the gate runs | §8 |
| Follow the day-to-day conventions | §9 |
| Avoid breaking a fragile invariant | §10 |
| Follow an existing recipe ("add a command", "add a screen", …) | §11 |
| Debug a failure, read logs, diagnose GPU/audio/network issues | §12 |
| Know what changed recently, or what work is recorded as open | §13 |
| Decode an unfamiliar term | §14 |
| Go deeper than this file | §15 |

### 0.2 Operating rules for agents in this repository

1. **Read before you write.** Read this file, then the authoritative doc for your area
   (§15), then the module and its nearest tests you are about to change.
2. **One change, one owner.** In a multi-agent session keep write scopes disjoint; do not
   reformat or "fix" files outside your task. The Lead owns the final diff and the gate.
3. **Do not commit, stage, push, tag or publish** unless the user explicitly asks. Do not
   create branches, rewrite history, or open PRs on your own.
4. **Never touch the user's real data.** App data lives in an OS data directory; tests
   must redirect it with `RIFT_DATA_DIR` to a throwaway directory (§7.5). Do not delete
   `<data dir>/bin/yt-dlp*` or any other pre-existing user file — they are ignored, not
   garbage.
5. **Verify with the project's own tools**, not ad-hoc scripts: the commands in §7/§8 are
   the contract. Do not weaken a gate (no `#[allow]` sprinkles, no deleted baselines, no
   `-Skip*` reported as a pass).
6. **Keep docs in lockstep.** `docs/*.md` are contracts. A contract change without its
   doc update is an incomplete change.
7. **Use `artifacts/` for scratch** (screenshots, probe logs, gate logs). It is
   git-ignored and exists exactly for this; never commit it.
8. **Do not invent dependencies or fallbacks.** No webview/JS/WASM, no external media
   binary, no second generalist/metadata HTTP client (the backend owns one dedicated image
   client, §5.3).
9. **Prefer evidence.** If you claim behavior, point at a file:line, a test name, or a
   command you ran. Match the repository's comment style: explain *why*.

---

## 1. What Rift is

### 1.1 Product

Rift is an ad-free, minimal desktop music player for YouTube Music. Shipped features
(user-facing summary in `README.md`):

- Search songs/artists/albums; artist pages (top songs + full discography split into
  albums/singles) and album pages.
- Shuffle and repeat playback, crossfade between tracks.
- Like songs, build and manage playlists, resume your session on next launch.
- Download tracks for offline listening with a determinate per-track progress bar.
- **Native audio streaming**: a clean-room, pure-Rust, JS-less innertube resolver plus a
  ranged downloader — no external media binary (`docs/native-streaming.md`).
- OS media-key / now-playing integration (Windows SMTC, Linux MPRIS, macOS
  MPNowPlayingInfoCenter) and Discord Rich Presence.
- Music-video results filtered out; built-in update checker (GitHub releases).
- Prebuilt binaries for Windows, macOS (universal), Linux; needs a Vulkan-capable GPU
  driver.

Workspace version **0.6.1**, license **Apache-2.0**, repository
`https://github.com/gage-lodba/Rift`, author Gage Lodba. It is an independent project,
not affiliated with YouTube/Google.

### 1.2 Technical identity — hard constraints, not preferences

1. **No webview, no browser engine, no JavaScript, no WASM.** Every pixel comes from the
   hand-written immediate-mode UI (`crates/rift-desktop/src/ui`) rasterised by the
   hand-written Vulkano renderer (`crates/rift-desktop/src/gfx`). The former webview
   shell was deleted in the native rewrite; the local acceptance gate (not CI) has
   dependency/manifest guards that fail if it comes back, and the retired framework's
   brand name is deliberately assembled from fragments in those guards — do not
   reintroduce it in project text either.
2. **No external media binary, no yt-dlp, no fallback path.** Audio is resolved by
   `rift-ytmusic::player`, streamed by `rift::fetch` (which wraps the crate-private
   `stream` downloader), decoded by rodio.
   There is no runtime switch, no installer, no detection surface.
3. **Clean room.** The innertube client and resolver were derived from observed traffic
   and public JS-less request contracts, not from copied GPL implementation code
   (`docs/native-streaming.md` §5). Never paste code from yt-dlp or similar projects.
4. **Immediate mode.** No retained widget tree, no DOM, no IPC boundary between UI and
   backend: each frame lays out from scratch, screens emit `Action`s, a controller calls
   `BackendHandle` methods directly and drains one `BackendEvent` channel per frame.
5. **One `cargo build`, one binary.** The only executable is `rift-music` (package
   `rift-desktop`). No bundler, packaging system, Node/npm, or `.cargo/config.toml`
   runner aliases. The one exception is the optional, source-tree Linux desktop
   launcher installer (`tools/install-linux-desktop.sh`): it only copies an
   already-built binary plus a `.desktop` entry/icons into XDG directories, is not
   part of the build, and is not shipped in release assets.
   `crates/rift`, `rift-types`, `rift-ytmusic`, `rift-discord` are libraries.
6. **The application is testable without a window.** `rift-desktop` has a library target
   so UI/renderer/controller logic can be tested headlessly (`App::frame` with synthetic
   `InputState`).

### 1.3 Deliberate non-goals / things that do not exist here

Do not spend time looking for these; their absence is intentional:

- **No user accounts** — the innertube client is anonymous (visitor data only); Rift
  never logs in, stores credentials, or supports library sync.
- **No video**, HLS/SABR delivery, JS signature/`n` solving, PO-token/BotGuard
  generation (`docs/native-streaming.md` §1).
- **No Discord OAuth/voice/chat** — Rich Presence only (`crates/rift-discord`).
- **No mobile/web/server build, no CLI/TUI, no plugin system, no i18n framework.**
- **No installers/packages** — releases are bare executables. (The optional Linux
  desktop-entry installer in the source tree is not a packaged release artifact; it
  just copies a built binary and the menu entry into the user's XDG directories.)
- **No telemetry/analytics.** Network traffic is YouTube (metadata/audio) plus the
  GitHub releases API (update check) and Discord's local IPC socket.

---

## 2. Quick start

### 2.1 Requirements

| Platform | Build/run requirements |
|---|---|
| **Windows 10/11** | Visual Studio C++ Build Tools (MSVC linker, Windows SDK `rc.exe` for the icon); an up-to-date GPU driver with Vulkan to run. A missing Vulkan ICD is reported by a native startup-error dialog, not a silent GUI-subsystem exit |
| **macOS 13+** | Xcode Command Line Tools to build; a Vulkan-capable GPU driver to run. Rift does not bundle a Vulkan loader/MoltenVK — install one if the system lacks it |
| **Linux** | Vulkan driver, ALSA, X11/Wayland/xkbcommon development headers to build; a Vulkan driver + audio device to run (a PipeWire/PulseAudio sound server is preferred; raw ALSA is the fallback) |

Also required: **Rust 1.88** — pinned by `rust-toolchain.toml`, so `rustup` selects it
automatically. Build at the pinned toolchain, not "latest stable"; CI and the local gate
build at exactly 1.88.0 and lint against that floor (§6.2).

Linux build dependencies (README; CI installs the same):

```sh
# CI's exact native build dependencies on Linux (the README list matches):
sudo apt-get install -y --no-install-recommends pkg-config \
  libasound2-dev libvulkan-dev libxkbcommon-dev libwayland-dev \
  libx11-dev libx11-xcb-dev libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev
```

Windows gate tooling: PowerShell 7 (`pwsh`) and Git for Windows (Git Bash) for the shell
guard scripts. `cargo-deny` is optional locally (the gate SKIPs it when absent; CI
always runs it).

### 2.2 Build & run

```sh
cargo run -p rift-desktop              # debug build, live backend (the app)
cargo build --release -p rift-desktop  # release binary: target/release/rift-music[.exe]
```

The window is 1160x740 logical px (minimum 940x620), undecorated with Rift's own 46 px
titlebar and window controls. On Windows the window stays hidden until the first frame is
presented (avoids the white flash); other platforms show it immediately.

For the fastest dev launch, run the built binary directly (`target/debug/rift-music`)
instead of `cargo run`: Cargo's freshness check plus loading the much larger debug
executable costs several hundred ms before `main`, while the release executable is cheap
to load. The startup timeline is logged and explained in
`docs/architecture.md#startup-path-and-timing`.

**The app needs a real Vulkan device to open a window.** For UI work without one, use
preview/screenshot mode (§6.3): it renders through headless Vulkan (still needs a Vulkan
driver, but no window manager) or runs with placeholder data and no backend.

### 2.3 Commands you will actually use

```sh
# the CI package list is deliberate — copy it rather than using --workspace
cargo fmt --all -- --check
cargo clippy -p rift -p rift-types -p rift-ytmusic -p rift-discord -p rift-desktop --all-targets -- -D warnings
# the gate also lints the configurations the debug/default run cannot see:
cargo clippy -p rift-desktop --no-default-features --all-targets -- -D warnings
cargo clippy --release -p rift-desktop --all-targets -- -D warnings
cargo clippy --release -p rift-desktop --features backend,preview --all-targets -- -D warnings
cargo test -p rift -p rift-types -p rift-ytmusic -p rift-discord -p rift-desktop
cargo test -p rift-desktop --no-default-features     # UI-only (inert backend) configuration
$env:RUSTDOCFLAGS = "-D warnings"; cargo doc -p rift -p rift-types -p rift-ytmusic -p rift-discord --no-deps   # PowerShell
# (POSIX sh equivalent: RUSTDOCFLAGS="-D warnings" cargo doc -p rift -p rift-types -p rift-ytmusic -p rift-discord --no-deps)

# full local acceptance gate (mirrors ci.yml; §8). Writes a log under artifacts/
./tools/ci-local.sh                     # Linux/macOS (bash)
pwsh tools/ci-local.ps1                 # Windows
./tools/ci-local.sh --skip-tests        # fast iteration (GATE: INCOMPLETE unless --allow-incomplete)
pwsh tools/ci-local.ps1 -SkipTests
./tools/ci-local.sh --skip-release      # skip release build + clippy + icon + screenshot (also incomplete)
pwsh tools/ci-local.ps1 -SkipRelease
```

Narrower runs while developing:

```sh
cargo test -p rift-ytmusic                  # innertube parsers/client, fixtures only (offline)
cargo test -p rift player                   # backend; substring filter on test names
cargo test -p rift-desktop --lib            # UI/renderer/state logic, no window
cargo test -p rift-desktop --test ui_flow   # headless end-to-end interaction flows
cargo check --workspace
```

Tests that need the network or an audio device are `#[ignore]`d and are never part of the
default suite (§7.3/§7.4). Add `--locked` when you want to fail exactly like CI does.

### 2.4 Ten-minute orientation path

1. Read §1 and §4 of this file.
2. Skim `docs/README.md` (the documentation index) and `docs/architecture.md`.
3. `cargo build -p rift-desktop`; either run the app or capture the home screen with
   preview mode (§6.3).
4. Locate your task's area in §3 and open its authoritative doc (§5.x / §15).
5. Read the module you are about to change **and its tests** — this repo encodes intent in
   tests and long explanatory comments.
6. Decide your verification before writing code: which test target, which gate step, which
   doc must change in the same diff.
7. Finish with the narrow commands for your area, then the full gate when practical (§8).

---

## 3. Repository map

### 3.1 Top level

```
Cargo.toml                 workspace root: 5 members, shared dependency versions,
                           workspace lint policy, dev/release profiles
Cargo.lock                 committed lockfile; every gate/CI cargo command uses --locked
rust-toolchain.toml        pins Rust 1.88.0 (+ rustfmt, clippy)
deny.toml                  cargo-deny supply-chain policy: advisories/licenses/bans/sources
LICENSE                    Apache-2.0
README.md                  user-facing overview, requirements, build/run/dev commands
agents.md                  ← this file
crates/                    the five workspace crates (3.2)
docs/                      authoritative developer documentation (3.3, §15)
tools/                     local acceptance gate + screenshot/compare scripts
.github/                   CI/release workflows, guard scripts, dependabot
Previews/image.png         canonical home-screen render at 1160x740
artifacts/                 git-ignored scratch: screenshots, probe output, gate logs
target*/                   build output, git-ignored; don't commit
```

Git conventions that matter when you look around: the tree is LF-normalized
(`.gitattributes`), the default branch is `main`, and `.gitignore` excludes
`target/`, `target-*`, `/artifacts/`, `/release-assets/`, `*.log`, `/.probe/`,
`/scratch/`, `__pycache__/` and `/CLAUDE.md`. A tracked agent-instructions file must
therefore not be named `CLAUDE.md`; `agents.md` is not ignored.

### 3.2 The crates

Dependency direction: `rift-desktop` → `rift` → {`rift-types`, `rift-ytmusic`,
`rift-discord`}; `rift-ytmusic` → `rift-types`; `rift-desktop` → `rift-types`.

| Crate (path) | Type | What it owns |
|---|---|---|
| [`crates/rift-desktop`](crates/rift-desktop) | binary + lib | **The application.** winit event loop, Vulkano renderer, hand-written immediate-mode GUI (layout, text/font atlas, widgets, screens), app state, controller, preview/screenshot mode, window-state and single-instance handling. Only binary: `rift-music`; lib target: `rift_desktop`. |
| [`crates/rift`](crates/rift) | lib | **Native backend library.** Audio thread (rodio), playback queue/crossfade, persisted library, offline downloads, settings, native streaming, OS media keys/now-playing, Discord presence, innertube wiring, and the `RiftBackend` / `BackendEvent` API. `#![forbid(unsafe_code)]`, `#![warn(missing_docs)]`. |
| [`crates/rift-ytmusic`](crates/rift-ytmusic) | lib | Clean-room anonymous YouTube Music innertube client (search, artist/album/playlist browse, credits) plus the JS-less audio-stream resolver. Fixture-based offline tests + loopback transport tests. |
| [`crates/rift-discord`](crates/rift-discord) | lib | In-repo Discord Rich Presence IPC client: version-1 handshake + `SET_ACTIVITY`/clear only. No OAuth/voice/chat and no external Discord/RPC SDK (only `serde`, `serde_json`, `tracing`). |
| [`crates/rift-types`](crates/rift-types) | lib | Shared serde models: `Track`, `Library`, `Playlist`, `QueueSnapshot`, `PlaybackState`, `Progress`, `Bootstrap`, `DownloadState`, `UpdateStatus`, … |

### 3.3 "Where do I find …?"

| Topic | Primary locations |
|---|---|
| Audio output/decode/seek/volume/crossfade | `crates/rift/src/audio.rs`, `crates/rift/src/player/**` |
| Playback queue, shuffle/repeat, play-next | `crates/rift/src/player/{state,queue,nav,playback,events}.rs` |
| Backend construction, command handlers, event bus | `crates/rift/src/backend.rs`, `crates/rift/src/commands/**`, `crates/rift/src/event.rs` |
| Library/likes/playlists, data dir, settings | `crates/rift/src/{library,paths,settings}.rs` |
| Offline downloads + progress | `crates/rift/src/downloads.rs`, `crates/rift/src/commands/downloads.rs` |
| HTTP fetch, update check, image fetch | `crates/rift/src/fetch.rs`, `crates/rift/src/commands/{updates,images,connectivity}.rs` |
| innertube search/browse requests + parsing | `crates/rift-ytmusic/src/{client,context,identity,params,parse}.rs` |
| Audio-stream resolution (client table/chain) | `crates/rift-ytmusic/src/player.rs`, `crates/rift/src/stream.rs` |
| OS media keys / now-playing | `crates/rift/src/media.rs` |
| Discord Rich Presence | `crates/rift/src/discord.rs`, `crates/rift-discord/src/**` |
| UI framework (widgets, layout, input, ids, popups) | `crates/rift-desktop/src/ui/**`, `docs/imgui-api.md` |
| Text/font atlas/icons | `crates/rift-desktop/src/text/**`, `crates/rift-desktop/src/ui/icons.rs` |
| Vulkan renderer, paint contract, shaders | `crates/rift-desktop/src/gfx/**`, `crates/rift-desktop/shaders/**` |
| Screens and their visual spec | `crates/rift-desktop/src/screens/**`, `crates/rift-desktop/src/theme.rs`, `docs/ui-spec.md` |
| App event loop, per-frame composition | `crates/rift-desktop/src/{main,app}.rs`, `docs/architecture.md` |
| UI→backend bridge, action mapping, cover pipeline | `crates/rift-desktop/src/controller/**`, `crates/rift-desktop/src/cover.rs`, `docs/controller-notes.md` |
| UI state, `View`/`Action` vocabulary | `crates/rift-desktop/src/state.rs` |
| Window-state persistence, single-instance guard, Linux desktop launcher | `crates/rift-desktop/src/{window_state,single_instance}.rs`, `crates/rift-desktop/assets/linux/Rift.desktop`, `tools/install-linux-desktop.sh`, `docs/native-platform.md` |
| Preview data and screenshot capture | `crates/rift-desktop/src/preview.rs`, `crates/rift-desktop/src/main.rs` (`--screenshot`), `tools/screenshot.ps1` |
| Build/CI/release and dependency policy | `Cargo.toml`, `rust-toolchain.toml`, `deny.toml`, `.github/`, `tools/` |
| Local acceptance gate | `tools/ci-local.sh` (Linux/macOS), `tools/ci-local.ps1` (Windows), `tools/{screenshot,window_shot,verify}.ps1`, `tools/compare_images.py` |

---

## 4. Architecture in one page

### 4.1 Process and threads

One process. The **UI thread** owns the winit window, the Vulkano renderer and the
immediate-mode `Ui`; it repaints every frame and must never block on disk, network, audio,
image decode or a future. The **backend** (`RiftBackend`) is constructed once by the app
and owns all other work. Threads (fixed names help in logs/profilers; details §5.3):

| Thread | Runs |
|---|---|
| app/UI thread | backend construction, `bootstrap()`, sync commands, per-frame event drain, layout/paint/present |
| backend Tokio runtime, 2 workers (`rift-backend`) | async commands (search/browse/playback fetch/download jobs/update check), event pump, `spawn_blocking` pool |
| `rift-audio` | rodio output device (Linux: prefers the session's sound server — cpal's PulseAudio host, PipeWire via `pipewire-pulse` — raw ALSA fallback; logs `audio output host:`), in-memory decode, seek/volume/crossfade, progress ticker (~4 Hz) |
| `rift-media` | souvlaki OS media keys/now-playing |
| `rift-discord` | Discord Rich Presence IPC loop |
| `rift-persist` | best-effort `playback.json` writes |
| `rift-durable` | coalesced atomic `library.json`/`settings.json` writes + shutdown flush |

### 4.2 Crate graph

```
rift-desktop (bin rift-music + lib rift_desktop)
  ├── rift (backend library)
  │     ├── rift-types   (shared models)
  │     ├── rift-ytmusic (innertube client + stream resolver) ── rift-types
  │     └── rift-discord (Rich Presence IPC)
  ├── rift-types
  └── winit / vulkano / fontdue / image / arboard / tokio
```

There is no IPC boundary between UI and backend: the controller calls `BackendHandle`
methods directly and drains a `BackendEvents` channel once per frame. That is why the
architecture has only two layers to reason about.

### 4.3 One frame of the app (UI path)

1. `main.rs` translates winit events into `ui::InputState` (pointer, buttons, wheel,
   keys/text, modifiers, viewport, scale factor, time/delta).
2. `App::tick()` drains backend/controller events into `State`, expires the toast, and
   forwards queued cover requests.
3. `app.rs` calls `ui.begin_frame(input, viewport)` and runs `screens::root`, which lays
   the whole UI out from scratch and pushes `Action`s (navigate, play, like, open menu…).
4. `ui.end_frame()` returns a `gfx::paint::PaintList` (ordered layers of primitives plus
   texture create/update/drop notifications).
5. `gfx::Renderer::frame(&paint)` uploads textures, records one command buffer, draws the
   swapchain image, presents.
6. **After** `end_frame`, `app.rs` dispatches the collected `Action`s. UI-local actions
   update `State`/queue `WindowCommand`s; backend-facing ones go through the controller.
   Results arrive as `ControllerEvent`s on a later frame, so state changes are visible
   next frame and each frame's paint list is internally consistent.

The frozen seam between the two halves is `crates/rift-desktop/src/gfx/paint.rs`:
**UI produces `PaintList`, renderer consumes `PaintList`.** Subsystem details:
`docs/architecture.md` ("Rendering model", "Paint contract", "Renderer API",
"Vulkan practices").

### 4.4 One backend command (backend path)

`Action` → `Controller::dispatch` → `BackendCommands` (production impl over
`BackendHandle`) → sync method or a future spawned on the backend runtime → worker
(player/audio/downloads/network) → state mutation + event → `BackendEvent` on the bus →
drained by `Controller::poll()` next frame → `ControllerEvent` → `State` → next frame
renders it. The full mapping is `docs/controller-notes.md` §4 (Action→command) and §9
(BackendEvent→ControllerEvent); the backend side is `docs/backend-api.md` §5–§7.

### 4.5 State ownership rules

- Screens receive `&mut State` for UI-local/transient state but **never call the backend**
  and never mutate backend mirrors directly; intent crosses the boundary only as `Action`.
- Cross-frame UI state (scroll offsets, text-input contents, focus, popup identity, cover
  texture cache) lives in `State` / `Ui` memory keyed by **stable ids**, not in widgets.
- Anything that survives a restart belongs to the backend data directory, not the UI
  crate (`docs/backend-api.md` §4; see §5.3, "Persistence and the data directory").
- The backend never touches the window/renderer except for the optional Windows
  `media_hwnd` (SMTC needs the HWND); that is its only window coupling.

---

## 5. Crate guides

Each subsection is orientation: the module map, the contracts you must respect, and the
traps. The authoritative doc named at the end of each subsection wins on details.

### 5.1 Foundations: `rift-types` and `rift-discord`

#### `crates/rift-types` — shared models (`crates/rift-types/src/lib.rs`)

One file, one dependency (`serde`). It defines the types that cross every crate boundary:
`Track` (+ `same_song`, `fill_missing_from`, `title_key`, `SongIndex`), `ArtistRef`,
`ArtistSummary`/`ArtistPage`, `AlbumSummary`/`AlbumPage`, `Playlist`, `Library`,
`QueueSnapshot`, `RepeatMode`, `PlaybackState`, `Progress`, `Bootstrap`, `DownloadState`,
`UpdateStatus`, and their helpers.

Rules:

- **Additive contract.** These types are serialized into user files (`library.json`,
  `settings.json`, `playback.json`) and into the event bus. Make additions
  backward-compatible; when reading old/partial data prefer `#[serde(default)]`-style
  tolerance over rejecting the file. `DownloadState` keeps `progress` as integer permille
  (0..=1000) so it can stay `Eq`; don't change that to `f32`.
- **`Track` carries no innertube video type** (that information is lost when a track comes
  from Rift's own JSON). Audio-only filtering is therefore split: typed filtering lives in
  `rift-ytmusic/src/parse.rs`, and the weaker title-marker fallback in
  `rift/src/commands/convert.rs`. Don't collapse the two rules.
- **`Track::same_song` is user-visible semantics**, not a utility: likes and duplicate
  repair match a stored song by song identity (title/artist/duration keying via
  `title_key`/`SongIndex`), not by video id — YouTube serves the same recording under
  multiple ids. (It returns true immediately for equal non-empty video ids; the
  title/artist/duration rule applies only when the ids differ, and it includes album
  evidence and a duration tolerance.) Changing the match rule changes what users see.
- Tests: `cargo test -p rift-types` (unit tests cover the `same_song`/`SongIndex`/
  `fill_missing_from` matrix).

#### `crates/rift-discord` — Rich Presence IPC (`crates/rift-discord/src/**`)

The subset of Discord's local RPC protocol Rift needs: version-1 handshake, then
`SET_ACTIVITY` and clear. No OAuth, voice, or chat.

- Transport is a local named pipe (Windows) / Unix socket; frames are
  `opcode: u32 LE` + `length: u32 LE` + UTF-8 JSON. On Unix the socket is opened with a
  250 ms read/write timeout (`SOCKET_IO_TIMEOUT`) so the blocking worker retires on a
  stall; the Windows named-pipe `File` has no std timeout (see §5.3 for the bounded
  fallback in the backend).
- The client answers `PING` with `PONG`, skips `PONG`/unknown opcodes and frames carrying
  another command's nonce, matches replies by nonce, and reports `CLOSE` codes cleanly.
- The **worker** (pacing, heartbeat, restart/replay) lives in `crates/rift/src/discord.rs`
  — see §5.3, "OS media keys and Discord presence". This crate is only the transport; don't add presence policy here.
- Tests: unit tests; one `#[ignore]`d live test needs a real Discord client and an app id:
  `$env:RIFT_DISCORD_APP_ID = "<application id>"; cargo test -p rift-discord -- --ignored`.

### 5.2 `rift-ytmusic` + the native streaming pipeline

The player's network identity: **no webview, no JavaScript, no external media binary**.
Two independent HTTP clients exist by design:

- **Metadata** — `rift_ytmusic::Client`: anonymous `WEB_REMIX` innertube calls to
  `https://music.youtube.com/youtubei/v1/{search,browse,next}` for search, artist/album
  pages, playlist import and credits.
- **Audio** — `rift_ytmusic::player::resolve`: a JS-less `VISIONOS`
  `POST https://www.youtube.com/youtubei/v1/player` returning a direct signed
  `googlevideo.com` URL (no `signatureCipher`, no `n` parameter, no PO token), followed by
  a **ranged** HTTP GET (the crate-private `rift::stream` downloader) shared by playback
  and offline downloads.

```
search/browse -> rift_ytmusic::Client -> rift_types::{Track, AlbumPage, ArtistPage, Playlist}
video id -> player::resolve -> AudioStream{url, itag=140, ...} -> rift::stream
            ├── fetch_bytes   (playback: bytes in memory -> rodio decoder)
            └── fetch_to_path (offline: caller's .part -> rename -> <safe_stem>.m4a)
```

```rust
// crates/rift-ytmusic/src/lib.rs (public surface, simplified)
pub use client::Client;
pub use error::{Error, Result};
pub use params::SearchFilter;
pub use player::AudioStream;
// resolver entry point: rift_ytmusic::player::resolve
```

#### Ownership map

| Path | Owns |
|---|---|
| `crates/rift-ytmusic/src/client.rs` | transport (`Client::post`), typed endpoints, response-size cap, 403 rotation, loopback transport tests |
| `crates/rift-ytmusic/src/context.rs` | `WEB_REMIX` client constants, context object, client-version scrape/cache |
| `crates/rift-ytmusic/src/identity.rs` | visitor-data pool, home-page scrape, ban/eviction, test seeding/gate |
| `crates/rift-ytmusic/src/params.rs` | `SearchFilter` → protobuf `params` constants (protocol constants, don't edit casually) |
| `crates/rift-ytmusic/src/parse.rs` | tolerant JSON → `rift_types` parsers (`tracks`, `artists`, `albums`, `album_page`, `artist_page`, `playlist_page`, `track_credits`) |
| `crates/rift-ytmusic/src/player.rs` | JS-less resolver: `ClientConfig` table, `CHAIN`, `AudioStream`, `resolve` |
| `crates/rift-ytmusic/src/error.rs` | `Error` (`#[non_exhaustive]`) |
| `crates/rift-ytmusic/tests/fixtures.rs` + `tests/fixtures/*.json` | offline parser tests against sanitized recorded responses |
| `crates/rift-ytmusic/examples/*.rs` | manual network probes + fixture tooling (never CI) |
| `crates/rift/src/stream.rs` | ranged downloader (`fetch_bytes`, `fetch_to_path`), URL trust, re-resolve on expiry |
| `crates/rift/src/fetch.rs` | `fetch_track` (offline-copy-first), `fetch_bytes`, `safe_stem` |
| `crates/rift/src/downloads.rs` | `Downloads` store: downloaded/downloading/failed sets + permille progress map |
| `crates/rift/src/commands/downloads.rs` | download worker: `.part` + `sync_all` + rename, throttled `DownloadProgress` events, kept-offline retries |

#### Metadata path (`WEB_REMIX`)

- `Client::new()` builds one `reqwest::Client` (Firefox UA, gzip+brotli, 5 s connect /
  30 s total timeouts), does **no** network I/O, and is cheap to clone. The client version
  and visitor-data pool are process-global caches, so all clients share one anonymous
  session.
- **Visitor data** is scraped from the `music.youtube.com` home page: a non-empty
  `__Secure-YEC` cookie, falling through to the page's `"visitorData":"…"` literal when
  the cookie is empty. An empty visitor id is rejected by YouTube with `LOGIN_REQUIRED`,
  so that fallback is load-bearing. A 403 calls `identity::ban(id)`, which marks and
  **evicts** the id so a retry cannot re-select it.
- Requests: `POST {HOME}/youtubei/v1/{endpoint}?prettyPrint=false` with the shared
  `{client: {clientName: "WEB_REMIX", …}, request, user}` context. Headers include
  `X-YouTube-Client-Name: 67`, `X-YouTube-Client-Version`, `X-Goog-EOM-Visitor-Id`, and
  `Cookie: SOCS=CAISAiAD`.
- `Client::post` makes at most **two** attempts: a 403 evicts the visitor and retries once
  with the next pool id; every other status is final. 403 is handled *before* body parsing
  (YouTube sometimes answers with an HTML block page). Responses are size-capped
  (`MAX_RESPONSE_BYTES` = 32 MiB release; enforced against both `Content-Length` and the
  streamed count because chunked/decompressed bodies declare no length).
- **Parsing** (`parse.rs`) is deliberately tolerant: skip unknown renderers, warn when
  rows were present but nothing parsed. Two rules to preserve: (a) typed UGC video rows
  (`musicVideoType == "MUSIC_VIDEO_TYPE_UGC"`) and "Video"/"Episode" badge rows are
  filtered out here; (b) `serde_json` is built **without** `preserve_order`, so object
  keys are alphabetical — **never choose a node with `find_all(...).first()`**; read a
  known path or match the requested id. Playlist pages carry `items_seen` (counts filtered
  rows too) so a fully-filtered page is not mistaken for the end; preserve every stop
  condition in the pagination loop.
- Credit backfill (`track_credits`, plus album/artist lookups in
  `crates/rift/src/commands/search.rs`) is bounded (`MAX_CREDIT_LOOKUPS = 40`, ≤8 in
  flight) and can take seconds for an artist page — that is expected, not a hang.

#### Stream resolution (`player.rs`)

- `player::resolve(video_id) -> Result<AudioStream>` sends one recorded player request per
  `ClientConfig` row in `CHAIN` order. `AudioStream { url, itag, mime_type,
  content_length, client }`; **the signed URL expires — never persist it**.
- The request is a `VISIONOS` client request to
  `POST https://www.youtube.com/youtubei/v1/player`, with `X-YouTube-Client-Name: 101`,
  `X-Goog-Visitor-Id` and a visitor from the shared pool. Bodies are capped at
  `MAX_PLAYER_BODY_BYTES = 8 MiB`.
- Failure classification: client-specific failures (`Parse`, non-403 `Api`, `Unavailable`,
  `NoAudioFormat`) fall through to the next row; `Http`, `Bootstrap`, `ResponseTooLarge`
  are terminal. One visitor-rotation budget per resolve; a second rejection errors (never
  loops).
- Format selection over `streamingData.adaptiveFormats` in response order: skip
  `signatureCipher`-only, missing/unparseable entries; preference `itag` 140 (AAC
  128 kbps) → 141 → 139 → any other `audio/mp4`. Only `audio/mp4` is directly
  playable (rodio cannot decode WebM/Opus, and downloads publish `.m4a`); a
  WebM/Opus-only response is `NoAudioFormat` with the rejected MIME logged.
- **Today `CHAIN = &[VISIONOS]`** — the single remaining structural risk (§13.3). A new
  client row ships only after a full ranged-GET proof (whole file **and** a non-zero
  offset both return `206` with the right byte count); `ANDROID_VR` is the cautionary
  example (direct URLs, but offsets past ~1 MiB are 403-gated). The test
  `the_shipped_chain_is_the_verified_visionos_client` fails on any new row until its
  contract is asserted.

#### Ranged downloader (`crates/rift/src/stream.rs`)

- `stream::resolve` wraps the resolver, validates the URL, and maps to
  `ResolvedStream { url, content_length, itag, mime_type }`. **Validate before requesting
  bytes**: HTTPS only, host `googlevideo.com` or `*.googlevideo.com`, no embedded
  credentials; redirects are followed at most twice and re-checked per hop
  (`validate_stream_url` / `allowed_stream_url`).
- The downloader uses **its own** HTTP client (5 s connect, 30 s *read* timeout; gzip/
  brotli disabled so byte offsets stay exact). A rolling **30 s / 1 KiB no-progress
  window** (bytes, not requests; `progress_window`) fails a drip that the per-read
  timeout cannot see. Never reuse the backend's JSON/cover client for audio.
- **Range requests are load-bearing.** A plain GET of a googlevideo URL is throttled to a
  few tens of KB/s; `Range` answers `206` at full speed (measured in
  `docs/native-streaming.md` §2.3). Every request is `Range: bytes=start-end` in
  `CHUNK_BYTES = 8 MiB` chunks.
- Transfer rules to preserve: a `206` must match the requested offset via `Content-Range`;
  a `200` in reply to a mid-download range is an error (never a silent restart); `403`/
  `410` means expiry → re-resolve **once** and resume only if the new stream is provably
  the same media (`itag`, MIME, matching length); completion requires a verified total;
  in-memory playback is capped at `MAX_MEMORY_BYTES = 256 MiB`.
- `fetch_to_path` writes the caller's `.part` path and **never renames** it — the download
  worker owns publication (a per-claim `<safe_stem>.<attempt>.m4a.part` →
  `<safe_stem>.m4a` on success, delete on failure).
  After the final verified write it `sync_all`s the file, so a crash cannot publish a
  non-zero but unsynced (truncated) `.m4a`; never publish a truncated or empty file.

#### Progress contract

`rift_types::DownloadState.progress: HashMap<String, u16>` is **permille (0..=1000)**,
keyed only by tracks currently downloading; absent means unknown (queued behind the active
transfer) and the UI keeps its indeterminate spinner. Every lifecycle transition clears the
entry; progress events are throttled (`PROGRESS_EMIT_INTERVAL = 100 ms` per active track;
first callback immediate). The UI clamps permille in `State::download_fraction`. Keep the
integer permille — it is what keeps `DownloadState: Eq` and the events cheap.

#### Error model

`rift_ytmusic::Error` (`#[non_exhaustive]`): `Http`, `Api { status, message }`, `Parse`,
`ResponseTooLarge { size }`, `Bootstrap` (no visitor identity), `Unavailable { reason }`
(video not playable: removed/private/age/region-gated), `NoAudioFormat` (no directly
downloadable audio; cipher-only responses land here). The backend wraps these in `anyhow`
with video-id/command context and emits **one** actionable `BackendEvent::Error` string.
**There is no fallback backend** that could mask the cause.

#### Tests vs live probes

Committed tests are **offline**: fixture parsing (`crates/rift-ytmusic/tests/fixtures.rs`)
and loopback transport tests that drive the shipped code against a `std::net::TcpListener`
on `127.0.0.1` (no mock-HTTP crate). Test-only seams are compile-time gated
(`context::set_home_for_test`, `player::PlayerBaseOverride`); a release binary contains
neither and cannot be redirected via environment variables. Transport tests share
process-global state (seeded visitor pool + `identity::pool_gate()`), so hold the gate for
the whole test or thread scheduling can change results.

Network-requiring commands (manual, never CI; never commit the URLs/visitors they see):

```sh
cargo test -p rift-ytmusic                                  # offline suite
cargo run -q -p rift-ytmusic --example resolve_stream       # 20-id coverage sample
cargo run -p rift-ytmusic --example resolve_stream -- <video-id> [<video-id>…]
cargo run -p rift --example native_probe                    # playback fetch -> decode
cargo run -p rift --example download_probe                  # real worker: progress, restart replay
cargo run -p rift --example smoke -- "daft punk get lucky"  # search -> fetch -> decode
cargo test -p rift-desktop --features backend --test live_backend -- \
  --ignored --nocapture --test-threads=1 live_native_playback_download_progress_and_offline_restart
```

`resolve_stream` also proves the download path: the whole-file ranged GET must be
`206 audio/*` with a matching byte count, and for >1 MiB streams a mid-offset range must
return `206` too (the check that catches offset-gated clients). Fixture tooling:
`cargo run -p rift-ytmusic --example fixture_report` and
`cargo run -p rift-ytmusic --example prune_fixture -- <capture.json> <kind> <fixture.json>`
(kinds: `tracks`, `artists`, `albums`, `album`, `artist`, `playlist`, `next`) — it re-parses
the original and the compacted document and refuses to write unless they agree. Committed
fixtures must stay clean (no `googlevideo`, `signatureCipher`, `visitorData`
`X-Goog-Visitor-Id`; URLs are `https://example.invalid/…`) — a test asserts this.

#### Invariants, with the reasons

1. **No JS-dependent path.** No signature/`n` solving, no JS interpreter, no PO
   token/BotGuard, no HLS/SABR. Cipher-only formats are `NoAudioFormat`; the fix is a
   better client row, not a solver.
2. **Ranged GETs are mandatory**, and a mid-download `200` is corruption, not a resume.
3. **One visitor rotation per resolve / per `Client::post`.** Retry loops burn the pool
   and hammer the endpoint.
4. **Never cache a failed client-version scrape** — the fallback is per-call; caching it
   poisons every later request with a 400.
5. **Empty `__Secure-YEC` must fall through** to the page's `visitorData` literal.
6. **A new client row needs the full ranged-GET proof**, not just "it resolves".
7. **Never `find_all(...).first()`** for a layout decision (keys are alphabetical).
8. **A zero-parsed playlist page may not be empty**; use `items_seen` and preserve every
   pagination stop condition.
9. **Signed URLs expire** — never persist one; one identity-checked re-resolve on 403/410.
10. **Publish only complete, verified files** (`.part` + rename, non-empty, byte count ==
    verifiable total).
11. **No environment overrides for hosts** — test overrides are `cfg(test)` only.
12. **Progress is permille and only for the active track.**

### 5.3 `rift` — the backend library

The framework-agnostic backend: audio output/decode, player core (queue, shuffle, repeat,
crossfade), persisted library/settings, offline downloads, native streaming, OS media
keys/now-playing, Discord presence, image/connectivity/update helpers. It has no
windowing, webview or IPC layer. Three seams connect it to the app:

```rust
let (backend, mut events) = RiftBackend::new(BackendConfig { data_dir: None, media_hwnd })?;
let snapshot = backend.bootstrap();   // synchronous launch snapshot, before the first frame
let handle = backend.handle();        // Clone + Send + Sync handle for tasks
// per frame: drain `events`; call sync commands directly; spawn async ones
backend.shutdown();                   // consumes the owner, flushes durable writes
```

**`docs/backend-api.md` is the contract** — constructor, every method signature/error/
blocking note, event payloads, data-directory layout, legacy command mapping. Read it
before wiring the app; this subsection is orientation plus drift notes.

#### Module map

| Path | Owns / key items |
|---|---|
| `crates/rift/src/lib.rs` | public surface: `RiftBackend`, `BackendHandle`, `BackendConfig`, `BackendError`, `BackendEvent(s)`, `native_data_dir`, `APP_IDENTIFIER`, `DATA_DIR_ENV`, `MAX_CROSSFADE`, `MAX_IMAGE_BYTES`, `VERSION`, `preview_mode()`; `#![forbid(unsafe_code)]`, `#![warn(missing_docs)]` |
| `crates/rift/src/backend.rs` | `RiftBackend`, `BackendHandle`, `BackendConfig`, `BackendError`, private `BackendInner`; startup metadata repair (bounded: 20 lookups, stop after 3 consecutive failures) |
| `crates/rift/src/commands/{bootstrap,search,playback,queue,library,downloads,images,updates,connectivity}.rs` | inherent `BackendHandle` command methods; `commands/convert.rs` audio-only filter (`is_audio_track`) and `MAX_FETCH_TRACKS = 1000` |
| `crates/rift/src/event.rs` | `BackendEvent` (10 variants), `BackendEvents`, legacy `name()` table |
| `crates/rift/src/player/{state,queue,nav,playback,events}.rs` | `PlayerCore`/`PlayerShared`, pure queue mutations, shuffle/repeat/prev/next, play/fetch/crossfade/announce, audio bridge; `player/tests.rs` is the largest unit suite |
| `crates/rift/src/audio.rs` + `audio/output.rs` | `AudioCmd`/`AudioEvent`; rodio decode/seek/volume/crossfade. `run` is a thin device-open/dispatch/tick loop; the private `AudioState` in `output.rs` owns the player, fade tails and play identity, with one handler method per command. Logs the selected host as `audio output host:` (Linux: sound server first, raw ALSA fallback) |
| `crates/rift/src/{library,settings,downloads}.rs` | `LibraryStore` (`library.json`, recents cap 30), `SettingsStore` (`settings.json`, `MAX_CROSSFADE = 12.0`), `Downloads` (on-disk `.m4a` presence is truth) |
| `crates/rift/src/{stream,fetch}.rs` | native downloader internals + public `rift::fetch` (`fetch_track` offline-first, `fetch_bytes`, `safe_stem`) — see §5.2 |
| `crates/rift/src/{media,discord}.rs` | `MediaHandle` over the `rift-media` thread (souvlaki); `DiscordHandle` over the `rift-discord` worker |
| `crates/rift/src/{paths,util}.rs` | data-dir resolution (`APP_IDENTIFIER = "dev.jerimiah.rift"`, `DATA_DIR_ENV = "RIFT_DATA_DIR"`); `DurableWriter`, `Persister`, `load_json`, `atomic_write`, `LockExt`, `is_online` |
| `crates/rift/examples/**` | manual live probes (binaries, not tests): `probe`, `smoke`, `download_probe`, `native_probe`, `discord_replay_probe` |

#### Construction, ownership, shutdown

- Construct **once, on the app/UI thread, before the event loop**, and never from inside a
  Tokio runtime (it builds and owns its own). Startup performs **no network I/O**: a
  missing network or audio device degrades later via `BackendEvent::Error`, not a
  constructor failure.
- `RiftBackend` is **not `Clone`** and owns the Tokio runtime (2 workers named
  `rift-backend`). `Deref<Target = BackendHandle>` lets `backend.search(..)` work directly;
  clone `BackendHandle`s into tasks so the runtime is dropped on the owning thread.
- `BackendConfig { data_dir, media_hwnd }`: `data_dir: None` → `RIFT_DATA_DIR` if set →
  platform default. `media_hwnd` is the winit `HWND` on Windows; SMTC panics without one,
  so the backend degrades to no OS media keys/now-playing on Windows when it is `None`
  (logged). Ignored on Linux/macOS.
- `BackendError` is only local-environment failure: `DataDir(String)`,
  `CreateDataDir { path, source }`, `Init(String)`.
- Read accessors: `bootstrap()`, `library()`, `downloads_state()`, `queue_snapshot()`,
  `playback_state()`, `volume()`, `data_dir()`, `runtime_handle()`, `spawn()`,
  `spawn_blocking()`, `preview_mode()`.
- `shutdown(self)` stops audio/media/Discord threads, flushes queued durable writes
  (bounded), then drops the runtime. Runtime drop **waits for in-flight blocking tasks**
  (an open native file dialog can delay it). Drop all `BackendHandle` clones before the
  process exits or the final `library.json`/`settings.json` writes can be lost. Call it on
  the application thread from the exit path.

#### Threads and blocking model

See the thread table in §4.1. The rules:

- **Sync** commands are UI-thread safe: short locks, no network, no direct disk I/O. The
  one sanctioned exception is `remove_downloads` (explicit user delete: inline unlinks +
  `Downloads` event); its `.names.json` update is queued to the download-name writer, not
  fsynced on the UI thread.
- **Async** commands must be awaited in a spawned task; never `block_on` on the UI thread
  or inside the backend runtime.
- Locks are `std::sync::Mutex` via `util::LockExt::lock_safe()` (poison recovered). Never
  hold `player.core`/`library` across `.await` — mutate + clone a snapshot under one
  guard, then emit. Existing event orderings: `Downloads` = downloaded → downloading →
  progress → failed; `bootstrap()` reads settings, then player, then library.

#### Event bus

```rust
pub enum BackendEvent {           // Debug + Clone + PartialEq; name() = legacy rift://… name
    Track(Track), State(PlaybackState), Queue(QueueSnapshot), Progress(Progress),
    Library(Library), Downloads(DownloadState), Error(String), Notice(String),
    DownloadProgress { id: String, permille: u16 }, // throttled byte progress
    OpenPlaylist(String),         // playlist id the UI should navigate to
}
```

- `BackendEvents` is `Send`; the app owns exactly one and receives via `try_recv()`,
  `drain()`, `blocking_recv()`, `recv()` or `is_closed()`. It is an **unbounded** Tokio
  mpsc: producers never block, so the app must drain every frame or memory grows.
- Events are **broadcasts, not replies**. A command's return value is the reply, and a
  command may emit several events before returning. Get launch state from `bootstrap()`,
  never by replaying events. Ordering is FIFO per producer only — treat the bus as
  "state changed to this", not a transaction log.
- `Progress` comes from the audio thread while `Playing` (~4 Hz; 30 ms cadence during a
  fade) plus on track start, seek and crossfade commit; ticks while
  `Loading`/`Paused`/`Stopped` are ignored. **Duplicate-looking events are by design**: one
  play emits `Track` before and after credit enrichment, and `Queue` on change and again
  after enrichment — consumers must be idempotent.
- Other emitters: `State` on transport transitions, `Library` on any library/recents
  change, `Downloads` on claims/transitions (whole snapshots), `DownloadProgress` on
  throttled byte progress (`id` + permille, no set clone), `Error`/`Notice` for toasts,
  `OpenPlaylist` after a backend-driven import.

#### Command surface

42 public command methods: the 40 preserved legacy commands plus two new ones,
`fetch_image` and `is_online`. (Three legacy window commands moved to winit and are now
`WindowCommand`s in the app crate; three yt-dlp management commands were removed with native
streaming.) `docs/backend-api.md` §7 records the same 46 → 43 → 42 accounting. All live on `BackendHandle`;
parameters/returns are the old IPC commands with
the injected `app`/`state` removed; errors are human-readable `String`s (legacy strings
preserved). `async` below means "must run in a spawned task"; the rest are sync and
UI-thread safe.

- **Startup:** `bootstrap() -> Bootstrap` — library, queue, state, volume, current track,
  progress, downloads, Discord/crossfade/update-notification settings, preview.
- **Search/catalog** (async, network; audio-only filter applied): `search(query)`,
  `search_artists`, `search_albums`; `get_artist(id)`, `get_artist_songs(id)` (up to 1000
  tracks + bounded credit backfill), `get_album(id)`.
- **Playback** (async): `play_tracks(tracks, start, source)` (validates `start`, returns
  before the stream is fetched), `play_track(track)` (replaces the queue with one track).
- **Transport/settings** (sync): `toggle_play()`, `next_track()` (manual next wraps and
  overrides repeat-one), `prev_track()` (restarts the current track when position > 3 s),
  `seek(position)` (no-op unless playing/paused; NaN ignored; clamps), `set_volume(f32)`
  (live only) / `save_volume(f32)` (persists; call on slider release),
  `set_discord_rpc(bool)`, `set_crossfade(secs)` (clamps `0..=12`), `toggle_shuffle()`,
  `cycle_repeat()` (Off → All → One).
- **Queue** (sync): `queue_add(track)` (deduped by id; starts playback if idle),
  `queue_play_next(tracks)`, `queue_move`, `queue_remove`, `queue_jump`, `queue_clear`.
- **Library/playlists** (mostly sync): `toggle_like(track)` (matched by song),
  `create_playlist`, `delete_playlist`, `rename_playlist`, `move_playlist`,
  `add_to_playlist`, `remove_from_playlist`; `import_yt_playlist(url)` is **async/network**
  and accepts a `list=` URL or bare id; `export_playlist(id)`/`import_playlist()` are
  fire-and-forget native `rfd` dialogs on the blocking pool, reporting via
  `Notice`/`Error` (plus `Library`/`OpenPlaylist` on import) and emitting nothing on
  cancel.
- **Downloads:** `download_tracks(tracks)` spawns a sequential background job that emits
  a `Downloads` snapshot on each claim/finish/fail/remove, `DownloadProgress { id,
  permille }` at most every 100 ms, and one coalesced `Error` per pass for failed one-off
  tracks. `remove_downloads(ids)` cancels in-line (tombstone + files) and emits
  `Downloads`.
- **Updates:** `check_update()` (**async**, GitHub `releases/latest`; never fails, degrades
  to "no update"), `set_update_notifications(bool)`, `open_url(url)` (sync, fallible:
  absolute `http(s)` only, embedded credentials stripped, platform opener gets one argv
  element, no shell).
- **Images:** `fetch_image(url)` (**async**; one GET through the backend's dedicated
  **image** client — HTTPS only, label-boundary host allow-list, redirects re-validated,
  hard cap `MAX_IMAGE_BYTES = 16 MiB`; raw encoded bytes). Metadata/connectivity keep the
  separate shared JSON client; the app crate has no HTTP client. This is the **only**
  cover-art transport.
- **Connectivity:** `is_online()` (**async**; `HEAD https://www.youtube.com/` with a 2 s
  cap; online iff the final host is `youtube.com`/`*.youtube.com` — any HTTP status
  counts; transport failure or another host = offline). Emits no events; the controller
  polls it.

#### Playback engine (queue, shuffle, crossfade)

- `PlayerCore` (`player/state.rs`) is the single mutable state: `queue`, `current`,
  `shuffle`/`repeat`, `state`, `volume`, `position`/`duration`, `source`,
  `shuffle_history`/`shuffle_cursor`, `generation`, `failures`,
  `crossfade`/`crossfade_armed_for`, `pending_next`. `PlayerShared` adds the audio sender,
  client, shared HTTP client, `playback_path`, `Persister`, `last_session`.
- A play: `start_playback` bumps `generation`, sets `Loading`, emits `Track` →
  `State(Loading)` → `Queue`, and spawns a fetch (offline first). The whole track is
  downloaded into memory and handed to the audio thread; `Playing` commits only after the
  decode acks (10 s bound), and stale fetch/decode results are discarded by generation.
  Failures auto-skip (budget = queue length, then stop with an `Error`); a direct jump
  resets the budget.
- **Play identity.** Audio events carry the generation the audio was started for
  (`AudioCmd::Play` sends `core.generation`; a crossfade sends `anchor + 1`, the
  generation `commit_crossfade` assigns). The manager drops
  `Position`/`Duration`/`Ended` whose id is not `core.generation`, and drops
  `Ended` unless still `Playing`, so a queued event from the track the user just
  left cannot move the bar, correct the length, arm a crossfade or advance the
  queue. `play_tracks`/`play_track` install the replacement queue and
  validate/claim the start index in one critical section (`prepare_play_index`).
- **Crossfade** (0..=12 s, 0 = hard cut) arms once per generation when `Playing`,
  repeat ≠ `One`, the duration is known and the remaining time is inside the window.
  It pins the next index (`pending_next`), prefetches it, and commits only while that
  generation/pin is current and playback is still `Playing` inside the window. Any
  invalidation (skip, pause, repeat/shuffle/queue edit, backward seek, slow prefetch)
  falls back to a hard cut at the natural end. Preserve these gates when changing playback.
- Shuffle keeps an ordered played-set/history covering every track once before repeating;
  Previous walks it; every queue mutation remaps history (`remap_history`) and clears a
  stale `pending_next`. Pure mutations live in `player/queue.rs`.
- `announce_track` (after `Playing`) adopts the decoded duration when metadata had none
  (rejects ≤0 or >24 h), writes it back to the queue row, backfills credits over the
  network, re-emits `Track`/`Queue`, records `recently_played` (cap 30) and emits
  `Library`. All now-playing publishing is gated on the play generation being current; OS
  media / Discord mirroring happens inside the backend, never in the UI.

#### Persistence and the data directory

- Precedence: `BackendConfig::data_dir` → `RIFT_DATA_DIR` (non-empty) → platform data
  dir + `APP_IDENTIFIER`. **Do not change the identifier or the layout** — existing
  installs keep their files there: Windows `%APPDATA%\dev.jerimiah.rift`; Linux
  `${XDG_DATA_HOME:-~/.local/share}/dev.jerimiah.rift`; macOS
  `~/Library/Application Support/dev.jerimiah.rift`.
- Files: `library.json` (`liked`, `playlists`, `recently_played`), `settings.json`
  (`volume`, `discord_rpc`, `crossfade`, `update_notifications`; unknown legacy keys such
  as `yt_dlp_path` are ignored — **do not add `deny_unknown_fields`**), `playback.json`
  (last queue snapshot, restored **stopped**, best-effort), `downloads/<safe stem>.m4a`
  (see "Downloads and offline" for the encoding and legacy readability),
  `downloads/.names.json` (the `stem -> id` index for names that are not
  self-identifying), `library.json.pre-collapse[.N]` (numbered pre-repair snapshot),
  `*.json.bad` / `*.json.bad.N` (corrupt/unreadable quarantine). The app also writes
  `.window-state.json` there, and on Unix `.single-instance.lock` (Windows uses a named
  mutex, no file) (§5.4).
- **Durable files** (`library.json`, `settings.json`): a mutation takes the lock,
  mutates + clones the snapshot, and queues it to `DurableWriter`. Pending snapshots are
  **coalesced per path** (the queued job keeps its position; its value/serializer is
  replaced), so N rapid mutations cost one queued snapshot and the newest state still
  wins. The queue holds one pending snapshot per path plus the in-flight write; a hard
  64-path cap sends an overflow path's write inline on the caller's thread (durability
  over latency, logged, never silently dropped). One FIFO thread serializes writes and
  publishes each atomically (unique temp file in the target directory → write →
  `sync_all` → rename; Unix also syncs the directory). A UI-thread command never waits on
  disk in the normal path; `flush()` is the barrier (when it returns true, the newest
  snapshot for every write queued before it has been attempted) and shutdown flushes. A
  failed write is logged, recorded (last file/reason + count) and surfaced as one
  coalesced `Error` toast per burst by a backend watchdog; it is not retried.
- **Corruption:** a JSON parse failure, invalid UTF-8, or a read error that is not
  `NotFound` moves the file to `<name>.json.bad`, then `.bad.1`, `.bad.2`, … (never
  overwriting an earlier backup) and loads defaults; the store stays writable because the
  original survives as the backup. If the move itself fails, the file is the only copy:
  the store becomes read-only for the session (refuses to save) and one startup `Error`
  toast names it. See `docs/backend-api.md` §4 and `util::LoadStatus`.
- Preview mode (`RIFT_PREVIEW` in a debug build) keeps settings/library in memory only,
  never writes `playback.json`, and no-ops network/playback commands, so placeholder rows
  cannot corrupt real user data.

#### Downloads and offline

- `Downloads` (`downloads.rs`): `downloaded`/`downloading`/`failed` sets plus the permille
  `progress` map. `state()` takes all locks in a fixed order so a snapshot never shows
  contradictory statuses.
- **On-disk names are an explicit contract.** `fetch::safe_stem(id)` writes
  `[a-z0-9_-]` literally and every other byte as `~` + two lowercase hex digits
  (`a/b` → `a~2fb`, `..` → `~2e~2e`): injective, decodable (`stem_id`), case-insensitively
  unique (uppercase is escaped), no separators/dots/spaces, Windows device names escaped
  (`con` → `~63on`), and capped at 120 chars (overlong ids keep a prefix + `!` + FNV-1a
  hex; `downloads/.names.json` maps those back). A stem containing `_` is also indexed,
  because the pre-P5 fold could have produced it.
- **Old files stay readable:** `load` never renames or deletes an existing `.m4a`. It
  records every non-empty stem; `is_downloaded`/`fetch_track` also consult
  `fetch::legacy_safe_stem(id)`, so a pre-P5 `<id>.m4a` (and an old `_`-folded
  `a_b.m4a` for `a/b`) still plays. A folded legacy name is ambiguous without an index
  entry and is accepted for every id that folds to it (pre-P5 behaviour); new writes never
  produce one, and publishing refuses to replace a non-empty final file.
- Transfer: the job claims a track atomically (`begin()`; a second claim returns `false`),
  `stream::fetch_to_path` writes that claim's `<safe_stem>.<attempt>.m4a.part` with a
  `progress(received, total)` callback and never renames, then the worker records any owned
  name and renames to
  `<safe_stem>.m4a` on success (never over a non-empty file, never creating a second name
  when a legacy copy exists) or deletes the `.part` on failure. Each claim has a token and
  its own part path; `remove` invalidates the token, so a stale worker cannot move progress,
  mark the row, delete the retry's part, or publish over it. `set_progress` ignores
  unclaimed ids; a `total == 0` drops the entry. The completed file is `sync_all`ed before
  the rename.
- **Remove cancels.** `Downloads::remove(id)` clears the claim/progress/failed marks,
  tombstones the id and deletes the current `<safe_stem>.m4a`, its `.part`, any pre-P5
  legacy name for the id (unless the name index says that `_` name belongs to another id)
  and the index entry (queued to the name writer; the unlinks are the only inline disk
  work). `begin` clears the tombstone (a retry is allowed);
  `finish`/`fail`/`mark_failed`/`set_progress` ignore a tombstoned id; `state()` filters
  them out. The worker re-checks the tombstone before and after its rename, so a canceled
  transfer publishes nothing and emits no error.
- Retry exists only for **kept-offline** playlists (`playlist_fully_downloaded`): up to 5
  passes with 0/30 s/1 m/2 m/4 m delays (capped), one coalesced toast, and an
  `is_online()` short-circuit that gives up while offline. Other failures fail immediately
  with a toast; gave-up ids stay in `failed` so the row offers retry, and re-claiming
  clears that mark.
- **Playback is offline-first**: `fetch::fetch_track(downloads_dir, id)` serves a
  non-empty local `<safe_stem>.m4a` (then the pre-P5 name) without network, otherwise
  falls back to `fetch::fetch_bytes`. The startup scan deletes zero-byte `.m4a` and
  leftover `.part` files. `safe_stem` encodes ids because they can come from imported JSON.

#### OS media keys and Discord presence

- `media.rs` starts the `rift-media` thread only when integration is available; with
  `media_hwnd: None` on Windows it skips entirely (SMTC panics without an HWND), logging
  once and losing only media keys/now-playing. OS keys map Toggle/Play/Pause →
  `toggle_playback`, Next → manual next, Previous → `play_prev`, Stop → `stop`; seek-style
  events are ignored.
- `discord.rs` owns presence policy on its own thread: pushes coalesced with
  `MIN_INTERVAL = 15 s`, re-asserted by a `HEARTBEAT = 60 s`; a same-track restart clears
  presence, waits `PRESENCE_REFRESH_GAP = 500 ms`, re-posts (so Discord's elapsed timer
  resets for repeat-one/replay). Each command's caller waits `IPC_TIMEOUT = 500 ms`; on
  Unix the transport sets a 250 ms socket read/write timeout so a silent peer fails as an
  I/O error and the worker retires/reconnects instead of being abandoned. The Windows
  named-pipe `File` has no std timeout (an interruptible read needs unsafe Win32 FFI,
  which this crate forbids), so the backend bounds that case instead:
  `MAX_ABANDONED_WORKERS = 3` stalled workers per session, then presence is disabled until
  restart (logged once) rather than leaking threads/sockets without bound; an activity that
  was already posted can remain visible until Discord clears it or the app restarts,
  because clearing it would need one more request. `APP_ID`/
  `LOGO_ASSET` constants live there — **a fork needs its own Discord application id and
  assets**.

#### Errors, invariants, pitfalls

- Never `unwrap`/`expect` on user data or the environment. Use `LockExt` (poison
  recovery) and the existing non-finite guards: NaN volume mutes, NaN crossfade disables,
  NaN seek is ignored, non-finite duration/position ticks are dropped, `f64` seconds go
  through `try_from_secs_f64` before reaching rodio/souvlaki (an audio-thread panic would
  silence the session).
- Command errors are UI copy: keep the exact legacy `String`s when touching a command
  (`"nothing to play"`, `"playlist name cannot be empty"`, …).
- `#![forbid(unsafe_code)]`; new public items need doc comments and public fallible
  functions need `# Errors` sections.
- Keep persisted formats additive/serde-default; never change `APP_IDENTIFIER`; preserve
  the atomic temp-file write and corrupt-file quarantine behavior.
- Do not build a client per fetch: metadata/connectivity share one, cover art uses the
  dedicated `commands::images` client (HTTPS-only, host allow-list by label-boundary
  suffix: `ytimg.com`, `ggpht.com`, `googleusercontent.com`, `gstatic.com`; no
  credentials/non-default ports; `image/*` content type; redirects re-validated per hop,
  max 3; 16 MiB cap), and audio uses `stream`'s own client.

#### Tests and probes

```sh
cargo check -p rift
cargo test -p rift                       # unit + loopback tests; no network
cargo check -p rift --examples
cargo clippy -p rift --all-targets
cargo tree -p rift                       # must stay free of webview/desktop-shell crates
```

There is no `tests/` integration directory in `crates/rift` and no `#[ignore]`d test
there; network experiments are the example binaries listed in §5.2/§7.4. `cargo run -p
rift` does not exist (library crate) — the app is `cargo run -p rift-desktop`.

#### Authoritative references

- `docs/backend-api.md` — constructor/config/errors, all command signatures, event
  payload/emission table, data-directory layout and durability, threading rules, legacy
  mapping, verification. Read before wiring; update in the same change as an API change.
- `docs/native-streaming.md` — streaming/download design (authoritative; §4.1 is a
  historical verification record).
- `docs/controller-notes.md` — the app-side consumer of this API.
- The source is final truth: module docs in `crates/rift/src/**` record rejected
  alternatives and bug history; `crates/rift-types/src/lib.rs` is the model contract.
  **Historical, not current:** `docs/innertube-research.md`.

### 5.4 `rift-desktop` — application shell, controller, platform glue

Everything between the OS and the UI/backend: process entry and winit event loop, per-frame
composition, the UI→backend bridge, UI state/actions, cover decoding, preview data, window
state/single instance, the Linux desktop launcher, the Windows icon build script, and the
dev/runtime Cargo features. The renderer, immediate-mode framework and screens are §5.5.

```toml
# crates/rift-desktop/Cargo.toml (comments in the manifest are load-bearing)
[[bin]] name = "rift-music"      # NOT "rift": Discord game detection would think it's the game RIFT
[lib]   name = "rift_desktop"    # lets the UI/controller be tested without a window
[features]
default = ["backend"]            # the real app
backend = ["dep:rift"]           # --no-default-features = same UI, inert controller
preview = []                     # dev-only; DO NOT add to default
```

Select it with `-p rift-desktop`; the executable is `target/{debug,release}/rift-music[.exe]`,
never `rift`. `lib.rs` modules: `app`, `controller`, `cover`, `gfx`, `preview` (cfg-gated),
`screens`, `single_instance`, `state`, `text`, `theme`, `ui`, `window_state`.

#### Ownership map

| Path | Owns |
|---|---|
| `src/main.rs` | `main()`, dev `Args` parsing, startup order, `NativeApp` (`ApplicationHandler`), window creation, redraw loop, winit→`InputState` translation, `WindowCommand` execution, startup timing logs, screenshot early-return |
| `src/app.rs` | `pub struct App`: owns `State` + `Ui` + `Controller`; `tick()`, `frame()`, `dispatch_ui()`, `WindowCommand`, cover-texture cache/trimming, toast lifetime |
| `src/state.rs` | `State` (everything rendered), `View`, `SearchState`, `Action` (the single UI→app vocabulary), `ResizeEdge`, small helpers |
| `src/controller/**` | `mod.rs` (`Controller`, `ControllerEvent`, poll/dispatch, navigation + generations, `TrackCache`, online probe), `commands.rs` (`BackendCommands`, `LiveCommands`/`InertCommands`), `tasks.rs` (async task results + filters), `cover.rs` (cover request de-dupe), `tests.rs`/`backend_event_tests.rs` |
| `src/cover.rs` | `decode_cover`, decode limits, `CoverTracker` (in-flight/failed de-dupe), `CoverImage`, `MAX_COVER_DIMENSION` |
| `src/preview.rs` | deterministic placeholder data (`preview_track`, `preview_library`, `preview_queue`, `preview_state`, `preview_artist_page`, `preview_album_page`, `preview_search_results`) |
| `src/theme.rs` | design tokens (`Theme::default()`; colours sRGB straight-alpha, metrics logical px) |
| `src/window_state.rs` | persisted size/position/maximized in the legacy `.window-state.json` |
| `src/single_instance.rs` | process-wide guard (Windows named mutex / Unix `flock`), raw FFI, no extra dependency |
| `build.rs` | embeds the Windows PE file icon from `assets/icons/rift.rc`; no-op on other hosts/targets |
| `assets/**` | embedded fonts, icon set, prerasterised logo, Windows `.rc`, and the Linux launcher entry (`linux/Rift.desktop`) |
| `shaders/**` | GLSL sources + committed `.spv` binaries embedded by `gfx/pipeline.rs` (§5.5) |
| `tests/ui_flow.rs` | headless end-to-end interaction suite (real `App`, recording backend) |
| `tests/desktop_entry.rs` | Linux desktop-launcher guard: committed entry fields/identity/categories, icon assets, and (Linux) the installer install→uninstall round-trip |
| `tests/live_backend.rs` | six `#[ignore]`d live backend/network/audio flows |

#### Process lifecycle (`main.rs`) — the order is load-bearing

1. `Instant` clock captured as the **first statement** (before logging init; all `startup:`
   timings are measured from it).
2. `tracing_subscriber` init; default filter `rift=debug,rift_desktop=debug`, overridable
   with `RUST_LOG`.
3. `Args::parse(std::env::args().skip(1))?`.
4. **Screenshot early-return** — `--screenshot <path>` runs `run_screenshot` and exits
   before any window, backend, long-lived thread **or the single-instance guard**. A
   capture therefore works while a normal Rift instance is running.
5. `single_instance::acquire()`; `None` → prints `Rift is already running.` and exits 0.
   A second launch does **not** focus the existing window (no IPC).
6. `run_windowed`: `EventLoop::new()` → `ControlFlow::WaitUntil(now + 16 ms)` →
   `event_loop.run_app(&mut NativeApp)`.

Dev `Args` (`--preview`/`--demo`, `--live`, `--screenshot <path>`, `--view <name>`,
`--size WxH`, `--pointer X,Y`, `--scale F`, `-h/--help`) exist only under
`#[cfg(any(debug_assertions, feature = "preview"))]`. In a shipped release binary every dev
flag is rejected at parsing with a message containing `development-only`, before any
window/GPU/backend work.

**Window creation** (once, from `resumed`): saved/default logical size (1160x740), min
940x620, `with_decorations(false)`, `with_resizable(true)`,
`with_visible(!HIDE_WINDOW_UNTIL_FIRST_FRAME)`, and on Linux the launcher identity
(`with_linux_launcher_identity()`: Wayland `app_id` / X11 `WM_CLASS` from `APP_ID` +
`BINARY_NAME`). The runtime window icon is loaded from
`assets/icons/128x128.png` (a different thing from the PE file icon described below). Six embedded
font faces parse on a scoped `rift-font-init` thread concurrently with
`gfx::Renderer::new(window)`, then join into `ui::Ui::new_with_fonts(fonts)` (a panicked
worker falls back to `Ui::new()` with a warning). The window is created **before** the
backend because Windows SMTC needs the `HWND`; `Controller::new(media_hwnd)` then constructs
`RiftBackend` once. A backend construction error is non-fatal: warn, fall back to
`Controller::new_preview()`, and toast the error.

`HIDE_WINDOW_UNTIL_FIRST_FRAME` is `true` on Windows only: create hidden, present the first
frame into the swapchain, then show + focus and log
`startup: window shown (painted) +N ms`. A failed first present still reveals the window so
a broken renderer is diagnosable. Other platforms show immediately (hidden-window swapchain
behaviour is not verified there).

**One redraw** (`WindowEvent::RedrawRequested`): update
`InputState` time/delta/scale/viewport (logical = physical / scale) → `app.tick()` → mirror
`window.is_maximized()` into state → `paint = app.frame(input)` → `renderer.frame(&paint)` →
reveal window on first successful present → execute `app.take_window_commands()`
(`Drag`, `Minimize`, `ToggleMaximize`, `Close`, `BeginResize(edge)`) → update OS cursor from
`app.cursor_shape` → `input.end_frame()` → `request_redraw()`. `about_to_wait` re-arms
`WaitUntil(now + 16 ms)` (steady ~60 fps; repainting every frame is intentional for
animations, and Windows stops delivering redraws when occluded).

Exit order matters: `window_state::save_on_exit` → `renderer.shutdown()` (GPU before the
surface/window drops) → `app.controller.shutdown()` (backend threads stop last).

**Winit → `InputState` translation**: mouse position, logical tooltip/edge positions, wheel
(`LineDelta * 40` or `PixelDelta`), modifiers, mapped keys + typed text. Near the border a
left press starts a native resize (5 logical px hit zone, swallowed before the UI sees it).
**Ctrl+Alt is treated as AltGr and still types** characters like `@ { €` (many layouts);
don't "fix" that. On focus loss, held keys/buttons are cleared *without* synthesising release
edges and a one-frame `focus_lost` flag tells `begin_frame` to drop stale latches so the
first click after alt-tab is a fresh press.

#### Frame contract (`app.rs`)

`App { state: State, ui: Ui, controller: Controller, window_commands: Vec<WindowCommand>,
cursor_shape, now }`; `App::new_with_ui(preview, controller, ui)` is the windowed path and
bootstraps the controller iff it is live. Two methods are a contract:

- **`App::tick()` runs before every `App::frame()`**: drain `controller.poll()`, apply
  controller events to `State`, expire the toast, and forward queued cover requests
  (`std::mem::take(&mut state.cover_requests)` → `controller.request_cover(url)`).
- **`App::frame(input)`**: global shortcuts only when `!ui.wants_keyboard()` (Space →
  TogglePlay, Ctrl/Cmd+Left/Right → prev/next, Escape clears playlist editors), then
  `ui.begin_frame` → `screens::root(&mut ui, &mut state, &mut actions)` → `paint =
  ui.end_frame()` → `cursor_shape = ui.cursor_shape()` → **then** dispatch each action.
  Backend results are visible on a later frame; a frame's paint list is internally
  consistent.
- `dispatch_ui` handles UI-local actions (navigation, editors, toast, `SetOnline`,
  `Window*` → `WindowCommand`s) and routes **everything else** through
  `controller.dispatch(other)`. An action is translated exactly once; the controller
  defensively ignores UI-local variants.
- **Window control never touches winit from the UI**: `Action::Window*` → `WindowCommand`
  → executed in `main.rs`. `ResizeEdge` lives in `state.rs`.
- **Cover textures**: on `ControllerEvent::Cover(image)` the app allocates
  `state.next_texture_id` (starts at 1024; font atlas owns low ids), uploads via
  `ui.upload_texture`, then trims `MAX_CACHED_COVERS = 256` (256 px RGBA8 ≈ 256 KiB each)
  by evicting the lowest texture ids and calling `ui.drop_texture`; a later frame simply
  re-requests the URL. Replies for URLs no longer in `state.covers` are dropped. The map
  is also bounded at `MAX_CACHED_COVER_ENTRIES = 512` entries (failed/in-flight URLs,
  FIFO via `State::cover_order`, never a texture-owning entry).

#### State and actions (`state.rs`)

`State` is a direct port of the legacy frontend's `use_state`: backend mirrors (`library`,
`queue`, `playback`, `track`, `progress`, `volume`, `discord_rpc`, `crossfade`,
`update_notifications`, `downloads`, `update_banner`, `update_status`, `online`, `preview`),
navigation (`view: View`, `search`, `artist_page`, `artist_songs`, `album_page`,
`page_generation`), transient UI state (`renaming`, `new_playlist`, `import_playlist`,
`toast`, `sidebar_width`, `window_maximized`) and covers (`covers`, `cover_requests`,
`next_texture_id`). `State::apply_bootstrap(Bootstrap)` seeds the mirrors. Helpers used by
screens include `liked_index()` (a cached `Arc<SongIndex>`, rebuilt when
`set_library()` is called or when the liked list's allocation changes — replace
the library through `set_library()`; a direct `library` assignment self-heals via
the allocation key, but an in-place element edit needs `set_library`),
`download_status()`, `download_fraction()`, `request_cover()`, `pinned_ids()`.

`View`: `Home`, `Search`, `Liked`, `Settings`, `Playlist(String)`, `Artist(String)`,
`ArtistSongs(String)`, `Album(String)`. `SearchTab`: `Songs` (default), `Artists`, `Albums`.

`Action` is the **only** vocabulary a screen uses for intent; the full variant list is in
`state.rs` and the mapping table is `docs/controller-notes.md` §4. Groups: navigation,
playback (`PlayTracks{tracks,start,source}`, `PlayTrack`, `TogglePlay`, `NextTrack`,
`PrevTrack`, `Seek(f64)`, `SetVolume(f32)`, `SaveVolume`, `ToggleShuffle`, `CycleRepeat`…),
queue, library/playlists (including rename/new-playlist editor actions), settings,
downloads, update/misc, and window control. **Add new actions to that table** in the same
change.

#### Controller: the only backend seam

`ControllerEvent` (drained by `App::tick`): backend broadcasts `Track(Option<Track>)`,
`PlaybackState`, `Queue`, `Progress`, `Library`, `Downloads`, `DownloadProgress { id,
permille }`, `Error`, `Notice`, `OpenPlaylist`; async replies `SearchResults{…}`,
`SearchArtists`, `SearchAlbums`, `ArtistPage{generation,artist_id,result}`,
`ArtistSongs{generation,artist_id,result}`, `AlbumPage{generation,album_id,result}`,
`UpdateStatus`, `Cover(CoverImage)`, `CoverFailed(String)`, `OpenArtist(String)`,
`Online(bool)`. A page `Err` sets `State::page_error` (never a bare toast), and the page
screens render a Retry control emitting `Action::RetryPage`.

`BackendCommands` (`Send + Sync + 'static`) is the testable command seam:
`commands::live::LiveCommands` wraps `rift::BackendHandle`; `InertCommands`
(preview/screenshot and `--no-default-features`) accepts and ignores everything
(`bootstrap` returns `Err("no backend attached")`); tests use recording doubles.
The compiler forces every `BackendCommands` implementation to stay complete.

Public surface: `Controller::new(media_hwnd) -> anyhow::Result<Self>` (builds the backend
once on the UI thread; without the `backend` feature it errors naming `--features
backend`), `new_preview()`, `#[doc(hidden)] with_commands(...)` (test seam),
`is_live()`, `bootstrap()`, `poll() -> Vec<ControllerEvent>`, `dispatch(Action)`,
`navigate_view(&View)`, `cancel_search()`, `request_cover(String)`, `check_update()`,
`online()`, `shutdown()`.

- `bootstrap()`: synchronous `BackendHandle::bootstrap()` snapshot; seeds controller
  mirrors (`volume`, `current_track`, `library`, `TrackCache`); runs the launch-time update
  check when enabled; starts the first reachability probe.
- `poll()` never blocks: emits synchronous-dispatch events queued in `pending`, drains the
  backend bus with `try_recv` until empty, applies async `TaskResult`s (generation filters
  + cover bookkeeping), then maybe spawns the periodic online probe.
- `dispatch()`: exhaustive `Action` match. `SetVolume` clamps and remembers; `SaveVolume`
  persists the remembered value. `MovePlaylist{from,to}` resolves the sidebar row index
  against the controller's library mirror, so a stale index cannot move the wrong playlist.
- **Stale generations**: `Action::Search` bumps `search_generation`; a reply for a
  different generation is dropped *before* becoming a `ControllerEvent`.
  `navigate_view(&View)` bumps `page_generation` on **every** navigation and de-duplicates
  in-flight page loads; artist/album replies carry the generation. Don't move this filtering
  into `app.rs` (which only mirrors `state.page_generation` as defence in depth).
- **`TrackCache`**: bounded `id → Track` map (8192 entries, oldest-insertion eviction) filled
  from bootstrap and every event/result. `ToggleLikeId`/`DownloadTracks` carry ids only and
  resolve through it; a miss becomes an explicit `Error` toast, never a guessed action.
- **Cover pipeline**: `State::request_cover` marks a miss and queues the URL →
  `App::tick` → `Controller::request_cover` (`CoverTracker` decides fetch/in-flight/failed;
  failures are remembered for the session and never retried) → `LiveCommands::fetch_cover`
  on the backend runtime → `BackendHandle::fetch_image` (dedicated image client, 16 MiB
  cap; §5.3) →
  `spawn_blocking(decode_cover)` → one `TaskResult` → `ControllerEvent::Cover`/`CoverFailed`.
  `decode_cover` enforces `MAX_SOURCE_EDGE = 4096` / `MAX_SOURCE_ALLOC = 64 MiB` **before
  allocation** (decompression-bomb guard) and downscales the longest edge to
  `MAX_COVER_DIMENSION = 256` with Lanczos3 (never upscales).
- **Online status**: `is_online()` probe at bootstrap and at most every
  `ONLINE_PROBE_INTERVAL = 10 s` when none is in flight; a changed result emits
  `Online(bool)`; failure keeps the previous value. Screens grey out un-downloaded tracks
  while offline.
- `shutdown()`: marks non-live, drops the event bus, then `RiftBackend::shutdown()`.
  **Never call while the app is still drawing**; only `main.rs::exiting` calls it.

**Threading model**: UI thread — backend construction, bootstrap, poll, sync dispatch
calls, cover de-dup bookkeeping, shutdown. Backend runtime — every async command. Blocking
pool — cover decode, `rfd` dialogs, backend file I/O. Backend threads — audio/media/
Discord/persist/durable (§4.1). The UI thread never awaits a future and never does network
or CPU-heavy work.

#### Platform glue

**`window_state.rs`** persists the window in the exact legacy file and JSON shape so
existing installations keep their remembered window: `<data dir>/.window-state.json`, key
`"main"`, object with `{width, height, x, y, prev_x, prev_y, maximized, visible,
decorated, fullscreen}`; sizes/positions are logical px converted through the live scale
factor. Constants: `FILE_NAME = ".window-state.json"`, `DEFAULT_SIZE = (1160, 740)`,
`MIN_SIZE = (940, 620)` (= `Theme::min_window_*`), `MAX_DIMENSION = 16384`. API:
`load`/`save` (best-effort atomic temp+rename) / `load_default` / `save_default` /
`restore(&Window, &WindowState)` / `track(&Window, &mut WindowState)` on
resize/move/scale/maximize events / `save_on_exit`. Restore clamps sizes, validates that a
remembered position still overlaps a monitor by `MIN_VISIBLE_EXTENT = 48` physical px (else
centres), and rejects zero sizes. A missing/corrupt file → `None`, file left in place.

**`single_instance.rs`** — exactly one Rift may own the audio device/library/playback
snapshot. Windows: named mutex `Local\dev.jerimiah.rift-single-instance` via raw `kernel32`
FFI (`Local\` is per-session, so different users can each run one). Linux/macOS/BSD:
non-blocking exclusive `flock(LOCK_EX | LOCK_NB)` on `<data dir>/.single-instance.lock`
(released by the kernel on crash). Other platforms: no-op success. `acquire() -> Option<Self>`
(RAII; `None` = busy, treated conservatively). Acquire **first**, before window/backend/
threads. `is_already_running()` is a diagnostic probe only.

**Linux desktop launcher** (`assets/linux/Rift.desktop`, `lib.rs`, `main.rs`,
`tools/install-linux-desktop.sh`) — optional application-menu integration: the committed
freedesktop entry plus the `APP_ID = "Rift"` / `BINARY_NAME = "rift-music"` identity that
`with_linux_launcher_identity()` puts on the window (Wayland `app_id` / X11 `WM_CLASS`) so
the compositor groups the window under the entry and its icon. The installer copies the
binary, five hicolor icons (32/64/128/256/512, from `assets/icons/`) and the entry into a
user prefix (default `~/.local`, no root), rebases the entry's `Exec`, refreshes the
desktop/icon caches best-effort and removes everything on `--uninstall`; `--dry-run` only
prints. Publication is transactional: files are staged under temporary names, the entry is
validated first, previous destinations are backed up, and a failed rename/manifest write
restores the previous binary/icons/entry/manifest (a first-install failure leaves nothing
behind). Desktop-entry basename, `APP_ID`, `StartupWMClass` and the WM class have to agree —
drift means a generic taskbar icon or a window not grouped under the menu entry. Guard:
`tests/desktop_entry.rs` (entry structure + icon sizes everywhere; installer
install→uninstall round-trip and mid-publish/manifest-write rollback on Linux). Contract: `docs/native-platform.md` `## Linux desktop launcher`.

**`build.rs` and the two icons**: the *runtime window icon* is set by
`main.rs::window_icon()` from `assets/icons/128x128.png` (`include_bytes!`) and only affects
the running window/taskbar button. The *executable file icon* that Explorer/Start
menu/shortcuts read without running the app is a Windows PE resource (`RT_GROUP_ICON` +
images) embedded by `build.rs`, which compiles `assets/icons/rift.rc` (`1 ICON "icon.ico"`)
through the Windows SDK `rc.exe` via the Windows-only host build dependency
`embed-resource`. `build.rs` returns early unless the host is Windows **and**
`CARGO_CFG_TARGET_OS == "windows"`, uses `manifest_required()` so a missing resource
compiler fails the build loudly, and emits `rerun-if-changed` for `.rc`/`.ico`. The resource
is checked, not assumed: `.github/scripts/check-windows-icon.ps1` parses the PE in CI, in
`release.yml`, and in `tools/ci-local.ps1` — because the regression shipped once silently.
Keep both icons; they serve different mechanisms.

**Assets/shaders are compile-time embedded** — no runtime asset paths. Fonts live in
`assets/fonts/` (Inter weights + Material Icons filled/outlined, licences included) and are
baked by `src/text/embedded.rs`; `assets/logo/logo@2x.png` (64x64 prerasterised titlebar
mark, regenerate with `python crates/rift-desktop/assets/logo/generate.py` — needs
`rsvg-convert`, Pillow, numpy) is baked by `screens/cards/textures.rs`; `shaders/*.spv` are
committed SPIR-V (§5.5). `Previews/image.png` is the canonical 1160x740 reference render.

#### Tests for this area

- Module unit tests: `controller/tests.rs` + `controller/backend_event_tests.rs` (dispatch
  mapping, generation drops, cover de-dupe, online edge-triggering, bootstrap,
  backend-event mapping behind `cfg(all(test, feature="backend"))`), `src/cover.rs` (decode
  caps/no-upscale/alpha/garbage bytes/tracker bound), `app.rs` (cover cache bound, orphan
  replies), `state.rs`, `main.rs` (focus loss), `window_state.rs` (legacy
  schema/clamping/off-screen restore), `single_instance.rs`.
- `tests/ui_flow.rs` — headless acceptance suite: the **real** `App` driven exactly like
  `main.rs` (`App::tick()` then `App::frame(InputState)`), preview state at 1160x740
  scale 1.0 with `Controller::with_commands(Recorder)`; asserts app state, paint list,
  queued `WindowCommand`s and recorded backend calls. Every click is a press frame + a
  release frame; tests serialise on a static `Mutex` because screens keep session statics.
- `tests/live_backend.rs` — six `#[ignore]`d live flows; real backend + network + audio
  device; each points `RIFT_DATA_DIR` at an empty temp dir and removes `RIFT_PREVIEW`,
  restoring/deleting on `Drop`, so real user data is never touched.
- `tests/desktop_entry.rs` — desktop-launcher guard: strict entry parse + required
  fields/identity/categories, bare `Exec` (no `%U`/`%F`), ASCII/LF, icon assets are square
  PNGs of their hicolor size; on Linux only it also runs `tools/install-linux-desktop.sh`
  into a throwaway `--prefix` and asserts the install → `--uninstall` round-trip plus the
  transactional rollback cases (mid-publish failure, manifest-write failure, first-install
  failure).

#### Invariants and traps

- **UI thread never blocks**: no network, disk, image decode or awaiting a future on it.
- Screens emit `Action`s; they never call the backend. An action is translated exactly once.
- Actions are dispatched **after** `ui.end_frame()`.
- **Preview/screenshot are dev-only** at three levels: cfg gates (the CLI/`--screenshot` use
  `cfg(any(debug_assertions, feature = "preview"))`; preview data in `app.rs` also enables
  `test`), release argument rejection, and gate assertions (CI/release/the gate assert the
  `--preview` case specifically). Never add `preview` to `default`.
- **`--screenshot` never constructs a backend**, even with `--live`, and runs *before* the
  single-instance guard — captures are deterministic and can run next to a live app. Pair it
  with `--preview` for placeholder data; without it the capture is the default/empty state
  (a lone `--screenshot` produces an almost-empty PNG). For a real-data window capture use
  the windowed path (`pwsh tools/window_shot.ps1 -Live`).
- **Second launch exits immediately** (no window, no focus IPC). If a window "never
  appears", check Task Manager for a running `rift-music`.
- Release builds are Windows **GUI-subsystem** (`#![cfg_attr(not(debug_assertions),
  windows_subsystem = "windows")]`): no console, so tracing output needs redirection (the
  gate does this). Debug builds keep a console.
- `BackendEvents` is unbounded — skipping `poll()` grows memory. Async completions travel on
  a separate controller channel.
- **Ids vs metadata**: `ToggleLikeId`/`DownloadTracks` carry ids and resolve via
  `TrackCache`; a miss toasts. New actions that need a `Track` should carry it.
- **Only one image-fetch path and one cover decode path** exist: backend `fetch_image`
  (its dedicated image client) + `src/cover.rs::decode_cover`. The app crate has no HTTP
  client; do not add one (`image` is otherwise only used for the window icon).
- Data files are user data: `.window-state.json` keeps the legacy keys; keep
  `RIFT_DATA_DIR` working (live tests depend on it).
- Assets are hard-coded by path in `include_bytes!`/`build.rs`; renaming/moving one breaks
  the build. The PE-icon check exists because the embed step can silently vanish.

#### Authoritative references

- `docs/architecture.md` — render model, paint/renderer/UI contracts, windowing, preview/
  screenshot, startup timing (authoritative; this subsection summarises).
- `docs/controller-notes.md` — full controller contract: threading, Action→command mapping
  (§4), `TrackCache` (§5), generations (§6), cover pipeline (§7), event mapping (§9),
  online (§10), tests (§13–§15).
- `docs/native-platform.md` — window-state, single-instance and Linux desktop-launcher
  contracts.
- `docs/backend-api.md` — the backend side of the seam.
- Gates/scripts: `.github/workflows/{ci,release}.yml`,
  `.github/scripts/{check-local-gates,check-toolchain-consistency,check-duplicate-deps}.sh`,
  `.github/scripts/check-windows-icon.ps1`, `tools/ci-local.sh`,
  `tools/{screenshot,window_shot,verify}.ps1`, `tools/compare_images.py`.

### 5.5 `rift-desktop` — immediate-mode UI, text, renderer, screens

Rift has **no webview, no retained widget tree, no external GUI crate**. Every pixel is
painted by the hand-written immediate-mode layer (`src/ui/**` + `src/text/**`), flattened
into a `PaintList`, and rasterised by the custom vulkano renderer (`src/gfx/**`). Screens
(`src/screens/**`) are plain functions that lay out, hit-test and paint from scratch every
frame.

#### Ownership map and frozen contracts

| Path | Responsibility |
|---|---|
| `src/ui/mod.rs` | `Ui` context, frame lifecycle, ids, `interact` state machine, clip stack, deferred layer, primitives, text/icon draw calls |
| `src/ui/widgets.rs` | styled widgets (`ButtonKind`, switch, checkbox, slider, badge, cover, progress, spinner, equalizer, panel, `format_duration`) |
| `src/ui/text_input/{mod,edit,state}.rs`, `src/ui/{scroll,popup}.rs` | single-line editor + clipboard (`mod` = widget/paint, `edit` = keyboard/clipboard editing, `state` = retained caret/selection + click mapping); scroll containers; menus/tooltips/toasts |
| `src/ui/{id,input,layout,paint,icons,clipboard}.rs` | hashed ids; winit-agnostic `InputState`; `Rect`/`Row`/`Column`/`Padding`; paint-contract re-exports + texture-id rules; `Icon` enum; `Clipboard` backends |
| `src/text/{mod,atlas,embedded}.rs` | fontdue shaping/measuring/wrapping, glyph cache, shelf atlas, `include_bytes!` faces |
| `src/gfx/paint.rs` | **frozen** paint contract: `Rect`, `Color`, `Prim`, `Layer`, `TextureUpload`, `PaintList` |
| `src/gfx/{mod,gpu,pipeline,batch,texture}.rs` | Vulkan bootstrap, pipeline, paint→vertex batching, texture lifecycle, window/offscreen targets |
| `src/screens/**` | screens; `screens/mod.rs` owns `layout()`/`root()` and chrome; `pages.rs` routes views |
| `src/theme.rs` | design tokens (`Theme::default()`) |
| `shaders/quad.vert`, `quad.frag` (+ committed `.spv`) | the only graphics pipeline |

Rules that keep the halves independent: the UI **never touches Vulkan**; the renderer
**never knows about fonts** (text is `Prim::Image` quads from the atlas); `gfx/paint.rs` is
the only shared type boundary (treat it as frozen); screens never paint outside their own
rects and never call the backend — they push `Action`s that `app.rs` dispatches after
`end_frame`.

#### One frame, the contract everything depends on

```rust
let mut ui = rift_desktop::ui::Ui::new();       // parses embedded fonts; no GPU/window
ui.begin_frame(input_state, viewport_rect);     // viewport = logical client rect
screens::root(&mut ui, &mut state, &mut actions);
let paint: PaintList = ui.end_frame();
let cursor = ui.cursor_shape();                 // main.rs maps to a winit CursorIcon
renderer.frame(&paint)?;                        // uploads textures, draws, presents
// app.rs then dispatches `actions`
```

- `begin_frame` stores the input, sets the font scale, starts font bookkeeping, resets
  per-frame state, consumes Tab/Shift+Tab using **last frame's** focus order, and asserts
  it is not called twice. It deliberately **does not clear texture-upload queues**, so cover
  art decoded between frames survives into this frame's `PaintList`.
- `end_frame` paints deferred tooltip/toasts, auto-closes a popup that no screen drew this
  frame, commits interaction bookkeeping, flattens base then deferred prims into
  painter-ordered `Layer`s, builds the atlas upload (whole-image `created` on the first
  frame/growth/scale change, dirty-rectangle `updated` regions for newly rasterised
  glyphs), drains created/updated/dropped, and returns a **fresh** `PaintList`. Calling it without `begin_frame` drains textures and returns an empty list.
- Screens never append to a `PaintList`; only `end_frame()` produces it, and it must not be
  retained across frames.
- `PaintList` invariants: everything in **logical px**, straight-alpha sRGB `Color`;
  painter order = layer order then primitive order; each `Layer` may carry one clip (`None`
  = whole target) that becomes a Vulkan scissor; `clear` is the attachment clear colour;
  `created`/`updated`/`dropped` are the complete texture diff. There is **no gradient,
  blur, rotation or text primitive** — gradients are baked RGBA textures
  (`screens/cards/textures.rs`), shadows are stacked translucent rounded rects
  (`Ui::shadow`), and rotations use a different glyph (e.g. `ChevronRight` instead of a
  rotated `ChevronDown`).

#### Coordinates, DPI and colour

- Logical px, origin top-left, `+y` down. Design against the **1160×740 reference** (min
  940×620), but always read `ui.viewport()` / `screens::layout`; never hard-code window size
  in a screen.
- `main.rs` puts the window scale into `InputState::scale_factor` and keeps `viewport`
  logical. Text/icons rasterise at **physical** size and emit glyph quads at **logical**
  size; the renderer multiplies all geometry by scale, so layout is identical at 100/125/
  150/200%. A scale change invalidates glyph caches and recreates the atlas (one full
  `updated` upload, same `ATLAS_TEXTURE_ID = 1`; 1024² → max 2048²; one variant at a time).
- Colours are sRGB straight alpha (`Color::hex(0xRRGGBB)` etc.). The renderer converts RGB
  to linear on the CPU, uploads images as sRGB, emits premultiplied linear from the shader
  with `(One, OneMinusSrcAlpha)` blending, and lets the `*_SRGB` target encode on store —
  alpha is never gamma-converted.

#### Ids and UI-owned retained state

`ui::Id` is a 64-bit FNV-1a hash (`Id::from_str`/`.with(key)`/`Id::hash_of`; `.child`
splits; `Id::NONE = 0`), stable only within one process (nothing persists ids). All
cross-frame widget state is keyed by it inside `Ui`: active widget, keyboard focus, hover
animations, scroll offsets, text carets/selections, tooltips, popup identity, toast queue.

- Widgets build ids automatically from a scope string + label + **sub-pixel quantised rect**
  (`auto_id`/`auto_id_icon`). Stateful widgets whose value flips (switch/play button) use
  `auto_id_stable`, which excludes state, so keyboard focus survives activation. Use
  `*_with_id` variants when a label or position can change without the control being a
  different control.
- Screen convention: prefix ids with the screen (`"home/…"`, `"queue/…"`) and derive row
  ids from stable data (`Id::from_str("queue/row").child(index as u64)` or
  `.with(&track.id)`). Never derive ids from pointers or frame counters. Nested controls
  (heart, kebab) hit-test before the row background so the inner control wins.
- `Sense { click, drag, focus }` is passed to `Ui::interact(rect, id, sense)`; `Response`
  carries `hovered, pressed, clicked, secondary_clicked, double_clicked, active, dragged,
  drag_delta, drag_started, focused, focus_visible, hover_t`. Press-then-release-over-same
  = click; double-click within 0.35 s; drag starts after 2 px. Open popups register input
  blockers for the next frame so widgets underneath neither hover nor click.

#### Layout (`ui/layout.rs`)

`Rect` (frozen) already has `right/bottom/center/contains/intersect/inset/split_top/…`;
`layout.rs` adds `split_vertical`, `inset_pad(Padding)`, `expand`, `offset`, `with_*`,
`split_cols::<N>(gap)`, `split_rows::<N>(gap)`, `align(Size, Align, VAlign)`, `clamped()`,
and sequential cursor containers `Column::new(rect, gap)` / `Row::new(rect, gap)` with
`fixed(px)`, `percent(pct)`, `remaining()`, `skip`, `rect()`. Gaps go **between** cells;
`percent` is of the whole rect (not the remainder), so percentages summing past 1 silently
squeeze later cells. Derive every rectangle from the `Rect` you were handed — never
recompute absolute coordinates in a screen.

#### Input, focus, keyboard, cursor

- `ui/input.rs::InputState` is winit-agnostic: viewport, scale factor, time/delta, pointer +
  `mouse_in_window`, button down edges, one-frame pressed/released edges, `focus_lost`,
  scroll (logical px), modifiers, ordered `keys: Vec<KeyInput>`, typed `text`. `main.rs`
  fills it and calls `end_frame()` after the UI consumed it. Helpers: `key_clicked`
  (ignores OS repeat), `key_pressed` (includes repeat), `key_down`, `command()`.
- Tab/Shift+Tab move focus in paint order using the previous frame's `focus_order`; clicking
  a focusable widget focuses it without the visible ring; clicking empty space blurs. Focus
  rings via `ui.focus_ring(rect, radius)` (offset +2, buttons) /
  `focus_ring_inset` (offset −2, rows/cards/menu items). Also `keyboard_mode()`,
  `focused_id()`, `request_focus(id)`, `clear_focus()`, `is_focused(id)`.
- **IME:** `main.rs` enables IME once at window creation and routes
  `WindowEvent::Ime(Ime::Commit)` through `ui::push_ime_text`; `KeyEvent::text` uses the
  same helper (control characters stripped, multi-byte preserved). Preedit is not
  previewed. The focused text field records its caret rectangle (`Ui::ime_cursor_area`)
  and `main.rs` calls `Window::set_ime_cursor_area` when it changes, so the candidate
  window opens at the field instead of the window origin. The paths are complementary per
  winit's contract; no dedupe guard.
- **Focus loss is one frame later:** `end_frame` compares the frame's starting focus with
  the final focus (click-away, another widget, Tab, vanished widget) and records it; a text
  field/editor reports `lost_focus` on its next call. `Ui::request_focus_on_open(id)` is the
  one-shot autofocus for inline editors (true only on the opening frame; a later
  `focused_id() == None` means the user clicked away and the editor closes).
- **`ui.wants_keyboard()` is frame-lagging**: cleared in `begin_frame`, recomputed in
  `end_frame`; `app.rs` checks it *before* `begin_frame` to skip global shortcuts. Any new
  global shortcut must gate the same way, and screens must not consume keys while a text
  field is active.
- Cursor requests are last-writer-wins within a frame; read `ui.cursor_shape()` only after
  `end_frame`. `main.rs` maps the shapes to winit icons.

#### Painting primitives, clips and layers

```rust
ui.fill(rect, color); ui.rounded_rect(rect, color, radius);
ui.bordered_rect(rect, fill, radius, border, border_color); ui.stroke(...); ui.line/hline/vline/circle(...);
ui.shadow(rect, radius, strength);                    // stacked translucent rects, no blur
ui.image(rect, texture_id, tint, radius); ui.image_uv(rect, texture_id, uv, tint, radius);
ui.panel(rect, radius); ui.elevated_card(rect, radius); ui.divider(rect);
ui.push_clip(rect) / pop_clip() / with_clip(rect, |ui| ...);   // nested clips intersect
ui.deferred(|ui| ...);                                // paints after all normal content
```

Every primitive stores its clip; `end_frame` starts a new `Layer` whenever the clip changes
and preserves order. Popups/tooltips/toasts paint into the deferred buffer, so they are
always on top regardless of declaration order. Prefer widget methods over hand-painted
approximations — hover/pressed/disabled/focus visuals and theme colours live in them.

#### Widget catalogue (do not re-invent)

```rust
ui.button(rect, "Play", ButtonKind::Primary) -> Response;   // Primary/Secondary/Ghost/PlayAll/HeadButton/Danger, Tab { active }, MenuItem { danger }
ui.button_with_id / button_styled / button_enabled / head_button;
ui.icon_button / icon_button_tinted / icon_button_styled / icon_button_enabled; ui.play_button(rect, playing);
ui.switch(rect /*44x24*/, &mut on) -> CheckboxResponse;     // .changed
ui.checkbox(rect, &mut checked, "Label"); ui.switch_enabled(...);
ui.slider(rect /*4px track row*/, &mut v, min, max) -> SliderResponse;  // .changed
ui.slider_styled / slider_enabled; ui.progress(rect, fraction, color); ui.spinner(rect); ui.equalizer(rect, playing);
ui.badge(rect, "Downloaded", BadgeKind::{Neutral,Success,Warning,Danger,Accent});
ui.cover(rect, Option<TextureId>, radius); ui.cover_round(rect, texture);
ui.text_input(rect, id, &mut text, "Placeholder", &TextInputStyle::default_theme(&theme));
ui.scroll_area(id, rect, content_h, |ui, content| ...) -> (R, ScrollResponse);  // + _stable/_dynamic
ui.popup(id, anchor, Size::new(180.0, n * MENU_ITEM_HEIGHT), PopupAlign::Right, |ui, content| ...);
ui.menu_item(...); ui.menu_separator(rect); ui.tooltip(id, anchor, hovered, "text"); ui.tooltip_for(&response, "text");
ui.toast_error("msg"); ui.toast_info("msg"); ui.toast_with_duration(...); ui.toast_sticky(...);
```

Toasts are single-slot (a newer toast replaces the previous and restarts its timer) and the
framework paints them deferred. Static helpers: `format_duration`, `format_position`,
`MENU_ITEM_HEIGHT = 33.0`, `MENU_MIN_CONTENT_WIDTH = 180.0`.

#### Text stack (`src/text`)

- Six faces are `include_bytes!`-embedded from `assets/fonts/`: Inter Regular/Medium/
  SemiBold/Bold + Material Icons filled/outlined. Parsing is optional at runtime (a
  corrupt/missing baked face warns once and degrades); normal builds never load fonts from
  disk.
- `TextStyle { size_px, weight, color, line_height, letter_spacing }`;
  `line_height == 0.0` means "font default" (Inter ≈ 1.21 em), **not** a spec line box. An
  explicit line height uses CSS half-leading with the reference font metrics
  (`REF_ASCENT_RATIO = 2210/2048`, `REF_DESCENT_RATIO = 514/2048`) so ink positions match
  `docs/ui-spec.md` §4.2; without it every label sits ~2 px high. Screens should use
  `cards::style(size, weight, color)` / `cards::line_box(size)` (size × 1.33) rather than
  raw `TextStyle::new`.
- Shaping (`Fonts::shape_into`) → `TextShape { glyphs, size, lines, baseline }`; glyphs are
  ordinary tinted `Prim::Image` quads. Measuring (`measure`, `measure_wrapped`,
  `line_width`) uses metrics only and **never rasterises**; rasterisation is lazy, cached
  per (face, glyph, physical px). Greedy wrap trims trailing whitespace, honours hard
  newlines and hard-breaks over-long words; `ellipsize` binary-searches a char-boundary
  prefix + `…`. All UTF-8-safe and fuzz-tested.
- Atlas `ATLAS_TEXTURE_ID = 1`, 1024² → max 2048²; a glyph that does not fit is skipped
  this frame and the atlas grows at the next `begin_frame` (deferred by design). Texture-id
  space: `< 1024` reserved for framework/atlas, app textures start at
  `FIRST_IMAGE_TEXTURE_ID = 1024` (generated app textures use 1_000_000+).
- Draw helpers: `text` (single line, left, v-centred, ellipsized), `text_centered`,
  `text_right`, `text_aligned`, `text_wrapped` (top-aligned), `text_wrapped_aligned`,
  `text_raw` (no wrap/ellipsis, can overflow — clip it yourself), `label_value`,
  `section_title`, `eyebrow`, `empty_state`; icons via `ui.icon(rect, Icon::Play, color,
  16.0)`, `icon_outlined`, `icon_font`, `icon_label`. `text_input` returns
  `TextInputResponse { changed, submitted, cancelled, lost_focus, response }`; per-field
  caret/selection lives in `Ui` keyed by id. Clipboard is the real OS clipboard by default
  (`arboard`, lazy, no-op when unavailable) with injectable memory/disabled backends for
  tests. On Linux the `arboard` dependency enables `wayland-data-control`, so a Wayland
  session uses the compositor's clipboard and only falls back to X11 without Wayland or
  without that protocol (see the `ui::clipboard` module docs).

#### Renderer (`src/gfx`)

- Public API: `Renderer::new(Arc<winit::Window>)`, `Renderer::new_headless(w, h,
  scale_factor)`, `window_size()`, `logical_size()`, `scale_factor()`, `resize(physical)`,
  `frame(&PaintList) -> Result<()>`, `shutdown()`, `wait_idle()`, and
  `gfx::screenshot(width, height, scale_factor, &paint, &path) -> Result<()>` (headless
  Vulkan → PNG; used by `--screenshot` and GPU tests).
- `frame` = texture sync (whole `created`/`updated` via staging; incremental glyphs as
  sub-rectangle `updated` regions; dropped retired 4 frames) →
  `FrameBatch::build` converts each prim to 6 vertices in device px (one scissor per clip
  layer, one draw per consecutive same-texture run, CPU-culls prims outside their clip) →
  record one command buffer → present. Three frames in flight (`FRAMES_IN_FLIGHT = 3`)
  through ring uniform/vertex buffers; `Vertex2D` is 80 bytes; `Fifo` present; sRGB surface
  format preferred (a UNORM fallback logs a warning and would need a manual sRGB encode).
- An `Image` prim referencing an unknown texture id still draws the 1×1 white fallback
  (logged once), so a texture-lifetime bug shows on screen instead of vanishing.
- **Shaders**: `shaders/quad.vert`/`quad.frag` are GLSL and `*.spv` are committed SPIR-V
  embedded with `include_bytes!`. **There is no build step that compiles GLSL** — editing
  the GLSL requires regenerating and committing both `.spv` files with a GLSL→SPIR-V
  compiler (e.g. `glslc shaders/quad.vert -o shaders/quad.vert.spv`, same for `.frag`;
  `glslangValidator` also works) — nothing verifies they match, so a stale `.spv` silently
  ships. vulkano 0.35 binds vertex
  inputs by **name**, so shader `in` variables must stay named exactly like `Vertex2D`
  fields (`pos, uv, color, rect, params, border_color`). The fragment shader does
  rounded-rect SDF antialiasing for solid and textured quads.
- Validation layer: enabled by default in debug builds when `VK_LAYER_KHRONOS_validation`
  is installed; `RIFT_VK_VALIDATION=0` opts out, `=1` forces in release; never a hard
  dependency. `RIFT_GFX_STATS=1` logs per-300-frame frame-time/vertex/draw/texture stats.
  Swapchain `OutOfDate`/`SurfaceLost` recreate and drop the frame; `DeviceLost` is fatal by
  design (no device-recreation path). No persistent pipeline cache; the UI repaints every
  frame rather than idling. Vulkan practices are in `docs/architecture.md` — read it before
  changing `gfx/` beyond the paint contract.

#### Screens (`src/screens`)

`screens::layout(ui, state) -> Layout { viewport, titlebar, body, sidebar, content, main,
queue, player }` computes the fixed bands from `Theme` + `state.sidebar_width`: titlebar
46 px, player 84 px, sidebar default 216 px (clamped 160–420; reference captures use 335),
queue panel only when non-empty and not on Settings (235 px expanded / 45 px collapsed,
clamped to leave main room). `main_content_rect` subtracts 24 px horizontal + 20 px vertical
padding and the always-reserved 10 px scrollbar gutter (`scrollbar-gutter: stable`).

`screens::root(ui, state, actions)` is the only entry point; it paints in DOM order and draws
content **and** chrome:

1. window fill (`theme.bg`), then window-anchored overlays (`chrome::toast`,
   `chrome::update_banner`) **early** so their buttons latch presses before the
   player/queue underneath (they still paint deferred/on top);
2. `menu::layer` early (consumes the outside click that dismisses an open menu);
3. main view: Home uses `scroll_area_stable_dynamic` and returns content height; every other
   view goes through `pages::page` inside `with_clip(main)`, falling back to Home;
4. queue band, titlebar, sidebar, player bar;
5. `menu::layer` again if content opened a menu this frame (`end_frame` would otherwise
   auto-close the never-drawn popup), then `menu::pin_backdrop_cursor`.

Conventions: screen fns are `fn(ui, state, actions, rect)` (routed view modules pair
`height(ui, state, width) -> f32` with `view(...)`; `pages::page` computes the stable scroll
container); views are `state::View`, actions are `state::Action`; screens never block, retain
widgets, call the backend, or allocate per-frame `Vec`/`format!` on hot paths (Home reuses a
subtitle buffer; shaping buffers are recycled).

Module map: `home.rs`, `liked.rs`, `playlist.rs`, `search.rs`, `artist.rs` (profile +
artist-songs), `album.rs`, `settings/` (`layout.rs` geometry, `view.rs` three cards,
`updates.rs` status line), `queue.rs` (UP NEXT body; collapse is a **thread-local static**),
`track_list/` (`TrackListOpts`, `list_height`, `track_list`, collection meta + Play/Download
head actions, row menu entries), `cards/` (shared primitives: buttons, section headings, card
grid `grid_metrics`, cover tile, track row, slider, generated textures), `chrome/`
(titlebar, sidebar incl. playlist drag-reorder/inline editors/resize handle, player bar,
queue panel wrapper, overlays), `menu/` (`Entry` carries an `Action`; `menu::open`, `layer`,
`is_open`, `kebab`; wraps `Ui::popup`), `overlays.rs`.

Screens keep a few **session-scoped statics by design**: queue collapse (thread-local)
and open-menu identity (`menu/state.rs`). The one-shot generated-texture upload guard is
**not** a process static: it lives on `Ui` (`Ui::claim_generated_textures`), so a second
`Ui` in the same process (tests, a future second window) still uploads its own logo/
fallback/note textures. Tests must serialise and reset the remaining statics.

Scroll only through `scroll_area` / `scroll_area_stable` / `scroll_area_dynamic` /
`scroll_area_stable_dynamic`: the container clips its own paint, offsets content, draws the
10 px scrollbar, and consumes wheel events only while hovered; the `stable` variants always
reserve the gutter so grids don't reflow; offsets are retained per id
(`scroll_offset`/`set_scroll_offset`/`reset_scroll`). Menus use `ui.popup(...)`, which
positions/clamps/flips the card, registers blockers for the next frame, closes on
click-outside/Escape/Tab, focuses the first item and returns focus to the trigger on Escape.
`content_size` is the **content** size: the popup adds its own 10 px padding/border (the
closure receives the content rect inset by 5 px) and clamps width to ≥80 px — pass the
rows' sizes, don't add padding.

#### Theme and the visual spec

`Theme::default()` is the single source of colours/metrics: surfaces (`bg #0e0e10`,
`bg_raised #151518`, `bg_hover #1d1d21`, `border #232328`, `titlebar/sidebar/player_bg
#111113`, `queue_bg #121215`), text (`#ececef`/`#8b8b93`), accent/danger/focus, font sizes,
`titlebar_height 46`, `sidebar_width 216`, `player_height 84`, `track_row_height 54`, radii,
spacing, min window 940×620. Use `ui.theme()` and clone it (`let theme =
ui.theme().clone();`) because `ui` is borrowed mutably while drawing; `theme_mut()` exists
but has no in-tree callers, so prefer `ui.theme().clone()`. Widget-internal colours outside `Theme`
are exported from `ui` (`SUCCESS`, `WARNING`, `DANGER_TEXT`, `SLIDER_TRACK`, `SCROLLBAR_*`);
don't add hex literals to screens.

`docs/ui-spec.md` is the authoritative pixel spec (§1 geometry/root paint order, §2 tokens +
state colours, §3 icons, §4 typography/line boxes, §5 assets, §6 chrome, §7 views, §8
interactions, §14 acceptance checklist). Reference captures: `Previews/image.png`
(1160×740) and `tools/reference-old-ui.png`.

When prose and code disagree, code + tests + `docs/ui-spec.md` win; fix the prose when you
touch it (the 2026-11-22 hardening pass cleared the last known values — `player_height` is
84 and the test-count tables were refreshed).

#### Tests: pure logic vs GPU vs headless flows

All in-tree via `cargo test -p rift-desktop`; counts drift, re-run rather than trusting
tables.

| Suite | Kind | Covers |
|---|---|---|
| `ui::tests`, `ui::tests_polish` | pure logic, no window/GPU | paint prims/layers/clip/deferred order; interaction, focus/Tab, keyboard activation; text prims/ellipsis/atlas id; icons; scroll; popups/blockers/toasts/tooltips; text input; theme styles; auto-id stability; texture lifecycle (incl. inter-frame uploads, `end_frame` without `begin_frame`); disabled widgets; menu keyboard nav; slider geometry |
| `ui::fuzz` | pure logic | deterministic xorshift over random frames (clicks/drags/wheel/Tab/typing/Escape/resizes/scale changes); finite non-negative rects, valid clips, texture payload sizes, clamped scroll, UTF-8, no double-created textures/popup leaks |
| `ui::gallery` | 1 pure + 2 `#[ignore]` GPU | builds the widget gallery paint list; ignored tests render gallery/HiDPI/image-check PNGs through the real renderer |
| `text::tests` + atlas tests | pure logic | embedded faces; measure vs layout (measure never rasterises); cache hits; atlas allocation/upload; line boxes + half-leading baseline; wrap regressions; ellipsis fit; randomized Unicode fuzz; HiDPI bitmap-vs-logical quads + cache invalidation |
| `gfx::tests` | **GPU, not `#[ignore]`** | headless render of hand-built `PaintList`s asserting pixels: rounded/bordered/UV/clip scene, replaced texture, multi-frame texture lifecycle, `screenshot()` PNG, clear colour, 10k prims + atlas growth, headless resize. Skips only when no Vulkan device can be created; a renderer failure while a device is available **fails** the suite (it is a regression, not a missing driver) |
| screen module tests | pure logic except 4 ignored GPU | each view's geometry/height/state machines and scripted interactions; `screens/mod.rs` pins the reference bands (`reference_layout_bands`, `home_preview_paints_the_reference_geometry`); 4 ignored visual-PNG tests write to `artifacts/screens/` |
| `ui::clipboard` | pure + opt-in OS path | memory round-trip always; the OS round-trip/paste/copy/Wayland-selection tests are opt-in with `RIFT_TEST_OS_CLIPBOARD=1` (they write the real clipboard, restore best-effort — a Wayland selection dies with the test process — and skip headless); write-then-read assertions poll because Wayland commits `set_selection` asynchronously; Linux/Wayland checks a real `wl-clipboard-rs` data-control copy (regression for the `wayland-data-control` feature; skips without Wayland/data-control); unavailable-backend no-op path |
| `tests/ui_flow.rs` | headless end-to-end | real `App` + recording backend; boot bands, sidebar nav, search, player/queue/settings controls, keyboard shortcuts, context menus, text editing, overlay precedence, duplicate playlist names, scroll. Serialised by a process lock (screens keep session statics) |
| `tests/live_backend.rs` | manual `#[ignore]` | live backend/network/audio — not needed for UI work |

```powershell
cargo test -p rift-desktop                                  # everything runnable; GPU skips only without a device
cargo test -p rift-desktop --lib                            # unit tests only
cargo test -p rift-desktop --test ui_flow                   # headless interaction flows
cargo test -p rift-desktop --lib ui::gallery -- --ignored --nocapture   # GPU visual QA (needs Vulkan)
cargo test -p rift-desktop --no-default-features --locked   # UI/renderer-only configuration
```

`ui::gallery`'s GPU tests take an internal mutex (`GPU_TEST_LOCK`) because each offscreen
render owns process-wide GPU state; never assume ignored GPU tests can run concurrently.
`gfx::tests` and the screenshot path run under the validation layer in debug builds.

#### Pitfalls that bite

- Paint-contract errors are silent on screen: an unknown `TextureId` becomes a white square
  (logged once), degenerate rects are skipped. Assert on `PaintList` in tests instead of
  eyeballing.
- `end_frame` auto-closes a popup not drawn that frame — a menu opened mid-frame must be
  drawn before it (that is why `root` calls `menu::layer` twice). `App::navigate` also
  closes an open menu explicitly, so a backend-driven jump cannot leave the old view's
  menu floating over the new page.
- `wants_keyboard()` is only meaningful between frames; inside a screen it is still false.
- On Linux, clipboard paste from other apps only works with `arboard`'s
  `wayland-data-control` feature: an X11-only connection sees the XWayland clipboard, and
  on Wayland desktops selections copied by Wayland apps (browsers) are not mirrored into
  it — Ctrl+V then silently no-ops. That was the "cannot paste a YouTube Music link into
  the import box" bug; `ui::clipboard`'s opt-in native round-trip and
  `wl-clipboard-rs` tests (`RIFT_TEST_OS_CLIPBOARD=1`) guard the path.
- `ui.text(...)` ellipsizes single lines and vertically centres; `text_raw` can overflow.
  `line_height = 0.0` gives Inter's default line box — use `cards::style`/`cards::line_box`
  for spec line boxes.
- Atlas growth is deferred one frame; only overflow after a scale/font-size change makes
  text briefly disappear. Do not "fix" it by clearing the atlas eagerly.
- Texture queues intentionally survive `begin_frame`; removing that would drop cover art
  decoded between frames.
- Session statics and the GPU test mutex mean screen/GPU tests must serialise.
- Screens own no GPU resources: allocate app texture ids ≥ `FIRST_IMAGE_TEXTURE_ID`, and
  drop cached covers with `ui.drop_texture` when they leave `State`.
- Only one pipeline exists and it has no rotation/gradient/blur primitives — use the
  established workarounds, don't extend the paint contract from a screen.

#### Authoritative references

- `crates/rift-desktop/src/gfx/paint.rs` — frozen paint contract.
- `crates/rift-desktop/src/{text/**}` — text/atlas invariants.
- `docs/imgui-api.md` — detailed widget/API reference (verify stale counts/values against
  source).
- `docs/ui-spec.md` — authoritative pixel/design spec.
- `docs/architecture.md` — rendering model, paint/renderer/Vulkan practices, screenshot mode.
- `crates/rift-desktop/src/theme.rs` — design tokens.
- `tools/{screenshot,window_shot}.ps1`, `tools/compare_images.py`, `tools/verify.ps1` —
  capture and comparison workflow (§6.3).

---

## 6. Build system, features, preview mode and environment

### 6.1 Workspace and profiles (root `Cargo.toml`)

- `[workspace] resolver = "2"`, five members under `crates/`. `[workspace.package]` is the
  single source of `version = "0.6.1"`, `edition = "2021"`, `license = "Apache-2.0"`,
  `authors`, `repository`, `rust-version = "1.88"`; members inherit via
  `version.workspace = true` etc. **A version bump is one root line** — never edit a
  member's version.
- All five crates are `publish = false` and stay that way (cargo-deny's
  `allow-wildcard-paths` for path dependencies depends on it).
- No `.cargo/config.toml`, no Node/npm, no bundler, no runner aliases: `cargo build` **is**
  the whole build.
- `[profile.release]`: `lto = true`, `codegen-units = 1`, `strip = "symbols"` (DWARF
  stripped, unwind tables kept so `RUST_BACKTRACE=1` works and a panicking thread can be
  caught). **`panic = "abort"` is deliberately absent** — do not "optimize" it in; a GUI app
  wants diagnosable thread failures, not a silently fatal process.
- `[profile.dev]`: `debug = "line-tables-only"` (the debug executable is ~42 MB and costs
  the Windows loader/AV scan hundreds of ms pre-`main`), plus
  `[profile.dev.package.fontdue] opt-level = 3` (six embedded font faces) and
  `[profile.dev.package.rift-desktop] opt-level = 1` (first-frame layout/raster). These
  speed up `cargo run`/`cargo test` without rebuilding every dependency.
- `Cargo.lock` is committed, never hand-edited, and every gate/CI/release
  build/check/test/clippy/doc/tree command passes `--locked` (`cargo fmt` takes no lock flag). While intentionally changing dependencies, use
  `./tools/ci-local.sh --allow-lock-update` (Linux/macOS) or
  `pwsh tools/ci-local.ps1 -AllowLockUpdate` (Windows) for intermediate passes, then one
  final run **without** it; commit `Cargo.toml` + `Cargo.lock` together.

### 6.2 The toolchain pin is stated in exactly three places

`rust-toolchain.toml` pins `channel = "1.88.0"` with `components = ["rustfmt", "clippy"]`
(rustup honours it automatically). `.github/scripts/check-toolchain-consistency.sh` fails CI
unless all three agree:

1. `[workspace.package] rust-version = "1.88"` in the root `Cargo.toml`;
2. `channel` in `rust-toolchain.toml`;
3. an explicit `toolchain: 1.88.0` input on **every** `dtolnay/rust-toolchain` use in
   `.github/workflows/*.yml`.

It also fails when an action use has no pin, or a pin is a non-version like `stable`. Why:
CI and the local gate compile at the declared floor, so `rust-version` is exercised on every
run and clippy refuses to suggest APIs newer than the floor. Raising the floor is fine, but
change all three places at once; the consistency guard checks only those three, while the
prose mentions in `README.md`/`docs/*.md` must be updated by hand. Don't add targets to
`rust-toolchain.toml`; `release.yml`
installs the two macOS slices it needs explicitly.

The local gate adds a check the file comparison cannot make: step 1 compares the active
`rustc --version` against the `rust-toolchain.toml` channel and fails hard on a mismatch
(no `-Skip*` bypass, §8.1). A `+toolchain`, `RUSTUP_TOOLCHAIN` or `rustup override` therefore
cannot build every step below with a newer compiler and still report `GATE: PASSED`.

### 6.3 Cargo features, preview mode and screenshots

`crates/rift-desktop` features:

| Feature | Meaning |
|---|---|
| `backend` (**default**) | the real app: links `rift` and constructs `RiftBackend` |
| `preview` (non-default) | dev-only placeholder data + dev CLI in **release** builds; the local gate builds an acceptance-only `--features backend,preview` binary, and CI clippy-lints that configuration in release (`cargo clippy --release -p rift-desktop --features backend,preview --all-targets --locked -- -D warnings`). **Never add it to `default`** |
| `--no-default-features` | same UI/renderer with an inert controller; skips audio/network/platform stack. Compiles the same public signatures, window still opens (toast explains the backend error). CI and the local gate test this configuration too |

`cfg(any(debug_assertions, feature = "preview", test))` gates preview support: debug builds
always have it, release builds only with the explicit feature. The shipped release binary
**rejects every dev flag** with a message containing `development-only`; CI, the release
workflow and the local gate (`tools/ci-local.sh` / `tools/ci-local.ps1`) assert the
`--preview` case (same rejection path). CI also compiles the acceptance-only release
configuration, so a break behind
`#[cfg(feature = "preview")]` fails on every OS rather than only in the local gate.

Preview/screenshot commands:

```sh
# deterministic one-shot offscreen render, no window (dev build has the dev CLI):
cargo run -p rift-desktop -- --preview --screenshot artifacts/screens/home.png --size 1160x740 --view home
# interactive window with placeholder data:
$env:RIFT_PREVIEW = "1"; cargo run -p rift-desktop
# download-progress UI capture (0..=1000 permille):
$env:RIFT_PREVIEW_DOWNLOAD = "640"; cargo run -p rift-desktop -- --preview --screenshot artifacts/native-stream/download-bar-liked.png --view liked --size 1160x740
# release-profile capture (maintainers; dev-only feature):
cargo run -p rift-desktop --release --features preview -- --preview --screenshot target/release-preview.png
# helpers
pwsh tools/screenshot.ps1 -View settings        # writes artifacts/screens/<view>.png
pwsh tools/window_shot.ps1                      # real window + swapchain, captures desktop region
python tools/compare_images.py Previews/image.png artifacts/screens/home.png --label home
```

Views: `home`, `liked`, `playlist`, `search`, `artist`, `artist-songs`, `album`, `settings`
(`queue` maps to home). `--screenshot` always uses an inert controller (even with
`--live`) — pair it with `--preview` to render the placeholder library, otherwise it captures
the empty default state. For real data capture the window (`tools/window_shot.ps1 -Live`). Preview data is neutral and deterministic (no cover
URLs, no network). `RIFT_PREVIEW_DOWNLOAD=640` marks placeholder tracks downloading so the
determinate bar can be captured.

### 6.4 Environment variables

| Variable | Effect |
|---|---|
| `RUST_LOG` | tracing filter; unset defaults to `rift=debug,rift_desktop=debug` (startup timeline + subsystem logs) |
| `RUSTDOCFLAGS` | set to `-D warnings` by the docs gate; don't assume rustdoc warnings are non-fatal |
| `RIFT_DATA_DIR` | overrides the data directory (precedence: config → env → platform dir). Tests use throwaway dirs; live tests require it |
| `RIFT_PREVIEW` | dev-only placeholder mode (`1`/non-empty, not `0`); release builds reject it. Also makes the backend preview-mode in debug builds (in-memory settings/library, no network/playback) |
| `RIFT_PREVIEW_DOWNLOAD` | `0..=1000` permille: in-flight placeholder tracks for download-bar captures |
| `RIFT_VK_VALIDATION` | debug auto-enables the Khronos validation layer when installed; `0` opts out, `1` forces in release |
| `RIFT_GFX_STATS` | `1` logs per-300-frame frame-time/vertex/draw/texture stats |
| `RIFT_DISCORD_APP_ID` | application id for the ignored live Discord RPC test |
| `RIFT_PYTHON` | Python for the local gate's image comparison (`--python-path`/`-PythonPath` overrides) |
| `PRUNE_TRACE` / `PRUNE_DEBUG` / `PRUNE_KEEP` / `PRUNE_REPORT` | fixture-regeneration debug/keep/report switches read by `crates/rift-ytmusic/examples/prune_fixture.rs` (§11.G) |
| `CARGO_TARGET_DIR` | point at a `target-*` scratch dir when `target/` is locked by a running process |

### 6.5 Platform prerequisites and helper scripts

Platform build prerequisites:

- **All:** Rust 1.88 via rustup (the toolchain file installs it + rustfmt/clippy on first
  cargo command). The app needs a Vulkan-capable GPU driver to run, but CI/gates never
  create a swapchain.
- **Windows:** Visual Studio C++ Build Tools (MSVC linker; the Windows SDK supplies `rc.exe`
  for `crates/rift-desktop/build.rs`), PowerShell 7 (`pwsh`), Git for Windows (Git Bash) for
  the shell guards.
- **macOS:** Xcode Command Line Tools (clang/linker); CI builds on `macos-15` (pinned;
  `macos-latest` rolled to a toolchain whose linker segfaults host proc-macro dylibs).
- **Linux:** ALSA is the only hard link dependency; Vulkan is loaded at runtime and winit's
  X11/Wayland backends are dlopen'd. The apt list in §2.1 is the complete set (it includes
  `pkg-config`, which the `-sys` crates need). At runtime the audio thread prefers the
  session's sound server — cpal's PulseAudio host, which PipeWire also serves through
  `pipewire-pulse` — and falls back to raw ALSA only when no server is reachable. That host
  is a pure-Rust protocol client: no extra system package and no new native link, and the
  selected host is logged as `audio output host:` at startup.

Helper scripts in `tools/`:

| Script | Purpose |
|---|---|
| `tools/ci-local.sh` | the local acceptance gate on Linux/macOS (bash), mirrors `ci.yml` and `tools/ci-local.ps1`; options `--skip-tests`, `--skip-release`, `--allow-incomplete`, `--allow-lock-update`, `--python-path`; same log path, rows and verdict as `ci-local.ps1` |
| `tools/ci-local.ps1` | the local acceptance gate on Windows, mirrors `ci.yml` and `tools/ci-local.sh`; parameters `-SkipTests`, `-SkipRelease`, `-AllowIncomplete`, `-AllowLockUpdate`, `-PythonPath`; writes `artifacts/ci-local-<timestamp>.log`; prints a PASS/FAIL/WARN/SKIP row per step and ends in `GATE: PASSED`, `GATE: PASSED with N warning(s)`, `GATE: INCOMPLETE (N skipped step(s))` or `GATE: FAILED`; exits 0 only when nothing was skipped or failed, 1 on failure, 2 when incomplete unless `-AllowIncomplete` |
| `tools/screenshot.ps1` | offscreen preview capture per view (`-View`, `-Live`, `-Window`) → `artifacts/screens/<view>.png`; prints the comparison command |
| `tools/window_shot.ps1` | launches the real window (swapchain proof) and captures the desktop region; Windows-only, manual, never a gate |
| `tools/verify.ps1` | lighter older suite: fmt, both feature-config checks, `cargo test -p rift`/`-p rift-desktop`, screenshots of 8 views, reference comparison. Not the acceptance gate (`ci-local.sh`/`ci-local.ps1` are) but keep it working |
| `tools/compare_images.py` | `REFERENCE NEW [--out DIR] [--label NAME]`; needs Pillow+numpy; writes side-by-side/diff/overlay PNGs under `artifacts/compare/`; prints mean/max channel delta and % pixels differing by >16; **always exits 0** (reports, doesn't judge). References: `tools/reference-old-ui.png` (the gate's archived old-UI target; small expected deltas) and `Previews/image.png` (native render; expected pixel-identical for the acceptance screen) |
| `tools/install-linux-desktop.sh` | Linux desktop-launcher installer: copies the built (or `--binary PATH`) `rift-music`, hicolor icons and `Rift.desktop` into `--prefix DIR` (default `~/.local`, no root needed), rewrites `Exec`, refreshes the desktop/icon caches best-effort; `--uninstall` removes it, `--dry-run` prints only, `--no-cache-refresh` skips the cache refresh |

`tools/verify.ps1`/`screenshot.ps1`/`window_shot.ps1` write only under `artifacts/`; all are
Windows-oriented (the app still builds and runs on the other platforms — only these helper
automations are Windows scripts). `tools/install-linux-desktop.sh` is the Linux-oriented
one: a bash script, run it on the Linux host (its guard test also runs it from `cargo test`).

---

## 7. Testing strategy

### 7.1 The rules

- **The default test suite is offline and hardware-independent.** No test hits the network,
  and GPU tests self-skip when no Vulkan device is present. Anything needing YouTube or a
  real audio device is `#[ignore]`d and run manually (§7.4).
- Tests live next to the code (`#[cfg(test)] mod tests` / `*_tests.rs` in the module tree)
  or in `crates/*/tests/` for integration targets. There is no mock-HTTP crate: transport
  tests use a real `std::net::TcpListener` on loopback.
- **Pure logic is preferred.** The player's decisions, layout math, renderer batching and
  UI interaction are testable without a window/GPU because they are separate from the
  event loop. Add the pure test first; use GPU/window tests only when pixels are the subject.
- Counts drift — run the commands, don't trust prose tables. Sizes after the 2026-11-22
  hardening + review + polish pass: `rift` 308 tests incl. 60 in `player/tests.rs` / 79 in
  the player module, `rift-ytmusic` 96 (81 lib + 15 fixtures), `rift-types` 16,
  `rift-discord` 18 + 1 ignored live, `rift-desktop` 375 lib + 1 bin + 14 `desktop_entry` +
  17 `ui_flow`, 6 ignored `live_backend` (+10 ignored doc-tests). **844 runnable test
  functions** in the default five-package gate.

### 7.2 Test inventory by crate

| Crate / target | Kind | What it proves |
|---|---|---|
| `rift-types` unit tests | pure | `same_song`/`SongIndex`/`fill_missing_from` matrix, model helpers |
| `rift-ytmusic` lib + `tests/fixtures.rs` | offline | parser tolerance on sanitized recorded responses; loopback transport (403 rotation, caps, client chain, resolver); fixture cleanliness |
| `rift` unit + inline loopback tests | offline | player/queue/shuffle/crossfade decisions (79 tests in the player module), library/settings/downloads stores, durable writes/corruption, streaming downloader against loopback servers, backend construction |
| `rift-discord` unit + `--ignored` | unit/offline + live | frame codec/handshake/nonce/CLOSE; live smoke needs Discord running |
| `rift-desktop` lib | offline | UI framework/fuzz/gallery (pure), text/atlas, gfx GPU tests (skip only when no Vulkan device can be created; a renderer failure with a device present fails), every screen's geometry/interactions, controller dispatch/generations/cover/online (backend feature), app cover cache, window-state, single-instance |
| `rift-desktop --test desktop_entry` | offline | committed Linux launcher entry: required fields/identity/categories, bare `Exec`, ASCII/LF, icon assets/sizes; installer install→uninstall round-trip plus transactional rollback cases (Linux only) |
| `rift-desktop --test ui_flow` | offline, headless | real `App` frame-by-frame interactions against a recording backend (17 flows) |
| `rift-desktop --test live_backend` | **live, `#[ignore]`** | 6 real backend/network/audio flows (playback, download progress, offline restart, etc.) |

### 7.3 Running subsets

```sh
# the gate's two configurations
cargo test -p rift -p rift-types -p rift-ytmusic -p rift-discord -p rift-desktop
cargo test -p rift-desktop --no-default-features

# single crate / target / name filter
cargo test -p rift
cargo test -p rift-desktop --lib
cargo test -p rift-desktop --test ui_flow
cargo test -p rift player                      # substring filter on test names
cargo test -p rift-desktop --lib ui::gallery -- --ignored --nocapture   # GPU visual QA
```

If a running `rift-music.exe` holds `target/` files open on Windows, use a scratch dir:
`$env:CARGO_TARGET_DIR = "target-verify"; cargo test` (`target-*` is git-ignored).

### 7.4 Live / network probes (manual, never CI)

```sh
cargo run -q -p rift-ytmusic --example resolve_stream                       # 20-id coverage sample
cargo run -p rift-ytmusic --example resolve_stream -- <video-id> [<video-id>…]
cargo run -p rift --example native_probe                                    # playback fetch -> decode
cargo run -p rift --example download_probe                                  # real download worker + progress + restart replay
cargo run -p rift --example smoke -- "daft punk get lucky"                  # search -> fetch -> decode
cargo run -p rift --example discord_replay_probe                            # Discord presence (needs Discord + audio device)
$env:RIFT_DISCORD_APP_ID = "<application id>"; cargo test -p rift-discord -- --ignored
cargo test -p rift-desktop --features backend --test live_backend -- \
  --ignored --nocapture --test-threads=1 live_native_playback_download_progress_and_offline_restart
```

These are evidence-gathering tools, not gates. Never commit the URLs/visitor ids they print;
never make a normal test depend on them.

### 7.5 Writing tests here

- **Never touch real user data.** Any test that could construct a backend or touch the data
  dir must set `RIFT_DATA_DIR` to a temp dir and remove `RIFT_PREVIEW` (the live suite does
  both and restores on `Drop`). The user's real library is never a fixture.
- **Serialise on process-global state.** Several areas keep session/process statics (screens'
  queue collapse, menu state, generated textures; ytmusic visitor pool; Discord; GPU state).
  Existing tests use static mutexes / gates — follow the local pattern rather than assuming
  parallel safety.
- **Two frames per click.** The UI is immediate mode: a click is a press frame and then a
  release frame. Assert after `end_frame()`; geometry helpers in `ui_flow.rs` mirror the
  screens' public constants so a layout change fails the interaction instead of clicking
  empty space.
- **Determinism.** Use fixed viewports/scales and direct clock values rather than sleeping.
  The fuzz/gallery harnesses use xorshift, not randomness from the OS.
- **Fixture hygiene.** ytmusic fixtures are sanitized (`example.invalid` URLs, no signatures/
  visitor ids) and a test enforces it; regenerate with `prune_fixture`, never hand-edit.
- **Regression tests for bug fixes.** This repository's history pairs fixes with a test that
  fails when the defect is re-injected; match that.

---

## 8. Quality gates and "done"

### 8.1 The local acceptance gate — `tools/ci-local.sh` / `pwsh tools/ci-local.ps1`

Two front-ends, one gate. `tools/ci-local.sh` is the POSIX (Linux/macOS) front-end; it
needs bash and the usual POSIX tools it invokes, not PowerShell. `pwsh tools/ci-local.ps1`
is the Windows one and requires `pwsh` 7+ (it sets
`$PSNativeCommandArgumentPassing = "Standard"` on 7.3+ so `--`/`-D warnings` survive).
Both run from any directory in the repository, write
`artifacts/ci-local-<yyyyMMdd-HHmmss>.log`, print the same 17 rows
(PASS/FAIL/SKIP/N/A/WARN) in the same order, and share the verdict contract below. Each
refuses the other host type with a pointer to the right script. Keep the two step lists in
sync with each other and with `ci.yml`; CI's
`.github/scripts/check-local-gates.sh` enforces it (`bash -n`, a 17-row set comparison, and
the four clippy configurations in both gates and `ci.yml`).

Options: `--skip-tests`/`-SkipTests`, `--skip-release`/`-SkipRelease`,
`--allow-incomplete`/`-AllowIncomplete`, `--allow-lock-update`/`-AllowLockUpdate`, and
`--python-path PATH`/`-PythonPath PATH` (defaults to `$RIFT_PYTHON`). A full run prints 17
rows in this order:

| # | Step | Notes |
|---|---|---|
| 1 | toolchain consistency | `.github/scripts/check-toolchain-consistency.sh` (run directly by the bash front-end; via Git Bash with a PowerShell fallback on Windows). Also **fails hard unless the active `rustc --version` equals the `rust-toolchain.toml` channel** (no bypass; catches `+toolchain`/`RUSTUP_TOOLCHAIN` overrides) |
| 2 | `cargo fmt --all -- --check` | |
| 3 | `cargo clippy` (`-D warnings`) | explicit package list, `--all-targets`, debug/default features |
| 4 | `cargo clippy -p rift-desktop --no-default-features` (`-D warnings`) | UI-only configuration; compiles different `cfg` branches than step 3 |
| 5 | `cargo doc` | 4 library crates, `RUSTDOCFLAGS=-D warnings` |
| 6 | `cargo test` | five packages, `--locked` |
| 7 | `cargo test -p rift-desktop --no-default-features` | UI-only configuration |
| 8 | release build | `-p rift-desktop --features backend`, prints binary size |
| 9 | `cargo clippy --release` (`-D warnings`) | shipped configuration, then `--features backend,preview` (acceptance-only). The debug clippy rows cannot see `not(debug_assertions)` code |
| 10 | windows executable file icon | `.github/scripts/check-windows-icon.ps1` (Windows only; the POSIX front-end reports N/A, and CI's Windows job checks it) |
| 11 | preview stripped from release | shipped binary must reject `--preview` with the `development-only` message |
| 12 | offscreen release screenshot | builds `--features backend,preview`, captures `artifacts/screens/ci-home.png` at 1160x740, then restores the shipped configuration. **Needs a working Vulkan driver/device** (headless, no window manager); on a GPU-less host use `--skip-release`/`-SkipRelease` (the run is then `GATE: INCOMPLETE` unless the incomplete-run switch is passed) |
| 13 | screenshot vs reference | compares against `tools/reference-old-ui.png` via `tools/compare_images.py`; metrics only — **soft: WARN, never FAIL**, skipped when no Python has Pillow+numpy |
| 14 | no legacy webview crates in the app dependency tree | `cargo tree -p rift-desktop` guard |
| 15 | no legacy webview/WASM manifests in the workspace | scans workspace Cargo.toml files |
| 16 | duplicate dependency baseline | `.github/scripts/check-duplicate-deps.sh` (direct on POSIX, via Git Bash on Windows); per-host baseline records exact `<crate> <version>` pairs, so a third version of an already-baselined crate fails |
| 17 | `cargo deny check all` | skipped when `cargo-deny` is not installed |

**The verdict is part of the contract.** A full run with no SKIP rows prints `GATE: PASSED`
(or `GATE: PASSED with N warning(s)`) and exits 0; only step 13 can warn. A hard failure
prints `GATE: FAILED` and exits 1. Any SKIP row prints `GATE: INCOMPLETE (N skipped
step(s))` and exits 2 — a skipped step is not a pass (§10.17). `N/A` marks a check that
cannot exist on this host (the Windows PE icon); it is not a SKIP and does not affect the
verdict, because CI's Windows job is its proof. The incomplete-run switch
(`--allow-incomplete`/`-AllowIncomplete`) keeps the INCOMPLETE line but exits 0 for a
deliberately partial run (e.g. `--skip-release` on a host without Vulkan); it never turns a
FAIL green.

`--skip-tests`/`-SkipTests` and `--skip-release`/`-SkipRelease` print fewer rows: the tests
switch replaces the two test rows with one SKIP row, the release switch collapses the release
block to three SKIP rows, and rows 11–13 disappear entirely. Environment degradations also
get their own SKIP rows: no Git Bash on Windows → step 16 skipped (step 1 falls back); no
Python+Pillow+numpy → comparison skipped; no `cargo-deny` → supply-chain step skipped; a
non-Windows host → the PE icon check is N/A (not a skip; CI's Windows job covers it).

- Only step 13 is soft (WARN, never FAIL). **CI enforces the toolchain guard, clippy in every
  configuration, duplicate baseline, cargo-deny and the build/test matrix; the screenshot
  comparison is local-only.** A run with SKIP rows is `GATE: INCOMPLETE`, not full proof.
- The POSIX front-end reports the PE-icon row as N/A, not SKIP: the icon is a Windows PE
  resource and CI's Windows job is its proof. N/A is platform scoping, not a skipped check,
  so a fully provisioned POSIX run can reach `GATE: PASSED`; the environment SKIPs
  (`cargo-deny`, Pillow+numpy) still make it INCOMPLETE until installed, and
  `--allow-incomplete` never turns those into a pass.
- Every build/check/test/clippy/doc/tree command gets `--locked` unless
  `--allow-lock-update`/`-AllowLockUpdate`; `cargo fmt` takes no lock flag and the local
  `cargo deny` invocation omits it (CI's deny action passes it).
- **Close any running Rift instance before the gate**: the screenshot step pre-checks for a
  `rift-music` process and fails if one exists (the capture path itself is designed to run
  alongside a live app — the gate is just conservative).
- Warm full run ≈5–6 min on the maintainer's Windows machine; the two release-profile builds
  dominate. On failure, fix and re-run the whole gate — later steps can be affected too.
- The Windows front-end assumes a Windows host; the POSIX front-end runs the same rows on
  Linux/macOS, calls the shell guards directly, and reports the PE icon check as N/A (not a
  skip; CI's Windows job checks it). Both need a Vulkan device only for the screenshot row.

### 8.2 CI — `.github/workflows/ci.yml`

Triggers: pushes to `main` and every PR; `concurrency` cancels in-flight runs per ref. Jobs,
cheap first:

1. **toolchain version is consistent** (ubuntu-22.04) — runs the guard script with
   `shell: bash` explicitly.
2. **rustfmt** (ubuntu-22.04) — installs 1.88.0 + rustfmt, `cargo fmt --all --check`.
3. **local gate scripts are in sync** (ubuntu-22.04) — runs
   `.github/scripts/check-local-gates.sh`: `bash -n` on the POSIX front-end, the PowerShell
   parser on the Windows one when `pwsh` is available, a set comparison of the two
   front-ends' 17 row names, and a check that all four clippy configurations are present in
   both gates and `ci.yml`. No Rust install needed.
4. **clippy, test & build (matrix: ubuntu-22.04, macos-15, windows-latest)**,
   `fail-fast: false`. Steps in order: install Linux system deps; Rust 1.88.0 + clippy;
   `Swatinem/rust-cache`; clippy for debug/default and UI-only `--no-default-features`;
   docs (`RUSTDOCFLAGS=-D warnings`, 4 library crates); tests; tests
   `--no-default-features`; duplicate-dependency check; release build; Windows-only PE icon
   check; shipped binary rejects dev flags; two release clippy steps (shipped configuration,
   then the acceptance-only `--features backend,preview`; separate steps because a Windows
   multi-command `run` can mask the first native failure with the second command's exit
   code). The release clippy steps replace the old
   `cargo check --release --features backend,preview` check: they compile the same graph and
   deny warnings, so neither a `#[cfg(feature = "preview")]` break nor a warning in
   `not(debug_assertions)` code can ship unnoticed.
5. **cargo-deny** (ubuntu-22.04) — `check --all-features --locked` via the pinned
   `EmbarkStudios/cargo-deny-action` (cargo-deny 0.22.x, the version `deny.toml` was
   validated against).

Jobs build at **1.88.0**, not `stable`. Actions are pinned to commit SHAs with a version
comment; never replace a SHA pin with `@vN` (Dependabot updates both). CI never opens a
swapchain, so no GPU is needed. The workflow is `permissions: contents: read` at the top
level and every `actions/checkout` sets `persist-credentials: false`: the default token is
not left in `.git/config` for a build script, proc macro or test to read, and no CI job
needs write access.

### 8.3 Release — `.github/workflows/release.yml`

- Triggered by pushing a `v*` tag (e.g. `git tag -a v0.6.1 -m "Rift v0.6.1" && git push
  origin v0.6.1`) or by `workflow_dispatch` (a manual run from **any** ref, including a tag
  named with `--ref`, only creates/updates a draft and never publishes: manual mode is
  derived from `github.event_name`, and `publish` requires `push` + tag). The workflow is `permissions: contents: read`; only the
  `release`, `checksums` and `publish` jobs raise `contents: write` for `gh release`, the
  `checksums` job additionally raises `id-token: write` and `attestations: write` for build
  provenance (no other job does), and every checkout sets `persist-credentials: false`
  (`GH_TOKEN` is passed explicitly).
- On a tag run the first step verifies **tag == workspace version** (strips `v`, greps
  `^version` from the root `Cargo.toml`). The tag names the release and is the `tag_name` the in-app
  update checker compares against `env!("CARGO_PKG_VERSION")`; a mismatch means a phantom
  update forever or a missed one.
- Matrix: `ubuntu-22.04` → `x86_64-unknown-linux-gnu` → `rift-music-linux-x86_64`;
  `macos-15` builds `aarch64-` + `x86_64-apple-darwin` and `lipo`s them into
  `rift-music-macos-universal` (Rust has no universal target); `windows-latest` →
  `x86_64-pc-windows-msvc` → `rift-music-windows-x86_64.exe`.
- The staged assets are re-checked before upload (Windows PE icon; shipped binary rejects
  `--preview`). For a new release, the three matrix jobs create one **draft** release on
  demand with `--draft` and upload to it via the preinstalled `gh` CLI + `GITHUB_TOKEN` with
  retries; a concurrent create by a sibling job is accepted only when the release now
  exists, and a real creation failure fails the job. A tag rerun reuses the existing release
  (draft or already published) and uploads with `--clobber`, as before.
  Packaging/installers are out of scope; `release-assets/` staging is git-ignored.
- A **`checksums`** job (`needs: release`; `contents: write` for the upload plus
  `id-token: write` and `attestations: write` for provenance) downloads the assets from the
  release, refuses an incomplete release (all three platform asset names must be present),
  writes `SHA256SUMS` over the downloaded files (excluding any older manifest), and uploads
  it with `--clobber`. It runs on manual draft runs too, so that path is exercised. It then
  attests build provenance for the three platform assets with the SHA-pinned
  `actions/attest-build-provenance` (three explicit `subject-path` entries — Linux,
  macOS-universal and Windows — never a glob); the step
  has no `continue-on-error`, so a failed attestation fails the job and `publish` cannot
  make a draft public. Checksums are integrity, not provenance: they are not a signature
  and do not attest who built the artifacts; the attestation is the provenance claim. The
  binaries are otherwise **unsigned** (no Windows Authenticode, no Apple Developer ID
  codesign/notarization): both need project-owned certificates and credentials and are
  deliberately out of scope until those exist. Users verify a download with
  `gh attestation verify <file> --repo gage-lodba/Rift` (add
  `--signer-workflow gage-lodba/Rift/.github/workflows/release.yml` to pin the workflow).
- A final **`publish`** job (`needs: [release, checksums]`,
  `if: github.event_name == 'push' && github.ref_type == 'tag'`,
  `permissions: contents: write`) checks that `SHA256SUMS` is attached, then runs
  `gh release edit "$tag" --draft=false` only after all three matrix jobs and the checksums
  job succeeded. A failed/expired matrix or checksums job therefore leaves a *new* release
  draft-only — never a public page missing platform assets or checksums. When the release is
  already published (a tag rerun), the job is a no-op: it never unpublishes or recreates it.

**Release checklist:**

1. Bump `version` in the root `Cargo.toml` under `[workspace.package]` — members inherit.
2. Regenerate `Cargo.lock` for the new workspace versions without `--locked` (e.g. a plain
   `cargo build` or the gate with `-AllowLockUpdate`), commit `Cargo.toml` + `Cargo.lock`
   together.
3. Check hard-coded version mentions across `agents.md`, `README.md` and `docs/*.md` (e.g.
   `docs/backend-api.md`'s package section; the `v0.6.0` fixture in
   `crates/rift/src/commands/updates.rs` can stay).
4. Run the local gate (`./tools/ci-local.sh` on Linux/macOS, `pwsh tools/ci-local.ps1` on
   Windows; full, **without** the lock-update switch) → `GATE: PASSED` with **no SKIP rows**
   (an environment skip makes it `GATE: INCOMPLETE`/exit 2); commit.
5. Tag the release commit and push (annotated tags, message `Rift vX.Y.Z`; follow
   `git show v0.6.1`).
6. Verify the workflow published all three assets, `SHA256SUMS`, and the notes read
   correctly; the final `publish` job makes the draft public only after every matrix job and
   the checksums job succeeded, so a partial release stays draft-only. Run
   `gh attestation verify <asset> --repo gage-lodba/Rift` on each downloaded asset to check
   the provenance attestation.

### 8.4 Definition of done

Your change is done when **all** of these are true:

- [ ] `cargo fmt --all -- --check` clean (do not reformat others' files).
- [ ] `cargo clippy` clean with `-D warnings` in **all four gated configurations**
      (`--all-targets`): the five-package debug/default command; `-p rift-desktop
      --no-default-features`; `--release -p rift-desktop`; and `--release -p rift-desktop
      --features backend,preview`.
- [ ] `$env:RUSTDOCFLAGS = "-D warnings"; cargo doc -p rift -p rift-types -p rift-ytmusic
      -p rift-discord --no-deps` clean (new public items documented; `# Errors` sections).
- [ ] Tests pass in **both** configurations: default and `-p rift-desktop
      --no-default-features`.
- [ ] The narrow tests for your area exist and pass; a bug fix has a regression test.
- [ ] The local gate has been run on the host(s) available to you and reaches
      `GATE: PASSED` (or `GATE: PASSED with N warning(s)`) with **no SKIP rows**. On
      Linux/macOS the Windows-only PE-icon row is N/A (not a skip), and CI's Windows job
      checks it; install `cargo-deny` and a Pillow+numpy Python so the optional environment
      rows are PASS rather than SKIP. A skipped step is still not a pass, and
      `--allow-incomplete` only makes the exit code usable for a deliberately partial run;
      it does not turn a SKIP into a PASS.
- [ ] Release build compiles; shipped binary still rejects `--preview`.
- [ ] No *unrecorded* duplicate dependency versions: the per-host baseline in
      `.github/scripts/check-duplicate-deps.sh` is updated deliberately when the dependency
      set adds **or loses** a pair (a stale pair now fails the guard, so it cannot silently
      authorize a reintroduction; never prune one to hide a regression); `cargo deny check
      all` clean if a dependency changed; `deny.toml` updated deliberately if a new license
      appears.
- [ ] Every affected `docs/*.md`/`README.md`/`agents.md` sentence is updated in the same
      change; no stale contract.
- [ ] Nothing from `target*/`, `artifacts/`, `*.log` or other git-ignored paths is staged.
- [ ] No commit/stage/push was made unless the user asked for it.

---

## 9. Conventions

### 9.1 Rust style and lint policy

- Rust 2021, `rustfmt` clean, no `rustfmt.toml`/`clippy.toml` — policy lives in
  `[workspace.lints]` in the root `Cargo.toml`, and every member opts in with
  `[lints] workspace = true`.
- Denied lints that affect how you write code: `unsafe_op_in_unsafe_fn`,
  `unused_must_use`, `undocumented_unsafe_blocks`, `dbg_macro`, `todo`, `unimplemented`.
  Warned: `elided_lifetimes_in_paths`, `unused_lifetimes`, `trivial_numeric_casts`,
  `unchecked_duration_subtraction`, `suspicious_operation_groupings`,
  `large_stack_arrays`, `large_stack_frames`, `missing_errors_doc`.
  Consequences: **no `dbg!`/`todo!`/`unimplemented!` may ship**, every `unsafe` block needs
  a `// SAFETY:` note, `Duration`/`Instant` subtraction must be checked, and public fallible
  functions on library crates need an `# Errors` section.
- `unsafe` is effectively absent: the backend is `#![forbid(unsafe_code)]`; the handful of
  allowed blocks in the renderer/platform glue are justified and reviewed. Prefer safe Rust.
- `#![warn(missing_docs)]` on the four library crates (`rift`, `rift-types`,
  `rift-ytmusic`, `rift-discord`); `rift-desktop` is an internal app crate, excluded from
  the rustdoc gate only (`missing_errors_doc` is workspace-wide).
- `.gitattributes` sets `* text=auto eol=lf`; the tree is LF-normalized. Don't introduce
  CRLF churn.

### 9.2 Comments: explain why

This repository is deliberately comment-dense in a specific way: comments explain reasons,
trade-offs and rejected alternatives ("`panic = "abort"` is deliberately NOT set because…",
"the failure is silent, so the resource is checked explicitly"). Match that:

- Comment the **why**, the failure mode, or the measurement — not the mechanics.
- When you remove/replace a non-obvious decision, record what convinced you in the code or
  the commit body.
- Don't add bare restating comments, and don't strip existing ones to "clean up".

### 9.3 Errors and I/O

- Backend/app boundaries use `anyhow::Result`; `rift-ytmusic` has a typed
  `thiserror`-based error. Keep public `Result` contracts documented (`# Errors`).
- Command errors are user-visible copy: preserve legacy message strings when touching a
  command.
- No blocking I/O on the UI thread. No network in constructors; the one sanctioned blocking
  exception is `RiftBackend::new`'s startup loads (library/settings/downloads) before the
  first frame. Persisted writes go through the durable writer; don't write user files ad-hoc.
- Guard non-finite floats where they enter audio/UI math; a panic on the audio thread
  silences playback.

### 9.4 Dependencies

- Adding a dependency is a reviewed decision, not a reflex. If more than one crate uses it,
  put the **version floor** in `[workspace.dependencies]` and let members list their own
  features (`serde = { workspace = true, features = [...] }`). Single-consumer crates are
  declared directly in that member.
- New deps must pass `cargo deny check all` (license allow-list in `deny.toml` — a new
  license is a deliberate review edit, not a CI surprise), the duplicate-version guard, and
  should not pull a new platform/TLS stack. Prefer std + existing deps.
- The Linux audio device is selected by the Linux-only direct `cpal` dependency in
  `crates/rift/Cargo.toml` (the `pulseaudio` feature only): cpal's PulseAudio host is a
  pure-Rust protocol client, so it adds no libpulse/libpipewire link and no
  libclang/bindgen build dependency. Do not enable cpal's `pipewire` feature — it adds a
  libpipewire runtime link and a libclang build dependency without changing where the
  audio goes.
- Never add: a webview/browser/JS/WASM crate, an external media binary, a second HTTP
  client in the app crate, or a CLI runner alias. (The backend deliberately has two
  clients: the shared metadata/connectivity client and one stricter client dedicated to
  cover images — see §5.3. Do not add a third.)
- `cargo install --locked cargo-deny` if you want the local supply-chain step; the gate
  SKIPs it when absent but CI enforces it.

### 9.5 Documentation

- `docs/*.md` are contracts, not blog posts. A behavior/API/geometry change updates its doc
  **in the same change** (§15 maps doc ↔ code).
- New public items need doc comments; error contracts need `# Errors` sections; a changed
  contract needs the doc's specifics updated, not just a sentence added.
- Historical docs (`docs/innertube-research.md`; `docs/native-streaming.md` §4.1) are marked
  as history — don't present them as current, don't delete them as "wrong".
- Keep `README.md` (user-facing commands/features), `docs/README.md` (index) and this file
  consistent when commands or layout change.

### 9.6 Git, commits and scratch space

- Branch `main`; linear history, no merge-commit style. Commits are
  Conventional-Commit-ish: `<type>(<scope>): <lowercase imperative summary>`, no trailing
  period. Types seen: `feat`, `fix`, `docs`, `test`, `ci`, `chore`, `build`; scopes name the
  touched component (`fix(library):`, `test(ytmusic):`, `ci:`) or are omitted. Twenty older
  commits have no prefix — follow the newer style.
- Bodies explain **why**: the failure mode, the measurement, what was verified and with what
  command. A fixed bug normally comes with a regression test that fails when the defect is
  re-injected. Release commits are `Release vX.Y.Z`.
- Keep changes scoped: source + tests + manifests/lock + the docs describing the changed
  contract belong in one change. Don't reformat other owners' files.
- **Do not commit, stage, push, tag or publish unless the user explicitly asks.** If asked
  to commit, never stage `target*/`, `artifacts/`, `*.log`, `/release-assets/`, `/.probe/`,
  `/scratch/`, `__pycache__/`.
- Scratch space is `artifacts/` (git-ignored): gate logs
  (`artifacts/ci-local-<timestamp>.log`), screenshots (`artifacts/screens/`, comparison
  output `artifacts/compare/`), probe evidence. Use it instead of the repo root; never
  force-add it.
- `target-*` (e.g. with `$env:CARGO_TARGET_DIR = "target-verify"`) is the sanctioned way to test while a
  built binary holds `target/` open.

### 9.7 Multi-agent sessions

- The Lead owns the final diff and runs the acceptance gate; teammates keep write scopes
  disjoint and write research/scratch only under `artifacts/`.
- Read the current file before editing; if a write is rejected as stale, re-read and rebase
  instead of forcing it.
- Report evidence (file:line, test name, command output) rather than assurances; the Lead
  will verify claims independently.

---

## 10. Landmines: cross-cutting invariants

These are the ways a well-intentioned change breaks Rift. Each has a reason and (usually) a
guard; do not "simplify" them away.

1. **Preview data, the `preview` feature and dev flags must never reach a shipped release.**
   `preview` is non-default; the shipped binary rejects dev flags with a `development-only`
   message; CI, release and the local gate assert this. Reason: placeholder data and a dev
   CLI in a user's binary; the failure is silent otherwise.
2. **The toolchain version is stated in three places** (`rust-version`, `rust-toolchain.toml`
   channel, CI `toolchain:` pins). Change all three together or the consistency guard fails.
   CI builds at the MSRV floor on purpose. The local gate also fails if the active `rustc` is
   not the pinned channel, so a `+toolchain`/`RUSTUP_TOOLCHAIN` override cannot pass it.
3. **`--locked` is part of every gate/CI/release build/check/test/clippy/doc/tree command.** A
   dependency edit without a regenerated `Cargo.lock` fails. Commit manifest + lock together
   (§6.1).
4. **The legacy webview must not come back.** Dependency-tree and manifest guards fail if a
   webview/JS/WASM crate (or its brand string) is reintroduced. This is an explicit product
   decision (native rewrite), not a missing capability.
5. **There is no external media binary and no fallback.** Never add yt-dlp/ffmpeg/deezer-
   style shells, installer/detection code, or a runtime switch. Old `<data dir>/bin/yt-dlp*`
   files and a stale `yt_dlp_path` settings key are ignored on purpose — don't delete user
   data to "clean up".
6. **Clean-room rules.** Don't copy GPL implementation code (yt-dlp/rustypipe/etc.) into the
   repo. The innertube requests were derived from observed traffic; keep it that way.
   Dependency licenses are managed in `deny.toml`.
7. **Single instance.** One `rift-music` owns the audio device/library. A second launch exits
   immediately (no focus IPC). Acquire the guard before creating the window/backend/threads;
   `--screenshot` is the exception (it runs before the guard), and the gate's pre-check is
   merely conservative.
8. **User data is sacred.** Never point production code at a test directory, never delete
   user files, keep persisted formats additive (`#[serde(default)]`-friendly), preserve
   `APP_IDENTIFIER` and the data-dir layout, atomic writes + corrupt-file quarantine, and
   never persist/rename half-written files. Tests use `RIFT_DATA_DIR` + temp dirs and
   restore prior values. Offline audio names go through `fetch::safe_stem`; `Downloads::load`
   never renames or deletes a pre-P5 file (it stays readable via `legacy_safe_stem`, §5.3).
9. **The UI thread never blocks.** No network, disk, image decode, `block_on`, or awaiting a
   future on it. Async work goes through the backend runtime, blocking pool, or a worker.
10. **The frame contract and stable ids.** `App::tick()` before `App::frame()`; screens emit
    `Action`s only; actions dispatch after `ui.end_frame()`; cross-frame widget state is
    keyed by stable ids (never frame counters/pointers); `wants_keyboard()` is read before
    `begin_frame`; `end_frame` auto-closes undrawn popups.
11. **Frozen seams.** `crates/rift-desktop/src/gfx/paint.rs` (`PaintList`/`Prim`/`Layer`),
    `ui/input.rs` (`InputState`), the `Renderer` entry points consumed by `app.rs`/`main.rs`,
    and the `BackendHandle`/`BackendEvent` surface consumed by `controller/**`. Changing one
    side requires the other side, its tests and its doc — coordinate instead of editing
    alone.
12. **The event bus is a broadcast, not a reply.** Unbounded channel, drained every frame;
    duplicate-looking `Track`/`Queue` emissions are by design; launch state comes from
    `bootstrap()`; treat events as "state changed to this".
13. **Threading rules.** Construct `RiftBackend` once on the app thread outside any Tokio
    runtime; never hold player/library locks across `.await`; never call `shutdown()` while
    drawing or while a native dialog is open; drop all `BackendHandle` clones before exit or
    the last durable writes can be lost.
14. **Platform gating: every change must keep all three OSes compiling.** Windows-only code
    is `cfg(windows)`-gated and the icon build script must no-op cleanly elsewhere; SMTC
    needs the `HWND` or media keys silently degrade; `HIDE_WINDOW_UNTIL_FIRST_FRAME` is
    Windows-only; release builds are Windows GUI-subsystem (no console).
15. **Assets and shaders are compile-time embedded by path.** Renaming/moving an asset
    breaks the build. Editing GLSL requires regenerating and committing both `.spv` files
    (there is no compile step; use `glslc`/`glslangValidator`, §5.5), and shader vertex
    input names must match `Vertex2D`.
16. **Release tags must equal the workspace version** and the workspace version lives only
    under `[workspace.package]`. The in-app update checker keys off the tag; a mismatch is a
    phantom update forever.
17. **Do not weaken a gate.** No `#[allow(...)]` sprinkles for clippy findings (fix the cause
    or justify narrowly), no deleting entries from `deny.toml`/the duplicate baseline (which
    records exact `<crate> <version>` pairs per host — update all three host lists together
    with the dependency change, never prune one to make the gate pass), no reporting a
    `-Skip*` run as a pass, no disabling CI steps.
18. **Network tests stay out of the default suite.** Fixtures must stay sanitized
    (`example.invalid`, no visitor/signature data); never commit probe output with real URLs
    or ids; never add analytics/telemetry.
19. **The UI-only configuration must keep compiling and testing.** `--no-default-features`
    (inert controller) is built by CI and the gate; a new backend-dependent code path needs a
    `cfg(feature = "backend")`/inert fallback.
20. **Windows file icon is a PE resource** embedded from `assets/icons/rift.rc`, distinct
    from the runtime window icon. If it silently disappears, the app still builds and runs
    but ships the generic icon — that regression happened once; the PE check exists.

---

## 11. Task playbooks

Use these as checklists; each one names the files to touch in order, the invariants that
apply, and how to verify. Cross-references point at the section that explains the contract.

### A. Add or change a backend command
1. Add the inherent method to `BackendHandle` in the owning
   `crates/rift/src/commands/<domain>.rs`; keep it **sync unless it truly awaits network
   I/O**; take short locks, clone the snapshot, emit after the lock.
2. Preserve legacy return types/error strings. Document it (public item + `# Errors`).
3. Update `docs/backend-api.md` §5 (params/returns/errors/blocking) and, if the table
   changes, §7's mapping.
4. If the app should call it: add the `BackendCommands` method (compiler forces all impls),
   the controller mapping, the `Action` if new, and a `ui_flow`/controller test.
5. Verify: `cargo test -p rift -p rift-desktop`, `cargo clippy -p rift -p rift-desktop
   --all-targets`.

### B. Wire a new UI intent end-to-end
1. Add the `Action` variant in `crates/rift-desktop/src/state.rs` (grouped with its peers).
2. `App::dispatch_ui` handles it if UI-local; otherwise the fall-through
   `other => controller.dispatch(other)` already routes it.
3. In the controller module: add the dispatch arm and the `BackendCommands` method
   (`controller/mod.rs` + `controller/commands.rs`); an async command also needs a
   `TaskResult` variant and its `apply_task` arm (`controller/tasks.rs`). Async work
   surfaces through `ControllerEvent` → `App::apply_controller_event`.
4. Emit the action from the screen; add the row to `docs/controller-notes.md` §4.
5. Verify: extend `tests/ui_flow.rs` to assert the recorded call, then
   `cargo test -p rift-desktop --test ui_flow`.

### C. Change a persisted model or data file
1. Edit `crates/rift-types/src/lib.rs` **additively**; decide the default for missing fields
   (`#[serde(default)]`) so old files load.
2. Update the owning store's load/save (`LibraryStore`/`SettingsStore`/`Downloads`), keeping
   `Default` equal to the old missing-field behavior; never add `deny_unknown_fields` to
   settings.
3. Add a reload/corrupt-file test in that store; update `docs/backend-api.md` §4.
4. Verify: `cargo test -p rift -p rift-types`, plus a manual launch against a copy of a real
   data dir if the format changed.

### D. Playback, queue, shuffle or crossfade change
1. Read `crates/rift/src/player/{state,queue,nav,playback}.rs` and `player/tests.rs`.
2. Add the case to `player/tests.rs` first; keep the pure mutations pure.
3. Preserve `generation` invalidation, `pending_next` clearing on every queue/mode edit,
   `remap_history`, auto-skip budget, and the crossfade arming/commit gates (§5.3, "Playback engine (queue, shuffle, crossfade)").
4. Verify: `cargo test -p rift player`, then `cargo test -p rift`; live playback via
   `cargo run -p rift-desktop` or `cargo run -p rift --example smoke`.

### E. Add or rotate an innertube player client (`rift-ytmusic`)
1. Follow the maintenance procedure in `docs/native-streaming.md` §3.1: capture the request,
   prove a **full ranged GET** (whole file `206` + non-zero offset `206` with matching
   bytes) before writing code.
2. Add a `ClientConfig` constant (+ a context builder for client-specific fields) in
   `crates/rift-ytmusic/src/player.rs`; append to `CHAIN`.
3. Extend `the_shipped_chain_is_the_verified_visionos_client` and the loopback chain tests;
   sanitize a recorded response into `tests/fixtures/`; update §3.1's table.
4. Verify with `cargo test -p rift-ytmusic` and the manual `resolve_stream`/`native_probe`
   commands (§5.2). Never add a row on "it resolves" alone.

### F. Debug a stream that fails
1. Classify the error: `Unavailable` (video verdict — the toast has YouTube's reason),
   `NoAudioFormat` (client stopped offering direct URLs — the live risk), `Api{403}` after
   rotation (visitor burned/IP block), `Http`/`Bootstrap` (network, not the video),
   `ResponseTooLarge`.
2. Reproduce one id directly: `cargo run -p rift --example native_probe -- <id>`, confirm
   with `resolve_stream -- <id>`.
3. No fallback exists: `fetch::fetch_track` surfaces exactly that cause. Use
   `RUST_LOG=rift=debug,rift_desktop=debug` when driving the app.

### G. Update a parser after YouTube changes a layout
1. Spot the warning ("N row(s) in the response but none could be parsed — the layout may
   have changed") in `parse.rs`.
2. Capture fresh JSON with `cargo run -p rift-ytmusic --example search_all -- "…"` (writes
   `target/search_{artists,albums}.json`), `browse_dump`, `dump_more`, or `search`.
3. Adjust the known path in `parse.rs` (never `find_all(...).first()`); prune the capture to
   a fixture with `prune_fixture`; extend `tests/fixtures.rs`.
4. Verify: `cargo test -p rift-ytmusic`; keep fixtures sanitized.

### H. Add or change a screen
1. Add the module under `crates/rift-desktop/src/screens/`; expose `height(...)` + `view(...)`
   if routed; wire it in `screens/pages.rs` and the `View` enum in `state.rs` (app-shell
   code — coordinate).
2. Derive every rect from `screens::layout`/`main_content_rect`; scroll only through
   `scroll_area*`; prefix ids; emit `Action`s only.
3. Add a pure geometry/height test; for visuals use an `#[ignore]` PNG test or
   `--screenshot`; if reference geometry changes, update `docs/ui-spec.md` §7 and the pinned
   tests in `screens/mod.rs`.
4. Verify: `cargo test -p rift-desktop`, then capture and compare (§6.3).

### I. Add a widget
1. Add a `pub fn` to `impl Ui` in `src/ui/widgets.rs` (new submodule only if it needs
   retained state).
2. Derive an id with `auto_id`/`auto_id_stable` (or accept an explicit `Id` for a
   `_with_id` variant); call `self.interact(rect, id, Sense::…)`; paint with
   primitives/theme tokens; return a `Response`-like struct.
3. Add a pure test with synthetic `InputState` frames (press frame + release frame) in
   `ui/tests.rs` or `ui/tests_polish.rs`; update `docs/imgui-api.md`'s widget list; don't
   hand-roll the widget in a screen.
4. Verify: `cargo test -p rift-desktop --lib`.

### J. Change window or platform behavior
1. Keep the split: the UI emits `WindowCommand`s; only `main.rs` calls winit.
2. Add a command = variant in `app.rs` + producer arm in `dispatch_ui` + executor arm in
   `main.rs::run_window_command`.
3. Geometry changes route through `window_state::track`/`save_on_exit`; preserve the
   undecorated 1160x740/min 940x620 contract, `HIDE_WINDOW_UNTIL_FIRST_FRAME` rationale and
   `apply_focus_lost` semantics (`docs/architecture.md`, `docs/native-platform.md`).
4. Verify: `cargo test -p rift-desktop` (window-state/single-instance tests), plus a manual
   run or `tools/window_shot.ps1`.

### K. Take a screenshot or add preview data
1. Edit `src/preview.rs` for new placeholder data; add a named capture to
   `App::set_preview_view` in `src/app.rs`; keep both under the dev/test cfg gates.
2. Capture: `cargo run -p rift-desktop -- --preview --screenshot artifacts/screens/<view>.png
   --size 1160x740 --view <view>` or `pwsh tools/screenshot.ps1 -View <view>`.
3. Compare: `python tools/compare_images.py Previews/image.png <png> --label <name>`
   (metrics only, never a hard failure). For download-bar captures use
   `RIFT_PREVIEW_DOWNLOAD=<permille>` with `--view liked`.
4. Verify the release strip still holds (`cargo build --release -p rift-desktop` rejects
   `--preview`).

### L. Change cover art fetching or decoding
1. Transport: backend `fetch_image` only (its dedicated image client,
   `MAX_IMAGE_BYTES = 16 MiB`); don't add another client.
2. Decode: `src/cover.rs` — keep `MAX_SOURCE_EDGE`/`MAX_SOURCE_ALLOC` enforced **before**
   allocation, `MAX_COVER_DIMENSION = 256` downscale (never upscale), on `spawn_blocking`.
3. Caching: `app.rs` owns `MAX_CACHED_COVERS`/texture lifetime; controller owns de-dup
   tracking; presentation is a screens concern (`cards/cover.rs`).
4. Verify: `cargo test -p rift-desktop --lib` (cover/tracker/app tests).

### M. Add or upgrade a dependency
1. Version floor in `[workspace.dependencies]` if shared; otherwise the member manifest.
2. Regenerate `Cargo.lock` (`./tools/ci-local.sh --allow-lock-update` or
   `pwsh tools/ci-local.ps1 -AllowLockUpdate`, or a plain `cargo build`); commit manifest +
   lock together.
3. License must already be allowed in `deny.toml` (add deliberately in review if not);
   check duplicates (`cargo tree -d` → run the guard from Git Bash); then a final full gate
   without `-AllowLockUpdate`.

### N. Bump the version / cut a release
Follow §8.3's checklist: root `[workspace.package] version` → regenerate lock → check
hard-coded version mentions → full gate → annotated tag `vX.Y.Z` matching the version →
push → verify release assets.

### O. Fix a failing gate step
1. Find the step name in `artifacts/ci-local-<timestamp>.log`; it maps 1:1 to a command in
   §8.1/§2.3.
2. Reproduce that command with `--locked`; fix the cause, not the symptom.
3. Re-run the whole gate (later steps can be affected). No gate weakening (§10 item 17).

### P. Update documentation after a change
1. Find the doc in §15; change the specific contract, not just prose.
2. Keep `README.md`, `docs/README.md`, `agents.md` consistent when commands/layout change.
3. If you found a pre-existing doc error, fix it in the same pass and note it in the commit
   body (there is a known-drift list in §13.4).

---

## 12. Debugging and observability

### 12.1 Logs and the startup timeline

- Tracing is used throughout. With no `RUST_LOG`, the default filter is
  `rift=debug,rift_desktop=debug`; override it (PowerShell):
  `$env:RUST_LOG = "rift=debug,rift_desktop=debug"` or `rift=trace` for more.
- Startup emits one `startup:` line per milestone (winit event loop, window, Vulkan
  instance/device, surface/pipeline, swapchain, textures, ring buffers, backend
  construction, fonts, bootstrap), ending in `first frame presented +N ms` and
  `window shown (painted) +N ms`. That timeline is the first place to look for launch
  slowness; `docs/architecture.md#startup-path-and-timing` explains which stage owns which
  cost and why the window is hidden until the first frame on Windows.
- **Release builds are Windows GUI-subsystem** (no console). Redirect stdout/stderr to a
  file to capture logs: `Start-Process -FilePath .\rift-music.exe -NoNewWindow -Wait
  -RedirectStandardOutput out.log -RedirectStandardError err.log` (that is what the gate
  does for the preview-strip and screenshot steps).
- Gate logs and capture logs are under `artifacts/`:
  `artifacts/ci-local-<timestamp>.log`, `artifacts/screens/*.stdout.log` /
  `*.stderr.log`, `artifacts/compare/` (side-by-side/diff/overlay PNGs). Probe evidence for
  the streaming work (git-ignored) is under `artifacts/native-stream/`.

### 12.2 Rendering / GPU

- Debug builds enable the Khronos validation layer when installed; `RIFT_VK_VALIDATION=0`
  opts out, `=1` forces it in release. Validation messages go to stderr; all GPU tests and
  `--screenshot` run under validation in debug builds.
- No Vulkan device means no window (hard requirement, not a fallback). `gfx::tests`
  self-skip with a message when there is no device; `ui::gallery`'s GPU tests need a device
  and must not run concurrently (`GPU_TEST_LOCK`).
- Swapchain `OutOfDate`/`SurfaceLost` are recovered by recreation; **`DeviceLost` is a hard
  error by design** (restart the app). Don't bolt on device-lost recovery as a drive-by.
- `RIFT_GFX_STATS=1` logs per-300-frame frame-time/vertex/draw/texture stats. A visual bug
  that is not a crash is usually a paint-contract/lifetime bug: reproduce as a pure
  `PaintList` test or a headless `screenshot()` instead of eyeballing.
- Editing GLSL without regenerating the committed `.spv` silently renders the old shader —
  there is no build step that catches it.

### 12.3 Backend, audio, network

- Backend failures surface as `BackendEvent::Error`/toasts, not panics; network state is
  tracked by the connectivity probe and the offline-first playback path.
- Do not trust "it should work" for playback/streaming: use the probe examples (§7.4) or
  the ignored `live_backend` tests, and read the error variant first (§11.F).
- An intermittent `http2 error: stream error detected` (or a cold-start playback/metadata
  failure that looks like an outage) is the oversized-header case: `YouTube` occasionally
  adds a ~6.2 KiB `content-security-policy-report-only` header to the music home page,
  pushing the response header list past hyper's 16 KiB h2 default. The innertube clients and
  the backend's shared (metadata/connectivity) client raise the receive limit to 64 KiB
  (`MAX_H2_RESPONSE_HEADER_LIST_BYTES` — a local constant in both crates; Google's
  advertised SETTINGS value); a recurrence means the header block has grown past that. The
  player POST and googlevideo downloads were never affected.
- Audio device problems (no device, no reachable sound server so raw ALSA is in use,
  exclusive-mode conflict) show up in startup/audio logs — the `audio output host:` line
  names the selected backend; on Windows SMTC/media keys require `BackendConfig.media_hwnd`
  and degrade with a log line, not an error.
- Discord presence: run `cargo run -p rift --example discord_replay_probe` with Discord
  running and `RUST_LOG=rift=debug`; the worker logs its pacing/retry decisions.

### 12.4 Common failure modes

| Symptom | Likely cause |
|---|---|
| A second launch exits immediately (window "never appears") | single-instance guard — another `rift-music` is already running (this is by design; no focus IPC). Check Task Manager |
| No window at all on first launch | no Vulkan device/driver, or a renderer error in the startup log (the window is revealed even if the first present fails, so check stderr) |
| `--screenshot` writes no PNG (direct debug/release invocation) | the release binary was built **without** the `preview` feature (dev flags rejected), or headless Vulkan failed. A running app does **not** block a capture — `run_screenshot` happens before the single-instance guard |
| The **gate's** screenshot step fails although the app is not running | a stale `rift-music` process holds the guard — the gate pre-checks and refuses conservatively (`tools/ci-local.sh`/`tools/ci-local.ps1`) |
| Data appears reset / wants to re-download | `RIFT_DATA_DIR` points somewhere else (tests do this), or the data dir could not be resolved/written; check the startup logs and the data directory (§5.3) |
| `cargo` builds with a different Rust | `rust-toolchain.toml` pin; run from the repo through `rustup`. Consistency is checked by the gate |
| Gate fails on formatting/clippy after a clean local build | `--all-targets` covers tests/examples/`build.rs`; the gate uses `-D warnings`, and the pinned toolchain may lint differently from a newer one |
| New dependency fails the gate | missing license in `deny.toml`, duplicate-version baseline, `--locked` lockfile drift, or a new platform/TLS stack (§9.4) |
| Playback silent/wrong on Linux | missing ALSA/Vulkan runtime libs, no audio device, no reachable sound server (raw ALSA fallback in use), or exclusive-mode conflict; the startup `audio output host:` line names the selected backend, and the audio-thread logs show the device/errors |
| Ctrl+V pastes nothing in a text field on Linux/Wayland (e.g. a link copied from the browser into the import box) | the `arboard` dependency lost its `wayland-data-control` feature, so the native clipboard only sees the XWayland clipboard and selections copied by Wayland apps are invisible (the read fails and paste is a silent no-op). Keep the feature in `crates/rift-desktop/Cargo.toml`; `ui::clipboard`'s `wl-clipboard-rs` test fails on a Wayland session when it is dropped |
| Rift missing from the application menu, or the taskbar shows a generic icon / an ungrouped window | the launcher was not installed (`tools/install-linux-desktop.sh`), the desktop cached an old entry (re-login / refresh caches), or an identity drift broke the match — entry basename, `APP_ID`/Wayland `app_id`/`WM_CLASS` and `StartupWMClass` must agree (`docs/native-platform.md` §Linux desktop launcher) |
| Screenshot differs from the reference | compare with `tools/compare_images.py` against `Previews/image.png` (native render; the acceptance screen is expected pixel-identical) or `tools/reference-old-ui.png` (the gate's archived old-UI reference; small deltas expected); inspect the diff/overlay under `artifacts/compare/` |

### 12.5 Debugging knobs worth remembering

| Knob | Use |
|---|---|
| `RUST_BACKTRACE=1` | full backtrace; dev profile keeps line tables without full DWARF |
| `RUST_LOG=rift=trace` | deep backend/network/player tracing |
| `RIFT_VK_VALIDATION=1` | force validation layer in a release build |
| `RIFT_GFX_STATS=1` | frame-time/vertex/draw/texture statistics |
| `CARGO_TARGET_DIR=target-verify` | build/test while a running binary holds `target/` open |
| `./tools/ci-local.sh --skip-release --skip-tests --allow-incomplete` | quick fmt/clippy/docs/toolchain/dupe/deny loop on Linux/macOS (deliberately incomplete; exit 0) |
| `pwsh tools/ci-local.ps1 -SkipRelease -SkipTests -AllowIncomplete` | same quick loop on Windows |
| `pwsh tools/screenshot.ps1 -View <view>` | deterministic UI capture without a window |
| `pwsh tools/window_shot.ps1` | prove the real swapchain path renders on this machine |

---

## 13. Project state, history and known open work

### 13.1 Where the project is (current tree: uncommitted 0.6.1 hardening)

- The current tree is green on Linux under the full 17-row bash gate. With a
  Pillow+numpy Python and cargo-deny 0.20.2 on `PATH`, every applicable row passes, the
  Windows PE-icon row is N/A, and the verdict is `GATE: PASSED`; the lockfile also carries
  the RUSTSEC-2026-0592 fix (`url` 2.5.9) and replaces the yanked `async-executor` 1.14.0
  with 1.14.1, so `cargo deny check all` is clean. Without those two local tools the default
  run is `GATE: INCOMPLETE (2 skipped step(s))` (screenshot comparison and cargo-deny). The
  Windows PE-icon check, the Windows/macOS CI matrix and a real tag push remain release-time
  obligations. Do not claim `GATE: PASSED` for a configuration missing those tools or for
  the Windows-only checks. See `artifacts/reviews/2026-11-22-hardening-pass.md` for the
  earlier hardening pass.
- The 2026-10-19 native rewrite replaced the webview shell with a four-crate workspace; the
  fifth crate (`rift-discord`) arrived with the 2026-10-26 change.
  The 2026-10-26 change shipped native streaming (removed yt-dlp), moved Discord Rich
  Presence into `rift-discord`, added the Windows file icon, and established the shared
  workspace dependencies/lint policy at version 0.6.0.
- The last review pass fixed 119 findings across six scopes. Its notes live in the
  git-ignored `artifacts/reviews/` — working notes/evidence, not contracts.
- The 2026-11-22 hardening pass closed the recorded open items: cached liked `SongIndex`,
  dirty-region glyph-atlas uploads, a `Ui`-scoped generated-texture guard, bounded Discord
  IPC stall recovery, collision-safe/legacy-readable offline filenames, coalesced durable
  writes, transactional launcher publication, and release build-provenance attestations.
  The follow-up independent review then fixed two playback/download races (a queue edit
  during an in-flight crossfade no longer disowns already-started audio; each download claim
  has its own token and `.part` path, so remove-then-retry cannot publish the retry's
  mid-write bytes), made `workflow_dispatch --ref <tag>` draft-only, and propagated the
  lightweight `DownloadProgress` event to the manual probe/live test. It also cleared the
  known doc drift. Evidence: `artifacts/reviews/2026-11-22-*.md` (git-ignored).

### 13.2 Recent history (why the tree looks like this)

| Change | Commit (date) | Why it matters to you |
|---|---|---|
| Native rewrite: webview shell → winit + Vulkano + immediate mode | `e95d46d` (2026-10-19) | Everything under `crates/rift-desktop` is the replacement; legacy-shell concepts in old docs are history |
| Native streaming: yt-dlp → clean-room resolver + ranged downloader | `afe55e2` (2026-10-26) | No external downloader; `docs/native-streaming.md` is the design/verification record; §4.1 there is historical |
| Workspace hardening: shared deps, lint policy, profiles, docs | same commit | Root `Cargo.toml` is the policy source; use `workspace = true` deps |
| Downloader durability fix | `f3f1a64` (2026-10-26) | The worker must wait for/flush the final audio write before publishing; the regression test exists |
| Non-Windows build-script clippy fix | `60c0806` (2026-10-26) | Windows-only build code must compile cleanly everywhere; `--all-targets` lints `build.rs` |

### 13.3 Recorded open work and residual risks

Recorded at the end of the last review (`artifacts/reviews/lead-notes.md`, git-ignored) and
in `docs/native-streaming.md` §7–§8. Treat them as **leads, not facts** — re-verify before
acting; some may already be fixed.

**Highest product risk — streaming resilience.** The native path depends on one JS-less
client family (`VISIONOS`) with no fallback. Adding a second fully-downloadable client is
the highest-value resilience work (`ANDROID_VR` is closest but offset-gated; `IOS` is
SABR-only; TV/web need JS signatures). Procedure: `docs/native-streaming.md` §3.1 —
a candidate counts only after a full ranged-GET proof.

**Recorded follow-ups (streaming/downloads):** *fixed 2026-11-22:* no-progress deadline
(rolling 30 s / 1 KiB byte window), `Downloads::remove` cancellation (tombstone + claim/
`.part` cleanup + pre/post-rename re-check), last-resort non-m4a formats (resolver accepts
only `audio/mp4`), the publish/cancel unit-test gap (`publish_part`), one-off failure
coalescing (one summary per pass), the per-tick `DownloadState` clone (lightweight
`DownloadProgress` now; lifecycle snapshots only), and the `safe_stem` collision/reserved-
name defect (injective `~xx` encoding, case-insensitive uniqueness, reserved-name
neutralization, length cap + `.names.json` index; pre-P5 files stay readable through
`legacy_safe_stem`, never renamed or deleted). Still open: test gaps around empty-
`__Secure-YEC`, oversized player bodies, and mid-download `200`/overrun/`416`; a pre-P5
`_`-folded name is inherently ambiguous without an index entry (accepted for every id that
folds to it, as before the fix), and a corrupt/lost `downloads/.names.json` leaves
capped-name audio on disk but unrecognized (never deleted).

**Recorded follow-ups (app/UI/perf, from the review):** *fixed 2026-11-22:* strict serde
quarantining a partially-corrupt `library.json` (tolerant per-row load + `#[serde(default)]`
models), pause-mid-crossfade tail (the fade clock freezes while paused), duration adoption
(preseeded previous length is no longer published or adopted), cover-cache growth (512-entry
FIFO bound for failed/in-flight URLs), clipboard paste size (64 KiB cap), update-banner
overflow, GPU tests masking renderer regressions as "no device", the titlebar search
debounce (`SEARCH_DEBOUNCE_SECS = 0.3` in `app.rs`), the per-frame liked `SongIndex` rebuild
(cached `Arc<SongIndex>` in `State`, invalidated by `set_library`, self-healing on direct
`library` assignment), the process-wide generated-texture `AtomicBool` (now a `Ui`-owned
one-shot guard, tested with two `Ui`s), the whole-atlas glyph upload (dirty-rect
`TextureUpdate::Region`, one batched fence wait per frame), the Discord IPC stall leak (Unix
250 ms socket deadlines; Windows bounded to 3 abandoned workers), and the launcher's
non-transactional publish (staged + validated + backup/rollback publication). The programmatic text-input focus item is closed too: the inline editors and the
playlist rename field already autofocus with the caret at the end, and a rename started
from the sidebar/Home card now navigates to the playlist view that owns the editor, so
there is always a focused field. The deferred structural splits are done as well:
`controller/` (mod + commands/tasks/cover/tests), `audio.rs` + private `audio/output.rs`
(a thin `run` over an `AudioState` with per-command handlers), and
`ui/text_input/` (mod + edit/state/tests) — all pure moves/extractions with the existing
test suites unchanged.

**Residuals closed in the 2026-11-22 polish pass:** an end that raced a pause now reports
`Stopped` at the end instead of a frozen `Playing` bar; the IME candidate window is anchored
to the focused field's caret (`Ui::ime_cursor_area` → `Window::set_ime_cursor_area`); a
pending debounced search is cancelled when the box loses focus; the seek bar is static with
an empty duration label when the length is unknown (no synthetic 1 s denominator); backend
navigation dismisses an open menu; headless frames advance the frame counter and drain
retired rings; Home no longer adds its bottom padding twice; and `rift-desktop`'s unused
`bytemuck`/`pollster`/`tokio` dependencies are gone.

**Environment-only verification gaps (recorded, not claimed as passed):**

- **Real swapchain driver failure.** The recovery state machine is pure-tested with
  injected failures (`gfx::tests::failed_swapchain_rebuild_recovers_with_a_fresh_swapchain`:
  failed `recreate`, fresh fallback, and no retry of a retired swapchain). A real
  `vkCreateSwapchainKHR` failure cannot be forced locally, and a `cfg(test)`/env injection
  hook would be unexercised surface because there is no headless *windowed*-swapchain
  harness — the residual is driver-specific, so it stays a manual risk rather than dead
  production code.
- **Windows/macOS builds and runtime paths.** Only Linux was built here. The Windows-only
  util test (`a_windows_sharing_violation_is_unreadable_and_refuses_to_overwrite`, real
  `share_mode(0)`) and the Windows named-pipe Discord path compile/run only on the CI
  matrix; proving them locally needs an MSVC C toolchain (aws-lc-sys cannot cross-compile
  here).
- **Release signing/provenance.** `release.yml` now attaches GitHub build-provenance
  attestations (`actions/attest-build-provenance`, `id-token`/`attestations: write`) and
  documents that binaries are unsigned. YAML/structure/SHA-pin checks pass locally, but
  OIDC issuance, attestation upload and `gh attestation verify` need a real `v*` tag push;
  so does the draft → checksums → publish transition.
- **`cargo-deny`** is not installed on this host, so the supply-chain gate remains
  CI-only. The *uncommitted set* does change dependencies (Linux-only `cpal`/`pulseaudio`
  and `arboard`'s `wayland-data-control`, which pulls `wl-clipboard-rs`/`nom 7+8`); licenses
  sampled as MIT/Apache and the duplicate baseline was regenerated, but advisories/bans
  have not been checked locally.

### 13.4 Known documentation drift (verify numbers against source)

As of 2026-11-22 the previously recorded drift is **fixed**: the command accounting
(46 legacy registered → 3 yt-dlp commands removed → 40 preserved + 3 window moved → 42
public methods with `fetch_image`/`is_online`), the progress cadence (~4 Hz / 250 ms),
the quarantine-on-read-error behaviour and `library.json.pre-collapse[.N]` listing in
`docs/backend-api.md` §4, the historical `#[ignore]` command in
`docs/native-streaming.md` §4.1.4, and `docs/imgui-api.md`'s `player_height`/test counts.

Numbers are still the fragile part: test counts, binary sizes and startup timings drift by
design and by machine. **Re-run the command before quoting a number**; never "fix" a
historical verification record (§4.1/§4.2 of `docs/native-streaming.md`) to match today's
counts — those sections are dated evidence, not current prose.

---

## 14. Glossary

| Term | Meaning |
|---|---|
| **innertube** | YouTube's internal JSON API used by its web/mobile clients; Rift uses the anonymous subset for search/browse/player. |
| **WEB_REMIX** | The innertube client identity used for YouTube **Music** metadata (`music.youtube.com`, client name id 67). |
| **visitor data** | Anonymous client identity string scraped from `music.youtube.com`; required by innertube, pooled/banned on 403. |
| **JS-less client** | An innertube client (e.g. `VISIONOS`) whose player response yields directly playable URLs without JS signature/`n` solving. |
| **VISIONOS / CHAIN** | The single currently usable JS-less client and the ordered client table (`player::CHAIN`) the resolver walks. |
| **ranged GET** | HTTP request with a `Range` header; mandatory for fast playback/downloads and for resuming on signed-URL expiry. |
| **itag** | YouTube's numeric format id; `140` = AAC 128 kbps (the historical choice), then 141/139/any audio/mp4. |
| **PO token / BotGuard** | YouTube anti-bot proof-of-origin token; deliberately out of scope. |
| **PaintList** | The UI's per-frame output: ordered layers of `Rect`/`Image` primitives + texture lifecycle, consumed by the renderer (`gfx/paint.rs`). |
| **immediate mode** | UI rebuilt from scratch every frame; widgets paint and hit-test the same rect in one pass; only ids/memory persist. |
| **`Action`** | Message a screen emits (navigate/play/like/…) that the controller applies; the only way the UI expresses intent. |
| **`BackendEvent` / `ControllerEvent`** | Backend→app broadcast events, and the controller's richer per-frame events (async replies, covers, online status). |
| **generation** | Monotonic counter used to drop stale async results (search/pages/play fetch) so an old reply cannot overwrite newer state. |
| **crossfade** | Overlap ramp between two tracks; armed once per generation, capped at `MAX_CROSSFADE = 12 s`, falls back to a hard cut on invalidation. |
| **kept offline / pinned** | A fully downloaded playlist: kept tracks are retried with backoff and re-downloaded when added. |
| **permille** | Progress unit 0..=1000 used between the downloader and the UI progress bar. |
| **safe stem** | `fetch::safe_stem(id)`: the injective `~xx` on-disk filename encoding for offline audio. Pre-P5 `_`-folded names remain readable through `fetch::legacy_safe_stem` and are never renamed at load. |
| **MPRIS / SMTC / MPNowPlayingInfoCenter** | OS now-playing/media-key APIs (Linux / Windows / macOS) via `souvlaki`. |
| **rodio / cpal** | Rust audio playback/decode stack used by the audio thread. |
| **vulkano / winit** | Vulkan wrapper and window/event-loop crate used by `rift-desktop`. |
| **sRGB / linear / premultiplied** | Colour path: UI colours are sRGB straight-alpha; the renderer converts to linear, blends premultiplied `(One, OneMinusSrcAlpha)`, and lets the sRGB target encode. |
| **preview mode** | Deterministic placeholder-data mode for UI work/screenshots; dev-only, stripped from shipped releases (`preview` feature / debug builds). |
| **MSRV** | Minimum supported Rust version — here the pinned 1.88.0 floor. |
| **gate** | The local CI equivalent (`tools/ci-local.sh` on Linux/macOS, `pwsh tools/ci-local.ps1` on Windows), plus the GitHub Actions jobs it mirrors. |
| **data directory** | OS app-data location for `library.json`, `settings.json`, `playback.json`, downloads, window state; overridable with `RIFT_DATA_DIR`. |
| **durable writer** | The coalescing background writer that atomically publishes `library.json`/`settings.json` (newest snapshot per path; FIFO one thread). |
| **quarantine** | Renaming a corrupt JSON file to `*.json.bad[.N]` and loading defaults instead of crashing. |

---

## 15. Documentation index and reading order

`docs/*.md` are the authoritative contracts; this file is the map. Read the doc for your
area before editing, and update it in the same change as the code.

| Document | Covers |
|---|---|
| [`README.md`](README.md) | Product, requirements, build/run, dev commands, probes, architecture summary, licence |
| [`docs/README.md`](docs/README.md) | Documentation index / which doc is authoritative for what |
| [`docs/architecture.md`](docs/architecture.md) | Workspace/module layout, render model, paint and renderer contracts, UI frame contract, windowing, icons, preview/screenshot, startup timing |
| [`docs/backend-api.md`](docs/backend-api.md) | `RiftBackend` construction, every command (params/returns/errors/blocking), `BackendEvent`s, data directory, legacy command mapping, known differences |
| [`docs/native-streaming.md`](docs/native-streaming.md) | Clean-room streaming design: validated request, Range proof, client table/chain, downloader, progress contract, clean-room policy, test strategy, risks/backlog. **Authoritative for streaming**; §4.1 is a historical record |
| [`docs/controller-notes.md`](docs/controller-notes.md) | UI↔backend bridge: threading, full Action→command table, stale generations, `TrackCache`, cover pipeline, event mapping, online status, tests |
| [`docs/imgui-api.md`](docs/imgui-api.md) | Immediate-mode GUI API: frame flow, ids, layout, text, widgets, text input, scrolling, popups, focus, theme, screen conventions, tests |
| [`docs/ui-spec.md`](docs/ui-spec.md) | Screen-by-screen visual/behavioural spec: tokens, geometry, states, interactions, acceptance target |
| [`docs/native-platform.md`](docs/native-platform.md) | Window-state persistence, the single-instance guard, and the Linux desktop launcher (entry/identity contract, installer, icon theme, guard tests) |
| [`docs/innertube-research.md`](docs/innertube-research.md) | **Historical** research that led to the innertube client (pre-native; its "keep yt-dlp" verdict is superseded). Reference only — never current design |
| [`LICENSE`](LICENSE) | Apache-2.0 |

Recommended reading order by task:

- **Anything at all:** this file §0–§4.
- **Backend/playback/library/downloads:** §5.3 → `docs/backend-api.md` → source.
- **Streaming/parsing:** §5.2 → `docs/native-streaming.md` → `rift-ytmusic/src/**`.
- **App shell/controller/window:** §5.4 → `docs/architecture.md` + `docs/controller-notes.md`.
- **UI/gfx/screens:** §5.5 → `docs/imgui-api.md` + `docs/ui-spec.md`.
- **Build/CI/tests/deps:** §6–§8 → `Cargo.toml`, `tools/ci-local.{sh,ps1}`, `.github/workflows/*`.
- **Git/process conventions:** §9 → `git log` for current style.
