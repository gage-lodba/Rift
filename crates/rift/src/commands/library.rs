//! Library and playlist commands: likes, playlist CRUD, reorder, and the
//! YouTube-Music / JSON import & export flows.
//!
//! **Persistence is off-thread.** Every synchronous mutation below takes the
//! library lock, updates the store, and returns; the JSON save is queued on the
//! backend's durable writer (`util::DurableWriter`) and serialized/atomically
//! written by its background thread. A command dispatched from the UI/controller
//! thread therefore never waits on `library.json` I/O, and the backend flushes the
//! queue on shutdown so the last change still survives a quit.
//!
//! The two filesystem-dialog commands ([`export_playlist`](BackendHandle::export_playlist)
//! and [`import_playlist`](BackendHandle::import_playlist)) use [`rfd`] instead
//! of the old shell's dialog plugin and return immediately: the native dialog
//! and the file I/O run on the backend's blocking pool, and completion is
//! reported through the event bus (`Notice` / `Error` / `OpenPlaylist` /
//! `Library`).

use rift_types::{Library, Playlist, Track};
use tracing::{info, warn};

use super::convert::{is_audio_track, MAX_FETCH_TRACKS};
use super::downloads::start_downloads;
use crate::backend::BackendHandle;
use crate::event::BackendEvent;
use crate::util::LockExt;

impl BackendHandle {
    /// Broadcast the persisted library after a mutation.
    fn emit_library(&self, library: &Library) {
        self.inner.emit(BackendEvent::Library(library.clone()));
    }

    /// Toggle a track in the user's likes.
    pub fn toggle_like(&self, track: Track) {
        let mut lib = self.inner.library.lock_safe();
        lib.toggle_like(track);
        drop(lib);
        let library = self.inner.library.lock_safe().data.clone();
        self.emit_library(&library);
    }

    /// Create an empty playlist. Errors on a blank (or whitespace-only) name.
    pub fn create_playlist(&self, name: String) -> Result<Playlist, String> {
        let name = name.trim();
        if name.is_empty() {
            return Err("playlist name cannot be empty".into());
        }
        let mut lib = self.inner.library.lock_safe();
        let playlist = lib.create_playlist(name.to_string());
        let library = lib.data.clone();
        drop(lib);
        self.emit_library(&library);
        Ok(playlist)
    }

    /// Import a `YouTube` Music (or `YouTube`) playlist by URL or ID into the
    /// library as a new local playlist. Audio-only tracks are kept (videos and
    /// episodes dropped).
    ///
    /// Network I/O: run the future from a spawned task.
    ///
    /// After the fetch, the only work left on the async worker is the in-memory
    /// fold + insert (which queues the save on the durable writer) — there is no
    /// `std::fs` call on this path, so a large import never blocks a runtime
    /// worker on `library.json` I/O.
    pub async fn import_yt_playlist(&self, url: String) -> Result<Playlist, String> {
        let id = parse_playlist_id(&url)
            .ok_or_else(|| "Couldn't find a playlist ID in that link.".to_string())?;
        info!("importing playlist {id}");

        let pl = self
            .inner
            .player
            .yt
            .playlist(&id, MAX_FETCH_TRACKS)
            .await
            .map_err(|e| format!("could not load playlist: {e}"))?;

        let Playlist {
            name, mut tracks, ..
        } = pl;
        tracks.retain(is_audio_track);
        if tracks.is_empty() {
            return Err("That playlist has no playable songs.".into());
        }

        let name = if name.trim().is_empty() {
            "Imported playlist".to_string()
        } else {
            name
        };
        let playlist = {
            let mut lib = self.inner.library.lock_safe();
            let playlist = lib.create_playlist_with(name, tracks);
            let library = lib.data.clone();
            drop(lib);
            self.emit_library(&library);
            playlist
        };
        Ok(playlist)
    }

    /// Export a playlist to a JSON file the user picks via a native Save dialog.
    /// The file is Rift's own `Playlist` JSON, re-importable losslessly.
    ///
    /// **Non-blocking, fire-and-forget.** The dialog and write happen on the
    /// backend's blocking pool; a successful save emits
    /// [`BackendEvent::Notice`], a failure emits [`BackendEvent::Error`], and a
    /// cancelled dialog emits nothing. Safe to call from the UI thread, though
    /// it is still a command: dispatch it from the controller like the rest.
    pub fn export_playlist(&self, id: String) {
        // Snapshot and serialize under the lock, before showing the dialog.
        let (file_name, json) = {
            let lib = self.inner.library.lock_safe();
            let Some(p) = lib.data.playlists.iter().find(|p| p.id == id) else {
                return;
            };
            match serde_json::to_vec_pretty(p) {
                Ok(json) => (format!("{}.json", safe_filename(&p.name)), json),
                Err(e) => {
                    warn!("could not serialize playlist {id}: {e}");
                    return;
                }
            }
        };

        let backend = self.inner.clone();
        self.inner.spawn_blocking(move || {
            let Some(path) = rfd::FileDialog::new()
                .add_filter("Rift playlist", &["json"])
                .set_file_name(file_name)
                .save_file()
            else {
                return; // cancelled
            };
            match std::fs::write(&path, &json) {
                Ok(()) => backend.emit(BackendEvent::Notice(format!(
                    "Exported to {}",
                    path.display()
                ))),
                Err(e) => backend.emit(BackendEvent::Error(format!(
                    "Could not export playlist: {e}"
                ))),
            }
        });
    }

    /// Import a playlist from a Rift JSON file the user picks via a native Open
    /// dialog. Always created as a new playlist (fresh id), never overwriting.
    ///
    /// **Non-blocking, fire-and-forget.** Reports completion through the event
    /// bus: `Library` + `OpenPlaylist` + `Notice` on success, `Error` on a bad or
    /// unreadable file, nothing when cancelled.
    pub fn import_playlist(&self) {
        let backend = self.inner.clone();
        self.inner.spawn_blocking(move || {
            let Some(path) = rfd::FileDialog::new()
                .add_filter("Rift playlist", &["json"])
                .pick_file()
            else {
                return; // cancelled
            };
            let parsed = match std::fs::read(&path)
                .map_err(|_| "That isn't a valid Rift playlist file.".to_owned())
                .and_then(|body| import_playlist_from(&body))
            {
                Ok(p) => p,
                Err(msg) => {
                    backend.emit(BackendEvent::Error(msg));
                    return;
                }
            };
            let (library, id, name, count) = {
                let mut lib = backend.library.lock_safe();
                let p = lib.create_playlist_with(parsed.name, parsed.tracks);
                let count = p.tracks.len();
                (lib.data.clone(), p.id, p.name, count)
            };
            backend.emit(BackendEvent::Library(library));
            // Jump the UI to the freshly imported playlist.
            backend.emit(BackendEvent::OpenPlaylist(id));
            backend.emit(BackendEvent::Notice(format!(
                "Imported \u{201c}{name}\u{201d} ({count} song{})",
                if count == 1 { "" } else { "s" }
            )));
        });
    }

    /// Delete a playlist by id.
    pub fn delete_playlist(&self, id: String) {
        let mut lib = self.inner.library.lock_safe();
        lib.delete_playlist(&id);
        let library = lib.data.clone();
        drop(lib);
        self.emit_library(&library);
    }

    /// Rename a playlist.
    pub fn rename_playlist(&self, id: String, name: String) {
        let mut lib = self.inner.library.lock_safe();
        lib.rename_playlist(&id, name);
        let library = lib.data.clone();
        drop(lib);
        self.emit_library(&library);
    }

    /// Move a playlist to position `to` in the sidebar order.
    pub fn move_playlist(&self, id: String, to: usize) {
        let mut lib = self.inner.library.lock_safe();
        let moved = lib.move_playlist(&id, to);
        let library = lib.data.clone();
        drop(lib);
        if moved {
            self.emit_library(&library);
        }
    }

    /// Add a track to a playlist.
    ///
    /// A playlist whose every track is already downloaded is treated as "kept
    /// offline": adding a new track pulls it down too, so the playlist stays
    /// fully available offline.
    pub fn add_to_playlist(&self, id: String, track: Track) {
        // A playlist whose every track is already downloaded is treated as "kept
        // offline": adding a new track should pull it down too, so the playlist
        // stays fully available offline. Evaluated before the add, and only for a
        // non-empty playlist that doesn't already contain this track.
        //
        // "Contains this track" is asked by song, exactly as `add_to_playlist`
        // asks it: a playlist that already holds this recording under the other
        // surface's video id will refuse the add, and downloading a row that was
        // never added would leave the offline copy dangling outside the playlist.
        let keep_offline = {
            let lib = self.inner.library.lock_safe();
            lib.data
                .playlists
                .iter()
                .find(|p| p.id == id)
                .is_some_and(|p| {
                    !p.tracks.iter().any(|t| t.same_song(&track))
                        && super::downloads::playlist_fully_downloaded(p, |tid| {
                            self.inner.downloads.is_downloaded(tid)
                        })
                })
        };

        {
            let mut lib = self.inner.library.lock_safe();
            lib.add_to_playlist(&id, track.clone());
            let library = lib.data.clone();
            drop(lib);
            self.emit_library(&library);
        }

        if keep_offline {
            start_downloads(&self.inner, vec![track]);
        }
    }

    /// Remove a track (by id) from a playlist.
    pub fn remove_from_playlist(&self, id: String, track_id: String) {
        let mut lib = self.inner.library.lock_safe();
        lib.remove_from_playlist(&id, &track_id);
        let library = lib.data.clone();
        drop(lib);
        self.emit_library(&library);
    }
}

/// Extract a YouTube/YouTube Music playlist ID from a pasted URL, or accept a
/// bare ID. Returns `None` if nothing usable is found.
fn parse_playlist_id(input: &str) -> Option<String> {
    let input = input.trim();
    if input.is_empty() {
        return None;
    }
    // URL with a `list=` query parameter (music.youtube.com / youtube.com).
    if let Some(rest) = input.split("list=").nth(1) {
        let id: String = rest
            .chars()
            .take_while(|c| *c != '&' && *c != '#')
            .collect();
        if !id.is_empty() {
            return Some(id);
        }
    }
    // Otherwise treat the whole thing as a bare ID, but only if it looks like
    // one (no scheme/spaces) so a stray URL without `list=` doesn't slip through.
    if !input.contains("://") && !input.contains(char::is_whitespace) {
        return Some(input.to_string());
    }
    None
}

/// Make a string safe to use as a filename: keep word chars, space, dash; drop
/// the rest. Avoids path separators and other awkward characters.
fn safe_filename(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == ' ' || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        "playlist".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Parse a Rift playlist document the user picked.
///
/// Split out of the dialog callback so the contract is behavioural rather than a
/// comment: this path is a **lossless** round trip of whatever
/// [`export_playlist`](BackendHandle::export_playlist) wrote, and deliberately
/// applies no audio/UGC filter. The innertube import applies one because it is
/// building a playlist from search results, which mix in music videos and
/// episodes; a filter here would silently drop rows the file contained, and the
/// file is the only thing the user has to go on. The store's dedup is the sole
/// transformation after this: it folds rows `Track::same_song` calls one song — a
/// title match plus agreement on whichever of duration, album and credit both
/// rows carry — never a title on its own.
pub(crate) fn import_playlist_from(body: &[u8]) -> Result<Playlist, String> {
    serde_json::from_slice(body).map_err(|_| "That isn't a valid Rift playlist file.".to_owned())
}

#[cfg(test)]
mod tests {
    use super::{import_playlist_from, parse_playlist_id, safe_filename};
    use rift_types::{Playlist, Track};

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

    /// The JSON import is a lossless round trip, and that is a behavioural
    /// contract rather than a comment asking the next maintainer to be careful.
    /// A filter added here (the tempting "copy what the innertube import does")
    /// would silently drop rows the file contained: the export wrote them, the
    /// file is the user's only copy, and nothing would report the loss.
    #[test]
    fn importing_a_playlist_keeps_every_row_including_a_video_titled_one() {
        let exported = Playlist {
            id: "pl-1".into(),
            name: "Mix".into(),
            tracks: vec![
                track("a", "Ordinary Song"),
                track("b", "Some Song (Official Video)"),
            ],
        };
        // Exactly what `export_playlist` writes.
        let body = serde_json::to_vec(&exported).expect("serialize the export");

        let imported = import_playlist_from(&body).expect("a well-formed Rift playlist imports");

        assert_eq!(
            imported, exported,
            "the imported playlist must equal the exported one"
        );
        assert!(
            imported
                .tracks
                .iter()
                .any(|t| t.title.contains("Official Video")),
            "the video-titled row has to survive: {imported:?}"
        );
    }

    #[test]
    fn a_file_that_is_not_a_playlist_reports_why() {
        assert!(import_playlist_from(b"{ not json").is_err());
        // Valid JSON, wrong shape — still refused rather than half-imported.
        assert!(import_playlist_from(b"{\"name\": \"Mix\"}").is_err());
    }

    #[test]
    fn parses_playlist_ids_from_urls_and_bare_ids() {
        assert_eq!(
            parse_playlist_id("https://music.youtube.com/playlist?list=PLabc123"),
            Some("PLabc123".into())
        );
        assert_eq!(
            parse_playlist_id("https://www.youtube.com/playlist?list=OLAK5uy_x&si=foo"),
            Some("OLAK5uy_x".into()),
            "stops at the next query param"
        );
        assert_eq!(parse_playlist_id("  PLbareId  "), Some("PLbareId".into()));
        assert_eq!(parse_playlist_id(""), None);
        assert_eq!(
            parse_playlist_id("https://youtube.com/watch?v=abc"),
            None,
            "a non-playlist URL yields nothing"
        );
    }

    /// The export filename is derived from the playlist name; path separators
    /// and reserved characters must not survive into it.
    #[test]
    fn export_filenames_are_stripped_to_a_single_safe_component() {
        assert_eq!(safe_filename("Road Trip"), "Road Trip");
        assert_eq!(safe_filename("a/b\\c:d"), "a_b_c_d");
        assert_eq!(safe_filename("   "), "playlist");
        assert!(!safe_filename("../evil").contains('/'));
        assert!(!safe_filename("../evil").contains('\\'));
    }
}
