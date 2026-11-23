//! Private async-command completions and their mapping to [`ControllerEvent`]s.

use rift_types::{
    AlbumPage, AlbumSummary, ArtistPage, ArtistSummary, Playlist, Track, UpdateStatus,
};

use super::{Controller, ControllerEvent};
use crate::cover::CoverImage;

// ---------------------------------------------------------------------------
// Async completions (controller-internal)
// ---------------------------------------------------------------------------

/// The completion half of an `async` command.
///
/// Kept private: `app.rs` only ever sees the mapped [`ControllerEvent`] values,
/// after [`Controller::poll`] has applied the stale-generation and cover-dedupe
/// bookkeeping. The controller's unit tests can inject these directly through
/// the recording double to drive the filters deterministically.
///
/// Only the `backend`-featured live implementation (and the tests) construct
/// these; a plain no-feature preview build legitimately has none constructed, and
/// the unit-test build constructs only the variants those tests drive.
#[derive(Debug)]
#[allow(dead_code)]
pub(super) enum TaskResult {
    PlayTracks(Result<(), String>),
    PlayTrack(Result<(), String>),
    SearchSongs {
        generation: u64,
        query: String,
        results: Result<Vec<Track>, String>,
    },
    SearchArtists {
        generation: u64,
        query: String,
        results: Result<Vec<ArtistSummary>, String>,
    },
    SearchAlbums {
        generation: u64,
        query: String,
        results: Result<Vec<AlbumSummary>, String>,
    },
    ArtistPage {
        generation: u64,
        artist_id: String,
        result: Result<ArtistPage, String>,
    },
    ArtistSongs {
        generation: u64,
        artist_id: String,
        result: Result<Vec<Track>, String>,
    },
    AlbumPage {
        generation: u64,
        album_id: String,
        result: Result<AlbumPage, String>,
    },
    ImportYtPlaylist(Result<Playlist, String>),
    CheckUpdate(Result<UpdateStatus, String>),
    Online(bool),
    Cover {
        url: String,
        result: Result<CoverImage, String>,
    },
}

impl Controller {
    // -----------------------------------------------------------------
    // Async completions
    // -----------------------------------------------------------------

    pub(super) fn apply_task(&mut self, task: TaskResult, events: &mut Vec<ControllerEvent>) {
        match task {
            TaskResult::PlayTracks(result) => {
                if let Err(message) = result {
                    events.push(ControllerEvent::Error(message));
                }
            }
            TaskResult::PlayTrack(result) => {
                if let Err(message) = result {
                    events.push(ControllerEvent::Error(message));
                }
            }
            TaskResult::SearchSongs {
                generation,
                query,
                results,
            } => {
                // Stale-search guard: a reply for a query that has since been
                // replaced (or cleared) never reaches the UI.
                if generation != self.search_generation {
                    return;
                }
                if let Ok(tracks) = &results {
                    self.tracks.insert_all(tracks);
                }
                events.push(ControllerEvent::SearchResults { query, results });
            }
            TaskResult::SearchArtists {
                generation,
                query,
                results,
            } => {
                if generation != self.search_generation {
                    return;
                }
                events.push(ControllerEvent::SearchArtists { query, results });
            }
            TaskResult::SearchAlbums {
                generation,
                query,
                results,
            } => {
                if generation != self.search_generation {
                    return;
                }
                events.push(ControllerEvent::SearchAlbums { query, results });
            }
            TaskResult::ArtistPage {
                generation,
                artist_id,
                result,
            } => {
                if generation != self.page_generation {
                    return;
                }
                self.pending_page = None;
                if let Ok(page) = &result {
                    self.tracks.insert_all(&page.tracks);
                }
                events.push(ControllerEvent::ArtistPage {
                    generation,
                    artist_id,
                    result,
                });
            }
            TaskResult::ArtistSongs {
                generation,
                artist_id,
                result,
            } => {
                if generation != self.page_generation {
                    return;
                }
                self.pending_page = None;
                if let Ok(tracks) = &result {
                    self.tracks.insert_all(tracks);
                }
                events.push(ControllerEvent::ArtistSongs {
                    generation,
                    artist_id,
                    result,
                });
            }
            TaskResult::AlbumPage {
                generation,
                album_id,
                result,
            } => {
                if generation != self.page_generation {
                    return;
                }
                self.pending_page = None;
                if let Ok(page) = &result {
                    self.tracks.insert_all(&page.tracks);
                }
                events.push(ControllerEvent::AlbumPage {
                    generation,
                    album_id,
                    result,
                });
            }
            TaskResult::ImportYtPlaylist(result) => match result {
                Ok(playlist) => {
                    self.tracks.insert_all(&playlist.tracks);
                    // The backend emits `Library` itself; the old frontend then
                    // navigated to the new playlist and toasted the count.
                    let count = playlist.tracks.len();
                    events.push(ControllerEvent::OpenPlaylist(playlist.id.clone()));
                    events.push(ControllerEvent::Notice(format!(
                        "Imported \u{201c}{}\u{201d} ({count} song{})",
                        playlist.name,
                        if count == 1 { "" } else { "s" }
                    )));
                }
                Err(message) => events.push(ControllerEvent::Error(message)),
            },
            TaskResult::CheckUpdate(result) => match result {
                Ok(status) => events.push(ControllerEvent::UpdateStatus(status)),
                Err(message) => events.push(ControllerEvent::Error(message)),
            },
            TaskResult::Online(online) => {
                self.probe_in_flight = false;
                if self.online != Some(online) {
                    self.online = Some(online);
                    events.push(ControllerEvent::Online(online));
                }
            }
            TaskResult::Cover { url, result } => {
                self.covers.finish(&url, result.is_ok());
                match result {
                    Ok(image) => events.push(ControllerEvent::Cover(image)),
                    Err(_) => events.push(ControllerEvent::CoverFailed(url)),
                }
            }
        }
    }
}
