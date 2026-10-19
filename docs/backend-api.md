# Backend API — `rift` crate (native backend library)

Status: **published contract for the app crate** (see
[`architecture.md`](architecture.md)). It is a framework-agnostic **library**:
no window, no webview, no IPC, no winit, no Vulkano. The desktop app
(`crates/rift-desktop/`, package `rift-desktop`) owns those and drives this
crate through:

1. one constructor, `RiftBackend::new`,
2. one event channel of `BackendEvent` values, drained per frame,
3. backend commands as plain methods on `BackendHandle`
   (reachable directly on `RiftBackend` through `Deref`).

The crate builds with `cargo check -p rift`; its test suites cover the player,
library, downloads, settings, util, sha256 and fetch paths; `cargo tree -p rift`
has no desktop-shell framework crates; `examples/probe.rs` compiles. A complete
`grep`-able mapping of the old command names is at the end.

---

## 1. Constructing the backend

```rust
pub struct RiftBackend { /* owns a dedicated Tokio runtime + shared state */ }
// not Clone: it owns the runtime. Use `handle()` for a cloneable task handle.

impl RiftBackend {
    pub fn new(config: BackendConfig)
        -> Result<(RiftBackend, BackendEvents), BackendError>;

    pub fn handle(&self) -> BackendHandle;
    pub fn runtime_handle(&self) -> tokio::runtime::Handle;
    pub fn spawn<F>(&self, future: F) -> tokio::task::JoinHandle<F::Output>
        where F: Future + Send + 'static, F::Output: Send + 'static;
    pub fn shutdown(self);
}
```

`RiftBackend::new` runs the whole former shell's `setup` hook:

* resolves and creates the data directory (see §4),
* loads `settings.json`, `library.json`, the restored playback queue,
  offline-download state,
* applies the persisted yt-dlp override,
* spawns the audio thread, the OS media thread and the Discord RPC thread,
* builds the shared `reqwest::Client` (5 s connect / 30 s total timeout),
* creates the backend's own multi-thread Tokio runtime (2 workers),
* spawns the audio-event pump (progress ticker, end-of-track advance,
  crossfade arming),
* loads zero bytes from the network — **startup works offline**.

Returns the owning value plus the event-bus receiver. The application must
**drain the receiver** or the bus grows without bound (§3).

### `BackendConfig`

```rust
pub struct BackendConfig {
    /// Override the data dir. None → RIFT_DATA_DIR if set → platform default.
    pub data_dir: Option<std::path::PathBuf>,
    /// Native window handle for the OS media session, as an integer.
    /// Windows only (SMTC needs an HWND); ignored on Linux/macOS.
    pub media_hwnd: Option<isize>,
}
impl Default for BackendConfig { /* both None */ }
impl BackendConfig {
    pub fn new() -> Self;
    pub fn with_data_dir(self, dir: impl Into<PathBuf>) -> Self;
    pub fn with_media_hwnd(self, hwnd: isize) -> Self;
}
```

* `data_dir` is for tests, portable installs and preview runs; the shipped app
  leaves it `None`.
* `media_hwnd`: **pass the winit window's Win32 `HWND` on Windows**
  (`window.window_handle()` → `RawWindowHandle::Win32(h)` → `h.hwnd as isize`).
  `souvlaki`'s SMTC backend panics without one, so the backend degrades instead:
  with `None` on Windows it logs a warning and runs without OS media keys /
  now-playing metadata. Headless/preview runs pass `None` and lose only those
  features. This is the only "window" coupling the backend has.

### `BackendError`

```rust
pub enum BackendError {
    DataDir(String),                                     // no directory could be resolved
    CreateDataDir { path: PathBuf, source: io::Error },  // exists in name but unwritable
    Init(String),                                        // client/runtime construction failed
}
```

Implements `Display + std::error::Error + From<paths::DataDirError>`.
There is no network error at construction: a missing network only shows up later
as `BackendEvent::Error` from playback/metadata commands.

### Ownership / threading rules

* Construct **once**, on the application thread, while the app is starting.
  It must not be called from inside a Tokio runtime context (use the window's
  `resumed` hook or `main` before the event loop).
* `RiftBackend` owns the runtime; **clone `BackendHandle`s, never the owner**.
  Tasks run on the backend runtime, so a task must not be the last owner of the
  runtime (a runtime cannot be dropped from inside itself).
* `RiftBackend: Deref<Target = BackendHandle>`: `backend.search(..)` works
  directly, and `backend.handle()` gives a `Clone + Send + Sync` handle for
  `tokio` tasks.
* `RiftBackend::shutdown(self)` (or dropping it as the last owner) flushes the
  queued durable writes (bounded wait), stops the audio, media and Discord
  threads, then shuts the runtime down. Call it on the application thread from
  the exit path. If a native file dialog is still open on the blocking pool,
  shutdown waits for it (documented loss; see §9).

### Where commands run

| Thread | Owner | What runs there |
|---|---|---|
| application/UI thread | `rift-desktop` | sync command methods (cheap), `bootstrap()`, event drain |
| backend runtime (2 workers, `rift-backend`) | `RiftBackend` | `spawn`ed futures (search, playback tasks, download jobs, event pump, `spawn_blocking` pool) |
| `rift-audio` | backend | rodio output device + progress ticker |
| `rift-media` | backend | souvlaki media keys / now-playing |
| `rift-discord` | backend | Discord Rich Presence IPC |
| `rift-persist` | backend | background playback-snapshot writes |
| `rift-durable` | backend | queued durable `library.json`/`settings.json` writes + shutdown flush |

---

## 2. `BackendHandle` — the command surface

```rust
#[derive(Clone)]            // Send + Sync
pub struct BackendHandle { /* Arc<shared state> */ }
```

All command methods are on `BackendHandle`; `RiftBackend` derefs to it.
Prefer `backend.handle()` (or `backend.handle().clone()`) when the call site is
inside a spawned future.

### Accessors (read-only snapshots, no event race)

| Method | Returns |
|---|---|
| `fn data_dir(&self) -> &Path` | data directory in use |
| `fn runtime_handle(&self) -> tokio::runtime::Handle` | backend runtime (for `tokio::spawn`/`spawn_blocking`) |
| `fn spawn<F>(&self, f: F) -> JoinHandle<F::Output>` | run a future on the backend runtime |
| `fn spawn_blocking<F, R>(&self, f: F) -> JoinHandle<R>` | run a blocking closure (e.g. own dialogs) off the UI thread |
| `fn preview_mode(&self) -> bool` | `RIFT_PREVIEW` debug mode |
| `fn library(&self) -> rift_types::Library` | likes + playlists + recents |
| `fn downloads_state(&self) -> rift_types::DownloadState` | offline download status |
| `fn queue_snapshot(&self) -> rift_types::QueueSnapshot` | queue/current/shuffle/repeat/source |
| `fn playback_state(&self) -> rift_types::PlaybackState` | stopped/loading/playing/paused |
| `fn volume(&self) -> f32` | 0.0..=1.0 |

`bootstrap()` (§5) is the one-shot launch snapshot that makes a separate race-free
initial read unnecessary.

---

## 3. Event bus

```rust
pub struct BackendEvents;   // Send, not Sync. The app owns exactly one.
impl BackendEvents {
    pub fn try_recv(&mut self) -> Option<BackendEvent>;      // non-blocking, UI thread safe
    pub fn drain(&mut self) -> Vec<BackendEvent>;            // everything queued, oldest first
    pub fn blocking_recv(&mut self) -> Option<BackendEvent>; // None = backend gone
    pub async fn recv(&mut self) -> Option<BackendEvent>;
    pub fn is_closed(&self) -> bool;
}
```

* Tokio `unbounded` mpsc underneath. Producers never block; the UI must drain
  **every frame** (`while let Some(ev) = events.try_recv() { state.apply(ev) }`)
  or a long-lived play session grows the queue. At 5 progress events/s this is
  trivial for a 60 fps drain.
* Ordering is FIFO per producer; there is no cross-producer total order (audio
  pump vs command threads), so treat the bus as *broadcast state changes*, not
  as a transactional log. The launch snapshot comes from `bootstrap()`, not from
  replaying events.
* When the backend is dropped the channel closes: `try_recv` → `None`,
  `blocking_recv`/`recv` → `None`. Producers ignore a closed bus (shutdown must
  not panic).
* Progress ticks are produced by the audio thread at ~5 Hz while playing and by
  `seek`/track start, so the seek bar keeps moving even when nothing else is
  happening.

### `BackendEvent` variants (payloads are the old `rift://…` payloads 1:1)

```rust
pub enum BackendEvent {
    Track(rift_types::Track),                      // rift://track
    State(rift_types::PlaybackState),              // rift://state
    Queue(rift_types::QueueSnapshot),              // rift://queue
    Progress(rift_types::Progress),                // rift://progress
    Library(rift_types::Library),                  // rift://library
    Downloads(rift_types::DownloadState),          // rift://downloads
    Error(String),                                 // rift://error
    Notice(String),                                // rift://notice
    OpenPlaylist(String),                          // rift://open_playlist (playlist id)
}
impl BackendEvent { pub fn name(&self) -> &'static str; }  // the old event name
```

`BackendEvent` is `Debug + Clone + PartialEq`. Emission points:

| Event | Emitted when |
|---|---|
| `Track` | playback starts; crossfade commits; credit enrichment updates the current row; player starts a new track |
| `State` | play/pause/stop/loading transitions, seek does not change state |
| `Queue` | any queue/current/shuffle/repeat change; also on a track's first play (twice is normal: once for the change, once after credit enrichment) |
| `Progress` | audio-thread position tick (~5 Hz), decoder duration correction, track start, crossfade commit, `seek` |
| `Library` | like/unlike, playlist create/import/delete/rename/move/add/remove, track backfill + recents on playback |
| `Downloads` | download claimed/finished/failed/removed, including each retry pass |
| `Error` | playback failure, download failure/give-up, import parse failure, export write failure, yt-dlp install failure |
| `Notice` | playlist exported, playlist imported (with count) |
| `OpenPlaylist` | after a backend-driven playlist import (jump to the new playlist) |

Drain example:

```rust
fn pump_events(state: &mut AppState, events: &mut rift::BackendEvents) {
    for event in events.drain() {
        match event {
            rift::BackendEvent::Track(track) => state.now_playing = Some(track),
            rift::BackendEvent::State(s) => state.playback = s,
            rift::BackendEvent::Queue(q) => state.queue = q,
            rift::BackendEvent::Progress(p) => state.progress = p,
            rift::BackendEvent::Library(lib) => state.library = lib,
            rift::BackendEvent::Downloads(d) => state.downloads = d,
            rift::BackendEvent::Error(msg) | rift::BackendEvent::Notice(msg) => {
                state.toasts.push(msg)
            }
            rift::BackendEvent::OpenPlaylist(id) => state.navigate_playlist(id),
        }
    }
}
```

---

## 4. Data directory

```rust
pub const APP_IDENTIFIER: &str = "dev.jerimiah.rift";
pub const DATA_DIR_ENV: &str = "RIFT_DATA_DIR";
pub fn native_data_dir() -> Result<PathBuf, DataDirError>;
```

Precedence: `BackendConfig::data_dir` → `RIFT_DATA_DIR` (non-empty) → platform
data directory + `APP_IDENTIFIER`. This matches the previous shell's
`app.path().app_data_dir()` exactly, so existing installs keep their data:

| Platform | Directory |
|---|---|
| Windows | `%APPDATA%\dev.jerimiah.rift` |
| Linux | `$XDG_DATA_HOME/dev.jerimiah.rift` (default `~/.local/share/dev.jerimiah.rift`) |
| macOS | `~/Library/Application Support/dev.jerimiah.rift` |

Files written there (all pre-existing formats, unchanged):

| Path | Contents |
|---|---|
| `library.json` | likes, playlists, recently played (`rift_types::Library`) |
| `settings.json` | volume, discord_rpc, crossfade, yt_dlp_path, update_notifications |
| `playback.json` | last queue snapshot (restored stopped on next launch) |
| `downloads/<id>.m4a` | offline audio (presence == downloaded) |
| `bin/yt-dlp[.exe]` | one-click yt-dlp install target |
| `*.json.bad` | backup of a corrupt state file (never overwritten, see `util::load_json`) |

`library.json` and `settings.json` are **durable** and written off the caller's
thread: a mutation queues a snapshot to a `util::DurableWriter` background
thread (`rift-durable`), which serializes it and replaces the file with an
atomic temp-file + rename in FIFO order — rapid mutations land in order, the
last write wins, and a sync command called on the UI thread never blocks on
disk. `RiftBackend::shutdown`/`Drop` flushes that queue (bounded wait, logged if
it gives up) before the runtime and threads stop. Startup **loads** remain
synchronous, and the one-time duplicate-song repair's pre-repair backup copy is
a synchronous startup-only `std::fs::copy`. Failure semantics are unchanged
from the old synchronous path: a failed write is logged, not retried, and
`load_json` still moves a corrupt file aside to `*.json.bad`.

Preview mode (`RIFT_PREVIEW=1` in a debug build) never persists settings and
skips network commands; the app renders placeholder data instead of calling the
backend.

---

## 5. Command methods

All former legacy IPC command functions are now methods with the **same
parameters and return types**, minus the injected `app: AppHandle` and
`state: State<AppState>` arguments. Async status is preserved: an `async fn` was
an async command and still must be driven from a task; a sync method was a
blocking command and is safe to call directly from the UI/controller thread
(they take short locks, and do neither network I/O nor disk I/O: library and
settings writes are queued to the durable writer, see §4).

Notation: `async` = returns a future, must be awaited inside a spawned task.
"Network" = performs network I/O (or spawns a subprocess that does). Errors are
`String` (human-readable, exactly as the old commands returned them).

### Startup

| Method | Signature | Notes |
|---|---|---|
| `bootstrap` | `fn bootstrap(&self) -> rift_types::Bootstrap` | synchronous full launch snapshot: library, queue, state, volume, current track, progress, downloads, discord_rpc, crossfade, yt_dlp_path, update_notifications, preview. Use before the first frame; no event race. |

`rift_types::Bootstrap` is unchanged from the old frontend contract.

### Search / catalog

| Method | Signature | Notes |
|---|---|---|
| `search` | `async fn search(&self, query: String) -> Result<Vec<Track>, String>` | network; audio-only filter applied |
| `search_artists` | `async fn search_artists(&self, query: String) -> Result<Vec<ArtistSummary>, String>` | network |
| `search_albums` | `async fn search_albums(&self, query: String) -> Result<Vec<AlbumSummary>, String>` | network |
| `get_artist` | `async fn get_artist(&self, id: String) -> Result<ArtistPage, String>` | network; audio-only tracks |
| `get_artist_songs` | `async fn get_artist_songs(&self, id: String) -> Result<Vec<Track>, String>` | network; up to 1000 tracks + bounded credit lookups; can take seconds |
| `get_album` | `async fn get_album(&self, id: String) -> Result<AlbumPage, String>` | network; audio-only tracks |

Errors: `"search failed: …"`, `"could not load artist: …"`,
`"artist has no songs list"`, etc. (old strings preserved).

### Playback / transport / settings

| Method | Signature | Notes |
|---|---|---|
| `play_tracks` | `async fn play_tracks(&self, tracks: Vec<Track>, start: usize, source: Option<String>) -> Result<(), String>` | validates `start`; `Err("nothing to play")`; returns before the stream is fetched (fetch happens in a spawned task); preview mode = no-op |
| `play_track` | `async fn play_track(&self, track: Track) -> Result<(), String>` | replaces the queue with one track; preview mode = no-op |
| `toggle_play` | `fn toggle_play(&self)` | play/pause/resume/cancel-loading |
| `next_track` | `fn next_track(&self)` | manual Next (wraps, overrides repeat-one) |
| `prev_track` | `fn prev_track(&self)` | restarts current when near the start |
| `seek` | `fn seek(&self, position: f64)` | no-op unless playing/paused; clamps to duration; emits `Progress` immediately |
| `set_volume` | `fn set_volume(&self, volume: f32)` | live change, no disk write (clamped 0..=1) |
| `save_volume` | `fn save_volume(&self, volume: f32)` | persists; call on slider release only |
| `set_discord_rpc` | `fn set_discord_rpc(&self, enabled: bool)` | persists + toggles RPC thread |
| `set_crossfade` | `fn set_crossfade(&self, secs: f32)` | clamps to `0..=MAX_CROSSFADE (12.0)`, persists, next transition |
| `toggle_shuffle` | `fn toggle_shuffle(&self)` | emits `Queue` |
| `cycle_repeat` | `fn cycle_repeat(&self)` | Off → All → One; emits `Queue` |

### Queue

| Method | Signature | Notes |
|---|---|---|
| `queue_add` | `fn queue_add(&self, track: Track)` | appends (dedup by id); starts playback if idle |
| `queue_play_next` | `fn queue_play_next(&self, tracks: Vec<Track>)` | insert after current; starts playback if idle |
| `queue_move` | `fn queue_move(&self, from: usize, to: usize)` | reorder; emits `Queue` only if it moved |
| `queue_remove` | `fn queue_remove(&self, index: usize)` | removes and continues playback appropriately |
| `queue_jump` | `fn queue_jump(&self, index: usize)` | start the indexed track |
| `queue_clear` | `fn queue_clear(&self)` | clears + stops; emits `Queue` |

Malformed indices are ignored / clamped exactly as before.

### Library / playlists

| Method | Signature | Notes |
|---|---|---|
| `toggle_like` | `fn toggle_like(&self, track: Track)` | emits `Library` |
| `create_playlist` | `fn create_playlist(&self, name: String) -> Result<Playlist, String>` | trims; `Err("playlist name cannot be empty")`; emits `Library` |
| `import_yt_playlist` | `async fn import_yt_playlist(&self, url: String) -> Result<Playlist, String>` | network; accepts URL with `list=` or bare id; errors `"Couldn't find a playlist ID in that link."`, `"That playlist has no playable songs."`; emits `Library` |
| `export_playlist` | `fn export_playlist(&self, id: String)` | **fire-and-forget**: native `rfd` Save dialog + write on the blocking pool; emits `Notice` on success, `Error` on write failure, nothing on cancel |
| `import_playlist` | `fn import_playlist(&self)` | **fire-and-forget**: native `rfd` Open dialog; parses Rift JSON; emits `Library` + `OpenPlaylist` + `Notice` on success, `Error` on bad file, nothing on cancel |
| `delete_playlist` | `fn delete_playlist(&self, id: String)` | emits `Library` |
| `rename_playlist` | `fn rename_playlist(&self, id: String, name: String)` | emits `Library` |
| `move_playlist` | `fn move_playlist(&self, id: String, to: usize)` | emits `Library` only if it moved |
| `add_to_playlist` | `fn add_to_playlist(&self, id: String, track: Track)` | emits `Library`; if the playlist was fully downloaded it also starts an offline download |
| `remove_from_playlist` | `fn remove_from_playlist(&self, id: String, track_id: String)` | emits `Library` |

**Dialog note:** the two file-dialog commands no longer take a shell window or a
callback. They spawn the native dialog on the backend blocking pool and report
through the event bus, so calling them from the UI thread does not block a
frame. `rfd` is used instead of the old dialog plugin; sync `rfd` is the right
API for this because it already runs off the UI thread here (on macOS the sync
API is only legal off the main thread, which this satisfies).

### Offline downloads

| Method | Signature | Notes |
|---|---|---|
| `download_tracks` | `fn download_tracks(&self, tracks: Vec<Track>)` | spawns the sequential background job (yt-dlp, SHA-verified only for the yt-dlp installer; audio files are written temp-file-then-rename); emits `Downloads` per track and `Error` on failures; kept-offline playlists retry with backoff |
| `remove_downloads` | `fn remove_downloads(&self, ids: Vec<String>)` | deletes files, emits `Downloads` |

### yt-dlp setup

| Method | Signature | Notes |
|---|---|---|
| `check_ytdlp` | `async fn check_ytdlp(&self) -> rift_types::YtDlpStatus` | runs `yt-dlp --version` (10 s cap), no panic if missing |
| `set_yt_dlp_path` | `async fn set_yt_dlp_path(&self, path: Option<String>) -> Result<YtDlpStatus, String>` | `None`/blank clears; validates + canonicalizes; on invalid path returns `Err` and keeps the stored value |
| `download_ytdlp` | `async fn download_ytdlp(&self) -> Result<YtDlpStatus, String>` | network; downloads into `<data_dir>/bin`, verifies the release SHA-256 **before** writing, persists the path; emits `Error` on failure |

### Updates

| Method | Signature | Notes |
|---|---|---|
| `check_update` | `async fn check_update(&self) -> Result<rift_types::UpdateStatus, String>` | network; degrades to "latest unknown" instead of erroring; version from `env!("CARGO_PKG_VERSION")` = workspace version |
| `set_update_notifications` | `fn set_update_notifications(&self, enabled: bool)` | persists |
| `open_url` | `fn open_url(&self, url: String) -> Result<(), String>` | only `http(s)://` (anything else is `Err`); `explorer.exe` on Windows (with a `cmd /C start` fallback), `xdg-open` on Linux, `open` on macOS |

### Cover art / images (new: not a former command)

The app renders YouTube thumbnails, and it must not create a second HTTP client
(second TLS stack / connection pool / timeout policy). The backend's shared
client is exposed for exactly this one bounded GET:

| Method | Signature | Notes |
|---|---|---|
| `fetch_image` | `async fn fetch_image(&self, url: String) -> Result<Vec<u8>, String>` | **network**; one GET through the backend's shared `reqwest::Client` (5 s connect / 30 s total timeout). Returns raw encoded image bytes (PNG/JPEG/WebP as served — caller decodes with its `image` crate). Only absolute `http(s)` URLs; body capped at `rift::MAX_IMAGE_BYTES` (16 MiB), enforced against both the declared length and the actual chunks. Errors: `"refusing to fetch a non-http(s) image url"`, `"image fetch failed: …"`, `"image too large …"`. Spawn it from a task; never block a frame on it. |

Use it as the single cover-art path:

```rust
let backend = backend.handle();
backend.spawn(async move {
    match backend.fetch_image(track.cover.clone()).await {
        Ok(bytes) => { /* decode + upload as a texture */ }
        Err(e) => tracing::debug!("cover art: {e}"), // a missing cover is non-fatal
    }
});
```

### Connectivity (new: not a former command)

The old webview had `online`/`offline` listeners; the native app has none, so the
backend exposes the same reachability rule its download-retry path uses:

| Method | Signature | Notes |
|---|---|---|
| `is_online` | `async fn is_online(&self) -> bool` | **network**; `HEAD https://www.youtube.com/` with a 2 s cap on the shared client. `true` when the request reached a `youtube.com` host (any HTTP status counts — a 403/429 while rate-limited is still online); `false` only on transport failure or a non-YouTube host (captive portal / transparent proxy). Emits no events; the controller polls it (~every 10 s) from a task and owns `State::online`. |

```rust
let backend = backend.handle();
backend.spawn(async move {
    let online = backend.is_online().await;
    let _ = controller_tx.send(ControllerEvent::Online(online));
});
```

---

## 6. How the app crate should drive it

```rust
use rift::{BackendConfig, BackendEvent, BackendEvents, RiftBackend};

struct Controller {
    backend: RiftBackend,
    events: BackendEvents,
}

impl Controller {
    /// Called once from `main`/`resumed`, after the winit window exists.
    fn start(window: &winit::window::Window) -> anyhow::Result<Self> {
        // Windows SMTC needs the HWND; everywhere else this is ignored.
        let media_hwnd = hwnd_of(window); // Option<isize>
        let (backend, events) = RiftBackend::new(BackendConfig {
            data_dir: None,
            media_hwnd,
        })?;
        let mut controller = Self { backend, events };
        // Optional: assert the restored state matches what the first frame shows.
        let _bootstrap = controller.backend.bootstrap();
        Ok(controller)
    }

    /// Once per frame, before laying out the UI.
    fn pump_events(&mut self, state: &mut AppState) {
        for event in self.events.drain() {
            match event {
                BackendEvent::Progress(p) => state.progress = p,
                BackendEvent::State(s) => state.playback = s,
                BackendEvent::Queue(q) => state.queue = q,
                BackendEvent::Track(t) => state.now_playing = Some(t),
                BackendEvent::Library(l) => state.library = l,
                BackendEvent::Downloads(d) => state.downloads = d,
                BackendEvent::Error(m) | BackendEvent::Notice(m) => state.toast(m),
                BackendEvent::OpenPlaylist(id) => state.navigate_playlist(id),
            }
        }
    }

    /// Called after `ui.end_frame()` with the frame's actions.
    fn dispatch(&mut self, action: Action) {
        match action {
            // Sync methods: call directly on the UI thread.
            Action::TogglePlay => self.backend.toggle_play(),
            Action::Next => self.backend.next_track(),
            Action::Seek(seconds) => self.backend.seek(seconds),
            Action::SetVolume(v) => self.backend.set_volume(v),
            Action::SaveVolume(v) => self.backend.save_volume(v),
            Action::PlayTracks { tracks, start, source } => {
                // Async methods: hand to the backend runtime; the result comes
                // back through the event bus or a UI-owned channel.
                let backend = self.backend.handle();
                self.backend.spawn(async move {
                    if let Err(e) = backend.play_tracks(tracks, start, source).await {
                        tracing::warn!("play_tracks: {e}");
                    }
                });
            }
            Action::Search(query) => {
                let backend = self.backend.handle();
                let tx = self.search_results_tx.clone(); // app-owned channel
                self.backend.spawn(async move {
                    let result = backend.search(query).await;
                    let _ = tx.send(result);
                });
            }
            Action::ExportPlaylist(id) => self.backend.export_playlist(id),
            Action::ImportPlaylist => self.backend.import_playlist(),
            Action::OpenUrl(url) => { let _ = self.backend.open_url(url); }
            // Window controls are winit's job now (see §9).
            Action::Minimize | Action::ToggleMaximize | Action::Close => { /* winit */ }
        }
    }

    /// On the application exit path, on the UI thread.
    fn shutdown(self) {
        self.backend.shutdown();
    }
}
```

Ordering rules that matter:

1. `RiftBackend::new` before the first `bootstrap()`/frame; pass `media_hwnd`
   only if the window already exists (it usually does in `resumed`).
2. Drain events **before** drawing; apply sync commands **after**
   `ui.end_frame()` (so a state change is visible next frame — the immediate-mode
   contract in `architecture.md`).
3. Never call an `async` command's future without `.await`ing it inside a task.
   Never `block_on` inside the UI thread or inside the backend runtime.
4. Use `backend.handle()` for the `'static` move into `spawn`; the owner stays
   on the UI thread so the runtime is dropped there.
5. `shutdown()` (consume the owner) on exit; after that don't touch the handle.

A dedicated event-pump thread is possible instead of per-frame draining:
`std::thread::spawn(move || while let Some(ev) = events.blocking_recv() { ... })`
`BackendEvents` is `Send`. The per-frame drain is simpler and sufficient at
5 progress events/s.

---

## 7. Old command → new method mapping (all 46 former commands)

The old backend registered **46** commands in `invoke_handler`/permissions
(the migration brief said 55; the checked-in source and the generated
permission set were both 46). 43 are preserved 1:1 as methods; the 3 window
commands moved to the winit app.

| Old `invoke("…")` | New call | Status |
|---|---|---|
| `search` | `backend.search(query).await` | preserved |
| `search_artists` | `backend.search_artists(query).await` | preserved |
| `search_albums` | `backend.search_albums(query).await` | preserved |
| `get_artist` | `backend.get_artist(id).await` | preserved |
| `get_artist_songs` | `backend.get_artist_songs(id).await` | preserved |
| `get_album` | `backend.get_album(id).await` | preserved |
| `play_tracks` | `backend.play_tracks(tracks, start, source).await` | preserved |
| `play_track` | `backend.play_track(track).await` | preserved |
| `toggle_play` | `backend.toggle_play()` | preserved |
| `next_track` | `backend.next_track()` | preserved |
| `prev_track` | `backend.prev_track()` | preserved |
| `seek` | `backend.seek(position)` | preserved |
| `set_volume` | `backend.set_volume(volume)` | preserved |
| `save_volume` | `backend.save_volume(volume)` | preserved |
| `set_discord_rpc` | `backend.set_discord_rpc(enabled)` | preserved |
| `set_crossfade` | `backend.set_crossfade(secs)` | preserved |
| `toggle_shuffle` | `backend.toggle_shuffle()` | preserved |
| `cycle_repeat` | `backend.cycle_repeat()` | preserved |
| `queue_add` | `backend.queue_add(track)` | preserved |
| `queue_play_next` | `backend.queue_play_next(tracks)` | preserved |
| `queue_move` | `backend.queue_move(from, to)` | preserved |
| `queue_remove` | `backend.queue_remove(index)` | preserved |
| `queue_jump` | `backend.queue_jump(index)` | preserved |
| `queue_clear` | `backend.queue_clear()` | preserved |
| `toggle_like` | `backend.toggle_like(track)` | preserved |
| `create_playlist` | `backend.create_playlist(name)` | preserved |
| `import_yt_playlist` | `backend.import_yt_playlist(url).await` | preserved |
| `export_playlist` | `backend.export_playlist(id)` | preserved (rfd, event-reported) |
| `import_playlist` | `backend.import_playlist()` | preserved (rfd, event-reported) |
| `delete_playlist` | `backend.delete_playlist(id)` | preserved |
| `rename_playlist` | `backend.rename_playlist(id, name)` | preserved |
| `move_playlist` | `backend.move_playlist(id, to)` | preserved |
| `add_to_playlist` | `backend.add_to_playlist(id, track)` | preserved |
| `remove_from_playlist` | `backend.remove_from_playlist(id, track_id)` | preserved |
| `bootstrap` | `backend.bootstrap()` | preserved |
| `download_tracks` | `backend.download_tracks(tracks)` | preserved |
| `remove_downloads` | `backend.remove_downloads(ids)` | preserved |
| `check_ytdlp` | `backend.check_ytdlp().await` | preserved |
| `set_yt_dlp_path` | `backend.set_yt_dlp_path(path).await` | preserved |
| `download_ytdlp` | `backend.download_ytdlp().await` | preserved |
| `check_update` | `backend.check_update().await` | preserved |
| `set_update_notifications` | `backend.set_update_notifications(enabled)` | preserved |
| `open_url` | `backend.open_url(url)` | preserved (native opener) |
| `window_minimize` | — | **moved to app** (`Window::set_minimized`) |
| `window_toggle_maximize` | — | **moved to app** (`Window::maximize`/`unmaximize`) |
| `window_close` | — | **moved to app** (winit exit) |

Everything that previously happened in desktop-shell plugins is now the app's
responsibility and has a native equivalent:

| Legacy shell plugin / runtime feature | Replacement |
|---|---|
| single-instance plugin | app: named mutex / IPC + focus of the existing window |
| window-state plugin | app: save/restore window size/position itself |
| dialog plugin | this crate: `rfd` (playlist import/export) |
| Legacy `app.emit`/event system | this crate: `BackendEvent` channel |
| Legacy `app.path().app_data_dir()` | this crate: `native_data_dir()` (§4) |
| Legacy `async_runtime` | this crate: `BackendHandle::spawn` / backend-owned Tokio runtime |
| Legacy `#[command]` + `invoke_handler` | plain methods (§5); the ACL/capability system is gone by construction |

---

## 8. Behaviour preserved outside the command API

* **Audio thread** — rodio playback, in-memory decode, seek, crossfade, volume,
  progress/end/decoder-failure events, all unchanged.
* **Progress ticker** — still a periodic sender: the audio thread pushes
  `Position` ~5×/s while playing; the event pump turns it into
  `BackendEvent::Progress`. It is not tied to commands or UI activity.
* **Queue/session persistence** — `playback.json` snapshot on every queue
  change, restored stopped at next launch; identical best-effort semantics
  (final change before a crash may not be flushed).
* **OS media keys / now-playing** — preserved without the previous shell. Linux MPRIS, macOS
  MPNowPlayingInfoCenter need no extra input; Windows SMTC needs the window
  `HWND` via `BackendConfig::media_hwnd`. Play/Pause/Toggle, Next, Previous and
  Stop are routed through the same player methods as before; seek-style media
  events are ignored exactly as they were. If the integration cannot start, the
  backend logs and degrades (no panic).
* **Discord Rich Presence** — unchanged thread/coalescing logic; enabled state
  comes from settings and `set_discord_rpc`.
* **yt-dlp resolution** — `RIFT_YTDLP` env override, then PATH, then common
  install dirs; the "not found" case is a normal `found: false` status.
* **ytmusic client / parsers** — `rift-ytmusic` is untouched.
* **`rift::fetch` and `rift::sha256`** — still public modules (the
  `examples/probe.rs` smoke test keeps working).

## 9. Known differences / documented losses

These are deliberate, not silent removals:

1. **Window commands** (`window_minimize`, `window_toggle_maximize`,
   `window_close`) are gone: the app owns the winit window. See
   `architecture.md` §Windowing.
2. **Single-instance and window-state plugins** are not part of the library;
   their behaviour belongs to the app crate's startup path.
3. **`BackendEvents` is unbounded**: a consumer that stops draining will grow
   memory. Drain every frame (or park a thread on `blocking_recv`).
4. **`shutdown()` can wait** for an in-flight blocking task — including a native
   file dialog that is still open — because the owned runtime drains its
   blocking pool on drop. Do not call `shutdown` while a dialog is up; close the
   UI path first.
5. **Windows without `media_hwnd`** loses SMTC media keys / now-playing. Logged
   once at startup; everything else works.
6. **`open_url` is not a generic launcher**: only `http(s)` URLs.
7. **No network at startup** — by design. Network failures surface later as
   `BackendEvent::Error` (e.g. playing a track offline) rather than failing
   construction.
8. **ACL / permissions / capabilities / gen schemas** are deleted with the previous shell;
   `tests/acl_covers_commands.rs` and `tests/ipc_contract.rs` (which validated
   the legacy IPC/ACL contract) are gone. The replacement guard is the 43-method
   mapping table above plus `cargo test -p rift`.

## 10. Verification

Run from the repository root:

```powershell
cargo check -p rift                       # library builds
cargo test -p rift                        # 179 unit tests, all green
cargo check -p rift --examples            # examples/probe.rs + examples/smoke.rs compile
cargo clippy -p rift --all-targets        # clean
cargo tree -p rift   # the tree has no desktop-shell or webview framework crates
```

`cargo run -p rift` no longer exists: this crate in a line-of-business library.
The binary is `cargo run -p rift-desktop` (app crate).

### Live pipeline smoke test (no shell framework, no window, no backend construction)

`cargo run -p rift --example smoke [-- search terms]` proves the full old
streaming path still works from the library alone: YouTube Music search via
`rift-ytmusic`, stream fetch through yt-dlp, and an in-memory rodio decode. It
uses `RIFT_YTDLP` if set, else the app's installed
`<data dir>/bin/yt-dlp[.exe]`, else auto-detection. Observed on 2026-10-18
(Windows 11, network available):

```text
yt-dlp override: %APPDATA%\dev.jerimiah.rift\bin\yt-dlp.exe
yt-dlp detected: found=true path=Some("...\yt-dlp.exe") version=Some("2026.08.19")
search: "Get Lucky (feat. Pharrell Williams and Nile Rodgers)" by Daft Punk, Pharrell Williams, Nile Rodgers (4D7u5KF7SP8)
fetch: 5979549 bytes (5839 KiB), reported duration 0s
decode: ok, decoder duration Some(369.662947845s)
SMOKE PASS: search -> fetch -> decode
```

That is the same search -> fetch -> decode pipeline the player uses, with no
desktop-shell framework crate anywhere in the dependency graph.

### Package / targets

* package `rift`, version from the workspace (`0.5.0`), Rust floor 1.88.
* `[lib] name = "rift"`, path `src/lib.rs`. No binary, no build script.
* Public re-exports include `RiftBackend`, `BackendHandle`, `BackendConfig`,
  `BackendError`, `BackendEvent`, `BackendEvents`, `native_data_dir`,
  `APP_IDENTIFIER`, `DATA_DIR_ENV`, `DataDirError`, `MAX_CROSSFADE`,
  `MAX_IMAGE_BYTES`, `VERSION`, `preview_mode()`, plus the `fetch` and `sha256`
  modules.
* Dependencies: `anyhow`, `directories`, `discord-rich-presence`, `rand`,
  `reqwest`, `rfd`, `rift-types`, `rift-ytmusic`, `rodio`, `serde`,
  `serde_json`, `souvlaki`, `tokio`, `tracing`. (`rfd` is target-split: native
  backends on Windows/macOS, `xdg-portal` on Linux.)
