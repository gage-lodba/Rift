//! OS media integration: media keys (play/pause/next/prev) and now-playing
//! metadata. Uses souvlaki for MPRIS (Linux), SMTC (Windows), and
//! MPNowPlayingInfoCenter (macOS).
//!
//! `MediaControls` is platform-specific and not `Send` on Windows/macOS, so it
//! is created and used entirely on a dedicated thread — it never crosses a
//! thread boundary. The player talks to that thread through the cheap,
//! `Send + Sync` [`MediaHandle`], which is stored in `AppState`.

use std::time::Duration;

use rift_types::{PlaybackState, Track};
use souvlaki::{
    MediaControlEvent, MediaControls, MediaMetadata, MediaPlayback, MediaPosition, PlatformConfig,
};
use tauri::AppHandle;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tracing::{debug, warn};

use crate::player;

enum MediaCmd {
    Track {
        title: String,
        artist: String,
        album: Option<String>,
        cover: String,
        duration: f64,
    },
    State {
        state: PlaybackState,
        position: f64,
    },
}

/// Cloneable handle the player uses to update the OS media session.
#[derive(Clone)]
pub struct MediaHandle(Option<UnboundedSender<MediaCmd>>);

impl MediaHandle {
    pub fn set_track(&self, track: &Track, duration: f64) {
        if let Some(tx) = &self.0 {
            let _ = tx.send(MediaCmd::Track {
                title: track.title.clone(),
                artist: track.artist.clone(),
                album: track.album.clone(),
                cover: track.cover.clone(),
                duration,
            });
        }
    }

    pub fn set_state(&self, state: PlaybackState, position: f64) {
        if let Some(tx) = &self.0 {
            let _ = tx.send(MediaCmd::State { state, position });
        }
    }
}

pub fn spawn(app: AppHandle) -> MediaHandle {
    // Windows ties the SMTC to a window; capture its handle while we're on the
    // main thread (as a plain integer, which is Send) and rebuild the pointer
    // inside the media thread.
    let hwnd = window_hwnd(&app);
    let (tx, rx) = mpsc::unbounded_channel();
    match std::thread::Builder::new()
        .name("rift-media".into())
        .spawn(move || run(rx, app, hwnd))
    {
        Ok(_) => MediaHandle(Some(tx)),
        Err(e) => {
            warn!("could not spawn media thread: {e}");
            MediaHandle(None)
        }
    }
}

fn run(mut rx: UnboundedReceiver<MediaCmd>, app: AppHandle, hwnd: Option<usize>) {
    let config = PlatformConfig {
        dbus_name: "rift",
        display_name: "Rift",
        hwnd: hwnd.map(|h| h as *mut std::ffi::c_void),
    };
    let mut controls = match MediaControls::new(config) {
        Ok(c) => c,
        Err(e) => {
            warn!("media controls unavailable: {e:?}");
            return;
        }
    };
    let handler_app = app.clone();
    if let Err(e) = controls.attach(move |event| handle_event(&handler_app, event)) {
        warn!("could not attach media controls: {e:?}");
        return;
    }

    // souvlaki services the OS side on its own thread/run loop; here we just
    // apply updates as the player sends them (blocking off the tokio channel).
    while let Some(cmd) = rx.blocking_recv() {
        let result = match cmd {
            MediaCmd::Track {
                title,
                artist,
                album,
                cover,
                duration,
            } => controls.set_metadata(MediaMetadata {
                title: Some(&title),
                artist: Some(&artist),
                album: album.as_deref(),
                cover_url: (!cover.is_empty()).then_some(cover.as_str()),
                duration: (duration > 0.0).then(|| Duration::from_secs_f64(duration)),
            }),
            MediaCmd::State { state, position } => {
                let progress = Some(MediaPosition(Duration::from_secs_f64(position.max(0.0))));
                let playback = match state {
                    PlaybackState::Playing => MediaPlayback::Playing { progress },
                    PlaybackState::Paused | PlaybackState::Loading => {
                        MediaPlayback::Paused { progress }
                    }
                    PlaybackState::Stopped => MediaPlayback::Stopped,
                };
                controls.set_playback(playback)
            }
        };
        if let Err(e) = result {
            debug!("media control update failed: {e:?}");
        }
    }
}

fn handle_event(app: &AppHandle, event: MediaControlEvent) {
    match event {
        MediaControlEvent::Toggle | MediaControlEvent::Play | MediaControlEvent::Pause => {
            player::toggle_playback(app);
        }
        MediaControlEvent::Next => player::play_next(app, true),
        MediaControlEvent::Previous => player::play_prev(app),
        MediaControlEvent::Stop => player::stop(app),
        _ => {}
    }
}

/// The main window's `HWND` as an integer, for the Windows SMTC. `None`
/// everywhere else (souvlaki ignores `hwnd` off Windows).
#[cfg(target_os = "windows")]
fn window_hwnd(app: &AppHandle) -> Option<usize> {
    use tauri::Manager;
    app.get_webview_window("main")
        .and_then(|w| w.hwnd().ok())
        .map(|h| h.0 as usize)
}

#[cfg(not(target_os = "windows"))]
fn window_hwnd(_app: &AppHandle) -> Option<usize> {
    None
}
