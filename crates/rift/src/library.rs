//! JSON-persisted user library: liked songs, playlists, recently played.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use rift_types::{Library, Playlist, Track};
use serde::Deserialize;
use serde_json::Value;
use tracing::{debug, info, warn};

use crate::util::{DurableWriter, LoadStatus};

const RECENT_CAP: usize = 30;

/// Where the pre-repair library is snapshotted before the one-time collapse
/// (or a tolerant parse that dropped rows) rewrites it. A sibling of
/// `library.json`, so the repair is recoverable by hand even if the merge rule
/// ever guesses wrong about a song. A later repair never overwrites an earlier
/// snapshot: it gets `.1`, `.2`, … (see [`crate::util::free_backup_path`]).
const PRE_COLLAPSE_BACKUP: &str = "library.json.pre-collapse";

/// How many dropped-row labels a warning quotes, so a badly broken file cannot
/// turn one log line into a megabyte of ids.
const MAX_DROPPED_LABELS: usize = 8;

/// The categories a tolerant load counts separately, so the warning names what
/// was lost instead of one opaque total.
#[derive(Debug, Clone, Copy)]
enum DroppedKind {
    Liked,
    Recent,
    Playlist,
    PlaylistTrack,
}

impl DroppedKind {
    fn label(self) -> &'static str {
        match self {
            Self::Liked => "liked",
            Self::Recent => "recently played",
            Self::Playlist => "playlist",
            Self::PlaylistTrack => "playlist track",
        }
    }
}

/// Rows a tolerant load could not understand, counted per category with a few
/// ids/titles kept for the warning.
#[derive(Default)]
struct DroppedRows {
    liked: usize,
    recently_played: usize,
    playlists: usize,
    playlist_tracks: usize,
    labels: Vec<String>,
}

impl DroppedRows {
    fn note(&mut self, kind: DroppedKind, index: usize, row: &Value, error: &str) {
        match kind {
            DroppedKind::Liked => self.liked += 1,
            DroppedKind::Recent => self.recently_played += 1,
            DroppedKind::Playlist => self.playlists += 1,
            DroppedKind::PlaylistTrack => self.playlist_tracks += 1,
        }
        let label = row_label(kind, index, row);
        debug!("library.json: dropping unreadable {label}: {error}");
        if self.labels.len() < MAX_DROPPED_LABELS {
            self.labels.push(label);
        }
    }

    fn total(&self) -> usize {
        self.liked + self.recently_played + self.playlists + self.playlist_tracks
    }

    fn any(&self) -> bool {
        self.total() > 0
    }

    /// Per-kind counts plus the first few row identities: enough for the user
    /// (and a bug report) to find the rows that were dropped, without quoting
    /// the rows themselves.
    fn summary(&self) -> String {
        let mut counts = Vec::new();
        let mut push = |n: usize, what: &str| {
            if n > 0 {
                counts.push(format!("{n} {what}"));
            }
        };
        push(self.liked, "liked");
        push(self.recently_played, "recently played");
        push(self.playlists, "playlist");
        push(self.playlist_tracks, "playlist track");
        let mut summary = counts.join(", ");
        if !self.labels.is_empty() {
            summary.push_str(" — dropped ");
            summary.push_str(&self.labels.join(", "));
            let hidden = self.total() - self.labels.len();
            if hidden > 0 {
                summary.push_str(&format!(", and {hidden} more"));
            }
        }
        summary
    }
}

/// A safe identity for a dropped row: an id, else a title/name, else its
/// position. Never the whole row (it may be huge, or not an object at all).
fn row_label(kind: DroppedKind, index: usize, row: &Value) -> String {
    let text = |key: &str| -> Option<&str> {
        row.get(key)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
    };
    match text("id")
        .or_else(|| text("title"))
        .or_else(|| text("name"))
    {
        Some(identity) => format!("{} #{index} ({identity:?})", kind.label()),
        None => format!("{} #{index}", kind.label()),
    }
}

/// A root field that must be an array when present.
///
/// Missing means "not written by this version", which the additive format
/// treats as empty. Any other type (including `null`, which is not an absent
/// field) is not usable state: returning `Err` keeps the existing whole-file
/// quarantine for a document whose structure is wrong.
fn array_field<'a>(
    root: &'a serde_json::Map<String, Value>,
    field: &str,
) -> Result<Option<&'a Vec<Value>>, String> {
    match root.get(field) {
        None => Ok(None),
        Some(Value::Array(rows)) => Ok(Some(rows)),
        Some(_) => Err(format!("`{field}` is not an array")),
    }
}

/// Deserialize one track row. A row is understood when it is a JSON object whose
/// present fields have the right types **and** it names something (a video id or
/// a title); the serde defaults fill absent fields so an old row still loads.
fn track_from_row(row: &Value) -> Result<Track, String> {
    let track = Track::deserialize(row).map_err(|e| e.to_string())?;
    if track.id.is_empty() && track.title.is_empty() {
        return Err("the row has neither a video id nor a title".into());
    }
    Ok(track)
}

/// Deserialize one playlist row and each of its tracks on its own.
///
/// A playlist's own fields are read here rather than through serde's strict
/// `Playlist` deserializer: absent fields default so an older file still loads,
/// while a present field of the wrong type rejects the playlist. `tracks` is
/// split out and parsed row by row, so one bad track costs that track, not the
/// playlist. A `tracks` container that is not an array is a malformed playlist
/// row and is dropped whole — there is nothing to iterate.
fn playlist_from_row(row: &Value, dropped: &mut DroppedRows) -> Result<Playlist, String> {
    let Some(fields) = row.as_object() else {
        return Err("the playlist row is not a JSON object".into());
    };
    let tracks = match fields.get("tracks") {
        None => None,
        Some(Value::Array(rows)) => Some(rows),
        Some(_) => return Err("the playlist's `tracks` is not an array".into()),
    };
    let text_field = |field: &str| -> Result<String, String> {
        match fields.get(field) {
            None => Ok(String::new()),
            Some(Value::String(value)) => Ok(value.clone()),
            Some(_) => Err(format!("the playlist's `{field}` is not a string")),
        }
    };
    let mut playlist = Playlist {
        id: text_field("id")?,
        name: text_field("name")?,
        tracks: Vec::new(),
    };
    if playlist.id.is_empty() && playlist.name.is_empty() {
        return Err("the playlist has neither an id nor a name".into());
    }
    for (index, row) in tracks.into_iter().flatten().enumerate() {
        match track_from_row(row) {
            Ok(track) => playlist.tracks.push(track),
            Err(error) => dropped.note(DroppedKind::PlaylistTrack, index, row, &error),
        }
    }
    Ok(playlist)
}

/// Parse `library.json` structurally and one row at a time.
///
/// `Err` means the document is not usable state at all (not valid JSON, not a
/// root object, or a root array of the wrong type): the caller must quarantine
/// it. `Ok((library, true))` means the document was usable but at least one row
/// was dropped; every readable row is in `library`, and the caller must save it
/// back after its pre-repair snapshot.
fn parse_library_document(bytes: &[u8]) -> Result<(Library, bool), String> {
    let document: Value = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    let Some(root) = document.as_object() else {
        return Err("the root document is not a JSON object".into());
    };
    let liked = array_field(root, "liked")?;
    let recently_played = array_field(root, "recently_played")?;
    let playlists = array_field(root, "playlists")?;

    let mut library = Library::default();
    let mut dropped = DroppedRows::default();

    if let Some(rows) = liked {
        for (index, row) in rows.iter().enumerate() {
            match track_from_row(row) {
                Ok(track) => library.liked.push(track),
                Err(error) => dropped.note(DroppedKind::Liked, index, row, &error),
            }
        }
    }
    if let Some(rows) = recently_played {
        for (index, row) in rows.iter().enumerate() {
            match track_from_row(row) {
                Ok(track) => library.recently_played.push(track),
                Err(error) => dropped.note(DroppedKind::Recent, index, row, &error),
            }
        }
    }
    if let Some(rows) = playlists {
        for (index, row) in rows.iter().enumerate() {
            match playlist_from_row(row, &mut dropped) {
                Ok(playlist) => library.playlists.push(playlist),
                Err(error) => dropped.note(DroppedKind::Playlist, index, row, &error),
            }
        }
    }

    if dropped.any() {
        warn!(
            "library.json: could not understand {} row(s) and dropped them; kept every readable \
             row ({})",
            dropped.total(),
            dropped.summary()
        );
        Ok((library, true))
    } else {
        Ok((library, false))
    }
}

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
    /// Set when `library.json` existed but could not be read or parsed **and**
    /// could not be moved aside: writing the in-memory defaults would destroy
    /// the user's only copy, so [`save`](Self::save) refuses to write for this
    /// session.
    read_only: bool,
    /// The startup toast for a load that could not preserve the file, held until
    /// the backend's event bus exists — this store is constructed before it.
    load_warning: Option<String>,
    /// The backend's durable writer. Saves are queued to it instead of written
    /// here, so a command called from the UI thread never blocks on disk I/O.
    writer: Arc<DurableWriter>,
}

impl LibraryStore {
    /// Load the library at startup.
    ///
    /// Loading stays **synchronous**: startup must finish before the app can
    /// run, and the repairs below have to be part of the returned state. The
    /// document is parsed row by row ([`parse_library_document`]), so one
    /// unreadable like, playlist or playlist track costs only that row; the
    /// repaired value is written back after the pre-repair snapshot so the good
    /// rows do not disappear on the next launch.
    ///
    /// The one write this can perform (the repair save) is queued to `writer`
    /// like every other save, but [`back_up_before_repair`] is written
    /// synchronously through [`crate::util::atomic_write`] — temp file in the
    /// same directory, `sync_all`, then rename — because it must be durable
    /// before the repair it protects can replace the live file.
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
                read_only: false,
                load_warning: None,
                writer: Arc::clone(writer),
            };
        }
        let (mut data, status) = crate::util::load_json_with_parser(&path, parse_library_document);
        let read_only = !status.may_overwrite();
        let load_warning = status.warning(&path);
        info!("library loaded from {}", path.display());
        let collapsed = collapse_duplicates(&mut data);
        let repaired_rows = matches!(status, LoadStatus::Repaired);
        let store = Self {
            path,
            data,
            ephemeral: false,
            read_only,
            load_warning,
            writer: Arc::clone(writer),
        };
        if collapsed > 0 {
            // Written back straight away, not left for the next change: the
            // duplicate rows are on disk, and a launch that merely reads them
            // again would keep showing the user two rows for one song. The save
            // is queued on the durable writer, so startup does not block on it
            // (the backend's shutdown flush makes it durable).
            warn!("collapsed {collapsed} duplicate song row(s) in the library");
        }
        if collapsed > 0 || repaired_rows {
            // Both repairs replace user data with a subset of it, so the old
            // file is snapshotted first and the repaired save only replaces it
            // after its own temp file is durable (see `atomic_write`).
            store.back_up_before_repair();
            store.save();
        }
        store
    }

    /// Snapshot the library aside before a repair rewrites it.
    ///
    /// The snapshot is written with [`crate::util::atomic_write`]: a temp file
    /// in the library's own directory, flushed with `sync_all`, then renamed to
    /// the backup name. A crash can therefore leave the old snapshot or the new
    /// one, never a half-written copy. The name is numbered via
    /// [`crate::util::free_backup_path`] (`library.json.pre-collapse`,
    /// `.pre-collapse.1`, …), so a second repair cannot destroy the first
    /// snapshot.
    ///
    /// Best effort on purpose: a backup that cannot be written must not stop the
    /// repair the user needs, and that repair's own save is atomic too, so the
    /// live file is never replaced by a partial write. The only loss in that
    /// case is the extra hand-recovery copy.
    ///
    /// This write is deliberately synchronous: it runs once, during startup, and
    /// the repair it protects must not be able to outrun it.
    fn back_up_before_repair(&self) {
        let requested = self.path.with_file_name(PRE_COLLAPSE_BACKUP);
        let backup = crate::util::free_backup_path(&requested);
        let bytes = match std::fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                debug!("no library.json to snapshot before repairing it");
                return;
            }
            Err(e) => {
                warn!("could not read the library before repairing it: {e}");
                return;
            }
        };
        match crate::util::atomic_write(&backup, &bytes) {
            Ok(()) => info!("pre-repair library saved to {}", backup.display()),
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
    ///
    /// Refuses to write when the load could neither read nor preserve
    /// `library.json`: the defaults in memory are not the user's library, and
    /// saving them would overwrite the one copy that is still on disk.
    pub fn save(&self) {
        if self.ephemeral || self.read_only {
            return;
        }
        self.writer
            .write(self.path.clone(), "library", self.data.clone(), |data| {
                serde_json::to_vec_pretty(data).map_err(|e| e.to_string())
            });
    }

    /// The warning for a load that could not preserve `library.json`, or `None`
    /// when the load was clean, repairable, or the file was absent.
    ///
    /// The backend emits this once its event bus exists; it is empty here
    /// because loading runs during construction, before the bus.
    pub(crate) fn load_warning(&self) -> Option<&str> {
        self.load_warning.as_deref()
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

    /// A first launch: no file, defaults in memory, and the first mutation
    /// creates the file. Missing is not an error.
    #[test]
    fn a_missing_library_loads_defaults_and_stays_writable() {
        let dir = crate::util::testing::TempDir::new("library-missing");
        let (mut lib, writer) = temp_store(&dir);
        assert!(lib.data.liked.is_empty());
        assert!(!dir.join("library.json").exists());

        lib.create_playlist("First Mix".into());
        assert!(writer.flush(), "the save must reach disk");

        let reloaded = crate::util::load_json::<Library>(&dir.join("library.json"));
        assert_eq!(reloaded.playlists.len(), 1);
        assert_eq!(reloaded.playlists[0].name, "First Mix");
    }

    /// A readable-but-corrupt file keeps its historical path: quarantine it,
    /// start from defaults, and let the next save write a fresh file.
    #[test]
    fn a_corrupt_library_is_quarantined_and_stays_writable() {
        let dir = crate::util::testing::TempDir::new("library-corrupt");
        let path = dir.join("library.json");
        std::fs::write(&path, b"{ this is not json").expect("seed corrupt file");

        let (mut lib, writer) = temp_store(&dir);
        lib.toggle_like(track("a"));
        assert!(writer.flush(), "the fresh library must be written");

        let backup = path.with_extension("json.bad");
        assert_eq!(
            std::fs::read(&backup).expect("the corrupt original must be kept"),
            b"{ this is not json"
        );
        let reloaded = crate::util::load_json::<Library>(&path);
        assert_eq!(reloaded.liked.len(), 1, "the store stayed writable");
    }

    /// The bug: a file that exists but cannot be read must not be treated as
    /// "missing". When even the quarantine rename fails, the store is read-only
    /// and a mutation + save leaves the original bytes untouched.
    #[cfg(unix)]
    #[test]
    fn an_unreadable_library_that_cannot_be_quarantined_is_never_overwritten() {
        let dir = crate::util::testing::TempDir::new("library-unreadable");
        if crate::util::testing::running_as_root(&dir) {
            return; // permission bits do not stop root
        }
        let path = dir.join("library.json");
        let stored = Library {
            liked: vec![track("kept")],
            ..Library::default()
        };
        crate::util::save_json(&path, &stored, "library");
        let original = std::fs::read(&path).expect("seed bytes");

        let guard = crate::util::testing::UnreadableFile::apply(dir.path(), &path);
        guard.lock_parent_dir();

        let (mut lib, writer) = temp_store(&dir);
        assert!(
            lib.read_only,
            "the load must put the store in read-only mode"
        );
        assert!(
            lib.load_warning()
                .is_some_and(|w| w.contains("changes will not be saved")),
            "the backend needs a startup warning: {:?}",
            lib.load_warning()
        );
        lib.toggle_like(track("new"));
        assert!(
            writer.flush(),
            "nothing is queued, so this drains instantly"
        );

        guard.make_file_readable();
        assert_eq!(
            std::fs::read(&path).expect("the original must still be there"),
            original,
            "an unreadable library that could not be quarantined must not be overwritten"
        );
        assert!(
            !path.with_extension("json.bad").exists(),
            "the failed quarantine must not leave a bogus backup"
        );
    }

    /// Finding 1: one malformed row in `liked`, one malformed playlist and one
    /// malformed playlist track must cost only those rows. Everything readable
    /// is kept, the load is not corruption, and the repaired subset is written
    /// back (after the pre-repair snapshot) so the good rows survive the next
    /// launch.
    #[test]
    fn a_partially_unreadable_library_keeps_every_readable_row() {
        let dir = crate::util::testing::TempDir::new("library-tolerant");
        let path = dir.join("library.json");
        let document = serde_json::json!({
            "liked": [
                track("liked-1"),
                {"id": 42, "title": "wrong id type"},
                track("liked-2"),
            ],
            "recently_played": [
                track("recent-1"),
                {"id": null, "title": "wrong id value"},
            ],
            "playlists": [
                {
                    "id": "pl-1",
                    "name": "Mix",
                    "tracks": [
                        track("track-1"),
                        {"title": 17},
                        track("track-2"),
                    ],
                },
                {"id": 9, "name": "wrong field type"},
                "not a playlist",
            ],
        });
        let original = serde_json::to_vec_pretty(&document).expect("serialize document");
        std::fs::write(&path, &original).expect("seed partially corrupt library");

        let (lib, writer) = temp_store(&dir);

        assert_eq!(
            lib.data
                .liked
                .iter()
                .map(|t| t.id.as_str())
                .collect::<Vec<_>>(),
            vec!["liked-1", "liked-2"],
            "the readable likes must all be kept"
        );
        assert_eq!(
            lib.data
                .recently_played
                .iter()
                .map(|t| t.id.as_str())
                .collect::<Vec<_>>(),
            vec!["recent-1"]
        );
        assert_eq!(lib.data.playlists.len(), 1, "only the readable playlist");
        assert_eq!(lib.data.playlists[0].id, "pl-1");
        assert_eq!(
            lib.data.playlists[0]
                .tracks
                .iter()
                .map(|t| t.id.as_str())
                .collect::<Vec<_>>(),
            vec!["track-1", "track-2"],
            "the playlist's readable tracks must survive their bad sibling"
        );
        assert!(
            lib.load_warning().is_none(),
            "a repairable file is a successful load, not a startup error"
        );
        assert!(
            !path.with_extension("json.bad").exists(),
            "a repairable file must not be quarantined"
        );
        assert!(
            !lib.read_only,
            "dropping rows must leave the library writable"
        );

        assert!(writer.flush(), "the repaired library must be saved");
        let reloaded: Library = crate::util::load_json(&path);
        assert_eq!(
            reloaded, lib.data,
            "the repaired file must round-trip the kept rows"
        );
        assert_eq!(
            std::fs::read(dir.join(super::PRE_COLLAPSE_BACKUP)).expect("pre-repair snapshot"),
            original,
            "the original document must be recoverable before the repair replaces it"
        );

        // A reload of the repaired file is a clean load: nothing left to drop,
        // so it must not queue another repair or make a second snapshot.
        let (clean, clean_writer) = temp_store(&dir);
        assert_eq!(clean.data, lib.data);
        assert!(clean_writer.flush());
        assert!(
            !dir.join(&format!("{}.1", super::PRE_COLLAPSE_BACKUP))
                .exists(),
            "a clean reload must not create another pre-repair snapshot"
        );
    }

    /// The structure that never becomes repairable keeps the historical whole-
    /// file quarantine: a root that is not an object, or a root array of the
    /// wrong type.
    #[test]
    fn a_library_whose_root_or_arrays_are_unusable_is_still_quarantined() {
        for (tag, bytes) in [
            ("root-array", br#"[]"#.as_slice()),
            ("root-string", br#""not a library""#.as_slice()),
            ("liked-object", br#"{"liked": {}}"#.as_slice()),
            (
                "playlists-string",
                br#"{"liked": [], "playlists": "nope"}"#.as_slice(),
            ),
        ] {
            let dir = crate::util::testing::TempDir::new(&format!("library-corrupt-{tag}"));
            let path = dir.join("library.json");
            std::fs::write(&path, bytes).expect("seed unusable document");

            let (lib, _writer) = temp_store(&dir);

            assert_eq!(
                lib.data,
                Library::default(),
                "{tag}: unusable state must start from defaults"
            );
            assert!(
                !path.exists(),
                "{tag}: the unusable file must be moved aside"
            );
            assert!(
                path.with_extension("json.bad").is_file(),
                "{tag}: the unusable file must survive as a backup"
            );
            assert!(
                !dir.join(super::PRE_COLLAPSE_BACKUP).exists(),
                "{tag}: there is no repaired state to snapshot"
            );
        }
    }

    /// A fully valid file is left byte-for-byte alone: no quarantine, no
    /// snapshot, no rewrite of rows that did not need one.
    #[test]
    fn a_fully_valid_library_is_loaded_without_rewriting_the_file() {
        let dir = crate::util::testing::TempDir::new("library-valid-unchanged");
        let path = dir.join("library.json");
        let stored = Library {
            liked: vec![track("liked")],
            recently_played: vec![track("recent")],
            playlists: vec![Playlist {
                id: "pl-1".into(),
                name: "Mix".into(),
                tracks: vec![track("playlist-track")],
            }],
        };
        crate::util::save_json(&path, &stored, "library");
        let before = std::fs::read(&path).expect("seed bytes");

        let (lib, writer) = temp_store(&dir);

        assert_eq!(lib.data, stored);
        assert!(
            writer.flush(),
            "nothing is queued, so this drains instantly"
        );
        assert_eq!(
            std::fs::read(&path).expect("read"),
            before,
            "a valid library must not be rewritten"
        );
        assert!(!path.with_extension("json.bad").exists());
        assert!(!dir.join(super::PRE_COLLAPSE_BACKUP).exists());
    }

    /// Finding 3: a second repair must not overwrite the first pre-repair
    /// snapshot. Both are numbered siblings of `library.json`, and the newer one
    /// holds the content that was live when the second repair ran.
    #[test]
    fn a_second_repair_keeps_both_pre_repair_snapshots() {
        let dir = crate::util::testing::TempDir::new("library-repair-backup-twice");
        let path = dir.join("library.json");
        crate::util::save_json(
            &path,
            &Library {
                liked: vec![album_row(), artist_row()],
                ..Library::default()
            },
            "library",
        );
        let first_bytes = std::fs::read(&path).expect("first seed");

        let (first, writer) = temp_store(&dir);
        assert_eq!(first.data.liked.len(), 1);
        assert!(writer.flush());

        crate::util::save_json(
            &path,
            &Library {
                liked: vec![artist_row(), album_row()],
                ..Library::default()
            },
            "library",
        );
        let second_bytes = std::fs::read(&path).expect("second seed");

        let (second, writer) = temp_store(&dir);
        assert_eq!(second.data.liked.len(), 1);
        assert!(writer.flush());

        let first_backup = dir.join(super::PRE_COLLAPSE_BACKUP);
        let second_backup = dir.join(&format!("{}.1", super::PRE_COLLAPSE_BACKUP));
        assert_eq!(
            std::fs::read(&first_backup).expect("first snapshot"),
            first_bytes,
            "the first repair's snapshot must stay intact"
        );
        assert_eq!(
            std::fs::read(&second_backup).expect("second snapshot"),
            second_bytes,
            "the second snapshot must be the content that was live before the second repair"
        );

        let current: Library = crate::util::load_json(&path);
        assert_eq!(
            current.liked.len(),
            1,
            "the live file must be the repaired subset, not either snapshot"
        );
    }

    /// Finding 2: parse corruption whose quarantine rename fails is the same
    /// safety problem as an unreadable file whose rename fails. The store must be
    /// read-only and leave the corrupt original untouched.
    #[cfg(unix)]
    #[test]
    fn a_corrupt_library_that_cannot_be_quarantined_is_never_overwritten() {
        let dir = crate::util::testing::TempDir::new("library-corrupt-unpreserved");
        if crate::util::testing::running_as_root(&dir) {
            return; // permission bits do not stop root
        }
        let path = dir.join("library.json");
        let original = b"{ this is not json";
        std::fs::write(&path, original).expect("seed corrupt file");

        let _guard = crate::util::testing::ReadOnlyDir::apply(dir.path());
        let (mut lib, writer) = temp_store(&dir);
        assert!(
            lib.read_only,
            "a corrupt file that could not be moved aside must make the store read-only"
        );
        assert!(
            lib.load_warning().is_some_and(|w| {
                w.contains("corrupt file could not be preserved")
                    && w.contains("changes will not be saved")
            }),
            "the backend needs a warning naming the unpreserved corruption: {:?}",
            lib.load_warning()
        );
        lib.toggle_like(track("new"));
        assert!(
            writer.flush(),
            "nothing is queued, so this drains instantly"
        );

        assert_eq!(
            std::fs::read(&path).expect("the original must still be there"),
            original,
            "an unpreserved corrupt library must not be overwritten"
        );
        assert!(
            !path.with_extension("json.bad").exists(),
            "the failed quarantine must not leave a bogus backup"
        );
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
