//! Controller unit tests: dispatch mapping, generations, cover de-dupe and mirror bookkeeping.

use super::*;
use rift_types::{ArtistRef, Playlist};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

// ------------------------------------------------ recording double

/// One recorded command call; enough payload to prove the translation
/// (ids, indices, booleans) without cloning whole backend responses.
#[derive(Clone, Debug, PartialEq)]
enum Call {
    TogglePlay,
    NextTrack,
    PrevTrack,
    Seek(f64),
    SetVolume(f32),
    SaveVolume(f32),
    SetDiscordRpc(bool),
    SetCrossfade(f32),
    ToggleShuffle,
    CycleRepeat,
    QueueAdd(String),
    QueuePlayNext(Vec<String>),
    QueueMove(usize, usize),
    QueueRemove(usize),
    QueueJump(usize),
    QueueClear,
    ToggleLike(String),
    CreatePlaylist(String),
    ImportYtPlaylist(String),
    ExportPlaylist(String),
    ImportPlaylist,
    DeletePlaylist(String),
    RenamePlaylist(String, String),
    MovePlaylist(String, usize),
    AddToPlaylist(String, String),
    RemoveFromPlaylist(String, String),
    DownloadTracks(Vec<String>),
    RemoveDownloads(Vec<String>),
    CheckUpdate,
    SetUpdateNotifications(bool),
    OpenUrl(String),
    PlayTracks(Vec<String>, usize, Option<String>),
    PlayTrack(String),
    SearchSongs(u64, String),
    SearchArtists(u64, String),
    SearchAlbums(u64, String),
    LoadArtistPage(u64, String),
    LoadArtistSongs(u64, String),
    LoadAlbumPage(u64, String),
    FetchCover(String),
    CheckOnline,
}

type Calls = Arc<Mutex<Vec<Call>>>;

/// A [`BackendCommands`] that records every call, standing in for the live
/// backend in unit tests.
struct RecordingCommands {
    calls: Calls,
    bootstrap: Bootstrap,
    fail_open_url: Arc<AtomicBool>,
    fail_create_playlist: Arc<AtomicBool>,
}

impl BackendCommands for RecordingCommands {
    fn bootstrap(&self) -> Result<Bootstrap, String> {
        // Not a UI action, so it is not recorded as a `Call`; tests that care
        // inspect `controller.volume` / the track cache instead.
        Ok(self.bootstrap.clone())
    }

    fn play_tracks(&self, tracks: Vec<Track>, start: usize, source: Option<String>) {
        self.record(Call::PlayTracks(ids(&tracks), start, source));
    }
    fn play_track(&self, track: Track) {
        self.record(Call::PlayTrack(track.id));
    }
    fn toggle_play(&self) {
        self.record(Call::TogglePlay);
    }
    fn next_track(&self) {
        self.record(Call::NextTrack);
    }
    fn prev_track(&self) {
        self.record(Call::PrevTrack);
    }
    fn seek(&self, position: f64) {
        self.record(Call::Seek(position));
    }
    fn set_volume(&self, volume: f32) {
        self.record(Call::SetVolume(volume));
    }
    fn save_volume(&self, volume: f32) {
        self.record(Call::SaveVolume(volume));
    }
    fn set_discord_rpc(&self, enabled: bool) {
        self.record(Call::SetDiscordRpc(enabled));
    }
    fn set_crossfade(&self, secs: f32) {
        self.record(Call::SetCrossfade(secs));
    }
    fn toggle_shuffle(&self) {
        self.record(Call::ToggleShuffle);
    }
    fn cycle_repeat(&self) {
        self.record(Call::CycleRepeat);
    }
    fn queue_add(&self, track: Track) {
        self.record(Call::QueueAdd(track.id));
    }
    fn queue_play_next(&self, tracks: Vec<Track>) {
        self.record(Call::QueuePlayNext(ids(&tracks)));
    }
    fn queue_move(&self, from: usize, to: usize) {
        self.record(Call::QueueMove(from, to));
    }
    fn queue_remove(&self, index: usize) {
        self.record(Call::QueueRemove(index));
    }
    fn queue_jump(&self, index: usize) {
        self.record(Call::QueueJump(index));
    }
    fn queue_clear(&self) {
        self.record(Call::QueueClear);
    }
    fn toggle_like(&self, track: Track) {
        self.record(Call::ToggleLike(track.id));
    }
    fn create_playlist(&self, name: String) -> Result<(), String> {
        self.record(Call::CreatePlaylist(name));
        if self.fail_create_playlist.load(Ordering::Relaxed) {
            Err("playlist name cannot be empty".into())
        } else {
            Ok(())
        }
    }
    fn import_yt_playlist(&self, url: String) {
        self.record(Call::ImportYtPlaylist(url));
    }
    fn export_playlist(&self, id: String) {
        self.record(Call::ExportPlaylist(id));
    }
    fn import_playlist(&self) {
        self.record(Call::ImportPlaylist);
    }
    fn delete_playlist(&self, id: String) {
        self.record(Call::DeletePlaylist(id));
    }
    fn rename_playlist(&self, id: String, name: String) {
        self.record(Call::RenamePlaylist(id, name));
    }
    fn move_playlist(&self, id: String, to: usize) {
        self.record(Call::MovePlaylist(id, to));
    }
    fn add_to_playlist(&self, id: String, track: Track) {
        self.record(Call::AddToPlaylist(id, track.id));
    }
    fn remove_from_playlist(&self, id: String, track_id: String) {
        self.record(Call::RemoveFromPlaylist(id, track_id));
    }
    fn download_tracks(&self, tracks: Vec<Track>) {
        self.record(Call::DownloadTracks(ids(&tracks)));
    }
    fn remove_downloads(&self, ids: Vec<String>) {
        self.record(Call::RemoveDownloads(ids));
    }
    fn check_update(&self) {
        self.record(Call::CheckUpdate);
    }
    fn set_update_notifications(&self, enabled: bool) {
        self.record(Call::SetUpdateNotifications(enabled));
    }
    fn open_url(&self, url: String) -> Result<(), String> {
        self.record(Call::OpenUrl(url));
        if self.fail_open_url.load(Ordering::Relaxed) {
            Err("refusing to open non-http(s) url".into())
        } else {
            Ok(())
        }
    }
    fn search_songs(&self, generation: u64, query: String) {
        self.record(Call::SearchSongs(generation, query));
    }
    fn search_artists(&self, generation: u64, query: String) {
        self.record(Call::SearchArtists(generation, query));
    }
    fn search_albums(&self, generation: u64, query: String) {
        self.record(Call::SearchAlbums(generation, query));
    }
    fn load_artist_page(&self, generation: u64, id: String) {
        self.record(Call::LoadArtistPage(generation, id));
    }
    fn load_artist_songs(&self, generation: u64, id: String) {
        self.record(Call::LoadArtistSongs(generation, id));
    }
    fn load_album_page(&self, generation: u64, id: String) {
        self.record(Call::LoadAlbumPage(generation, id));
    }
    fn fetch_cover(&self, url: String) {
        self.record(Call::FetchCover(url));
    }
    fn check_online(&self) {
        self.record(Call::CheckOnline);
    }
}

impl RecordingCommands {
    fn record(&self, call: Call) {
        self.calls.lock().unwrap().push(call);
    }
}

/// A controller wired to a recording double, with helpers to inspect the
/// recorded calls and to inject async completions.
struct Harness {
    controller: Controller,
    calls: Calls,
    tasks: mpsc::Sender<TaskResult>,
    /// Switches the recording double's fallible commands to return `Err`.
    fail_open_url: Arc<AtomicBool>,
    fail_create_playlist: Arc<AtomicBool>,
}

impl Harness {
    fn new() -> Self {
        Self::with_bootstrap(Bootstrap::default())
    }

    fn with_bootstrap(bootstrap: Bootstrap) -> Self {
        let (sender, receiver) = mpsc::channel();
        let calls: Calls = Arc::new(Mutex::new(Vec::new()));
        let fail_open_url = Arc::new(AtomicBool::new(false));
        let fail_create_playlist = Arc::new(AtomicBool::new(false));
        let commands = RecordingCommands {
            calls: calls.clone(),
            bootstrap,
            fail_open_url: fail_open_url.clone(),
            fail_create_playlist: fail_create_playlist.clone(),
        };
        let mut controller = Controller::base(Box::new(commands), receiver, true);
        // Tests drive completions explicitly; keep the periodic reachability
        // probe out of the recorded call log (a dedicated test re-enables it).
        controller.probe_in_flight = true;
        Self {
            controller,
            calls,
            tasks: sender,
            fail_open_url,
            fail_create_playlist,
        }
    }

    /// Make `commands.open_url` reject the next URLs (backend scheme gate).
    fn fail_open_url(&self) {
        self.fail_open_url.store(true, Ordering::Relaxed);
    }

    /// Make `commands.create_playlist` reject the next names.
    fn fail_create_playlist(&self) {
        self.fail_create_playlist.store(true, Ordering::Relaxed);
    }

    fn take_calls(&self) -> Vec<Call> {
        std::mem::take(&mut *self.calls.lock().unwrap())
    }

    fn emit(&self, task: TaskResult) {
        self.tasks.send(task).expect("controller is alive");
    }

    fn dispatch(&mut self, action: Action) {
        self.controller.dispatch(action);
    }

    fn poll(&mut self) -> Vec<ControllerEvent> {
        self.controller.poll()
    }
}

fn ids(tracks: &[Track]) -> Vec<String> {
    tracks.iter().map(|t| t.id.clone()).collect()
}

fn track(id: &str) -> Track {
    Track {
        id: id.into(),
        title: format!("Title {id}"),
        artist: "Artist".into(),
        album: None,
        duration: Some(120),
        cover: String::new(),
        artists: vec![ArtistRef {
            id: Some(format!("artist-{id}")),
            name: "Artist".into(),
        }],
        album_id: None,
    }
}

fn library_with(tracks: &[&str]) -> Library {
    Library {
        liked: tracks.iter().map(|id| track(id)).collect(),
        playlists: vec![Playlist {
            id: "pl-1".into(),
            name: "Playlist".into(),
            tracks: vec![track("a")],
        }],
        recently_played: Vec::new(),
    }
}

// ------------------------------------------------------- dispatch map

#[test]
fn dispatch_maps_transport_and_volume_to_the_backend() {
    let mut h = Harness::new();
    h.dispatch(Action::TogglePlay);
    h.dispatch(Action::NextTrack);
    h.dispatch(Action::PrevTrack);
    h.dispatch(Action::Seek(42.5));
    h.dispatch(Action::SetVolume(0.75));
    h.dispatch(Action::SaveVolume);
    h.dispatch(Action::ToggleShuffle);
    h.dispatch(Action::CycleRepeat);
    assert_eq!(
        h.take_calls(),
        vec![
            Call::TogglePlay,
            Call::NextTrack,
            Call::PrevTrack,
            Call::Seek(42.5),
            Call::SetVolume(0.75),
            // SaveVolume carries no value: the last SetVolume is persisted.
            Call::SaveVolume(0.75),
            Call::ToggleShuffle,
            Call::CycleRepeat,
        ]
    );
}

#[test]
fn dispatch_maps_playback_and_queue_commands() {
    let mut h = Harness::new();
    h.dispatch(Action::PlayTracks {
        tracks: vec![track("a"), track("b")],
        start: 1,
        source: Some("liked".into()),
    });
    h.dispatch(Action::PlayTrack(track("c")));
    h.dispatch(Action::QueueAdd(track("d")));
    h.dispatch(Action::QueuePlayNext(track("e")));
    h.dispatch(Action::QueueMove { from: 2, to: 0 });
    h.dispatch(Action::QueueRemove(3));
    h.dispatch(Action::QueueJump(1));
    h.dispatch(Action::QueueClear);
    assert_eq!(
        h.take_calls(),
        vec![
            Call::PlayTracks(vec!["a".into(), "b".into()], 1, Some("liked".into())),
            Call::PlayTrack("c".into()),
            Call::QueueAdd("d".into()),
            Call::QueuePlayNext(vec!["e".into()]),
            Call::QueueMove(2, 0),
            Call::QueueRemove(3),
            Call::QueueJump(1),
            Call::QueueClear,
        ]
    );
}

#[test]
fn dispatch_maps_library_and_playlist_commands() {
    let mut h = Harness::with_bootstrap(Bootstrap {
        library: library_with(&["a", "b"]),
        ..Bootstrap::default()
    });
    h.controller.bootstrap().expect("bootstrap");
    h.take_calls();

    h.dispatch(Action::ToggleLike(track("b")));
    h.dispatch(Action::ToggleLikeId("a".into()));
    h.dispatch(Action::CreatePlaylist("New".into()));
    h.dispatch(Action::RenamePlaylist {
        id: "pl-1".into(),
        name: "Renamed".into(),
    });
    h.dispatch(Action::DeletePlaylist("pl-1".into()));
    // MovePlaylist carries a sidebar row index; the backend moves by id, so
    // the controller resolves index 0 against its library mirror.
    h.dispatch(Action::MovePlaylist { from: 0, to: 3 });
    h.dispatch(Action::AddToPlaylist {
        playlist_id: "pl-1".into(),
        track: track("c"),
    });
    h.dispatch(Action::RemoveFromPlaylist {
        playlist_id: "pl-1".into(),
        track_id: "a".into(),
    });
    h.dispatch(Action::ExportPlaylist("pl-1".into()));
    h.dispatch(Action::ImportPlaylistFile);
    h.dispatch(Action::ImportYtPlaylistUrl(
        "  https://x/playlist?list=PL1 ".into(),
    ));
    assert_eq!(
        h.take_calls(),
        vec![
            Call::ToggleLike("b".into()),
            // ToggleLikeId resolves through the cache of delivered tracks.
            Call::ToggleLike("a".into()),
            Call::CreatePlaylist("New".into()),
            Call::RenamePlaylist("pl-1".into(), "Renamed".into()),
            Call::DeletePlaylist("pl-1".into()),
            Call::MovePlaylist("pl-1".into(), 3),
            Call::AddToPlaylist("pl-1".into(), "c".into()),
            Call::RemoveFromPlaylist("pl-1".into(), "a".into()),
            Call::ExportPlaylist("pl-1".into()),
            Call::ImportPlaylist,
            Call::ImportYtPlaylist("https://x/playlist?list=PL1".into()),
        ]
    );
}

#[test]
fn an_unknown_toggle_like_id_reports_an_error_instead_of_guessing() {
    let mut h = Harness::new();
    h.dispatch(Action::ToggleLikeId("missing".into()));
    assert!(h.take_calls().is_empty());
    let events = h.poll();
    assert!(
        matches!(&events[..], [ControllerEvent::Error(message)] if message.contains("missing")),
        "unexpected events: {events:?}"
    );
}

#[test]
fn dispatch_maps_settings_update_commands() {
    let mut h = Harness::new();
    h.dispatch(Action::SetDiscordRpc(false));
    h.dispatch(Action::SetCrossfade(3.5));
    h.dispatch(Action::SetUpdateNotifications(false));
    h.dispatch(Action::CheckUpdate);
    h.dispatch(Action::OpenUrl("https://example.com".into()));
    assert_eq!(
        h.take_calls(),
        vec![
            Call::SetDiscordRpc(false),
            Call::SetCrossfade(3.5),
            Call::SetUpdateNotifications(false),
            Call::CheckUpdate,
            Call::OpenUrl("https://example.com".into()),
        ]
    );
}

#[test]
fn dispatch_maps_downloads_and_resolves_ids_through_the_track_cache() {
    let mut h = Harness::with_bootstrap(Bootstrap {
        library: library_with(&["a", "b"]),
        ..Bootstrap::default()
    });
    h.controller.bootstrap().expect("bootstrap");
    h.take_calls();

    h.dispatch(Action::DownloadTracks(vec!["a".into(), "b".into()]));
    h.dispatch(Action::RemoveDownloads(vec!["b".into()]));
    assert_eq!(
        h.take_calls(),
        vec![
            Call::DownloadTracks(vec!["a".into(), "b".into()]),
            Call::RemoveDownloads(vec!["b".into()]),
        ]
    );
}

#[test]
fn dispatch_keeps_ui_local_actions_off_the_backend() {
    let mut h = Harness::new();
    h.dispatch(Action::DismissUpdateBanner);
    h.dispatch(Action::WindowMinimize);
    h.dispatch(Action::Toast("hi".into()));
    h.dispatch(Action::SetOnline(true));
    h.dispatch(Action::SearchQueryChanged("typing".into()));
    h.dispatch(Action::SearchBlur);
    h.dispatch(Action::ImportYtPlaylist);
    h.dispatch(Action::BeginNewPlaylist);
    assert!(h.take_calls().is_empty());
}

#[test]
fn fallible_commands_surface_their_error_on_the_next_poll() {
    let mut h = Harness::new();
    h.fail_open_url();
    h.dispatch(Action::OpenUrl("javascript:alert(1)".into()));
    assert_eq!(
        h.take_calls(),
        vec![Call::OpenUrl("javascript:alert(1)".into())]
    );
    let events = h.poll();
    assert!(
        matches!(&events[..], [ControllerEvent::Error(message)] if message.contains("http")),
        "unexpected events: {events:?}"
    );
}

#[test]
fn a_rejected_playlist_name_is_toasted_instead_of_silently_dropped() {
    let mut h = Harness::new();
    h.fail_create_playlist();
    h.dispatch(Action::CreatePlaylist("   ".into()));
    assert_eq!(h.take_calls(), vec![Call::CreatePlaylist("   ".into())]);
    let events = h.poll();
    assert!(
        matches!(
            &events[..],
            [ControllerEvent::Error(message)] if message.contains("empty")
        ),
        "unexpected events: {events:?}"
    );
}

#[test]
fn open_current_artist_resolves_the_playing_tracks_first_linkable_artist() {
    let mut h = Harness::new();
    h.emit(TaskResult::PlayTracks(Ok(())));
    h.emit(TaskResult::PlayTrack(Ok(())));
    // The current track comes from backend broadcasts; drive the mapping
    // helper directly with the controller mirror seeded the same way.
    h.controller.current_track = Some(track("live"));
    h.dispatch(Action::OpenCurrentArtist);
    let events = h.poll();
    assert!(
        matches!(
            &events[..],
            [ControllerEvent::OpenArtist(id)] if id == "artist-live"
        ),
        "unexpected events: {events:?}"
    );
}

// ------------------------------------------------ search generations

#[test]
fn search_results_for_a_superseded_query_are_dropped() {
    let mut h = Harness::new();
    h.dispatch(Action::Search("first".into()));
    h.dispatch(Action::Search("second".into()));
    assert_eq!(
        h.take_calls(),
        vec![
            Call::SearchSongs(1, "first".into()),
            Call::SearchSongs(2, "second".into()),
        ]
    );

    // The first query's (slower) reply arrives after the second was issued.
    h.emit(TaskResult::SearchSongs {
        generation: 1,
        query: "first".into(),
        results: Ok(vec![track("old")]),
    });
    assert!(
        h.poll().is_empty(),
        "a superseded reply must not reach the UI"
    );

    h.emit(TaskResult::SearchSongs {
        generation: 2,
        query: "second".into(),
        results: Ok(vec![track("new")]),
    });
    let events = h.poll();
    assert!(
        matches!(
            &events[..],
            [ControllerEvent::SearchResults { query, results }]
                if query == "second" && results.as_ref().is_ok_and(|t| t.len() == 1)
        ),
        "unexpected events: {events:?}"
    );
}

#[test]
fn clearing_the_search_box_cancels_the_in_flight_search() {
    let mut h = Harness::new();
    h.dispatch(Action::Search("query".into()));
    h.controller.cancel_search();
    h.emit(TaskResult::SearchSongs {
        generation: 1,
        query: "query".into(),
        results: Ok(vec![track("a")]),
    });
    assert!(h.poll().is_empty());
}

#[test]
fn artist_and_album_tabs_are_lazily_fetched_once_per_search() {
    let mut h = Harness::new();
    h.dispatch(Action::SetSearchTab(SearchTab::Artists));
    assert!(h.take_calls().is_empty(), "no query yet, nothing to fetch");

    // A query submitted while the Artists tab is open fetches songs *and*
    // the active tab: the app just reset `artists` to `None`, so without
    // this the visible tab would sit on "Searching..." forever.
    h.dispatch(Action::Search("daft punk".into()));
    assert_eq!(
        h.take_calls(),
        vec![
            Call::SearchSongs(1, "daft punk".into()),
            Call::SearchArtists(1, "daft punk".into()),
        ]
    );

    // Re-selecting the active tab (or any tab already fetched for this
    // query) must not duplicate the request.
    h.dispatch(Action::SetSearchTab(SearchTab::Artists));
    h.dispatch(Action::SetSearchTab(SearchTab::Artists));
    h.dispatch(Action::SetSearchTab(SearchTab::Albums));
    h.dispatch(Action::SetSearchTab(SearchTab::Albums));
    assert_eq!(
        h.take_calls(),
        vec![Call::SearchAlbums(1, "daft punk".into())]
    );

    // A new query resets the lazy flags and refetches whichever tab is
    // active: Albums now, with the new generation.
    h.dispatch(Action::Search("kavinsky".into()));
    assert_eq!(
        h.take_calls(),
        vec![
            Call::SearchSongs(2, "kavinsky".into()),
            Call::SearchAlbums(2, "kavinsky".into()),
        ]
    );
    h.dispatch(Action::SetSearchTab(SearchTab::Artists));
    assert_eq!(
        h.take_calls(),
        vec![Call::SearchArtists(2, "kavinsky".into())]
    );

    // Switching back to the Artists tab is remembered across the next
    // submit too.
    h.dispatch(Action::Search("third".into()));
    assert_eq!(
        h.take_calls(),
        vec![
            Call::SearchSongs(3, "third".into()),
            Call::SearchArtists(3, "third".into()),
        ]
    );

    // Songs is the one tab that is already fetched by the query itself, so
    // it must not be requested twice.
    h.dispatch(Action::SetSearchTab(SearchTab::Songs));
    h.dispatch(Action::Search("fourth".into()));
    assert_eq!(h.take_calls(), vec![Call::SearchSongs(4, "fourth".into())]);
}

// -------------------------------------------------- page generations

#[test]
fn navigating_to_a_page_requests_it_at_the_current_generation() {
    let mut h = Harness::new();
    h.controller.navigate_view(&View::Artist("artist-1".into()));
    h.controller.navigate_view(&View::Album("album-1".into()));
    h.controller
        .navigate_view(&View::ArtistSongs("artist-1".into()));
    assert_eq!(
        h.take_calls(),
        vec![
            Call::LoadArtistPage(1, "artist-1".into()),
            Call::LoadAlbumPage(2, "album-1".into()),
            Call::LoadArtistSongs(3, "artist-1".into()),
        ]
    );
}

#[test]
fn a_page_reply_for_an_abandoned_view_is_dropped() {
    let mut h = Harness::new();
    h.controller.navigate_view(&View::Artist("artist-1".into()));
    // The user leaves before the page arrives.
    h.controller.navigate_view(&View::Home);
    h.emit(TaskResult::ArtistPage {
        generation: 1,
        artist_id: "artist-1".into(),
        result: Ok(ArtistPage {
            id: "artist-1".into(),
            name: "Artist One".into(),
            image: String::new(),
            description: None,
            subscribers: None,
            tracks: vec![track("a")],
            albums: Vec::new(),
            tracks_playlist_id: None,
        }),
    });
    assert!(h.poll().is_empty(), "the abandoned page must not land");

    // A fresh navigation to the same page still loads it.
    h.controller.navigate_view(&View::Artist("artist-1".into()));
    assert_eq!(
        h.take_calls(),
        vec![
            Call::LoadArtistPage(1, "artist-1".into()),
            // Generation 2 was consumed leaving the page; the new request is 3.
            Call::LoadArtistPage(3, "artist-1".into()),
        ]
    );
}

#[test]
fn a_duplicate_navigation_to_the_loading_page_does_not_refetch() {
    let mut h = Harness::new();
    h.controller.navigate_view(&View::Artist("artist-1".into()));
    h.controller.navigate_view(&View::Artist("artist-1".into()));
    assert_eq!(
        h.take_calls(),
        vec![Call::LoadArtistPage(1, "artist-1".into())]
    );
}

#[test]
fn retry_page_reissues_the_last_page_request() {
    let mut h = Harness::new();
    h.controller.navigate_view(&View::Album("album-1".into()));
    h.dispatch(Action::RetryPage);
    assert_eq!(
        h.take_calls(),
        vec![
            Call::LoadAlbumPage(1, "album-1".into()),
            Call::LoadAlbumPage(2, "album-1".into()),
        ]
    );
}

#[test]
fn a_failed_page_emits_a_page_error_and_retry_reissues_it() {
    let mut h = Harness::new();
    h.controller.navigate_view(&View::Artist("artist-1".into()));
    h.emit(TaskResult::ArtistPage {
        generation: 1,
        artist_id: "artist-1".into(),
        result: Err("could not load artist: offline".into()),
    });
    let events = h.poll();
    assert!(
        matches!(
            &events[..],
            [ControllerEvent::ArtistPage { generation: 1, artist_id, result: Err(message) }]
                if artist_id == "artist-1" && message.contains("offline")
        ),
        "a failed page load must reach the UI as a retryable page result, \
         not a bare toast: {events:?}"
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, ControllerEvent::Error(_))),
        "the page error is carried by the page reply, not toasted"
    );
    h.take_calls();
    h.dispatch(Action::RetryPage);
    assert_eq!(
        h.take_calls(),
        vec![Call::LoadArtistPage(2, "artist-1".into())]
    );
}

// ------------------------------------------------------- cover dedupe

#[test]
fn cover_requests_are_deduplicated_while_in_flight() {
    let mut h = Harness::new();
    h.controller.request_cover("https://img/1".into());
    h.controller.request_cover("https://img/1".into());
    h.controller.request_cover("  https://img/1  ".into());
    h.controller.request_cover("https://img/2".into());
    assert_eq!(
        h.take_calls(),
        vec![
            Call::FetchCover("https://img/1".into()),
            Call::FetchCover("https://img/2".into()),
        ]
    );
}

#[test]
fn a_failed_cover_is_not_requested_again() {
    let mut h = Harness::new();
    h.controller.request_cover("https://img/bad".into());
    h.take_calls();
    h.emit(TaskResult::Cover {
        url: "https://img/bad".into(),
        result: Err("image fetch failed: offline".into()),
    });
    let events = h.poll();
    assert!(
        matches!(
            &events[..],
            [ControllerEvent::CoverFailed(url)] if url == "https://img/bad"
        ),
        "unexpected events: {events:?}"
    );

    h.controller.request_cover("https://img/bad".into());
    assert!(
        h.take_calls().is_empty(),
        "a known-bad URL is not refetched"
    );
    let events = h.poll();
    assert!(
        matches!(
            &events[..],
            [ControllerEvent::CoverFailed(url)] if url == "https://img/bad"
        ),
        "the UI is told to render the fallback again: {events:?}"
    );
}

#[test]
fn a_decoded_cover_is_emitted_and_a_later_request_may_refetch() {
    let mut h = Harness::new();
    h.controller.request_cover("https://img/good".into());
    h.take_calls();
    h.emit(TaskResult::Cover {
        url: "https://img/good".into(),
        result: Ok(CoverImage {
            url: "https://img/good".into(),
            width: 2,
            height: 1,
            rgba: vec![0; 8],
        }),
    });
    let events = h.poll();
    assert!(
        matches!(
            &events[..],
            [ControllerEvent::Cover(image)] if image.url == "https://img/good"
        ),
        "unexpected events: {events:?}"
    );
    // The tracker no longer considers it in flight; a re-request is allowed
    // (the UI's texture cache normally prevents one).
    h.controller.request_cover("https://img/good".into());
    assert_eq!(
        h.take_calls(),
        vec![Call::FetchCover("https://img/good".into())]
    );
}

#[test]
fn preview_mode_fails_cover_requests_immediately() {
    let mut controller = Controller::new_preview();
    controller.request_cover("https://img/1".into());
    assert!(matches!(
        &controller.poll()[..],
        [ControllerEvent::CoverFailed(url)] if url == "https://img/1"
    ));
}

// --------------------------------------------------- other completions

#[test]
fn a_failed_async_playback_is_reported_as_an_error() {
    let mut h = Harness::new();
    h.emit(TaskResult::PlayTracks(Err("nothing to play".into())));
    let events = h.poll();
    assert!(
        matches!(&events[..], [ControllerEvent::Error(message)] if message == "nothing to play"),
        "unexpected events: {events:?}"
    );
}

#[test]
fn a_yt_playlist_import_navigates_and_toasts_like_the_old_ui() {
    let mut h = Harness::new();
    h.emit(TaskResult::ImportYtPlaylist(Ok(Playlist {
        id: "pl-9".into(),
        name: "Mix".into(),
        tracks: vec![track("a"), track("b")],
    })));
    let events = h.poll();
    assert!(
        matches!(
            &events[..],
            [
                ControllerEvent::OpenPlaylist(id),
                ControllerEvent::Notice(message)
            ] if id == "pl-9" && message.contains("Mix") && message.contains("2 songs")
        ),
        "unexpected events: {events:?}"
    );
}

#[test]
fn reachability_changes_emit_online_only_on_transitions() {
    let mut h = Harness::new();
    h.emit(TaskResult::Online(true));
    h.emit(TaskResult::Online(true));
    h.emit(TaskResult::Online(false));
    let events = h.poll();
    assert!(
        matches!(
            &events[..],
            [
                ControllerEvent::Online(true),
                ControllerEvent::Online(false)
            ]
        ),
        "unexpected events: {events:?}"
    );
}

#[test]
fn bootstrap_seeds_the_mirrors_and_runs_the_launch_update_check() {
    let mut h = Harness::with_bootstrap(Bootstrap {
        library: library_with(&["a"]),
        volume: 0.4,
        track: Some(track("a")),
        queue: QueueSnapshot {
            tracks: vec![track("a"), track("b")],
            ..QueueSnapshot::default()
        },
        update_notifications: true,
        ..Bootstrap::default()
    });
    h.controller.bootstrap().expect("bootstrap");
    let calls = h.take_calls();
    assert_eq!(
        calls,
        vec![Call::CheckUpdate],
        "launch update check (notifications on); probes are suppressed in tests"
    );
    assert_eq!(h.controller.volume, 0.4);
    assert_eq!(
        h.controller.current_track.as_ref().map(|t| t.id.as_str()),
        Some("a")
    );
    assert!(
        h.controller.tracks.get("b").is_some(),
        "the restored queue's tracks are cached for id-only actions"
    );
}

#[test]
fn the_reachability_probe_runs_on_the_interval_and_reports_transitions() {
    let mut h = Harness::new();
    h.controller.probe_in_flight = false;
    h.controller.last_probe = None;
    // First poll is due: one probe, no result yet.
    assert!(h.poll().is_empty());
    assert_eq!(h.take_calls(), vec![Call::CheckOnline]);

    // A result arrives; the transition is emitted once.
    h.emit(TaskResult::Online(false));
    h.emit(TaskResult::Online(false));
    let events = h.poll();
    assert!(
        matches!(&events[..], [ControllerEvent::Online(false)]),
        "unexpected events: {events:?}"
    );
    // Not due again inside the interval.
    assert!(h.poll().is_empty());
    assert!(h.take_calls().is_empty(), "no probe storm");
}

#[test]
fn a_track_cache_insert_updates_metadata_in_place() {
    let mut cache = TrackCache::default();
    let mut first = track("a");
    first.title = "Old".into();
    cache.insert(first);
    let mut updated = track("a");
    updated.title = "New".into();
    cache.insert(updated);
    assert_eq!(cache.len(), 1);
    assert_eq!(cache.get("a").map(|t| t.title.as_str()), Some("New"));
}
