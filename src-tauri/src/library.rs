//! JSON-persisted user library: liked songs, playlists, recently played.

use std::collections::HashMap;
use std::path::PathBuf;

use rift_types::{Library, Playlist, Track};
use tracing::{debug, info, warn};

const RECENT_CAP: usize = 30;

/// Where the pre-repair library is snapshotted before the one-time collapse
/// rewrites it. A sibling of `library.json`, so the repair is recoverable by
/// hand even if the merge rule ever guesses wrong about a song.
const PRE_COLLAPSE_BACKUP: &str = "library.json.pre-collapse";

/// Fold rows that are the same song ([`Track::same_song`]) into their first
/// occurrence, which keeps its position and its id and absorbs whatever a later
/// row knew that it did not. Returns the folded rows and how many were folded
/// away.
///
/// Two lookups rather than one, because `same_song` has two ways in: the same id
/// is the same song whatever the titles say, and otherwise equal titles are
/// required — so an id hit is checked first and the title bucket after it.
/// Bucketing by title is what keeps this linear on average for the long lists (a
/// 1000-track import) that a pairwise scan would make quadratic; the pathological
/// case is one title bucket holding every row (at worst quadratic, only when the
/// rows share a title).
fn fold_song_duplicates(tracks: Vec<Track>) -> (Vec<Track>, usize) {
    let before = tracks.len();
    if before < 2 {
        return (tracks, 0);
    }
    let mut by_id: HashMap<String, usize> = HashMap::new();
    let mut by_title: HashMap<String, Vec<usize>> = HashMap::new();
    let mut out: Vec<Track> = Vec::with_capacity(before);
    for t in tracks {
        let key = t.title_key();
        // An empty id is "no id", not an id every empty-id row shares, so it
        // only ever takes part in the title comparison.
        let by_id_hit = if t.id.is_empty() {
            None
        } else {
            by_id.get(&t.id).copied()
        };
        let existing = by_id_hit.or_else(|| {
            by_title
                .get(&key)
                .and_then(|positions| positions.iter().copied().find(|&i| out[i].same_song(&t)))
        });
        match existing {
            Some(i) => {
                debug!(
                    "folding {} ({:?}) into {} ({:?})",
                    t.id, t.title, out[i].id, out[i].title
                );
                out[i].fill_missing_from(&t);
            }
            None => {
                if !t.id.is_empty() {
                    by_id.insert(t.id.clone(), out.len());
                }
                by_title.entry(key).or_default().push(out.len());
                out.push(t);
            }
        }
    }
    let folded = before - out.len();
    (out, folded)
}

/// Collapse a song stored twice — once from each surface — into one row, and
/// report how many rows that removed.
///
/// The duplicates exist because an older build matched likes, recents and
/// playlist entries by video id alone: liking a song from an artist page and
/// then opening the album it is from stored one row per surface (see
/// [`Track::same_song`]). Merging is lossless — the surviving row keeps its
/// position and id and takes the fields it was missing from the row it absorbs,
/// so the artist-less album-page copy ends up with the credit and the
/// duration.
///
/// The rule cannot be certain about every pair (the two cases it accepts are
/// priced in on [`Track::same_song`]), and this runs on playlists the user
/// curated, so the *first* repair snapshots the file to
/// [`PRE_COLLAPSE_BACKUP`] before writing: a wrong merge is recoverable by hand
/// rather than silent.
fn collapse_duplicates(data: &mut Library) -> usize {
    let mut folded = 0;
    for list in [&mut data.liked, &mut data.recently_played] {
        let (merged, n) = fold_song_duplicates(std::mem::take(list));
        *list = merged;
        folded += n;
    }
    for playlist in &mut data.playlists {
        let (merged, n) = fold_song_duplicates(std::mem::take(&mut playlist.tracks));
        playlist.tracks = merged;
        folded += n;
    }
    folded
}

pub struct LibraryStore {
    path: PathBuf,
    pub data: Library,
    /// Preview mode: the library lives in memory only. The real library.json is
    /// neither read nor written, so interacting with placeholder rows can't
    /// persist junk or reorder the user's real playlists.
    ephemeral: bool,
}

impl LibraryStore {
    pub fn load(dir: &std::path::Path) -> Self {
        let path = dir.join("library.json");
        if crate::settings::preview_mode() {
            warn!("preview mode: library runs in memory only and will not be saved");
            return Self {
                path,
                data: Library::default(),
                ephemeral: true,
            };
        }
        let mut data = crate::util::load_json(&path);
        info!("library loaded from {}", path.display());
        let collapsed = collapse_duplicates(&mut data);
        let store = Self {
            path,
            data,
            ephemeral: false,
        };
        if collapsed > 0 {
            // Written back straight away, not left for the next change: the
            // duplicate rows are on disk, and a launch that merely reads them
            // again would keep showing the user two rows for one song.
            warn!("collapsed {collapsed} duplicate song row(s) in the library");
            store.back_up_before_repair();
            store.save();
        }
        store
    }

    /// Copy the library aside before the one-time repair rewrites it.
    ///
    /// Best effort on purpose: a backup that cannot be written must not stop the
    /// repair the user needs, and the write itself stays atomic, so the worst
    /// case is the pre-repair file being lost on a disk already refusing writes.
    fn back_up_before_repair(&self) {
        let backup = self.path.with_file_name(PRE_COLLAPSE_BACKUP);
        match std::fs::copy(&self.path, &backup) {
            Ok(_) => info!("pre-repair library saved to {}", backup.display()),
            Err(e) => warn!("could not back up the library before repairing it: {e}"),
        }
    }

    pub fn save(&self) {
        if self.ephemeral {
            return;
        }
        crate::util::save_json(&self.path, &self.data, "library");
    }

    /// Like a track, or un-like it if it is already liked.
    ///
    /// Matched by song, not by video id, because the two surfaces disagree about
    /// the id ([`Track::same_song`]): the heart on the album page has to find —
    /// and remove — the row the artist page stored, and a library that already
    /// holds both copies has to lose both, or un-liking leaves the heart lit.
    pub fn toggle_like(&mut self, track: Track) {
        let before = self.data.liked.len();
        self.data.liked.retain(|t| !t.same_song(&track));
        if self.data.liked.len() == before {
            self.data.liked.insert(0, track);
        }
        self.save();
    }

    /// Record a play, newest first.
    ///
    /// A song already in the list moves to the front instead of appearing twice,
    /// and the row it replaces keeps the fields the stored copy knew that this
    /// surface's copy does not: playing the album page's row of a song the
    /// artist page's row recorded must not drop the byline from Recently played.
    pub fn push_recent(&mut self, track: Track) {
        let mut track = track;
        self.data.recently_played.retain(|stored| {
            if stored.same_song(&track) {
                track.fill_missing_from(stored);
                false
            } else {
                true
            }
        });
        self.data.recently_played.insert(0, track);
        self.data.recently_played.truncate(RECENT_CAP);
        self.save();
    }

    /// Copy freshly resolved artist credits onto every stored copy of a
    /// track that was saved without them. Saves only if something changed.
    pub fn backfill_track(&mut self, src: &Track) {
        if src.artists.is_empty() {
            return;
        }
        // Matched by song: the creditless copy is usually the one stored from
        // the album page, which carries the *other* surface's video id for this
        // recording — the exact row that needs the credits.
        let apply = |t: &mut Track| -> bool {
            if t.artists.is_empty() && t.same_song(src) {
                t.fill_missing_from(src)
            } else {
                false
            }
        };
        let mut changed = false;
        for t in &mut self.data.liked {
            changed |= apply(t);
        }
        for p in &mut self.data.playlists {
            for t in &mut p.tracks {
                changed |= apply(t);
            }
        }
        for t in &mut self.data.recently_played {
            changed |= apply(t);
        }
        if changed {
            self.save();
        }
    }

    pub fn create_playlist(&mut self, name: String) -> Playlist {
        self.create_playlist_with(name, Vec::new())
    }

    /// Create a playlist pre-populated with `tracks` (used by import). Rows for
    /// the same song within the input are folded into their first occurrence,
    /// preserving first-seen order.
    pub fn create_playlist_with(&mut self, name: String, tracks: Vec<Track>) -> Playlist {
        let (tracks, folded) = fold_song_duplicates(tracks);
        if folded > 0 {
            // The importing surfaces can list one song under two uploads (a
            // playlist that holds both the art track and the music video), and a
            // pre-fix Rift export can hold two ids for one song. Folding them is
            // right, but say so: the file the user imported had more rows than
            // the playlist they get back.
            info!("import: folded {folded} duplicate song row(s)");
        }
        let playlist = Playlist {
            id: format!("pl-{:016x}", rand::random::<u64>()),
            name,
            tracks,
        };
        self.data.playlists.push(playlist.clone());
        self.save();
        playlist
    }

    pub fn delete_playlist(&mut self, id: &str) {
        self.data.playlists.retain(|p| p.id != id);
        self.save();
    }

    pub fn rename_playlist(&mut self, id: &str, name: String) {
        if let Some(p) = self.data.playlists.iter_mut().find(|p| p.id == id) {
            p.name = name;
            self.save();
        }
    }

    /// Move the playlist with `id` to position `to` in the sidebar order.
    /// Returns `true` if the order changed.
    pub fn move_playlist(&mut self, id: &str, to: usize) -> bool {
        let Some(from) = self.data.playlists.iter().position(|p| p.id == id) else {
            return false;
        };
        if from == to || to >= self.data.playlists.len() {
            return false;
        }
        let p = self.data.playlists.remove(from);
        self.data.playlists.insert(to, p);
        self.save();
        true
    }

    pub fn add_to_playlist(&mut self, id: &str, track: Track) {
        if let Some(p) = self.data.playlists.iter_mut().find(|p| p.id == id) {
            // By song, so adding the album page's copy of a song the playlist
            // already holds from the artist page is a no-op rather than a
            // second row for one song.
            if !p.tracks.iter().any(|t| t.same_song(&track)) {
                p.tracks.push(track);
                self.save();
            }
        }
    }

    /// Remove the row with this video id. Deliberately exact: the UI removes the
    /// row the user clicked, and a playlist that somehow held two ids for one
    /// song should lose only that one.
    pub fn remove_from_playlist(&mut self, id: &str, track_id: &str) {
        if let Some(p) = self.data.playlists.iter_mut().find(|p| p.id == id) {
            p.tracks.retain(|t| t.id != track_id);
            self.save();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rift_types::ArtistRef;

    fn track(id: &str) -> Track {
        Track {
            id: id.into(),
            title: id.into(),
            artist: String::new(),
            album: None,
            duration: None,
            cover: String::new(),
            artists: Vec::new(),
            album_id: None,
        }
    }

    /// Daft Punk's *Homework* → "Around the World" as the **album page** renders
    /// it: the official music video's id, the duration, and no artist credit
    /// (album rows leave the byline to the page header).
    fn album_row() -> Track {
        Track {
            id: "K0HSD_i2DvA".into(),
            title: "Around the World".into(),
            artist: String::new(),
            album: Some("Homework".into()),
            duration: Some(430),
            cover: "https://example.invalid/album-art".into(),
            artists: Vec::new(),
            album_id: Some("MPREb_dMHztPsrDgN".into()),
        }
    }

    /// The same song as the **artist page's** Top songs renders it: the art
    /// track's id, the credit, and no duration. Two ids for one song, which is
    /// the whole bug.
    fn artist_row() -> Track {
        Track {
            id: "Jb6gcoR266U".into(),
            title: "Around the World".into(),
            artist: "Daft Punk".into(),
            album: Some("Homework".into()),
            duration: None,
            cover: String::new(),
            artists: vec![ArtistRef {
                id: Some("UCRr1xG_2WIDs18a6cIiCxeA".into()),
                name: "Daft Punk".into(),
            }],
            album_id: Some("MPREb_dMHztPsrDgN".into()),
        }
    }

    fn temp_store(dir: &crate::util::testing::TempDir) -> LibraryStore {
        LibraryStore::load(dir.path())
    }

    #[test]
    fn like_toggles_and_persists_across_reload() {
        let dir = crate::util::testing::TempDir::new("library");
        let mut lib = temp_store(&dir);
        let path = lib.path.clone();
        lib.toggle_like(track("a"));
        assert_eq!(lib.data.liked.len(), 1);

        // Reload from disk: the like survived.
        let reloaded = LibraryStore::load(path.parent().unwrap());
        assert_eq!(reloaded.data.liked.len(), 1);

        lib.toggle_like(track("a"));
        assert!(lib.data.liked.is_empty());
    }

    #[test]
    fn recently_played_dedups_caps_and_orders_newest_first() {
        let dir = crate::util::testing::TempDir::new("library");
        let mut lib = temp_store(&dir);
        lib.push_recent(track("a"));
        lib.push_recent(track("b"));
        lib.push_recent(track("a")); // re-play moves "a" to front, no duplicate
        assert_eq!(
            lib.data
                .recently_played
                .iter()
                .map(|t| t.id.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "b"]
        );

        for i in 0..RECENT_CAP + 5 {
            lib.push_recent(track(&format!("x{i}")));
        }
        assert_eq!(lib.data.recently_played.len(), RECENT_CAP);
    }

    #[test]
    fn playlist_add_is_idempotent_and_remove_works() {
        let dir = crate::util::testing::TempDir::new("library");
        let mut lib = temp_store(&dir);
        let pl = lib.create_playlist("Mix".into());
        lib.add_to_playlist(&pl.id, track("a"));
        lib.add_to_playlist(&pl.id, track("a")); // duplicate ignored
        assert_eq!(lib.data.playlists[0].tracks.len(), 1);

        lib.remove_from_playlist(&pl.id, "a");
        assert!(lib.data.playlists[0].tracks.is_empty());

        lib.delete_playlist(&pl.id);
        assert!(lib.data.playlists.is_empty());
    }

    #[test]
    fn loading_a_library_keeps_a_video_titled_track() {
        // The stored library is the other half of the "no audio filter off the
        // innertube paths" contract: whatever was saved must come back, whether
        // or not its title looks like a music video. A load-time filter would
        // drop the user's row silently, on the next launch, with the file still
        // on disk holding it.
        let dir = crate::util::testing::TempDir::new("library-video-title");
        let mut lib = temp_store(&dir);
        lib.toggle_like(Track {
            title: "Some Song (Official Video)".into(),
            ..track("v")
        });

        let reloaded = LibraryStore::load(dir.path());
        assert_eq!(
            reloaded.data.liked.len(),
            1,
            "the like must survive the save"
        );
        assert_eq!(
            reloaded.data.liked[0].title, "Some Song (Official Video)",
            "the title must come back exactly as stored"
        );
    }

    #[test]
    fn backfill_copies_credits_onto_creditless_copies() {
        let dir = crate::util::testing::TempDir::new("library");
        let mut lib = temp_store(&dir);
        lib.toggle_like(track("a")); // stored without artist credits

        let mut enriched = track("a");
        enriched.artist = "Daft Punk".into();
        enriched.artists = vec![ArtistRef {
            id: Some("chan".into()),
            name: "Daft Punk".into(),
        }];
        lib.backfill_track(&enriched);

        assert_eq!(lib.data.liked[0].artist, "Daft Punk");
        assert_eq!(lib.data.liked[0].artists.len(), 1);
    }

    /// **The reported bug.** Like the song on the artist page, then open the
    /// album it is from and click the heart there. The album page's row is the
    /// same song under a different video id, so the click is an *unlike* — not a
    /// second row for one song.
    #[test]
    fn liking_the_same_song_from_the_album_page_unlikes_it() {
        let dir = crate::util::testing::TempDir::new("library-two-surfaces");
        let mut lib = temp_store(&dir);
        lib.toggle_like(artist_row());
        assert_eq!(lib.data.liked.len(), 1);

        lib.toggle_like(album_row());

        assert!(
            lib.data.liked.is_empty(),
            "the album page's heart must find the song the artist page stored, \
             not add its own copy: {:?}",
            lib.data.liked
        );
    }

    /// A library that already holds both copies — which is what the user's
    /// library.json has — must lose both on one unlike, or the heart stays lit
    /// and the next click re-adds a row.
    #[test]
    fn unliking_clears_every_copy_the_old_id_match_stored() {
        let dir = crate::util::testing::TempDir::new("library-both-copies");
        let mut lib = temp_store(&dir);
        lib.data.liked = vec![album_row(), artist_row()];

        lib.toggle_like(artist_row());

        assert!(
            lib.data.liked.is_empty(),
            "one unlike has to clear both copies: {:?}",
            lib.data.liked
        );
    }

    /// **The duplicate rows the user is looking at.** A stored library with one
    /// song under two video ids comes back as one row that knows both halves:
    /// the artist from the artist page and the duration from the album page.
    /// The repair is written to disk, so the next launch reads one row.
    #[test]
    fn loading_collapses_a_song_stored_from_both_surfaces() {
        let dir = crate::util::testing::TempDir::new("library-collapse");
        let playlist = Playlist {
            id: "pl-1".into(),
            name: "Mix".into(),
            tracks: vec![album_row(), track("other"), artist_row()],
        };
        let stored = Library {
            liked: vec![album_row(), artist_row()],
            recently_played: vec![artist_row(), album_row()],
            playlists: vec![playlist],
        };
        crate::util::save_json(&dir.join("library.json"), &stored, "library");

        let lib = LibraryStore::load(dir.path());

        assert_eq!(
            lib.data.liked.len(),
            1,
            "one song, one row: {:?}",
            lib.data.liked
        );
        let kept = &lib.data.liked[0];
        assert_eq!(
            kept.id, "K0HSD_i2DvA",
            "the first row keeps its id and its position in the list"
        );
        assert_eq!(kept.artist, "Daft Punk", "the credit it was missing");
        assert_eq!(kept.artists.len(), 1);
        assert_eq!(kept.duration, Some(430), "the duration it already had");

        assert_eq!(lib.data.recently_played.len(), 1);
        assert_eq!(
            lib.data.playlists[0]
                .tracks
                .iter()
                .map(|t| t.id.as_str())
                .collect::<Vec<_>>(),
            vec!["K0HSD_i2DvA", "other"],
            "the playlist keeps its order, one row per song"
        );

        // On disk too: a repair that only lived in memory would come back as
        // two rows on the next launch.
        let reloaded = crate::util::load_json::<Library>(&dir.join("library.json"));
        assert_eq!(reloaded.liked.len(), 1);
    }

    /// The other half of the contract: the collapse only merges rows
    /// `same_song` is sure about. Two different songs that share a title — the
    /// same name by a different artist, or the same name on a different album —
    /// stay exactly where they are.
    #[test]
    fn loading_keeps_different_songs_that_share_a_title() {
        let dir = crate::util::testing::TempDir::new("library-namesakes");
        let mut cover = track("cover");
        cover.title = "Around the World".into();
        cover.artist = "Another Artist".into();
        cover.album_id = Some("MPREb_other".into());
        let mut same_artist_other_album = artist_row();
        same_artist_other_album.id = "single".into();
        same_artist_other_album.album_id = Some("MPREb_single".into());

        let stored = Library {
            liked: vec![artist_row(), cover, same_artist_other_album],
            ..Library::default()
        };
        crate::util::save_json(&dir.join("library.json"), &stored, "library");

        let lib = LibraryStore::load(dir.path());

        assert_eq!(
            lib.data.liked.len(),
            3,
            "none of these are the same song: {:?}",
            lib.data.liked
        );
    }

    /// A credit that exists only in the structured field still tells two songs
    /// apart.
    ///
    /// Rows `enrich_track` produced carry `artists` and an empty `artist`, and
    /// that is the shape that would let a cover with the same title and length
    /// collapse into a real like. Pinned at the store because this is the
    /// destructive path: a wrong answer here deletes a row on next launch.
    #[test]
    fn loading_does_not_fold_a_cover_whose_credit_is_structural() {
        let dir = crate::util::testing::TempDir::new("library-structural-credit");
        let mut liked = track("PwILkY9gRrc");
        liked.title = "Da Funk".into();
        liked.artists = vec![ArtistRef {
            id: Some("UCRr1xG_2WIDs18a6cIiCxeA".into()),
            name: "Daft Punk".into(),
        }];
        liked.album_id = Some("MPREb_dMHztPsrDgN".into());
        liked.duration = Some(329);

        let mut karaoke = track("karaoke");
        karaoke.title = "Da Funk".into();
        karaoke.artist = "Karaoke Band".into();
        karaoke.album_id = Some("MPREb_karaoke".into());
        karaoke.duration = Some(329);

        let stored = Library {
            liked: vec![liked, karaoke],
            ..Library::default()
        };
        crate::util::save_json(&dir.join("library.json"), &stored, "library");

        let lib = LibraryStore::load(dir.path());

        assert_eq!(
            lib.data.liked.len(),
            2,
            "the structured credit has to keep these apart: {:?}",
            lib.data.liked
        );
    }

    /// Empty ids are "no id", not one id every corrupt row shares. Two rows
    /// carrying one must be compared on their metadata, exactly like any other
    /// pair — and metadata that disagrees leaves them alone.
    #[test]
    fn loading_does_not_fold_rows_that_only_share_an_empty_id() {
        let dir = crate::util::testing::TempDir::new("library-empty-id");
        let mut first = track("");
        first.title = "First".into();
        first.artist = "Artist A".into();
        let mut second = track("");
        second.title = "Second".into();
        second.artist = "Artist B".into();

        let stored = Library {
            liked: vec![first, second],
            ..Library::default()
        };
        crate::util::save_json(&dir.join("library.json"), &stored, "library");

        let lib = LibraryStore::load(dir.path());

        assert_eq!(lib.data.liked.len(), 2, "empty ids are not an identity");
    }

    /// The repair rewrites the user's file, so the file it replaced has to be
    /// recoverable: the pre-repair snapshot is written before the rewrite and
    /// holds the duplicate rows exactly as an older build stored them.
    #[test]
    fn the_repair_snapshots_the_library_before_rewriting_it() {
        let dir = crate::util::testing::TempDir::new("library-repair-backup");
        let stored = Library {
            liked: vec![album_row(), artist_row()],
            ..Library::default()
        };
        crate::util::save_json(&dir.join("library.json"), &stored, "library");

        let lib = LibraryStore::load(dir.path());

        assert_eq!(lib.data.liked.len(), 1);
        let backup = dir.join(super::PRE_COLLAPSE_BACKUP);
        assert!(
            backup.is_file(),
            "the pre-repair library must be kept at {}",
            backup.display()
        );
        let before: Library = crate::util::load_json(&backup);
        assert_eq!(
            before.liked.len(),
            2,
            "the snapshot must hold the rows the repair replaced: {:?}",
            before.liked
        );
    }

    /// Playing the album's copy of a song already in Recently played moves that
    /// song to the front instead of listing it twice.
    #[test]
    fn recently_played_dedups_by_song_not_by_video() {
        let dir = crate::util::testing::TempDir::new("library-recent-songs");
        let mut lib = temp_store(&dir);
        lib.push_recent(artist_row());
        lib.push_recent(album_row());

        assert_eq!(
            lib.data.recently_played.len(),
            1,
            "one song, one row: {:?}",
            lib.data.recently_played
        );
        assert_eq!(lib.data.recently_played[0].id, "K0HSD_i2DvA");
    }

    /// Moving a song to the front of Recently played must not make the row
    /// poorer: the surface it was just played from may be the half-empty one
    /// (album-page rows have no byline), and the stored copy's fields are what
    /// the row keeps.
    #[test]
    fn replaying_a_song_from_the_other_surface_keeps_its_fields() {
        let dir = crate::util::testing::TempDir::new("library-recent-fields");
        let mut lib = temp_store(&dir);
        lib.push_recent(artist_row()); // credit, no duration
        lib.push_recent(album_row()); // duration, no credit

        let row = &lib.data.recently_played[0];
        assert_eq!(row.id, "K0HSD_i2DvA", "the row that was just played wins");
        assert_eq!(row.artist, "Daft Punk", "the byline it did not carry");
        assert_eq!(row.artists.len(), 1);
        assert_eq!(row.duration, Some(430), "the duration the surface knew");
    }

    #[test]
    fn a_playlist_refuses_the_same_song_from_another_surface() {
        let dir = crate::util::testing::TempDir::new("library-playlist-songs");
        let mut lib = temp_store(&dir);
        let pl = lib.create_playlist("Mix".into());
        lib.add_to_playlist(&pl.id, artist_row());
        lib.add_to_playlist(&pl.id, album_row());

        assert_eq!(
            lib.data.playlists[0].tracks.len(),
            1,
            "adding the album page's copy must not add a second row: {:?}",
            lib.data.playlists[0].tracks
        );

        // Removing the row that is there is still exact, by id.
        lib.remove_from_playlist(&pl.id, "Jb6gcoR266U");
        assert!(lib.data.playlists[0].tracks.is_empty());
    }

    /// An imported playlist is one row per song too: the same song twice in the
    /// source (an album that lists the single, a mix that repeats a track) is
    /// one row here.
    #[test]
    fn an_imported_playlist_folds_one_song_to_one_row() {
        let dir = crate::util::testing::TempDir::new("library-import-songs");
        let mut lib = temp_store(&dir);
        let pl = lib.create_playlist_with(
            "Import".into(),
            vec![album_row(), artist_row(), track("other")],
        );

        assert_eq!(
            pl.tracks.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(),
            vec!["K0HSD_i2DvA", "other"],
            "first-seen order, one row per song"
        );
        assert_eq!(
            pl.tracks[0].artist, "Daft Punk",
            "the surviving row keeps the halves it had and takes the ones it did not"
        );
    }

    /// The half of the old dedup that ids alone used to cover: two rows for one
    /// video, one of them re-titled by a later fetch. `same_song` calls them one
    /// song on the id alone, so the fold has to compare ids as well as titles —
    /// bucketing by title only would keep both.
    #[test]
    fn an_import_folds_a_repeated_id_whose_title_changed() {
        let dir = crate::util::testing::TempDir::new("library-import-retitled");
        let mut lib = temp_store(&dir);
        let mut retitled = album_row();
        retitled.title = "Around the World (Remastered)".into();

        let pl = lib.create_playlist_with("Import".into(), vec![album_row(), retitled]);

        assert_eq!(
            pl.tracks.len(),
            1,
            "one video, one row, whatever the two titles say: {:?}",
            pl.tracks
        );
    }

    /// Credits resolved for one surface's copy fill in the other surface's copy
    /// — the album page's artist-less row is exactly the one that needs them.
    #[test]
    fn backfill_crosses_to_the_other_surfaces_copy() {
        let dir = crate::util::testing::TempDir::new("library-backfill-songs");
        let mut lib = temp_store(&dir);
        lib.toggle_like(album_row()); // stored without the credit
        assert!(lib.data.liked[0].artists.is_empty());

        lib.backfill_track(&artist_row());

        assert_eq!(lib.data.liked[0].artist, "Daft Punk");
        assert_eq!(lib.data.liked[0].artists.len(), 1);
        assert_eq!(
            lib.data.liked[0].duration,
            Some(430),
            "the credit is added, the duration it already had is kept"
        );
    }
}
