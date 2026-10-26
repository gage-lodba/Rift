//! JSON-persisted user library: liked songs, playlists, recently played.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use rift_types::{Library, Playlist, Track};
use tracing::{debug, info, warn};

use crate::util::DurableWriter;

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
        if let Some(i) = existing {
            debug!(
                "folding {} ({:?}) into {} ({:?})",
                t.id, t.title, out[i].id, out[i].title
            );
            // The surviving row absorbs what the folded row knew; whether that
            // changed a field is irrelevant here because folding the row at all
            // is what schedules the one-time save below.
            let _ = out[i].fill_missing_from(&t);
        } else {
            if !t.id.is_empty() {
                by_id.insert(t.id.clone(), out.len());
            }
            by_title.entry(key).or_default().push(out.len());
            out.push(t);
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

/// Whether a stored row still has the user-visible metadata gap: no duration,
/// or neither a byline nor structured artist credits.
fn is_incomplete(track: &Track) -> bool {
    track.duration.is_none() || (track.artist.is_empty() && track.artists.is_empty())
}

/// Whether two rows share a **non-empty** video id.
///
/// An empty id is "no id", not an id every empty-id row shares, so the id
/// shortcut must never match on it — the rule [`Track::same_song`] itself uses.
fn same_video_id(a: &str, b: &str) -> bool {
    !a.is_empty() && a == b
}

/// Whether `source` carries any field that could fill a stored row. Used to skip
/// the whole matching pass for rows that know nothing (empty placeholders).
fn has_metadata_to_offer(source: &Track) -> bool {
    source.duration.is_some()
        || !source.artist.is_empty()
        || !source.artists.is_empty()
        || source.album.is_some()
        || !source.cover.is_empty()
        || source.album_id.is_some()
}

/// Whether `stored` has at least one empty field `fill_missing_from` could fill.
/// The cheap pre-check keeps the startup queue × library merge from running the
/// song matcher for every pair.
fn needs_metadata(stored: &Track) -> bool {
    stored.duration.is_none()
        || stored.artist.is_empty()
        || stored.artists.is_empty()
        || stored.album.is_none()
        || stored.cover.is_empty()
        || stored.album_id.is_none()
}

pub struct LibraryStore {
    path: PathBuf,
    pub data: Library,
    /// Preview mode: the library lives in memory only. The real library.json is
    /// neither read nor written, so interacting with placeholder rows can't
    /// persist junk or reorder the user's real playlists.
    ephemeral: bool,
    /// The backend's durable writer. Saves are queued to it instead of written
    /// here, so a command called from the UI thread never blocks on disk I/O.
    writer: Arc<DurableWriter>,
}

impl LibraryStore {
    /// Load the library at startup.
    ///
    /// Loading stays **synchronous**: startup must finish before the app can
    /// run, and the repair below has to be part of the returned state. The one
    /// write this can perform (the collapse repair's save) is queued to
    /// `writer` like every other save, but [`back_up_before_repair`] is a direct
    /// `std::fs::copy` — startup-only, once per repaired launch, and deliberate
    /// rather than an oversight.
    ///
    /// [`back_up_before_repair`]: Self::back_up_before_repair
    pub fn load(dir: &std::path::Path, writer: &Arc<DurableWriter>) -> Self {
        let path = dir.join("library.json");
        if crate::settings::preview_mode() {
            warn!("preview mode: library runs in memory only and will not be saved");
            return Self {
                path,
                data: Library::default(),
                ephemeral: true,
                writer: Arc::clone(writer),
            };
        }
        let mut data = crate::util::load_json(&path);
        info!("library loaded from {}", path.display());
        let collapsed = collapse_duplicates(&mut data);
        let store = Self {
            path,
            data,
            ephemeral: false,
            writer: Arc::clone(writer),
        };
        if collapsed > 0 {
            // Written back straight away, not left for the next change: the
            // duplicate rows are on disk, and a launch that merely reads them
            // again would keep showing the user two rows for one song. The save
            // is queued on the durable writer, so startup does not block on it
            // (the backend's shutdown flush makes it durable).
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
    ///
    /// This copy is deliberately synchronous: it runs once, during startup, and
    /// the repair it protects must not be able to outrun it.
    fn back_up_before_repair(&self) {
        let backup = self.path.with_file_name(PRE_COLLAPSE_BACKUP);
        match std::fs::copy(&self.path, &backup) {
            Ok(_) => info!("pre-repair library saved to {}", backup.display()),
            Err(e) => warn!("could not back up the library before repairing it: {e}"),
        }
    }

    /// Queue the library to be written to disk.
    ///
    /// Fire-and-forget by design: every mutation below calls this, and a
    /// mutation is normally triggered by a click on the UI thread, so blocking
    /// that thread on a JSON write + rename is exactly the latency this avoids.
    /// The durable writer serializes and atomically writes the snapshot in
    /// queue order; [`DurableWriter::flush`] is the barrier that makes the
    /// result observable (tests and shutdown).
    pub fn save(&self) {
        if self.ephemeral {
            return;
        }
        self.writer
            .write(self.path.clone(), "library", self.data.clone(), |data| {
                serde_json::to_vec_pretty(data).map_err(|e| e.to_string())
            });
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

    /// Record a play and repair the stored copies of its song in **one** save.
    ///
    /// The playback announcement path used to call `apply_metadata` (which saves)
    /// and then `push_recent` (which saves again): two whole-library writes and
    /// two serializations for every played track. This merges the played row into
    /// every stored copy, moves it to the front of Recently played, and queues a
    /// single write. Returns whether the merge changed a stored row.
    pub fn record_played(&mut self, track: Track) -> bool {
        let changed = self.merge_one(&track);
        self.insert_recent(track);
        self.save();
        changed
    }

    /// Insert `track` at the front of Recently played, folding it into an
    /// existing row for the same song and keeping that row's known fields.
    /// Save-free: callers decide when the single write happens.
    fn insert_recent(&mut self, mut track: Track) {
        self.data.recently_played.retain(|stored| {
            if stored.same_song(&track) {
                // A re-play moves the song to the front; whether the stored row
                // could teach the played copy anything does not change that.
                let _ = track.fill_missing_from(stored);
                false
            } else {
                true
            }
        });
        self.data.recently_played.insert(0, track);
        self.data.recently_played.truncate(RECENT_CAP);
    }

    /// The startup-cost-limited companion to [`apply_metadata`](Self::apply_metadata):
    /// only the lists the
    /// user can see at launch (`liked` + `recently_played`), and only rows with a
    /// user-visible gap ([`is_incomplete`]).
    ///
    /// `RiftBackend::new` merges the restored playback queue synchronously, so
    /// the matching must stay cheap for a large imported library: playlists are
    /// skipped here (they are repaired on play by
    /// [`record_played`](Self::record_played)) and fully-visible rows are not
    /// even considered (`needs_metadata` would also look at cover/album, which is
    /// not worth a startup pass). Fields beyond the visible gap may still be
    /// filled as a side effect of `fill_missing_from` on a matched row.
    pub fn merge_visible_metadata_from(&mut self, sources: &[Track]) -> bool {
        let mut changed = false;
        for source in sources {
            if !has_metadata_to_offer(source) {
                continue;
            }
            changed |= self.merge_visible_one(source);
        }
        if changed {
            self.save();
        }
        changed
    }

    /// Fill a visible incomplete row from `source`. See
    /// [`merge_visible_metadata_from`](Self::merge_visible_metadata_from) for the
    /// restriction, and [`merge_one`](Self::merge_one) for the general rule.
    fn merge_visible_one(&mut self, source: &Track) -> bool {
        let mut changed = false;
        let apply = |stored: &mut Track| -> bool {
            if !is_incomplete(stored) {
                return false;
            }
            if same_video_id(&stored.id, &source.id) || stored.same_song(source) {
                stored.fill_missing_from(source)
            } else {
                false
            }
        };
        for t in &mut self.data.liked {
            changed |= apply(t);
        }
        for t in &mut self.data.recently_played {
            changed |= apply(t);
        }
        changed
    }

    /// Merge one freshly fetched candidate into every stored copy of its song.
    ///
    /// Same matching rule as [`merge_visible_metadata_from`](Self::merge_visible_metadata_from)
    /// but over every list: matched by id or [`Track::same_song`], fills missing
    /// fields only, saves once.
    /// Used by the background metadata repair and by playback announcements so a
    /// played track repairs its liked/recent/playlist copies.
    pub fn apply_metadata(&mut self, candidate: &Track) -> bool {
        if self.merge_one(candidate) {
            self.save();
            true
        } else {
            false
        }
    }

    /// Fill every stored copy of `source`'s song. Returns whether any field on
    /// any row changed.
    ///
    /// The lock is per call by design: callers run this while holding the
    /// library lock and never across an `await`.
    fn merge_one(&mut self, source: &Track) -> bool {
        let mut changed = false;
        let apply = |stored: &mut Track| -> bool {
            if !needs_metadata(stored) {
                return false;
            }
            if same_video_id(&stored.id, &source.id) || stored.same_song(source) {
                stored.fill_missing_from(source)
            } else {
                false
            }
        };
        for t in &mut self.data.liked {
            changed |= apply(t);
        }
        for t in &mut self.data.recently_played {
            changed |= apply(t);
        }
        for playlist in &mut self.data.playlists {
            for t in &mut playlist.tracks {
                changed |= apply(t);
            }
        }
        changed
    }

    /// Clones of stored rows (from `liked` and `recently_played`, deduplicated)
    /// that are still missing metadata, capped at `limit`.
    ///
    /// "Incomplete" is the user-visible bug: no duration, or neither a byline
    /// nor structured credits. Playlist-only rows are deliberately excluded —
    /// they are not shown in the Liked view, and including them would spend the
    /// bounded network repair on rows the user cannot see. The result is a
    /// snapshot for the background repair to look up one row at a time.
    pub fn incomplete_tracks(&self, limit: usize) -> Vec<Track> {
        let mut out = Vec::new();
        if limit == 0 {
            return out;
        }
        let mut seen: HashSet<String> = HashSet::new();
        for track in self
            .data
            .liked
            .iter()
            .chain(self.data.recently_played.iter())
        {
            if out.len() >= limit {
                break;
            }
            // Filter before deduplicating: a *complete* liked row must not mask
            // an incomplete recently-played row for the same video.
            if !is_incomplete(track) {
                continue;
            }
            // Empty ids are "no id", not one id every corrupt row shares; those
            // dedupe on the same title key the song matcher compares on.
            let key = if track.id.is_empty() {
                track.title_key()
            } else {
                track.id.clone()
            };
            if !seen.insert(key) {
                continue;
            }
            out.push(track.clone());
        }
        out
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

    /// Delete a playlist by id. Saves only when one was actually removed.
    pub fn delete_playlist(&mut self, id: &str) {
        let before = self.data.playlists.len();
        self.data.playlists.retain(|p| p.id != id);
        if self.data.playlists.len() != before {
            self.save();
        }
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
    /// song should lose only that one. Saves only when a row was removed.
    pub fn remove_from_playlist(&mut self, id: &str, track_id: &str) {
        if let Some(p) = self.data.playlists.iter_mut().find(|p| p.id == id) {
            let before = p.tracks.len();
            p.tracks.retain(|t| t.id != track_id);
            if p.tracks.len() != before {
                self.save();
            }
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

    /// A **complete** copy of the same song, the shape the restored playback
    /// queue or a fresh metadata lookup carries: id, byline, credits and length
    /// all known. This is the repair source the bug report is about.
    fn complete_row(id: &str) -> Track {
        Track {
            id: id.into(),
            title: "Around the World".into(),
            artist: "Daft Punk".into(),
            album: Some("Homework".into()),
            duration: Some(430),
            cover: "https://example.invalid/album-art".into(),
            artists: vec![ArtistRef {
                id: Some("UCRr1xG_2WIDs18a6cIiCxeA".into()),
                name: "Daft Punk".into(),
            }],
            album_id: Some("MPREb_dMHztPsrDgN".into()),
        }
    }

    /// A store plus the durable writer it saves through.
    ///
    /// Tests hold the writer so they can `flush()` — the deterministic barrier —
    /// instead of sleeping for the background write (or hoping it finished).
    fn temp_store(
        dir: &crate::util::testing::TempDir,
    ) -> (LibraryStore, std::sync::Arc<DurableWriter>) {
        let writer = std::sync::Arc::new(DurableWriter::spawn());
        (LibraryStore::load(dir.path(), &writer), writer)
    }

    #[test]
    fn like_toggles_and_persists_across_reload() {
        let dir = crate::util::testing::TempDir::new("library");
        let (mut lib, writer) = temp_store(&dir);
        let path = lib.path.clone();
        lib.toggle_like(track("a"));
        assert_eq!(lib.data.liked.len(), 1);

        // Reload from disk: the like survived. The flush is the barrier — no
        // sleeping for the background write.
        assert!(writer.flush(), "the like must reach disk before the reload");
        let reloaded = LibraryStore::load(path.parent().unwrap(), &writer);
        assert_eq!(reloaded.data.liked.len(), 1);

        lib.toggle_like(track("a"));
        assert!(lib.data.liked.is_empty());
    }

    #[test]
    fn recently_played_dedups_caps_and_orders_newest_first() {
        let dir = crate::util::testing::TempDir::new("library");
        let (mut lib, _writer) = temp_store(&dir);
        lib.record_played(track("a"));
        lib.record_played(track("b"));
        lib.record_played(track("a")); // re-play moves "a" to front, no duplicate
        assert_eq!(
            lib.data
                .recently_played
                .iter()
                .map(|t| t.id.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "b"]
        );

        for i in 0..RECENT_CAP + 5 {
            lib.record_played(track(&format!("x{i}")));
        }
        assert_eq!(lib.data.recently_played.len(), RECENT_CAP);
    }

    #[test]
    fn playlist_add_is_idempotent_and_remove_works() {
        let dir = crate::util::testing::TempDir::new("library");
        let (mut lib, _writer) = temp_store(&dir);
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
        let (mut lib, writer) = temp_store(&dir);
        lib.toggle_like(Track {
            title: "Some Song (Official Video)".into(),
            ..track("v")
        });

        assert!(writer.flush(), "the like must reach disk before the reload");
        let reloaded = LibraryStore::load(dir.path(), &writer);
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
    fn apply_metadata_copies_credits_onto_creditless_copies() {
        let dir = crate::util::testing::TempDir::new("library");
        let (mut lib, _writer) = temp_store(&dir);
        lib.toggle_like(track("a")); // stored without artist credits

        let mut enriched = track("a");
        enriched.artist = "Daft Punk".into();
        enriched.artists = vec![ArtistRef {
            id: Some("chan".into()),
            name: "Daft Punk".into(),
        }];
        lib.apply_metadata(&enriched);

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
        let (mut lib, _writer) = temp_store(&dir);
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
        let (mut lib, _writer) = temp_store(&dir);
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

        let (lib, writer) = temp_store(&dir);

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
        // two rows on the next launch. The repair's save is queued on the
        // durable writer, so flush is the barrier that makes it readable.
        assert!(writer.flush(), "the repair must be written back");
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

        let (lib, _writer) = temp_store(&dir);

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

        let (lib, _writer) = temp_store(&dir);

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

        let (lib, _writer) = temp_store(&dir);

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

        let (lib, _writer) = temp_store(&dir);

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
        let (mut lib, _writer) = temp_store(&dir);
        lib.record_played(artist_row());
        lib.record_played(album_row());

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
        let (mut lib, _writer) = temp_store(&dir);
        lib.record_played(artist_row()); // credit, no duration
        lib.record_played(album_row()); // duration, no credit

        let row = &lib.data.recently_played[0];
        assert_eq!(row.id, "K0HSD_i2DvA", "the row that was just played wins");
        assert_eq!(row.artist, "Daft Punk", "the byline it did not carry");
        assert_eq!(row.artists.len(), 1);
        assert_eq!(row.duration, Some(430), "the duration the surface knew");
    }

    #[test]
    fn a_playlist_refuses_the_same_song_from_another_surface() {
        let dir = crate::util::testing::TempDir::new("library-playlist-songs");
        let (mut lib, _writer) = temp_store(&dir);
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
        let (mut lib, _writer) = temp_store(&dir);
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
        let (mut lib, _writer) = temp_store(&dir);
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
    fn apply_metadata_crosses_to_the_other_surfaces_copy() {
        let dir = crate::util::testing::TempDir::new("library-backfill-songs");
        let (mut lib, _writer) = temp_store(&dir);
        lib.toggle_like(album_row()); // stored without the credit
        assert!(lib.data.liked[0].artists.is_empty());

        lib.apply_metadata(&artist_row());

        assert_eq!(lib.data.liked[0].artist, "Daft Punk");
        assert_eq!(lib.data.liked[0].artists.len(), 1);
        assert_eq!(
            lib.data.liked[0].duration,
            Some(430),
            "the credit is added, the duration it already had is kept"
        );
    }

    /// The UI-thread contract: a click queues a save and returns immediately, so
    /// several rapid mutations are several queued snapshots rather than three
    /// disk writes. The writer applies them FIFO, which is what makes the *last*
    /// state — not an intermediate one — the state the next launch reads.
    #[test]
    fn rapid_mutations_are_written_in_order_and_the_last_one_wins() {
        let dir = crate::util::testing::TempDir::new("library-rapid-writes");
        let (mut lib, writer) = temp_store(&dir);

        // Three mutations with no wait between them, ending on an unlike: the
        // file must not be left at the middle snapshot.
        lib.toggle_like(artist_row());
        lib.toggle_like(track("b"));
        lib.toggle_like(artist_row());
        assert_eq!(
            lib.data
                .liked
                .iter()
                .map(|t| t.id.as_str())
                .collect::<Vec<_>>(),
            vec!["b"],
            "final in-memory state: only b is liked"
        );

        assert!(
            writer.flush(),
            "every queued save must be attempted before the reload"
        );
        let reloaded = LibraryStore::load(dir.path(), &writer);
        assert_eq!(
            reloaded
                .data
                .liked
                .iter()
                .map(|t| t.id.as_str())
                .collect::<Vec<_>>(),
            vec!["b"],
            "the last queued save must be the one on disk, not an intermediate one"
        );
    }

    /// A like is durable as soon as the writer has flushed, even with the store
    /// (and its last strong writer handle) dropped — the writer's own drain is
    /// the last line of defence on a shutdown path that forgot to flush.
    #[test]
    fn a_flushed_like_survives_dropping_the_store() {
        let dir = crate::util::testing::TempDir::new("library-drop-durable");
        {
            let (mut lib, writer) = temp_store(&dir);
            lib.toggle_like(track("kept"));
            assert!(writer.flush(), "the like must reach disk");
        } // store + writer dropped here.

        let writer = std::sync::Arc::new(DurableWriter::spawn());
        let reloaded = LibraryStore::load(dir.path(), &writer);
        assert_eq!(reloaded.data.liked.len(), 1);
        assert_eq!(reloaded.data.liked[0].id, "kept");
    }

    /// The mirror image: a mutation is queued at call time, so dropping the store
    /// (without flushing first) and flushing the writer afterwards still persists
    /// the last queued save. This is the "store owner dies, writer outlives it"
    /// shape the shutdown path relies on.
    #[test]
    fn a_save_survives_dropping_the_store_before_the_flush() {
        let dir = crate::util::testing::TempDir::new("library-drop-before-flush");
        let writer = std::sync::Arc::new(DurableWriter::spawn());
        {
            let mut lib = LibraryStore::load(dir.path(), &writer);
            lib.create_playlist("Deferred Mix".into());
        } // store dropped; the writer (and its queued save) stays alive.

        assert!(writer.flush(), "the queued save must drain after the drop");
        let reloaded = LibraryStore::load(dir.path(), &writer);
        assert_eq!(reloaded.data.playlists.len(), 1);
        assert_eq!(reloaded.data.playlists[0].name, "Deferred Mix");
    }

    // -----------------------------------------------------------------------
    // Metadata repair (task-45): the reported bug is a liked row showing no
    // duration / artist because it was liked from a surface that carried
    // neither. These tests pin the offline half of the repair.
    // -----------------------------------------------------------------------

    /// The startup merge: a complete playback-queue copy fills the missing
    /// fields on the liked row, on a recently-played copy of the same song under
    /// the *other* video id, and on a playlist copy — then saves once.
    #[test]
    fn apply_metadata_fills_missing_fields_across_lists_and_saves() {
        let dir = crate::util::testing::TempDir::new("library-merge");
        let (mut lib, writer) = temp_store(&dir);

        // Liked from the album page: duration, no byline/credits.
        lib.data.liked = vec![album_row()];
        // Recently played: the artist page's copy of the same song (byline, no
        // duration), stored under the art track's other video id.
        lib.data.recently_played = vec![artist_row()];
        // A playlist copy of the same song with neither half.
        lib.data.playlists.push(Playlist {
            id: "pl-1".into(),
            name: "Mix".into(),
            tracks: vec![Track {
                duration: None,
                album: None,
                album_id: None,
                ..album_row()
            }],
        });

        let changed = lib.apply_metadata(&complete_row("K0HSD_i2DvA"));
        assert!(changed, "the merge must report that it repaired rows");

        let liked = &lib.data.liked[0];
        assert_eq!(liked.artist, "Daft Punk", "the missing byline");
        assert_eq!(liked.artists.len(), 1, "the missing structured credit");
        assert_eq!(liked.duration, Some(430), "the known duration is kept");

        let recent = &lib.data.recently_played[0];
        assert_eq!(
            recent.duration,
            Some(430),
            "the artist-page row is the same song under the other id"
        );
        assert_eq!(
            recent.artist, "Daft Punk",
            "and its known byline must not be overwritten"
        );

        let playlist_row = &lib.data.playlists[0].tracks[0];
        assert_eq!(playlist_row.artist, "Daft Punk");
        assert_eq!(playlist_row.duration, Some(430));

        // Persisted: reload reads the repaired rows back.
        assert!(
            writer.flush(),
            "the repair must reach disk before the reload"
        );
        let reloaded = LibraryStore::load(dir.path(), &writer);
        assert_eq!(reloaded.data.liked[0].artist, "Daft Punk");
        assert_eq!(reloaded.data.liked[0].duration, Some(430));

        // Idempotent: a second merge has nothing left to fill.
        assert!(
            !lib.apply_metadata(&complete_row("K0HSD_i2DvA")),
            "a complete row must report unchanged"
        );
    }

    /// Known fields are never overwritten, even when the matching source carries
    /// a different value under the same id.
    #[test]
    fn apply_metadata_never_overwrites_known_fields() {
        let dir = crate::util::testing::TempDir::new("library-merge-known");
        let (mut lib, _writer) = temp_store(&dir);
        let known = complete_row("same-id");
        lib.data.liked = vec![known.clone()];

        let mut conflicting = complete_row("same-id");
        conflicting.duration = Some(999);
        conflicting.artist = "Somebody Else".into();
        conflicting.album = Some("Another Album".into());
        conflicting.cover = "https://example.invalid/other".into();

        assert!(
            !lib.apply_metadata(&conflicting),
            "a fully-known row has nothing to fill"
        );
        assert_eq!(lib.data.liked[0], known, "no field may be overwritten");
    }

    /// The cross-surface half: the liked row has the artist page's id, the
    /// complete queue row has the album page's id, and `same_song` is what
    /// bridges them.
    #[test]
    fn apply_metadata_matches_the_other_surface_id_via_same_song() {
        let dir = crate::util::testing::TempDir::new("library-merge-song");
        let (mut lib, writer) = temp_store(&dir);
        lib.data.liked = vec![artist_row()];

        assert!(lib.apply_metadata(&album_row()));

        assert_eq!(lib.data.liked[0].id, "Jb6gcoR266U", "the row keeps its id");
        assert_eq!(
            lib.data.liked[0].duration,
            Some(430),
            "filled via same_song"
        );
        assert_eq!(lib.data.liked[0].artist, "Daft Punk", "known byline kept");

        assert!(writer.flush());
        let reloaded = LibraryStore::load(dir.path(), &writer);
        assert_eq!(reloaded.data.liked[0].duration, Some(430));
    }

    /// `incomplete_tracks`: only the user-visible gaps, deduplicated, capped,
    /// and never playlist-only rows.
    #[test]
    fn incomplete_tracks_filters_dedupes_limits_and_ignores_playlists() {
        let dir = crate::util::testing::TempDir::new("library-incomplete");
        let (mut lib, _writer) = temp_store(&dir);

        let mut no_duration = album_row(); // duration known, byline missing
        no_duration.duration = None;
        let mut no_credit = complete_row("creditless"); // duration known, no credit
        no_credit.artist = String::new();
        no_credit.artists.clear();

        lib.data.liked = vec![complete_row("complete"), no_duration.clone(), no_credit];
        lib.data.recently_played = vec![no_duration]; // same id, deduped
        lib.data.playlists.push(Playlist {
            id: "pl-1".into(),
            name: "Mix".into(),
            tracks: vec![Track {
                duration: None,
                artist: String::new(),
                artists: Vec::new(),
                ..album_row()
            }],
        });

        let found = lib.incomplete_tracks(10);
        assert_eq!(
            found.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(),
            vec!["K0HSD_i2DvA", "creditless"],
            "the duplicate across liked+recent is deduped; the complete row and \
             the playlist-only row are excluded"
        );
        assert_eq!(
            lib.incomplete_tracks(1).len(),
            1,
            "the limit caps the batch"
        );
        assert!(lib.incomplete_tracks(0).is_empty(), "limit 0 means no work");
    }

    /// Empty ids are "no id", so the dedupe falls back to the same title key the
    /// song matcher uses.
    #[test]
    fn incomplete_tracks_dedupes_empty_id_rows_by_title() {
        let dir = crate::util::testing::TempDir::new("library-incomplete-title");
        let (mut lib, _writer) = temp_store(&dir);
        lib.data.liked = vec![track(""), track("")];
        assert_eq!(
            lib.incomplete_tracks(10).len(),
            1,
            "two empty-id rows for the same song are one lookup"
        );
    }

    /// `apply_metadata` merges by id **and** by `same_song`, fills only missing
    /// fields, and persists.
    #[test]
    fn apply_metadata_merges_by_id_and_by_song_and_persists() {
        let dir = crate::util::testing::TempDir::new("library-apply");
        let (mut lib, writer) = temp_store(&dir);
        lib.data.liked = vec![album_row(), artist_row()];

        assert!(lib.apply_metadata(&complete_row("K0HSD_i2DvA")));

        // By id (album row) and by same_song (artist row, other id).
        for row in &lib.data.liked {
            assert_eq!(row.artist, "Daft Punk");
            assert_eq!(row.duration, Some(430));
        }
        assert!(
            !lib.apply_metadata(&complete_row("K0HSD_i2DvA")),
            "a second merge has nothing left to fill"
        );

        assert!(writer.flush());
        let reloaded = LibraryStore::load(dir.path(), &writer);
        // The load-time collapse folds the two surfaces' rows into one complete
        // row, which is the same repaired state a restart shows.
        assert_eq!(reloaded.data.liked.len(), 1);
        assert_eq!(reloaded.data.liked[0].artist, "Daft Punk");
        assert_eq!(reloaded.data.liked[0].duration, Some(430));
    }

    /// Empty sources (or sources that know nothing) are a no-op: the startup
    /// merge runs on every launch, so it must be free when there is nothing to
    /// merge.
    #[test]
    fn apply_metadata_with_no_useful_sources_reports_unchanged() {
        let dir = crate::util::testing::TempDir::new("library-merge-empty");
        let (mut lib, _writer) = temp_store(&dir);
        lib.data.liked = vec![album_row()];

        assert!(!lib.apply_metadata(&track("empty-knowledge")));
        assert_eq!(lib.data.liked[0].artist, "");
    }

    /// An empty id is "no id", so an empty-id source must not merge into every
    /// empty-id row by id equality (`Track::same_song`'s rule, applied here).
    #[test]
    fn an_empty_id_source_does_not_merge_by_id() {
        let dir = crate::util::testing::TempDir::new("library-merge-empty-id");
        let (mut lib, _writer) = temp_store(&dir);
        lib.data.liked = vec![track("")];

        let mut source = complete_row("");
        source.title = "Something Else".into();

        assert!(
            !lib.apply_metadata(&source),
            "an empty id must not match every empty-id row"
        );
        assert_eq!(lib.data.liked[0].artist, "");
    }

    /// The filter-before-dedupe fix: a complete liked row must not mask an
    /// incomplete recently-played row for the same video.
    #[test]
    fn incomplete_tracks_does_not_let_a_complete_liked_row_mask_a_recent_one() {
        let dir = crate::util::testing::TempDir::new("library-incomplete-mask");
        let (mut lib, _writer) = temp_store(&dir);

        lib.data.liked = vec![complete_row("K0HSD_i2DvA")];
        let mut incomplete_recent = album_row();
        incomplete_recent.duration = None; // album row with no length: the bug shape
        lib.data.recently_played = vec![incomplete_recent];

        let found = lib.incomplete_tracks(10);
        assert_eq!(
            found.len(),
            1,
            "the incomplete recent row must not be masked"
        );
        assert_eq!(found[0].id, "K0HSD_i2DvA");
        assert!(found[0].duration.is_none());
    }

    /// The startup-cost-limited variant touches only visible lists and only rows
    /// with a user-visible gap: playlist-only rows and rows missing only cosmetic
    /// fields are left for playback repair.
    #[test]
    fn merge_visible_metadata_from_repairs_visible_rows_only() {
        let dir = crate::util::testing::TempDir::new("library-merge-visible");
        let (mut lib, _writer) = temp_store(&dir);

        lib.data.liked = vec![album_row()]; // no byline/credits: visible gap
        lib.data.recently_played = vec![artist_row()]; // no duration: visible gap
        lib.data.playlists.push(Playlist {
            id: "pl-1".into(),
            name: "Mix".into(),
            tracks: vec![Track {
                duration: None,
                artist: String::new(),
                artists: Vec::new(),
                ..album_row()
            }],
        });
        let mut cosmetic = complete_row("cosmetic");
        cosmetic.cover = String::new(); // complete metadata, only cover missing
        lib.data.liked.push(cosmetic);

        assert!(lib.merge_visible_metadata_from(&[complete_row("K0HSD_i2DvA")]));

        assert_eq!(lib.data.liked[0].artist, "Daft Punk");
        assert_eq!(lib.data.recently_played[0].duration, Some(430));
        assert!(
            lib.data.playlists[0].tracks[0].artist.is_empty()
                && lib.data.playlists[0].tracks[0].duration.is_none(),
            "playlists are repaired on play, not on the startup path"
        );
        assert!(
            lib.data.liked[1].cover.is_empty(),
            "a complete row missing only cosmetic fields is not a startup target"
        );
    }

    /// A played track repairs the stored copies and enters Recently played with
    /// one save (the announcement path used to save the whole library twice).
    #[test]
    fn record_played_merges_and_records_a_play_in_one_save() {
        let dir = crate::util::testing::TempDir::new("library-record-played");
        let (mut lib, writer) = temp_store(&dir);
        lib.data.liked = vec![album_row()]; // missing byline/credits

        assert!(
            lib.record_played(complete_row("K0HSD_i2DvA")),
            "the merge changed the liked row"
        );
        assert_eq!(lib.data.liked[0].artist, "Daft Punk");
        assert_eq!(lib.data.recently_played.len(), 1);
        assert_eq!(lib.data.recently_played[0].duration, Some(430));

        // The row is already complete, so a replay only moves it (no merge).
        assert!(!lib.record_played(complete_row("K0HSD_i2DvA")));
        assert_eq!(lib.data.recently_played.len(), 1);

        assert!(writer.flush(), "the single queued save must reach disk");
        let reloaded = LibraryStore::load(dir.path(), &writer);
        assert_eq!(reloaded.data.liked[0].artist, "Daft Punk");
        assert_eq!(reloaded.data.recently_played.len(), 1);
    }
}
