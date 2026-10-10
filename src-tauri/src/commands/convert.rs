//! Audio-track filtering rule used across search, playback, and import.
//!
//! # One rule, two data paths
//!
//! Where a row carries a type, the type decides. Innertube rows carry
//! `watchEndpointMusicConfig.musicVideoType`, and `rift_ytmusic`'s parsers drop
//! `MUSIC_VIDEO_TYPE_UGC` rows on the strength of it
//! (`parse::track_from_mrlir` via `parse::is_non_audio_row`). That rule is
//! title-independent, and it is pinned by
//! `parse::tests::a_ugc_typed_row_with_a_clean_title_is_dropped`: a UGC upload
//! with a clean title never reaches this module from search, album, artist or
//! playlist-import.
//!
//! What this module adds on those same paths is the second net: an upload typed
//! as a plain track (`MUSIC_VIDEO_TYPE_ATV`/`OMV`) whose title still carries a
//! video marker. The type cannot catch that one, and the marker is the only
//! evidence left.
//!
//! Where no type exists at all, the two paths must differ, and they do —
//! deliberately, not silently:
//!
//! - Rift's own playlist JSON is a lossless round trip by design (see
//!   `commands::library::import_playlist`, which must not drop rows the file
//!   contained). `Track` has no field for a type, so re-importing a playlist
//!   whose UGC rows were dropped upstream cannot bring the type back with it,
//!   and this filter — the only one that could look — would be guessing from the
//!   title. It is not applied there for that reason.
//! - Where the fallback *is* applied to signal-less data, it keeps a clean-title
//!   track: without `musicVideoType` there is nothing to distinguish a
//!   clean-titled UGC cover from a catalog track, and dropping it on a hunch
//!   would delete real songs. Rift does not own YouTube uploaders' naming.
//!
//! Both halves are asserted, so the split is a decision rather than an accident:
//! the parser test above covers the typed branch, and
//! `is_audio_track_keeps_a_clean_title_the_type_would_have_caught` below records
//! exactly what the fallback cannot do.

use rift_types::Track;

/// Upper bound on tracks pulled when importing a playlist. Bounds worst-case
/// continuation requests for very large playlists while covering essentially
/// all real ones.
pub(crate) const MAX_FETCH_TRACKS: usize = 1000;

/// Heuristic: does this title look like a music-video upload rather than an
/// audio track? YouTube Music's "Songs" filter occasionally surfaces the
/// "Official Video" version of a track (typically when there's no separate
/// audio-only upload), and these come back typed as a plain track.
///
/// Matches specific marker *phrases* (not the bare word "video") so legitimate
/// songs whose names happen to contain "video" — e.g. "Video Games" — aren't
/// dropped. Audio-leaning variants ("official audio", "lyric video",
/// "visualizer") are deliberately not matched.
fn looks_like_video_title(title: &str) -> bool {
    let t = title.to_lowercase();
    const MARKERS: [&str; 5] = [
        "official video",
        "music video",
        "official hd video",
        "official 4k video",
        "video clip",
    ];
    MARKERS.iter().any(|m| t.contains(m))
}

/// Keep only audio tracks: the **title fallback** for paths that carry no
/// `musicVideoType` — Rift's JSON import/export and the stored library.
///
/// This is not the whole rule; see the module docs. On the innertube paths the
/// typed UGC signal is already enforced inside `rift_ytmusic::parse`, before a
/// `Track` exists, which is why this function never has to reason about a UGC
/// row there. It is still applied to those tracks as a second, weaker net for
/// "Official Video" uploads that arrive typed as plain tracks — which is what it
/// was written for.
pub(crate) fn is_audio_track(t: &Track) -> bool {
    !looks_like_video_title(&t.title)
}

#[cfg(test)]
mod tests {
    use super::{is_audio_track, looks_like_video_title};
    use rift_types::Track;

    fn track(title: &str) -> Track {
        Track {
            id: "test-id".into(),
            title: title.into(),
            artist: "Test Artist".into(),
            album: None,
            duration: None,
            cover: String::new(),
            artists: Vec::new(),
            album_id: None,
        }
    }

    #[test]
    fn flags_official_video_uploads() {
        for t in [
            "Song Title (Official Video)",
            "Song Title (Official Music Video)",
            "Song Title [Music Video]",
            "Song Title (Official HD Video)",
            "Artist - Song (Video Clip)",
        ] {
            assert!(looks_like_video_title(t), "should flag: {t}");
        }
    }

    #[test]
    fn keeps_audio_tracks_and_video_named_songs() {
        for t in [
            "Video Games",                 // a real song name, not a video upload
            "Song Title (Official Audio)", // audio variant
            "Song Title (Lyric Video)",    // lyric videos are audio-leaning
            "Song Title (Visualizer)",
            "Plain Song Title",
        ] {
            assert!(!looks_like_video_title(t), "should keep: {t}");
        }
    }

    /// The signal-less branch of L10, recorded as a test rather than a comment.
    ///
    /// These titles are what a UGC upload looks like *after* the type has been
    /// lost: JSON export/import carries no `musicVideoType`, so the fallback
    /// cannot tell this from a catalog track and deliberately keeps it.
    /// `rift_ytmusic`'s `a_ugc_typed_row_with_a_clean_title_is_dropped` asserts
    /// the other branch, where the type is still present and does the dropping.
    /// If someone "fixes" this test by making the fallback guess harder from the
    /// title, they are changing what a re-imported playlist sounds like.
    #[test]
    fn is_audio_track_keeps_a_clean_title_the_type_would_have_caught() {
        assert!(is_audio_track(&track("Blinding Lights (Cover)")));
        assert!(is_audio_track(&track("Blinding Lights")));
        assert!(is_audio_track(&track("The Weeknd Playlist 2025")));
    }

    /// The fallback's own job, through the entry point the callers use — not
    /// just the private helper the other tests exercise.
    #[test]
    fn is_audio_track_drops_marker_titles() {
        assert!(!is_audio_track(&track("Song Title (Official Video)")));
        assert!(!is_audio_track(&track("SONG TITLE [MUSIC VIDEO]")));
        assert!(is_audio_track(&track("Video Games")));
    }
}
