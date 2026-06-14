//! JSON-persisted user library: liked songs, playlists, recently played.

use std::path::PathBuf;

use rift_types::{Library, Playlist, Track};
use tracing::{info, warn};

const RECENT_CAP: usize = 30;

pub struct LibraryStore {
    path: PathBuf,
    pub data: Library,
}

impl LibraryStore {
    pub fn load(dir: &std::path::Path) -> Self {
        let path = dir.join("library.json");
        let data = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| match serde_json::from_str(&s) {
                Ok(lib) => Some(lib),
                Err(e) => {
                    warn!("could not parse {}: {e}", path.display());
                    None
                }
            })
            .unwrap_or_default();
        info!("library loaded from {}", path.display());
        Self { path, data }
    }

    pub fn save(&self) {
        match serde_json::to_string_pretty(&self.data) {
            Ok(json) => {
                if let Err(e) = std::fs::write(&self.path, json) {
                    warn!("failed to save library: {e}");
                }
            }
            Err(e) => warn!("failed to serialize library: {e}"),
        }
    }

    pub fn toggle_like(&mut self, track: Track) {
        if let Some(pos) = self.data.liked.iter().position(|t| t.id == track.id) {
            self.data.liked.remove(pos);
        } else {
            self.data.liked.insert(0, track);
        }
        self.save();
    }

    pub fn push_recent(&mut self, track: Track) {
        self.data.recently_played.retain(|t| t.id != track.id);
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
        let apply = |t: &mut Track| {
            if t.id == src.id && t.artists.is_empty() {
                t.artists = src.artists.clone();
                t.artist = src.artist.clone();
                if t.album_id.is_none() {
                    t.album_id = src.album_id.clone();
                }
                if t.album.is_none() {
                    t.album = src.album.clone();
                }
                true
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
        let playlist = Playlist {
            id: format!("pl-{:016x}", rand::random::<u64>()),
            name,
            tracks: Vec::new(),
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

    pub fn add_to_playlist(&mut self, id: &str, track: Track) {
        if let Some(p) = self.data.playlists.iter_mut().find(|p| p.id == id) {
            if !p.tracks.iter().any(|t| t.id == track.id) {
                p.tracks.push(track);
                self.save();
            }
        }
    }

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

    fn temp_store() -> LibraryStore {
        let dir = std::env::temp_dir().join(format!("rift-lib-{:016x}", rand::random::<u64>()));
        std::fs::create_dir_all(&dir).unwrap();
        LibraryStore::load(&dir)
    }

    #[test]
    fn like_toggles_and_persists_across_reload() {
        let mut lib = temp_store();
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
        let mut lib = temp_store();
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
        let mut lib = temp_store();
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
    fn backfill_copies_credits_onto_creditless_copies() {
        let mut lib = temp_store();
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
}
