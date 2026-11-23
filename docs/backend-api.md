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
library, downloads, settings, util, native streaming (resolver-backed range
downloader + progress), and the durable stores; `cargo tree -p rift` has no
desktop-shell framework crates and no external media binary; `examples/probe.rs`
and `examples/native_probe.rs` compile. A complete `grep`-able mapping of the old
command names is at the end.

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
| `rift-durable` | backend | coalesced durable `library.json`/`settings.json` writes + shutdown flush |

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
| `fn downloads_state(&self) -> rift_types::DownloadState` | offline download status, including per-track progress for in-flight downloads |
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
  or a long-lived play session grows the queue. At ~4 progress events/s this is
  trivial for a 60 fps drain.
* Ordering is FIFO per producer; there is no cross-producer total order (audio
  pump vs command threads), so treat the bus as *broadcast state changes*, not
  as a transactional log. The launch snapshot comes from `bootstrap()`, not from
  replaying events.
* When the backend is dropped the channel closes: `try_recv` → `None`,
  `blocking_recv`/`recv` → `None`. Producers ignore a closed bus (shutdown must
  not panic).
* Progress ticks are produced by the audio thread at ~4 Hz (250 ms) while playing and by
  `seek`/track start, so the seek bar keeps moving even when nothing else is
  happening. Every `Position`/`Duration`/`Ended` audio event carries the **play
  identity** (the player generation the audio is playing under). The manager
  discards one whose id is not `core.generation`, and discards `Ended` unless
  playback is still `Playing`: a queued event from a stopped, paused or skipped
  track can neither move the bar, correct the length, arm a crossfade nor
  advance the queue.

### `BackendEvent` variants (payloads are the old `rift://…` payloads 1:1)

```rust
pub enum BackendEvent {
    Track(rift_types::Track),                      // rift://track
    State(rift_types::PlaybackState),              // rift://state
    Queue(rift_types::QueueSnapshot),              // rift://queue
    Progress(rift_types::Progress),                // rift://progress
    Library(rift_types::Library),                  // rift://library
    Downloads(rift_types::DownloadState),          // rift://downloads
    DownloadProgress { id: String, permille: u16 }, // rift://download_progress (new)
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
| `Progress` | audio-thread position tick (~4 Hz / 250 ms), decoder duration correction, track start, crossfade commit, `seek` |
| `Library` | like/unlike, playlist create/import/delete/rename/move/add/remove, track backfill + recents on playback |
| `Downloads` | download claimed/finished/failed/removed, including each retry pass — a whole snapshot, authoritative for the status sets |
| `DownloadProgress` | at most every 100 ms for the actively transferring track; carries only `id` + permille (`0..=1000`), so the UI can update one fraction without cloning `DownloadState` |
| `Error` | playback failure, download failure/give-up, import parse failure, export write failure |
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
            rift::BackendEvent::DownloadProgress { id, permille } => {
                state.downloads.progress.insert(id, permille);
            }
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

Files written there (all pre-existing formats except the additive name index,
which older builds simply ignore):

| Path | Contents |
|---|---|
| `library.json` | likes, playlists, recently played (`rift_types::Library`) |
| `settings.json` | volume, discord_rpc, crossfade, update_notifications |
| `playback.json` | last queue snapshot (restored stopped on next launch) |
| `downloads/<safe stem>.m4a` | offline audio (presence == downloaded); the stem is `fetch::safe_stem(id)`, an injective `~xx` escape over `[a-z0-9_-]` |
| `downloads/.names.json` | `stem -> id` ownership index for names that are not self-identifying (a capped/overlong stem, or a stem containing `_` that a pre-P5 fold could also produce); written atomically and pruned on load |
| `*.json.bad`, `*.json.bad.N` | backup of an unusable (corrupt or unreadable) state file; the first copy keeps `*.json.bad`, later ones get numbered names so no backup is ever overwritten (see `util::load_json_with_parser` / `util::load_json_with_status`) |
| `library.json.pre-collapse`, `library.json.pre-collapse.N` | snapshot of `library.json` taken before a load-time repair (the duplicate-song collapse, or a tolerant load that dropped unreadable rows); the first snapshot keeps the bare name and later repairs get numbered names, so an earlier snapshot is never overwritten |

Offline naming is an explicit on-disk contract. `fetch::safe_stem` keeps
`[a-z0-9_-]` literally and writes every other byte as `~` plus two lowercase hex
digits (`a/b` -> `a~2fb`, `..` -> `~2e~2e`): reversible (`fetch::stem_id`),
case-insensitively unique (`AbC` vs `abc`), free of separators/dots/spaces, and
with Windows device names escaped (`con` -> `~63on`). Stems are capped at 120
characters; a longer id keeps up to a 103-character prefix plus `!` and FNV-1a
hex, mapped back through `downloads/.names.json`. That index also records a
stem containing `_`, because the pre-P5 fold could have produced the same name
for another id; an entry therefore marks an owned name, and `remove` leaves a
`_` name owned by a different id alone. A transfer's temporary file is
`<safe stem>.<claim token>.m4a.part`; `remove` invalidates the token, so a stale
worker cannot publish over or delete the retry's part, and the next startup scan
deletes any `.part` left by a crash. `Downloads::load` additionally keeps
every existing non-empty `.m4a` stem and accepts the pre-P5 `_`-folded name
(`fetch::legacy_safe_stem`, e.g. `a_b.m4a` for `a/b`) as a fallback, so an
upgraded install's files stay playable without being renamed or deleted. A
folded legacy name without an index entry cannot be reversed, so it is accepted
for every id that folds to it; new writes never produce an ambiguous name, and
publishing refuses to replace a non-empty final file it did not create.

`library.json` and `settings.json` are **durable** and written off the caller's
thread: a mutation queues a snapshot to a `util::DurableWriter` background
thread (`rift-durable`), which serializes it and replaces the file with an
atomic temp-file write + flush + rename in FIFO order. Pending snapshots are
**coalesced per path**: a newer save replaces the queued value/serializer in
place (same queue position), so N rapid mutations cost one queued snapshot and
the newest state still wins; the queue holds one pending snapshot per path plus
the in-flight write, and a hard 64-path cap sends an overflow path's write
inline on the caller's thread (logged, never silently dropped). A sync command
called on the UI thread never blocks on disk in the normal path. `flush()`
(and shutdown flush/drop) is the barrier: when it returns true, the newest
snapshot for every write queued before it has been attempted — not that every
save succeeded. The contents are flushed to the storage device before the
rename (a flush failure is logged and the publish still happens), so
`shutdown`/`flush` returning means the *content* is durable rather than only
visible in the page cache. Startup **loads** remain synchronous. The pre-repair
snapshot is also written synchronously, through the same `util::atomic_write`
(temp file in the library's own directory, `sync_all`, rename), so it is
complete before the repair it protects can replace the live file; it is
numbered so a second repair cannot destroy the first snapshot. Failure
semantics: a failed write is logged and not retried.

`library.json` is parsed **tolerantly per row**. The root must be a JSON object
and `liked`, `recently_played` and `playlists` must be arrays when present (a
missing array means empty; an explicit `null` or other type is the wrong type);
otherwise the whole file takes the corrupt/quarantine path below. Each track
row, each playlist and each of a playlist's track rows is then deserialized on
its own (absent fields default, unknown fields are ignored). A row that cannot
be understood — a non-object, a present field of the wrong type, or a row with
neither an id nor a title/name — is dropped with a warning naming the category,
counts and a few ids; every readable row is kept. Dropped rows make the load a
successful, writable load, and the repaired subset is queued for an atomic save
after the numbered `library.json.pre-collapse` snapshot, so the good rows are
not lost on the next launch. A playlist whose own `id`/`name` or `tracks`
container is malformed is dropped as a unit (there is nothing to iterate in a
non-array container); its readable sibling playlists and rows are kept.
`settings.json` has no row structure and keeps the strict parser.

On load, a file that is not parseable (bad JSON or not valid UTF-8) is corrupt:
it is moved aside to `*.json.bad` (numbered if that name is taken), the store
starts from defaults, and it stays writable. A file that **exists but cannot be
read** — a permission problem, or a transient sharing violation from a
backup/AV tool on Windows — is *not* treated as missing: the same move-aside is
attempted first, and if it succeeds the defaults are safe to save because the
original survives as the backup. If the move-aside itself fails, whether for a
read failure or for parse corruption, the file on disk is the user's only copy:
the store becomes read-only for the session (`save` returns without writing) and
an error naming the file is logged. Both unpreserved outcomes are reported
through one `BackendEvent::Error` toast emitted by `RiftBackend::new` once its
event bus exists (both stores load before the bus is created, so the warning is
held until then; library and settings warnings are combined into that single
toast); the parse-corruption one says the corrupt file could not be preserved.

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
(they take short locks and do no network I/O; library and settings writes are
queued to the durable writer, see §4). The one deliberate disk I/O on a sync
command is `remove_downloads`, which tombstones the selected ids and unlinks
their offline files (and any in-flight `.part`) inline: that is an explicit user
delete, the unlinks are cheap and bounded by the selection, and the UI needs the
resulting `Downloads` event for the next frame.
`RiftBackend::shutdown`/`Drop` flushes the durable queue (bounded wait, logged
if it gives up) before the runtime and threads stop.

Notation: `async` = returns a future, must be awaited inside a spawned task.
"Network" = performs network I/O. Errors are
`String` (human-readable, exactly as the old commands returned them).

### Startup

| Method | Signature | Notes |
|---|---|---|
| `bootstrap` | `fn bootstrap(&self) -> rift_types::Bootstrap` | synchronous full launch snapshot: library, queue, state, volume, current track, progress, downloads, discord_rpc, crossfade, update_notifications, preview. Use before the first frame; no event race. |

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
| `seek` | `fn seek(&self, position: f64)` | no-op unless playing/paused; clamps to duration; ignores non-finite positions; emits `Progress` immediately |
| `set_volume` | `fn set_volume(&self, volume: f32)` | live change, no disk write (clamped 0..=1; non-finite becomes a 0.0 mute) |
| `save_volume` | `fn save_volume(&self, volume: f32)` | persists; call on slider release only (clamped 0..=1; non-finite mutes) |
| `set_discord_rpc` | `fn set_discord_rpc(&self, enabled: bool)` | persists + toggles RPC thread |
| `set_crossfade` | `fn set_crossfade(&self, secs: f32)` | clamps to `0..=MAX_CROSSFADE (12.0)`; non-finite disables crossfade; persists, next transition |

**Play identity / atomic request.** `play_tracks`/`play_track` replace the queue,
validate `start`, reset the shuffle anchor, drop a prefetched crossfade pick and
bump the player generation in one core critical section, so two rapid play
commands cannot install one queue and start an index drawn from the other. The
generation is the play identity sent to the audio thread; a crossfade sends the
generation its commit will assign (`anchor + 1`). See
`player/playback.rs::{prepare_play_index, crossfade_play_id}`.
| `toggle_shuffle` | `fn toggle_shuffle(&self)` | emits `Queue` |
| `cycle_repeat` | `fn cycle_repeat(&self)` | Off → All → One → Off; emits `Queue` |

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
| `download_tracks` | `fn download_tracks(&self, tracks: Vec<Track>)` | spawns the sequential background job: native resolved + range-downloaded audio (streamed to a temp `.part` file with a progress callback, then published as `<safe stem>.m4a`); audio files are always published temp-file-then-rename and never over a non-empty file; emits a `Downloads` snapshot on each claim/finish/fail/remove, lightweight `DownloadProgress { id, permille }` at most every 100 ms while bytes move, and one coalesced `Error` per pass for failed one-off tracks (a single failure keeps its exact message; several become a count + first reason); kept-offline playlists retry with backoff |
| `remove_downloads` | `fn remove_downloads(&self, ids: Vec<String>)` | cancels the id: clears its in-flight claim/progress/failed marks, tombstones it (a later `begin`/retry clears the tombstone), and deletes its `<safe stem>.m4a`, any pre-P5 legacy name for the id (unless the name index says that `_` name belongs to another id), the `.part`, and its `downloads/.names.json` entry (the index write is queued to the dedicated name writer; only the unlinks are sync-command disk I/O, see §5 intro; no-op in preview mode). The worker re-checks the tombstone before and after its rename, so a removed transfer publishes nothing and emits no error. Emits `Downloads`. |

`DownloadState` includes `progress: HashMap<String, u16>` — the completed
fraction of each in-flight download in permille (`0..=1000`). Only ids currently
in `downloading` appear; absent means unknown (the track is queued behind the
active transfer), and the desktop app keeps its spinner instead of drawing a bar.
The worker emits at most one snapshot per 100 ms while bytes are moving.

### Audio streaming (native only)

Playback and offline downloads share one path: `fetch::fetch_bytes`
(plus the downloader's file-streaming variant). Resolution is the JS-less
`VISIONOS` innertube client (`rift_ytmusic::player::resolve`), which returns a
direct itag-140 URL; `rift::stream` downloads it with 8 MiB ranged GETs,
resuming at the written offset after a signed-URL expiry. There is no fallback
backend and no environment switch — a native failure surfaces as the fetch/
download error (playback toast, or the download worker's kept-offline retry).

Resolution is a small client table tried in preference order, so a YouTube
client reshuffle is a table row plus a loopback test rather than a new request
path. Today the table has exactly one entry (`VISIONOS`) — the only JS-less
client that returned *fully downloadable* direct URLs in the 2026-10-26 probe
(`ANDROID_VR` resolves but gates transfers after ~1 MiB and the rest are
cipher/SABR-only). `docs/native-streaming.md` §3.1 records the alternatives, the
revisit trigger and the add-a-client procedure; `AudioStream::client` names the
entry that served a stream for logs/probes. Phase 5 removed the yt-dlp path that
used to sit behind this one.

### Updates

| Method | Signature | Notes |
|---|---|---|
| `check_update` | `async fn check_update(&self) -> Result<rift_types::UpdateStatus, String>` | network; degrades to "latest unknown" instead of erroring; version from `env!("CARGO_PKG_VERSION")` = workspace version |
| `set_update_notifications` | `fn set_update_notifications(&self, enabled: bool)` | persists |
| `open_url` | `fn open_url(&self, url: String) -> Result<(), String>` | only well-formed absolute `http(s)` URLs (scheme + non-empty authority, no ASCII whitespace/control characters) — anything else is `Err`; `explorer.exe` on Windows (one argv element, no shell), `xdg-open` on Linux, `open` on macOS |

### Cover art / images (new: not a former command)

The app renders YouTube thumbnails, and it must not build a client per fetch.
The backend owns one **dedicated image client** with the shared 5 s connect /
30 s total timeout but a stricter policy; metadata and connectivity keep the
shared client, so their behaviour is unchanged.

| Method | Signature | Notes |
|---|---|---|
| `fetch_image` | `async fn fetch_image(&self, url: String) -> Result<Vec<u8>, String>` | **network**; one GET through the dedicated image client. Absolute **HTTPS** only; no credentials; no non-default port. Host allow-list by label-boundary suffix: `ytimg.com`, `ggpht.com`, `googleusercontent.com`, `gstatic.com` (the hosts the fixtures/app consume: `i.ytimg.com`, `yt3`/`lh3.googleusercontent.com`, `yt3.ggpht.com`, `www.gstatic.com/youtube/media/ytm/images/artist_avatar…`). Response `Content-Type` must start with `image/`. Redirects are re-validated per hop against the same rule, capped at 3, and never followed off-list. Body capped at `rift::MAX_IMAGE_BYTES` (16 MiB), enforced against both the declared length and the streamed chunks. Errors: `"refusing to fetch an image: …"`, `"image fetch failed: …"`, `"image too large …"`. Spawn it from a task; never block a frame on it. |

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
~4 progress events/s.

---

## 7. Old command → new method mapping (43 legacy commands in the tree)

The old backend registered **46** commands in `invoke_handler`/permissions
(the migration brief said 55; the checked-in source and the generated
permission set were both 46). Native streaming removed three yt-dlp management
commands (`commands/ytdlp.rs`: check / set-path / install), leaving 43: **40**
are preserved 1:1 as methods and the **3** window commands moved to the winit
app. Two methods were added by the native rewrite (`fetch_image`, `is_online`),
so `BackendHandle` exposes **42** public command methods.

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
  `Position` every 250 ms (~4×/s) while playing; the event pump turns it into
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
* **Discord Rich Presence** — the RPC IPC is implemented in-repo by the
  `rift-discord` crate (official RPC protocol: named pipe / Unix socket, v1
  handshake, `SET_ACTIVITY`, clear; no OAuth/voice/chat commands). The transport
  services the protocol's control opcodes instead of treating them as corruption:
  a `PING` from Discord is answered with a `PONG` carrying the same payload,
  `PONG`/unknown opcodes are skipped, a frame carrying another command's nonce is
  skipped instead of being mistaken for the reply, and a `CLOSE` opcode is
  reported with Discord's close code so the caller reconnects. On Unix the
  socket carries a 250 ms read/write timeout, shorter than the backend's 500 ms
  per-command deadline, so a silent peer is an I/O error and the worker thread
  retires/reconnects normally instead of being abandoned. A Windows named pipe
  has no std timeout (an interruptible read needs unsafe FFI, which the crate
  forbids), so the backend bounds that case: at most 3 stalled workers are
  abandoned per session, then presence is disabled until restart (one warning),
  rather than leaking a thread/socket per stall. An activity posted before the cap can
  remain visible until Discord clears it or the app restarts (clearing needs a request). The backend
  keeps its dedicated thread, coalescing and 15 s/60 s pacing; enabled state
  comes from settings and `set_discord_rpc`. A track that restarts (repeat-one,
  replay) is detected as a same-identity `Track`; the presence is cleared, left
  clear briefly so Discord registers a real state change, then re-posted with the
  new `(start, end)` timestamps. The audio event loop also ignores position ticks
  delivered while a new track is still loading: such a tick carries the outgoing
  track's end position and would otherwise re-anchor the new bar at the end of
  the song. Together these make Discord restart the elapsed timer on a loop.
* **ytmusic client / parsers** — `rift-ytmusic` is untouched in behaviour: the
  same innertube requests and parsers. Robustness hardening only: an innertube
  response body is bounded at 32 MiB (declared `Content-Length` early-reject plus
  a streamed byte count) and reports `Error::ResponseTooLarge` beyond it, and a
  403 with a non-JSON body now rotates the visitor id and retries once instead of
  surfacing a parse error.
* **`rift::fetch`** — still a public module (the `examples/probe.rs` smoke test
  and the native probes keep working); it is now the thin native-only wrapper
  over `rift_ytmusic::player` + `rift::stream`. The `sha256` module was removed
  with the yt-dlp installer it served.

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
   the legacy IPC/ACL contract) are gone. The replacement guard is the command
   mapping table above plus `cargo test -p rift`.

## 10. Verification

Run from the repository root:

```powershell
cargo check -p rift                       # library builds
cargo test -p rift                        # all green
cargo check -p rift --examples            # examples/probe.rs + examples/smoke.rs compile
cargo clippy -p rift --all-targets        # clean
cargo tree -p rift   # the tree has no desktop-shell or webview framework crates
```

`cargo run -p rift` no longer exists: this crate in a line-of-business library.
The binary is `cargo run -p rift-desktop` (app crate).

### Live pipeline smoke test (no shell framework, no window, no backend construction)

`cargo run -p rift --example smoke [-- search terms]` proves the full streaming
path still works from the library alone: YouTube Music search via
`rift-ytmusic`, native stream fetch, and an in-memory rodio decode. Observed on
2026-10-18 (Windows 11, network available; predates the native migration — the
smoke output is otherwise unchanged):

```text
search: "Get Lucky (feat. Pharrell Williams and Nile Rodgers)" by Daft Punk, Pharrell Williams, Nile Rodgers (4D7u5KF7SP8)
fetch: 5979549 bytes (5839 KiB), reported duration 0s
decode: ok, decoder duration Some(369.662947845s)
SMOKE PASS: search -> fetch -> decode
```

That is the same search -> fetch -> decode pipeline the player uses, with no
desktop-shell framework crate anywhere in the dependency graph.

`cargo run -p rift --example native_probe [-- <video-id>…]` is the manual native
matrix: it fetches a default sample of real tracks through the playback path
(`fetch::fetch_bytes`), decodes each result with the playback decoder, and prints
bytes/timing/decoded length; it requires every requested id to succeed and pins
the unavailable-video error mapping with one known-invalid id. Run command and
latest results are in `docs/native-streaming.md` §6/§4.1.

`cargo run -p rift --example discord_replay_probe` is the manual Discord Rich
Presence replay check: it plays a short real track ("Me at the zoo", ~19 s) with
repeat-one enabled and lets it end and loop **naturally** — no seek, so the
presence's progress bar is not put at an artificial position. With
`RUST_LOG=rift=debug` the log must contain
`same track restarted; clearing the Discord presence before re-posting` after
the replay, and the Discord elapsed timer must restart near 0:00. It needs
network, an audio device and a running Discord client; audio is muted for the
probe. The `rift-discord` crate also has an ignored live test that exercises
handshake/set/clear/refresh against the real Discord client:
`RIFT_DISCORD_APP_ID=<application id> cargo test -p rift-discord -- --ignored`.

### Package / targets

* package `rift`, version from the workspace (`0.6.1`), Rust floor 1.88.
* `[lib] name = "rift"`, path `src/lib.rs`. No binary, no build script.
* Public re-exports include `RiftBackend`, `BackendHandle`, `BackendConfig`,
  `BackendError`, `BackendEvent`, `BackendEvents`, `native_data_dir`,
  `APP_IDENTIFIER`, `DATA_DIR_ENV`, `DataDirError`, `MAX_CROSSFADE`,
  `MAX_IMAGE_BYTES`, `VERSION`, `preview_mode()`, plus the `fetch` module.
* Dependencies: `anyhow`, `directories`, `rand`, `reqwest`, `rfd`,
  `rift-discord`, `rift-types`, `rift-ytmusic`, `rodio`, `serde`, `serde_json`,
  `souvlaki`, `tokio`, `tracing`. (`rfd` is target-split: native backends on
  Windows/macOS, `xdg-portal` on Linux.)
