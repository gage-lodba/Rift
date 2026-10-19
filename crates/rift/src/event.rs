//! The backend's event bus.
//!
//! The previous desktop shell used to push updates to the webview with
//! `app.emit("rift://track", payload)`. The native backend replaces that with
//! **one** unbounded channel of [`BackendEvent`] values; the application drains
//! it from the UI thread once per frame with [`BackendEvents::try_recv`] (or
//! parks a thread on [`BackendEvents::blocking_recv`]).
//!
//! Producer side: [`EventSender`] is cloneable, `Send + Sync`, and its `send` is
//! non-blocking, so it can be used from the synchronous command wrappers, the
//! audio-thread pump, background download jobs and the media/Discord threads
//! alike.
//!
//! The variants keep the old event payloads **1:1**, so the app crate ports
//! straight across from the legacy web event handlers:
//!
//! | Old shell event | Event | Payload |
//! |---|---|---|
//! | `rift://track` | [`BackendEvent::Track`] | [`rift_types::Track`] |
//! | `rift://state` | [`BackendEvent::State`] | [`rift_types::PlaybackState`] |
//! | `rift://queue` | [`BackendEvent::Queue`] | [`rift_types::QueueSnapshot`] |
//! | `rift://progress` | [`BackendEvent::Progress`] | [`rift_types::Progress`] |
//! | `rift://library` | [`BackendEvent::Library`] | [`rift_types::Library`] |
//! | `rift://downloads` | [`BackendEvent::Downloads`] | [`rift_types::DownloadState`] |
//! | `rift://error` | [`BackendEvent::Error`] | `String` (toast) |
//! | `rift://notice` | [`BackendEvent::Notice`] | `String` (toast) |
//! | `rift://open_playlist` | [`BackendEvent::OpenPlaylist`] | `String` (playlist id) |

use rift_types::{DownloadState, Library, PlaybackState, Progress, QueueSnapshot, Track};
use tokio::sync::mpsc;

/// One update from the backend to the application.
///
/// These are *broadcasts*, not replies: a command's return value is the reply
/// (e.g. `search` returns its tracks directly), while every change that the UI
/// must observe arrives here. A command may emit several events before it
/// returns.
#[derive(Debug, Clone, PartialEq)]
pub enum BackendEvent {
    /// The now-playing track changed (also emitted when credit enrichment
    /// updates the current row's stored metadata).
    Track(Track),
    /// Playback state changed: stopped / loading / playing / paused.
    State(PlaybackState),
    /// The queue, current index, shuffle or repeat mode changed.
    Queue(QueueSnapshot),
    /// Periodic position tick (from the audio thread, several times a second
    /// while playing; also emitted on seek and when a track starts).
    Progress(Progress),
    /// The persisted library changed (likes, playlists, recents).
    Library(Library),
    /// Offline-download status changed.
    Downloads(DownloadState),
    /// A user-facing error toast.
    Error(String),
    /// A user-facing informational toast.
    Notice(String),
    /// Ask the UI to navigate to a playlist id (emitted after an import driven
    /// from the backend).
    OpenPlaylist(String),
}

impl BackendEvent {
    /// The legacy event name this variant replaces.
    ///
    /// Kept for parity, logging and tests: the app crate migrates from the old
    /// web handlers by event name, so having the old name visible makes the
    /// mapping explicit rather than implied.
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            BackendEvent::Track(_) => "rift://track",
            BackendEvent::State(_) => "rift://state",
            BackendEvent::Queue(_) => "rift://queue",
            BackendEvent::Progress(_) => "rift://progress",
            BackendEvent::Library(_) => "rift://library",
            BackendEvent::Downloads(_) => "rift://downloads",
            BackendEvent::Error(_) => "rift://error",
            BackendEvent::Notice(_) => "rift://notice",
            BackendEvent::OpenPlaylist(_) => "rift://open_playlist",
        }
    }
}

/// Cloneable producer side of the event bus.
///
/// Every send is best-effort: a closed bus means the application is shutting
/// down, which is not an error worth propagating through playback code.
#[derive(Clone)]
pub(crate) struct EventSender(mpsc::UnboundedSender<BackendEvent>);

impl EventSender {
    /// Create the bus, returning the producer side plus the application's
    /// receiver.
    pub(crate) fn channel() -> (Self, BackendEvents) {
        let (tx, rx) = mpsc::unbounded_channel();
        (Self(tx), BackendEvents(rx))
    }

    /// Publish an event. Returns `false` once the receiver has been dropped.
    pub(crate) fn send(&self, event: BackendEvent) -> bool {
        self.0.send(event).is_ok()
    }
}

/// The application's end of the event bus.
///
/// The immediate-mode app polls this once per frame:
///
/// ```text
/// while let Some(event) = self.events.try_recv() {
///     controller.apply(event);
/// }
/// ```
///
/// It is `Send`, so a platform other than the UI thread can also own it, and it
/// offers both a synchronous [`try_recv`](BackendEvents::try_recv) (UI thread)
/// and a blocking [`blocking_recv`](BackendEvents::blocking_recv) (dedicated
/// pump thread).
pub struct BackendEvents(mpsc::UnboundedReceiver<BackendEvent>);

impl BackendEvents {
    /// Receive one event if one is queued. Never blocks; safe to call from the
    /// UI thread every frame.
    pub fn try_recv(&mut self) -> Option<BackendEvent> {
        self.0.try_recv().ok()
    }

    /// Drain everything currently queued, newest last. Convenience for a
    /// per-frame pump; equivalent to looping over [`try_recv`](Self::try_recv).
    pub fn drain(&mut self) -> Vec<BackendEvent> {
        let mut events = Vec::new();
        while let Some(event) = self.try_recv() {
            events.push(event);
        }
        events
    }

    /// Block until an event arrives. Returns `None` once the backend is gone
    /// (shutdown), which is the pump thread's exit condition.
    pub fn blocking_recv(&mut self) -> Option<BackendEvent> {
        self.0.blocking_recv()
    }

    /// Async receive for an async event loop.
    pub async fn recv(&mut self) -> Option<BackendEvent> {
        self.0.recv().await
    }

    /// Whether the backend has been dropped and no further events can arrive.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.0.is_closed()
    }
}

impl std::fmt::Debug for BackendEvents {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BackendEvents").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rift_types::RepeatMode;

    fn track() -> Track {
        Track {
            id: "id".into(),
            title: "Title".into(),
            artist: "Artist".into(),
            album: None,
            duration: Some(1),
            cover: String::new(),
            artists: Vec::new(),
            album_id: None,
        }
    }

    fn sample_events() -> Vec<BackendEvent> {
        vec![
            BackendEvent::Track(track()),
            BackendEvent::State(PlaybackState::Playing),
            BackendEvent::Queue(QueueSnapshot {
                tracks: vec![track()],
                current: Some(0),
                shuffle: false,
                repeat: RepeatMode::Off,
                source: None,
            }),
            BackendEvent::Progress(Progress {
                position: 1.0,
                duration: 2.0,
            }),
            BackendEvent::Library(Library::default()),
            BackendEvent::Downloads(DownloadState::default()),
            BackendEvent::Error("boom".into()),
            BackendEvent::Notice("hi".into()),
            BackendEvent::OpenPlaylist("pl".into()),
        ]
    }

    /// Each variant still names the legacy event it replaced; this table is the
    /// porting contract for the app crate, so it is pinned rather than implied.
    #[test]
    fn every_variant_keeps_its_old_event_name() {
        let names: Vec<&str> = sample_events().iter().map(BackendEvent::name).collect();
        assert_eq!(
            names,
            [
                "rift://track",
                "rift://state",
                "rift://queue",
                "rift://progress",
                "rift://library",
                "rift://downloads",
                "rift://error",
                "rift://notice",
                "rift://open_playlist",
            ]
        );
    }

    /// Events are delivered in order and without loss, and the app-side helpers
    /// see exactly what the producers sent.
    #[test]
    fn events_round_trip_in_order() {
        let (tx, mut rx) = EventSender::channel();
        let sent = sample_events();
        for event in &sent {
            assert!(tx.send(event.clone()), "the bus is open");
        }
        assert_eq!(rx.drain(), sent);
        assert_eq!(rx.try_recv(), None, "the queue is empty");
    }

    /// A dropped receiver must not make producers panic: the backend keeps
    /// running during shutdown until its threads are told to stop.
    #[test]
    fn sending_to_a_dropped_receiver_is_a_false_not_a_panic() {
        let (tx, rx) = EventSender::channel();
        drop(rx);
        assert!(!tx.send(BackendEvent::Notice("too late".into())));
    }

    #[test]
    fn the_receiver_is_send_so_it_can_live_on_a_pump_thread() {
        fn assert_send<T: Send>() {}
        assert_send::<BackendEvents>();
        assert_send::<EventSender>();
        fn assert_sync<T: Sync>() {}
        assert_sync::<EventSender>();
    }
}
