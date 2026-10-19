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
//! Wherever no type reaches a `Track`, nothing filters it, and that is
//! deliberate rather than an omission:
//!
//! - `commands::library::import_playlist` parses Rift's own playlist JSON and is
//!   a lossless round trip by design (see its own doc comment). A filter there
//!   would drop rows the file contained, and nothing could put them back:
//!   `Track` has no field to carry a type through the round trip, so the export
//!   cannot preserve the evidence such a filter would need. That path does not
//!   call this module, and it must not start.
//! - The stored library is Rift's own data as well, already filtered when it was
//!   collected; nothing re-filters it.
//!
//! Where the fallback does run, it keeps a clean-title track: the title is the
//! only evidence left, and without `musicVideoType` there is nothing to
//! distinguish a clean-titled UGC cover from a catalog track. Dropping one on a
//! hunch would delete real songs — Rift does not own `YouTube` uploaders' naming.
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
/// audio track? `YouTube` Music's "Songs" filter occasionally surfaces the
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

/// Keep only audio tracks: the title-marker net for the **innertube** call
/// sites, where a `Track` no longer carries the type its own row had.
///
/// Called by `commands::search` (search, artist, artist songs, album) and
/// `commands::library::import_yt_playlist` — every path that builds tracks from
/// an innertube response. The typed UGC signal is enforced upstream inside
/// `rift_ytmusic::parse` (see the module docs), so what this adds is the one
/// case the type cannot catch: an upload that arrives typed as a plain track
/// while a video marker is still in its title.
///
/// Deliberately *not* a blanket filter. In particular
/// `commands::library::import_playlist` — Rift's own playlist JSON, a lossless
/// round trip — must not gain a call to this function: it would drop rows the
/// file contained, and the type that could justify the drop does not survive the
/// round trip. Changing that is a decision about the round trip rather than a
/// bug fix, and it needs its own test, because the tests below call this
/// function directly and none of them can see where it is wired in.
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
