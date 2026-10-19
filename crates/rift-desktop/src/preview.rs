//! Preview mode data: neutral placeholders for repository screenshots.
//!
//! Ported one-for-one from the old frontend's `preview_track` /
//! `preview_library` / `preview_queue` (ui/src/app.rs). The reference screenshot
//! `Previews/image.png` was produced from this data at 1160x740:
//! library cards "Liked Songs / Playlist 1 / Playlist 2 / Playlist 3", a
//! recently-played list of "Song Title N", a six-track "UP NEXT (6)" queue and
//! "Song Title 1" playing at 1:23 / 3:34.
//!
//! Preview mode is enabled by the `--preview` flag or `RIFT_PREVIEW=1`, exactly
//! like the old dev-only mode. It never touches the user's real library.

use rift_types::{
    AlbumPage, AlbumSummary, ArtistPage, ArtistSummary, Library, PlaybackState, Playlist, Progress,
    QueueSnapshot, RepeatMode, Track,
};

/// A placeholder track for preview mode. `cover` is empty so the fallback tile
/// (dark background + music note) renders, exactly as in the reference shot.
pub fn preview_track(n: usize) -> Track {
    Track {
        id: format!("preview-{n}"),
        title: format!("Song Title {n}"),
        artist: "Artist Name".into(),
        album: Some("Album Name".into()),
        duration: Some(150 + (n as u32 * 37) % 120),
        cover: String::new(),
        artists: Vec::new(),
        album_id: None,
    }
}

/// Neutral library rendered in preview mode, so repo screenshots show generic
/// placeholders instead of a personal library.
pub fn preview_library() -> Library {
    let playlist = |n: usize, len: usize, start: usize| Playlist {
        id: format!("preview-pl-{n}"),
        name: format!("Playlist {n}"),
        tracks: (start..start + len).map(preview_track).collect(),
    };
    Library {
        liked: (1..=8).map(preview_track).collect(),
        playlists: vec![playlist(1, 12, 1), playlist(2, 9, 13), playlist(3, 16, 22)],
        recently_played: (1..=12).map(preview_track).collect(),
    }
}

/// Placeholder queue: a handful of upcoming tracks with the first playing.
pub fn preview_queue() -> QueueSnapshot {
    QueueSnapshot {
        tracks: (1..=6).map(preview_track).collect(),
        current: Some(0),
        shuffle: false,
        repeat: RepeatMode::Off,
        source: None,
    }
}

/// The preview bootstrap-equivalent state.
pub fn preview_state() -> crate::state::State {
    let mut state = crate::state::State {
        library: preview_library(),
        queue: preview_queue(),
        playback: PlaybackState::Playing,
        track: Some(preview_track(1)),
        progress: Progress {
            position: 83.0,
            duration: 214.0,
        },
        volume: 1.0,
        preview: true,
        // The reference screenshot (`Previews/image.png`) was captured after the
        // sidebar had been drag-resized to 335 px. Interactive launches keep the
        // 216 px default from the theme; the deterministic screenshot mode uses
        // the width in the reference so comparisons are like-for-like.
        sidebar_width: 335.0,
        ..crate::state::State::default()
    };
    state.search.query = String::new();
    state
}

/// A synthetic artist page for previewing the artist view without network.
pub fn preview_artist_page() -> ArtistPage {
    let albums: Vec<AlbumSummary> = (1..=4)
        .map(|n| AlbumSummary {
            id: format!("preview-album-{n}"),
            name: format!("Album {n}"),
            cover: String::new(),
            artist: "Artist Name".into(),
            artist_id: Some("preview-artist-1".into()),
            year: Some(2020 + n as u16),
            album_type: if n == 4 {
                "Single".into()
            } else {
                "Album".into()
            },
        })
        .collect();
    ArtistPage {
        id: "preview-artist-1".into(),
        name: "Artist Name".into(),
        image: String::new(),
        description: None,
        subscribers: Some(1_200_000),
        tracks: (1..=5).map(preview_track).collect(),
        albums,
        tracks_playlist_id: Some("preview-pl-songs".into()),
    }
}

/// A synthetic album page for previewing the album view without network.
pub fn preview_album_page() -> AlbumPage {
    AlbumPage {
        id: "preview-album-1".into(),
        name: "Album Name".into(),
        cover: String::new(),
        artist: "Artist Name".into(),
        artist_id: Some("preview-artist-1".into()),
        year: Some(2024),
        album_type: "Album".into(),
        tracks: (1..=10).map(preview_track).collect(),
    }
}

/// Synthetic search results for previewing the search view without network.
pub fn preview_search_results() -> (Vec<Track>, Vec<ArtistSummary>, Vec<AlbumSummary>) {
    let tracks = (1..=8).map(preview_track).collect();
    let artists = (1..=4)
        .map(|n| ArtistSummary {
            id: format!("preview-artist-{n}"),
            name: format!("Artist Name {n}"),
            avatar: String::new(),
            subscribers: Some(1_000_000 * n as u64),
        })
        .collect();
    let albums = (1..=4)
        .map(|n| AlbumSummary {
            id: format!("preview-album-{n}"),
            name: format!("Album Name {n}"),
            cover: String::new(),
            artist: format!("Artist Name {n}"),
            artist_id: Some(format!("preview-artist-{n}")),
            year: Some(2020 + n as u16),
            album_type: "Album".into(),
        })
        .collect();
    (tracks, artists, albums)
}
