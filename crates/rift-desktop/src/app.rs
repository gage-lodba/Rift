//! The root application object: owns UI state, the immediate-mode context and
//! the backend controller, and runs exactly one frame at a time.
//!
//! `main.rs` drives it from winit; `--screenshot` mode drives it once and feeds
//! the resulting [`PaintList`] to the offscreen renderer.

use rift_types::{PlaybackState, RepeatMode};
use tracing::{debug, warn};

use crate::controller::{Controller, ControllerEvent};
use crate::gfx::paint::PaintList;
#[cfg(any(debug_assertions, feature = "preview", test))]
use crate::preview;
use crate::screens;
use crate::state::{play_collection_action, Action, ResizeEdge, State, Toast, View};
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
}

impl App {
    /// Build the app. `controller` decides whether it is a live application or
    /// preview/screenshot mode; `preview` seeds the placeholder data.
    pub fn new(preview: bool, controller: Controller) -> Self {
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
            ui: Ui::new(),
            controller,
            window_commands: Vec::new(),
            cursor_shape: CursorShape::Default,
            now: 0.0,
        };
        if app.controller.is_live() {
            match app.controller.bootstrap() {
                Ok(bootstrap) => app.state.apply_bootstrap(bootstrap),
                Err(error) => {
                    warn!("bootstrap failed: {error}");
                    app.toast_error(format!("bootstrap: {error}"));
                }
            }
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
            ControllerEvent::Library(library) => self.state.library = library,
            ControllerEvent::Downloads(downloads) => self.state.downloads = downloads,
            ControllerEvent::Error(message) => {
                self.state.toast = Some(Toast::error(message, self.now));
            }
            ControllerEvent::Notice(message) => {
                self.state.toast = Some(Toast::notice(message, self.now));
            }
            ControllerEvent::OpenPlaylist(id) => {
                self.state.view = View::Playlist(id);
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
            ControllerEvent::ArtistPage { generation, page } => {
                if generation >= self.state.page_generation {
                    self.state.page_generation = generation;
                    self.state.artist_page = Some(page);
                }
            }
            ControllerEvent::ArtistSongs {
                generation,
                artist_id,
                tracks,
            } => {
                if generation >= self.state.page_generation {
                    self.state.page_generation = generation;
                    self.state.artist_songs = Some((artist_id, tracks));
                }
            }
            ControllerEvent::AlbumPage { generation, page } => {
                if generation >= self.state.page_generation {
                    self.state.page_generation = generation;
                    self.state.album_page = Some(page);
                }
            }
            ControllerEvent::UpdateStatus(status) => {
                if status.update_available {
                    self.state.update_banner = Some(status.clone());
                }
                self.state.update_status = Some(status);
            }
            ControllerEvent::YtDlpStatus(status) => {
                self.state.yt_dlp_status = Some(status);
            }
            ControllerEvent::Cover(image) => {
                let id = self.state.next_texture_id;
                self.state.next_texture_id += 1;
                if let Some(entry) = self.state.covers.get_mut(&image.url) {
                    entry.texture = Some(id);
                    entry.requested = false;
                }
                self.ui
                    .upload_texture(id, image.width, image.height, image.rgba);
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
        paint
    }

    /// Take the window commands produced during the last frame.
    pub fn take_window_commands(&mut self) -> Vec<WindowCommand> {
        std::mem::take(&mut self.window_commands)
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
                // Clearing the box must invalidate an in-flight search so its
                // results cannot repopulate the cleared view.
                if query.is_empty() && !self.state.search.shown_query.is_empty() {
                    self.controller.cancel_search();
                    self.state.search.shown_query.clear();
                    self.state.search.songs.clear();
                    self.state.search.artists = None;
                    self.state.search.albums = None;
                    self.state.search.busy = false;
                }
                self.state.search.query = query;
            }
            Search(query) => {
                self.state.search.query = query.clone();
                self.state.search.meta_query(&query);
                self.state.view = View::Search;
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

            // ----------------------------------------------- playlist editor
            BeginRenamePlaylist { id, name } => {
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
