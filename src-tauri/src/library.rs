//! JSON-persisted user library: liked songs, playlists, recently played.

use std::collections::HashMap;
use std::path::PathBuf;

use rift_types::{Library, Playlist, Track};
use tracing::{info, warn};

const RECENT_CAP: usize = 30;

/// Fold rows that are the same song ([`Track::same_song`]) into their first
/// occurrence, which keeps its position and its id and absorbs whatever a later
/// row knew that it did not. Returns the folded rows and how many were folded
/// away.
///
/// Two lookups rather than one, because `same_song` has two ways in: the same id
/// is the same song whatever the titles say, and otherwise equal titles are
/// required — so an id hit is checked first and the title bucket after it.
/// Bucketing by title is what keeps this linear for the long lists (a
/// 1000-track import) that a pairwise scan would make quadratic.
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
        let existing = by_id.get(&t.id).copied().or_else(|| {
            by_title
                .get(&key)
                .and_then(|positions| positions.iter().copied().find(|&i| out[i].same_song(&t)))
        });
        match existing {
            Some(i) => {
                out[i].fill_missing_from(&t);
            }
            None => {
                by_id.insert(t.id.clone(), out.len());
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
/// duration — and [`Track::same_song`] is deliberately conservative about which
/// rows count as the same song, so nothing else can be removed by this.
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
            store.save();
        }
        store
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

    pub fn push_recent(&mut self, track: Track) {
        self.data.recently_played.retain(|t| !t.same_song(&track));
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
    /// the same song within the input are dropped, preserving first-seen order.
    pub fn create_playlist_with(&mut self, name: String, tracks: Vec<Track>) -> Playlist {
        let playlist = Playlist {
            id: format!("pl-{:016x}", rand::random::<u64>()),
            name,
            tracks: fold_song_duplicates(tracks).0,
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
