//! The backend command seam: the [`BackendCommands`] trait and its inert/live implementations.

use rift_types::{Bootstrap, Track};

// ---------------------------------------------------------------------------
// Command seam
// ---------------------------------------------------------------------------

/// Every backend command the UI can trigger, as a trait so the mapping is
/// testable without a live backend.
///
/// The live implementation (`live`) wraps `rift::BackendHandle`; tests inject a
/// recording double. Methods fall into three groups:
///
/// * **sync** — call-through to a backend method that takes short locks and does
///   no network I/O; safe on the UI thread.
/// * **async** — spawned on the backend runtime; the result (or its absence) is
///   reported later through the controller-owned completion channel as one of
///   the `TaskResult` values, which [`Controller::poll`](super::Controller::poll)
///   turns into [`ControllerEvent`](super::ControllerEvent)s after applying
///   stale-generation filters.
/// * **fallible sync** — returns `Err(String)` for the controller to surface as
///   [`ControllerEvent::Error`](super::ControllerEvent::Error) (e.g. a blank
///   playlist name, a refused URL).
///
/// The parameter names mirror the backend API so the mapping table in
/// `docs/controller-notes.md` reads 1:1.
pub trait BackendCommands: Send + Sync + 'static {
    // ------------------------------------------------------------- startup
    /// Synchronous launch snapshot (`backend.bootstrap()`).
    ///
    /// # Errors
    ///
    /// Implementations return the backend's error message when the launch
    /// snapshot cannot be read; the controller surfaces it as a startup toast.
    fn bootstrap(&self) -> Result<Bootstrap, String>;

    // ------------------------------------------------------------ playback
    fn play_tracks(&self, tracks: Vec<Track>, start: usize, source: Option<String>);
    fn play_track(&self, track: Track);
    fn toggle_play(&self);
    fn next_track(&self);
    fn prev_track(&self);
    fn seek(&self, position: f64);
    fn set_volume(&self, volume: f32);
    fn save_volume(&self, volume: f32);
    fn set_discord_rpc(&self, enabled: bool);
    fn set_crossfade(&self, secs: f32);
    fn toggle_shuffle(&self);
    fn cycle_repeat(&self);

    // --------------------------------------------------------------- queue
    fn queue_add(&self, track: Track);
    fn queue_play_next(&self, tracks: Vec<Track>);
    fn queue_move(&self, from: usize, to: usize);
    fn queue_remove(&self, index: usize);
    fn queue_jump(&self, index: usize);
    fn queue_clear(&self);

    // ------------------------------------------------------------- library
    fn toggle_like(&self, track: Track);
    /// Create a playlist.
    ///
    /// # Errors
    ///
    /// Returns the backend's validation message (e.g. a blank name) so the
    /// controller can toast it instead of silently dropping the action.
    fn create_playlist(&self, name: String) -> Result<(), String>;
    fn import_yt_playlist(&self, url: String);
    fn export_playlist(&self, id: String);
    fn import_playlist(&self);
    fn delete_playlist(&self, id: String);
    fn rename_playlist(&self, id: String, name: String);
    fn move_playlist(&self, id: String, to: usize);
    fn add_to_playlist(&self, id: String, track: Track);
    fn remove_from_playlist(&self, id: String, track_id: String);

    // ------------------------------------------------------------ downloads
    fn download_tracks(&self, tracks: Vec<Track>);
    fn remove_downloads(&self, ids: Vec<String>);

    // ------------------------------------------------------------- updates
    fn check_update(&self);
    fn set_update_notifications(&self, enabled: bool);
    /// Open an external URL in the OS browser.
    ///
    /// # Errors
    ///
    /// Returns the backend's refusal message for a non-`http(s)` URL (or a
    /// failed native open), which the controller toasts.
    fn open_url(&self, url: String) -> Result<(), String>;

    // ------------------------------------------------------ search/catalog
    fn search_songs(&self, generation: u64, query: String);
    fn search_artists(&self, generation: u64, query: String);
    fn search_albums(&self, generation: u64, query: String);
    fn load_artist_page(&self, generation: u64, id: String);
    fn load_artist_songs(&self, generation: u64, id: String);
    fn load_album_page(&self, generation: u64, id: String);

    // --------------------------------------------------------------- images
    /// Fetch one cover URL and report a decoded
    /// [`TaskResult::Cover`](super::tasks::TaskResult::Cover) (or its error) through
    /// the controller channel.
    fn fetch_cover(&self, url: String);

    // ------------------------------------------------------- connectivity
    /// Run one cheap reachability probe and report
    /// [`TaskResult::Online`](super::tasks::TaskResult::Online).
    fn check_online(&self);
}

/// The inert implementation: preview/screenshot mode, and the fallback when the
/// app is built without the `backend` feature. Every command is accepted and
/// ignored; no thread, runtime or network client exists.
#[derive(Debug, Default)]
pub(super) struct InertCommands;

impl BackendCommands for InertCommands {
    fn bootstrap(&self) -> Result<Bootstrap, String> {
        Err("no backend attached".into())
    }

    fn play_tracks(&self, _: Vec<Track>, _: usize, _: Option<String>) {}
    fn play_track(&self, _: Track) {}
    fn toggle_play(&self) {}
    fn next_track(&self) {}
    fn prev_track(&self) {}
    fn seek(&self, _: f64) {}
    fn set_volume(&self, _: f32) {}
    fn save_volume(&self, _: f32) {}
    fn set_discord_rpc(&self, _: bool) {}
    fn set_crossfade(&self, _: f32) {}
    fn toggle_shuffle(&self) {}
    fn cycle_repeat(&self) {}

    fn queue_add(&self, _: Track) {}
    fn queue_play_next(&self, _: Vec<Track>) {}
    fn queue_move(&self, _: usize, _: usize) {}
    fn queue_remove(&self, _: usize) {}
    fn queue_jump(&self, _: usize) {}
    fn queue_clear(&self) {}

    fn toggle_like(&self, _: Track) {}
    fn create_playlist(&self, _: String) -> Result<(), String> {
        Ok(())
    }
    fn import_yt_playlist(&self, _: String) {}
    fn export_playlist(&self, _: String) {}
    fn import_playlist(&self) {}
    fn delete_playlist(&self, _: String) {}
    fn rename_playlist(&self, _: String, _: String) {}
    fn move_playlist(&self, _: String, _: usize) {}
    fn add_to_playlist(&self, _: String, _: Track) {}
    fn remove_from_playlist(&self, _: String, _: String) {}

    fn download_tracks(&self, _: Vec<Track>) {}
    fn remove_downloads(&self, _: Vec<String>) {}

    fn check_update(&self) {}
    fn set_update_notifications(&self, _: bool) {}
    fn open_url(&self, _: String) -> Result<(), String> {
        Ok(())
    }

    fn search_songs(&self, _: u64, _: String) {}
    fn search_artists(&self, _: u64, _: String) {}
    fn search_albums(&self, _: u64, _: String) {}
    fn load_artist_page(&self, _: u64, _: String) {}
    fn load_artist_songs(&self, _: u64, _: String) {}
    fn load_album_page(&self, _: u64, _: String) {}

    fn fetch_cover(&self, _: String) {}
    fn check_online(&self) {}
}

// ---------------------------------------------------------------------------
// Live implementation (`rift` feature)
// ---------------------------------------------------------------------------

#[cfg(feature = "backend")]
pub(super) use live::LiveCommands;

#[cfg(feature = "backend")]
mod live {
    //! The production [`BackendCommands`](super::BackendCommands) implementation.
    //!
    //! Each async command clones the cheap `BackendHandle` (never the owning
    //! `RiftBackend`, which owns the runtime and must stay on the UI thread) and
    //! spawns a task on the backend runtime. The task sends exactly one
    //! [`TaskResult`](super::super::tasks::TaskResult) back, so a reply can never be
    //! delivered twice.

    use std::sync::mpsc::Sender;

    use rift_types::{Bootstrap, Track};

    use super::super::tasks::TaskResult;
    use super::BackendCommands;
    use crate::cover::decode_cover;

    /// Production command seam: forwards to `rift::BackendHandle`.
    pub(in crate::controller) struct LiveCommands {
        handle: rift::BackendHandle,
        /// Completion channel back to the controller (owned by the UI thread).
        tasks: Sender<TaskResult>,
    }

    impl LiveCommands {
        pub(in crate::controller) fn new(
            handle: rift::BackendHandle,
            tasks: Sender<TaskResult>,
        ) -> Self {
            Self { handle, tasks }
        }

        /// Spawn an async command whose future resolves to one `TaskResult`.
        fn spawn_result<F>(&self, future: F)
        where
            F: std::future::Future<Output = TaskResult> + Send + 'static,
        {
            let tasks = self.tasks.clone();
            self.handle.spawn(async move {
                let result = future.await;
                // A closed channel means the app is exiting; nothing to report.
                let _ = tasks.send(result);
            });
        }

        /// Spawn a cover fetch: network on the backend runtime, decode/downscale
        /// on its blocking pool, then one completion with the decoded image.
        fn spawn_cover(&self, url: String) {
            let handle = self.handle.clone();
            let tasks = self.tasks.clone();
            self.handle.spawn(async move {
                let result = match handle.fetch_image(url.clone()).await {
                    Ok(bytes) => {
                        let decode_handle = handle.clone();
                        let decode_url = url.clone();
                        match decode_handle
                            .spawn_blocking(move || decode_cover(&decode_url, &bytes))
                            .await
                        {
                            Ok(result) => result,
                            Err(error) => Err(format!("cover decode task failed: {error}")),
                        }
                    }
                    Err(message) => Err(message),
                };
                let _ = tasks.send(TaskResult::Cover { url, result });
            });
        }
    }

    impl BackendCommands for LiveCommands {
        fn bootstrap(&self) -> Result<Bootstrap, String> {
            Ok(self.handle.bootstrap())
        }

        // ------------------------------------------------------- playback
        fn play_tracks(&self, tracks: Vec<Track>, start: usize, source: Option<String>) {
            let handle = self.handle.clone();
            self.spawn_result(async move {
                TaskResult::PlayTracks(handle.play_tracks(tracks, start, source).await)
            });
        }

        fn play_track(&self, track: Track) {
            let handle = self.handle.clone();
            self.spawn_result(async move { TaskResult::PlayTrack(handle.play_track(track).await) });
        }

        fn toggle_play(&self) {
            self.handle.toggle_play();
        }
        fn next_track(&self) {
            self.handle.next_track();
        }
        fn prev_track(&self) {
            self.handle.prev_track();
        }
        fn seek(&self, position: f64) {
            self.handle.seek(position);
        }
        fn set_volume(&self, volume: f32) {
            self.handle.set_volume(volume);
        }
        fn save_volume(&self, volume: f32) {
            self.handle.save_volume(volume);
        }
        fn set_discord_rpc(&self, enabled: bool) {
            self.handle.set_discord_rpc(enabled);
        }
        fn set_crossfade(&self, secs: f32) {
            self.handle.set_crossfade(secs);
        }
        fn toggle_shuffle(&self) {
            self.handle.toggle_shuffle();
        }
        fn cycle_repeat(&self) {
            self.handle.cycle_repeat();
        }

        // ---------------------------------------------------------- queue
        fn queue_add(&self, track: Track) {
            self.handle.queue_add(track);
        }
        fn queue_play_next(&self, tracks: Vec<Track>) {
            self.handle.queue_play_next(tracks);
        }
        fn queue_move(&self, from: usize, to: usize) {
            self.handle.queue_move(from, to);
        }
        fn queue_remove(&self, index: usize) {
            self.handle.queue_remove(index);
        }
        fn queue_jump(&self, index: usize) {
            self.handle.queue_jump(index);
        }
        fn queue_clear(&self) {
            self.handle.queue_clear();
        }

        // -------------------------------------------------------- library
        fn toggle_like(&self, track: Track) {
            self.handle.toggle_like(track);
        }
        fn create_playlist(&self, name: String) -> Result<(), String> {
            self.handle.create_playlist(name).map(|_| ())
        }
        fn import_yt_playlist(&self, url: String) {
            let handle = self.handle.clone();
            self.spawn_result(async move {
                TaskResult::ImportYtPlaylist(handle.import_yt_playlist(url).await)
            });
        }
        fn export_playlist(&self, id: String) {
            self.handle.export_playlist(id);
        }
        fn import_playlist(&self) {
            self.handle.import_playlist();
        }
        fn delete_playlist(&self, id: String) {
            self.handle.delete_playlist(id);
        }
        fn rename_playlist(&self, id: String, name: String) {
            self.handle.rename_playlist(id, name);
        }
        fn move_playlist(&self, id: String, to: usize) {
            self.handle.move_playlist(id, to);
        }
        fn add_to_playlist(&self, id: String, track: Track) {
            self.handle.add_to_playlist(id, track);
        }
        fn remove_from_playlist(&self, id: String, track_id: String) {
            self.handle.remove_from_playlist(id, track_id);
        }

        // ------------------------------------------------------ downloads
        fn download_tracks(&self, tracks: Vec<Track>) {
            self.handle.download_tracks(tracks);
        }
        fn remove_downloads(&self, ids: Vec<String>) {
            self.handle.remove_downloads(ids);
        }

        // -------------------------------------------------------- updates
        fn check_update(&self) {
            let handle = self.handle.clone();
            self.spawn_result(async move { TaskResult::CheckUpdate(handle.check_update().await) });
        }
        fn set_update_notifications(&self, enabled: bool) {
            self.handle.set_update_notifications(enabled);
        }
        fn open_url(&self, url: String) -> Result<(), String> {
            self.handle.open_url(url)
        }

        // ------------------------------------------------ search/catalog
        fn search_songs(&self, generation: u64, query: String) {
            let handle = self.handle.clone();
            self.spawn_result(async move {
                let results = handle.search(query.clone()).await;
                TaskResult::SearchSongs {
                    generation,
                    query,
                    results,
                }
            });
        }
        fn search_artists(&self, generation: u64, query: String) {
            let handle = self.handle.clone();
            self.spawn_result(async move {
                let results = handle.search_artists(query.clone()).await;
                TaskResult::SearchArtists {
                    generation,
                    query,
                    results,
                }
            });
        }
        fn search_albums(&self, generation: u64, query: String) {
            let handle = self.handle.clone();
            self.spawn_result(async move {
                let results = handle.search_albums(query.clone()).await;
                TaskResult::SearchAlbums {
                    generation,
                    query,
                    results,
                }
            });
        }
        fn load_artist_page(&self, generation: u64, id: String) {
            let handle = self.handle.clone();
            let artist_id = id.clone();
            self.spawn_result(async move {
                TaskResult::ArtistPage {
                    generation,
                    artist_id,
                    result: handle.get_artist(id).await,
                }
            });
        }
        fn load_artist_songs(&self, generation: u64, id: String) {
            let handle = self.handle.clone();
            let artist_id = id.clone();
            self.spawn_result(async move {
                TaskResult::ArtistSongs {
                    generation,
                    artist_id,
                    result: handle.get_artist_songs(id).await,
                }
            });
        }
        fn load_album_page(&self, generation: u64, id: String) {
            let handle = self.handle.clone();
            let album_id = id.clone();
            self.spawn_result(async move {
                TaskResult::AlbumPage {
                    generation,
                    album_id,
                    result: handle.get_album(id).await,
                }
            });
        }

        // --------------------------------------------------------- images
        fn fetch_cover(&self, url: String) {
            self.spawn_cover(url);
        }

        // --------------------------------------------------- connectivity
        fn check_online(&self) {
            let handle = self.handle.clone();
            self.spawn_result(async move { TaskResult::Online(handle.is_online().await) });
        }
    }
}
