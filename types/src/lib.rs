//! Shared data models between the Tauri backend and the Yew frontend.

use std::collections::{HashMap, HashSet};

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

/// Trimmed, whitespace-collapsed, lowercased text — the form every comparison
/// in [`Track::same_song`] uses, so two spellings of one name agree.
fn normalized(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

impl Track {
    /// The title key [`Track::same_song`] compares on.
    ///
    /// Public because callers that have to compare *many* rows at once bucket
    /// them by this first (see [`SongIndex`]); a second, slightly different
    /// normalisation at a call site is exactly how two "same song" rules drift
    /// apart.
    pub fn title_key(&self) -> String {
        normalized(&self.title)
    }

    /// Whether this row and `other` are the same **song**, rather than the same
    /// video.
    ///
    /// YouTube Music hands one song out under more than one video id, and
    /// neither surface carries the other's. Daft Punk's "Around the World" is
    /// `Jb6gcoR266U` (the art track) in the artist page's Top songs, but
    /// `K0HSD_i2DvA` (the official music video) on the *Homework* album page —
    /// both linking album `MPREb_dMHztPsrDgN`. The album row carries a duration
    /// and **no artist** (album rows leave the byline to the page header); the
    /// artist row carries the artist and **no duration**. So a rule that
    /// compares video ids alone treats one song as two, and the heart lit on the
    /// artist page stays dark on the album the song came from.
    ///
    /// The rule, in the order the evidence runs out:
    ///
    /// 1. The same id is the same song.
    /// 2. The titles must match (and not be empty).
    /// 3. Two credits that are both known must agree — an absent one is not a
    ///    disagreement, it is the album page's normal shape.
    /// 4. Two durations that are both known decide it: equal is the same
    ///    recording, different is not. This is what catches the same recording
    ///    re-released on another album, which the two surfaces describe with
    ///    different album ids (*Veridis Quo* is `HhZaHf8RP6g` on *Discovery*
    ///    and `qe8Q7mjxjig` on the *Interstella* single, 346 s both).
    /// 5. With a duration missing on one side, the album has to match — and if
    ///    there is no album either, the credit has to be on both sides and
    ///    agree.
    ///
    /// Step 5 is deliberately conservative. "Same title, different release" is
    /// only *proof* of a different recording when the lengths differ, and every
    /// album has an "Intro"; matching two of them on a shared name alone would
    /// light a heart on a song the user never liked.
    pub fn same_song(&self, other: &Track) -> bool {
        if self.id == other.id {
            return true;
        }
        let title = self.title_key();
        if title.is_empty() || title != other.title_key() {
            return false;
        }
        if conflicts(&self.artist, &other.artist) {
            return false;
        }
        if let (Some(a), Some(b)) = (self.duration, other.duration) {
            return a == b;
        }
        match (self.album_id.as_deref(), other.album_id.as_deref()) {
            (Some(a), Some(b)) => a == b,
            _ => {
                let artist = normalized(&self.artist);
                !artist.is_empty() && artist == normalized(&other.artist)
            }
        }
    }

    /// Copy the fields this row is missing from `other`, a stored copy of the
    /// same song that came from another surface. Never overwrites a value that
    /// is already here, and reports whether anything changed.
    ///
    /// This is what makes collapsing two surfaces' rows into one a *merge*
    /// rather than a choice between two half-filled rows: the album page's copy
    /// has the duration and the artist page's has the credit.
    pub fn fill_missing_from(&mut self, other: &Track) -> bool {
        let mut changed = false;
        let take = |slot: &mut String, value: &str| {
            if slot.is_empty() && !value.is_empty() {
                *slot = value.to_owned();
                true
            } else {
                false
            }
        };
        changed |= take(&mut self.artist, &other.artist);
        changed |= take(&mut self.cover, &other.cover);
        if self.artists.is_empty() && !other.artists.is_empty() {
            self.artists = other.artists.clone();
            changed = true;
        }
        if self.album.is_none() && other.album.is_some() {
            self.album = other.album.clone();
            changed = true;
        }
        if self.album_id.is_none() && other.album_id.is_some() {
            self.album_id = other.album_id.clone();
            changed = true;
        }
        if self.duration.is_none() && other.duration.is_some() {
            self.duration = other.duration;
            changed = true;
        }
        changed
    }
}

/// Whether two credits are both known and disagree.
///
/// An absent credit is not evidence of a different song — it is the normal
/// state of an album page's rows, which omit the byline the header already
/// carries.
fn conflicts(a: &str, b: &str) -> bool {
    !a.trim().is_empty() && !b.trim().is_empty() && normalized(a) != normalized(b)
}

/// Membership by *song* rather than by video id: "is this row one of these?"
/// where "these" are stored tracks (the user's likes, a playlist) that may have
/// been stored from another surface.
///
/// [`SongIndex::contains`] short-circuits on the id — the common case, since a
/// list is usually rendered from the surface its rows were stored from — and
/// otherwise costs one bucket lookup rather than a scan of the whole list, so a
/// long list stays cheap per rendered row. Rows are bucketed by
/// [`Track::title_key`], which [`Track::same_song`] requires anyway.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SongIndex {
    ids: HashSet<String>,
    by_title: HashMap<String, Vec<Track>>,
}

impl SongIndex {
    pub fn new<'a>(tracks: impl IntoIterator<Item = &'a Track>) -> Self {
        let mut index = Self::default();
        for track in tracks {
            index.insert(track);
        }
        index
    }

    fn insert(&mut self, track: &Track) {
        self.ids.insert(track.id.clone());
        self.by_title
            .entry(track.title_key())
            .or_default()
            .push(track.clone());
    }

    /// Whether `track` is one of the songs in the index.
    pub fn contains(&self, track: &Track) -> bool {
        if self.by_title.is_empty() {
            // Nothing indexed: every row would pay for a title key it could
            // never find, and a fresh install renders whole lists this way.
            return false;
        }
        if self.ids.contains(&track.id) {
            return true;
        }
        self.by_title
            .get(&track.title_key())
            .is_some_and(|bucket| bucket.iter().any(|stored| stored.same_song(track)))
    }
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
/// Sets, not lists: the UI checks membership per rendered row, and long track
/// lists against a large offline collection would make `Vec::contains` scans
/// quadratic per render.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DownloadState {
    /// Track IDs that are fully downloaded and available offline.
    pub downloaded: HashSet<String>,
    /// Track IDs currently being downloaded.
    pub downloading: HashSet<String>,
    /// Track IDs whose download was given up on after repeated failures. Rows
    /// surface these with a retry affordance instead of retrying forever.
    #[serde(default)]
    pub failed: HashSet<String>,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn track(id: &str, title: &str) -> Track {
        Track {
            id: id.into(),
            title: title.into(),
            artist: String::new(),
            album: None,
            duration: None,
            cover: String::new(),
            artists: Vec::new(),
            album_id: None,
        }
    }

    /// Daft Punk's *Homework* → "Around the World": the album page's row.
    ///
    /// Recorded from the live album page (`MPREb_dMHztPsrDgN`): the id is the
    /// official music video, the row carries the duration and no artist credit.
    fn album_row() -> Track {
        Track {
            id: "K0HSD_i2DvA".into(),
            title: "Around the World".into(),
            album: Some("Homework".into()),
            duration: Some(430),
            cover: "https://example.invalid/album-art".into(),
            album_id: Some("MPREb_dMHztPsrDgN".into()),
            ..track("K0HSD_i2DvA", "Around the World")
        }
    }

    /// The same song as the artist page's Top songs row: the art track, with
    /// the artist credit and no duration.
    fn artist_row() -> Track {
        Track {
            id: "Jb6gcoR266U".into(),
            title: "Around the World".into(),
            artist: "Daft Punk".into(),
            album: Some("Homework".into()),
            artists: vec![ArtistRef {
                id: Some("UCRr1xG_2WIDs18a6cIiCxeA".into()),
                name: "Daft Punk".into(),
            }],
            album_id: Some("MPREb_dMHztPsrDgN".into()),
            ..track("Jb6gcoR266U", "Around the World")
        }
    }

    /// The bug report, in one assertion: one song, two video ids, one from each
    /// surface. Which is *the* row is not part of the rule, so both directions
    /// are pinned.
    #[test]
    fn the_album_and_artist_pages_describe_one_song() {
        assert!(
            album_row().same_song(&artist_row()),
            "the album row and the artist row are one song"
        );
        assert!(
            artist_row().same_song(&album_row()),
            "same_song must be symmetric or a lookup depends on which side is which"
        );
    }

    #[test]
    fn an_index_built_from_one_surface_answers_the_other() {
        // Liked from the artist page, viewed on the album page.
        let liked = SongIndex::new(&[artist_row()]);
        assert!(
            liked.contains(&album_row()),
            "the album page's row must find a like stored from the artist page"
        );
        // And the reverse: liked from the album page, viewed on the artist page.
        let liked = SongIndex::new(&[album_row()]);
        assert!(
            liked.contains(&artist_row()),
            "the artist page's row must find a like stored from the album page"
        );
        // A different song is still a miss, id path and key path alike.
        assert!(!liked.contains(&track("zzz", "Around the World")));
    }

    /// The id is decisive on its own: a re-resolved copy whose metadata has
    /// been replaced still matches, which is what keeps this from regressing
    /// the plain case.
    #[test]
    fn the_same_id_is_the_same_song_whatever_else_changed() {
        let a = track("same", "One Title");
        let mut b = track("same", "One Title (Remastered)");
        b.artist = "Someone Else".into();
        b.duration = Some(1);
        assert!(a.same_song(&b));
    }

    /// Two songs sharing a title but nothing else are two songs. Without this,
    /// every cover and every same-named track would light up the same heart.
    #[test]
    fn a_shared_title_alone_is_not_a_shared_song() {
        let mut a = track("a", "Yesterday");
        a.artist = "The Beatles".into();
        a.album_id = Some("MPREb_beatles".into());
        let mut b = track("b", "Yesterday");
        b.artist = "Someone Else".into();
        b.album_id = Some("MPREb_other".into());
        assert!(!a.same_song(&b), "different artist, different album");
    }

    /// The same title on the same album at two different lengths is two
    /// recordings (an edit and the album cut), not one song.
    #[test]
    fn a_different_recording_on_the_same_album_is_a_different_song() {
        let mut radio_edit = track("edit", "One More Time");
        radio_edit.album_id = Some("MPREb_x".into());
        radio_edit.duration = Some(200);
        let mut album_cut = track("cut", "One More Time");
        album_cut.album_id = Some("MPREb_x".into());
        album_cut.duration = Some(320);
        assert!(!radio_edit.same_song(&album_cut));

        // Unknown on one side is not a conflict: that is the artist page's
        // shape, and refusing to match there is the whole bug.
        album_cut.duration = None;
        assert!(radio_edit.same_song(&album_cut));
    }

    /// One recording re-released on a second album: the surfaces disagree about
    /// the album id, and the length is the evidence that they are the same song.
    ///
    /// Both rows are from the library this was reported against — "Veridis Quo"
    /// is `HhZaHf8RP6g` on *Discovery* and `qe8Q7mjxjig` on the *Interstella*
    /// single, 346 s on both, and only the album page's copy has no credit.
    #[test]
    fn one_recording_on_a_second_release_matches_by_its_length() {
        let mut discovery = track("HhZaHf8RP6g", "Veridis Quo");
        discovery.album = Some("Discovery".into());
        discovery.album_id = Some("MPREb_7ltM34kr0mH".into());
        discovery.duration = Some(346);
        let mut interstella = track("qe8Q7mjxjig", "Veridis Quo");
        interstella.album = Some("Something About Us (Love Theme from Interstella)".into());
        interstella.artist = "Daft Punk".into();
        interstella.album_id = Some("MPREb_DYSzDJvUSDu".into());
        interstella.duration = Some(346);

        assert!(
            discovery.same_song(&interstella),
            "same title, same length, no conflicting credit"
        );

        // Same two releases, one second apart: two recordings.
        interstella.duration = Some(347);
        assert!(!discovery.same_song(&interstella));
    }

    /// With no length on either side the album is the only structure left, and
    /// it has to match. "Same title, different release" is not proof of
    /// anything on its own — every album has an "Intro".
    #[test]
    fn the_same_title_on_another_release_is_not_guessed_at() {
        let mut single = track("single", "Get Lucky");
        single.artist = "Daft Punk".into();
        single.album_id = Some("MPREb_single".into());
        let mut album = track("album", "Get Lucky");
        album.artist = "Daft Punk".into();
        album.album_id = Some("MPREb_album".into());
        assert!(!single.same_song(&album));

        // The same album, though, is enough once there is no length to compare.
        album.album_id = single.album_id.clone();
        assert!(single.same_song(&album));
    }

    /// With no album on either side the artist credit is the only shared
    /// signal, and it has to be present on both rows to count.
    #[test]
    fn without_an_album_the_credit_decides() {
        let mut a = track("a", "Song");
        a.artist = "Daft Punk".into();
        let mut b = track("b", "Song");
        b.artist = "daft punk".into();
        assert!(a.same_song(&b), "the same credit, spelled differently");
        assert!(!a.same_song(&track("c", "Song")), "no credit on one side");
    }

    /// With nothing to compare — no id, no title, no album, no credit — the
    /// answer is "no". A library row with an empty title must not become a
    /// wildcard that matches every other empty one.
    #[test]
    fn rows_with_nothing_in_common_are_not_guessed_at() {
        assert!(!track("a", "").same_song(&track("b", "")));
    }

    #[test]
    fn merging_takes_the_missing_fields_from_the_other_surface() {
        let mut merged = album_row();
        assert!(merged.fill_missing_from(&artist_row()));
        assert_eq!(merged.artist, "Daft Punk");
        assert_eq!(merged.artists.len(), 1);
        // The fields it already had are untouched, including its own id.
        assert_eq!(merged.id, "K0HSD_i2DvA");
        assert_eq!(merged.duration, Some(430));
        assert_eq!(merged.cover, "https://example.invalid/album-art");

        // Nothing left to take: a second pass changes nothing.
        assert!(!merged.fill_missing_from(&artist_row()));

        // And an existing value is never overwritten.
        assert!(!merged.fill_missing_from(&Track {
            artist: "Not Daft Punk".into(),
            duration: Some(1),
            ..artist_row()
        }));
        assert_eq!(merged.artist, "Daft Punk");
        assert_eq!(merged.duration, Some(430));
    }
}
