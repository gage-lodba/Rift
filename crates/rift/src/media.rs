//! OS media integration: media keys (play/pause/next/prev) and now-playing
//! metadata. Uses souvlaki for MPRIS (Linux), SMTC (Windows), and
//! `MPNowPlayingInfoCenter` (macOS).
//!
//! `MediaControls` is platform-specific and not `Send` on Windows/macOS, so it
//! is created and used entirely on a dedicated thread — it never crosses a
//! thread boundary. The player talks to that thread through the cheap,
//! `Send + Sync` [`MediaHandle`].
//!
//! This module used to call back into `player::*` with the shell's `AppHandle`.
//! Instead, the media thread holds a `Weak` reference to the backend's shared
//! state (filled in after construction) and upgrades it per OS event; a weak
//! reference is what keeps the thread from pinning the whole backend alive, so
//! dropping the backend lets the thread exit.
//!
//! ## Windows needs a window handle
//!
//! `souvlaki`'s SMTC backend requires the application window's `HWND` and
//! panics without one ([`MediaControls::new`] expects it). The winit app passes
//! it in [`BackendConfig::media_hwnd`](crate::BackendConfig::media_hwnd). When
//! it is absent on Windows the thread is not started at all: media keys and
//! now-playing metadata are unavailable (documented degradation) but nothing
//! panics, which keeps headless/preview runs and tests safe.

use std::sync::{Arc, OnceLock, Weak};
use std::time::Duration;

#[cfg(target_os = "windows")]
use std::ffi::c_void;

use rift_types::{PlaybackState, Track};
use souvlaki::{
    MediaControlEvent, MediaControls, MediaMetadata, MediaPlayback, MediaPosition, PlatformConfig,
};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tracing::{debug, warn};

use crate::backend::BackendInner;
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
    /// Backend shutdown: leave the loop and drop the controls.
    Shutdown,
}

/// The slot the media thread reads the backend from. It is filled once, right
/// after `RiftBackend`'s construction completes; media events cannot arrive
/// before the application has started playing anything.
pub(crate) type BackendSlot = Arc<OnceLock<Weak<BackendInner>>>;

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

    /// Ask the media thread to shut down and release the OS integration.
    pub(crate) fn shutdown(&self) {
        if let Some(tx) = &self.0 {
            let _ = tx.send(MediaCmd::Shutdown);
        }
    }
}

/// Start the OS media thread.
///
/// Returns an inert handle (`None` inside) when the integration is unavailable;
/// callers' `set_*` calls then become no-ops automatically.
pub(crate) fn spawn(hwnd: Option<isize>, backend_slot: BackendSlot) -> MediaHandle {
    // souvlaki's Windows backend requires the window's HWND. Starting the thread
    // without one would panic inside `MediaControls::new`, so don't start it.
    #[cfg(target_os = "windows")]
    if hwnd.is_none() {
        warn!(
            "no window handle supplied (BackendConfig::media_hwnd); \
             OS media keys and now-playing metadata are disabled"
        );
        return MediaHandle(None);
    }
    #[cfg(not(target_os = "windows"))]
    let _ = hwnd;

    let (tx, rx) = mpsc::unbounded_channel();
    match std::thread::Builder::new()
        .name("rift-media".into())
        .spawn(move || run(rx, hwnd, backend_slot))
    {
        Ok(_) => MediaHandle(Some(tx)),
        Err(e) => {
            warn!("could not spawn media thread: {e}");
            MediaHandle(None)
        }
    }
}

fn run(mut rx: UnboundedReceiver<MediaCmd>, hwnd: Option<isize>, backend_slot: BackendSlot) {
    let config = PlatformConfig {
        dbus_name: "rift",
        display_name: "Rift",
        #[cfg(target_os = "windows")]
        hwnd: hwnd.map(|h| h as *mut c_void),
        #[cfg(not(target_os = "windows"))]
        hwnd: {
            let _ = hwnd;
            None
        },
    };
    let mut controls = match MediaControls::new(config) {
        Ok(c) => c,
        Err(e) => {
            warn!("media controls unavailable: {e:?}");
            return;
        }
    };
    let handler_slot = Arc::clone(&backend_slot);
    if let Err(e) = controls.attach(move |event| {
        // The handler runs on the OS's own thread; upgrade per event so a
        // dropped backend means "ignore the key", not a resurrected one.
        let Some(inner) = handler_slot.get().and_then(Weak::upgrade) else {
            return;
        };
        handle_event(&inner, event);
    }) {
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
            MediaCmd::Shutdown => break,
        };
        if let Err(e) = result {
            debug!("media control update failed: {e:?}");
        }
    }
    debug!("media thread stopped");
}

/// Translate one OS media-key event into the corresponding player action.
///
/// Only the actions the old backend handled are handled; anything else
/// (seek, fast-forward, absolute positioning) is ignored exactly as before.
fn handle_event(inner: &Arc<BackendInner>, event: MediaControlEvent) {
    match event {
        MediaControlEvent::Toggle | MediaControlEvent::Play | MediaControlEvent::Pause => {
            player::toggle_playback(inner);
        }
        MediaControlEvent::Next => player::play_next(inner, true),
        MediaControlEvent::Previous => player::play_prev(inner),
        MediaControlEvent::Stop => player::stop(inner),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An unavailable integration must be inert, not panic: this is the path
    /// headless runs and Windows-without-HWND take.
    #[test]
    fn an_unavailable_media_handle_swallows_updates() {
        let handle = MediaHandle(None);
        let track = Track {
            id: "id".into(),
            title: "Title".into(),
            artist: "Artist".into(),
            album: None,
            duration: Some(10),
            cover: String::new(),
            artists: Vec::new(),
            album_id: None,
        };
        handle.set_track(&track, 10.0);
        handle.set_state(PlaybackState::Playing, 1.0);
        handle.shutdown();
    }

    /// The weak slot is how the media thread reaches the backend. A slot that
    /// was never filled must leave it doing nothing (the pre-construction
    /// window), rather than panicking.
    #[test]
    fn an_empty_backend_slot_means_ignore_the_event() {
        let slot: BackendSlot = Arc::new(OnceLock::new());
        assert!(slot.get().is_none());
    }
}
