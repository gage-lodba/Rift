//! Shared data models between the native Rift backend and the desktop app.
//!
//! These types are the wire/serialization contract between `rift`,
//! `rift-ytmusic` and `rift-desktop`, and they are what lands in the persisted
//! JSON state files. They are therefore part of the library crates' public API
//! and must stay documented (the `missing_docs` lint is enabled below).

// Pure data crate: no FFI, no `unsafe` is allowed. (Keep it that way — this is
// also reachable from targets where the Vulkan/windowing stack is not built.)
#![forbid(unsafe_code)]
// Every public item here is a contract other crates (and the on-disk JSON)
// depend on, so a new `pub` item has to come with documentation.
#![warn(missing_docs)]

// Equality: every model in this file is made of exact types (no floats), so the
// ones without an `f32`/`f64` field also implement `Eq`, which lets callers use
// them in sets/maps and assert equality without a float epsilon. `Progress` and
// `Bootstrap` carry floats and deliberately stop at `PartialEq`; adding a float
// field to one of the `Eq` types means it has to lose its `Eq` derive again.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

/// A single artist credit on a track.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtistRef {
    /// Channel ID, for navigation. `None` for unlinkable credits.
    pub id: Option<String>,
    /// Artist display name as the source page spells it.
    pub name: String,
}

/// A playable track, normalized from `YouTube` Music metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Track {
    /// `YouTube` video ID — the stable key used to fetch and play the audio.
    pub id: String,
    /// Track title as shown in the UI.
    pub title: String,
    /// All artist names joined with ", " — for places that show plain text.
    pub artist: String,
    /// Album name, when the source surface supplied one.
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

/// How far apart two known durations can be and still describe one recording.
///
/// The same audio is rendered with lengths that differ by a second or two
/// depending on the upload and the surface: album rows round, a re-upload pads
/// the tail, and the art track and the music video of one recording are not cut
/// to the same frame. Treating a one-second difference as proof of two
/// recordings would leave the heart dark on the row the user is looking at —
/// the reported bug in its most literal form.
const DURATION_TOLERANCE_SECS: u32 = 2;

/// The credited names a row actually knows, as a candidate set to intersect.
///
/// Every shape the two credit fields are stored in contributes a candidate:
/// the joined byline as a whole (so a name that itself contains a comma, like
/// "Earth, Wind & Fire", still meets its structured twin), each structured
/// credit (the field album pages resolve, and the one that can be populated
/// while the byline is empty — `enrich_track` used to store exactly that
/// shape), and the byline split back into names (so a byline that lists a
/// featured artist can meet a surface that lists only the primary one).
fn credit_names(track: &Track) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    let mut add = |name: String| {
        if !name.is_empty() && !names.contains(&name) {
            names.push(name);
        }
    };
    add(normalized(&track.artist));
    for artist in &track.artists {
        add(normalized(&artist.name));
    }
    for part in track.artist.split([',', '&']) {
        add(normalized(part));
    }
    names
}

/// Whether two rows can be about the same recording as far as their credits go.
///
/// An absent credit is not evidence of a different song — album-page rows carry
/// no byline (the header has it) — so one side knowing nothing passes. Two
/// known sides must name at least one artist in common, which is what tolerates
/// the surfaces' disagreement about featured artists: a playlist row links only
/// the primary artist ("Lady Gaga") while the album page links the featured one
/// too ("Lady Gaga, Colby O'Donis"), and an exact-string comparison would veto
/// one recording as two.
fn credits_compatible(a: &Track, b: &Track) -> bool {
    let names_a = credit_names(a);
    let names_b = credit_names(b);
    if names_a.is_empty() || names_b.is_empty() {
        return true;
    }
    names_a.iter().any(|name| names_b.contains(name))
}

impl Track {
    /// The title key [`Track::same_song`] compares on.
    ///
    /// Public because callers that have to compare *many* rows at once bucket
    /// them by this first (see [`SongIndex`]); a second, slightly different
    /// normalisation at a call site is exactly how two "same song" rules drift
    /// apart.
    #[must_use]
    pub fn title_key(&self) -> String {
        normalized(&self.title)
    }

    /// Whether this row and `other` are the same **song**, rather than the same
    /// video.
    ///
    /// `YouTube` Music hands one song out under more than one video id, and
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
    /// 1. The same non-empty id is the same song. An empty id is "no id", not
    ///    an id every empty-id row shares.
    /// 2. The titles must match (and not be empty).
    /// 3. Credits, when known on both sides, must be compatible — share a name
    ///    (see the private `credits_compatible` helper). An absent credit is not
    ///    a disagreement.
    /// 4. Both durations known: agreement within the private
    ///    `DURATION_TOLERANCE_SECS` tolerance means one recording, a greater
    ///    difference means two. This is what
    ///    catches the same recording re-released on another album, which the two
    ///    surfaces describe with different album ids (*Veridis Quo* is stored
    ///    under *Discovery* as `HhZaHf8RP6g` and under *Something About Us (Love
    ///    Theme from Interstella)* as `qe8Q7mjxjig`, 346 s both).
    /// 5. With a duration missing on one side, the album decides: the same
    ///    album is the same song entry; different albums are not guessed at
    ///    (every album has an "Intro"), and with no album on at least one side
    ///    the credit has to be known on both.
    ///
    /// It is deliberately conservative: a shared title alone is never enough,
    /// and the two rules that can still be wrong are priced in. Step 4 treats
    /// "same title, same length, no conflicting credit" as one recording, and a
    /// pair that shares only a title and a length with no credit on either side
    /// can still be two creditless album-page rows; the repo's review notes both.
    #[must_use]
    pub fn same_song(&self, other: &Track) -> bool {
        if !self.id.is_empty() && self.id == other.id {
            return true;
        }
        let title = self.title_key();
        if title.is_empty() || title != other.title_key() {
            return false;
        }
        if !credits_compatible(self, other) {
            return false;
        }
        let durations = match (self.duration, other.duration) {
            (Some(a), Some(b)) => Some(a.abs_diff(b) <= DURATION_TOLERANCE_SECS),
            _ => None,
        };
        match (self.album_id.as_deref(), other.album_id.as_deref()) {
            // One album entry under two uploads: same length, or one side simply
            // did not know the length.
            (Some(a), Some(b)) if a == b => durations.unwrap_or(true),
            // Two releases describe one recording with different album ids only
            // when their lengths agree.
            (Some(_), Some(_)) => durations == Some(true),
            // No album to compare: a matching length is evidence; without one,
            // both sides have to name the song's artist.
            _ => match durations {
                Some(agree) => agree,
                None => !credit_names(self).is_empty() && !credit_names(other).is_empty(),
            },
        }
    }

    /// Copy the fields this row is missing from `other`, a stored copy of the
    /// same song that came from another surface. Never overwrites a value that
    /// is already here, and reports whether anything changed.
    ///
    /// This is what makes collapsing two surfaces' rows into one a *merge*
    /// rather than a choice between two half-filled rows: the album page's copy
    /// has the duration and the artist page's has the credit.
    ///
    /// The id and title are never copied: they are the row's identity, and by
    /// the time two rows are merged `same_song` already treats them as equal.
    #[must_use]
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
        // A byline can be missing while the structured credits are known (the
        // shape `enrich_track` used to store); derive it rather than leaving the
        // row with no plain-text artist.
        if self.artist.is_empty() && !self.artists.is_empty() {
            self.artist = self
                .artists
                .iter()
                .map(|a| a.name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
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

/// Membership by *song* rather than by video id: "is this row one of these?"
/// where "these" are stored tracks (the user's likes, a playlist) that may have
/// been stored from another surface.
///
/// [`SongIndex::contains`] short-circuits on the id — the common case, since a
/// list is usually rendered from the surface its rows were stored from — and
/// otherwise costs one bucket lookup rather than a scan of the whole list, so a
/// long list stays cheap per rendered row. Rows are bucketed by
/// [`Track::title_key`], which [`Track::same_song`] requires anyway.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SongIndex {
    ids: HashSet<String>,
    by_title: HashMap<String, Vec<Track>>,
}

impl SongIndex {
    /// Build an index from any iterator of track references (typically a
    /// library's likes or a playlist's rows).
    #[must_use]
    pub fn new<'a>(tracks: impl IntoIterator<Item = &'a Track>) -> Self {
        let mut index = Self::default();
        for track in tracks {
            index.insert(track);
        }
        index
    }

    fn insert(&mut self, track: &Track) {
        if !track.id.is_empty() {
            self.ids.insert(track.id.clone());
        }
        self.by_title
            .entry(track.title_key())
            .or_default()
            .push(track.clone());
    }

    /// Whether `track` is one of the songs in the index.
    #[must_use]
    pub fn contains(&self, track: &Track) -> bool {
        if self.by_title.is_empty() {
            // Nothing indexed: every row would pay for a title key it could
            // never find, and a fresh install renders whole lists this way.
            return false;
        }
        if !track.id.is_empty() && self.ids.contains(&track.id) {
            return true;
        }
        self.by_title
            .get(&track.title_key())
            .is_some_and(|bucket| bucket.iter().any(|stored| stored.same_song(track)))
    }
}

/// An artist as shown in search results.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtistSummary {
    /// Browse/channel ID used to open the artist page.
    pub id: String,
    /// Artist name as shown in results.
    pub name: String,
    /// URL of the artist avatar (may be empty).
    pub avatar: String,
    /// Subscriber count, when the source reports one.
    pub subscribers: Option<u64>,
}

/// An album as shown in search results and artist pages.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlbumSummary {
    /// Browse ID used to open the album page.
    pub id: String,
    /// Album title.
    pub name: String,
    /// URL of the cover art (may be empty).
    pub cover: String,
    /// Display byline for the album's artist(s).
    pub artist: String,
    /// Browse ID of the primary artist, when linkable.
    pub artist_id: Option<String>,
    /// Release year, when the source reports one.
    pub year: Option<u16>,
    /// "Album", "EP", "Single", ...
    pub album_type: String,
}

/// A full artist profile page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtistPage {
    /// Browse/channel ID of the artist.
    pub id: String,
    /// Artist name.
    pub name: String,
    /// URL of the header image (may be empty).
    pub image: String,
    /// Short biography/description paragraph, when the source provides one.
    pub description: Option<String>,
    /// Subscriber count, when the source reports one.
    pub subscribers: Option<u64>,
    /// The artist's most popular tracks.
    pub tracks: Vec<Track>,
    /// Albums and singles shown on the page (most recent first).
    pub albums: Vec<AlbumSummary>,
    /// Playlist ID backing the artist's full song catalog, if one exists. When
    /// set, the UI offers a "Show all songs" view that loads it on demand.
    #[serde(default)]
    pub tracks_playlist_id: Option<String>,
}

/// A full album page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlbumPage {
    /// Browse ID of the album.
    pub id: String,
    /// Album title.
    pub name: String,
    /// URL of the cover art (may be empty).
    pub cover: String,
    /// Display byline for the album's artist(s).
    pub artist: String,
    /// Browse ID of the primary artist, when linkable.
    pub artist_id: Option<String>,
    /// Release year, when the source reports one.
    pub year: Option<u16>,
    /// "Album", "EP", "Single", ...
    pub album_type: String,
    /// Tracks in album order.
    pub tracks: Vec<Track>,
}

/// A named, ordered collection of tracks the user owns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Playlist {
    /// Stable local identifier (generated by the library, not a `YouTube` id).
    pub id: String,
    /// User-visible playlist name.
    pub name: String,
    /// Tracks in playlist order.
    pub tracks: Vec<Track>,
}

/// Everything persisted to disk.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Library {
    /// Tracks the user has liked ("Liked Songs").
    pub liked: Vec<Track>,
    /// User-created and imported playlists, in sidebar order.
    pub playlists: Vec<Playlist>,
    /// Most-recently played tracks, newest first (bounded).
    pub recently_played: Vec<Track>,
}

/// What playback does at the end of the queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepeatMode {
    /// Stop at the end of the queue.
    #[default]
    Off,
    /// Wrap around to the start of the queue.
    All,
    /// Replay the current track forever.
    One,
}

/// Snapshot of the playback queue, sent to the frontend on every change.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueueSnapshot {
    /// Queue contents in playback order.
    pub tracks: Vec<Track>,
    /// Index of the current track in `tracks`, if any.
    pub current: Option<usize>,
    /// Whether shuffle mode is enabled.
    pub shuffle: bool,
    /// Repeat mode currently in effect.
    pub repeat: RepeatMode,
    /// Where the queue came from (e.g. `"liked"`, `"playlist:<id>"`), used to
    /// mark the actively playing collection in the UI.
    #[serde(default)]
    pub source: Option<String>,
}

/// Playback status, sent to the frontend on every change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum PlaybackState {
    /// Nothing is playing and no track is loaded.
    #[default]
    Stopped,
    /// Stream is being resolved / downloaded.
    Loading,
    /// Audio is playing.
    Playing,
    /// A track is loaded but paused.
    Paused,
}

/// Periodic progress tick.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct Progress {
    /// Current playback position, in seconds.
    pub position: f64,
    /// Track length in seconds; `0.0` while the length is still unknown.
    pub duration: f64,
}

/// Full state snapshot fetched by the frontend on startup.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Bootstrap {
    /// The persisted library (likes, playlists, recents).
    pub library: Library,
    /// The restored playback queue.
    pub queue: QueueSnapshot,
    /// Playback state at launch (always stopped until something is played).
    pub state: PlaybackState,
    /// Current volume, `0.0..=1.0`.
    pub volume: f32,
    /// The current track, if the restored queue names one.
    pub track: Option<Track>,
    /// Last known playback position/duration.
    pub progress: Progress,
    /// Offline-download status at launch.
    pub downloads: DownloadState,
    /// Whether Discord Rich Presence is currently enabled.
    #[serde(default)]
    pub discord_rpc: bool,
    /// Crossfade overlap between tracks, in seconds (0 = disabled).
    #[serde(default)]
    pub crossfade: f32,
    /// Whether to check for updates on launch and notify the user.
    #[serde(default)]
    pub update_notifications: bool,
    /// Preview mode: the UI renders placeholder data instead of the library
    /// (dev builds launched with `RIFT_PREVIEW=1`; always false in releases).
    #[serde(default)]
    pub preview: bool,
}

/// Offline-download status, sent to the frontend whenever it changes.
/// Sets, not lists: the UI checks membership per rendered row, and long track
/// lists against a large offline collection would make `Vec::contains` scans
/// quadratic per render.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DownloadState {
    /// Track IDs that are fully downloaded and available offline.
    pub downloaded: HashSet<String>,
    /// Track IDs currently being downloaded.
    pub downloading: HashSet<String>,
    /// Track IDs whose download was given up on after repeated failures. Rows
    /// surface these with a retry affordance instead of retrying forever.
    #[serde(default)]
    pub failed: HashSet<String>,
    /// Completed fraction of each in-flight download, in permille (`0..=1000`).
    /// Only tracks currently in `downloading` may appear here. Absent means the
    /// fraction is unknown — the track is queued behind the one actively
    /// transferring, or its transfer has not reported yet — and the UI keeps its
    /// indeterminate spinner instead of drawing a bar.
    ///
    /// Integer permille rather than an `f32` fraction because this type keeps
    /// its `Eq` derive (it has no float fields), which the UI's state and event
    /// comparisons rely on, while still being fine-grained enough for a smooth
    /// 3 px bar.
    #[serde(default)]
    pub progress: HashMap<String, u16>,
}

/// Result of checking GitHub for a newer Rift release.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
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

    /// One structured credit, with no channel id — only the name matters to the
    /// comparison under test.
    fn credit(name: &str) -> ArtistRef {
        ArtistRef {
            id: None,
            name: name.into(),
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

    /// A structured credit with no plain-text byline still vetoes a match.
    ///
    /// `enrich_track` used to store the credits it resolved on `artists` and
    /// leave `artist` empty, so the app's own library has rows like this (eight
    /// of them, measured). A credit check that reads only `artist` sees "no
    /// credit" on that row, and a cover with the same title and the same length
    /// would win on the duration rule — the false positive that motivated this
    /// test.
    #[test]
    fn structured_credits_veto_a_match_the_byline_cannot() {
        let mut liked = track("PwILkY9gRrc", "Da Funk");
        liked.artists = vec![credit("Daft Punk")];
        liked.album_id = Some("MPREb_dMHztPsrDgN".into());
        liked.duration = Some(329);

        let mut karaoke = track("karaoke", "Da Funk");
        karaoke.artist = "Karaoke Band".into();
        karaoke.album_id = Some("MPREb_karaoke".into());
        karaoke.duration = Some(329);

        assert!(
            !liked.same_song(&karaoke),
            "the structured credit must veto one recording as two"
        );
        assert!(!karaoke.same_song(&liked), "and symmetrically");
    }

    /// The surfaces disagree about *how many* artists a row links, not about
    /// which recording it is: playlist rows link the primary artist only, album
    /// pages link the featured ones too. An exact-string credit comparison —
    /// what this replaced — called one recording two and kept the heart dark.
    #[test]
    fn featured_credits_that_only_one_surface_links_do_not_split_a_song() {
        let mut from_playlist = track("pl", "Get Lucky (feat. Pharrell Williams)");
        from_playlist.artist = "Daft Punk".into();
        from_playlist.album_id = Some("MPREb_ram".into());
        from_playlist.duration = Some(248);

        let mut from_album_page = track("al", "Get Lucky (feat. Pharrell Williams)");
        from_album_page.artist = "Daft Punk, Pharrell Williams".into();
        from_album_page.artists = vec![credit("Daft Punk"), credit("Pharrell Williams")];
        from_album_page.album_id = Some("MPREb_ram".into());
        from_album_page.duration = Some(248);

        assert!(from_playlist.same_song(&from_album_page));
        assert!(from_album_page.same_song(&from_playlist));
    }

    /// An artist name that itself contains a comma must still meet its
    /// structured twin, which is why the byline as a whole is a candidate name
    /// before it is split apart.
    #[test]
    fn a_comma_in_an_artist_name_does_not_split_it_against_itself() {
        let mut byline = track("a", "September");
        byline.artist = "Earth, Wind & Fire".into();
        byline.duration = Some(215);
        let mut structured = track("b", "September");
        structured.artists = vec![credit("Earth, Wind & Fire")];
        structured.duration = Some(215);
        assert!(byline.same_song(&structured));
    }

    /// The credit check is what tells two recordings apart when everything else
    /// agrees — same title, same album, same length. Deleting it as dead weight
    /// has to fail this test.
    #[test]
    fn two_artists_on_the_same_album_entry_are_two_songs() {
        let mut original = track("a", "Yesterday");
        original.artist = "The Beatles".into();
        original.album_id = Some("MPREb_beatles".into());
        original.duration = Some(125);

        let mut cover = track("b", "Yesterday");
        cover.artist = "Someone Else".into();
        cover.album_id = Some("MPREb_beatles".into());
        cover.duration = Some(125);

        assert!(!original.same_song(&cover));
    }

    /// A second or two of difference is the same recording; more than the
    /// tolerance is two recordings.
    #[test]
    fn a_second_or_two_of_length_is_still_the_same_recording() {
        let mut a = track("a", "Song");
        a.album_id = Some("MPREb_x".into());
        a.duration = Some(430);
        let mut b = track("b", "Song");
        b.album_id = Some("MPREb_x".into());
        b.duration = Some(431);
        assert!(
            a.same_song(&b),
            "one second apart is one recording, not two"
        );

        b.duration = Some(440);
        assert!(!a.same_song(&b), "ten seconds apart is two recordings");
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
    /// is stored under *Discovery* as `HhZaHf8RP6g` and under *Something About
    /// Us (Love Theme from Interstella)* as `qe8Q7mjxjig`, 346 s on both, and
    /// only the album page's copy has no credit.
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

        // Within the tolerance one upload's rounding is not a second recording.
        interstella.duration = Some(347);
        assert!(discovery.same_song(&interstella));

        // Beyond it, it is.
        interstella.duration = Some(350);
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

    /// An empty id is "no id", not an id every empty-id row shares: a corrupt
    /// or hand-edited row must not become a wildcard that folds into — or
    /// unlikes — every other one.
    #[test]
    fn an_empty_id_is_not_an_identity() {
        let mut one = track("", "One");
        one.artist = "Artist A".into();
        let mut two = track("", "Two");
        two.artist = "Artist B".into();
        assert!(
            !one.same_song(&two),
            "two unrelated empty-id rows are not one song"
        );

        // With the same metadata the ordinary rule still applies, exactly as it
        // would for two different ids.
        let mut also_one = track("", "One");
        also_one.artist = "Artist A".into();
        assert!(one.same_song(&also_one));
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
