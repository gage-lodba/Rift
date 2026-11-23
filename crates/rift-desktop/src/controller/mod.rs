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
//! (`commands::live`) wraps `rift::BackendHandle`; the tests use a recording double, so
//! the whole Action→command mapping, the generation filters, cover de-duplication
//! and event mapping are unit-testable with no backend, network or window.
//!
//! # Module layout
//!
//! The controller is split along its seams so each file owns one concern:
//!
//! * `commands.rs` — the [`BackendCommands`] trait, the inert implementation
//!   used by preview/screenshot mode, and the `backend`-gated `live`
//!   implementation that forwards to `rift::BackendHandle`.
//! * `tasks.rs` — the private `TaskResult` completion type and the per-task
//!   mapping [`Controller::poll`] applies (stale-generation filters and the
//!   cover/probe bookkeeping included).
//! * `cover.rs` — [`Controller::request_cover`]'s de-duplication path
//!   (`CoverImage`/`CoverTracker` themselves live in `crate::cover`).
//! * `tests.rs` and `backend_event_tests.rs` — the unit tests, so the
//!   production modules never compile the recording doubles.
//!
//! This file keeps the [`Controller`] core — construction, bootstrap, polling,
//! dispatch, navigation/search generation counters, cancellation, shutdown and
//! the connectivity probe — because those all touch the struct's private state.
//!
//! [`dispatch`]: Controller::dispatch

use std::collections::{HashMap, VecDeque};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use rift_types::{
    AlbumPage, AlbumSummary, ArtistPage, ArtistSummary, Bootstrap, DownloadState, Library,
    PlaybackState, Progress, QueueSnapshot, Track, UpdateStatus,
};

pub use self::commands::BackendCommands;
use self::commands::InertCommands;
#[cfg(feature = "backend")]
use self::commands::LiveCommands;
use self::tasks::TaskResult;
pub use crate::cover::CoverImage;
use crate::cover::CoverTracker;
use crate::state::{Action, SearchTab, View};

mod commands;
mod cover;
mod tasks;

/// How often the controller probes network reachability. The old frontend got
/// this for free from the webview's `online`/`offline` events; the native app
/// asks the backend for one cheap HEAD request every ten seconds.
const ONLINE_PROBE_INTERVAL: Duration = Duration::from_secs(10);

// `CoverImage` lives in `crate::cover`; it is re-exported here because it is part of
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
    /// One in-flight download's fraction advanced; the status sets did not
    /// change (lifecycle transitions still arrive as [`Self::Downloads`]).
    /// Carried separately so a progress tick never clones the whole state.
    DownloadProgress {
        id: String,
        permille: u16,
    },
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
    ///
    /// Like the search replies, the result is carried instead of only the
    /// success value: a failed load becomes an `Err(message)` for the same
    /// request, so the app can render a retryable page-error state instead of a
    /// toast that leaves the view on "Loading…" forever. `artist_id` identifies
    /// which page failed, so a stale error can never be applied to another view.
    ArtistPage {
        generation: u64,
        artist_id: String,
        result: Result<ArtistPage, String>,
    },
    /// The "Show all songs" catalog for an artist finished loading (or failed;
    /// see [`ControllerEvent::ArtistPage`]).
    ArtistSongs {
        generation: u64,
        artist_id: String,
        result: Result<Vec<Track>, String>,
    },
    /// A requested album page finished loading (or failed; see
    /// [`ControllerEvent::ArtistPage`]).
    AlbumPage {
        generation: u64,
        album_id: String,
        result: Result<AlbumPage, String>,
    },
    /// Result of a requested update check.
    UpdateStatus(UpdateStatus),
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
    /// The active search sub-tab (mirrors `SearchState::tab`). A new query
    /// requests this tab's results too, because the app resets the lazy lists
    /// to `None` on every submit; otherwise an open Artists/Albums tab would
    /// stay on "Searching..." until the user clicked a different tab and back.
    active_search_tab: SearchTab,

    /// Generation of the newest page request (old `page_gen`); bumped by every
    /// navigation so a page reply from a page the user has left is dropped.
    page_generation: u64,
    /// The page request currently in flight, if any.
    pending_page: Option<PageRequest>,
    /// The page the current view wants, kept so `Action::RetryPage` can re-issue.
    last_page: Option<PageKind>,

    /// Mirror of the persisted library (for id-addressable sidebar actions).
    library: Library,
    /// Mirror of the backend's download state.
    ///
    /// The backend sends lifecycle transitions as whole [`DownloadState`]
    /// snapshots and throttled per-chunk fractions as
    /// [`rift::BackendEvent::DownloadProgress`]. Fractions are applied to this
    /// mirror and forwarded as [`ControllerEvent::DownloadProgress`] (`id` +
    /// permille) so a tick never clones the status sets; only lifecycle
    /// transitions carry a whole snapshot.
    downloads: DownloadState,
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
    ///
    /// # Errors
    ///
    /// Returns the backend's construction error when the data directory cannot
    /// be resolved/created, or the runtime/HTTP client cannot be built. Callers
    /// fall back to [`Self::new_preview`] and toast the message, so the window
    /// still opens without a live backend.
    pub fn new(media_hwnd: Option<isize>) -> anyhow::Result<Self> {
        #[cfg(feature = "backend")]
        {
            let config = rift::BackendConfig {
                data_dir: None,
                media_hwnd,
            };
            let (backend, events) = rift::RiftBackend::new(config)?;
            let (sender, receiver) = mpsc::channel();
            let commands = LiveCommands::new(backend.handle(), sender);
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
            active_search_tab: SearchTab::Songs,
            page_generation: 0,
            pending_page: None,
            last_page: None,
            library: Library::default(),
            downloads: DownloadState::default(),
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
    ///
    /// # Errors
    ///
    /// Fails for a preview/screenshot controller (no backend attached) and when
    /// the backend cannot produce its launch snapshot; `app.rs` toasts the
    /// message and keeps running with the default state.
    pub fn bootstrap(&mut self) -> anyhow::Result<Bootstrap> {
        if !self.live {
            anyhow::bail!("no backend attached");
        }
        let bootstrap = self
            .commands
            .bootstrap()
            .map_err(|message| anyhow::anyhow!("{message}"))?;
        self.volume = bootstrap.volume;
        self.downloads = bootstrap.downloads.clone();
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
                    let mapped = self.absorb_backend_event(event);
                    events.push(mapped);
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
            // UI-local: the search box text/focus changed; `App` owns that state.
            SearchQueryChanged(_) | SearchBlur => {}
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

    /// Ask the backend for the latest release status.
    pub fn check_update(&mut self) {
        self.commands.check_update();
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
        // A new query clears the app's lazy artist/album results (`None`), so
        // the tab the user is looking at would spin forever. Fetch it now —
        // `Songs` needs nothing beyond the request above, and the
        // `*_requested` flags keep a later `SetSearchTab` from duplicating it.
        if self.active_search_tab != SearchTab::Songs {
            self.request_search_tab(self.active_search_tab);
        }
    }

    /// Lazily fetch the artists/albums tabs for the current query (the old UI
    /// fetched them when the tab was first opened, not with the songs search).
    /// Also remembers the active tab so a new query can refresh it (see
    /// [`Self::start_search`]).
    fn request_search_tab(&mut self, tab: SearchTab) {
        self.active_search_tab = tab;
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
    // Backend broadcasts and mirrors
    // -----------------------------------------------------------------

    /// Map one backend broadcast to its UI event, updating the controller's
    /// mirrors on the way (current track, library, track cache).
    ///
    /// Every backend event currently has a UI counterpart, so the mapping is
    /// total; a future backend-only event would have to decide explicitly what
    /// the app should do with it.
    #[cfg(feature = "backend")]
    fn absorb_backend_event(&mut self, event: rift::BackendEvent) -> ControllerEvent {
        use rift::BackendEvent as Event;
        match event {
            Event::Track(track) => {
                self.current_track = Some(track.clone());
                self.tracks.insert(track.clone());
                ControllerEvent::Track(Some(track))
            }
            Event::State(state) => ControllerEvent::PlaybackState(state),
            Event::Queue(queue) => {
                self.tracks.insert_all(&queue.tracks);
                ControllerEvent::Queue(queue)
            }
            Event::Progress(progress) => ControllerEvent::Progress(progress),
            Event::Library(library) => {
                self.apply_library(&library);
                ControllerEvent::Library(library)
            }
            Event::Downloads(downloads) => {
                self.downloads = downloads.clone();
                ControllerEvent::Downloads(downloads)
            }
            Event::DownloadProgress { id, permille } => {
                // Per-chunk progress is not a lifecycle transition: update the
                // mirror and forward only the fraction. Cloning the whole
                // `DownloadState` here (as an earlier version did) re-cloned the
                // status sets on every 100 ms tick, which is exactly what this
                // event exists to avoid.
                self.downloads.progress.insert(id.clone(), permille);
                ControllerEvent::DownloadProgress { id, permille }
            }
            Event::Error(message) => ControllerEvent::Error(message),
            Event::Notice(message) => ControllerEvent::Notice(message),
            Event::OpenPlaylist(id) => ControllerEvent::OpenPlaylist(id),
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
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests;

// ---------------------------------------------------------------------------
// Backend-event mapping tests (feature-gated: they construct `rift::BackendEvent`)
// ---------------------------------------------------------------------------

#[cfg(all(test, feature = "backend"))]
mod backend_event_tests;
