//! Backend-broadcast → [`ControllerEvent`](super::ControllerEvent) mapping tests (`backend` feature).

use super::*;
use rift::BackendEvent;
use rift_types::RepeatMode;
use std::sync::mpsc;

fn controller() -> Controller {
    let (_sender, receiver) = mpsc::channel();
    Controller::base(Box::new(InertCommands), receiver, true)
}

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

/// Every backend broadcast keeps the old `rift://…` event's payload and maps
/// to exactly one [`ControllerEvent`]; this is the porting contract from the
/// legacy web event handlers, pinned rather than implied.
#[test]
fn every_backend_event_maps_one_to_one() {
    let mut controller = controller();

    let mapped = controller.absorb_backend_event(BackendEvent::Track(track()));
    assert!(matches!(mapped, ControllerEvent::Track(Some(t)) if t.id == "id"));
    assert_eq!(
        controller.current_track.as_ref().map(|t| t.id.as_str()),
        Some("id")
    );

    let mapped = controller.absorb_backend_event(BackendEvent::State(PlaybackState::Playing));
    assert!(matches!(
        mapped,
        ControllerEvent::PlaybackState(PlaybackState::Playing)
    ));

    let snapshot = QueueSnapshot {
        tracks: vec![track()],
        current: Some(0),
        shuffle: false,
        repeat: RepeatMode::Off,
        source: Some("liked".into()),
    };
    let mapped = controller.absorb_backend_event(BackendEvent::Queue(snapshot.clone()));
    assert!(matches!(mapped, ControllerEvent::Queue(q) if q == snapshot));

    let progress = Progress {
        position: 1.0,
        duration: 2.0,
    };
    let mapped = controller.absorb_backend_event(BackendEvent::Progress(progress));
    assert!(matches!(mapped, ControllerEvent::Progress(p) if p == progress));

    let mut library = Library::default();
    library.liked.push(track());
    let mapped = controller.absorb_backend_event(BackendEvent::Library(library.clone()));
    assert!(matches!(mapped, ControllerEvent::Library(l) if l == library));
    // The library mirror feeds id-resolved actions (MovePlaylist etc.).
    assert_eq!(controller.library.liked.len(), 1);

    let downloads = DownloadState::default();
    let mapped = controller.absorb_backend_event(BackendEvent::Downloads(downloads.clone()));
    assert!(matches!(mapped, ControllerEvent::Downloads(d) if d == downloads));

    // Per-chunk progress updates the mirror and is forwarded as a
    // lightweight event (no whole-state clone).
    let mapped = controller.absorb_backend_event(BackendEvent::DownloadProgress {
        id: "t1".into(),
        permille: 420,
    });
    assert!(
        matches!(
            mapped,
            ControllerEvent::DownloadProgress { ref id, permille: 420 } if id == "t1"
        ),
        "download progress is forwarded as id + permille"
    );
    assert_eq!(controller.downloads.progress.get("t1"), Some(&420));

    let mapped = controller.absorb_backend_event(BackendEvent::Error("boom".into()));
    assert!(matches!(mapped, ControllerEvent::Error(m) if m == "boom"));

    let mapped = controller.absorb_backend_event(BackendEvent::Notice("hi".into()));
    assert!(matches!(mapped, ControllerEvent::Notice(m) if m == "hi"));

    let mapped = controller.absorb_backend_event(BackendEvent::OpenPlaylist("pl".into()));
    assert!(matches!(mapped, ControllerEvent::OpenPlaylist(id) if id == "pl"));
}
