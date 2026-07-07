//! Shared conversions from rustypipe models to Rift's own types, plus the
//! audio-track filtering rule used across search, playback, and import.

use rift_types::{AlbumSummary, Track};
use rustypipe::model::{AlbumItem, AlbumType, Thumbnail, TrackItem};

/// Upper bound on tracks pulled when importing a playlist or loading an
/// artist's full catalog. Bounds worst-case continuation requests for very
/// large playlists while covering essentially all real ones.
pub(crate) const MAX_FETCH_TRACKS: usize = 1000;

pub(crate) fn thumb(thumbs: &[Thumbnail]) -> String {
    // Prefer the smallest thumbnail at least TARGET wide — crisp for headers
    // without pulling the full-resolution original for tiny list rows/cards.
    // Fall back to the largest available when nothing meets the target.
    const TARGET: u32 = 512;
    thumbs
        .iter()
        .filter(|t| t.width >= TARGET)
        .min_by_key(|t| t.width)
        .or_else(|| thumbs.iter().max_by_key(|t| t.width))
        .map(|t| t.url.clone())
        .unwrap_or_default()
}

pub(crate) fn join_artists(artists: &[rustypipe::model::ArtistId]) -> String {
    let joined = artists
        .iter()
        .map(|a| a.name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    if joined.is_empty() {
        "Unknown Artist".into()
    } else {
        joined
    }
}

pub(crate) fn album_type_label(t: AlbumType) -> String {
    match t {
        AlbumType::Album => "Album".into(),
        AlbumType::Ep => "EP".into(),
        AlbumType::Single => "Single".into(),
        other => format!("{other:?}"),
    }
}

pub(crate) fn convert_artists(artists: &[rustypipe::model::ArtistId]) -> Vec<rift_types::ArtistRef> {
    artists
        .iter()
        .map(|a| rift_types::ArtistRef {
            id: a.id.clone(),
            name: a.name.clone(),
        })
        .collect()
}

/// Heuristic: does this title look like a music-video upload rather than an
/// audio track? YouTube Music's "Songs" filter occasionally surfaces the
/// "Official Video" version of a track (typically when there's no separate
/// audio-only upload), and these come back typed as a plain track, so the
/// `TrackType` filter alone doesn't catch them.
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

/// Keep only official YouTube Music audio tracks: drops music videos and
/// podcast episodes by type, and "Official Video" uploads that slip through
/// typed as tracks. The single place this rule lives.
pub(crate) fn is_audio_track(t: &TrackItem) -> bool {
    t.track_type.is_track() && !looks_like_video_title(&t.name)
}

pub(crate) fn convert(item: TrackItem) -> Track {
    Track {
        title: item.name,
        artist: join_artists(&item.artists),
        artists: convert_artists(&item.artists),
        album: item.album.as_ref().map(|a| a.name.clone()),
        album_id: item.album.map(|a| a.id),
        duration: item.duration,
        cover: thumb(&item.cover),
        id: item.id,
    }
}

pub(crate) fn convert_album_item(item: AlbumItem) -> AlbumSummary {
    AlbumSummary {
        artist: join_artists(&item.artists),
        artist_id: item
            .artist_id
            .or_else(|| item.artists.iter().find_map(|a| a.id.clone())),
        name: item.name,
        cover: thumb(&item.cover),
        year: item.year,
        album_type: album_type_label(item.album_type),
        id: item.id,
    }
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
