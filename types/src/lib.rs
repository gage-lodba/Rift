//! Shared data models between the Tauri backend and the Yew frontend.

use serde::{Deserialize, Serialize};

/// A single artist credit on a track.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArtistRef {
    /// Channel ID, for navigation. `None` for unlinkable credits.
    pub id: Option<String>,
    pub name: String,
}

/// A playable track, normalized from YouTube Music metadata.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Track {
    pub id: String,
    pub title: String,
    /// All artist names joined with ", " — for places that show plain text.
    pub artist: String,
    pub album: Option<String>,
    /// Duration in seconds, if known.
    pub duration: Option<u32>,
    /// URL of the cover thumbnail (may be empty).
    pub cover: String,
    /// Individual artist credits, each linkable to a profile.
    #[serde(default)]
    pub artists: Vec<ArtistRef>,
    /// Album browse ID, for navigation.
    #[serde(default)]
    pub album_id: Option<String>,
}

/// An artist as shown in search results.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArtistSummary {
    pub id: String,
    pub name: String,
    pub avatar: String,
    pub subscribers: Option<u64>,
}

/// An album as shown in search results and artist pages.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AlbumSummary {
    pub id: String,
    pub name: String,
    pub cover: String,
    pub artist: String,
    pub artist_id: Option<String>,
    pub year: Option<u16>,
    /// "Album", "EP", "Single", ...
    pub album_type: String,
}

/// A full artist profile page.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArtistPage {
    pub id: String,
    pub name: String,
    pub image: String,
    pub description: Option<String>,
    pub subscribers: Option<u64>,
    /// The artist's most popular tracks.
    pub tracks: Vec<Track>,
    pub albums: Vec<AlbumSummary>,
    /// Playlist ID backing the artist's full song catalog, if one exists. When
    /// set, the UI offers a "Show all songs" view that loads it on demand.
    #[serde(default)]
    pub tracks_playlist_id: Option<String>,
}

/// A full album page.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AlbumPage {
    pub id: String,
    pub name: String,
    pub cover: String,
    pub artist: String,
    pub artist_id: Option<String>,
    pub year: Option<u16>,
    pub album_type: String,
    pub tracks: Vec<Track>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Playlist {
    pub id: String,
    pub name: String,
    pub tracks: Vec<Track>,
}

/// Everything persisted to disk.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Library {
    pub liked: Vec<Track>,
    pub playlists: Vec<Playlist>,
    pub recently_played: Vec<Track>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepeatMode {
    #[default]
    Off,
    All,
    One,
}

/// Snapshot of the playback queue, sent to the frontend on every change.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct QueueSnapshot {
    pub tracks: Vec<Track>,
    /// Index of the current track in `tracks`, if any.
    pub current: Option<usize>,
    pub shuffle: bool,
    pub repeat: RepeatMode,
    /// Where the queue came from (e.g. "liked", "playlist:<id>"), used to
    /// mark the actively playing collection in the UI.
    #[serde(default)]
    pub source: Option<String>,
}

/// Playback status, sent to the frontend on every change.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum PlaybackState {
    #[default]
    Stopped,
    /// Stream is being resolved / downloaded.
    Loading,
    Playing,
    Paused,
}

/// Periodic progress tick.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct Progress {
    pub position: f64,
    pub duration: f64,
}

/// Full state snapshot fetched by the frontend on startup.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Bootstrap {
    pub library: Library,
    pub queue: QueueSnapshot,
    pub state: PlaybackState,
    pub volume: f32,
    pub track: Option<Track>,
    pub progress: Progress,
    pub downloads: DownloadState,
    /// Whether Discord Rich Presence is currently enabled.
    #[serde(default)]
    pub discord_rpc: bool,
    /// Crossfade overlap between tracks, in seconds (0 = disabled).
    #[serde(default)]
    pub crossfade: f32,
    /// User-configured custom path to the yt-dlp binary (empty = auto-detect).
    #[serde(default)]
    pub yt_dlp_path: Option<String>,
    /// Whether to check for updates on launch and notify the user.
    #[serde(default)]
    pub update_notifications: bool,
    /// Preview mode: the UI renders placeholder data instead of the library
    /// (dev builds launched with RIFT_PREVIEW=1; always false in releases).
    #[serde(default)]
    pub preview: bool,
}

/// Offline-download status, sent to the frontend whenever it changes.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DownloadState {
    /// Track IDs that are fully downloaded and available offline.
    pub downloaded: Vec<String>,
    /// Track IDs currently being downloaded.
    pub downloading: Vec<String>,
    /// Track IDs whose download was given up on after repeated failures. Rows
    /// surface these with a retry affordance instead of retrying forever.
    #[serde(default)]
    pub failed: Vec<String>,
}

/// Result of probing the system for the yt-dlp binary (the load-bearing
/// streaming fallback). Surfaced in Settings so users can confirm it's
/// installed and see which copy Rift will use.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct YtDlpStatus {
    /// Whether a working yt-dlp was found and ran successfully.
    pub found: bool,
    /// Absolute path Rift resolved, if any.
    pub path: Option<String>,
    /// Version string reported by `yt-dlp --version`, if it ran.
    pub version: Option<String>,
}

/// Result of checking GitHub for a newer Rift release.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct UpdateStatus {
    /// The running app version (e.g. "0.1.0").
    pub current: String,
    /// Latest published release version, with any leading `v` stripped.
    /// `None` if the check failed or no release exists.
    pub latest: Option<String>,
    /// Whether `latest` is newer than `current`.
    pub update_available: bool,
    /// URL of the latest release page, for the "Download" action.
    pub url: Option<String>,
}

/// Event channel names shared by both sides.
pub mod events {
    pub const TRACK: &str = "rift://track";
    pub const STATE: &str = "rift://state";
    pub const QUEUE: &str = "rift://queue";
    pub const PROGRESS: &str = "rift://progress";
    pub const LIBRARY: &str = "rift://library";
    pub const DOWNLOADS: &str = "rift://downloads";
    pub const ERROR: &str = "rift://error";
    /// Informational (non-error) toast, e.g. "Exported …".
    pub const NOTICE: &str = "rift://notice";
    /// Ask the frontend to navigate to a playlist (payload: its id). Emitted
    /// after a backend-driven import so the user lands on the new playlist.
    pub const OPEN_PLAYLIST: &str = "rift://open_playlist";
}
