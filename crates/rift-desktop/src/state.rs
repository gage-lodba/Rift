//! Application and UI state for the native Rift UI.
//!
//! This is a direct port of the state the legacy web app owned: everything
//! the old frontend kept in `use_state`, plus the small amount of extra state a
//! native immediate-mode UI needs (cover-art textures, toast expiry, sidebar
//! width). Screens read `&State` and push [`Action`]s; they never call the
//! backend and never mutate state themselves.
//!
//! Action application lives in `app.rs` + `controller.rs`; the backend bridge
//! turns [`Action`]s into backend commands and backend events back into state
//! updates.

use std::collections::{HashMap, HashSet};

use rift_types::{
    AlbumPage, AlbumSummary, ArtistPage, ArtistSummary, DownloadState, Library, PlaybackState,
    Playlist, Progress, QueueSnapshot, RepeatMode, SongIndex, Track, UpdateStatus,
};

use crate::gfx::paint::TextureId;

/// The main view routed to by the sidebar and search.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum View {
    Home,
    Search,
    Liked,
    Settings,
    Playlist(String),
    Artist(String),
    /// All songs by an artist; the inner value is the artist ID.
    ArtistSongs(String),
    Album(String),
}

impl View {
    /// Where the sidebar highlight should point while this view is active.
    pub fn nav_view(&self) -> View {
        self.clone()
    }
}

/// Search result tabs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SearchTab {
    #[default]
    Songs,
    Artists,
    Albums,
}

/// State of the search box and its three result lists.
#[derive(Clone, Debug, Default)]
pub struct SearchState {
    /// Text currently in the titlebar search box.
    pub query: String,
    /// The query the displayed results belong to (so the header says
    /// `Results for "…"` rather than the live box contents).
    pub shown_query: String,
    pub songs: Vec<Track>,
    pub artists: Option<Vec<ArtistSummary>>,
    pub albums: Option<Vec<AlbumSummary>>,
    /// A search command is in flight; screens show a spinner / "Searching…".
    pub busy: bool,
    pub tab: SearchTab,
    /// Bumped on every submitted search so a stale response can be dropped.
    ///
    /// The controller owns the authoritative request generations (it is the half
    /// that issues the requests and receives the replies, and it drops a reply
    /// for an older generation before it ever reaches the app). This counter is
    /// bumped by `App::dispatch_ui` on every submitted search so screens can key
    /// "Searching…" on the query too.
    pub generation: u64,
}

/// One cached cover art thumbnail.
#[derive(Clone, Debug, Default)]
pub struct CoverEntry {
    pub texture: Option<TextureId>,
    /// A fetch/decode is in flight or queued.
    pub requested: bool,
    /// The URL failed to load; render the fallback tile and stop retrying.
    pub failed: bool,
}

/// A short-lived toast. `shown_at` is `Ui` time (seconds) when it appeared, so
/// `app.rs` can expire it after [`Toast::LIFETIME`].
#[derive(Clone, Debug)]
pub struct Toast {
    pub message: String,
    pub shown_at: f64,
    pub is_error: bool,
}

impl Toast {
    pub const LIFETIME: f64 = 6.0;
    pub const LIFETIME_NOTICE: f64 = 5.0;

    pub fn error(message: impl Into<String>, now: f64) -> Self {
        Self {
            message: message.into(),
            shown_at: now,
            is_error: true,
        }
    }

    pub fn notice(message: impl Into<String>, now: f64) -> Self {
        Self {
            message: message.into(),
            shown_at: now,
            is_error: false,
        }
    }

    pub fn is_expired(&self, now: f64) -> bool {
        let lifetime = if self.is_error {
            Self::LIFETIME
        } else {
            Self::LIFETIME_NOTICE
        };
        now - self.shown_at > lifetime
    }
}

/// Everything the UI renders.
#[derive(Clone, Debug)]
pub struct State {
    // ---------------------------------------------------- backend data
    pub library: Library,
    pub queue: QueueSnapshot,
    pub playback: PlaybackState,
    /// The currently loaded/playing track, if any.
    pub track: Option<Track>,
    /// Live playback position; refreshed by progress events at ~4 Hz.
    pub progress: Progress,
    pub volume: f32,
    pub discord_rpc: bool,
    pub crossfade: f32,
    pub update_notifications: bool,
    pub downloads: DownloadState,
    /// Launch-time update check result (drives the dismissible banner).
    pub update_banner: Option<UpdateStatus>,
    /// Cached update check result for the Settings view.
    pub update_status: Option<UpdateStatus>,
    /// Network reachability; offline greys out un-downloaded tracks.
    pub online: bool,
    /// Dev-only placeholder-data mode (`RIFT_PREVIEW=1`).
    pub preview: bool,

    // ------------------------------------------------------- navigation
    pub view: View,
    pub search: SearchState,
    /// Artist page cache for the current `View::Artist`.
    pub artist_page: Option<ArtistPage>,
    /// Full song catalog for "Show all songs": `(artist_id, tracks)`.
    pub artist_songs: Option<(String, Vec<Track>)>,
    /// Album page cache for the current `View::Album`.
    pub album_page: Option<AlbumPage>,
    /// Generation of the newest artist/album page request the controller issued,
    /// mirrored from [`crate::controller::ControllerEvent`] page results.
    ///
    /// `app.rs` keeps it monotonic and ignores a page event older than it: the
    /// controller already drops superseded responses (it bumped the generation
    /// when it replaced the request), so this is the app-side half of the old
    /// frontend's `page_gen` rule — a late page from a previous navigation can
    /// never replace the page the user is looking at, even if a controller
    /// filter is ever bypassed. `0` means "no page has loaded yet".
    pub page_generation: u64,

    // ------------------------------------------------- transient UI state
    /// `(playlist id, draft name)` while a playlist is being renamed.
    pub renaming: Option<(String, String)>,
    /// Draft name while the "New playlist" input is open; `Some("")` = open.
    pub new_playlist: Option<String>,
    /// Draft text of the sidebar "import playlist from a `YouTube` Music link/ID"
    /// paste box; `Some("")` = the box is open (mirrors the old UI's
    /// `.side-new-input`). UI-local, like [`Self::new_playlist`]: the chrome
    /// screen owns opening/editing/closing it and submits the final value as
    /// [`Action::ImportYtPlaylistUrl`].
    pub import_playlist: Option<String>,
    /// The newest toast, if one is on screen.
    pub toast: Option<Toast>,
    /// Sidebar width in logical px (drag-resize handle persists it).
    pub sidebar_width: f32,
    /// Whether the OS window is currently maximized (titlebar icon).
    pub window_maximized: bool,

    // ------------------------------------------------------- cover cache
    /// Cover-art texture cache keyed by URL.
    ///
    /// Bounded in practice: `app.rs` evicts the oldest decoded textures once its
    /// `MAX_CACHED_COVERS` cap is reached (each 256 px RGBA8 cover is 256 KiB of
    /// VRAM), so a long browsing session cannot grow the cache without limit.
    /// Entries without a texture (in flight or known-failed) are a few bytes and
    /// are left alone by eviction.
    pub covers: HashMap<String, CoverEntry>,
    /// URLs the controller should fetch (deduplicated; drained each frame).
    pub cover_requests: Vec<String>,
    /// Next texture id to hand out for cover art (font atlas uses low ids).
    pub next_texture_id: TextureId,
}

impl Default for State {
    fn default() -> Self {
        Self {
            library: Library::default(),
            queue: QueueSnapshot::default(),
            playback: PlaybackState::Stopped,
            track: None,
            progress: Progress::default(),
            volume: 1.0,
            discord_rpc: true,
            crossfade: 0.0,
            update_notifications: true,
            downloads: DownloadState::default(),
            update_banner: None,
            update_status: None,
            online: true,
            preview: false,

            view: View::Home,
            search: SearchState::default(),
            artist_page: None,
            artist_songs: None,
            album_page: None,
            page_generation: 0,

            renaming: None,
            new_playlist: None,
            import_playlist: None,
            toast: None,
            sidebar_width: crate::theme::Theme::default().sidebar_width,
            window_maximized: false,

            covers: HashMap::new(),
            cover_requests: Vec::new(),
            next_texture_id: 1024,
        }
    }
}

impl State {
    /// Apply a backend bootstrap snapshot.
    pub fn apply_bootstrap(&mut self, b: rift_types::Bootstrap) {
        self.library = b.library;
        self.queue = b.queue;
        self.playback = b.state;
        self.volume = b.volume;
        self.track = b.track;
        self.progress = b.progress;
        self.downloads = b.downloads;
        self.discord_rpc = b.discord_rpc;
        self.crossfade = b.crossfade;
        self.update_notifications = b.update_notifications;
        self.preview = b.preview;
    }

    /// The playing collection's source tag (`"liked"`, `"playlist:<id>"`, …),
    /// only while a track is actually loaded.
    pub fn playing_source(&self) -> Option<&str> {
        match self.playback {
            PlaybackState::Playing | PlaybackState::Paused | PlaybackState::Loading => {
                self.queue.source.as_deref()
            }
            PlaybackState::Stopped => None,
        }
    }

    pub fn is_playing(&self) -> bool {
        self.playback == PlaybackState::Playing
    }

    /// A lookup for whether a song is liked, by song identity rather than video
    /// id (the old UI matched `Track::same_song`, and so must this).
    pub fn liked_index(&self) -> SongIndex {
        SongIndex::new(&self.library.liked)
    }

    pub fn is_liked(&self, liked: &SongIndex, track: &Track) -> bool {
        liked.contains(track)
    }

    pub fn playlist(&self, id: &str) -> Option<&Playlist> {
        self.library.playlists.iter().find(|p| p.id == id)
    }

    /// `(id, name, track count)` for the sidebar playlist rows.
    pub fn sidebar_playlists(&self) -> Vec<(String, String, usize)> {
        self.library
            .playlists
            .iter()
            .map(|p| (p.id.clone(), p.name.clone(), p.tracks.len()))
            .collect()
    }

    /// `(id, name)` options for the "add to playlist" submenu.
    pub fn playlist_options(&self) -> Vec<(String, String)> {
        self.library
            .playlists
            .iter()
            .map(|p| (p.id.clone(), p.name.clone()))
            .collect()
    }

    /// Tracks whose offline copy is pinned by a fully downloaded playlist; the
    /// backend refuses to remove those individually.
    pub fn pinned_ids(&self) -> HashSet<String> {
        self.library
            .playlists
            .iter()
            .filter(|p| playlist_fully_downloaded(p, &self.downloads.downloaded))
            .flat_map(|p| p.tracks.iter().map(|t| t.id.clone()))
            .collect()
    }

    /// The downloaded-state of a track for row rendering.
    pub fn download_status(&self, track: &Track) -> DownloadStatus {
        if self.downloads.downloaded.contains(&track.id) {
            DownloadStatus::Downloaded
        } else if self.downloads.downloading.contains(&track.id) {
            DownloadStatus::Downloading
        } else if self.downloads.failed.contains(&track.id) {
            DownloadStatus::Failed
        } else {
            DownloadStatus::None
        }
    }

    /// Known byte-level fraction (`0.0..=1.0`) of a track's in-flight download.
    ///
    /// `None` means the fraction is unknown: the track is queued behind the one
    /// actively transferring (the native downloader reports progress only for the
    /// active track). Rows keep their indeterminate spinner in that case. The
    /// backend reports permille (`0..=1000`); it is clamped here so a malformed
    /// snapshot can never paint a determinate bar past the end of its track.
    pub fn download_fraction(&self, track_id: &str) -> Option<f32> {
        self.downloads
            .progress
            .get(track_id)
            .map(|permille| f32::from(*permille).clamp(0.0, 1000.0) / 1000.0)
    }

    /// Cover texture for a URL, if it has finished loading.
    pub fn cover_texture(&self, url: &str) -> Option<TextureId> {
        self.covers.get(url).and_then(|c| c.texture)
    }

    /// Mark a cover URL as needed; the controller fetches it if it has not been
    /// requested yet. Returns the texture if already available.
    pub fn request_cover(&mut self, url: &str) -> Option<TextureId> {
        if url.is_empty() {
            return None;
        }
        let entry = self.covers.entry(url.to_string()).or_default();
        if let Some(id) = entry.texture {
            return Some(id);
        }
        if !entry.requested && !entry.failed {
            entry.requested = true;
            self.cover_requests.push(url.to_string());
        }
        None
    }
}

/// Per-row download indicator state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DownloadStatus {
    None,
    Downloading,
    Downloaded,
    Failed,
}

/// Every track of `p` is downloaded (and it has any): the playlist pins its
/// tracks' offline copies, which then can't be removed individually.
pub fn playlist_fully_downloaded(p: &Playlist, downloaded: &HashSet<String>) -> bool {
    !p.tracks.is_empty() && p.tracks.iter().all(|t| downloaded.contains(&t.id))
}

/// Human-readable total duration for a collection subtitle, e.g. "1 hr 23 min".
pub fn fmt_total_duration(secs: u32) -> String {
    let (h, m) = (secs / 3600, (secs % 3600) / 60);
    if h > 0 {
        format!("{h} hr {m} min")
    } else if m > 0 {
        format!("{m} min")
    } else {
        format!("{secs} sec")
    }
}

/// Format seconds as `m:ss` (or `h:mm:ss`).
pub fn fmt_time(secs: f64) -> String {
    let total = secs.max(0.0).floor() as u64;
    let (h, m, s) = (total / 3600, (total % 3600) / 60, total % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// Displayed name for the current view (used by the titlebar/sidebar).
pub fn view_title(state: &State) -> String {
    match &state.view {
        View::Home => "Home".into(),
        View::Search => "Search".into(),
        View::Liked => "Liked Songs".into(),
        View::Settings => "Settings".into(),
        View::Playlist(id) => state
            .playlist(id)
            .map(|p| p.name.clone())
            .unwrap_or_else(|| "Playlist".into()),
        View::Artist(id) => state
            .artist_page
            .as_ref()
            .filter(|p| &p.id == id)
            .map(|p| p.name.clone())
            .unwrap_or_else(|| "Artist".into()),
        View::ArtistSongs(id) => state
            .artist_page
            .as_ref()
            .filter(|p| &p.id == id)
            .map(|p| p.name.clone())
            .unwrap_or_else(|| "Artist".into()),
        View::Album(id) => state
            .album_page
            .as_ref()
            .filter(|p| &p.id == id)
            .map(|p| p.name.clone())
            .unwrap_or_else(|| "Album".into()),
    }
}

// ---------------------------------------------------------------------------
// Actions
// ---------------------------------------------------------------------------

/// Something the user did that needs the controller or backend. Screens return
/// these instead of touching state or IPC directly, so the UI stays testable and
/// the backend boundary stays in one place.
#[derive(Clone, Debug)]
pub enum Action {
    // --------------------------------------------------------- navigation
    Navigate(View),
    OpenArtist(String),
    OpenArtistSongs(String),
    OpenAlbum(String),
    SetSearchTab(SearchTab),
    /// Submit the current search query.
    Search(String),
    /// Update the search box text (no command).
    SearchQueryChanged(String),
    ToggleQueuePanel,
    ToggleQueueCollapsed,

    // ---------------------------------------------------------- playback
    /// Replace the queue with `tracks`, starting at `start`, tagged `source`.
    PlayTracks {
        tracks: Vec<Track>,
        start: usize,
        source: Option<String>,
    },
    /// Replace the queue with a single track (radio-style play).
    PlayTrack(Track),
    TogglePlay,
    NextTrack,
    PrevTrack,
    Seek(f64),
    SetVolume(f32),
    SaveVolume,
    ToggleShuffle,
    CycleRepeat,
    ToggleLike(Track),
    OpenCurrentArtist,

    // ------------------------------------------------------------- queue
    QueueAdd(Track),
    QueuePlayNext(Track),
    QueueMove {
        from: usize,
        to: usize,
    },
    QueueRemove(usize),
    QueueJump(usize),
    QueueClear,

    // ----------------------------------------------------------- library
    ToggleLikeId(String),
    CreatePlaylist(String),
    RenamePlaylist {
        id: String,
        name: String,
    },
    DeletePlaylist(String),
    /// Move a sidebar playlist row (`from` -> `to`).
    MovePlaylist {
        from: usize,
        to: usize,
    },
    AddToPlaylist {
        playlist_id: String,
        track: Track,
    },
    RemoveFromPlaylist {
        playlist_id: String,
        track_id: String,
    },
    ImportYtPlaylist,
    /// Import a `YouTube` Music playlist from a pasted link/ID.
    ///
    /// The old UI had an inline paste box in the sidebar and invoked
    /// `import_yt_playlist` with its value; this variant is the native
    /// equivalent, while bare [`Action::ImportYtPlaylist`] stays UI-local ("open
    /// the paste box"). The controller spawns the network import and reports the
    /// result through `ControllerEvent::OpenPlaylist` + `Notice` / `Error`.
    ImportYtPlaylistUrl(String),
    ImportPlaylistFile,
    ExportPlaylist(String),
    /// Open the rename input for a playlist (local state only).
    BeginRenamePlaylist {
        id: String,
        name: String,
    },
    /// Open the "new playlist" input (local state only).
    BeginNewPlaylist,
    /// Update the draft name of the open rename/new-playlist input (local).
    PlaylistDraftChanged {
        id: Option<String>,
        name: String,
    },
    /// Close the open playlist name input (local).
    ClosePlaylistEditor,

    // ---------------------------------------------------------- settings
    SetDiscordRpc(bool),
    SetCrossfade(f32),
    SetUpdateNotifications(bool),
    CheckUpdate,

    // --------------------------------------------------------- downloads
    DownloadTracks(Vec<String>),
    RemoveDownloads(Vec<String>),

    // ------------------------------------------------------------ update
    DismissUpdateBanner,
    OpenUrl(String),

    // ------------------------------------------------------------- window
    WindowDrag,
    WindowMinimize,
    WindowToggleMaximize,
    WindowClose,
    WindowBeginResize(ResizeEdge),

    // -------------------------------------------------------------- misc
    /// Show a temporary toast (used for local validation messages).
    Toast(String),
    ToastError(String),
    /// Retry loading the currently open page.
    RetryPage,
    /// Toggle the "offline" indicator (dev/test hook).
    SetOnline(bool),
}

/// Which window edge/corner is being dragged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResizeEdge {
    Left,
    Right,
    Top,
    Bottom,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

/// The action for a "play this collection" button, mirroring the old UI: if the
/// collection is already loaded, toggle playback instead of restarting it; with
/// shuffle on, start at a random track. `random` is passed in so this is
/// deterministic and testable.
pub fn play_collection_action(
    state: &State,
    tracks: Vec<Track>,
    source: Option<String>,
    random: f64,
) -> Action {
    let is_current = source.is_some()
        && source == state.queue.source
        && matches!(
            state.playback,
            PlaybackState::Playing | PlaybackState::Paused | PlaybackState::Loading
        );
    if is_current {
        return Action::TogglePlay;
    }
    let start = if state.queue.shuffle && tracks.len() > 1 {
        shuffled_start(random, tracks.len())
    } else {
        0
    };
    Action::PlayTracks {
        tracks,
        start,
        source,
    }
}

/// Where a shuffled "play this collection" click starts. Never returns `len`:
/// the backend rejects `start >= tracks.len()` with "nothing to play".
pub fn shuffled_start(random: f64, len: usize) -> usize {
    if len == 0 {
        return 0;
    }
    let scaled = random * len as f64;
    if scaled >= len as f64 {
        return len - 1;
    }
    (scaled.floor().max(0.0) as usize).min(len - 1)
}

/// The repeat-mode button cycles Off -> All -> One -> Off.
pub fn next_repeat(mode: RepeatMode) -> RepeatMode {
    match mode {
        RepeatMode::Off => RepeatMode::All,
        RepeatMode::All => RepeatMode::One,
        RepeatMode::One => RepeatMode::Off,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shuffled_start_never_returns_len() {
        assert_eq!(shuffled_start(0.0, 0), 0);
        assert_eq!(shuffled_start(0.999_999, 10), 9);
        assert_eq!(shuffled_start(1.0, 10), 9);
        assert_eq!(shuffled_start(-0.5, 10), 0);
    }

    #[test]
    fn repeat_cycles() {
        assert_eq!(next_repeat(RepeatMode::Off), RepeatMode::All);
        assert_eq!(next_repeat(RepeatMode::All), RepeatMode::One);
        assert_eq!(next_repeat(RepeatMode::One), RepeatMode::Off);
    }

    #[test]
    fn fmt_time_formats() {
        assert_eq!(fmt_time(0.0), "0:00");
        assert_eq!(fmt_time(83.2), "1:23");
        assert_eq!(fmt_time(214.0), "3:34");
        assert_eq!(fmt_time(3661.0), "1:01:01");
    }

    #[test]
    fn toast_expiry() {
        let t = Toast::notice("hi", 10.0);
        assert!(!t.is_expired(11.0));
        assert!(t.is_expired(16.0));
    }

    #[test]
    fn download_fraction_maps_permille_to_a_clamped_unit_interval() {
        let mut state = State::default();
        assert_eq!(state.download_fraction("t1"), None, "absent = unknown");

        state.downloads.progress.insert("t1".into(), 0);
        assert_eq!(state.download_fraction("t1"), Some(0.0));
        state.downloads.progress.insert("t1".into(), 250);
        assert_eq!(state.download_fraction("t1"), Some(0.25));
        state.downloads.progress.insert("t1".into(), 1000);
        assert_eq!(state.download_fraction("t1"), Some(1.0));

        // Defensive clamp: the contract is 0..=1000, but a bad snapshot must
        // not paint a bar wider than the row.
        state.downloads.progress.insert("t1".into(), u16::MAX);
        assert_eq!(state.download_fraction("t1"), Some(1.0));

        // Another track's fraction is not ours.
        state.downloads.progress.insert("t2".into(), 500);
        assert_eq!(state.download_fraction("t1"), Some(1.0));
        assert_eq!(state.download_fraction("t2"), Some(0.5));
    }

    #[test]
    fn play_collection_toggles_the_current_collection() {
        let mut state = State::default();
        state.queue.source = Some("liked".into());
        state.playback = PlaybackState::Playing;
        let action = play_collection_action(&state, vec![], Some("liked".into()), 0.5);
        assert!(matches!(action, Action::TogglePlay));
    }
}
