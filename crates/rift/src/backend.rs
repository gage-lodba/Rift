//! Construction, shared state and shutdown of the native backend.
//!
//! The application constructs a [`RiftBackend`] **once** (typically right after
//! its window exists) and keeps it for the whole session:
//!
//! ```text
//! let (backend, mut events) = RiftBackend::new(BackendConfig {
//!     media_hwnd,
//!     ..Default::default()
//! })?;
//! ```
//!
//! [`RiftBackend`] owns the backend's dedicated Tokio runtime. The
//! [`BackendHandle`] it derefs to is the cheap, `Clone + Send + Sync` half that
//! carries every command method; clone *that* into `tokio` tasks rather than the
//! owning value, so the runtime is always dropped on the application thread.
//!
//! Everything the former `main.rs` did in the shell's `setup` hook lives in
//! [`RiftBackend::new`]: data directory, settings, library, downloads, the audio
//! thread, the restored playback snapshot, the shared HTTP client, OS media
//! integration and Discord Rich Presence. Nothing in here performs network I/O,
//! so startup works offline (playback and metadata simply fail until the network
//! returns, each with an error event).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use rift_types::{Library, PlaybackState, QueueSnapshot, Track};
use tracing::{debug, info, warn};

use crate::audio::{self, AudioCmd};
use crate::discord::{self, DiscordHandle};
use crate::downloads::Downloads;
use crate::event::{BackendEvent, BackendEvents, EventSender};
use crate::library::LibraryStore;
use crate::media::{self, MediaHandle};
use crate::paths;
use crate::player::{self, PlayerCore, PlayerShared};
use crate::settings::{self, SettingsStore};
use crate::util::{DurableWriter, LockExt, Persister};

/// Maximum number of library rows the startup metadata repair looks up over the
/// network per launch. Rows beyond the cap, or rows whose lookup fails while
/// offline, are repaired on a later launch — or immediately when played, since
/// playback merges the fetched metadata back into the library.
const REPAIR_LOOKUP_LIMIT: usize = 20;

/// Consecutive failed lookups after which the repair stops. Three in a row is
/// the offline / blocked-network signal; the rows simply stay incomplete and the
/// next launch tries again.
const REPAIR_MAX_CONSECUTIVE_ERRORS: u32 = 3;

/// Optional construction parameters.
///
/// [`Default`] matches the shipped application: the historical data directory
/// and no window handle. Windows needs [`media_hwnd`](Self::media_hwnd) for the
/// OS media session; see the field docs.
#[derive(Debug, Clone, Default)]
pub struct BackendConfig {
    /// Override the data directory.
    ///
    /// `None` uses `RIFT_DATA_DIR` if set, else the platform data directory for
    /// [`APP_IDENTIFIER`](crate::APP_IDENTIFIER) (that is, exactly what the
    /// previous desktop build used: `%APPDATA%\dev.jerimiah.rift` on Windows).
    pub data_dir: Option<PathBuf>,
    /// Native window handle for the OS media session, as an integer.
    ///
    /// On Windows, `souvlaki`'s SMTC backend **requires** a real `HWND` and
    /// panics without one; pass the `winit` window's raw Win32 handle
    /// (`window.window_handle()` → `RawWindowHandle::Win32(h).hwnd` cast to
    /// `isize`). When `None` on Windows, the backend skips OS media integration
    /// entirely and logs a warning: media keys and now-playing metadata are lost,
    /// everything else works. Ignored on Linux/macOS (MPRIS / `MPNowPlayingInfo`
    /// need no window).
    pub media_hwnd: Option<isize>,
}

impl BackendConfig {
    /// Default configuration (see [`Default`]).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Use an explicit data directory.
    #[must_use]
    pub fn with_data_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.data_dir = Some(dir.into());
        self
    }

    /// Pass the native window handle (`HWND` as `isize`) for OS media controls.
    #[must_use]
    pub fn with_media_hwnd(mut self, hwnd: isize) -> Self {
        self.media_hwnd = Some(hwnd);
        self
    }
}

/// Why the backend could not be constructed.
///
/// Construction performs no network I/O; these are local-environment failures
/// (unwritable data directory, a TLS backend that cannot be loaded, a runtime or
/// audio thread that cannot be created).
#[derive(Debug)]
pub enum BackendError {
    /// The data directory could not be resolved (no base directory and no
    /// `RIFT_DATA_DIR`).
    DataDir(String),
    /// The data directory exists in name but could not be created.
    CreateDataDir {
        /// Path that could not be created (or written to later).
        path: PathBuf,
        /// Underlying filesystem error (`NotFound`, permission denied, …).
        source: std::io::Error,
    },
    /// A required component failed to initialize.
    Init(String),
}

impl std::fmt::Display for BackendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BackendError::DataDir(message) => write!(f, "no usable data directory: {message}"),
            BackendError::CreateDataDir { path, source } => write!(
                f,
                "could not create the data directory {}: {source}",
                path.display()
            ),
            BackendError::Init(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for BackendError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            BackendError::CreateDataDir { source, .. } => Some(source),
            _ => None,
        }
    }
}

impl From<paths::DataDirError> for BackendError {
    fn from(err: paths::DataDirError) -> Self {
        BackendError::DataDir(err.to_string())
    }
}

/// Shared state every backend operation reaches through.
///
/// Deliberately contains no Tokio [`Runtime`](tokio::runtime::Runtime): only the
/// runtime's [`Handle`](tokio::runtime::Handle). Tasks capture this type (via
/// [`BackendHandle`]), so dropping the last reference may happen on a runtime
/// worker thread — dropping a runtime there would panic, dropping a handle does
/// not.
pub(crate) struct BackendInner {
    pub(crate) player: Arc<PlayerShared>,
    pub(crate) library: Arc<Mutex<LibraryStore>>,
    pub(crate) downloads: Arc<Downloads>,
    pub(crate) settings: Arc<Mutex<SettingsStore>>,
    pub(crate) media: MediaHandle,
    pub(crate) discord: DiscordHandle,
    pub(crate) events: EventSender,
    pub(crate) data_dir: PathBuf,
    pub(crate) runtime: tokio::runtime::Handle,
    /// Background writer for durable state (`library.json`, `settings.json`).
    ///
    /// Held here (rather than only inside the stores) so shutdown can flush every
    /// queued write from one place before the runtime and threads stop.
    pub(crate) durable: Arc<DurableWriter>,
}

impl BackendInner {
    /// Publish an event (no-op once the application dropped its receiver).
    pub(crate) fn emit(&self, event: BackendEvent) {
        self.events.send(event);
    }

    /// Run a blocking closure on the backend's blocking pool.
    pub(crate) fn spawn_blocking<F, R>(&self, task: F) -> tokio::task::JoinHandle<R>
    where
        F: FnOnce() -> R + Send + 'static,
        R: Send + 'static,
    {
        self.runtime.spawn_blocking(task)
    }
}

/// Cheap, cloneable handle to a running backend.
///
/// This is what every command method lives on. Clone it into tasks:
///
/// ```text
/// let handle = backend.handle();
/// handle.clone().spawn(async move {
///     let tracks = handle.search("daft punk".into()).await;
///     // ... send the result wherever your UI expects it ...
/// });
/// ```
///
/// # Lifetime and shutdown
///
/// A clone keeps the backend's shared state — including the durable writer —
/// alive after [`RiftBackend::shutdown`] consumed the owning value. Shutdown
/// flushes every save queued **before** it runs, but saves queued afterwards by
/// tasks that are still winding down are drained by the durable writer's `Drop`
/// only when the last clone is released. Callers must therefore drop every
/// `BackendHandle` before returning from `main` (and before any
/// `std::process::exit`), or those final writes can be lost without a warning.
#[derive(Clone)]
pub struct BackendHandle {
    pub(crate) inner: Arc<BackendInner>,
}

impl BackendHandle {
    /// The data directory in use (`%APPDATA%\dev.jerimiah.rift` on Windows by
    /// default).
    #[must_use]
    pub fn data_dir(&self) -> &Path {
        &self.inner.data_dir
    }

    /// The runtime commands are spawned on.
    #[must_use]
    pub fn runtime_handle(&self) -> tokio::runtime::Handle {
        self.inner.runtime.clone()
    }

    /// Spawn a task on the backend runtime.
    pub fn spawn<F>(&self, future: F) -> tokio::task::JoinHandle<F::Output>
    where
        F: std::future::Future + Send + 'static,
        F::Output: Send + 'static,
    {
        self.inner.runtime.spawn(future)
    }

    /// Run a blocking closure on the backend's blocking pool (used for the file
    /// dialogs and for tests; never call a dialog from the UI thread directly).
    pub fn spawn_blocking<F, R>(&self, task: F) -> tokio::task::JoinHandle<R>
    where
        F: FnOnce() -> R + Send + 'static,
        R: Send + 'static,
    {
        self.inner.runtime.spawn_blocking(task)
    }

    /// Whether the debug-only preview mode is active (`RIFT_PREVIEW=1`).
    ///
    /// In preview mode commands that would touch the network are no-ops and
    /// settings are never persisted; the desktop app also renders placeholder
    /// data instead of calling the backend at all.
    #[must_use]
    pub fn preview_mode(&self) -> bool {
        settings::preview_mode()
    }

    /// A snapshot of the persisted library (likes, playlists, recents).
    ///
    /// `bootstrap()` returns the same data together with the rest of the launch
    /// state; this accessor is for re-syncing after the app has been awake for a
    /// while, with no event race.
    #[must_use]
    pub fn library(&self) -> Library {
        self.inner.library.lock_safe().data.clone()
    }

    /// A snapshot of offline-download status.
    #[must_use]
    pub fn downloads_state(&self) -> rift_types::DownloadState {
        self.inner.downloads.state()
    }

    /// A snapshot of the playback queue (tracks, current index, shuffle, repeat,
    /// source).
    #[must_use]
    pub fn queue_snapshot(&self) -> QueueSnapshot {
        let core = self.inner.player.core.lock_safe();
        player::snapshot(&core)
    }

    /// The current playback state.
    #[must_use]
    pub fn playback_state(&self) -> PlaybackState {
        self.inner.player.core.lock_safe().state
    }

    /// Current volume, 0.0..=1.0.
    #[must_use]
    pub fn volume(&self) -> f32 {
        self.inner.player.core.lock_safe().volume
    }
}

impl std::fmt::Debug for BackendHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BackendHandle")
            .field("data_dir", &self.inner.data_dir)
            .finish_non_exhaustive()
    }
}

/// The backend, owned once by the application.
///
/// Derefs to [`BackendHandle`], so all command methods are callable directly on
/// it. It also owns the dedicated Tokio runtime every task runs on; dropping it
/// (or calling [`shutdown`](Self::shutdown)) stops the audio, media and Discord
/// threads.
pub struct RiftBackend {
    /// Declared first so it is dropped before the shared state: runtime shutdown
    /// cancels the event-pump task, which releases its `BackendHandle` clone, and
    /// then the remaining state drops on the caller's thread.
    runtime: tokio::runtime::Runtime,
    handle: BackendHandle,
}

impl std::ops::Deref for RiftBackend {
    type Target = BackendHandle;

    fn deref(&self) -> &BackendHandle {
        &self.handle
    }
}

impl RiftBackend {
    /// Start the backend.
    ///
    /// Returns the owning value plus the event-bus receiver the application must
    /// drain. Construction does **not** block on the network and does not depend
    /// on a window existing (pass [`BackendConfig::default()`] for the headless /
    /// preview degraded path).
    pub fn new(config: BackendConfig) -> Result<(Self, BackendEvents), BackendError> {
        let data_dir = resolve_data_dir(&config)?;
        std::fs::create_dir_all(&data_dir).map_err(|source| BackendError::CreateDataDir {
            path: data_dir.clone(),
            source,
        })?;
        info!("data dir: {}", data_dir.display());

        let yt = rift_ytmusic::Client::new().map_err(|e| {
            BackendError::Init(format!("could not create the YouTube Music client: {e:#}"))
        })?;

        // One durable writer for both user-data files: every library/settings save
        // is queued here and written on this thread, so a command on the UI thread
        // never blocks on disk I/O. Shutdown flushes it (see `Drop`).
        let durable = Arc::new(DurableWriter::spawn());

        let library = Arc::new(Mutex::new(LibraryStore::load(&data_dir, &durable)));
        let downloads = Arc::new(Downloads::load(data_dir.join("downloads")));
        let settings = SettingsStore::load(&data_dir, &durable);
        let volume = settings.data.volume;
        let discord_rpc = settings.data.discord_rpc;
        let crossfade = f64::from(settings.data.crossfade);
        // Apply the persisted custom yt-dlp location (if any) before any fetch.
        crate::fetch::set_ytdlp_override(
            settings
                .data
                .yt_dlp_path
                .clone()
                .map(std::path::PathBuf::from),
        );
        info!("restored volume {volume}");
        let settings = Arc::new(Mutex::new(settings));

        // One bus for every consumer: command wrappers, the audio pump, download
        // jobs and the media/Discord threads all publish through clones of it.
        let (events, event_rx) = EventSender::channel();

        let (audio_events_tx, audio_events_rx) = tokio::sync::mpsc::unbounded_channel();
        let audio_tx = audio::spawn(audio_events_tx);
        // Apply the persisted volume to the fresh audio thread.
        let _ = audio_tx.send(AudioCmd::Volume(volume));

        // Restore the previous session's queue (stopped, not auto-playing).
        let playback_path = data_dir.join("playback.json");
        let mut core = PlayerCore {
            volume,
            crossfade,
            ..PlayerCore::default()
        };
        if let Some(snap) = player::load_snapshot(&playback_path) {
            tracing::info!("restored queue of {} tracks", snap.tracks.len());
            core.shuffle = snap.shuffle;
            core.repeat = snap.repeat;
            core.source = snap.source;
            core.current = snap.current.filter(|&i| i < snap.tracks.len());
            core.queue = snap.tracks;
            if let Some(t) = core.current.and_then(|i| core.queue.get(i)) {
                core.duration = f64::from(t.duration.unwrap_or(0));
            }
            if core.shuffle {
                core.shuffle_history = core.current.into_iter().collect();
            }
        }

        let player = Arc::new(PlayerShared {
            core: Mutex::new(core),
            audio: audio_tx,
            yt,
            // Every request this client makes is small and latency-bound
            // (update checks, the release-asset download, yt-dlp probing), so it
            // needs a ceiling: reqwest's default is *no* timeout, and a stalled
            // socket would otherwise wedge a command forever — including a
            // download worker holding a `Downloads::begin` claim that is then
            // never released.
            http: reqwest::Client::builder()
                .connect_timeout(std::time::Duration::from_secs(5))
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .map_err(|e| {
                    BackendError::Init(format!(
                        "could not build the HTTP client (TLS backend unavailable): {e}"
                    ))
                })?,
            playback_path,
            persist: Persister::spawn(),
            last_session: Mutex::new(None),
        });

        // The media thread needs to call back into the player when the OS sends a
        // media key. It holds only a `Weak` reference, filled in after the shared
        // state exists (it never fires before the app has started playback).
        let media_slot: Arc<OnceLock<Weak<BackendInner>>> = Arc::new(OnceLock::new());
        let media = media::spawn(config.media_hwnd, media_slot.clone());
        let discord = discord::spawn(discord_rpc);

        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("rift-backend")
            .enable_all()
            .build()
            .map_err(|e| BackendError::Init(format!("could not start the backend runtime: {e}")))?;

        let inner = Arc::new(BackendInner {
            player,
            library,
            downloads,
            settings,
            media,
            discord,
            events,
            data_dir,
            runtime: runtime.handle().clone(),
            durable,
        });
        // Ignore a `set` race: there is exactly one writer and it runs before any
        // playback can request a media action.
        let _ = media_slot.set(Arc::downgrade(&inner));

        // The restored playback queue is the complete, offline metadata source
        // for the user's library: the player writes it from full metadata, while
        // a liked row can have been stored from an album page that carried
        // neither byline nor length. Fill those rows now, before the launch
        // snapshot is exposed, and save once if anything changed. This runs
        // synchronously on the startup thread, so it uses the visible-lists-only
        // variant (`liked` + `recently_played`); playlists are repaired on play.
        let restored_queue = inner.player.core.lock_safe().queue.clone();
        if !restored_queue.is_empty() {
            let changed = inner
                .library
                .lock_safe()
                .merge_visible_metadata_from(&restored_queue);
            if changed {
                info!("metadata repair: filled library rows from the restored playback queue");
            }
        }

        // The audio thread is the progress ticker: it forwards position updates
        // several times a second, and this pump turns them into `Progress`
        // events, advances the queue on track end, and arms crossfades.
        let pump_state = Arc::clone(&inner);
        runtime.spawn(async move {
            player::event_loop(pump_state, audio_events_rx).await;
        });

        // Bounded background repair for whatever the offline merge could not
        // fill: one search per incomplete liked/recent row (max
        // `REPAIR_LOOKUP_LIMIT` per launch), sequentially, and never on the
        // startup path. Preview/offline simply leave the rows for the next launch
        // or for playback-time backfilling.
        let repair_state = Arc::clone(&inner);
        runtime.spawn(async move {
            repair_incomplete_library_metadata(repair_state).await;
        });

        info!("backend started");
        Ok((
            Self {
                runtime,
                handle: BackendHandle { inner },
            },
            event_rx,
        ))
    }

    /// A cloneable handle for use from spawned tasks.
    pub fn handle(&self) -> BackendHandle {
        self.handle.clone()
    }

    /// The runtime every command runs on.
    pub fn runtime_handle(&self) -> tokio::runtime::Handle {
        self.runtime.handle().clone()
    }

    /// Spawn a task on the backend runtime (see [`BackendHandle::spawn`]).
    pub fn spawn<F>(&self, future: F) -> tokio::task::JoinHandle<F::Output>
    where
        F: std::future::Future + Send + 'static,
        F::Output: Send + 'static,
    {
        self.runtime.spawn(future)
    }

    /// Stop the audio, media and Discord threads, flush pending durable writes,
    /// then drop the runtime.
    ///
    /// Call this from the application thread on exit. It is equivalent to
    /// dropping the value (the [`Drop`] impl does the same), but reads as the
    /// explicit lifecycle step in `main.rs`.
    ///
    /// The flush is a barrier for saves queued before this call; see
    /// [`BackendHandle`]'s "Lifetime and shutdown" section — every
    /// `BackendHandle` clone must be dropped before the process exits for the
    /// writer's final drain to run.
    pub fn shutdown(self) {
        drop(self);
    }

    /// Stop every background thread. Idempotent; sends are ignored if the thread
    /// has already exited.
    fn stop_background_threads(&self) {
        let _ = self.handle.inner.player.audio.send(AudioCmd::Shutdown);
        self.handle.inner.media.shutdown();
        self.handle.inner.discord.shutdown();
    }

    /// Wait (bounded) for queued `library.json` / `settings.json` writes to be
    /// attempted.
    ///
    /// This is the final guarantee for user data on a normal quit: the last like
    /// or volume change sits in the durable writer's queue rather than on the
    /// caller's thread, and it must not be lost. `DurableWriter::flush` bounds the
    /// wait and logs if it gives up, so a wedged disk cannot hang the process
    /// forever on the way out.
    fn flush_durable_writes(&self) {
        // Other live handles keep the writer alive past this flush, so the
        // writer's `Drop` drain (documented on `BackendHandle`) is the fallback
        // for anything queued after this point. The normal desktop app still
        // holds its controller's command handle here, so this is informational
        // rather than a warning: `warn!` would fire on every clean quit.
        let handles = Arc::strong_count(&self.handle.inner);
        if handles > 1 {
            tracing::debug!(
                handles,
                "shutting down with other BackendHandle clones alive; writes queued after \
                 this flush drain when the last clone drops"
            );
        }
        let _ = self.handle.inner.durable.flush();
    }
}

impl Drop for RiftBackend {
    fn drop(&mut self) {
        self.stop_background_threads();
        // Flush before the owned runtime and the shared state are torn down. (The
        // runtime's own drop waits for in-flight blocking tasks such as an open
        // import dialog; anything those queue after this flush is still drained by
        // the durable writer's `Drop`.)
        self.flush_durable_writes();
    }
}

impl std::fmt::Debug for RiftBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RiftBackend")
            .field("data_dir", &self.handle.inner.data_dir)
            .finish_non_exhaustive()
    }
}

/// Look up fresh metadata for the library rows the offline startup merge could
/// not complete.
///
/// Production wrapper around [`repair_with_lookup`]: the injected lookup is the
/// real YouTube Music search. Runs as one background task per launch; see
/// [`repair_with_lookup`] for the policy.
async fn repair_incomplete_library_metadata(backend: Arc<BackendInner>) {
    if crate::settings::preview_mode() {
        return;
    }
    let lookup_backend = Arc::clone(&backend);
    let lookup = move |query: String| {
        let yt = lookup_backend.player.yt.clone();
        async move { yt.search_tracks(&query).await.map_err(|e| e.to_string()) }
    };
    let library = Arc::clone(&backend.library);
    let emit_backend = Arc::clone(&backend);
    let repaired = repair_with_lookup(&library, lookup, move |snapshot: &Library| {
        emit_backend.emit(BackendEvent::Library(snapshot.clone()));
    })
    .await;
    if repaired > 0 {
        info!("metadata repair: filled metadata for {repaired} library row(s)");
    }
}

/// The repair pass itself, with the per-row lookup injected so it is testable
/// without a network.
///
/// A snapshot of at most [`REPAIR_LOOKUP_LIMIT`] incomplete liked/recent rows is
/// looked up sequentially. The search query is the stored title plus whichever
/// byline the row has (title only when the byline is empty, which is exactly the
/// album-row shape the bug report describes). Candidate selection is
/// [`choose_metadata_candidate`].
///
/// Failure policy: a lookup error is logged at debug and skipped; three
/// consecutive errors (offline / blocked network) stop the pass. Every apply
/// takes the library lock on its own and never across an `await`, so a slow
/// lookup cannot block the UI or a command. Returns the number of rows repaired;
/// when that is non-zero `emit` is called exactly once with the repaired library,
/// so an open Liked view refreshes without a restart.
async fn repair_with_lookup<L, Fut, E>(
    library: &Arc<Mutex<LibraryStore>>,
    lookup: L,
    emit: E,
) -> usize
where
    L: Fn(String) -> Fut,
    Fut: std::future::Future<Output = Result<Vec<Track>, String>>,
    E: Fn(&Library),
{
    let pending = {
        let guard = library.lock_safe();
        guard.incomplete_tracks(REPAIR_LOOKUP_LIMIT)
    };
    if pending.is_empty() {
        return 0;
    }
    info!(
        "metadata repair: looking up {} incomplete library row(s)",
        pending.len()
    );

    let mut repaired = 0usize;
    let mut consecutive_errors = 0u32;
    for stored in pending {
        let title = stored.title.trim();
        if title.is_empty() {
            continue;
        }
        let artist = stored.artist.trim();
        let query = if artist.is_empty() {
            title.to_owned()
        } else {
            format!("{title} {artist}")
        };
        let results = match lookup(query).await {
            Ok(results) => {
                consecutive_errors = 0;
                results
            }
            Err(error) => {
                consecutive_errors += 1;
                debug!(
                    "metadata repair: lookup for {:?} failed: {error}",
                    stored.title
                );
                if consecutive_errors >= REPAIR_MAX_CONSECUTIVE_ERRORS {
                    warn!(
                        "metadata repair: stopped after {consecutive_errors} consecutive lookup \
                         failures; incomplete rows will be retried next launch or when played"
                    );
                    break;
                }
                continue;
            }
        };
        let Some(candidate) = choose_metadata_candidate(&stored, &results) else {
            debug!(
                "metadata repair: no usable search result for {:?} by {:?}",
                stored.title, stored.artist
            );
            continue;
        };
        // One short lock per apply; the lookup above is the only slow part and it
        // happens with no lock held.
        let changed = library.lock_safe().apply_metadata(&candidate);
        if changed {
            debug!(
                "metadata repair: filled {:?} from search result {}",
                stored.title, candidate.id
            );
            repaired += 1;
        }
    }

    if repaired > 0 {
        let snapshot = library.lock_safe().data.clone();
        emit(&snapshot);
    }
    repaired
}

/// Pick the search result that can repair `stored`.
///
/// Preference order:
/// 1. an **exact non-empty id** match, which is unambiguously the requested
///    video;
/// 2. otherwise the first complete result that [`Track::same_song`] matches
///    **and** that is safe to trust without an id: the stored row must know its
///    album (and the candidate's must agree) or know its own duration (which
///    `same_song` used as evidence). With neither, `same_song` reduces to title +
///    a shared credit name, which can select a different recording of the song;
///    such ambiguous rows are left for playback-time repair, where the actual
///    stream settles what is playing.
///
/// "Complete" means the candidate can actually repair the user-visible gap: it
/// has a duration and at least one artist credit (byline or structured). An
/// incomplete result is ignored rather than merged: it would spend a lookup and
/// still leave the row half-empty.
fn choose_metadata_candidate(stored: &Track, results: &[Track]) -> Option<Track> {
    let complete = |track: &Track| {
        track.duration.is_some() && (!track.artist.is_empty() || !track.artists.is_empty())
    };
    if let Some(exact) = results.iter().find(|candidate| {
        !candidate.id.is_empty() && candidate.id == stored.id && complete(candidate)
    }) {
        return Some(exact.clone());
    }
    results
        .iter()
        .find(|candidate| {
            complete(candidate)
                && stored.same_song(candidate)
                && confident_without_an_id(stored, candidate)
        })
        .cloned()
}

/// Whether a `same_song` candidate is trustworthy when the ids differ.
///
/// `same_song` is deliberately conservative, but a stored row that knows neither
/// its album nor its length reduces the rule to title + credit name. For a
/// network lookup that is too weak (many recordings share a title), so require
/// one piece of album/length evidence on the stored row: matching non-empty album
/// ids, or a known stored duration (which `same_song` already checks against the
/// candidate's length).
fn confident_without_an_id(stored: &Track, candidate: &Track) -> bool {
    let albums_agree = match (stored.album_id.as_deref(), candidate.album_id.as_deref()) {
        (Some(stored_album), Some(candidate_album)) => {
            !stored_album.is_empty() && stored_album == candidate_album
        }
        _ => false,
    };
    albums_agree || stored.duration.is_some()
}

/// Resolve the data directory: explicit config, then `RIFT_DATA_DIR`, then the
/// platform default.
fn resolve_data_dir(config: &BackendConfig) -> Result<PathBuf, BackendError> {
    let env_override = std::env::var_os(paths::DATA_DIR_ENV);
    match &config.data_dir {
        Some(dir) => Ok(dir.clone()),
        None => match env_override.filter(|value| !value.is_empty()) {
            Some(value) => Ok(PathBuf::from(value)),
            None => paths::native_data_dir().map_err(BackendError::from),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tests that construct a real backend (threads + a Tokio runtime) take this
    /// so they do not run concurrently inside one test process.
    static BACKEND_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// The full constructor starts the audio, media and Discord threads. That
    /// works on a local Windows desktop session and on the macOS/Linux runners,
    /// but the headless Windows CI runner kills the process with an access
    /// violation shortly after the first construction. Skip only that
    /// environment; everywhere else the constructor stays covered.
    fn skip_headless_windows_ci() -> bool {
        cfg!(windows) && std::env::var_os("CI").is_some()
    }

    #[test]
    fn the_config_defaults_to_the_historical_layout_and_no_window_handle() {
        let config = BackendConfig::default();
        assert!(config.data_dir.is_none());
        assert!(config.media_hwnd.is_none());
        let configured = BackendConfig::new()
            .with_data_dir("D:\\rift")
            .with_media_hwnd(0x1234);
        assert_eq!(configured.data_dir.as_deref(), Some(Path::new("D:\\rift")));
        assert_eq!(configured.media_hwnd, Some(0x1234));
    }

    /// An explicit data directory bypasses the environment and the platform
    /// lookup, so preview/portable installs never touch the real library.
    #[test]
    fn an_explicit_data_dir_is_used_verbatim() {
        let dir = PathBuf::from("D:\\portable-rift");
        let config = BackendConfig::new().with_data_dir(dir.clone());
        assert_eq!(resolve_data_dir(&config).expect("explicit dir"), dir);
    }

    /// The owning value and the handle must both be usable across threads; the
    /// event pump and every media/Discord/media-key thread depend on it.
    #[test]
    fn the_public_handle_types_are_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<BackendHandle>();
        assert_send_sync::<RiftBackend>();
        assert_send_sync::<BackendConfig>();
    }

    /// The real startup/shutdown path, with no window and no network: this is the
    /// smoke test that would catch a startup panic (a missing data directory, a
    /// media thread that expected an HWND, an event pump that needs a window) and a
    /// shutdown that leaves the data directory unusable or its last queued write
    /// on the writer's queue.
    ///
    /// Uses an explicit temp directory so the developer's real library is never
    /// read or written. Audio/Discord/media availability is environment-dependent
    /// and explicitly allowed to degrade (the backend logs and carries on), which
    /// is exactly the contract for a headless machine.
    ///
    /// Leaked OS threads cannot be observed portably from here; what this test
    /// pins is that the shutdown path returns, persists the last queued save, and
    /// leaves parsable state files behind for the next launch.
    #[test]
    fn a_backend_starts_and_stops_cleanly_without_a_window_or_network() {
        use rift_types::PlaybackState;

        // Both tests here construct a real backend (audio/media/Discord threads
        // plus a Tokio runtime); serialize them so they cannot contend for the
        // same OS devices inside one test process.
        if skip_headless_windows_ci() {
            return;
        }
        let _backend_guard = BACKEND_TEST_LOCK.lock_safe();

        let dir = crate::util::testing::TempDir::new("backend-start-stop");
        let (backend, mut events) =
            RiftBackend::new(BackendConfig::new().with_data_dir(dir.path().to_path_buf()))
                .expect("the backend must start without a window or a network");

        assert_eq!(backend.data_dir(), dir.path());
        assert_eq!(backend.preview_mode(), crate::settings::preview_mode());

        // The launch snapshot is consistent with a fresh data directory.
        let bootstrap = backend.bootstrap();
        assert_eq!(bootstrap.volume, 1.0);
        assert_eq!(bootstrap.state, PlaybackState::Stopped);
        assert!(bootstrap.queue.tracks.is_empty());
        assert!(bootstrap.library.liked.is_empty());
        assert!(bootstrap.library.playlists.is_empty());
        assert!(!bootstrap.preview || crate::settings::preview_mode());
        assert_eq!(backend.library(), bootstrap.library);

        // A sync command that touches the audio thread and the persisted settings
        // works even when no audio device exists (the send is best-effort).
        backend.set_volume(0.5);
        assert_eq!(backend.volume(), 0.5);
        let settings_path = dir.path().join("settings.json");
        backend.save_volume(0.5);
        // The save is queued on the durable writer, not written inline: `flush`
        // is the barrier that makes it observable (no sleeping).
        assert!(
            backend.handle.inner.durable.flush(),
            "save_volume's write must drain to disk"
        );
        assert!(
            settings_path.exists(),
            "save_volume must persist {}",
            settings_path.display()
        );
        let restored = SettingsStore::load(dir.path(), &backend.handle.inner.durable);
        assert!((restored.data.volume - 0.5).abs() < 1e-6);

        // The event bus is live: draining must never block or panic.
        for event in events.drain() {
            let _ = event.name();
        }

        // Queue a final settings change without flushing it: dropping the owner
        // is the shutdown path, and it must persist the last queued write.
        backend.save_volume(0.75);

        // Dropping the owner is the shutdown path (shutdown() would be explicit).
        drop(backend);

        // Reload exactly as the next launch would. This is the meaningful end of
        // the "clean stop" assertion: the shutdown flush ran (the save is on
        // disk) and the data directory is still parsable.
        let verify_writer = std::sync::Arc::new(DurableWriter::spawn());
        let settings = SettingsStore::load(dir.path(), &verify_writer);
        assert!(
            (settings.data.volume - 0.75).abs() < 1e-6,
            "shutdown must persist the last queued settings change: got {}",
            settings.data.volume
        );
        let library = LibraryStore::load(dir.path(), &verify_writer);
        assert!(
            library.data.liked.is_empty() && library.data.playlists.is_empty(),
            "the data directory must stay parsable across shutdown"
        );
    }

    /// A normal quit must not lose the last user changes, and it is the
    /// **explicit flush in the shutdown path** that makes them durable before the
    /// runtime and the last handle are torn down. This test pins that barrier
    /// instead of merely observing the writer's own `Drop` fallback (which would
    /// drain the same queue even if the shutdown flush were deleted):
    ///
    /// * a `BackendHandle` clone is kept alive, so `shutdown()` cannot drop the
    ///   shared state — and therefore cannot drop the durable writer — before the
    ///   reload;
    /// * a gate job holds the writer thread, so the user saves queued after it
    ///   cannot be written until the test releases the gate;
    /// * `shutdown()` runs on its own thread: it must block inside the flush
    ///   barrier while the gate is closed. If the explicit flush were removed,
    ///   shutdown would return and the bounded wait below would observe it;
    /// * after the gate is released, the reload proves both queued files were
    ///   written before shutdown finished.
    #[test]
    fn shutdown_flushes_pending_library_and_settings_writes() {
        use std::sync::mpsc;
        use std::time::Duration;

        if skip_headless_windows_ci() {
            return;
        }
        let _backend_guard = BACKEND_TEST_LOCK.lock_safe();

        let dir = crate::util::testing::TempDir::new("backend-shutdown-flush");
        let (backend, _events) =
            RiftBackend::new(BackendConfig::new().with_data_dir(dir.path().to_path_buf()))
                .expect("the backend must start without a window or a network");

        // This clone keeps `Arc<BackendInner>` (and the durable writer) alive past
        // `shutdown()`, so only the explicit flush can make the queued saves
        // durable before the reload below.
        let keep_alive = backend.handle();
        let writer = std::sync::Arc::clone(&keep_alive.inner.durable);

        // Gate the writer thread: its serializer blocks until the test releases
        // it, so every later job (the user saves, then the shutdown Flush) queues
        // behind a job that is provably still running.
        let (started_tx, started_rx) = mpsc::channel();
        let (gate_tx, gate_rx) = mpsc::channel::<()>();
        let gate_path = dir.join("gate-marker.json");
        writer.write(gate_path, "gate", (), move |_| -> Result<Vec<u8>, String> {
            let _ = started_tx.send(());
            let _ = gate_rx.recv();
            Ok(b"gate released".to_vec())
        });
        started_rx
            .recv_timeout(Duration::from_secs(30))
            .expect("the gate job should be running on the writer thread");

        // Two rapid mutations, queued behind the gate: nothing on disk is
        // guaranteed until the shutdown flush runs.
        backend.save_volume(0.25);
        backend
            .create_playlist("Shutdown Mix".into())
            .expect("create the playlist");

        let (done_tx, done_rx) = mpsc::channel();
        let shutdown = std::thread::spawn(move || {
            backend.shutdown();
            let _ = done_tx.send(());
        });

        // The explicit flush enqueues a Flush behind the user saves and waits for
        // the ack, so shutdown cannot finish while the gate is closed. With the
        // flush removed, shutdown would return here instead of blocking.
        assert!(
            done_rx.recv_timeout(Duration::from_millis(500)).is_err(),
            "shutdown returned before the durable-write barrier: \
             is flush_durable_writes() still called from Drop?"
        );

        gate_tx.send(()).expect("release the writer gate");
        done_rx
            .recv_timeout(Duration::from_secs(30))
            .expect("shutdown must finish once the writer gate is released");
        shutdown.join().expect("the shutdown thread must not panic");

        // Reload exactly as the next launch would, with a fresh writer.
        let verify_writer = std::sync::Arc::new(DurableWriter::spawn());
        let library = LibraryStore::load(dir.path(), &verify_writer);
        assert_eq!(
            library.data.playlists.len(),
            1,
            "the playlist queued just before shutdown must survive it"
        );
        assert_eq!(library.data.playlists[0].name, "Shutdown Mix");
        let settings = SettingsStore::load(dir.path(), &verify_writer);
        assert!(
            (settings.data.volume - 0.25).abs() < 1e-6,
            "the volume queued just before shutdown must survive it: got {}",
            settings.data.volume
        );

        // Releasing the last clone is the writer's final drain point; it must
        // succeed cleanly after shutdown (nothing left queued that fails to write).
        drop(keep_alive);
    }

    // -----------------------------------------------------------------------
    // Metadata repair (task-45): selection rule + the offline startup merge.
    // -----------------------------------------------------------------------

    fn candidate_track(id: &str) -> Track {
        Track {
            id: id.into(),
            title: "Around the World".into(),
            artist: "Daft Punk".into(),
            album: Some("Homework".into()),
            duration: Some(430),
            cover: String::new(),
            artists: vec![rift_types::ArtistRef {
                id: Some("chan".into()),
                name: "Daft Punk".into(),
            }],
            album_id: Some("MPREb_dMHztPsrDgN".into()),
        }
    }

    fn incomplete_liked() -> Track {
        // The album-page shape from the bug report: id, title, album, duration
        // — but no byline and no structured credits.
        Track {
            id: "K0HSD_i2DvA".into(),
            title: "Around the World".into(),
            artist: String::new(),
            album: Some("Homework".into()),
            duration: Some(430),
            cover: String::new(),
            artists: Vec::new(),
            album_id: Some("MPREb_dMHztPsrDgN".into()),
        }
    }

    /// Exact-id match wins over a `same_song` result: it is unambiguously the
    /// requested video.
    #[test]
    fn choose_metadata_candidate_prefers_the_exact_id() {
        let stored = incomplete_liked();
        let same_song = candidate_track("Jb6gcoR266U");
        let exact = candidate_track("K0HSD_i2DvA");
        let results = vec![same_song, exact.clone()];
        assert_eq!(
            choose_metadata_candidate(&stored, &results).map(|t| t.id),
            Some(exact.id)
        );
    }

    /// An incomplete exact-id row is ignored: it cannot repair the gap, so the
    /// complete `same_song` result (the other surface's upload) is used instead.
    #[test]
    fn choose_metadata_candidate_ignores_incomplete_results() {
        let stored = incomplete_liked();
        let incomplete_exact = Track {
            artist: String::new(),
            artists: Vec::new(),
            duration: None,
            ..candidate_track("K0HSD_i2DvA")
        };
        let complete_other_surface = candidate_track("Jb6gcoR266U");
        let results = vec![incomplete_exact, complete_other_surface.clone()];
        assert_eq!(
            choose_metadata_candidate(&stored, &results).map(|t| t.id),
            Some(complete_other_surface.id)
        );
    }

    /// Nothing usable: all results are unrelated or themselves incomplete.
    #[test]
    fn choose_metadata_candidate_reports_none_when_nothing_usable() {
        let stored = incomplete_liked();
        assert!(choose_metadata_candidate(&stored, &[]).is_none());

        let mut unrelated = candidate_track("other");
        unrelated.title = "Some Other Song".into();
        let incomplete = Track {
            artist: String::new(),
            artists: Vec::new(),
            duration: None,
            ..candidate_track("K0HSD_i2DvA")
        };
        assert!(choose_metadata_candidate(&stored, &[unrelated, incomplete]).is_none());
    }

    /// The offline half of the fix, through the real constructor: a library whose
    /// liked row lacks the byline/credits is repaired from the complete restored
    /// playback queue before the backend is handed to the app (no network — the
    /// queue copy completes the row, so the background repair has nothing to do).
    #[test]
    fn startup_merge_fills_incomplete_liked_metadata_from_the_restored_queue() {
        use rift_types::{Library, RepeatMode};

        if skip_headless_windows_ci() {
            return;
        }
        let _backend_guard = BACKEND_TEST_LOCK.lock_safe();

        let dir = crate::util::testing::TempDir::new("backend-metadata-merge");
        let complete = candidate_track("K0HSD_i2DvA");

        // Seed the previous session: a queue with the complete metadata and a
        // liked row that knows only the album-page half.
        let snapshot = QueueSnapshot {
            tracks: vec![complete],
            current: Some(0),
            shuffle: false,
            repeat: RepeatMode::Off,
            source: None,
        };
        crate::util::save_json(&dir.join("playback.json"), &snapshot, "playback");
        let stored = Library {
            liked: vec![incomplete_liked()],
            ..Library::default()
        };
        crate::util::save_json(&dir.join("library.json"), &stored, "library");

        let (backend, _events) =
            RiftBackend::new(BackendConfig::new().with_data_dir(dir.path().to_path_buf()))
                .expect("the backend must start without a window or a network");

        let library = backend.library();
        let liked = library.liked.first().expect("the like survives startup");
        assert_eq!(liked.artist, "Daft Punk", "filled from the queue's byline");
        assert_eq!(liked.artists.len(), 1, "filled from the queue's credits");
        assert_eq!(liked.duration, Some(430), "the known duration is kept");

        drop(backend);
    }

    /// A stored row with matching non-empty album ids is trusted over a
    /// `same_song` candidate even when it lost its duration.
    #[test]
    fn choose_metadata_candidate_accepts_album_evidence_without_an_id() {
        let mut stored = incomplete_liked();
        stored.id = "stored-id".into();
        stored.duration = None; // album is the only evidence left
        let candidate = candidate_track("Jb6gcoR266U"); // same album id
        assert_eq!(
            choose_metadata_candidate(&stored, &[candidate]).map(|t| t.id),
            Some("Jb6gcoR266U".to_owned())
        );
    }

    /// A stored row that knows neither its album nor its duration is ambiguous
    /// without an id: `same_song` alone must not pick a recording.
    #[test]
    fn choose_metadata_candidate_rejects_ambiguous_no_album_no_duration() {
        let mut stored = incomplete_liked();
        stored.id = "stored-id".into();
        stored.duration = None;
        stored.album_id = None;
        stored.artist = "Daft Punk".into(); // title + credit name: still ambiguous
        assert!(
            choose_metadata_candidate(&stored, &[candidate_track("Jb6gcoR266U")]).is_none(),
            "no album and no known duration: leave it for playback repair"
        );
    }

    /// A known stored duration is the other accepted piece of evidence.
    #[test]
    fn choose_metadata_candidate_accepts_a_known_stored_duration() {
        let mut stored = incomplete_liked();
        stored.id = "stored-id".into();
        stored.album_id = None;
        // duration stays Some(430): same_song checks the candidate against it.
        assert_eq!(
            choose_metadata_candidate(&stored, &[candidate_track("Jb6gcoR266U")]).map(|t| t.id),
            Some("Jb6gcoR266U".to_owned())
        );
    }

    /// The exact-id shortcut requires a non-empty candidate id: an empty id is
    /// "no id", not a match for an also-empty stored id.
    #[test]
    fn choose_metadata_candidate_requires_a_non_empty_exact_id() {
        let mut stored = incomplete_liked();
        stored.id = String::new();
        let mut unrelated = candidate_track("");
        unrelated.title = "A Different Song".into();
        assert!(
            choose_metadata_candidate(&stored, &[unrelated]).is_none(),
            "an empty candidate id must not exact-match"
        );
    }

    // -----------------------------------------------------------------------
    // The async repair pass itself, with an injected lookup (no network).
    // -----------------------------------------------------------------------

    /// A store with `rows` as liked songs, plus the temp dir/writer that keep it
    /// alive for a repair test.
    fn repair_store(
        rows: Vec<Track>,
        name: &str,
    ) -> (
        Arc<Mutex<LibraryStore>>,
        crate::util::testing::TempDir,
        Arc<DurableWriter>,
    ) {
        let dir = crate::util::testing::TempDir::new(name);
        let writer = Arc::new(DurableWriter::spawn());
        let mut store = LibraryStore::load(dir.path(), &writer);
        store.data.liked = rows;
        (Arc::new(Mutex::new(store)), dir, writer)
    }

    /// Repairs are applied and the pass emits exactly one `Library` event.
    #[tokio::test]
    async fn metadata_repair_fills_rows_and_emits_exactly_one_event() {
        let (library, _dir, _writer) = repair_store(vec![incomplete_liked()], "backend-repair-ok");
        let emitted: Arc<Mutex<Vec<Library>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&emitted);

        let repaired = repair_with_lookup(
            &library,
            |_query: String| std::future::ready(Ok(vec![candidate_track("K0HSD_i2DvA")])),
            move |snapshot: &Library| sink.lock_safe().push(snapshot.clone()),
        )
        .await;

        assert_eq!(repaired, 1);
        let events = emitted.lock_safe();
        assert_eq!(events.len(), 1, "exactly one Library event per pass");
        assert_eq!(events[0].liked[0].artist, "Daft Punk");
        assert_eq!(events[0].liked[0].artists.len(), 1);
    }

    /// No usable result: nothing is applied and no event is emitted.
    #[tokio::test]
    async fn metadata_repair_skips_rows_without_a_usable_result() {
        let (library, _dir, _writer) =
            repair_store(vec![incomplete_liked()], "backend-repair-no-result");
        let emitted: Arc<Mutex<usize>> = Arc::new(Mutex::new(0));
        let sink = Arc::clone(&emitted);

        let repaired = repair_with_lookup(
            &library,
            |_query: String| std::future::ready(Ok(Vec::new())),
            move |_snapshot: &Library| *sink.lock_safe() += 1,
        )
        .await;

        assert_eq!(repaired, 0);
        assert_eq!(*emitted.lock_safe(), 0, "no event without a repair");
        assert_eq!(library.lock_safe().data.liked[0].artist, "");
    }

    /// Three consecutive lookup errors stop the pass (offline/blocked network).
    #[tokio::test]
    async fn metadata_repair_stops_after_three_consecutive_errors() {
        let rows: Vec<Track> = (0..5)
            .map(|i| {
                let mut row = incomplete_liked();
                row.id = format!("id-{i}");
                row.title = format!("Around the World {i}");
                row
            })
            .collect();
        let (library, _dir, _writer) = repair_store(rows, "backend-repair-errors");
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = Arc::clone(&calls);

        let repaired = repair_with_lookup(
            &library,
            move |_query: String| {
                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                std::future::ready(Err("offline".to_owned()))
            },
            |_snapshot: &Library| panic!("no repair, so no Library event"),
        )
        .await;

        assert_eq!(repaired, 0);
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            REPAIR_MAX_CONSECUTIVE_ERRORS as usize,
            "the pass must stop after the third consecutive failure"
        );
    }

    /// The pass is bounded per launch: it looks up at most
    /// [`REPAIR_LOOKUP_LIMIT`] rows.
    #[tokio::test]
    async fn metadata_repair_caps_the_lookups_per_launch() {
        let rows: Vec<Track> = (0..REPAIR_LOOKUP_LIMIT + 5)
            .map(|i| {
                let mut row = incomplete_liked();
                row.id = format!("id-{i}");
                row.title = format!("Around the World {i}");
                row
            })
            .collect();
        let (library, _dir, _writer) = repair_store(rows, "backend-repair-limit");
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = Arc::clone(&calls);

        let repaired = repair_with_lookup(
            &library,
            move |_query: String| {
                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                std::future::ready(Ok(Vec::new()))
            },
            |_snapshot: &Library| {},
        )
        .await;

        assert_eq!(repaired, 0);
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            REPAIR_LOOKUP_LIMIT,
            "no more than the per-launch cap"
        );
    }

    /// The library lock must not be held across the lookup await: the injected
    /// lookup probes it from inside its own future.
    #[tokio::test]
    async fn metadata_repair_does_not_hold_the_lock_across_the_lookup() {
        let (library, _dir, _writer) =
            repair_store(vec![incomplete_liked()], "backend-repair-lock");
        let probe = Arc::clone(&library);

        let repaired = repair_with_lookup(
            &library,
            move |_query: String| {
                let probe = Arc::clone(&probe);
                async move {
                    assert!(
                        probe.try_lock().is_ok(),
                        "the repair pass must not hold the library lock across the await"
                    );
                    Ok(Vec::new())
                }
            },
            |_snapshot: &Library| {},
        )
        .await;

        assert_eq!(repaired, 0);
    }
}
