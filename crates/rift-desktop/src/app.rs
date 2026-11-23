//! The root application object: owns UI state, the immediate-mode context and
//! the backend controller, and runs exactly one frame at a time.
//!
//! `main.rs` drives it from winit; `--screenshot` mode drives it once and feeds
//! the resulting [`PaintList`] to the offscreen renderer.

use rift_types::{PlaybackState, RepeatMode};
use tracing::{debug, warn};

use crate::controller::{Controller, ControllerEvent};
use crate::gfx::paint::{PaintList, TextureId};
#[cfg(any(debug_assertions, feature = "preview", test))]
use crate::preview;
use crate::screens;
use crate::state::{
    play_collection_action, Action, ResizeEdge, State, Toast, View, MAX_CACHED_COVERS,
};
use crate::ui::{CursorShape, InputState, Key, Ui};

/// A request to the OS window, produced by the UI (titlebar buttons, drag
/// regions, edge resizes) and executed by `main.rs`, which owns the winit window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowCommand {
    Drag,
    Minimize,
    ToggleMaximize,
    Close,
    BeginResize(ResizeEdge),
}

/// How long the search box text must stay unchanged before the live search is
/// issued (`ui-spec.md` §6.1: 300 ms). Enter and the Search button bypass it.
pub const SEARCH_DEBOUNCE_SECS: f64 = 0.3;

/// The whole application.
pub struct App {
    pub state: State,
    pub ui: Ui,
    pub controller: Controller,
    /// Window commands collected during the last frame; drained by `main.rs`.
    window_commands: Vec<WindowCommand>,
    /// Cursor shape the UI requested for the frame just built; `main.rs` maps it
    /// to a winit cursor icon.
    pub cursor_shape: CursorShape,
    /// Last frame time (UI clock), used for toast expiry and animations.
    now: f64,
    /// Pending debounced live search: `(trimmed query, deadline in UI seconds)`.
    ///
    /// A search-box edit emits [`Action::SearchQueryChanged`] immediately, but
    /// the network `Action::Search` is only dispatched once the text has been
    /// stable for [`SEARCH_DEBOUNCE_SECS`] — otherwise every keystroke launched
    /// its own request. Driven by [`InputState::time`], never a wall-clock
    /// sleep/thread; Enter, the Search button and clearing the box all resolve
    /// or cancel it immediately.
    search_debounce: Option<(String, f64)>,
}

impl App {
    /// Build the app. `controller` decides whether it is a live application or
    /// preview/screenshot mode; `preview` seeds the placeholder data.
    pub fn new(preview: bool, controller: Controller) -> Self {
        let ui_started = std::time::Instant::now();
        let ui = Ui::new();
        debug!(
            "startup: ui/fonts {:.1} ms",
            ui_started.elapsed().as_secs_f64() * 1000.0
        );
        Self::new_with_ui(preview, controller, ui)
    }

    /// Build the app around an already-constructed [`Ui`] context.
    ///
    /// The UI context's only expensive part is parsing the embedded fonts, which
    /// is CPU-only and independent of the window and the backend. The windowed
    /// entry point builds it on a worker thread while the main thread is inside
    /// the Vulkan renderer construction, then hands the finished context here —
    /// the startup path overlaps two otherwise-serial costs. Keeping the
    /// constructor separate also means tests and the screenshot path can keep
    /// using [`App::new`] unchanged.
    pub fn new_with_ui(preview: bool, controller: Controller, ui: Ui) -> Self {
        let state = if preview {
            // Preview data exists only in development builds (or a build with
            // the explicit `preview` feature); the shipped release build never
            // receives `preview == true` from `main`.
            #[cfg(any(debug_assertions, feature = "preview", test))]
            {
                crate::preview::preview_state()
            }
            #[cfg(not(any(debug_assertions, feature = "preview", test)))]
            {
                State::default()
            }
        } else {
            State::default()
        };
        let mut app = Self {
            state,
            ui,
            controller,
            window_commands: Vec::new(),
            cursor_shape: CursorShape::Default,
            now: 0.0,
            search_debounce: None,
        };
        if app.controller.is_live() {
            let bootstrap_started = std::time::Instant::now();
            match app.controller.bootstrap() {
                Ok(bootstrap) => app.state.apply_bootstrap(bootstrap),
                Err(error) => {
                    warn!("bootstrap failed: {error}");
                    app.toast_error(format!("bootstrap: {error}"));
                }
            }
            debug!(
                "startup: bootstrap snapshot {:.1} ms",
                bootstrap_started.elapsed().as_secs_f64() * 1000.0
            );
        }
        app
    }

    /// Replace the state with a named preview scenario. Used by `--screenshot
    /// --view <name>` so every screen can be captured without network.
    ///
    /// Development-only: compiled for debug builds and the explicit `preview`
    /// feature, never into the shipped release binary.
    #[cfg(any(debug_assertions, feature = "preview", test))]
    pub fn set_preview_view(&mut self, view: &str) {
        self.state.view = match view {
            "home" => View::Home,
            "search" => {
                let (tracks, artists, albums) = preview::preview_search_results();
                self.state.search.query = "Song Title".into();
                self.state.search.shown_query = "Song Title".into();
                self.state.search.songs = tracks;
                self.state.search.artists = Some(artists);
                self.state.search.albums = Some(albums);
                View::Search
            }
            "liked" => View::Liked,
            "settings" => View::Settings,
            "artist" => {
                self.state.artist_page = Some(preview::preview_artist_page());
                View::Artist("preview-artist-1".into())
            }
            "artist-songs" => {
                self.state.artist_page = Some(preview::preview_artist_page());
                self.state.artist_songs = Some((
                    "preview-artist-1".into(),
                    (1..=20).map(preview::preview_track).collect(),
                ));
                View::ArtistSongs("preview-artist-1".into())
            }
            "album" => {
                self.state.album_page = Some(preview::preview_album_page());
                View::Album("preview-album-1".into())
            }
            "playlist" => View::Playlist("preview-pl-1".into()),
            "queue" => View::Home,
            other => {
                warn!("unknown preview view {other:?}; using home");
                View::Home
            }
        };
    }

    /// Push an error toast (also used for local validation failures).
    pub fn toast_error(&mut self, message: impl Into<String>) {
        self.state.toast = Some(Toast::error(message, self.now));
    }

    /// Push an informational toast.
    pub fn toast(&mut self, message: impl Into<String>) {
        self.state.toast = Some(Toast::notice(message, self.now));
    }

    /// Drain backend events into state and expire stale UI state. Called once
    /// per frame before [`Self::frame`].
    pub fn tick(&mut self) {
        for event in self.controller.poll() {
            self.apply_controller_event(event);
        }
        // Expire the toast.
        if let Some(toast) = &self.state.toast {
            if toast.is_expired(self.now) {
                self.state.toast = None;
            }
        }
        // Kick off cover downloads the screens asked for during the last frame.
        let requests = std::mem::take(&mut self.state.cover_requests);
        for url in requests {
            self.controller.request_cover(url);
        }
    }

    fn apply_controller_event(&mut self, event: ControllerEvent) {
        match event {
            ControllerEvent::Track(track) => {
                // A new track resets the seek bar to 0 until progress arrives.
                if self.state.track.as_ref().map(|t| &t.id) != track.as_ref().map(|t| &t.id) {
                    self.state.progress = rift_types::Progress::default();
                }
                self.state.track = track;
            }
            ControllerEvent::PlaybackState(state) => self.state.playback = state,
            ControllerEvent::Queue(queue) => self.state.queue = queue,
            ControllerEvent::Progress(progress) => self.state.progress = progress,
            ControllerEvent::Library(library) => self.state.set_library(library),
            ControllerEvent::Downloads(downloads) => self.state.downloads = downloads,
            ControllerEvent::DownloadProgress { id, permille } => {
                // A progress tick queued just before a finish/remove snapshot
                // must not recreate a bar for a track no longer in flight.
                if self.state.downloads.downloading.contains(&id) {
                    self.state.downloads.progress.insert(id, permille);
                }
            }
            ControllerEvent::Error(message) => {
                self.state.toast = Some(Toast::error(message, self.now));
            }
            ControllerEvent::Notice(message) => {
                self.state.toast = Some(Toast::notice(message, self.now));
            }
            ControllerEvent::OpenPlaylist(id) => {
                // Route through `navigate` so the controller's page-generation
                // bookkeeping sees the view change: an artist/album page reply
                // that is still in flight must not land after the jump.
                self.navigate(View::Playlist(id));
            }
            ControllerEvent::SearchResults { query, results } => {
                if self.state.search.shown_query == query {
                    self.state.search.busy = false;
                    match results {
                        Ok(songs) => self.state.search.songs = songs,
                        Err(message) => {
                            self.state.search.songs.clear();
                            self.toast_error(message);
                        }
                    }
                }
            }
            ControllerEvent::SearchArtists { query, results } => {
                if self.state.search.shown_query == query {
                    match results {
                        Ok(artists) => self.state.search.artists = Some(artists),
                        Err(message) => {
                            self.state.search.artists = Some(Vec::new());
                            self.toast_error(message);
                        }
                    }
                }
            }
            ControllerEvent::SearchAlbums { query, results } => {
                if self.state.search.shown_query == query {
                    match results {
                        Ok(albums) => self.state.search.albums = Some(albums),
                        Err(message) => {
                            self.state.search.albums = Some(Vec::new());
                            self.toast_error(message);
                        }
                    }
                }
            }
            ControllerEvent::ArtistPage {
                generation,
                artist_id,
                result,
            } => match result {
                Ok(page) => {
                    if generation >= self.state.page_generation {
                        self.state.page_generation = generation;
                        self.state.page_error = None;
                        self.state.artist_page = Some(page);
                    }
                }
                Err(message) => {
                    self.apply_page_error(View::Artist(artist_id), generation, message);
                }
            },
            ControllerEvent::ArtistSongs {
                generation,
                artist_id,
                result,
            } => match result {
                Ok(tracks) => {
                    if generation >= self.state.page_generation {
                        self.state.page_generation = generation;
                        self.state.page_error = None;
                        self.state.artist_songs = Some((artist_id, tracks));
                    }
                }
                Err(message) => {
                    self.apply_page_error(View::ArtistSongs(artist_id), generation, message);
                }
            },
            ControllerEvent::AlbumPage {
                generation,
                album_id,
                result,
            } => match result {
                Ok(page) => {
                    if generation >= self.state.page_generation {
                        self.state.page_generation = generation;
                        self.state.page_error = None;
                        self.state.album_page = Some(page);
                    }
                }
                Err(message) => {
                    self.apply_page_error(View::Album(album_id), generation, message);
                }
            },
            ControllerEvent::UpdateStatus(status) => {
                if status.update_available {
                    self.state.update_banner = Some(status.clone());
                }
                self.state.update_status = Some(status);
            }
            ControllerEvent::Cover(image) => {
                // A cover can only be used if its cache entry still exists: the
                // entry is what the screens look the texture up through. It can
                // have been evicted while the fetch was in flight (the entry cap
                // prefers failed/never-decoded URLs, but a stale request is a
                // candidate too), so an orphaned reply is simply dropped rather
                // than uploading a texture nothing could ever release.
                if !self.state.covers.contains_key(&image.url) {
                    debug!("dropping cover for unknown url: {}", image.url);
                    return;
                }
                let id = self.state.next_texture_id;
                self.state.next_texture_id = self.state.next_texture_id.wrapping_add(1);
                if let Some(entry) = self.state.covers.get_mut(&image.url) {
                    entry.texture = Some(id);
                    entry.requested = false;
                }
                self.ui
                    .upload_texture(id, image.width, image.height, image.rgba);
                self.trim_cover_textures();
            }
            ControllerEvent::CoverFailed(url) => {
                if let Some(entry) = self.state.covers.get_mut(&url) {
                    entry.failed = true;
                    entry.requested = false;
                }
            }
            ControllerEvent::OpenArtist(id) => {
                self.state.artist_page = None;
                self.navigate(View::Artist(id));
            }
            ControllerEvent::Online(online) => {
                self.state.online = online;
                debug!("network reachability: {online}");
            }
        }
    }

    /// Enter the retryable page-error state for `failed`, if it is still the
    /// page the user is looking at.
    ///
    /// The controller already dropped stale page replies before they became
    /// events, so the view check is defence in depth (and the generation guard
    /// mirrors the success arms). The page caches are cleared so the view cannot
    /// fall back to a stale page from a previous navigation, and `page_error`
    /// drives the compact error + Retry state in `album.rs`/`artist.rs` instead
    /// of a permanent "Loading…".
    fn apply_page_error(&mut self, failed: View, generation: u64, message: String) {
        if generation < self.state.page_generation || self.state.view != failed {
            return;
        }
        self.state.page_generation = generation;
        match &self.state.view {
            View::Artist(_) => self.state.artist_page = None,
            View::ArtistSongs(_) => self.state.artist_songs = None,
            View::Album(_) => self.state.album_page = None,
            View::Home | View::Search | View::Liked | View::Settings | View::Playlist(_) => {}
        }
        self.state.page_error = Some(message);
    }

    /// Issue the pending debounced live search once its query has been stable
    /// for [`SEARCH_DEBOUNCE_SECS`]. Called at the end of every frame (after the
    /// frame's actions, so Enter/submit can supersede it); no timer thread or
    /// sleep is involved — the deadline is on the UI clock.
    fn flush_search_debounce(&mut self) {
        let Some((query, deadline)) = self.search_debounce.as_ref() else {
            return;
        };
        if self.now < *deadline {
            return;
        }
        // `lost_focus` is committed in `end_frame` and reported next frame, so a
        // click-away that lands on the debounce frame could otherwise dispatch
        // the search, navigate away and swallow the click. The field's current
        // focus is already up to date here.
        if !self
            .ui
            .is_focused(crate::screens::chrome::search_input_id())
        {
            self.search_debounce = None;
            return;
        }
        let query = query.clone();
        self.search_debounce = None;
        self.dispatch_ui(Action::Search(query));
    }

    /// Keep the cover-texture cache bounded (see [`MAX_CACHED_COVERS`]).
    ///
    /// Evicts the oldest decoded covers — texture ids are handed out in upload
    /// order, so the lowest id is the oldest — and releases their GPU images
    /// through the UI's dropped-texture list (the renderer retires them safely
    /// after the in-flight frames). The cache entries are removed with their
    /// texture ([`State::remove_cover`]), keeping the insertion order in sync;
    /// only entries that own a texture are chosen, so an in-flight fetch is
    /// never orphaned by this trimmer (the separate entry cap in
    /// [`State::request_cover`] handles never-decoded URLs). Called after every
    /// decoded cover arrives.
    fn trim_cover_textures(&mut self) {
        if self.state.covers.len() <= MAX_CACHED_COVERS {
            return;
        }
        let mut loaded: Vec<(TextureId, String)> = self
            .state
            .covers
            .iter()
            .filter_map(|(url, entry)| entry.texture.map(|id| (id, url.clone())))
            .collect();
        if loaded.len() <= MAX_CACHED_COVERS {
            return;
        }
        loaded.sort_unstable_by_key(|(id, _)| *id);
        let excess = loaded.len() - MAX_CACHED_COVERS;
        for (id, url) in loaded.into_iter().take(excess) {
            // The entry may have been replaced by a re-request in the meantime;
            // only drop the texture that is still stored for this URL.
            if self
                .state
                .covers
                .get(&url)
                .is_some_and(|entry| entry.texture == Some(id))
            {
                self.state.remove_cover(&url);
                self.ui.drop_texture(id);
            }
        }
    }

    /// Build one frame. Returns its paint list; window commands are queued for
    /// `main.rs` and can be taken with [`Self::take_window_commands`].
    pub fn frame(&mut self, input: InputState) -> PaintList {
        self.now = input.time;
        let viewport = input.viewport;

        // Global keyboard shortcuts (the old UI's Window-level listeners): skip
        // typing fields, which handle their own keys.
        if !self.ui.wants_keyboard() {
            if input.key_clicked(Key::Space) {
                self.dispatch_ui(Action::TogglePlay);
            }
            if input.command() && input.key_pressed(Key::Right) {
                self.dispatch_ui(Action::NextTrack);
            }
            if input.command() && input.key_pressed(Key::Left) {
                self.dispatch_ui(Action::PrevTrack);
            }
            if input.key_clicked(Key::Escape) {
                self.state.renaming = None;
                self.state.new_playlist = None;
            }
        }

        self.ui.begin_frame(input, viewport);
        let mut actions = Vec::new();
        screens::root(&mut self.ui, &mut self.state, &mut actions);
        let paint = self.ui.end_frame();
        self.cursor_shape = self.ui.cursor_shape();

        for action in actions {
            self.dispatch_ui(action);
        }
        // Resolve the debounce *after* this frame's actions: an Enter or Search
        // click dispatched above clears it, so a submit and an expiring debounce
        // can never issue two searches for the same query in one frame.
        self.flush_search_debounce();
        paint
    }

    /// Take the window commands produced during the last frame.
    pub fn take_window_commands(&mut self) -> Vec<WindowCommand> {
        std::mem::take(&mut self.window_commands)
    }

    /// Logical-pixel caret rectangle of the focused text input, if one was
    /// drawn this frame. `main.rs` turns it into `set_ime_cursor_area`.
    pub fn ime_cursor_area(&self) -> Option<crate::ui::Rect> {
        self.ui.ime_cursor_area()
    }

    /// Apply one UI action: local state changes immediately, backend commands
    /// through the controller.
    pub fn dispatch_ui(&mut self, action: Action) {
        use Action::*;
        match action {
            // --------------------------------------------------- navigation
            Navigate(view) => self.navigate(view),
            OpenArtist(id) => {
                self.state.artist_page = None;
                self.navigate(View::Artist(id));
            }
            OpenArtistSongs(id) => {
                self.state.artist_songs = None;
                self.navigate(View::ArtistSongs(id));
            }
            OpenAlbum(id) => {
                self.state.album_page = None;
                self.navigate(View::Album(id));
            }
            SetSearchTab(tab) => {
                self.state.search.tab = tab;
                // The controller lazily fetches artist/album results the first
                // time a tab is opened for the submitted query (old-UI behaviour).
                self.controller.dispatch(Action::SetSearchTab(tab));
            }
            SearchQueryChanged(query) => {
                // The titlebar is the only writer of `search.query`; the action
                // mirrors the raw text and, for a non-empty box, schedules the
                // debounced live search. `Search` itself (below) never rewrites
                // the edit buffer, so typed whitespace survives.
                self.state.search.query = query.clone();
                let trimmed = query.trim();
                if trimmed.is_empty() {
                    // Clearing (or typing only spaces) cancels a pending
                    // debounce immediately, so no request can fire after the box
                    // is empty.
                    self.search_debounce = None;
                    // Clearing the box must invalidate an in-flight search so its
                    // results cannot repopulate the cleared view.
                    if !self.state.search.shown_query.is_empty() {
                        self.controller.cancel_search();
                        self.state.search.shown_query.clear();
                        self.state.search.songs.clear();
                        self.state.search.artists = None;
                        self.state.search.albums = None;
                        self.state.search.busy = false;
                    }
                } else {
                    self.search_debounce =
                        Some((trimmed.to_string(), self.now + SEARCH_DEBOUNCE_SECS));
                }
            }
            SearchBlur => self.search_debounce = None,
            Search(query) => {
                // Enter / the Search button submit immediately and supersede a
                // pending live-search debounce (only one request may result).
                self.search_debounce = None;
                // `query` is the *trimmed* box text handed over to the search
                // command; the box itself is the single writer of the live edit
                // buffer, which `SearchQueryChanged` (emitted first on the same
                // edit frame) has already updated with the raw text. Writing the
                // trimmed payload back into `state.search.query` would delete the
                // whitespace the user just typed — a space at the end of its
                // frame vanished, so `"sign of the time"` searched as
                // `"signofthetime"`.
                self.state.search.meta_query(&query);
                // Route through the shared navigation helper, not a raw view
                // assignment: submitting a search must bump the page generation
                // (dropping an abandoned artist/album reply) and clear a stale
                // page error, exactly like every other navigation.
                self.navigate(View::Search);
                self.controller.dispatch(Action::Search(query));
            }
            ToggleQueuePanel | ToggleQueueCollapsed => {}

            // ----------------------------------------------------- playback
            SetVolume(volume) => {
                self.state.volume = volume.clamp(0.0, 1.0);
                self.controller
                    .dispatch(Action::SetVolume(self.state.volume));
            }
            SetCrossfade(secs) => {
                self.state.crossfade = secs;
                self.controller.dispatch(Action::SetCrossfade(secs));
            }
            SetDiscordRpc(enabled) => {
                self.state.discord_rpc = enabled;
                self.controller.dispatch(Action::SetDiscordRpc(enabled));
            }
            SetUpdateNotifications(enabled) => {
                self.state.update_notifications = enabled;
                self.controller
                    .dispatch(Action::SetUpdateNotifications(enabled));
                if !enabled {
                    self.state.update_banner = None;
                }
            }
            SetOnline(online) => self.state.online = online,
            DismissUpdateBanner => self.state.update_banner = None,
            Toast(message) => self.toast(message),
            ToastError(message) => self.toast_error(message),
            // Retry is a new page request: clear the error first so the view
            // shows its loading state on this frame, then let the controller
            // re-issue `last_page` at a fresh generation.
            RetryPage => {
                self.state.page_error = None;
                self.controller.dispatch(Action::RetryPage);
            }

            // ----------------------------------------------- playlist editor
            BeginRenamePlaylist { id, name } => {
                // The only rename editor lives in the playlist view header
                // (ui-spec §7.3): a rename started from the sidebar/Home card
                // must navigate there, or it sets `renaming` with nothing on
                // screen to show or focus it.
                if self.state.view != View::Playlist(id.clone()) {
                    self.navigate(View::Playlist(id.clone()));
                }
                self.state.renaming = Some((id, name));
                self.state.new_playlist = None;
            }
            BeginNewPlaylist => {
                self.state.new_playlist = Some(String::new());
                self.state.renaming = None;
            }
            PlaylistDraftChanged { id, name } => match id {
                Some(id) => self.state.renaming = Some((id, name)),
                None => self.state.new_playlist = Some(name),
            },
            ClosePlaylistEditor => {
                self.state.renaming = None;
                self.state.new_playlist = None;
            }

            // ------------------------------------------------------- window
            WindowDrag => self.window_commands.push(WindowCommand::Drag),
            WindowMinimize => self.window_commands.push(WindowCommand::Minimize),
            WindowToggleMaximize => self.window_commands.push(WindowCommand::ToggleMaximize),
            WindowClose => self.window_commands.push(WindowCommand::Close),
            WindowBeginResize(edge) => self.window_commands.push(WindowCommand::BeginResize(edge)),

            // ------------------------------------------- everything backend
            other => self.controller.dispatch(other),
        }
    }

    fn navigate(&mut self, view: View) {
        debug!("navigate to {view:?}");
        // Leaving the page invalidates its error state; a new request or success
        // clears it too. A pending live-search debounce is also abandoned: the
        // user has moved on, and it must not yank them back to Search.
        self.state.page_error = None;
        self.search_debounce = None;
        // An open context menu belongs to the old view; a backend-driven jump
        // (`OpenPlaylist`) must not leave it floating over the new page. The
        // per-frame auto-close only covers menus that no screen drew.
        crate::screens::menu::close(&mut self.ui);
        self.state.view = view;
        // Issue page loads / cancel superseded ones; search and local views are
        // ignored by the controller.
        let view = self.state.view.clone();
        self.controller.navigate_view(&view);
    }

    /// Play a collection with the old UI's exact semantics (toggle when it is
    /// already loaded; start shuffled when shuffle is on). `random` comes from
    /// the caller so the choice is testable.
    pub fn play_collection(
        &mut self,
        tracks: Vec<rift_types::Track>,
        source: Option<String>,
        random: f64,
    ) {
        let action = play_collection_action(&self.state, tracks, source, random);
        self.dispatch_ui(action);
    }

    /// The repeat button's Off -> All -> One cycle.
    pub fn cycle_repeat(&mut self) {
        let next = crate::state::next_repeat(self.state.queue.repeat);
        self.state.queue.repeat = next;
        self.controller.dispatch(Action::CycleRepeat);
    }

    /// Convenience for the player bar: is the current track liked?
    pub fn current_track_is_liked(&self) -> bool {
        match &self.state.track {
            Some(track) => {
                let liked = self.state.liked_index();
                liked.contains(track)
            }
            None => false,
        }
    }

    /// Whether a track row should be greyed out: offline and not downloaded.
    pub fn track_unavailable(&self, track: &rift_types::Track) -> bool {
        !self.state.online && !self.state.downloads.downloaded.contains(&track.id)
    }

    /// Shuffle state for the transport controls.
    pub fn shuffle(&self) -> bool {
        self.state.queue.shuffle
    }

    pub fn repeat(&self) -> RepeatMode {
        self.state.queue.repeat
    }

    /// Playing state for the big play/pause button.
    pub fn is_playing(&self) -> bool {
        matches!(
            self.state.playback,
            PlaybackState::Playing | PlaybackState::Loading
        )
    }
}

/// Small helper on the search state: remember which query the results belong to
/// and mark the search in flight.
impl crate::state::SearchState {
    pub fn meta_query(&mut self, query: &str) {
        self.shown_query = query.to_string();
        self.busy = true;
        self.songs.clear();
        self.artists = None;
        self.albums = None;
        self.generation = self.generation.wrapping_add(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cover::CoverImage;
    use crate::state::MAX_CACHED_COVER_ENTRIES;

    fn cover_event(url: &str) -> ControllerEvent {
        ControllerEvent::Cover(CoverImage {
            url: url.to_string(),
            width: 1,
            height: 1,
            rgba: vec![1, 2, 3, 255],
        })
    }

    /// A failed page reply must become the retryable `page_error` state, and a
    /// later successful reply (or navigation / retry) clears it. A failure for a
    /// page the user is not looking at is ignored.
    #[test]
    fn a_failed_page_reply_sets_and_a_success_clears_the_page_error() {
        let mut app = App::new(false, Controller::new_preview());
        app.state.view = View::Artist("artist-1".into());

        app.apply_controller_event(ControllerEvent::ArtistPage {
            generation: 1,
            artist_id: "artist-1".into(),
            result: Err("could not load artist: offline".into()),
        });
        assert_eq!(
            app.state.page_error.as_deref(),
            Some("could not load artist: offline")
        );
        assert_eq!(app.state.page_generation, 1);
        assert!(
            app.state.artist_page.is_none(),
            "a failed page must not keep a stale cache"
        );

        // A failure for a different page/view never replaces this one.
        app.apply_controller_event(ControllerEvent::AlbumPage {
            generation: 2,
            album_id: "album-1".into(),
            result: Err("could not load album".into()),
        });
        assert_eq!(
            app.state.page_error.as_deref(),
            Some("could not load artist: offline"),
            "an error for another view is ignored"
        );

        app.apply_controller_event(ControllerEvent::ArtistPage {
            generation: 3,
            artist_id: "artist-1".into(),
            result: Ok(crate::preview::preview_artist_page()),
        });
        assert_eq!(app.state.page_error, None, "a successful reply clears it");
        assert_eq!(app.state.page_generation, 3);
        assert!(app.state.artist_page.is_some());
    }

    /// Losing focus in the search box must cancel a pending debounced search,
    /// or it would switch the view ~300 ms after the user moved to another
    /// control.
    #[test]
    fn search_blur_cancels_a_pending_debounce() {
        let mut app = App::new(false, Controller::new_preview());
        app.search_debounce = Some(("daft".into(), app.now + SEARCH_DEBOUNCE_SECS));
        app.dispatch_ui(Action::SearchBlur);
        assert!(app.search_debounce.is_none());
    }

    /// The blur action is one frame late. If the debounce expires on the frame
    /// that a click blurred the box, flushing must not search/navigate away and
    /// swallow that click; the field's current focus is the deciding state.
    #[test]
    fn an_expired_debounce_does_not_fire_after_the_box_lost_focus() {
        let mut app = App::new(false, Controller::new_preview());
        app.search_debounce = Some(("daft".into(), app.now - 1.0));
        app.flush_search_debounce();
        assert!(app.search_debounce.is_none());
        assert_eq!(
            app.state.view,
            View::Home,
            "the view must not move to Search"
        );
    }

    /// Rename from the sidebar/Home card must reach the playlist view that owns
    /// the editor (ui-spec §7.3); otherwise `state.renaming` is set with no
    /// field to autofocus and the user has to click into the playlist manually.
    #[test]
    fn renaming_a_playlist_from_another_view_navigates_to_it() {
        let mut app = App::new(false, Controller::new_preview());
        app.dispatch_ui(Action::BeginRenamePlaylist {
            id: "pl-1".into(),
            name: "Mix".into(),
        });
        assert_eq!(app.state.view, View::Playlist("pl-1".into()));
        assert_eq!(
            app.state.renaming.as_ref().map(|(id, _)| id.as_str()),
            Some("pl-1")
        );

        // From another playlist, it follows the rename target.
        app.dispatch_ui(Action::BeginRenamePlaylist {
            id: "pl-2".into(),
            name: "Other".into(),
        });
        assert_eq!(app.state.view, View::Playlist("pl-2".into()));
        assert_eq!(
            app.state.renaming.as_ref().map(|(id, _)| id.as_str()),
            Some("pl-2")
        );
    }

    /// A backend-driven navigation must dismiss an open context menu; otherwise
    /// it floats over the new page until something else closes it.
    #[test]
    fn backend_navigation_closes_an_open_menu() {
        let mut app = App::new(false, Controller::new_preview());
        crate::screens::menu::open(
            &mut app.ui,
            crate::ui::Id::from_str("test/menu"),
            crate::ui::Rect::new(10.0, 10.0, 20.0, 20.0),
            Vec::new(),
        );
        assert!(crate::screens::menu::is_open());

        app.apply_controller_event(ControllerEvent::OpenPlaylist("pl-1".into()));

        assert!(
            !crate::screens::menu::is_open(),
            "navigation must close the old view's menu"
        );
        assert_eq!(app.state.view, View::Playlist("pl-1".into()));
    }

    /// Submitting a search is a navigation: it must clear a stale page error
    /// and cancel an abandoned page request, not just flip `view`.
    #[test]
    fn submitting_a_search_uses_the_shared_navigation_path() {
        let mut app = App::new(false, Controller::new_preview());
        app.state.view = View::Album("album-1".into());
        app.state.page_error = Some("old page failure".into());

        app.dispatch_ui(Action::Search("daft punk".into()));

        assert_eq!(app.state.view, View::Search);
        assert_eq!(
            app.state.page_error, None,
            "search navigation must clear a stale page error"
        );
        assert_eq!(app.state.search.shown_query, "daft punk");
    }

    /// A stalled page load followed by navigation must not carry the error to
    /// the next view.
    #[test]
    fn navigation_clears_a_previous_page_error() {
        let mut app = App::new(false, Controller::new_preview());
        app.state.page_error = Some("old failure".into());
        app.navigate(View::Home);
        assert_eq!(app.state.page_error, None);
    }

    /// A long browsing session visits more distinct cover URLs than are worth
    /// keeping in VRAM: the cache must stay bounded, evict oldest-first, and hand
    /// evicted textures to the renderer instead of leaking them.
    #[test]
    fn the_cover_texture_cache_is_bounded_and_releases_evicted_textures() {
        let mut app = App::new(false, Controller::new_preview());
        let total = MAX_CACHED_COVERS + 8;
        for index in 0..total {
            let url = format!("https://img.example/{index}");
            // The screens mark a URL as wanted first; the controller's decoded
            // reply then fills that entry in.
            assert!(app.state.request_cover(&url).is_none());
            app.apply_controller_event(cover_event(&url));
        }

        assert_eq!(app.state.covers.len(), MAX_CACHED_COVERS, "bounded cache");
        assert!(
            app.state
                .covers
                .contains_key(&format!("https://img.example/{}", total - 1)),
            "the newest cover must survive"
        );
        assert!(
            !app.state.covers.contains_key("https://img.example/0"),
            "the oldest cover is evicted first"
        );

        // The evicted textures are handed to the renderer: the UI flushes its
        // drop list into the next frame's paint list.
        let paint = app.ui.end_frame();
        assert_eq!(paint.dropped.len(), total - MAX_CACHED_COVERS);
    }

    /// A reply whose cache entry no longer exists (evicted, or never requested)
    /// must not upload a texture that nothing can ever release.
    #[test]
    fn a_cover_reply_for_an_unknown_url_uploads_nothing() {
        let mut app = App::new(false, Controller::new_preview());
        let before = app.state.next_texture_id;
        app.apply_controller_event(cover_event("https://img.example/orphan"));
        assert_eq!(app.state.next_texture_id, before, "no texture id consumed");
        assert!(
            app.ui.end_frame().created.is_empty(),
            "no texture may be uploaded for an unknown url"
        );
    }

    /// An offline session can produce far more failed cover URLs than decoded
    /// ones. They must not accumulate for the whole session, and the cap must
    /// not get in the way of a cover that does load afterwards.
    #[test]
    fn failed_cover_urls_stay_bounded_and_a_later_cover_still_loads() {
        let mut app = App::new(false, Controller::new_preview());
        for index in 0..(MAX_CACHED_COVER_ENTRIES * 3) {
            let url = format!("https://img.example/failed/{index}");
            assert_eq!(app.state.request_cover(&url), None);
            app.apply_controller_event(ControllerEvent::CoverFailed(url));
        }
        assert!(
            app.state.covers.len() <= MAX_CACHED_COVER_ENTRIES,
            "failed urls must stay bounded, got {}",
            app.state.covers.len()
        );

        let url = "https://img.example/decoded-after-failures";
        assert_eq!(app.state.request_cover(url), None);
        app.apply_controller_event(cover_event(url));
        assert!(
            app.state.cover_texture(url).is_some(),
            "a cover fetched after many failures must still load"
        );
        assert!(
            app.state.covers.len() <= MAX_CACHED_COVER_ENTRIES,
            "the loaded cover must not lift the entry cap"
        );
        let paint = app.ui.end_frame();
        assert_eq!(paint.created.len(), 1, "the fresh cover is uploaded");
    }
}
