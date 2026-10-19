//! Shared player state (`PlayerCore` / `PlayerShared`) and the broadcast +
//! persistence helpers that operate on it.

use std::sync::mpsc::Sender;
use std::sync::Mutex;

use rift_types::{PlaybackState, QueueSnapshot, RepeatMode, Track};
use rift_ytmusic::Client;
use tracing::warn;

use crate::audio::AudioCmd;
use crate::backend::BackendInner;
use crate::event::BackendEvent;
use crate::util::{LockExt, Persister};

pub struct PlayerCore {
    pub queue: Vec<Track>,
    pub current: Option<usize>,
    pub shuffle: bool,
    pub repeat: RepeatMode,
    pub state: PlaybackState,
    pub volume: f32,
    pub position: f64,
    pub duration: f64,
    /// Where the queue came from (e.g. "liked", "playlist:<id>").
    pub source: Option<String>,
    /// Ordered queue indices visited in the current shuffle cycle. Doubles as
    /// the "already played" set (so a cycle covers every track once) and as the
    /// back/forward history for Previous/Next under shuffle. Every entry names a
    /// live queue index: each queue edit carries the list through the same
    /// renumbering the edit applied to the queue (see `queue::remap_history`).
    pub shuffle_history: Vec<usize>,
    /// Position of the current track within `shuffle_history`. Stepping back
    /// with Previous moves it left; Next moves it right, replaying the existing
    /// order before drawing a new random track.
    pub shuffle_cursor: usize,
    /// Bumped on every play request; stale downloads check it and bail.
    pub generation: u64,
    /// Consecutive auto-play failures. Lets a queue of unplayable tracks stop
    /// instead of skipping forever; reset on any successful playback or a fresh
    /// user-initiated jump.
    pub failures: u32,
    /// Crossfade overlap in seconds; 0 disables it (a hard cut between tracks).
    pub crossfade: f64,
    /// Generation a crossfade has already been triggered for, so the position
    /// watcher fires at most once per track.
    pub crossfade_armed_for: Option<u64>,
    /// Next index drawn ahead of time by a crossfade. Consumed exactly once by
    /// whichever advance happens first — the crossfade commit or a natural
    /// end-of-track — so the (possibly random, under shuffle) pick isn't drawn
    /// twice.
    pub pending_next: Option<usize>,
}

impl Default for PlayerCore {
    fn default() -> Self {
        Self {
            queue: Vec::new(),
            current: None,
            shuffle: false,
            repeat: RepeatMode::Off,
            state: PlaybackState::Stopped,
            volume: 1.0,
            position: 0.0,
            duration: 0.0,
            source: None,
            shuffle_history: Vec::new(),
            shuffle_cursor: 0,
            generation: 0,
            failures: 0,
            crossfade: 0.0,
            crossfade_armed_for: None,
            pending_next: None,
        }
    }
}

pub struct PlayerShared {
    pub core: Mutex<PlayerCore>,
    pub audio: Sender<AudioCmd>,
    pub yt: Client,
    pub http: reqwest::Client,
    /// Where the queue snapshot is persisted so the session is restored on the
    /// next launch.
    pub playback_path: std::path::PathBuf,
    /// Background writer for the playback snapshot (keeps disk I/O off the
    /// command/playback threads).
    pub persist: Persister,
    /// The snapshot last handed to [`Persister`]. `emit_queue` is a broadcast
    /// that runs after almost every queue mutation *and* from inside the
    /// playback task, so several calls in a row carry an identical snapshot
    /// (notably a track's first play, which emits once for the change and again
    /// after credit enrichment). Comparing against this makes the repeats free
    /// instead of a fresh write burst.
    pub last_session: Mutex<Option<QueueSnapshot>>,
}

pub fn snapshot(core: &PlayerCore) -> QueueSnapshot {
    QueueSnapshot {
        tracks: core.queue.clone(),
        current: core.current,
        shuffle: core.shuffle,
        repeat: core.repeat,
        source: core.source.clone(),
    }
}

/// Broadcast the current queue/current/mode to the app and persist the session.
///
/// `emit_queue` is the universal "queue/current/mode changed" broadcast, so it's
/// the natural place to persist the session for the next launch. Position isn't
/// part of the snapshot, so progress ticks don't write. Preview mode never
/// persists, so a placeholder queue can't overwrite the user's real restored
/// session.
///
/// Serialization is deferred to the persister's own thread: `snapshot` above
/// clones every track (up to `MAX_FETCH_TRACKS`), and doing the JSON walk on
/// this thread too would put both a large clone and a disk write in the path
/// of every queue mutation.
pub(crate) fn emit_queue(backend: &BackendInner, shared: &PlayerShared) {
    let snap = {
        let core = shared.core.lock_safe();
        snapshot(&core)
    };
    if !crate::settings::preview_mode()
        && session_changed(&mut shared.last_session.lock_safe(), &snap)
    {
        if let Err(e) = shared
            .persist
            .write(shared.playback_path.clone(), snap.clone(), |s| {
                serde_json::to_vec(s).unwrap_or_default()
            })
        {
            warn!("could not queue the playback snapshot: {e}");
        }
    }
    backend.emit(BackendEvent::Queue(snap));
}

/// Record `snap` as the session to persist, reporting whether it differs from
/// the last one recorded. Pure, so the "skip the redundant write" rule that
/// keeps a track's first play from writing twice is testable on its own.
pub(crate) fn session_changed(last: &mut Option<QueueSnapshot>, snap: &QueueSnapshot) -> bool {
    if last.as_ref() == Some(snap) {
        return false;
    }
    *last = Some(snap.clone());
    true
}

/// Load a queue snapshot persisted by a previous session, if any.
pub fn load_snapshot(path: &std::path::Path) -> Option<QueueSnapshot> {
    let data = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&data).ok()
}

/// Broadcast a playback-state change and mirror it to the OS media session and
/// Discord Rich Presence.
pub(crate) fn emit_state(backend: &BackendInner, state: PlaybackState) {
    backend.emit(BackendEvent::State(state));
    // Mirror playback state to the OS media session and the Discord Rich
    // Presence. Both handles are no-ops when their integration is unavailable.
    let position = backend.player.core.lock_safe().position;
    backend.media.set_state(state, position);
    backend.discord.set_state(state, position);
}
