//! Audio-track filtering rule used across search, playback, and import.

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

/// Keep only audio tracks: drops "Official Video" uploads that slip through
/// typed as plain tracks.
pub(crate) fn is_audio_track(t: &Track) -> bool {
    !looks_like_video_title(&t.title)
}

#[cfg(test)]
mod tests {
    use super::looks_like_video_title;

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
}
