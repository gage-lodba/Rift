//! Live end-to-end smoke through the controller → real `rift::RiftBackend`.
//!
//! These are **manual, `#[ignore]`-gated** tests: they construct the real backend
//! (audio/media/Discord threads, Tokio runtime, shared HTTP client) and use the
//! real `YouTube` Music network. They are not part of the CI/default
//! `cargo test -p rift-desktop` run; run them on demand:
//!
//! ```powershell
//! cargo test -p rift-desktop --features backend --test live_backend -- --ignored --nocapture --test-threads=1
//! ```
//!
//! Safety: every test sets `RIFT_DATA_DIR` to an empty throwaway directory under
//! `%TEMP%`, and restores the environment + deletes the directory when done. The
//! user's real `%APPDATA%\dev.jerimiah.rift` is never read or written (the
//! backend's data-dir precedence is config → `RIFT_DATA_DIR` → platform dir).
//!
//! The tests share a process-wide lock because `RIFT_DATA_DIR` is process-global,
//! and they run on the test thread (never inside a Tokio runtime: the backend
//! constructs its own runtime, which panics inside an existing one).

#![cfg(feature = "backend")]

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use rift_desktop::app::App;
use rift_desktop::controller::{Controller, ControllerEvent};
use rift_desktop::state::{Action, View};
use rift_types::{PlaybackState, Track};

/// Serialises the tests: `RIFT_DATA_DIR`/`RIFT_PREVIEW` are process-global.
static SERIAL: Mutex<()> = Mutex::new(());

/// A popular, stable query for the live search tests.
const SEARCH_QUERY: &str = "daft punk get lucky";

/// How long a live search may take before the smoke test fails.
const SEARCH_TIMEOUT: Duration = Duration::from_secs(45);
/// How long any other async reply may take.
const REPLY_TIMEOUT: Duration = Duration::from_secs(30);
/// How long a reply that performs real network I/O may take.
///
/// The backend's shared HTTP client already allows 30 s for a request, so waiting
/// exactly that long would race the client's own timeout on a slow connection;
/// leave headroom instead of turning a slow network into a flaky failure.
const NETWORK_TIMEOUT: Duration = Duration::from_secs(45);

// ---------------------------------------------------------------------------
// live harness: temp data dir + env guard + serialised backend lifetime
// ---------------------------------------------------------------------------

/// A unique temp directory, removed on drop.
fn unique_temp_dir(tag: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let unique = NEXT.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "rift-live-smoke-{tag}-{}-{unique}",
        std::process::id()
    ))
}

/// One live backend against a throwaway data directory.
struct Live {
    controller: Option<Controller>,
    data_dir: PathBuf,
    old_data_dir: Option<OsString>,
    old_preview: Option<OsString>,
    _lock: MutexGuard<'static, ()>,
}

impl Live {
    fn start(tag: &str) -> Self {
        let lock = SERIAL
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let old_data_dir = std::env::var_os("RIFT_DATA_DIR");
        let old_preview = std::env::var_os("RIFT_PREVIEW");
        // The whole point is the real backend, not the dev preview mode.
        std::env::remove_var("RIFT_PREVIEW");

        // A bare throwaway data directory: no settings seeding needed, the
        // backend's defaults fill everything in.
        let data_dir = unique_temp_dir(tag);
        std::fs::create_dir_all(&data_dir).expect("create the throwaway data directory");

        std::env::set_var("RIFT_DATA_DIR", &data_dir);
        let controller = Controller::new(None).expect("live backend should construct");
        assert!(controller.is_live(), "controller must be live");
        Self {
            controller: Some(controller),
            data_dir,
            old_data_dir,
            old_preview,
            _lock: lock,
        }
    }

    fn controller(&mut self) -> &mut Controller {
        self.controller.as_mut().expect("live controller")
    }

    /// Poll until `predicate` matches, collecting (and returning) every event
    /// seen. Times out with a compact event trace so failures are debuggable.
    fn poll_until(
        &mut self,
        timeout: Duration,
        mut predicate: impl FnMut(&ControllerEvent) -> bool,
    ) -> Result<Vec<ControllerEvent>, String> {
        let started = Instant::now();
        let mut seen = Vec::new();
        loop {
            for event in self.controller().poll() {
                if predicate(&event) {
                    seen.push(event);
                    return Ok(seen);
                }
                seen.push(event);
            }
            if started.elapsed() > timeout {
                return Err(format!(
                    "timed out after {timeout:?}; events seen: {:?}",
                    seen.iter().map(kind).collect::<Vec<_>>()
                ));
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Poll for the given duration, returning every event seen.
    fn poll_for(&mut self, duration: Duration) -> Vec<ControllerEvent> {
        let started = Instant::now();
        let mut seen = Vec::new();
        while started.elapsed() < duration {
            seen.extend(self.controller().poll());
            std::thread::sleep(Duration::from_millis(20));
        }
        seen
    }

    /// Shut the backend down but keep the throwaway data directory, so a test
    /// can construct a second backend on the same store (the restart path).
    fn shutdown_backend(&mut self) {
        if let Some(mut controller) = self.controller.take() {
            controller.shutdown();
        }
    }

    /// Construct a fresh controller/backend on the same data directory after
    /// [`shutdown_backend`](Self::shutdown_backend).
    fn restart(&mut self) {
        assert!(
            self.controller.is_none(),
            "restart needs shutdown_backend first"
        );
        self.controller = Some(Controller::new(None).expect("live backend should restart"));
    }

    /// Shut the backend down and measure how long it takes.
    fn shutdown(mut self) -> Duration {
        let started = Instant::now();
        if let Some(mut controller) = self.controller.take() {
            controller.shutdown();
            drop(controller);
        }
        started.elapsed()
    }
}

impl Drop for Live {
    fn drop(&mut self) {
        // A test that failed before `shutdown()` must still stop the backend's
        // threads before the temp directory is removed.
        if let Some(mut controller) = self.controller.take() {
            controller.shutdown();
        }
        match &self.old_data_dir {
            Some(value) => std::env::set_var("RIFT_DATA_DIR", value),
            None => std::env::remove_var("RIFT_DATA_DIR"),
        }
        match &self.old_preview {
            Some(value) => std::env::set_var("RIFT_PREVIEW", value),
            None => std::env::remove_var("RIFT_PREVIEW"),
        }
        // The backend's persist thread is stopped by shutdown; give the OS a
        // moment to release file handles before removing the scratch tree.
        let mut last_error = None;
        for _ in 0..20 {
            match std::fs::remove_dir_all(&self.data_dir) {
                Ok(()) => return,
                Err(error) => {
                    last_error = Some(error);
                    std::thread::sleep(Duration::from_millis(25));
                }
            }
        }
        eprintln!(
            "warning: could not remove the live-smoke data dir {}: {}",
            self.data_dir.display(),
            last_error.expect("an error was recorded")
        );
    }
}

fn kind(event: &ControllerEvent) -> &'static str {
    match event {
        ControllerEvent::Track(_) => "Track",
        ControllerEvent::PlaybackState(_) => "PlaybackState",
        ControllerEvent::Queue(_) => "Queue",
        ControllerEvent::Progress(_) => "Progress",
        ControllerEvent::Library(_) => "Library",
        ControllerEvent::Downloads(_) => "Downloads",
        ControllerEvent::Error(_) => "Error",
        ControllerEvent::Notice(_) => "Notice",
        ControllerEvent::OpenPlaylist(_) => "OpenPlaylist",
        ControllerEvent::SearchResults { .. } => "SearchResults",
        ControllerEvent::SearchArtists { .. } => "SearchArtists",
        ControllerEvent::SearchAlbums { .. } => "SearchAlbums",
        ControllerEvent::ArtistPage { .. } => "ArtistPage",
        ControllerEvent::ArtistSongs { .. } => "ArtistSongs",
        ControllerEvent::AlbumPage { .. } => "AlbumPage",
        ControllerEvent::UpdateStatus(_) => "UpdateStatus",
        ControllerEvent::Cover(_) => "Cover",
        ControllerEvent::CoverFailed(_) => "CoverFailed",
        ControllerEvent::OpenArtist(_) => "OpenArtist",
        ControllerEvent::Online(_) => "Online",
    }
}

/// A synthetic track for event-pump tests (no network involved).
fn smoke_track() -> Track {
    Track {
        id: "live-smoke-track".into(),
        title: "Live Smoke Track".into(),
        artist: "Smoke Artist".into(),
        album: Some("Smoke Album".into()),
        duration: Some(60),
        cover: String::new(),
        artists: Vec::new(),
        album_id: None,
    }
}

// ---------------------------------------------------------------------------
// 1. live bootstrap, event pump, clean shutdown
// ---------------------------------------------------------------------------

#[test]
#[ignore = "live backend smoke: needs temp data dir + audio device; run with --ignored"]
fn live_bootstrap_event_pump_and_clean_shutdown() {
    let mut live = Live::start("bootstrap");

    let started = Instant::now();
    let bootstrap = live.controller().bootstrap().expect("bootstrap");
    let bootstrap_elapsed = started.elapsed();
    eprintln!(
        "bootstrap in {bootstrap_elapsed:?}: data_dir={}, library={} liked/{} playlists/{} recent, \
         state={:?}, volume={}, discord_rpc={}, preview={}",
        live.data_dir.display(),
        bootstrap.library.liked.len(),
        bootstrap.library.playlists.len(),
        bootstrap.library.recently_played.len(),
        bootstrap.state,
        bootstrap.volume,
        bootstrap.discord_rpc,
        bootstrap.preview,
    );
    assert!(
        bootstrap.state == PlaybackState::Stopped,
        "nothing restored"
    );
    assert!(bootstrap.library.liked.is_empty(), "throwaway data dir");
    assert!(bootstrap.library.playlists.is_empty());
    assert!(bootstrap.volume.is_finite(), "volume is finite");
    assert!(
        (0.0..=1.0).contains(&bootstrap.volume),
        "volume in range: {}",
        bootstrap.volume
    );
    assert!(!bootstrap.preview, "real backend, not preview mode");

    // Event pump: a like toggle is backend-local (no network/audio) and must
    // produce a `Library` broadcast through the real channel.
    live.controller()
        .dispatch(Action::ToggleLike(smoke_track()));
    let pump_started = Instant::now();
    let events = live
        .poll_until(REPLY_TIMEOUT, |event| {
            matches!(event, ControllerEvent::Library(_))
        })
        .expect("the backend event bus should deliver the Library broadcast");
    let library = events
        .iter()
        .find_map(|event| match event {
            ControllerEvent::Library(library) => Some(library),
            _ => None,
        })
        .expect("a Library event");
    eprintln!(
        "event pump: 'Library' arrived after {:?} ({} events in total: {:?})",
        pump_started.elapsed(),
        events.len(),
        events.iter().map(kind).collect::<Vec<_>>()
    );
    assert_eq!(
        library.liked.len(),
        1,
        "the listener's library update contains the like"
    );

    // No audio is playing in this smoke test, so no Progress ticks are expected;
    // the pump has to stay non-blocking regardless (each poll must return fast).
    let poll_started = Instant::now();
    let idle_events = live.poll_for(Duration::from_millis(300));
    let idle_poll = poll_started.elapsed();
    eprintln!(
        "idle pump: {idle_poll:?} for 300 ms of polling ({} events; no audio output expected)",
        idle_events.len()
    );
    assert!(
        idle_poll < Duration::from_secs(2),
        "polling an idle backend must not block: {idle_poll:?}"
    );

    let shutdown = live.shutdown();
    eprintln!("clean shutdown in {shutdown:?}");
    assert!(
        shutdown < Duration::from_secs(10),
        "backend shutdown should be quick: {shutdown:?}"
    );
}

// ---------------------------------------------------------------------------
// 2. live search through the app (UI Action → state)
// ---------------------------------------------------------------------------

#[test]
#[ignore = "live backend smoke: performs a real YouTube Music search; run with --ignored"]
fn live_search_through_app_state() {
    let mut live = Live::start("search-app");
    let controller = live.controller.take().expect("live controller");
    let mut app = App::new(false, controller);
    assert!(
        app.controller.is_live(),
        "App::new keeps the live controller"
    );
    assert!(
        app.state.toast.is_none(),
        "bootstrap succeeded; error toast: {:?}",
        app.state.toast
    );
    assert!(app.state.library.liked.is_empty());

    app.dispatch_ui(Action::Search(SEARCH_QUERY.into()));
    assert!(app.state.search.busy, "search marked in flight");
    assert_eq!(app.state.view, View::Search);

    let started = Instant::now();
    while app.state.search.busy {
        app.tick();
        if let Some(toast) = &app.state.toast {
            if toast.is_error {
                panic!("search failed through the app bridge: {}", toast.message);
            }
        }
        assert!(
            started.elapsed() < SEARCH_TIMEOUT,
            "live search did not resolve within {SEARCH_TIMEOUT:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let elapsed = started.elapsed();
    let songs = &app.state.search.songs;
    eprintln!(
        "live search {:?} → {} songs in {elapsed:?}; shown_query={:?}; first={:?}",
        SEARCH_QUERY,
        songs.len(),
        app.state.search.shown_query,
        songs.first().map(|track| (&track.title, &track.artist))
    );
    assert_eq!(app.state.search.shown_query, SEARCH_QUERY);
    assert!(!songs.is_empty(), "live search returned results");
    assert!(
        songs.iter().all(|track| !track.id.is_empty()),
        "every result has an id"
    );

    let shutdown = {
        let mut controller = std::mem::replace(&mut app.controller, Controller::new_preview());
        let started = Instant::now();
        controller.shutdown();
        drop(controller);
        started.elapsed()
    };
    drop(app);
    let _ = live; // restores env + removes the temp dir in Drop
    eprintln!("clean shutdown in {shutdown:?}");
}

// ---------------------------------------------------------------------------
// 3. stale-search generation filtering over the live backend
// ---------------------------------------------------------------------------

#[test]
#[ignore = "live backend smoke: performs two real YouTube Music searches; run with --ignored"]
fn live_stale_search_generation_is_filtered() {
    let mut live = Live::start("search-gen");
    live.controller().bootstrap().expect("bootstrap");

    // Two searches back-to-back, without polling in between: the first reply is
    // stale by the time it can arrive, so the controller must drop it.
    let first = "daft punk";
    let second = "kavinsky nightcall";
    live.controller().dispatch(Action::Search(first.into()));
    live.controller().dispatch(Action::Search(second.into()));

    let started = Instant::now();
    let mut results = Vec::new();
    while started.elapsed() < SEARCH_TIMEOUT {
        for event in live.controller().poll() {
            if let ControllerEvent::SearchResults {
                query,
                results: reply,
            } = event
            {
                results.push((query, reply));
            }
        }
        if !results.is_empty() {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    eprintln!(
        "stale-generation search: {} SearchResults in {:?}; queries={:?}",
        results.len(),
        started.elapsed(),
        results
            .iter()
            .map(|(query, _)| query.as_str())
            .collect::<Vec<_>>()
    );
    assert_eq!(
        results.len(),
        1,
        "only the newest query may be emitted: {:?}",
        results
            .iter()
            .map(|(query, _)| query.as_str())
            .collect::<Vec<_>>()
    );
    let (query, reply) = &results[0];
    assert_eq!(
        query, second,
        "the emitted reply belongs to the newest query"
    );
    let songs = reply.as_ref().expect("the newest search succeeds");
    assert!(!songs.is_empty(), "live search returned results");
    eprintln!(
        "newest query {:?} returned {} songs (first: {:?})",
        query,
        songs.len(),
        songs.first().map(|track| &track.title)
    );

    let shutdown = live.shutdown();
    eprintln!("clean shutdown in {shutdown:?}");
}

// ---------------------------------------------------------------------------
// 4. cover fetch → decode → dedupe
// ---------------------------------------------------------------------------

#[test]
#[ignore = "live backend smoke: fetches a real cover image over the network; run with --ignored"]
fn live_cover_fetch_decode_and_dedupe() {
    let mut live = Live::start("cover");
    live.controller().bootstrap().expect("bootstrap");

    // Get a real cover URL from a live search result.
    live.controller()
        .dispatch(Action::Search(SEARCH_QUERY.into()));
    let events = live
        .poll_until(SEARCH_TIMEOUT, |event| {
            matches!(
                event,
                ControllerEvent::SearchResults { results: Ok(songs), .. } if !songs.is_empty()
            )
        })
        .expect("live search results");
    let cover_url = events
        .iter()
        .find_map(|event| match event {
            ControllerEvent::SearchResults {
                results: Ok(songs), ..
            } => songs
                .iter()
                .find(|track| !track.cover.is_empty())
                .map(|track| track.cover.clone()),
            _ => None,
        })
        .expect("the first result has a cover URL");
    eprintln!("search returned a cover URL: {cover_url}");

    // Request the same URL twice before polling: the second is de-duplicated
    // while the first is still in flight, so only ONE Cover event may arrive.
    let started = Instant::now();
    live.controller().request_cover(cover_url.clone());
    live.controller().request_cover(cover_url.clone());
    let events = live
        .poll_until(
            NETWORK_TIMEOUT,
            |event| matches!(event, ControllerEvent::Cover(image) if image.url == cover_url),
        )
        .expect("the cover should be fetched and decoded");
    let decode_elapsed = started.elapsed();
    let image = events
        .iter()
        .find_map(|event| match event {
            ControllerEvent::Cover(image) => Some(image),
            _ => None,
        })
        .expect("a Cover event");
    eprintln!(
        "cover decoded in {decode_elapsed:?}: {}x{} ({} RGBA bytes)",
        image.width,
        image.height,
        image.rgba.len()
    );
    assert!(
        image.width > 0 && image.height > 0,
        "decoded cover has pixels"
    );
    assert!(
        image.width <= 256 && image.height <= 256,
        "cover must be downscaled to <=256 px: {}x{}",
        image.width,
        image.height
    );
    assert_eq!(
        image.rgba.len(),
        image.width as usize * image.height as usize * 4,
        "tightly packed RGBA8"
    );
    assert!(
        image.rgba.chunks_exact(4).any(|pixel| pixel[3] > 0),
        "decoded pixels are not fully transparent"
    );

    // De-dupe check: nothing else for this URL should arrive afterwards.
    let extra = live.poll_for(Duration::from_millis(1200));
    let duplicate_covers = extra
        .iter()
        .filter(|event| matches!(event, ControllerEvent::Cover(image) if image.url == cover_url))
        .count();
    eprintln!(
        "dedupe: {duplicate_covers} extra Cover events for the same URL (extra events: {:?})",
        extra.iter().map(kind).collect::<Vec<_>>()
    );
    assert_eq!(duplicate_covers, 0, "the second request is de-duplicated");

    // Downscale evidence on a real >256 px source: YouTube thumbnail URLs carry
    // their size in a `=w<W>-h<H>-…` suffix, so ask for the 544 px variant. If
    // the URL does not follow that pattern the unit tests in `cover.rs` already
    // pin the cap, so this is evidence rather than the sole guarantee.
    if cover_url.contains("w120-h120") {
        let large_url = cover_url.replace("w120-h120", "w544-h544");
        eprintln!("requesting the large variant to prove downscaling: {large_url}");
        live.controller().request_cover(large_url.clone());
        let large_started = Instant::now();
        match live.poll_until(
            NETWORK_TIMEOUT,
            |event| matches!(event, ControllerEvent::Cover(image) if image.url == large_url),
        ) {
            Ok(events) => {
                let large = events
                    .iter()
                    .find_map(|event| match event {
                        ControllerEvent::Cover(image) => Some(image),
                        _ => None,
                    })
                    .expect("a Cover event");
                eprintln!(
                    "large cover decoded in {:?}: {}x{} — capped at 256: {}",
                    large_started.elapsed(),
                    large.width,
                    large.height,
                    large.width <= 256 && large.height <= 256
                );
                assert!(
                    large.width <= 256 && large.height <= 256,
                    "a >256 px source must be downscaled: {}x{}",
                    large.width,
                    large.height
                );
                assert_eq!(
                    large.rgba.len(),
                    large.width as usize * large.height as usize * 4
                );
            }
            Err(error) => eprintln!(
                "note: the 544 px thumbnail variant was unavailable ({error}); \
                 the 256 px cap is already unit-tested in cover.rs"
            ),
        }
    } else {
        eprintln!("note: cover URL has no w120-h120 suffix to derive a larger variant from");
    }

    // A dead URL fails gracefully and is not retried by the tracker (the second
    // request re-emits CoverFailed from the known-failed set, no fetch).
    let dead_url = "https://invalid.invalid/rift-live-smoke-cover.jpg";
    live.controller().request_cover(dead_url.into());
    let failures = live.poll_until(
        REPLY_TIMEOUT,
        |event| matches!(event, ControllerEvent::CoverFailed(url) if url == dead_url),
    );
    eprintln!(
        "dead cover URL reported CoverFailed: {:?}",
        failures.is_ok()
    );
    failures.expect("a dead URL reports CoverFailed");
    live.controller().request_cover(dead_url.into());
    let failures = live
        .poll_for(Duration::from_millis(600))
        .into_iter()
        .filter(|event| matches!(event, ControllerEvent::CoverFailed(url) if url == dead_url))
        .count();
    eprintln!("known-failed URL re-emitted CoverFailed {failures} time(s) with no new fetch");
    assert_eq!(
        failures, 1,
        "a known-failed URL is reported exactly once per request, not retried"
    );

    let shutdown = live.shutdown();
    eprintln!("clean shutdown in {shutdown:?}");
}

// ---------------------------------------------------------------------------
// 5. update check
// ---------------------------------------------------------------------------

#[test]
#[ignore = "live backend smoke: hits GitHub; run with --ignored"]
fn live_update_check() {
    let mut live = Live::start("update");
    live.controller().bootstrap().expect("bootstrap");

    // Update check: current version always reported; network failure degrades to
    // "latest unknown" rather than hanging, so only require a status event.
    live.controller().check_update();
    let update_started = Instant::now();
    let events = live
        .poll_until(NETWORK_TIMEOUT, |event| {
            matches!(event, ControllerEvent::UpdateStatus(_))
        })
        .expect("update-check reply");
    let status = events
        .iter()
        .find_map(|event| match event {
            ControllerEvent::UpdateStatus(status) => Some(status),
            _ => None,
        })
        .expect("an UpdateStatus event");
    eprintln!(
        "update check in {:?}: current={:?}, latest={:?}, update_available={}, url={:?}",
        update_started.elapsed(),
        status.current,
        status.latest,
        status.update_available,
        status.url
    );
    assert!(!status.current.is_empty(), "running version is reported");
    if status.latest.is_none() {
        eprintln!("note: GitHub release check degraded to 'latest unknown' (network/rate limit)");
    }

    let shutdown = live.shutdown();
    eprintln!("clean shutdown in {shutdown:?}");
}

// ---------------------------------------------------------------------------
// 7. native streaming end-to-end
// ---------------------------------------------------------------------------

/// One track through the whole native pipeline via the real controller/backend:
/// native playback, a download whose per-track progress arrives as `Downloads`
/// events, then a backend restart that discovers the offline copy and plays it
/// again.
///
/// This is the automated equivalent of the interactive Phase 4 pass for a single
/// track; the human pass still verifies what it sounds and looks like. It drives
/// the shipping native downloader — there is no other streaming backend.
#[test]
#[ignore = "live backend smoke: real network + audio device; run with --ignored"]
fn live_native_playback_download_progress_and_offline_restart() {
    /// A stable, well-known upload used by the other live probes.
    const ID: &str = "K0HSD_i2DvA";

    let mut live = Live::start("native-streaming");
    let track = Track {
        id: ID.into(),
        title: "Around the World".into(),
        artist: "Daft Punk".into(),
        album: None,
        duration: Some(242),
        cover: String::new(),
        artists: Vec::new(),
        album_id: None,
    };

    // 1. Playback through the real controller.
    live.controller().dispatch(Action::PlayTrack(track.clone()));
    let events = live
        .poll_until(NETWORK_TIMEOUT, |event| {
            matches!(
                event,
                ControllerEvent::PlaybackState(PlaybackState::Playing)
            )
        })
        .expect("native playback should reach Playing");
    assert!(
        events
            .iter()
            .any(|event| matches!(event, ControllerEvent::Track(Some(t)) if t.id == track.id)),
        "the playing track must surface before Playing: {:?}",
        events.iter().map(kind).collect::<Vec<_>>()
    );
    live.poll_until(
        NETWORK_TIMEOUT,
        |event| matches!(event, ControllerEvent::Progress(p) if p.position > 0.5),
    )
    .expect("playback position should advance");

    // 2. Download through the real controller; the in-flight progress map must
    // reach the app state before the completed state does.
    live.controller()
        .dispatch(Action::DownloadTracks(vec![track.id.clone()]));
    let mut saw_progress = false;
    let mut max_permille = 0u16;
    live.poll_until(NETWORK_TIMEOUT, |event| {
        if let ControllerEvent::Downloads(state) = event {
            if let Some(permille) = state.progress.get(&track.id) {
                saw_progress = true;
                max_permille = max_permille.max(*permille);
            }
            state.downloaded.contains(&track.id)
        } else {
            false
        }
    })
    .expect("download should complete");
    assert!(
        saw_progress,
        "a Downloads snapshot must carry the in-flight progress entry before the completed state"
    );
    assert!(
        max_permille <= 1000,
        "progress permille must stay clamped, saw {max_permille}"
    );
    let offline_file = live
        .data_dir
        .join("downloads")
        .join(format!("{}.m4a", track.id));
    let size = std::fs::metadata(&offline_file)
        .expect("the offline audio file should exist")
        .len();
    assert!(
        size > 100_000,
        "offline file suspiciously small: {size} bytes"
    );

    // 3. Restart on the same store: the startup scan must rediscover the copy,
    // and playing it must work again (fetch_track prefers the local file).
    live.shutdown_backend();
    live.restart();
    let bootstrap = live
        .controller()
        .bootstrap()
        .expect("bootstrap after restart");
    assert!(
        bootstrap.downloads.downloaded.contains(&track.id),
        "restart must rediscover the offline copy"
    );
    live.controller().dispatch(Action::PlayTrack(track.clone()));
    live.poll_until(NETWORK_TIMEOUT, |event| {
        matches!(
            event,
            ControllerEvent::PlaybackState(PlaybackState::Playing)
        )
    })
    .expect("offline playback should reach Playing");

    let shutdown = live.shutdown();
    eprintln!("native streaming e2e passed; shutdown in {shutdown:?}");
}
