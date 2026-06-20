//! Queue management and track playback orchestration.
//!
//! Tracks are resolved to stream URLs with rustypipe, downloaded fully into
//! memory (a typical m4a is 3–5 MB) and handed to the audio thread. A
//! generation counter guards against races when the user skips while a
//! download is still in flight.

use std::sync::mpsc::Sender;
use std::sync::Mutex;

use rift_types::{events, ArtistRef, PlaybackState, Progress, QueueSnapshot, RepeatMode, Track};
use rustypipe::client::RustyPipe;
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::mpsc::UnboundedReceiver;
use tracing::{debug, error, info};

use crate::audio::{AudioCmd, AudioEvent};
use crate::util::{LockExt, Persister};
use crate::AppState;

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
    /// back/forward history for Previous/Next under shuffle.
    pub shuffle_history: Vec<usize>,
    /// Position of the current track within `shuffle_history`. Stepping back
    /// with Previous moves it left; Next moves it right, replaying the existing
    /// order before drawing a new random track.
    pub shuffle_cursor: usize,
    /// Bumped on every play request; stale downloads check it and bail.
    pub generation: u64,
    /// Bumped whenever the queue is replaced; stale radio fills check it.
    pub epoch: u64,
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
            epoch: 0,
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
    pub rp: RustyPipe,
    pub http: reqwest::Client,
    /// Where the queue snapshot is persisted so the session is restored on the
    /// next launch.
    pub playback_path: std::path::PathBuf,
    /// Background writer for the playback snapshot (keeps disk I/O off the
    /// command/playback threads).
    pub persist: Persister,
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

pub fn emit_queue(app: &AppHandle, shared: &PlayerShared) {
    let snap = {
        let core = shared.core.lock_safe();
        snapshot(&core)
    };
    // emit_queue is the universal "queue/current/mode changed" broadcast, so
    // it's the natural place to persist the session for the next launch.
    // Position isn't part of the snapshot, so progress ticks don't write.
    if let Ok(json) = serde_json::to_vec(&snap) {
        shared.persist.write(shared.playback_path.clone(), json);
    }
    let _ = app.emit(events::QUEUE, &snap);
}

/// Load a queue snapshot persisted by a previous session, if any.
pub fn load_snapshot(path: &std::path::Path) -> Option<QueueSnapshot> {
    let data = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&data).ok()
}

pub fn emit_state(app: &AppHandle, state: PlaybackState) {
    let _ = app.emit(events::STATE, &state);
    // Mirror playback state to the OS media session (no-op off Linux) and the
    // Discord Rich Presence.
    let app_state = app.state::<AppState>();
    let position = app_state.player.core.lock_safe().position;
    app_state.media.set_state(state, position);
    app_state.discord.set_state(state, position);
}

/// Start playing the track at `index`, treating it as a fresh anchor for the
/// shuffle history. Used by direct jumps — clicking a track, starting a
/// collection, resuming a stopped track. Sequential and shuffle Next/Previous
/// go through [`play_next`]/[`play_prev`], which preserve the history instead.
pub fn play_index(app: &AppHandle, index: usize) {
    {
        let state = app.state::<AppState>();
        let mut core = state.player.core.lock_safe();
        if index >= core.queue.len() {
            return;
        }
        // A deliberate jump is a fresh start: give the queue a full retry budget.
        core.failures = 0;
        if core.shuffle {
            core.shuffle_history = vec![index];
            core.shuffle_cursor = 0;
        }
    }
    start_playback(app, index);
}

/// Resolve and play the track at `index`. Assumes the shuffle history has
/// already been positioned by the caller.
fn start_playback(app: &AppHandle, index: usize) {
    let state = app.state::<AppState>();
    let shared = state.player.clone();
    let library = state.library.clone();
    let downloads = state.downloads.clone();
    let app = app.clone();

    let (track, generation) = {
        let mut core = shared.core.lock_safe();
        let Some(track) = core.queue.get(index).cloned() else {
            return;
        };
        core.current = Some(index);
        core.generation += 1;
        // A hard start supersedes any crossfade the position watcher prefetched.
        core.pending_next = None;
        core.state = PlaybackState::Loading;
        core.position = 0.0;
        core.duration = track.duration.unwrap_or(0) as f64;
        (track, core.generation)
    };
    let _ = shared.audio.send(AudioCmd::Stop);

    info!(
        "playing \"{}\" by {} ({})",
        track.title, track.artist, track.id
    );
    let _ = app.emit(events::TRACK, &track);
    // Announce the track to Discord before the Loading state so the presence
    // reflects the new track immediately (the accurate duration backfills via
    // the OS media session once the stream resolves).
    app.state::<AppState>()
        .discord
        .set_track(&track, track.duration.unwrap_or(0) as f64);
    emit_state(&app, PlaybackState::Loading);
    emit_queue(&app, &shared);

    tauri::async_runtime::spawn(async move {
        match rift::fetch::fetch_track(&shared.rp, &shared.http, &downloads.dir, &track.id).await {
            Ok((data, duration)) => {
                {
                    let mut core = shared.core.lock_safe();
                    if core.generation != generation {
                        debug!("discarding stale download for {}", track.id);
                        return;
                    }
                    core.state = PlaybackState::Playing;
                    core.failures = 0;
                    if duration > 0.0 {
                        core.duration = duration;
                    }
                }
                let _ = shared.audio.send(AudioCmd::Play(data));
                emit_state(&app, PlaybackState::Playing);
                let dur = shared.core.lock_safe().duration;
                let _ = app.emit(
                    events::PROGRESS,
                    Progress {
                        position: 0.0,
                        duration: dur,
                    },
                );
                announce_track(&app, &shared, &library, index, track, dur).await;
            }
            Err(e) => {
                error!("failed to play {}: {e:#}", track.id);
                let skip = {
                    let mut core = shared.core.lock_safe();
                    if core.generation != generation {
                        return;
                    }
                    core.failures += 1;
                    // Skip a bad track instead of stalling the queue, but stop
                    // once we've failed a whole queue's worth in a row so an
                    // all-broken queue can't spin forever.
                    if core.failures < core.queue.len().max(1) as u32 {
                        true
                    } else {
                        core.state = PlaybackState::Stopped;
                        false
                    }
                };
                let _ = app.emit(
                    events::ERROR,
                    format!("Could not play \u{201c}{}\u{201d}: {e:#}", track.title),
                );
                if skip {
                    debug!("auto-skipping failed track {}", track.id);
                    play_next(&app, false);
                } else {
                    emit_state(&app, PlaybackState::Stopped);
                }
            }
        }
    });
}

/// Backfill per-artist credits (with channel IDs) for a track saved before
/// they were stored. Returns `true` if anything changed. Tracks that already
/// have credits, or whose lookup fails, are left untouched.
async fn enrich_track(rp: &RustyPipe, track: &mut Track) -> bool {
    if !track.artists.is_empty() {
        return false;
    }
    let details = match rp.query().music_details(&track.id).await {
        Ok(d) => d,
        Err(e) => {
            debug!("could not enrich {}: {e}", track.id);
            return false;
        }
    };
    let item = details.track;
    let artists: Vec<ArtistRef> = item
        .artists
        .iter()
        .map(|a| ArtistRef {
            id: a.id.clone(),
            name: a.name.clone(),
        })
        .collect();
    if artists.is_empty() {
        return false;
    }
    track.artists = artists;
    if track.album_id.is_none() {
        if let Some(al) = item.album {
            if track.album.is_none() {
                track.album = Some(al.name);
            }
            track.album_id = Some(al.id);
        }
    }
    true
}

/// Refresh the now-playing surfaces once a track is actually playing: publish
/// it to the OS media session, backfill artist credits (persisting them so list
/// rows become linkable), and record it in the library's recents. Shared by the
/// fresh-start ([`start_playback`]) and crossfade ([`begin_crossfade`]) paths.
async fn announce_track(
    app: &AppHandle,
    shared: &std::sync::Arc<PlayerShared>,
    library: &std::sync::Arc<Mutex<crate::library::LibraryStore>>,
    index: usize,
    mut track: Track,
    dur: f64,
) {
    app.state::<AppState>().media.set_track(&track, dur);

    if enrich_track(&shared.rp, &mut track).await {
        {
            let mut core = shared.core.lock_safe();
            if let Some(slot) = core.queue.get_mut(index) {
                if slot.id == track.id {
                    *slot = track.clone();
                }
            }
        }
        let _ = app.emit(events::TRACK, &track);
        emit_queue(app, shared);
    }

    let mut lib = library.lock_safe();
    lib.backfill_track(&track);
    lib.push_recent(track);
    let _ = app.emit(events::LIBRARY, &lib.data);
}

/// Begin a crossfade into the next track: draw it now, fetch it in the
/// background, and once its bytes are ready overlap it with the current track
/// via [`AudioCmd::Crossfade`]. Triggered by the position watcher as the current
/// track nears its end.
///
/// If the fetch is too slow and the current track ends first, the natural
/// end-of-track path advances normally (reusing the same drawn pick), and this
/// crossfade is discarded — a graceful fall back to a hard cut.
fn begin_crossfade(app: &AppHandle) {
    let state = app.state::<AppState>();
    let shared = state.player.clone();
    let library = state.library.clone();
    let downloads = state.downloads.clone();
    let app = app.clone();

    let (index, anchor_gen, fade) = {
        let mut core = shared.core.lock_safe();
        // Draw the next track now and remember it. pick_next advances the
        // shuffle bookkeeping; pending_next pins the pick so the actual advance
        // (here or on a natural end) reuses it instead of drawing again.
        let Some(next) = pick_next(&mut core, false) else {
            return;
        };
        core.pending_next = Some(next);
        (next, core.generation, core.crossfade)
    };

    info!("crossfading to queue index {index}");

    tauri::async_runtime::spawn(async move {
        let Some(track) = shared.core.lock_safe().queue.get(index).cloned() else {
            return;
        };
        match rift::fetch::fetch_track(&shared.rp, &shared.http, &downloads.dir, &track.id).await {
            Ok((data, duration)) => {
                let dur = {
                    let mut core = shared.core.lock_safe();
                    // Commit only if nothing advanced while we fetched: the
                    // generation is unchanged and our pick is still pending. If
                    // a natural end (or a skip) beat us, it already consumed the
                    // pick and bumped the generation — drop this crossfade.
                    if core.generation != anchor_gen || core.pending_next != Some(index) {
                        debug!("discarding stale crossfade for {}", track.id);
                        return;
                    }
                    // The crossfade armed while Playing, but the user may have
                    // paused (or cancelled to Stopped) during the prefetch.
                    // Committing now would call player.play() on the audio thread
                    // and resume against the user's wish, leaving state desynced.
                    // Leave the pick pending so the natural end-of-track path
                    // (after the user resumes) reuses it instead.
                    if core.state != PlaybackState::Playing {
                        debug!("crossfade target ready but playback is paused; deferring");
                        return;
                    }
                    core.pending_next = None;
                    core.current = Some(index);
                    core.state = PlaybackState::Playing;
                    core.generation += 1;
                    core.failures = 0;
                    core.position = 0.0;
                    core.duration = if duration > 0.0 {
                        duration
                    } else {
                        track.duration.unwrap_or(0) as f64
                    };
                    core.duration
                };

                let fade = std::time::Duration::from_secs_f64(fade.max(0.0));
                let _ = shared.audio.send(AudioCmd::Crossfade(data, fade));
                // Playback never left the Playing state, so only the track and
                // its surfaces need refreshing.
                let _ = app.emit(events::TRACK, &track);
                app.state::<AppState>().discord.set_track(&track, dur);
                emit_state(&app, PlaybackState::Playing);
                emit_queue(&app, &shared);
                let _ = app.emit(
                    events::PROGRESS,
                    Progress {
                        position: 0.0,
                        duration: dur,
                    },
                );
                announce_track(&app, &shared, &library, index, track, dur).await;
            }
            Err(e) => {
                // Leave the pick pending so the natural end-of-track retries it
                // (and its own skip logic handles a track that stays broken),
                // rather than redrawing and corrupting the shuffle cycle.
                debug!("crossfade prefetch failed for {}: {e:#}", track.id);
            }
        }
    });
}

fn rand_index(len: usize) -> usize {
    (rand::random::<u64>() as usize) % len
}

/// Decide the next queue index and advance the shuffle history. `manual` marks
/// a user-initiated Next (wraps at the end and overrides repeat-one); automatic
/// advancement respects repeat mode.
///
/// Under shuffle: if Previous stepped the cursor back into the history, Next
/// walks forward through that existing order; otherwise a track not yet played
/// this cycle is drawn at random and appended, so every track plays once before
/// any repeats.
fn pick_next(core: &mut PlayerCore, manual: bool) -> Option<usize> {
    let len = core.queue.len();
    let cur = core.current?;
    if len == 0 {
        return None;
    }
    // A crossfade may have already drawn the next track (advancing the shuffle
    // bookkeeping as it did). Honour that pick so it isn't drawn twice.
    if let Some(i) = core.pending_next.take() {
        if i < len {
            return Some(i);
        }
    }
    if !manual && core.repeat == RepeatMode::One {
        return Some(cur);
    }
    if core.shuffle && len > 1 {
        // Previous left the cursor behind the front: replay forward.
        if core.shuffle_cursor + 1 < core.shuffle_history.len() {
            core.shuffle_cursor += 1;
            return Some(core.shuffle_history[core.shuffle_cursor]);
        }
        let pool: Vec<usize> = (0..len)
            .filter(|i| !core.shuffle_history.contains(i))
            .collect();
        let pick = if pool.is_empty() {
            // Every track has played this cycle.
            if !manual && core.repeat != RepeatMode::All {
                return None;
            }
            // Start a fresh cycle, keeping the current track out of the running
            // so it isn't repeated back-to-back.
            core.shuffle_history.clear();
            let fresh: Vec<usize> = (0..len).filter(|i| *i != cur).collect();
            fresh[rand_index(fresh.len())]
        } else {
            pool[rand_index(pool.len())]
        };
        core.shuffle_history.push(pick);
        core.shuffle_cursor = core.shuffle_history.len() - 1;
        return Some(pick);
    }
    if cur + 1 < len {
        Some(cur + 1)
    } else if manual || core.repeat == RepeatMode::All {
        Some(0)
    } else {
        None
    }
}

/// Toggle play/pause, resume a stopped track, or cancel an in-flight load.
/// Shared by the `toggle_play` command and the OS media keys.
pub fn toggle_playback(app: &AppHandle) {
    let state = app.state::<AppState>();
    enum Action {
        None,
        Emit(PlaybackState),
        Replay(usize),
        Cancel,
    }
    let action = {
        let mut core = state.player.core.lock_safe();
        match core.state {
            PlaybackState::Playing => {
                let _ = state.player.audio.send(AudioCmd::Pause);
                core.state = PlaybackState::Paused;
                Action::Emit(PlaybackState::Paused)
            }
            PlaybackState::Paused => {
                let _ = state.player.audio.send(AudioCmd::Resume);
                core.state = PlaybackState::Playing;
                Action::Emit(PlaybackState::Playing)
            }
            PlaybackState::Stopped => match core.current {
                Some(i) => Action::Replay(i),
                None => Action::None,
            },
            // Cancel an in-flight load: bump the generation so the pending
            // fetch is discarded when it lands, and stop.
            PlaybackState::Loading => {
                core.generation += 1;
                core.state = PlaybackState::Stopped;
                core.position = 0.0;
                Action::Cancel
            }
        }
    };
    match action {
        Action::Emit(s) => emit_state(app, s),
        Action::Replay(i) => play_index(app, i),
        Action::Cancel => {
            let _ = state.player.audio.send(AudioCmd::Stop);
            emit_state(app, PlaybackState::Stopped);
        }
        Action::None => {}
    }
}

pub fn play_next(app: &AppHandle, manual: bool) {
    let state = app.state::<AppState>();
    let shared = state.player.clone();
    let next = {
        let mut core = shared.core.lock_safe();
        pick_next(&mut core, manual)
    };
    match next {
        Some(i) => start_playback(app, i),
        None => stop(app),
    }
}

/// Pick the previous queue index, mirroring [`pick_next`]. Near the start of a
/// track — or at the very front of the shuffle history — this returns the
/// current track to restart it. Under shuffle it steps back through the actual
/// playback history rather than queue order (mutating `shuffle_cursor`), so a
/// following Next replays the track you came from.
fn pick_prev(core: &mut PlayerCore) -> Option<usize> {
    let cur = core.current?;
    let prev = if core.position > 3.0 {
        cur
    } else if core.shuffle && core.queue.len() > 1 {
        if core.shuffle_cursor > 0 {
            core.shuffle_cursor -= 1;
            core.shuffle_history[core.shuffle_cursor]
        } else {
            cur
        }
    } else if cur > 0 {
        cur - 1
    } else {
        cur
    };
    Some(prev)
}

pub fn play_prev(app: &AppHandle) {
    let state = app.state::<AppState>();
    let shared = state.player.clone();
    let prev = {
        let mut core = shared.core.lock_safe();
        pick_prev(&mut core)
    };
    if let Some(i) = prev {
        start_playback(app, i);
    }
}

/// What the caller should do after [`remove_from_queue`] mutates the core.
pub enum RemoveOutcome {
    /// Index was out of range; nothing changed.
    None,
    /// Queue changed but playback didn't; just re-broadcast.
    EmitOnly,
    /// The playing track was removed; start the one that took its slot.
    PlayIndex(usize),
    /// The playing track was the last one; stop.
    Stop,
}

/// Remove the track at `index`, fixing up `current` and resetting the shuffle
/// history (queue indices shift on removal). Pure so the index math is testable.
pub fn remove_from_queue(core: &mut PlayerCore, index: usize) -> RemoveOutcome {
    if index >= core.queue.len() {
        return RemoveOutcome::None;
    }
    core.queue.remove(index);
    core.shuffle_history.clear();
    core.shuffle_cursor = 0;
    match core.current {
        // A track before the current one went away: current shifts down by one.
        Some(cur) if index < cur => {
            core.current = Some(cur - 1);
            core.shuffle_history = vec![cur - 1];
            RemoveOutcome::EmitOnly
        }
        // The playing track was removed: move to whatever took its place, or
        // stop if it was the last one.
        Some(cur) if index == cur => {
            if core.queue.is_empty() {
                core.current = None;
                RemoveOutcome::Stop
            } else {
                RemoveOutcome::PlayIndex(cur.min(core.queue.len() - 1))
            }
        }
        // Removal was after the current track: its index is unchanged.
        Some(cur) => {
            core.shuffle_history = vec![cur];
            RemoveOutcome::EmitOnly
        }
        None => RemoveOutcome::EmitOnly,
    }
}

pub fn stop(app: &AppHandle) {
    let state = app.state::<AppState>();
    let shared = state.player.clone();
    {
        let mut core = shared.core.lock_safe();
        core.state = PlaybackState::Stopped;
        core.position = 0.0;
    }
    let _ = shared.audio.send(AudioCmd::Stop);
    emit_state(app, PlaybackState::Stopped);
}

/// Forwards events from the audio thread to the frontend and advances the
/// queue when a track ends.
pub async fn event_loop(app: AppHandle, mut rx: UnboundedReceiver<AudioEvent>) {
    while let Some(ev) = rx.recv().await {
        match ev {
            AudioEvent::Duration(duration) => {
                let position = {
                    let state = app.state::<AppState>();
                    let mut core = state.player.core.lock_safe();
                    core.duration = duration;
                    core.position
                };
                let _ = app.emit(events::PROGRESS, Progress { position, duration });
            }
            AudioEvent::Position(position) => {
                let (duration, crossfade) = {
                    let state = app.state::<AppState>();
                    let mut core = state.player.core.lock_safe();
                    core.position = position;
                    // Arm a crossfade once, when the track is within the overlap
                    // window of its end. Repeat-one is excluded (a track can't
                    // overlap itself); keyed by generation so it fires at most
                    // once per track.
                    let arm = core.crossfade > 0.0
                        && core.state == PlaybackState::Playing
                        && core.repeat != RepeatMode::One
                        && core.duration > 0.0
                        && core.crossfade_armed_for != Some(core.generation)
                        // Don't re-arm while a crossfade prefetch is already in
                        // flight (e.g. after a seek cleared the arm flag).
                        && core.pending_next.is_none()
                        && core.duration - position <= core.crossfade;
                    if arm {
                        core.crossfade_armed_for = Some(core.generation);
                    }
                    (core.duration, arm)
                };
                let _ = app.emit(events::PROGRESS, Progress { position, duration });
                if crossfade {
                    debug!("nearing track end, starting crossfade");
                    begin_crossfade(&app);
                }
            }
            AudioEvent::Ended => {
                debug!("track ended, advancing queue");
                play_next(&app, false);
            }
            AudioEvent::Failed(msg) => {
                let _ = app.emit(events::ERROR, msg);
                stop(&app);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_track(i: usize) -> Track {
        Track {
            id: i.to_string(),
            title: String::new(),
            artist: String::new(),
            album: None,
            duration: None,
            cover: String::new(),
            artists: Vec::new(),
            album_id: None,
        }
    }

    /// A core with `n` tracks, shuffle on, anchored on `start` as `play_index`
    /// would leave it.
    fn shuffled_core(n: usize, start: usize) -> PlayerCore {
        PlayerCore {
            queue: (0..n).map(dummy_track).collect(),
            current: Some(start),
            shuffle: true,
            shuffle_history: vec![start],
            shuffle_cursor: 0,
            ..PlayerCore::default()
        }
    }

    /// Apply a chosen index the way `start_playback` would (sets `current`).
    fn advance(core: &mut PlayerCore, idx: usize) {
        core.current = Some(idx);
    }

    #[test]
    fn shuffle_covers_every_track_once_before_repeating() {
        let mut core = shuffled_core(4, 2);
        let mut seen = vec![2];
        for _ in 0..3 {
            let n = pick_next(&mut core, true).expect("a next track");
            advance(&mut core, n);
            seen.push(n);
        }
        seen.sort_unstable();
        assert_eq!(seen, vec![0, 1, 2, 3], "every track plays exactly once");

        // Cycle complete: auto-advance with repeat off stops.
        assert_eq!(pick_next(&mut core, false), None);
    }

    #[test]
    fn shuffle_previous_retraces_then_next_replays_forward() {
        let mut core = shuffled_core(5, 0);

        // Build a history by skipping forward.
        let mut played = vec![0usize];
        for _ in 0..3 {
            let n = pick_next(&mut core, true).unwrap();
            advance(&mut core, n);
            played.push(n);
        }
        assert_eq!(core.shuffle_history, played);
        assert_eq!(core.shuffle_cursor, played.len() - 1);

        // Previous steps back through the *actual* play order.
        let p1 = pick_prev(&mut core).unwrap();
        advance(&mut core, p1);
        assert_eq!(p1, played[2]);
        let p2 = pick_prev(&mut core).unwrap();
        advance(&mut core, p2);
        assert_eq!(p2, played[1]);

        // Next now replays forward through the existing order — no new draws.
        let f1 = pick_next(&mut core, true).unwrap();
        advance(&mut core, f1);
        assert_eq!(f1, played[2]);
        let f2 = pick_next(&mut core, true).unwrap();
        advance(&mut core, f2);
        assert_eq!(f2, played[3]);
        assert_eq!(core.shuffle_history, played, "replaying must not redraw");
    }

    #[test]
    fn previous_near_start_of_track_restarts_it() {
        let mut core = shuffled_core(5, 0);
        let n = pick_next(&mut core, true).unwrap();
        advance(&mut core, n);
        // More than a few seconds in: Previous restarts the current track.
        core.position = 5.0;
        assert_eq!(pick_prev(&mut core), Some(n));
    }

    #[test]
    fn sequential_next_prev_walk_queue_order() {
        let mut core = PlayerCore {
            queue: (0..3).map(dummy_track).collect(),
            current: Some(0),
            ..PlayerCore::default()
        };
        assert_eq!(pick_next(&mut core, false), Some(1));
        advance(&mut core, 1);
        assert_eq!(pick_prev(&mut core), Some(0));
        // End of queue, repeat off, auto-advance stops.
        advance(&mut core, 2);
        assert_eq!(pick_next(&mut core, false), None);
        // Manual next from the end wraps.
        assert_eq!(pick_next(&mut core, true), Some(0));
    }

    fn queue_core(n: usize, current: Option<usize>) -> PlayerCore {
        PlayerCore {
            queue: (0..n).map(dummy_track).collect(),
            current,
            ..PlayerCore::default()
        }
    }

    #[test]
    fn pending_next_is_honoured_once_then_falls_through() {
        let mut core = PlayerCore {
            queue: (0..3).map(dummy_track).collect(),
            current: Some(0),
            pending_next: Some(2),
            ..PlayerCore::default()
        };
        // A crossfade-prefetched pick is returned and consumed.
        assert_eq!(pick_next(&mut core, false), Some(2));
        assert_eq!(core.pending_next, None);
        // The next call falls through to normal sequential order.
        advance(&mut core, 2);
        assert_eq!(pick_next(&mut core, true), Some(0));
    }

    #[test]
    fn stale_out_of_range_pending_next_is_ignored() {
        let mut core = PlayerCore {
            queue: (0..2).map(dummy_track).collect(),
            current: Some(0),
            pending_next: Some(9),
            ..PlayerCore::default()
        };
        // An index left over from a longer queue is dropped, not played.
        assert_eq!(pick_next(&mut core, false), Some(1));
        assert_eq!(core.pending_next, None);
    }

    #[test]
    fn remove_before_current_shifts_current_down() {
        let mut core = queue_core(5, Some(3));
        assert!(matches!(
            remove_from_queue(&mut core, 1),
            RemoveOutcome::EmitOnly
        ));
        assert_eq!(core.current, Some(2));
        assert_eq!(core.queue.len(), 4);
    }

    #[test]
    fn remove_after_current_keeps_current() {
        let mut core = queue_core(5, Some(1));
        assert!(matches!(
            remove_from_queue(&mut core, 3),
            RemoveOutcome::EmitOnly
        ));
        assert_eq!(core.current, Some(1));
    }

    #[test]
    fn remove_current_plays_the_one_that_took_its_slot() {
        let mut core = queue_core(5, Some(2));
        assert!(matches!(
            remove_from_queue(&mut core, 2),
            RemoveOutcome::PlayIndex(2)
        ));
    }

    #[test]
    fn remove_current_at_end_clamps_to_new_last() {
        let mut core = queue_core(3, Some(2));
        assert!(matches!(
            remove_from_queue(&mut core, 2),
            RemoveOutcome::PlayIndex(1)
        ));
    }

    #[test]
    fn remove_last_remaining_current_stops() {
        let mut core = queue_core(1, Some(0));
        assert!(matches!(
            remove_from_queue(&mut core, 0),
            RemoveOutcome::Stop
        ));
        assert_eq!(core.current, None);
        assert!(core.queue.is_empty());
    }

    #[test]
    fn remove_out_of_range_is_a_noop() {
        let mut core = queue_core(2, Some(0));
        assert!(matches!(
            remove_from_queue(&mut core, 9),
            RemoveOutcome::None
        ));
        assert_eq!(core.queue.len(), 2);
    }
}
