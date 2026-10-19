//! Bridge between the immediate-mode UI and the native Rift backend.
//!
//! The UI never talks to the backend directly: `app.rs` turns [`Action`]s into
//! calls on this controller, and drains [`ControllerEvent`]s from it once per
//! frame. With the `backend` cargo feature enabled the events/commands are
//! serviced by `rift::RiftBackend` (see
//! `docs/backend-api.md`); without it (and in preview/screenshot
//! mode) the controller is inert, which is what lets the renderer and UI be
//! built and tested before the port lands.
//!
//! # Threading model
//!
//! * **UI thread** — [`Controller::new`] constructs the backend (and owns its
//!   Tokio runtime, so the runtime is always dropped on the UI thread),
//!   [`Controller::bootstrap`] reads the launch snapshot, [`Controller::poll`]
//!   drains the event bus with a non-blocking `try_recv`, and [`dispatch`] turns
//!   actions into commands. Sync commands are called inline; they take short
//!   locks and do no network I/O, exactly as `backend-api.md` §5 specifies.
//! * **Backend runtime** — every `async` command (network I/O), the cover-art
//!   HTTP fetch and the reachability probe are spawned as tasks on the backend's
//!   runtime. Their results come back through a controller-owned channel, never
//!   by blocking the UI thread.
//! * **Blocking pool** — cover-art decode/downscale runs here (`spawn_blocking`),
//!   so a multi-megapixel JPEG never stalls a frame.
//!
//! # Stale-result guarding
//!
//! The old web frontend kept `search_gen` / `page_gen` counters: every reply
//! carried the generation it was issued for and was dropped if a newer request
//! had since bumped the counter. The controller now owns those counters (it is
//! the half that issues requests and receives replies): a search result is only
//! emitted for the newest submitted query, and an artist/album page result only
//! for the newest navigation. Results are dropped *before* becoming a
//! [`ControllerEvent`], so `app.rs` never sees a stale one; page events also
//! carry their `generation` so the app can keep `State::page_generation`
//! monotonic as defence in depth.
//!
//! # Testing seam
//!
//! Commands go through the [`BackendCommands`] trait. The live implementation
//! (`live`) wraps `rift::BackendHandle`; the tests use a recording double, so
//! the whole Action→command mapping, the generation filters, cover de-duplication
//! and event mapping are unit-testable with no backend, network or window.
//!
//! [`dispatch`]: Controller::dispatch

use std::collections::{HashMap, VecDeque};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use rift_types::{
    AlbumPage, AlbumSummary, ArtistPage, ArtistSummary, Bootstrap, DownloadState, Library,
    PlaybackState, Playlist, Progress, QueueSnapshot, Track, UpdateStatus, YtDlpStatus,
};

pub use crate::cover::CoverImage;
use crate::cover::{CoverRequest, CoverTracker};
use crate::state::{Action, SearchTab, View};

/// How often the controller probes network reachability. The old frontend got
/// this for free from the webview's `online`/`offline` events; the native app
/// asks the backend for one cheap HEAD request every ten seconds.
const ONLINE_PROBE_INTERVAL: Duration = Duration::from_secs(10);

// `CoverImage` lives in `cover.rs`; it is re-exported here because it is part of
// this module's public event API (`crate::controller::CoverImage` is the path
// `app.rs` already uses).

// ---------------------------------------------------------------------------
// Event type shared with app.rs
// ---------------------------------------------------------------------------

/// Something that happened outside the UI that the UI must react to.
#[derive(Clone, Debug)]
pub enum ControllerEvent {
    // ------------------------------------------------ backend broadcasts
    Track(Option<Track>),
    PlaybackState(PlaybackState),
    Queue(QueueSnapshot),
    Progress(Progress),
    Library(Library),
    Downloads(DownloadState),
    /// A user-visible error (backend `rift://error`, or a failed command).
    Error(String),
    /// An informational toast (backend `rift://notice`).
    Notice(String),
    /// Backend-driven navigation (e.g. after a playlist file import).
    OpenPlaylist(String),

    // --------------------------------------------- command replies (async)
    /// Songs for a submitted search. `Err` means the search failed (the UI
    /// clears its busy flag and toasts the message).
    SearchResults {
        query: String,
        results: Result<Vec<Track>, String>,
    },
    /// Artists for the current search, fetched lazily when the tab is first
    /// opened (mirrors the old UI). `Err` clears the "loading" tab to empty.
    SearchArtists {
        query: String,
        results: Result<Vec<ArtistSummary>, String>,
    },
    /// Albums for the current search; see [`ControllerEvent::SearchArtists`].
    SearchAlbums {
        query: String,
        results: Result<Vec<AlbumSummary>, String>,
    },
    /// A requested artist page finished loading. `generation` is the controller's
    /// page generation at issue time; the controller already dropped superseded
    /// replies, the value is for `app.rs`'s monotonic page-generation guard.
    ArtistPage {
        generation: u64,
        page: ArtistPage,
    },
    /// The "Show all songs" catalog for an artist finished loading.
    ArtistSongs {
        generation: u64,
        artist_id: String,
        tracks: Vec<Track>,
    },
    /// A requested album page finished loading.
    AlbumPage {
        generation: u64,
        page: AlbumPage,
    },
    /// Result of a requested update check.
    UpdateStatus(UpdateStatus),
    /// Result of a requested yt-dlp probe (also after setting the path or
    /// downloading the binary).
    YtDlpStatus(YtDlpStatus),
    /// A cover URL finished downloading and decoding.
    Cover(CoverImage),
    /// The cover request failed (or this is preview mode, which has no network);
    /// render the fallback tile.
    CoverFailed(String),

    // ------------------------------------------------------- navigation
    /// Open an artist page; emitted for `Action::OpenCurrentArtist` once the
    /// controller resolves the current track's first linkable artist.
    OpenArtist(String),

    // ------------------------------------------------------- network status
    /// Network reachability changed (the first probe result always emits).
    Online(bool),
}

// ---------------------------------------------------------------------------
// Command seam
// ---------------------------------------------------------------------------

/// Every backend command the UI can trigger, as a trait so the mapping is
/// testable without a live backend.
///
/// The live implementation (`live`) wraps `rift::BackendHandle`; tests inject a
/// recording double. Methods fall into three groups:
///
/// * **sync** — call-through to a backend method that takes short locks and does
///   no network I/O; safe on the UI thread.
/// * **async** — spawned on the backend runtime; the result (or its absence) is
///   reported later through the controller-owned completion channel as one of
///   the `TaskResult` values, which [`Controller::poll`] turns into
///   [`ControllerEvent`]s after applying stale-generation filters.
/// * **fallible sync** — returns `Err(String)` for the controller to surface as
///   [`ControllerEvent::Error`] (e.g. a blank playlist name, a refused URL).
///
/// The parameter names mirror the backend API so the mapping table in
/// `docs/controller-notes.md` reads 1:1.
pub trait BackendCommands: Send + Sync + 'static {
    // ------------------------------------------------------------- startup
    /// Synchronous launch snapshot (`backend.bootstrap()`).
    fn bootstrap(&self) -> Result<Bootstrap, String>;

    // ------------------------------------------------------------ playback
    fn play_tracks(&self, tracks: Vec<Track>, start: usize, source: Option<String>);
    fn play_track(&self, track: Track);
    fn toggle_play(&self);
    fn next_track(&self);
    fn prev_track(&self);
    fn seek(&self, position: f64);
    fn set_volume(&self, volume: f32);
    fn save_volume(&self, volume: f32);
    fn set_discord_rpc(&self, enabled: bool);
    fn set_crossfade(&self, secs: f32);
    fn toggle_shuffle(&self);
    fn cycle_repeat(&self);

    // --------------------------------------------------------------- queue
    fn queue_add(&self, track: Track);
    fn queue_play_next(&self, tracks: Vec<Track>);
    fn queue_move(&self, from: usize, to: usize);
    fn queue_remove(&self, index: usize);
    fn queue_jump(&self, index: usize);
    fn queue_clear(&self);

    // ------------------------------------------------------------- library
    fn toggle_like(&self, track: Track);
    fn create_playlist(&self, name: String) -> Result<(), String>;
    fn import_yt_playlist(&self, url: String);
    fn export_playlist(&self, id: String);
    fn import_playlist(&self);
    fn delete_playlist(&self, id: String);
    fn rename_playlist(&self, id: String, name: String);
    fn move_playlist(&self, id: String, to: usize);
    fn add_to_playlist(&self, id: String, track: Track);
    fn remove_from_playlist(&self, id: String, track_id: String);

    // ------------------------------------------------------------ downloads
    fn download_tracks(&self, tracks: Vec<Track>);
    fn remove_downloads(&self, ids: Vec<String>);

    // ------------------------------------------------------------- updates
    fn check_update(&self);
    fn set_update_notifications(&self, enabled: bool);
    fn open_url(&self, url: String) -> Result<(), String>;

    // -------------------------------------------------------------- yt-dlp
    fn check_ytdlp(&self);
    fn set_yt_dlp_path(&self, path: Option<String>);
    fn download_ytdlp(&self);

    // ------------------------------------------------------ search/catalog
    fn search_songs(&self, generation: u64, query: String);
    fn search_artists(&self, generation: u64, query: String);
    fn search_albums(&self, generation: u64, query: String);
    fn load_artist_page(&self, generation: u64, id: String);
    fn load_artist_songs(&self, generation: u64, id: String);
    fn load_album_page(&self, generation: u64, id: String);

    // --------------------------------------------------------------- images
    /// Fetch one cover URL and report a decoded
    /// [`TaskResult::Cover`] (or its error) through the controller channel.
    fn fetch_cover(&self, url: String);

    // ------------------------------------------------------- connectivity
    /// Run one cheap reachability probe and report [`TaskResult::Online`].
    fn check_online(&self);
}

/// The inert implementation: preview/screenshot mode, and the fallback when the
/// app is built without the `backend` feature. Every command is accepted and
/// ignored; no thread, runtime or network client exists.
#[derive(Debug, Default)]
struct InertCommands;

impl BackendCommands for InertCommands {
    fn bootstrap(&self) -> Result<Bootstrap, String> {
        Err("no backend attached".into())
    }

    fn play_tracks(&self, _: Vec<Track>, _: usize, _: Option<String>) {}
    fn play_track(&self, _: Track) {}
    fn toggle_play(&self) {}
    fn next_track(&self) {}
    fn prev_track(&self) {}
    fn seek(&self, _: f64) {}
    fn set_volume(&self, _: f32) {}
    fn save_volume(&self, _: f32) {}
    fn set_discord_rpc(&self, _: bool) {}
    fn set_crossfade(&self, _: f32) {}
    fn toggle_shuffle(&self) {}
    fn cycle_repeat(&self) {}

    fn queue_add(&self, _: Track) {}
    fn queue_play_next(&self, _: Vec<Track>) {}
    fn queue_move(&self, _: usize, _: usize) {}
    fn queue_remove(&self, _: usize) {}
    fn queue_jump(&self, _: usize) {}
    fn queue_clear(&self) {}

    fn toggle_like(&self, _: Track) {}
    fn create_playlist(&self, _: String) -> Result<(), String> {
        Ok(())
    }
    fn import_yt_playlist(&self, _: String) {}
    fn export_playlist(&self, _: String) {}
    fn import_playlist(&self) {}
    fn delete_playlist(&self, _: String) {}
    fn rename_playlist(&self, _: String, _: String) {}
    fn move_playlist(&self, _: String, _: usize) {}
    fn add_to_playlist(&self, _: String, _: Track) {}
    fn remove_from_playlist(&self, _: String, _: String) {}

    fn download_tracks(&self, _: Vec<Track>) {}
    fn remove_downloads(&self, _: Vec<String>) {}

    fn check_update(&self) {}
    fn set_update_notifications(&self, _: bool) {}
    fn open_url(&self, _: String) -> Result<(), String> {
        Ok(())
    }

    fn check_ytdlp(&self) {}
    fn set_yt_dlp_path(&self, _: Option<String>) {}
    fn download_ytdlp(&self) {}

    fn search_songs(&self, _: u64, _: String) {}
    fn search_artists(&self, _: u64, _: String) {}
    fn search_albums(&self, _: u64, _: String) {}
    fn load_artist_page(&self, _: u64, _: String) {}
    fn load_artist_songs(&self, _: u64, _: String) {}
    fn load_album_page(&self, _: u64, _: String) {}

    fn fetch_cover(&self, _: String) {}
    fn check_online(&self) {}
}

// ---------------------------------------------------------------------------
// Async completions (controller-internal)
// ---------------------------------------------------------------------------

/// The completion half of an `async` command.
///
/// Kept private: `app.rs` only ever sees the mapped [`ControllerEvent`] values,
/// after [`Controller::poll`] has applied the stale-generation and cover-dedupe
/// bookkeeping. Tests in this module can inject these directly through the
/// recording double to drive the filters deterministically.
///
/// Only the `backend`-featured live implementation (and the tests) construct
/// these; a plain no-feature preview build legitimately has none constructed, and
/// the unit-test build constructs only the variants those tests drive.
#[derive(Debug)]
#[allow(dead_code)]
enum TaskResult {
    PlayTracks(Result<(), String>),
    PlayTrack(Result<(), String>),
    SearchSongs {
        generation: u64,
        query: String,
        results: Result<Vec<Track>, String>,
    },
    SearchArtists {
        generation: u64,
        query: String,
        results: Result<Vec<ArtistSummary>, String>,
    },
    SearchAlbums {
        generation: u64,
        query: String,
        results: Result<Vec<AlbumSummary>, String>,
    },
    ArtistPage {
        generation: u64,
        result: Result<ArtistPage, String>,
    },
    ArtistSongs {
        generation: u64,
        artist_id: String,
        result: Result<Vec<Track>, String>,
    },
    AlbumPage {
        generation: u64,
        result: Result<AlbumPage, String>,
    },
    ImportYtPlaylist(Result<Playlist, String>),
    SetYtDlpPath(Result<YtDlpStatus, String>),
    DownloadYtDlp(Result<YtDlpStatus, String>),
    CheckUpdate(Result<UpdateStatus, String>),
    CheckYtDlp(YtDlpStatus),
    Online(bool),
    Cover {
        url: String,
        result: Result<CoverImage, String>,
    },
}

/// Which artist/album page a request is for.
#[derive(Clone, Debug, PartialEq, Eq)]
enum PageKind {
    Artist(String),
    ArtistSongs(String),
    Album(String),
}

/// An in-flight page request, tracked so a duplicate navigation to the page
/// that is already loading does not start a second network request.
#[derive(Clone, Debug)]
struct PageRequest {
    kind: PageKind,
}

/// Bounded id → [`Track`] cache.
///
/// Some `Action`s carry only a track id (`ToggleLikeId`, `DownloadTracks`), while
/// the backend command needs the full `Track` (it matches likes by song identity
/// and stores metadata for downloads). Every track the controller delivers —
/// bootstrap, library/queue events, search results, artist/album pages — is
/// remembered here, so those actions can be translated. The bound (oldest
/// insertion wins eviction) keeps a long browsing session from growing without
/// limit; a miss is reported to the user rather than guessed at.
#[derive(Debug, Default)]
struct TrackCache {
    tracks: HashMap<String, Track>,
    order: VecDeque<String>,
}

impl TrackCache {
    /// Maximum distinct tracks remembered. A session would need to out-browse
    /// this many unique tracks before an Action's id could miss.
    const MAX: usize = 8192;

    fn insert(&mut self, track: Track) {
        let id = track.id.clone();
        if let Some(existing) = self.tracks.get_mut(&id) {
            // Metadata can be enriched after first delivery (credits/album
            // backfill); the newest copy wins.
            *existing = track;
            return;
        }
        if self.tracks.len() >= Self::MAX {
            if let Some(oldest) = self.order.pop_front() {
                self.tracks.remove(&oldest);
            }
        }
        self.order.push_back(id.clone());
        self.tracks.insert(id, track);
    }

    fn insert_all(&mut self, tracks: &[Track]) {
        for track in tracks {
            self.insert(track.clone());
        }
    }

    fn get(&self, id: &str) -> Option<&Track> {
        self.tracks.get(id)
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.tracks.len()
    }
}

// ---------------------------------------------------------------------------
// Controller
// ---------------------------------------------------------------------------

/// Handle to the backend, owning the (optional) tokio runtime and event stream.
pub struct Controller {
    /// Whether a live backend is attached (false in preview/screenshot mode).
    live: bool,
    /// The command seam: live backend, or the inert/recording implementation.
    commands: Box<dyn BackendCommands>,
    /// Completions of async commands, drained non-blocking by [`Self::poll`].
    tasks: Receiver<TaskResult>,
    /// Events produced synchronously by [`Self::dispatch`] (command errors,
    /// derived navigation), delivered on the next [`Self::poll`].
    pending: Vec<ControllerEvent>,

    /// Generation of the newest submitted search (old `search_gen`).
    search_generation: u64,
    /// The submitted query the current search generation belongs to.
    search_query: String,
    /// The lazy artist/album sub-searches were already requested for this query.
    artists_requested: bool,
    albums_requested: bool,

    /// Generation of the newest page request (old `page_gen`); bumped by every
    /// navigation so a page reply from a page the user has left is dropped.
    page_generation: u64,
    /// The page request currently in flight, if any.
    pending_page: Option<PageRequest>,
    /// The page the current view wants, kept so `Action::RetryPage` can re-issue.
    last_page: Option<PageKind>,

    /// Mirror of the persisted library (for id-addressable sidebar actions).
    library: Library,
    /// The current track, as last announced by the backend.
    current_track: Option<Track>,
    /// Last known volume (for `Action::SaveVolume`, which carries no value).
    volume: f32,
    /// Metadata cache for Actions that carry a track id only.
    tracks: TrackCache,

    /// Cover-art in-flight/failed bookkeeping.
    covers: CoverTracker,
    /// Last observed network reachability (`None` until the first probe).
    online: Option<bool>,
    /// A reachability probe is in flight; don't stack another.
    probe_in_flight: bool,
    last_probe: Option<Instant>,

    /// The owning backend (it owns the tokio runtime, so the UI thread holds it
    /// for the whole session and drops/shuts it down on exit).
    #[cfg(feature = "backend")]
    backend: Option<rift::RiftBackend>,
    /// The backend's event bus; drained per frame.
    #[cfg(feature = "backend")]
    backend_events: Option<rift::BackendEvents>,
}

impl Controller {
    /// Construct in preview/screenshot mode: no backend, no data directory, no
    /// threads. All commands are accepted and ignored; cover requests fail
    /// immediately so the fallback tiles render.
    pub fn new_preview() -> Self {
        let (_sender, receiver) = mpsc::channel();
        Self::base(Box::new(InertCommands), receiver, false)
    }

    /// Install a custom [`BackendCommands`] implementation.
    ///
    /// **Test seam, not a production entry point.** The headless interaction
    /// tests (`crates/rift-desktop/tests/ui_flow.rs`) build a controller around a recording
    /// double to prove that real UI input (clicks, typing, keyboard shortcuts)
    /// reaches the backend bridge as the right command, with no backend, window,
    /// GPU or network. The returned controller is non-live: no event bus,
    /// rendering, runtime or reachability probe is created, so production
    /// behaviour is untouched by this constructor's existence.
    ///
    /// Production code uses [`Self::new`] (live) or [`Self::new_preview`]
    /// (screenshot mode).
    #[doc(hidden)]
    pub fn with_commands(commands: Box<dyn BackendCommands>) -> Self {
        let (_sender, receiver) = mpsc::channel();
        Self::base(commands, receiver, false)
    }

    /// Construct against the real backend. `media_hwnd` is the winit window's
    /// native handle for OS media integration (SMTC on Windows); `None` runs the
    /// backend in a degraded, headless-compatible mode.
    ///
    /// With the `backend` cargo feature disabled this always returns an error
    /// explaining how to build a live application; `main.rs` reacts by falling
    /// back to [`Self::new_preview`] so the UI still opens.
    pub fn new(media_hwnd: Option<isize>) -> anyhow::Result<Self> {
        #[cfg(feature = "backend")]
        {
            let config = rift::BackendConfig {
                data_dir: None,
                media_hwnd,
            };
            let (backend, events) = rift::RiftBackend::new(config)?;
            let (sender, receiver) = mpsc::channel();
            let commands = live::LiveCommands::new(backend.handle(), sender);
            let mut controller = Self::base(Box::new(commands), receiver, true);
            controller.backend = Some(backend);
            controller.backend_events = Some(events);
            Ok(controller)
        }
        #[cfg(not(feature = "backend"))]
        {
            let _ = media_hwnd;
            anyhow::bail!(
                "rift-desktop was built without the backend feature; \
                 run with `--features backend` for a live application"
            )
        }
    }

    /// Common constructor. `live` marks a real backend; preview controllers pass
    /// `false` so no probe is ever spawned.
    fn base(commands: Box<dyn BackendCommands>, tasks: Receiver<TaskResult>, live: bool) -> Self {
        Self {
            live,
            commands,
            tasks,
            pending: Vec::new(),
            search_generation: 0,
            search_query: String::new(),
            artists_requested: false,
            albums_requested: false,
            page_generation: 0,
            pending_page: None,
            last_page: None,
            library: Library::default(),
            current_track: None,
            volume: 1.0,
            tracks: TrackCache::default(),
            covers: CoverTracker::default(),
            online: None,
            probe_in_flight: false,
            last_probe: None,
            #[cfg(feature = "backend")]
            backend: None,
            #[cfg(feature = "backend")]
            backend_events: None,
        }
    }

    /// Whether a real backend is attached (false in preview/screenshot mode).
    pub fn is_live(&self) -> bool {
        self.live
    }

    /// Fetch the startup snapshot. Blocks briefly; called once at startup.
    ///
    /// Also seeds the controller's mirror state (volume, current track, library,
    /// track cache), runs the launch-time update check the old frontend did when
    /// notifications were enabled, and starts the first reachability probe.
    pub fn bootstrap(&mut self) -> anyhow::Result<Bootstrap> {
        if !self.live {
            anyhow::bail!("no backend attached");
        }
        let bootstrap = self
            .commands
            .bootstrap()
            .map_err(|message| anyhow::anyhow!("{message}"))?;
        self.volume = bootstrap.volume;
        self.current_track = bootstrap.track.clone();
        self.apply_library(&bootstrap.library);
        if let Some(track) = &bootstrap.track {
            self.tracks.insert(track.clone());
        }
        self.tracks.insert_all(&bootstrap.queue.tracks);
        // The old web app ran `check_update` after bootstrap when launch
        // notifications were enabled, then raised the dismissible banner from the
        // resulting status; app.rs does the banner half on `UpdateStatus`.
        if bootstrap.update_notifications {
            self.commands.check_update();
        }
        self.probe_online();
        Ok(bootstrap)
    }

    /// Drain events that arrived since the last frame. Never blocks.
    pub fn poll(&mut self) -> Vec<ControllerEvent> {
        let mut events = std::mem::take(&mut self.pending);

        // 1. Backend broadcasts (track/state/queue/progress/library/downloads/
        //    toasts/open-playlist). The receiver is temporarily taken out so the
        //    mapping methods can borrow `self` mutably.
        #[cfg(feature = "backend")]
        {
            if let Some(mut incoming) = self.backend_events.take() {
                while let Some(event) = incoming.try_recv() {
                    if let Some(mapped) = self.absorb_backend_event(event) {
                        events.push(mapped);
                    }
                }
                self.backend_events = Some(incoming);
            }
        }

        // 2. Async command completions: filter stale generations, update the
        //    controller's bookkeeping, then map to public events.
        while let Ok(task) = self.tasks.try_recv() {
            self.apply_task(task, &mut events);
        }

        // 3. Periodic, cheap reachability probe (spawned, never awaited here).
        self.maybe_probe_online();
        events
    }

    /// Translate a UI action into a backend command (or local controller action,
    /// e.g. page generation bookkeeping).
    pub fn dispatch(&mut self, action: Action) {
        use Action::*;
        match action {
            // --------------------------------------------- navigation (local)
            Navigate(view) => self.navigate_view(&view),
            OpenArtist(id) => self.navigate_view(&View::Artist(id)),
            OpenArtistSongs(id) => self.navigate_view(&View::ArtistSongs(id)),
            OpenAlbum(id) => self.navigate_view(&View::Album(id)),
            SetSearchTab(tab) => self.request_search_tab(tab),
            Search(query) => self.start_search(query),
            // UI-local: the search box text changed; `App` owns that state.
            SearchQueryChanged(_) => {}
            ToggleQueuePanel | ToggleQueueCollapsed => {}
            BeginRenamePlaylist { .. }
            | BeginNewPlaylist
            | PlaylistDraftChanged { .. }
            | ClosePlaylistEditor => {}

            // ---------------------------------------------------- playback
            PlayTracks {
                tracks,
                start,
                source,
            } => self.commands.play_tracks(tracks, start, source),
            PlayTrack(track) => self.commands.play_track(track),
            TogglePlay => self.commands.toggle_play(),
            NextTrack => self.commands.next_track(),
            PrevTrack => self.commands.prev_track(),
            Seek(position) => self.commands.seek(position),
            SetVolume(volume) => {
                self.volume = volume.clamp(0.0, 1.0);
                self.commands.set_volume(self.volume);
            }
            // The action carries no value: the slider's last `SetVolume` is the
            // value to persist (the old UI sent the release value explicitly).
            SaveVolume => self.commands.save_volume(self.volume),
            ToggleShuffle => self.commands.toggle_shuffle(),
            CycleRepeat => self.commands.cycle_repeat(),
            ToggleLike(track) => self.commands.toggle_like(track),
            ToggleLikeId(id) => self.toggle_like_id(&id),
            OpenCurrentArtist => self.open_current_artist(),

            // ------------------------------------------------------- queue
            QueueAdd(track) => self.commands.queue_add(track),
            QueuePlayNext(track) => self.commands.queue_play_next(vec![track]),
            QueueMove { from, to } => self.commands.queue_move(from, to),
            QueueRemove(index) => self.commands.queue_remove(index),
            QueueJump(index) => self.commands.queue_jump(index),
            QueueClear => self.commands.queue_clear(),

            // ----------------------------------------------------- library
            CreatePlaylist(name) => {
                if let Err(message) = self.commands.create_playlist(name) {
                    self.pending.push(ControllerEvent::Error(message));
                }
            }
            RenamePlaylist { id, name } => self.commands.rename_playlist(id, name),
            DeletePlaylist(id) => self.commands.delete_playlist(id),
            // The Action carries the sidebar row index (a UI position); the
            // backend moves by playlist id, resolved against the last library
            // event so a stale index can never move the wrong playlist. A row
            // the app no longer knows is ignored, exactly like the old UI.
            MovePlaylist { from, to } => {
                if let Some(id) = self.library.playlists.get(from).map(|p| p.id.clone()) {
                    self.commands.move_playlist(id, to);
                }
            }
            AddToPlaylist { playlist_id, track } => {
                self.commands.add_to_playlist(playlist_id, track);
            }
            RemoveFromPlaylist {
                playlist_id,
                track_id,
            } => self.commands.remove_from_playlist(playlist_id, track_id),
            ImportYtPlaylist => {
                // UI-local: opens the paste box; the submitted link arrives as
                // `ImportYtPlaylistUrl`.
            }
            ImportYtPlaylistUrl(url) => {
                let url = url.trim().to_string();
                if !url.is_empty() {
                    self.commands.import_yt_playlist(url);
                }
            }
            ImportPlaylistFile => self.commands.import_playlist(),
            ExportPlaylist(id) => self.commands.export_playlist(id),

            // ---------------------------------------------------- settings
            SetDiscordRpc(enabled) => self.commands.set_discord_rpc(enabled),
            SetCrossfade(secs) => self.commands.set_crossfade(secs),
            SetUpdateNotifications(enabled) => self.commands.set_update_notifications(enabled),
            CheckUpdate => self.commands.check_update(),
            CheckYtDlp => self.commands.check_ytdlp(),
            SetYtDlpPath(path) => self.commands.set_yt_dlp_path(path),
            DownloadYtDlp => self.commands.download_ytdlp(),

            // --------------------------------------------------- downloads
            DownloadTracks(ids) => self.request_downloads(ids),
            RemoveDownloads(ids) => self.commands.remove_downloads(ids),

            // -------------------------------------------------------- misc
            DismissUpdateBanner => {} // UI-local
            OpenUrl(url) => {
                if let Err(message) = self.commands.open_url(url) {
                    self.pending.push(ControllerEvent::Error(message));
                }
            }
            // Window commands belong to winit (`main.rs`), never the backend.
            WindowDrag | WindowMinimize | WindowToggleMaximize | WindowClose
            | WindowBeginResize(_) => {}
            Toast(_) | ToastError(_) => {} // UI-local
            RetryPage => self.retry_page(),
            SetOnline(_) => {} // dev/test hook; live probes report `Online`
        }
    }

    /// Ask the backend to fetch and decode a cover-art thumbnail.
    ///
    /// De-duplicated: at most one fetch per URL is in flight and a URL that
    /// already failed is never retried (the UI renders the fallback tile). In
    /// preview mode there is no HTTP client, so the URL fails immediately and the
    /// UI shows the fallback instead of waiting forever.
    pub fn request_cover(&mut self, url: String) {
        let url = url.trim().to_string();
        if url.is_empty() {
            return;
        }
        if !self.live {
            self.pending.push(ControllerEvent::CoverFailed(url));
            return;
        }
        match self.covers.request(&url) {
            CoverRequest::Fetch => self.commands.fetch_cover(url),
            CoverRequest::InFlight => {}
            CoverRequest::Failed => self.pending.push(ControllerEvent::CoverFailed(url)),
        }
    }

    /// Ask the backend for the latest release status.
    pub fn check_update(&mut self) {
        self.commands.check_update();
    }

    /// Ask the backend to probe for yt-dlp.
    pub fn check_ytdlp(&mut self) {
        self.commands.check_ytdlp();
    }

    /// Stop backend threads cleanly (called on exit).
    ///
    /// Drops the event bus first (so producers stop queuing), then shuts the
    /// owning backend — and with it the tokio runtime — down on this thread.
    pub fn shutdown(&mut self) {
        self.live = false;
        #[cfg(feature = "backend")]
        {
            self.backend_events = None;
            if let Some(backend) = self.backend.take() {
                backend.shutdown();
            }
        }
    }

    /// Last observed network reachability, if a probe has completed yet.
    ///
    /// The UI normally updates `State::online` from [`ControllerEvent::Online`];
    /// this accessor exists for callers that cannot wait for the next drain.
    pub fn online(&self) -> Option<bool> {
        self.online
    }

    // -----------------------------------------------------------------
    // Navigation / search request tracking
    // -----------------------------------------------------------------

    /// Called by the app when the active view changes (and by [`Self::dispatch`]
    /// for the page-opening Actions).
    ///
    /// Any navigation supersedes in-flight page requests — the old `page_gen`
    /// rule — so a page reply for a view the user has left can never be applied.
    /// Page views additionally issue the matching load, de-duplicated while the
    /// same page is already in flight.
    pub fn navigate_view(&mut self, view: &View) {
        let page = match view {
            View::Artist(id) => Some(PageKind::Artist(id.clone())),
            View::ArtistSongs(id) => Some(PageKind::ArtistSongs(id.clone())),
            View::Album(id) => Some(PageKind::Album(id.clone())),
            View::Home | View::Search | View::Liked | View::Settings | View::Playlist(_) => None,
        };
        match page {
            None => {
                self.page_generation = self.page_generation.wrapping_add(1);
                self.pending_page = None;
                self.last_page = None;
            }
            Some(kind) => {
                // Re-navigating to the page that is already loading must not
                // start a second request: the first result will fill the state
                // the app just cleared.
                if self
                    .pending_page
                    .as_ref()
                    .is_some_and(|pending| pending.kind == kind)
                {
                    self.last_page = Some(kind);
                    return;
                }
                self.request_page(kind);
            }
        }
    }

    /// Cancel an in-flight search, as the old UI did when the search box was
    /// cleared: bump the generation so no late reply can repopulate the cleared
    /// results.
    pub fn cancel_search(&mut self) {
        self.search_generation = self.search_generation.wrapping_add(1);
        self.search_query.clear();
        self.artists_requested = false;
        self.albums_requested = false;
    }

    fn start_search(&mut self, query: String) {
        self.search_generation = self.search_generation.wrapping_add(1);
        self.search_query = query.clone();
        self.artists_requested = false;
        self.albums_requested = false;
        if query.is_empty() {
            // The old titlebar refused to submit a blank query. If one is
            // dispatched anyway, resolve it as empty results so the app's
            // "Searching…" flag cannot stick forever.
            self.pending.push(ControllerEvent::SearchResults {
                query,
                results: Ok(Vec::new()),
            });
            return;
        }
        let generation = self.search_generation;
        self.commands.search_songs(generation, query);
    }

    /// Lazily fetch the artists/albums tabs for the current query (the old UI
    /// fetched them when the tab was first opened, not with the songs search).
    fn request_search_tab(&mut self, tab: SearchTab) {
        if self.search_query.is_empty() {
            return;
        }
        match tab {
            SearchTab::Songs => {}
            SearchTab::Artists => {
                if !self.artists_requested {
                    self.artists_requested = true;
                    self.commands
                        .search_artists(self.search_generation, self.search_query.clone());
                }
            }
            SearchTab::Albums => {
                if !self.albums_requested {
                    self.albums_requested = true;
                    self.commands
                        .search_albums(self.search_generation, self.search_query.clone());
                }
            }
        }
    }

    fn request_page(&mut self, kind: PageKind) {
        self.page_generation = self.page_generation.wrapping_add(1);
        let generation = self.page_generation;
        self.last_page = Some(kind.clone());
        self.pending_page = Some(PageRequest { kind: kind.clone() });
        match kind {
            PageKind::Artist(id) => self.commands.load_artist_page(generation, id),
            PageKind::ArtistSongs(id) => self.commands.load_artist_songs(generation, id),
            PageKind::Album(id) => self.commands.load_album_page(generation, id),
        }
    }

    fn retry_page(&mut self) {
        if let Some(kind) = self.last_page.clone() {
            self.request_page(kind);
        }
    }

    fn open_current_artist(&mut self) {
        let artist_id = self
            .current_track
            .as_ref()
            .and_then(|track| track.artists.iter().find_map(|artist| artist.id.clone()));
        if let Some(id) = artist_id {
            self.pending.push(ControllerEvent::OpenArtist(id));
        }
    }

    // -----------------------------------------------------------------
    // Action helpers that need controller-owned state
    // -----------------------------------------------------------------

    /// `Action::ToggleLikeId` carries only an id; the backend matches likes by
    /// song identity, so it needs the full track. Resolve it from the cache of
    /// everything the controller has delivered to the UI.
    fn toggle_like_id(&mut self, id: &str) {
        match self.tracks.get(id).cloned() {
            Some(track) => self.commands.toggle_like(track),
            None => self.pending.push(ControllerEvent::Error(format!(
                "Could not update the like: no metadata for track {id}."
            ))),
        }
    }

    /// `Action::DownloadTracks` carries track ids only; the backend stores the
    /// full metadata with the offline copy. Translate through the cache and
    /// report ids that are unknown instead of silently downloading a subset.
    fn request_downloads(&mut self, ids: Vec<String>) {
        if ids.is_empty() {
            return;
        }
        let mut tracks = Vec::with_capacity(ids.len());
        let mut missing = 0usize;
        for id in &ids {
            match self.tracks.get(id) {
                Some(track) => tracks.push(track.clone()),
                None => missing += 1,
            }
        }
        if !tracks.is_empty() {
            self.commands.download_tracks(tracks);
        }
        if missing > 0 {
            self.pending.push(ControllerEvent::Error(format!(
                "Could not start {missing} download{}: the track metadata is no longer available.",
                if missing == 1 { "" } else { "s" }
            )));
        }
    }

    // -----------------------------------------------------------------
    // Async completions
    // -----------------------------------------------------------------

    fn apply_task(&mut self, task: TaskResult, events: &mut Vec<ControllerEvent>) {
        match task {
            TaskResult::PlayTracks(result) => {
                if let Err(message) = result {
                    events.push(ControllerEvent::Error(message));
                }
            }
            TaskResult::PlayTrack(result) => {
                if let Err(message) = result {
                    events.push(ControllerEvent::Error(message));
                }
            }
            TaskResult::SearchSongs {
                generation,
                query,
                results,
            } => {
                // Stale-search guard: a reply for a query that has since been
                // replaced (or cleared) never reaches the UI.
                if generation != self.search_generation {
                    return;
                }
                if let Ok(tracks) = &results {
                    self.tracks.insert_all(tracks);
                }
                events.push(ControllerEvent::SearchResults { query, results });
            }
            TaskResult::SearchArtists {
                generation,
                query,
                results,
            } => {
                if generation != self.search_generation {
                    return;
                }
                events.push(ControllerEvent::SearchArtists { query, results });
            }
            TaskResult::SearchAlbums {
                generation,
                query,
                results,
            } => {
                if generation != self.search_generation {
                    return;
                }
                events.push(ControllerEvent::SearchAlbums { query, results });
            }
            TaskResult::ArtistPage { generation, result } => {
                if generation != self.page_generation {
                    return;
                }
                self.pending_page = None;
                match result {
                    Ok(page) => {
                        self.tracks.insert_all(&page.tracks);
                        events.push(ControllerEvent::ArtistPage { generation, page });
                    }
                    Err(message) => events.push(ControllerEvent::Error(message)),
                }
            }
            TaskResult::ArtistSongs {
                generation,
                artist_id,
                result,
            } => {
                if generation != self.page_generation {
                    return;
                }
                self.pending_page = None;
                match result {
                    Ok(tracks) => {
                        self.tracks.insert_all(&tracks);
                        events.push(ControllerEvent::ArtistSongs {
                            generation,
                            artist_id,
                            tracks,
                        });
                    }
                    Err(message) => events.push(ControllerEvent::Error(message)),
                }
            }
            TaskResult::AlbumPage { generation, result } => {
                if generation != self.page_generation {
                    return;
                }
                self.pending_page = None;
                match result {
                    Ok(page) => {
                        self.tracks.insert_all(&page.tracks);
                        events.push(ControllerEvent::AlbumPage { generation, page });
                    }
                    Err(message) => events.push(ControllerEvent::Error(message)),
                }
            }
            TaskResult::ImportYtPlaylist(result) => match result {
                Ok(playlist) => {
                    self.tracks.insert_all(&playlist.tracks);
                    // The backend emits `Library` itself; the old frontend then
                    // navigated to the new playlist and toasted the count.
                    let count = playlist.tracks.len();
                    events.push(ControllerEvent::OpenPlaylist(playlist.id.clone()));
                    events.push(ControllerEvent::Notice(format!(
                        "Imported \u{201c}{}\u{201d} ({count} song{})",
                        playlist.name,
                        if count == 1 { "" } else { "s" }
                    )));
                }
                Err(message) => events.push(ControllerEvent::Error(message)),
            },
            TaskResult::SetYtDlpPath(result) => match result {
                Ok(status) => events.push(ControllerEvent::YtDlpStatus(status)),
                // The backend keeps the old stored value on a rejected path and
                // the rejection is the interesting half; surface it as a toast.
                Err(message) => events.push(ControllerEvent::Error(message)),
            },
            TaskResult::DownloadYtDlp(result) => {
                // Failure is already toasted by the backend itself (it emits an
                // `Error` event before returning `Err`), so only the status is
                // forwarded here to avoid a duplicate toast.
                if let Ok(status) = result {
                    events.push(ControllerEvent::YtDlpStatus(status));
                }
            }
            TaskResult::CheckUpdate(result) => match result {
                Ok(status) => events.push(ControllerEvent::UpdateStatus(status)),
                Err(message) => events.push(ControllerEvent::Error(message)),
            },
            TaskResult::CheckYtDlp(status) => events.push(ControllerEvent::YtDlpStatus(status)),
            TaskResult::Online(online) => {
                self.probe_in_flight = false;
                if self.online != Some(online) {
                    self.online = Some(online);
                    events.push(ControllerEvent::Online(online));
                }
            }
            TaskResult::Cover { url, result } => {
                self.covers.finish(&url, result.is_ok());
                match result {
                    Ok(image) => events.push(ControllerEvent::Cover(image)),
                    Err(_) => events.push(ControllerEvent::CoverFailed(url)),
                }
            }
        }
    }

    // -----------------------------------------------------------------
    // Backend broadcasts and mirrors
    // -----------------------------------------------------------------

    /// Map one backend broadcast to its UI event, updating the controller's
    /// mirrors on the way (current track, library, track cache).
    #[cfg(feature = "backend")]
    fn absorb_backend_event(&mut self, event: rift::BackendEvent) -> Option<ControllerEvent> {
        use rift::BackendEvent as Event;
        match event {
            Event::Track(track) => {
                self.current_track = Some(track.clone());
                self.tracks.insert(track.clone());
                Some(ControllerEvent::Track(Some(track)))
            }
            Event::State(state) => Some(ControllerEvent::PlaybackState(state)),
            Event::Queue(queue) => {
                self.tracks.insert_all(&queue.tracks);
                Some(ControllerEvent::Queue(queue))
            }
            Event::Progress(progress) => Some(ControllerEvent::Progress(progress)),
            Event::Library(library) => {
                self.apply_library(&library);
                Some(ControllerEvent::Library(library))
            }
            Event::Downloads(downloads) => Some(ControllerEvent::Downloads(downloads)),
            Event::Error(message) => Some(ControllerEvent::Error(message)),
            Event::Notice(message) => Some(ControllerEvent::Notice(message)),
            Event::OpenPlaylist(id) => Some(ControllerEvent::OpenPlaylist(id)),
        }
    }

    fn apply_library(&mut self, library: &Library) {
        self.tracks.insert_all(&library.liked);
        self.tracks.insert_all(&library.recently_played);
        for playlist in &library.playlists {
            self.tracks.insert_all(&playlist.tracks);
        }
        self.library = library.clone();
    }

    // -----------------------------------------------------------------
    // Connectivity probe
    // -----------------------------------------------------------------

    fn maybe_probe_online(&mut self) {
        if !self.live || self.probe_in_flight {
            return;
        }
        let due = self
            .last_probe
            .is_none_or(|at| at.elapsed() >= ONLINE_PROBE_INTERVAL);
        if due {
            self.probe_online();
        }
    }

    /// Start one reachability probe (spawned; its result updates `Online`).
    fn probe_online(&mut self) {
        if !self.live || self.probe_in_flight {
            return;
        }
        self.probe_in_flight = true;
        self.last_probe = Some(Instant::now());
        self.commands.check_online();
    }
}

// ---------------------------------------------------------------------------
// Live implementation (`rift` feature)
// ---------------------------------------------------------------------------

#[cfg(feature = "backend")]
mod live {
    //! The production [`BackendCommands`](super::BackendCommands) implementation.
    //!
    //! Each async command clones the cheap `BackendHandle` (never the owning
    //! `RiftBackend`, which owns the runtime and must stay on the UI thread) and
    //! spawns a task on the backend runtime. The task sends exactly one
    //! [`TaskResult`](super::TaskResult) back, so a reply can never be lost or
    //! delivered twice.

    use std::sync::mpsc::Sender;

    use rift_types::{Bootstrap, Track};

    use super::{BackendCommands, TaskResult};
    use crate::cover::decode_cover;

    /// Production command seam: forwards to `rift::BackendHandle`.
    pub(super) struct LiveCommands {
        handle: rift::BackendHandle,
        /// Completion channel back to the controller (owned by the UI thread).
        tasks: Sender<TaskResult>,
    }

    impl LiveCommands {
        pub(super) fn new(handle: rift::BackendHandle, tasks: Sender<TaskResult>) -> Self {
            Self { handle, tasks }
        }

        /// Spawn an async command whose future resolves to one `TaskResult`.
        fn spawn_result<F>(&self, future: F)
        where
            F: std::future::Future<Output = TaskResult> + Send + 'static,
        {
            let tasks = self.tasks.clone();
            self.handle.spawn(async move {
                let result = future.await;
                // A closed channel means the app is exiting; nothing to report.
                let _ = tasks.send(result);
            });
        }

        /// Spawn a cover fetch: network on the backend runtime, decode/downscale
        /// on its blocking pool, then one completion with the decoded image.
        fn spawn_cover(&self, url: String) {
            let handle = self.handle.clone();
            let tasks = self.tasks.clone();
            self.handle.spawn(async move {
                let result = match handle.fetch_image(url.clone()).await {
                    Ok(bytes) => {
                        let decode_handle = handle.clone();
                        let decode_url = url.clone();
                        match decode_handle
                            .spawn_blocking(move || decode_cover(&decode_url, &bytes))
                            .await
                        {
                            Ok(result) => result,
                            Err(error) => Err(format!("cover decode task failed: {error}")),
                        }
                    }
                    Err(message) => Err(message),
                };
                let _ = tasks.send(TaskResult::Cover { url, result });
            });
        }
    }

    impl BackendCommands for LiveCommands {
        fn bootstrap(&self) -> Result<Bootstrap, String> {
            Ok(self.handle.bootstrap())
        }

        // ------------------------------------------------------- playback
        fn play_tracks(&self, tracks: Vec<Track>, start: usize, source: Option<String>) {
            let handle = self.handle.clone();
            self.spawn_result(async move {
                TaskResult::PlayTracks(handle.play_tracks(tracks, start, source).await)
            });
        }

        fn play_track(&self, track: Track) {
            let handle = self.handle.clone();
            self.spawn_result(async move { TaskResult::PlayTrack(handle.play_track(track).await) });
        }

        fn toggle_play(&self) {
            self.handle.toggle_play();
        }
        fn next_track(&self) {
            self.handle.next_track();
        }
        fn prev_track(&self) {
            self.handle.prev_track();
        }
        fn seek(&self, position: f64) {
            self.handle.seek(position);
        }
        fn set_volume(&self, volume: f32) {
            self.handle.set_volume(volume);
        }
        fn save_volume(&self, volume: f32) {
            self.handle.save_volume(volume);
        }
        fn set_discord_rpc(&self, enabled: bool) {
            self.handle.set_discord_rpc(enabled);
        }
        fn set_crossfade(&self, secs: f32) {
            self.handle.set_crossfade(secs);
        }
        fn toggle_shuffle(&self) {
            self.handle.toggle_shuffle();
        }
        fn cycle_repeat(&self) {
            self.handle.cycle_repeat();
        }

        // ---------------------------------------------------------- queue
        fn queue_add(&self, track: Track) {
            self.handle.queue_add(track);
        }
        fn queue_play_next(&self, tracks: Vec<Track>) {
            self.handle.queue_play_next(tracks);
        }
        fn queue_move(&self, from: usize, to: usize) {
            self.handle.queue_move(from, to);
        }
        fn queue_remove(&self, index: usize) {
            self.handle.queue_remove(index);
        }
        fn queue_jump(&self, index: usize) {
            self.handle.queue_jump(index);
        }
        fn queue_clear(&self) {
            self.handle.queue_clear();
        }

        // -------------------------------------------------------- library
        fn toggle_like(&self, track: Track) {
            self.handle.toggle_like(track);
        }
        fn create_playlist(&self, name: String) -> Result<(), String> {
            self.handle.create_playlist(name).map(|_| ())
        }
        fn import_yt_playlist(&self, url: String) {
            let handle = self.handle.clone();
            self.spawn_result(async move {
                TaskResult::ImportYtPlaylist(handle.import_yt_playlist(url).await)
            });
        }
        fn export_playlist(&self, id: String) {
            self.handle.export_playlist(id);
        }
        fn import_playlist(&self) {
            self.handle.import_playlist();
        }
        fn delete_playlist(&self, id: String) {
            self.handle.delete_playlist(id);
        }
        fn rename_playlist(&self, id: String, name: String) {
            self.handle.rename_playlist(id, name);
        }
        fn move_playlist(&self, id: String, to: usize) {
            self.handle.move_playlist(id, to);
        }
        fn add_to_playlist(&self, id: String, track: Track) {
            self.handle.add_to_playlist(id, track);
        }
        fn remove_from_playlist(&self, id: String, track_id: String) {
            self.handle.remove_from_playlist(id, track_id);
        }

        // ------------------------------------------------------ downloads
        fn download_tracks(&self, tracks: Vec<Track>) {
            self.handle.download_tracks(tracks);
        }
        fn remove_downloads(&self, ids: Vec<String>) {
            self.handle.remove_downloads(ids);
        }

        // -------------------------------------------------------- updates
        fn check_update(&self) {
            let handle = self.handle.clone();
            self.spawn_result(async move { TaskResult::CheckUpdate(handle.check_update().await) });
        }
        fn set_update_notifications(&self, enabled: bool) {
            self.handle.set_update_notifications(enabled);
        }
        fn open_url(&self, url: String) -> Result<(), String> {
            self.handle.open_url(url)
        }

        // --------------------------------------------------------- yt-dlp
        fn check_ytdlp(&self) {
            let handle = self.handle.clone();
            self.spawn_result(async move { TaskResult::CheckYtDlp(handle.check_ytdlp().await) });
        }
        fn set_yt_dlp_path(&self, path: Option<String>) {
            let handle = self.handle.clone();
            self.spawn_result(async move {
                TaskResult::SetYtDlpPath(handle.set_yt_dlp_path(path).await)
            });
        }
        fn download_ytdlp(&self) {
            let handle = self.handle.clone();
            self.spawn_result(
                async move { TaskResult::DownloadYtDlp(handle.download_ytdlp().await) },
            );
        }

        // ------------------------------------------------ search/catalog
        fn search_songs(&self, generation: u64, query: String) {
            let handle = self.handle.clone();
            self.spawn_result(async move {
                let results = handle.search(query.clone()).await;
                TaskResult::SearchSongs {
                    generation,
                    query,
                    results,
                }
            });
        }
        fn search_artists(&self, generation: u64, query: String) {
            let handle = self.handle.clone();
            self.spawn_result(async move {
                let results = handle.search_artists(query.clone()).await;
                TaskResult::SearchArtists {
                    generation,
                    query,
                    results,
                }
            });
        }
        fn search_albums(&self, generation: u64, query: String) {
            let handle = self.handle.clone();
            self.spawn_result(async move {
                let results = handle.search_albums(query.clone()).await;
                TaskResult::SearchAlbums {
                    generation,
                    query,
                    results,
                }
            });
        }
        fn load_artist_page(&self, generation: u64, id: String) {
            let handle = self.handle.clone();
            self.spawn_result(async move {
                TaskResult::ArtistPage {
                    generation,
                    result: handle.get_artist(id).await,
                }
            });
        }
        fn load_artist_songs(&self, generation: u64, id: String) {
            let handle = self.handle.clone();
            let artist_id = id.clone();
            self.spawn_result(async move {
                TaskResult::ArtistSongs {
                    generation,
                    artist_id,
                    result: handle.get_artist_songs(id).await,
                }
            });
        }
        fn load_album_page(&self, generation: u64, id: String) {
            let handle = self.handle.clone();
            self.spawn_result(async move {
                TaskResult::AlbumPage {
                    generation,
                    result: handle.get_album(id).await,
                }
            });
        }

        // --------------------------------------------------------- images
        fn fetch_cover(&self, url: String) {
            self.spawn_cover(url);
        }

        // --------------------------------------------------- connectivity
        fn check_online(&self) {
            let handle = self.handle.clone();
            self.spawn_result(async move { TaskResult::Online(handle.is_online().await) });
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use rift_types::ArtistRef;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    // ------------------------------------------------ recording double

    /// One recorded command call; enough payload to prove the translation
    /// (ids, indices, booleans) without cloning whole backend responses.
    #[derive(Clone, Debug, PartialEq)]
    enum Call {
        TogglePlay,
        NextTrack,
        PrevTrack,
        Seek(f64),
        SetVolume(f32),
        SaveVolume(f32),
        SetDiscordRpc(bool),
        SetCrossfade(f32),
        ToggleShuffle,
        CycleRepeat,
        QueueAdd(String),
        QueuePlayNext(Vec<String>),
        QueueMove(usize, usize),
        QueueRemove(usize),
        QueueJump(usize),
        QueueClear,
        ToggleLike(String),
        CreatePlaylist(String),
        ImportYtPlaylist(String),
        ExportPlaylist(String),
        ImportPlaylist,
        DeletePlaylist(String),
        RenamePlaylist(String, String),
        MovePlaylist(String, usize),
        AddToPlaylist(String, String),
        RemoveFromPlaylist(String, String),
        DownloadTracks(Vec<String>),
        RemoveDownloads(Vec<String>),
        CheckUpdate,
        SetUpdateNotifications(bool),
        OpenUrl(String),
        CheckYtDlp,
        SetYtDlpPath(Option<String>),
        DownloadYtDlp,
        PlayTracks(Vec<String>, usize, Option<String>),
        PlayTrack(String),
        SearchSongs(u64, String),
        SearchArtists(u64, String),
        SearchAlbums(u64, String),
        LoadArtistPage(u64, String),
        LoadArtistSongs(u64, String),
        LoadAlbumPage(u64, String),
        FetchCover(String),
        CheckOnline,
    }

    type Calls = Arc<Mutex<Vec<Call>>>;

    /// A [`BackendCommands`] that records every call, standing in for the live
    /// backend in unit tests.
    struct RecordingCommands {
        calls: Calls,
        bootstrap: Bootstrap,
        fail_open_url: Arc<AtomicBool>,
        fail_create_playlist: Arc<AtomicBool>,
    }

    impl BackendCommands for RecordingCommands {
        fn bootstrap(&self) -> Result<Bootstrap, String> {
            // Not a UI action, so it is not recorded as a `Call`; tests that care
            // inspect `controller.volume` / the track cache instead.
            Ok(self.bootstrap.clone())
        }

        fn play_tracks(&self, tracks: Vec<Track>, start: usize, source: Option<String>) {
            self.record(Call::PlayTracks(ids(&tracks), start, source));
        }
        fn play_track(&self, track: Track) {
            self.record(Call::PlayTrack(track.id));
        }
        fn toggle_play(&self) {
            self.record(Call::TogglePlay);
        }
        fn next_track(&self) {
            self.record(Call::NextTrack);
        }
        fn prev_track(&self) {
            self.record(Call::PrevTrack);
        }
        fn seek(&self, position: f64) {
            self.record(Call::Seek(position));
        }
        fn set_volume(&self, volume: f32) {
            self.record(Call::SetVolume(volume));
        }
        fn save_volume(&self, volume: f32) {
            self.record(Call::SaveVolume(volume));
        }
        fn set_discord_rpc(&self, enabled: bool) {
            self.record(Call::SetDiscordRpc(enabled));
        }
        fn set_crossfade(&self, secs: f32) {
            self.record(Call::SetCrossfade(secs));
        }
        fn toggle_shuffle(&self) {
            self.record(Call::ToggleShuffle);
        }
        fn cycle_repeat(&self) {
            self.record(Call::CycleRepeat);
        }
        fn queue_add(&self, track: Track) {
            self.record(Call::QueueAdd(track.id));
        }
        fn queue_play_next(&self, tracks: Vec<Track>) {
            self.record(Call::QueuePlayNext(ids(&tracks)));
        }
        fn queue_move(&self, from: usize, to: usize) {
            self.record(Call::QueueMove(from, to));
        }
        fn queue_remove(&self, index: usize) {
            self.record(Call::QueueRemove(index));
        }
        fn queue_jump(&self, index: usize) {
            self.record(Call::QueueJump(index));
        }
        fn queue_clear(&self) {
            self.record(Call::QueueClear);
        }
        fn toggle_like(&self, track: Track) {
            self.record(Call::ToggleLike(track.id));
        }
        fn create_playlist(&self, name: String) -> Result<(), String> {
            self.record(Call::CreatePlaylist(name));
            if self.fail_create_playlist.load(Ordering::Relaxed) {
                Err("playlist name cannot be empty".into())
            } else {
                Ok(())
            }
        }
        fn import_yt_playlist(&self, url: String) {
            self.record(Call::ImportYtPlaylist(url));
        }
        fn export_playlist(&self, id: String) {
            self.record(Call::ExportPlaylist(id));
        }
        fn import_playlist(&self) {
            self.record(Call::ImportPlaylist);
        }
        fn delete_playlist(&self, id: String) {
            self.record(Call::DeletePlaylist(id));
        }
        fn rename_playlist(&self, id: String, name: String) {
            self.record(Call::RenamePlaylist(id, name));
        }
        fn move_playlist(&self, id: String, to: usize) {
            self.record(Call::MovePlaylist(id, to));
        }
        fn add_to_playlist(&self, id: String, track: Track) {
            self.record(Call::AddToPlaylist(id, track.id));
        }
        fn remove_from_playlist(&self, id: String, track_id: String) {
            self.record(Call::RemoveFromPlaylist(id, track_id));
        }
        fn download_tracks(&self, tracks: Vec<Track>) {
            self.record(Call::DownloadTracks(ids(&tracks)));
        }
        fn remove_downloads(&self, ids: Vec<String>) {
            self.record(Call::RemoveDownloads(ids));
        }
        fn check_update(&self) {
            self.record(Call::CheckUpdate);
        }
        fn set_update_notifications(&self, enabled: bool) {
            self.record(Call::SetUpdateNotifications(enabled));
        }
        fn open_url(&self, url: String) -> Result<(), String> {
            self.record(Call::OpenUrl(url));
            if self.fail_open_url.load(Ordering::Relaxed) {
                Err("refusing to open non-http(s) url".into())
            } else {
                Ok(())
            }
        }
        fn check_ytdlp(&self) {
            self.record(Call::CheckYtDlp);
        }
        fn set_yt_dlp_path(&self, path: Option<String>) {
            self.record(Call::SetYtDlpPath(path));
        }
        fn download_ytdlp(&self) {
            self.record(Call::DownloadYtDlp);
        }
        fn search_songs(&self, generation: u64, query: String) {
            self.record(Call::SearchSongs(generation, query));
        }
        fn search_artists(&self, generation: u64, query: String) {
            self.record(Call::SearchArtists(generation, query));
        }
        fn search_albums(&self, generation: u64, query: String) {
            self.record(Call::SearchAlbums(generation, query));
        }
        fn load_artist_page(&self, generation: u64, id: String) {
            self.record(Call::LoadArtistPage(generation, id));
        }
        fn load_artist_songs(&self, generation: u64, id: String) {
            self.record(Call::LoadArtistSongs(generation, id));
        }
        fn load_album_page(&self, generation: u64, id: String) {
            self.record(Call::LoadAlbumPage(generation, id));
        }
        fn fetch_cover(&self, url: String) {
            self.record(Call::FetchCover(url));
        }
        fn check_online(&self) {
            self.record(Call::CheckOnline);
        }
    }

    impl RecordingCommands {
        fn record(&self, call: Call) {
            self.calls.lock().unwrap().push(call);
        }
    }

    /// A controller wired to a recording double, with helpers to inspect the
    /// recorded calls and to inject async completions.
    struct Harness {
        controller: Controller,
        calls: Calls,
        tasks: mpsc::Sender<TaskResult>,
        /// Switches the recording double's fallible commands to return `Err`.
        fail_open_url: Arc<AtomicBool>,
        fail_create_playlist: Arc<AtomicBool>,
    }

    impl Harness {
        fn new() -> Self {
            Self::with_bootstrap(Bootstrap::default())
        }

        fn with_bootstrap(bootstrap: Bootstrap) -> Self {
            let (sender, receiver) = mpsc::channel();
            let calls: Calls = Arc::new(Mutex::new(Vec::new()));
            let fail_open_url = Arc::new(AtomicBool::new(false));
            let fail_create_playlist = Arc::new(AtomicBool::new(false));
            let commands = RecordingCommands {
                calls: calls.clone(),
                bootstrap,
                fail_open_url: fail_open_url.clone(),
                fail_create_playlist: fail_create_playlist.clone(),
            };
            let mut controller = Controller::base(Box::new(commands), receiver, true);
            // Tests drive completions explicitly; keep the periodic reachability
            // probe out of the recorded call log (a dedicated test re-enables it).
            controller.probe_in_flight = true;
            Self {
                controller,
                calls,
                tasks: sender,
                fail_open_url,
                fail_create_playlist,
            }
        }

        /// Make `commands.open_url` reject the next URLs (backend scheme gate).
        fn fail_open_url(&self) {
            self.fail_open_url.store(true, Ordering::Relaxed);
        }

        /// Make `commands.create_playlist` reject the next names.
        fn fail_create_playlist(&self) {
            self.fail_create_playlist.store(true, Ordering::Relaxed);
        }

        fn take_calls(&self) -> Vec<Call> {
            std::mem::take(&mut *self.calls.lock().unwrap())
        }

        fn emit(&self, task: TaskResult) {
            self.tasks.send(task).expect("controller is alive");
        }

        fn dispatch(&mut self, action: Action) {
            self.controller.dispatch(action);
        }

        fn poll(&mut self) -> Vec<ControllerEvent> {
            self.controller.poll()
        }
    }

    fn ids(tracks: &[Track]) -> Vec<String> {
        tracks.iter().map(|t| t.id.clone()).collect()
    }

    fn track(id: &str) -> Track {
        Track {
            id: id.into(),
            title: format!("Title {id}"),
            artist: "Artist".into(),
            album: None,
            duration: Some(120),
            cover: String::new(),
            artists: vec![ArtistRef {
                id: Some(format!("artist-{id}")),
                name: "Artist".into(),
            }],
            album_id: None,
        }
    }

    fn library_with(tracks: &[&str]) -> Library {
        Library {
            liked: tracks.iter().map(|id| track(id)).collect(),
            playlists: vec![Playlist {
                id: "pl-1".into(),
                name: "Playlist".into(),
                tracks: vec![track("a")],
            }],
            recently_played: Vec::new(),
        }
    }

    // ------------------------------------------------------- dispatch map

    #[test]
    fn dispatch_maps_transport_and_volume_to_the_backend() {
        let mut h = Harness::new();
        h.dispatch(Action::TogglePlay);
        h.dispatch(Action::NextTrack);
        h.dispatch(Action::PrevTrack);
        h.dispatch(Action::Seek(42.5));
        h.dispatch(Action::SetVolume(0.75));
        h.dispatch(Action::SaveVolume);
        h.dispatch(Action::ToggleShuffle);
        h.dispatch(Action::CycleRepeat);
        assert_eq!(
            h.take_calls(),
            vec![
                Call::TogglePlay,
                Call::NextTrack,
                Call::PrevTrack,
                Call::Seek(42.5),
                Call::SetVolume(0.75),
                // SaveVolume carries no value: the last SetVolume is persisted.
                Call::SaveVolume(0.75),
                Call::ToggleShuffle,
                Call::CycleRepeat,
            ]
        );
    }

    #[test]
    fn dispatch_maps_playback_and_queue_commands() {
        let mut h = Harness::new();
        h.dispatch(Action::PlayTracks {
            tracks: vec![track("a"), track("b")],
            start: 1,
            source: Some("liked".into()),
        });
        h.dispatch(Action::PlayTrack(track("c")));
        h.dispatch(Action::QueueAdd(track("d")));
        h.dispatch(Action::QueuePlayNext(track("e")));
        h.dispatch(Action::QueueMove { from: 2, to: 0 });
        h.dispatch(Action::QueueRemove(3));
        h.dispatch(Action::QueueJump(1));
        h.dispatch(Action::QueueClear);
        assert_eq!(
            h.take_calls(),
            vec![
                Call::PlayTracks(vec!["a".into(), "b".into()], 1, Some("liked".into())),
                Call::PlayTrack("c".into()),
                Call::QueueAdd("d".into()),
                Call::QueuePlayNext(vec!["e".into()]),
                Call::QueueMove(2, 0),
                Call::QueueRemove(3),
                Call::QueueJump(1),
                Call::QueueClear,
            ]
        );
    }

    #[test]
    fn dispatch_maps_library_and_playlist_commands() {
        let mut h = Harness::with_bootstrap(Bootstrap {
            library: library_with(&["a", "b"]),
            ..Bootstrap::default()
        });
        h.controller.bootstrap().expect("bootstrap");
        h.take_calls();

        h.dispatch(Action::ToggleLike(track("b")));
        h.dispatch(Action::ToggleLikeId("a".into()));
        h.dispatch(Action::CreatePlaylist("New".into()));
        h.dispatch(Action::RenamePlaylist {
            id: "pl-1".into(),
            name: "Renamed".into(),
        });
        h.dispatch(Action::DeletePlaylist("pl-1".into()));
        // MovePlaylist carries a sidebar row index; the backend moves by id, so
        // the controller resolves index 0 against its library mirror.
        h.dispatch(Action::MovePlaylist { from: 0, to: 3 });
        h.dispatch(Action::AddToPlaylist {
            playlist_id: "pl-1".into(),
            track: track("c"),
        });
        h.dispatch(Action::RemoveFromPlaylist {
            playlist_id: "pl-1".into(),
            track_id: "a".into(),
        });
        h.dispatch(Action::ExportPlaylist("pl-1".into()));
        h.dispatch(Action::ImportPlaylistFile);
        h.dispatch(Action::ImportYtPlaylistUrl(
            "  https://x/playlist?list=PL1 ".into(),
        ));
        assert_eq!(
            h.take_calls(),
            vec![
                Call::ToggleLike("b".into()),
                // ToggleLikeId resolves through the cache of delivered tracks.
                Call::ToggleLike("a".into()),
                Call::CreatePlaylist("New".into()),
                Call::RenamePlaylist("pl-1".into(), "Renamed".into()),
                Call::DeletePlaylist("pl-1".into()),
                Call::MovePlaylist("pl-1".into(), 3),
                Call::AddToPlaylist("pl-1".into(), "c".into()),
                Call::RemoveFromPlaylist("pl-1".into(), "a".into()),
                Call::ExportPlaylist("pl-1".into()),
                Call::ImportPlaylist,
                Call::ImportYtPlaylist("https://x/playlist?list=PL1".into()),
            ]
        );
    }

    #[test]
    fn an_unknown_toggle_like_id_reports_an_error_instead_of_guessing() {
        let mut h = Harness::new();
        h.dispatch(Action::ToggleLikeId("missing".into()));
        assert!(h.take_calls().is_empty());
        let events = h.poll();
        assert!(
            matches!(&events[..], [ControllerEvent::Error(message)] if message.contains("missing")),
            "unexpected events: {events:?}"
        );
    }

    #[test]
    fn dispatch_maps_settings_update_and_ytdlp_commands() {
        let mut h = Harness::new();
        h.dispatch(Action::SetDiscordRpc(false));
        h.dispatch(Action::SetCrossfade(3.5));
        h.dispatch(Action::SetUpdateNotifications(false));
        h.dispatch(Action::CheckUpdate);
        h.dispatch(Action::CheckYtDlp);
        h.dispatch(Action::SetYtDlpPath(Some("C:\\yt-dlp.exe".into())));
        h.dispatch(Action::DownloadYtDlp);
        h.dispatch(Action::OpenUrl("https://example.com".into()));
        assert_eq!(
            h.take_calls(),
            vec![
                Call::SetDiscordRpc(false),
                Call::SetCrossfade(3.5),
                Call::SetUpdateNotifications(false),
                Call::CheckUpdate,
                Call::CheckYtDlp,
                Call::SetYtDlpPath(Some("C:\\yt-dlp.exe".into())),
                Call::DownloadYtDlp,
                Call::OpenUrl("https://example.com".into()),
            ]
        );
    }

    #[test]
    fn dispatch_maps_downloads_and_resolves_ids_through_the_track_cache() {
        let mut h = Harness::with_bootstrap(Bootstrap {
            library: library_with(&["a", "b"]),
            ..Bootstrap::default()
        });
        h.controller.bootstrap().expect("bootstrap");
        h.take_calls();

        h.dispatch(Action::DownloadTracks(vec!["a".into(), "b".into()]));
        h.dispatch(Action::RemoveDownloads(vec!["b".into()]));
        assert_eq!(
            h.take_calls(),
            vec![
                Call::DownloadTracks(vec!["a".into(), "b".into()]),
                Call::RemoveDownloads(vec!["b".into()]),
            ]
        );
    }

    #[test]
    fn dispatch_keeps_ui_local_actions_off_the_backend() {
        let mut h = Harness::new();
        h.dispatch(Action::DismissUpdateBanner);
        h.dispatch(Action::WindowMinimize);
        h.dispatch(Action::Toast("hi".into()));
        h.dispatch(Action::SetOnline(true));
        h.dispatch(Action::SearchQueryChanged("typing".into()));
        h.dispatch(Action::ImportYtPlaylist);
        h.dispatch(Action::BeginNewPlaylist);
        assert!(h.take_calls().is_empty());
    }

    #[test]
    fn fallible_commands_surface_their_error_on_the_next_poll() {
        let mut h = Harness::new();
        h.fail_open_url();
        h.dispatch(Action::OpenUrl("javascript:alert(1)".into()));
        assert_eq!(
            h.take_calls(),
            vec![Call::OpenUrl("javascript:alert(1)".into())]
        );
        let events = h.poll();
        assert!(
            matches!(&events[..], [ControllerEvent::Error(message)] if message.contains("http")),
            "unexpected events: {events:?}"
        );
    }

    #[test]
    fn a_rejected_playlist_name_is_toasted_instead_of_silently_dropped() {
        let mut h = Harness::new();
        h.fail_create_playlist();
        h.dispatch(Action::CreatePlaylist("   ".into()));
        assert_eq!(h.take_calls(), vec![Call::CreatePlaylist("   ".into())]);
        let events = h.poll();
        assert!(
            matches!(
                &events[..],
                [ControllerEvent::Error(message)] if message.contains("empty")
            ),
            "unexpected events: {events:?}"
        );
    }

    #[test]
    fn open_current_artist_resolves_the_playing_tracks_first_linkable_artist() {
        let mut h = Harness::new();
        h.emit(TaskResult::PlayTracks(Ok(())));
        h.emit(TaskResult::PlayTrack(Ok(())));
        // The current track comes from backend broadcasts; drive the mapping
        // helper directly with the controller mirror seeded the same way.
        h.controller.current_track = Some(track("live"));
        h.dispatch(Action::OpenCurrentArtist);
        let events = h.poll();
        assert!(
            matches!(
                &events[..],
                [ControllerEvent::OpenArtist(id)] if id == "artist-live"
            ),
            "unexpected events: {events:?}"
        );
    }

    // ------------------------------------------------ search generations

    #[test]
    fn search_results_for_a_superseded_query_are_dropped() {
        let mut h = Harness::new();
        h.dispatch(Action::Search("first".into()));
        h.dispatch(Action::Search("second".into()));
        assert_eq!(
            h.take_calls(),
            vec![
                Call::SearchSongs(1, "first".into()),
                Call::SearchSongs(2, "second".into()),
            ]
        );

        // The first query's (slower) reply arrives after the second was issued.
        h.emit(TaskResult::SearchSongs {
            generation: 1,
            query: "first".into(),
            results: Ok(vec![track("old")]),
        });
        assert!(
            h.poll().is_empty(),
            "a superseded reply must not reach the UI"
        );

        h.emit(TaskResult::SearchSongs {
            generation: 2,
            query: "second".into(),
            results: Ok(vec![track("new")]),
        });
        let events = h.poll();
        assert!(
            matches!(
                &events[..],
                [ControllerEvent::SearchResults { query, results }]
                    if query == "second" && results.as_ref().is_ok_and(|t| t.len() == 1)
            ),
            "unexpected events: {events:?}"
        );
    }

    #[test]
    fn clearing_the_search_box_cancels_the_in_flight_search() {
        let mut h = Harness::new();
        h.dispatch(Action::Search("query".into()));
        h.controller.cancel_search();
        h.emit(TaskResult::SearchSongs {
            generation: 1,
            query: "query".into(),
            results: Ok(vec![track("a")]),
        });
        assert!(h.poll().is_empty());
    }

    #[test]
    fn artist_and_album_tabs_are_lazily_fetched_once_per_search() {
        let mut h = Harness::new();
        h.dispatch(Action::SetSearchTab(SearchTab::Artists));
        assert!(h.take_calls().is_empty(), "no query yet, nothing to fetch");

        h.dispatch(Action::Search("daft punk".into()));
        h.dispatch(Action::SetSearchTab(SearchTab::Artists));
        h.dispatch(Action::SetSearchTab(SearchTab::Artists));
        h.dispatch(Action::SetSearchTab(SearchTab::Albums));
        h.dispatch(Action::SetSearchTab(SearchTab::Albums));
        assert_eq!(
            h.take_calls(),
            vec![
                Call::SearchSongs(1, "daft punk".into()),
                Call::SearchArtists(1, "daft punk".into()),
                Call::SearchAlbums(1, "daft punk".into()),
            ]
        );

        // A new query resets the lazy flags: the tabs fetch again for it.
        h.dispatch(Action::Search("kavinsky".into()));
        h.dispatch(Action::SetSearchTab(SearchTab::Artists));
        assert_eq!(
            h.take_calls(),
            vec![
                Call::SearchSongs(2, "kavinsky".into()),
                Call::SearchArtists(2, "kavinsky".into()),
            ]
        );
    }

    // -------------------------------------------------- page generations

    #[test]
    fn navigating_to_a_page_requests_it_at_the_current_generation() {
        let mut h = Harness::new();
        h.controller.navigate_view(&View::Artist("artist-1".into()));
        h.controller.navigate_view(&View::Album("album-1".into()));
        h.controller
            .navigate_view(&View::ArtistSongs("artist-1".into()));
        assert_eq!(
            h.take_calls(),
            vec![
                Call::LoadArtistPage(1, "artist-1".into()),
                Call::LoadAlbumPage(2, "album-1".into()),
                Call::LoadArtistSongs(3, "artist-1".into()),
            ]
        );
    }

    #[test]
    fn a_page_reply_for_an_abandoned_view_is_dropped() {
        let mut h = Harness::new();
        h.controller.navigate_view(&View::Artist("artist-1".into()));
        // The user leaves before the page arrives.
        h.controller.navigate_view(&View::Home);
        h.emit(TaskResult::ArtistPage {
            generation: 1,
            result: Ok(ArtistPage {
                id: "artist-1".into(),
                name: "Artist One".into(),
                image: String::new(),
                description: None,
                subscribers: None,
                tracks: vec![track("a")],
                albums: Vec::new(),
                tracks_playlist_id: None,
            }),
        });
        assert!(h.poll().is_empty(), "the abandoned page must not land");

        // A fresh navigation to the same page still loads it.
        h.controller.navigate_view(&View::Artist("artist-1".into()));
        assert_eq!(
            h.take_calls(),
            vec![
                Call::LoadArtistPage(1, "artist-1".into()),
                // Generation 2 was consumed leaving the page; the new request is 3.
                Call::LoadArtistPage(3, "artist-1".into()),
            ]
        );
    }

    #[test]
    fn a_duplicate_navigation_to_the_loading_page_does_not_refetch() {
        let mut h = Harness::new();
        h.controller.navigate_view(&View::Artist("artist-1".into()));
        h.controller.navigate_view(&View::Artist("artist-1".into()));
        assert_eq!(
            h.take_calls(),
            vec![Call::LoadArtistPage(1, "artist-1".into())]
        );
    }

    #[test]
    fn retry_page_reissues_the_last_page_request() {
        let mut h = Harness::new();
        h.controller.navigate_view(&View::Album("album-1".into()));
        h.dispatch(Action::RetryPage);
        assert_eq!(
            h.take_calls(),
            vec![
                Call::LoadAlbumPage(1, "album-1".into()),
                Call::LoadAlbumPage(2, "album-1".into()),
            ]
        );
    }

    #[test]
    fn a_failed_page_reports_an_error_and_retry_works() {
        let mut h = Harness::new();
        h.controller.navigate_view(&View::Artist("artist-1".into()));
        h.emit(TaskResult::ArtistPage {
            generation: 1,
            result: Err("could not load artist: offline".into()),
        });
        let events = h.poll();
        assert!(
            matches!(&events[..], [ControllerEvent::Error(message)] if message.contains("offline")),
            "unexpected events: {events:?}"
        );
        h.take_calls();
        h.dispatch(Action::RetryPage);
        assert_eq!(
            h.take_calls(),
            vec![Call::LoadArtistPage(2, "artist-1".into())]
        );
    }

    // ------------------------------------------------------- cover dedupe

    #[test]
    fn cover_requests_are_deduplicated_while_in_flight() {
        let mut h = Harness::new();
        h.controller.request_cover("https://img/1".into());
        h.controller.request_cover("https://img/1".into());
        h.controller.request_cover("  https://img/1  ".into());
        h.controller.request_cover("https://img/2".into());
        assert_eq!(
            h.take_calls(),
            vec![
                Call::FetchCover("https://img/1".into()),
                Call::FetchCover("https://img/2".into()),
            ]
        );
    }

    #[test]
    fn a_failed_cover_is_not_requested_again() {
        let mut h = Harness::new();
        h.controller.request_cover("https://img/bad".into());
        h.take_calls();
        h.emit(TaskResult::Cover {
            url: "https://img/bad".into(),
            result: Err("image fetch failed: offline".into()),
        });
        let events = h.poll();
        assert!(
            matches!(
                &events[..],
                [ControllerEvent::CoverFailed(url)] if url == "https://img/bad"
            ),
            "unexpected events: {events:?}"
        );

        h.controller.request_cover("https://img/bad".into());
        assert!(
            h.take_calls().is_empty(),
            "a known-bad URL is not refetched"
        );
        let events = h.poll();
        assert!(
            matches!(
                &events[..],
                [ControllerEvent::CoverFailed(url)] if url == "https://img/bad"
            ),
            "the UI is told to render the fallback again: {events:?}"
        );
    }

    #[test]
    fn a_decoded_cover_is_emitted_and_a_later_request_may_refetch() {
        let mut h = Harness::new();
        h.controller.request_cover("https://img/good".into());
        h.take_calls();
        h.emit(TaskResult::Cover {
            url: "https://img/good".into(),
            result: Ok(CoverImage {
                url: "https://img/good".into(),
                width: 2,
                height: 1,
                rgba: vec![0; 8],
            }),
        });
        let events = h.poll();
        assert!(
            matches!(
                &events[..],
                [ControllerEvent::Cover(image)] if image.url == "https://img/good"
            ),
            "unexpected events: {events:?}"
        );
        // The tracker no longer considers it in flight; a re-request is allowed
        // (the UI's texture cache normally prevents one).
        h.controller.request_cover("https://img/good".into());
        assert_eq!(
            h.take_calls(),
            vec![Call::FetchCover("https://img/good".into())]
        );
    }

    #[test]
    fn preview_mode_fails_cover_requests_immediately() {
        let mut controller = Controller::new_preview();
        controller.request_cover("https://img/1".into());
        assert!(matches!(
            &controller.poll()[..],
            [ControllerEvent::CoverFailed(url)] if url == "https://img/1"
        ));
    }

    // --------------------------------------------------- other completions

    #[test]
    fn a_failed_async_playback_is_reported_as_an_error() {
        let mut h = Harness::new();
        h.emit(TaskResult::PlayTracks(Err("nothing to play".into())));
        let events = h.poll();
        assert!(
            matches!(&events[..], [ControllerEvent::Error(message)] if message == "nothing to play"),
            "unexpected events: {events:?}"
        );
    }

    #[test]
    fn a_yt_playlist_import_navigates_and_toasts_like_the_old_ui() {
        let mut h = Harness::new();
        h.emit(TaskResult::ImportYtPlaylist(Ok(Playlist {
            id: "pl-9".into(),
            name: "Mix".into(),
            tracks: vec![track("a"), track("b")],
        })));
        let events = h.poll();
        assert!(
            matches!(
                &events[..],
                [
                    ControllerEvent::OpenPlaylist(id),
                    ControllerEvent::Notice(message)
                ] if id == "pl-9" && message.contains("Mix") && message.contains("2 songs")
            ),
            "unexpected events: {events:?}"
        );
    }

    #[test]
    fn reachability_changes_emit_online_only_on_transitions() {
        let mut h = Harness::new();
        h.emit(TaskResult::Online(true));
        h.emit(TaskResult::Online(true));
        h.emit(TaskResult::Online(false));
        let events = h.poll();
        assert!(
            matches!(
                &events[..],
                [
                    ControllerEvent::Online(true),
                    ControllerEvent::Online(false)
                ]
            ),
            "unexpected events: {events:?}"
        );
    }

    #[test]
    fn bootstrap_seeds_the_mirrors_and_runs_the_launch_update_check() {
        let mut h = Harness::with_bootstrap(Bootstrap {
            library: library_with(&["a"]),
            volume: 0.4,
            track: Some(track("a")),
            queue: QueueSnapshot {
                tracks: vec![track("a"), track("b")],
                ..QueueSnapshot::default()
            },
            update_notifications: true,
            ..Bootstrap::default()
        });
        h.controller.bootstrap().expect("bootstrap");
        let calls = h.take_calls();
        assert_eq!(
            calls,
            vec![Call::CheckUpdate],
            "launch update check (notifications on); probes are suppressed in tests"
        );
        assert_eq!(h.controller.volume, 0.4);
        assert_eq!(
            h.controller.current_track.as_ref().map(|t| t.id.as_str()),
            Some("a")
        );
        assert!(
            h.controller.tracks.get("b").is_some(),
            "the restored queue's tracks are cached for id-only actions"
        );
    }

    #[test]
    fn the_reachability_probe_runs_on_the_interval_and_reports_transitions() {
        let mut h = Harness::new();
        h.controller.probe_in_flight = false;
        h.controller.last_probe = None;
        // First poll is due: one probe, no result yet.
        assert!(h.poll().is_empty());
        assert_eq!(h.take_calls(), vec![Call::CheckOnline]);

        // A result arrives; the transition is emitted once.
        h.emit(TaskResult::Online(false));
        h.emit(TaskResult::Online(false));
        let events = h.poll();
        assert!(
            matches!(&events[..], [ControllerEvent::Online(false)]),
            "unexpected events: {events:?}"
        );
        // Not due again inside the interval.
        assert!(h.poll().is_empty());
        assert!(h.take_calls().is_empty(), "no probe storm");
    }

    #[test]
    fn a_track_cache_insert_updates_metadata_in_place() {
        let mut cache = TrackCache::default();
        let mut first = track("a");
        first.title = "Old".into();
        cache.insert(first);
        let mut updated = track("a");
        updated.title = "New".into();
        cache.insert(updated);
        assert_eq!(cache.len(), 1);
        assert_eq!(cache.get("a").map(|t| t.title.as_str()), Some("New"));
    }
}

// ---------------------------------------------------------------------------
// Backend-event mapping tests (feature-gated: they construct `rift::BackendEvent`)
// ---------------------------------------------------------------------------

#[cfg(all(test, feature = "backend"))]
mod backend_event_tests {
    use super::*;
    use rift::BackendEvent;
    use rift_types::RepeatMode;
    use std::sync::mpsc;

    fn controller() -> Controller {
        let (_sender, receiver) = mpsc::channel();
        Controller::base(Box::new(InertCommands), receiver, true)
    }

    fn track() -> Track {
        Track {
            id: "id".into(),
            title: "Title".into(),
            artist: "Artist".into(),
            album: None,
            duration: Some(1),
            cover: String::new(),
            artists: Vec::new(),
            album_id: None,
        }
    }

    /// Every backend broadcast keeps the old `rift://…` event's payload and maps
    /// to exactly one [`ControllerEvent`]; this is the porting contract from the
    /// legacy web event handlers, pinned rather than implied.
    #[test]
    fn every_backend_event_maps_one_to_one() {
        let mut controller = controller();

        let mapped = controller
            .absorb_backend_event(BackendEvent::Track(track()))
            .expect("track maps");
        assert!(matches!(mapped, ControllerEvent::Track(Some(t)) if t.id == "id"));
        assert_eq!(
            controller.current_track.as_ref().map(|t| t.id.as_str()),
            Some("id")
        );

        let mapped = controller
            .absorb_backend_event(BackendEvent::State(PlaybackState::Playing))
            .expect("state maps");
        assert!(matches!(
            mapped,
            ControllerEvent::PlaybackState(PlaybackState::Playing)
        ));

        let snapshot = QueueSnapshot {
            tracks: vec![track()],
            current: Some(0),
            shuffle: false,
            repeat: RepeatMode::Off,
            source: Some("liked".into()),
        };
        let mapped = controller
            .absorb_backend_event(BackendEvent::Queue(snapshot.clone()))
            .expect("queue maps");
        assert!(matches!(mapped, ControllerEvent::Queue(q) if q == snapshot));

        let progress = Progress {
            position: 1.0,
            duration: 2.0,
        };
        let mapped = controller
            .absorb_backend_event(BackendEvent::Progress(progress))
            .expect("progress maps");
        assert!(matches!(mapped, ControllerEvent::Progress(p) if p == progress));

        let mut library = Library::default();
        library.liked.push(track());
        let mapped = controller
            .absorb_backend_event(BackendEvent::Library(library.clone()))
            .expect("library maps");
        assert!(matches!(mapped, ControllerEvent::Library(l) if l == library));
        // The library mirror feeds id-resolved actions (MovePlaylist etc.).
        assert_eq!(controller.library.liked.len(), 1);

        let downloads = DownloadState::default();
        let mapped = controller
            .absorb_backend_event(BackendEvent::Downloads(downloads.clone()))
            .expect("downloads maps");
        assert!(matches!(mapped, ControllerEvent::Downloads(d) if d == downloads));

        let mapped = controller
            .absorb_backend_event(BackendEvent::Error("boom".into()))
            .expect("error maps");
        assert!(matches!(mapped, ControllerEvent::Error(m) if m == "boom"));

        let mapped = controller
            .absorb_backend_event(BackendEvent::Notice("hi".into()))
            .expect("notice maps");
        assert!(matches!(mapped, ControllerEvent::Notice(m) if m == "hi"));

        let mapped = controller
            .absorb_backend_event(BackendEvent::OpenPlaylist("pl".into()))
            .expect("open playlist maps");
        assert!(matches!(mapped, ControllerEvent::OpenPlaylist(id) if id == "pl"));
    }
}
