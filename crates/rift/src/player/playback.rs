//! Track resolution and playback: fetching stream bytes, starting a track,
//! backfilling credits, publishing now-playing surfaces, and crossfading.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use rift_types::{ArtistRef, PlaybackState, Progress, RepeatMode, Track};
use tracing::{debug, error, info, warn};

use super::nav::{pick_next, play_next};
use super::state::{emit_queue, emit_queue_quiet, emit_state, PlayerCore, PlayerShared};
use crate::audio::{AudioCmd, CROSSFADE_CANCELLED};
use crate::backend::BackendInner;
use crate::event::BackendEvent;
use crate::util::LockExt;

/// How long to wait for the audio thread to acknowledge a decode.
///
/// Decoding is from memory, so this is milliseconds in practice; the bound only
/// exists so a wedged audio thread cannot hang the playback task forever.
const DECODE_ACK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Report a hard playback failure for the request identified by `generation`:
/// stop the player state and surface the reason as an error toast.
///
/// The generation check is what keeps a stale request — one the user already
/// skipped past — from clobbering a newer track's state or showing an error
/// about a track that is no longer current.
fn fail_playback(
    backend: &Arc<BackendInner>,
    shared: &Arc<PlayerShared>,
    generation: u64,
    message: String,
) {
    {
        let mut core = shared.core.lock_safe();
        if core.generation != generation {
            return;
        }
        core.state = PlaybackState::Stopped;
    }
    backend.emit(BackendEvent::Error(message));
    emit_state(backend, PlaybackState::Stopped);
}

/// The length to publish for a track once its stream has decoded.
///
/// Decoder > fetch > queue-row metadata > unknown (0.0). The *preseeded* length
/// shown while `Loading` deliberately keeps the previous track's value so the
/// bar does not collapse, but that value is not a length this track ever had:
/// publishing it here (or adopting it into the row) would persist the previous
/// track's duration onto the new one.
pub(crate) fn published_duration(decoder: f64, fetch: f64, metadata: Option<u32>) -> f64 {
    for value in [decoder, fetch] {
        if value.is_finite() && value > 0.0 {
            return value;
        }
    }
    metadata.map_or(0.0, f64::from)
}

/// The two lengths `announce_track` needs: what to display/publish, and what
/// may be adopted into a row that has no length yet.
#[derive(Debug, Clone, Copy)]
pub(super) struct TrackLengths {
    /// Decoder > fetch > queue-row metadata > 0.0.
    pub display: f64,
    /// Decoder > fetch, or 0.0 when neither knows (metadata is not adopted).
    pub adopt: f64,
}

impl TrackLengths {
    pub(super) fn new(decoder: f64, fetch: f64, metadata: Option<u32>) -> Self {
        Self {
            display: published_duration(decoder, fetch, metadata),
            adopt: adoptable_duration(decoder, fetch),
        }
    }
}

/// The stream-reported length that may be written into a row whose length was
/// unknown, or 0.0 when neither the decoder nor the fetch knows it.
///
/// Metadata is deliberately not part of this: it is already the row's value,
/// and `adopt_fetched_duration` must never overwrite a known length.
pub(crate) fn adoptable_duration(decoder: f64, fetch: f64) -> f64 {
    for value in [decoder, fetch] {
        if value.is_finite() && value > 0.0 {
            return value;
        }
    }
    0.0
}

/// The length to show for a track whose stream is still being fetched.
///
/// [`crate::fetch::fetch_track`] reports 0.0 when the length is unknown — it
/// hands back the raw stream rather than probing the media, and the offline path
/// short-circuits entirely — and a track whose metadata lacked a duration has
/// `None`. Taking that zero at face value zeroes the seek bar's range until the
/// decoder corrects it a moment later, which reads as the bar jumping on every
/// offline track. Pure, so the rule is testable without a decoder.
pub(crate) fn preseed_duration(track_duration: Option<u32>, previous: f64) -> f64 {
    match track_duration {
        Some(secs) => f64::from(secs),
        // No metadata length either: keep the length the bar already had rather
        // than collapsing it, and let the decoder's `AudioEvent::Duration`
        // supply the authoritative value.
        None => previous,
    }
}

/// The longest stream length that will be adopted into metadata: a day. Anything
/// longer is not a song length but a corrupt/absurd probe (a live stream, a
/// broken container), and writing it into the library would be worse than the
/// missing field.
const MAX_ADOPTED_DURATION_SECS: f64 = 24.0 * 60.0 * 60.0;

/// Adopt the fetched stream/decoder length into a track whose metadata never
/// carried one. Returns `true` when the duration was set.
///
/// Album-page rows can be stored (and liked) with no length; the stream itself
/// always knows the real one by the time the track is playing. The duration is
/// rounded to whole seconds because that is the unit of [`Track::duration`], and
/// non-positive/absurd values are ignored rather than written: `0.0` is what the
/// fetch reports when the length is unknown, and the decoder reports the real
/// value right after anyway. The 24-hour cap keeps a broken probe from becoming
/// permanent library data. Pure, so the rule is testable without a decoder.
pub(crate) fn adopt_fetched_duration(track: &mut Track, dur: f64) -> bool {
    if track.duration.is_some() || !dur.is_finite() || dur <= 0.0 {
        return false;
    }
    let seconds = dur.round();
    if !(1.0..=MAX_ADOPTED_DURATION_SECS).contains(&seconds) {
        return false;
    }
    track.duration = Some(seconds as u32);
    true
}

/// Whether a position tick should start a crossfade into the next track.
///
/// Records the arm — keyed by [`PlayerCore::generation`] — when it returns
/// `true`, so the watcher fires at most once per track. Pure, so the rules this
/// encodes are testable without an audio thread:
///
/// * crossfades must be enabled (`crossfade > 0`) and a length must be known;
/// * only while actually playing; repeat-one is excluded because a track cannot
///   overlap itself (and the repeat is a hard replay);
/// * a crossfade already armed for this generation, or a prefetched pick still
///   in flight, must not draw a second target;
/// * the tick has to be inside the overlap window at the end of the track.
///
/// Keys the arm on the generation rather than a bool so a seek that clears the
/// flag (to re-evaluate the window) and a new track both behave correctly.
pub(crate) fn should_arm_crossfade(core: &mut PlayerCore, position: f64) -> bool {
    let arm = core.crossfade > 0.0
        && core.state == PlaybackState::Playing
        && core.repeat != RepeatMode::One
        && core.duration > 0.0
        && core.crossfade_armed_for != Some(core.generation)
        && core.pending_next.is_none()
        && core.duration - position <= core.crossfade;
    if arm {
        core.crossfade_armed_for = Some(core.generation);
    }
    arm
}

/// A play request whose queue/index claim has been made under one core lock and
/// is waiting for its side effects (events, the audio command, the fetch).
#[derive(Debug, Clone)]
pub(super) struct PlayRequest {
    pub(super) index: usize,
    pub(super) track: Track,
    pub(super) generation: u64,
}

/// Make `index` current in `core` and claim the playback generation.
///
/// This is the under-lock half of [`play_index`] and [`start_playback`], and it
/// is pure so the claim can be tested in the same critical section the callers
/// use. `None` means `index` is not in the queue; nothing was changed.
fn claim_play(core: &mut PlayerCore, index: usize) -> Option<PlayRequest> {
    let track = core.queue.get(index)?.clone();
    core.current = Some(index);
    // The generation doubles as the play identity handed to the audio thread:
    // the thread echoes it on every position/duration/end event, and the manager
    // drops anything tagged with a generation that is no longer current.
    //
    // Advance by two, not one: a prefetched crossfade uses `generation + 1` as
    // its play id before it is committed (see `crossfade_play_id`). If a hard
    // play only advanced by one it would reuse that id, letting a stale
    // crossfade event (or the orphaned-crossfade `StopIfPlaying`) act on the
    // new play.
    core.generation += 2;
    // A hard start supersedes any crossfade the position watcher prefetched.
    core.pending_next = None;
    core.state = PlaybackState::Loading;
    core.position = 0.0;
    core.duration = preseed_duration(track.duration, core.duration);
    Some(PlayRequest {
        index,
        track,
        generation: core.generation,
    })
}

/// The under-lock half of a command-layer play request: optionally replace the
/// queue, validate `index` against the queue that then exists, reset the failure
/// budget, anchor the shuffle cycle on the jump, and claim the track.
///
/// The replacement, the index check and the claim are one critical section on
/// purpose. `play_tracks`/`play_track` run as async commands on the backend
/// runtime, so two of them can interleave. When the command layer installed the
/// queue under one lock and then called a play function that validated the index
/// under a second, B could replace the queue between A's install and A's check:
/// A then validated `start` against B's queue and could start the wrong track (or
/// silently lose its own request). Folding the steps in makes each request either
/// play its own queue's index or nothing at all.
///
/// `None` when `index` does not exist in the resulting queue; the previous queue
/// (and every other field) is left untouched in that case.
pub(super) fn prepare_play_index(
    core: &mut PlayerCore,
    index: usize,
    replacement: Option<(Vec<Track>, Option<String>)>,
) -> Option<PlayRequest> {
    if let Some((tracks, source)) = replacement {
        if index >= tracks.len() {
            return None;
        }
        core.queue = tracks;
        core.source = source;
        core.shuffle_history.clear();
        core.shuffle_cursor = 0;
    }
    if index >= core.queue.len() {
        return None;
    }
    // A deliberate jump is a fresh start: give the queue a full retry budget.
    core.failures = 0;
    super::nav::anchor_playing(core, index);
    claim_play(core, index)
}

/// Start playing the track at `index`. Used by direct jumps — clicking a track,
/// starting a collection, resuming a stopped track, and the two play commands.
/// Sequential and shuffle Next/Previous go through [`play_next`]/
/// [`super::nav::play_prev`], which preserve the history instead.
///
/// `replacement` is the optional `(tracks, source)` queue replacement used by
/// `play_tracks`/`play_track`: it is installed in the same critical section that
/// validates `index` and claims the track, so two rapid play requests cannot
/// start an index drawn from each other's queue. `None` plays from the queue that
/// already exists. Returns whether the request was valid (the index existed in
/// the resulting queue); a request superseded after its claim still returns
/// `true` — the newer play owns the state and this one is discarded.
pub(crate) fn play_index(
    backend: &Arc<BackendInner>,
    index: usize,
    replacement: Option<(Vec<Track>, Option<String>)>,
) -> bool {
    let request = {
        let mut core = backend.player.core.lock_safe();
        prepare_play_index(&mut core, index, replacement)
    };
    match request {
        Some(request) => {
            start_prepared_playback(backend, request);
            true
        }
        None => false,
    }
}

/// Resolve and play the track at `index` without touching the queue.
///
/// Assumes the shuffle history has already been positioned by the caller (Next,
/// Previous, an auto-advance or a crossfade commit). A direct jump uses
/// [`play_index`], which also resets the failure budget and anchors the cycle.
pub(crate) fn start_playback(backend: &Arc<BackendInner>, index: usize) {
    let request = {
        let mut core = backend.player.core.lock_safe();
        claim_play(&mut core, index)
    };
    if let Some(request) = request {
        start_prepared_playback(backend, request);
    }
}

/// Hand a claimed play to the audio thread: stop whatever is playing, announce
/// the new track, and spawn the fetch/decode task that commits `Playing`.
///
/// The claim and this hand-off are separated by the core lock, so the claim may
/// already have been superseded. That request is discarded: replaying its
/// `Stop`/`Track`/`Loading` events would describe a track the newer state has
/// replaced, and its fetch would then be discarded by the generation check —
/// leaving the UI on `Loading` forever.
fn start_prepared_playback(backend: &Arc<BackendInner>, request: PlayRequest) {
    let shared = Arc::clone(&backend.player);
    let library = Arc::clone(&backend.library);
    let PlayRequest {
        index,
        track,
        generation,
    } = request;

    if !is_current_generation(&shared.core.lock_safe(), generation) {
        debug!("discarding a play request superseded before it reached the audio thread");
        return;
    }

    // A hard start supersedes any crossfade the position watcher prefetched.
    // Bump the audio-side token before the `Stop` below: the crossfade command
    // may already be queued ahead of this start's `Stop`, and the audio thread
    // must drop it rather than fade in a track this request has replaced. A
    // prefetch that has not sent its command yet re-checks the generation and pin
    // under the core lock and bails, so it cannot slip in after this bump.
    shared.crossfade_epoch.fetch_add(1, Ordering::SeqCst);

    // The audio thread is the only way anything can be heard. If it is gone
    // (its spawn failed, or the output device/thread died), fail the request
    // now: carrying on would fetch a stream nobody can decode and leave the UI
    // reporting `Loading` forever.
    if shared.audio.send(AudioCmd::Stop).is_err() {
        error!("audio thread is gone; cannot play {}", track.id);
        // `current` was already moved onto the requested track above; broadcast
        // the queue so the UI's snapshot matches the Stopped state below.
        emit_queue(backend, &shared);
        fail_playback(
            backend,
            &shared,
            generation,
            format!(
                "Could not play \u{201c}{}\u{201d}: audio output is unavailable",
                track.title
            ),
        );
        return;
    }

    info!(
        "playing \"{}\" by {} ({})",
        track.title, track.artist, track.id
    );
    backend.emit(BackendEvent::Track(track.clone()));
    // Announce the track to Discord before the Loading state so the presence
    // reflects the new track immediately (the accurate duration backfills via
    // the OS media session once the stream resolves).
    backend
        .discord
        .set_track(&track, f64::from(track.duration.unwrap_or(0)));
    emit_state(backend, PlaybackState::Loading);
    emit_queue(backend, &shared);

    let runtime = backend.runtime.clone();
    let task_backend = Arc::clone(backend);
    runtime.spawn(async move {
        let backend = task_backend;
        match crate::fetch::fetch_track(&backend.downloads.dir, &track.id).await {
            Ok((data, duration)) => {
                {
                    let core = shared.core.lock_safe();
                    if core.generation != generation {
                        debug!("discarding stale download for {}", track.id);
                        return;
                    }
                }
                // Hand the bytes to the audio thread and wait for it to decode
                // them. `Playing` is announced only on a successful decode: the
                // state used to flip here, before the audio thread had looked at
                // the payload, so a decode failure left the UI reporting Playing
                // with an advancing bar over silence until the tick noticed.
                let (ack_tx, ack_rx) = tokio::sync::oneshot::channel();
                if shared
                    .audio
                    .send(AudioCmd::Play(data, Some(ack_tx), generation))
                    .is_err()
                {
                    // The thread died between the stop above and this hand-off.
                    // Commit the failure (a stale request must not touch a newer
                    // track's state) instead of leaving `Loading` up forever.
                    error!("audio thread is gone; cannot play {}", track.id);
                    fail_playback(
                        &backend,
                        &shared,
                        generation,
                        format!(
                            "Could not play \u{201c}{}\u{201d}: audio output is unavailable",
                            track.title
                        ),
                    );
                    return;
                }
                // The audio thread decodes in-memory bytes, so this is a few
                // milliseconds; the timeout only guards against a wedged thread.
                let decoded = match tokio::time::timeout(DECODE_ACK_TIMEOUT, ack_rx).await {
                    Ok(Ok(result)) => result,
                    Ok(Err(_)) => Err("audio thread dropped the decode request".to_owned()),
                    Err(_) => Err(format!(
                        "audio thread did not decode within {}s",
                        DECODE_ACK_TIMEOUT.as_secs()
                    )),
                };

                let stream_dur = match decoded {
                    Ok(d) => d,
                    Err(msg) => {
                        // The bytes reached the audio thread and it refused them,
                        // so there is nothing to play and nothing playing. Commit
                        // Stopped (the fetch-failure path below cannot be reused:
                        // auto-skipping here would race the audio thread's own
                        // `DecodeFailed`, and with an ack outstanding the thread
                        // deliberately stays quiet so only one path advances the
                        // queue).
                        error!("failed to play {}: {msg}", track.id);
                        fail_playback(
                            &backend,
                            &shared,
                            generation,
                            format!("Could not play \u{201c}{}\u{201d}: {msg}", track.title),
                        );
                        return;
                    }
                };

                let published = published_duration(stream_dur, duration, track.duration);
                let adoptable = adoptable_duration(stream_dur, duration);
                let dur = {
                    let mut core = shared.core.lock_safe();
                    if core.generation != generation {
                        debug!("discarding stale decode for {}", track.id);
                        return;
                    }
                    core.state = PlaybackState::Playing;
                    core.failures = 0;
                    core.duration = published;
                    published
                };
                // Refresh Discord with the accurate stream/decoder duration: the
                // initial set_track (before Loading) only had the metadata guess,
                // which may be 0 or slightly off. The OS media session gets the
                // same correction in announce_track. Sent before the Playing
                // state so the presence's time bar (shown only while Playing)
                // uses the right length. set_track renders without timestamps, so
                // this refresh doesn't flicker the bar.
                backend.discord.set_track(&track, dur);
                // A newer PlayRequest can emit its own Track/Loading after this
                // request's start-side events but before this decode lands. The
                // generation check above proves this play owns the transport, so
                // re-assert the track at commit; otherwise the last Track could
                // name the superseded request's row.
                backend.emit(BackendEvent::Track(track.clone()));
                emit_state(&backend, PlaybackState::Playing);
                backend.emit(BackendEvent::Progress(Progress {
                    position: 0.0,
                    duration: dur,
                }));
                announce_track(
                    &backend,
                    &shared,
                    &library,
                    index,
                    generation,
                    track,
                    TrackLengths {
                        display: dur,
                        adopt: adoptable,
                    },
                )
                .await;
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
                backend.emit(BackendEvent::Error(format!(
                    "Could not play \u{201c}{}\u{201d}: {e:#}",
                    track.title
                )));
                if skip {
                    debug!("auto-skipping failed track {}", track.id);
                    play_next(&backend, false);
                } else {
                    emit_state(&backend, PlaybackState::Stopped);
                }
            }
        }
    });
}

/// The plain-text byline for a set of resolved credits: what
/// [`Track::artist`] holds and every non-linkable surface shows.
///
/// Pure, so the join rule is testable without a network round trip.
pub(crate) fn byline(artists: &[ArtistRef]) -> String {
    artists
        .iter()
        .map(|a| a.name.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Backfill per-artist credits (with channel IDs) for a track saved before
/// they were stored. Returns `true` if anything changed. Tracks that already
/// have credits, or whose lookup fails, are left untouched.
async fn enrich_track(yt: &rift_ytmusic::Client, track: &mut Track) -> bool {
    if !track.artists.is_empty() {
        return false;
    }
    let (artists, album, album_id) = match yt.track_credits(&track.id).await {
        Ok(c) => c,
        Err(e) => {
            debug!("could not enrich {}: {e}", track.id);
            return false;
        }
    };
    if artists.is_empty() {
        return false;
    }
    track.artists = artists;
    // Fill the byline too. Storing the structured credits alone left the row
    // with `artists` populated and `artist` empty — the shape that used to
    // defeat the song matcher's credit check (it read only the byline), and one
    // the app's own library has eight of.
    if track.artist.is_empty() {
        track.artist = byline(&track.artists);
    }
    if track.album_id.is_none() {
        if track.album.is_none() {
            track.album = album;
        }
        track.album_id = album_id;
    }
    true
}

/// Whether playback is still on the request that produced `generation`.
///
/// [`announce_track`] re-checks this after every await before publishing
/// now-playing state: a skip bumps the generation, so a late credit/duration
/// update for the skipped track must not emit a stale `Track` event — the UI
/// (and for the media-session publish, the OS) would show the track the user
/// skipped past as now playing. Pure so the gate is testable.
pub(super) fn is_current_generation(core: &PlayerCore, generation: u64) -> bool {
    core.generation == generation
}

/// Refresh the now-playing surfaces once a track is actually playing: publish
/// it to the OS media session, backfill artist credits (persisting them so list
/// rows become linkable), and record it in the library's recents. Shared by the
/// fresh-start ([`start_playback`]) and crossfade ([`begin_crossfade`]) paths.
///
/// `generation` is the play request this announcement belongs to. Credit
/// enrichment awaits a network round trip, so the user may skip to another
/// track while it runs; every now-playing publication below is therefore gated
/// on this request still being the current one. Without that gate a skip during
/// enrichment would end with a stale `Track` event — the UI (and, before the
/// gate, the OS media session) would show the track the user already skipped
/// past as now playing. The queue-row metadata repairs are still applied when
/// stale: they improve the row wherever it now sits in the queue.
async fn announce_track(
    backend: &Arc<BackendInner>,
    shared: &Arc<PlayerShared>,
    library: &Arc<std::sync::Mutex<crate::library::LibraryStore>>,
    index: usize,
    generation: u64,
    mut track: Track,
    lengths: TrackLengths,
) {
    let dur = lengths.display;
    let still_current = || is_current_generation(&shared.core.lock_safe(), generation);

    // A row liked from an album page can have no length; the stream we just
    // decoded does. Adopt it before announcing, write it into the queue row so
    // the restored session has it on the next launch, and re-publish the
    // now-playing payload: `Track` was emitted before the stream resolved, so
    // without this the UI/OS session would keep the duration-less copy.
    if adopt_fetched_duration(&mut track, lengths.adopt) {
        {
            let mut core = shared.core.lock_safe();
            if let Some(slot) = core.queue.get_mut(index) {
                if slot.id == track.id && slot.duration.is_none() {
                    slot.duration = track.duration;
                }
            }
        }
        emit_queue_quiet(backend, shared);
        if still_current() {
            backend.emit(BackendEvent::Track(track.clone()));
        }
    }
    if still_current() {
        backend.media.set_track(&track, dur);
    }

    if enrich_track(&shared.yt, &mut track).await {
        {
            let mut core = shared.core.lock_safe();
            if let Some(slot) = core.queue.get_mut(index) {
                if slot.id == track.id {
                    *slot = track.clone();
                }
            }
        }
        emit_queue_quiet(backend, shared);
        if still_current() {
            backend.emit(BackendEvent::Track(track.clone()));
        }
    }

    let mut lib = library.lock_safe();
    // Broader than the old credits-only backfill: a played track is a complete
    // metadata source, so it repairs its liked/recent/playlist copies (including
    // a duration the row never had) and moves itself into Recently played — all
    // with a single library save rather than one per step.
    lib.record_played(track);
    backend.emit(BackendEvent::Library(lib.data.clone()));
}

/// Why a fetched crossfade target cannot be handed to the audio thread yet.
///
/// This is the *pre-send* gate: the prefetch may still be in flight while
/// playback moves on. Once the command has been sent, the audio thread owns the
/// target and only its decode acknowledgement decides the commit (or a newer
/// command supersedes it); see [`finish_crossfade`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CrossfadeHold {
    /// Nothing blocks the hand-off: the target is still the current prefetch and
    /// playback is still inside the overlap window.
    None,
    /// Playback advanced while the target was fetching (a skip, a natural end,
    /// or a new prefetch consumed/changed the pin). This prefetch is obsolete.
    Stale,
    /// Playback was paused (or stopped) before the command was sent. Sending it
    /// would make the audio thread load and start a track the user asked to
    /// pause; the pin is left for the natural end-of-track path instead.
    Paused,
    /// A backward seek pulled the position back out of the overlap window.
    /// Sending it would cut the song short, so the pin is left for the natural
    /// end-of-track path (a hard cut).
    LeftWindow,
}

/// Decide whether a fetched crossfade target may be handed to the audio thread.
///
/// Pure, so the exactly-once rule this enforces is testable without a network
/// fetch or an audio device: a target may be sent only while the prefetch is
/// still the one that is current (same generation, same pin), playback is still
/// `Playing`, and the position is still inside the overlap window. Every
/// rejection leaves the pin in `pending_next` for the natural end-of-track path
/// (or, when stale, the advance that superseded it has already taken over).
///
/// After the command is sent this gate no longer applies: the audio thread has
/// loaded (and perhaps paused) the target, so [`finish_crossfade`] checks only
/// that the prefetch is still current and that the decode succeeded.
pub(super) fn crossfade_hold(
    core: &PlayerCore,
    anchor_generation: u64,
    index: usize,
) -> CrossfadeHold {
    if core.generation != anchor_generation || core.pending_next != Some(index) {
        return CrossfadeHold::Stale;
    }
    if core.state != PlaybackState::Playing {
        return CrossfadeHold::Paused;
    }
    if core.duration > 0.0 && core.duration - core.position > core.crossfade {
        return CrossfadeHold::LeftWindow;
    }
    CrossfadeHold::None
}

/// The play identity a crossfade hands the audio thread.
///
/// This is the generation [`commit_crossfade`] will assign when the decode
/// acknowledgement lands (`anchor_generation + 1`). The audio thread starts the
/// target as soon as it decodes — before the manager has committed it — so the
/// id on its events has to be the one the committed track will carry. Until the
/// commit makes that generation current every tagged event is dropped by the
/// manager's generation gate; a superseded crossfade never commits, so its events
/// stay dropped. Hard plays and cancellations advance by two ([`claim_play`],
/// [`super::nav::cancel_playback`]), so they skip the value reserved here and a
/// new play can never collide with an in-flight crossfade's id. Pure so the
/// contract between the send and the commit is tested directly.
pub(super) fn crossfade_play_id(anchor_generation: u64) -> u64 {
    anchor_generation + 1
}

/// Commit a prefetched crossfade pick: `index` becomes the playing track. The
/// caller has already established that the prefetch is still current (the
/// generation is unchanged, `pending_next` still names it) and that the audio
/// thread has acknowledged that the target decoded. Pure so the bookkeeping it
/// touches is testable on its own; the fetched duration is applied by the caller.
pub(crate) fn commit_crossfade(core: &mut PlayerCore, index: usize) {
    core.pending_next = None;
    // Anchor the shuffle cycle on the pick before it becomes current. Normally
    // it is already the cursor's entry — the prefetch drew it through
    // `pick_next` — and this only moves the cursor onto it; it is kept as this
    // consumer's own guarantee that a track becoming current ends up in the
    // played set (and so out of the draw pool), however the history was rewritten
    // in between.
    super::nav::anchor_playing(core, index);
    core.current = Some(index);
    core.state = PlaybackState::Playing;
    // Exactly the play id the audio thread was handed with the crossfade; the
    // two must not drift apart or the committed track's own events would be
    // dropped. See `crossfade_play_id`.
    core.generation = crossfade_play_id(core.generation);
    core.failures = 0;
    core.position = 0.0;
}

/// What the audio thread's answer to [`AudioCmd::Crossfade`] means for the
/// player state.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum CrossfadeAckOutcome {
    /// The target decoded, is still the current prefetch, and was committed.
    Committed {
        /// The queue index the audible target was committed at. A queue edit
        /// may have relocated the row while the decode was in flight; the
        /// caller must announce/adopt against this index, not the original one.
        index: usize,
        /// The generation the new track was committed under.
        generation: u64,
        /// `Playing`, or `Paused` when the user paused while the decode was in
        /// flight and the audio thread loaded (and paused) the target.
        state: PlaybackState,
        /// The length to broadcast: the decoder's, else the fetch's, else the
        /// queue row's.
        duration: f64,
        /// Where the new track starts: normally zero, but a seek that landed
        /// while the audio thread was decoding is preserved here.
        position: f64,
    },
    /// A newer command changed the generation or the pin while the decode was
    /// in flight. It owns the state; this acknowledgement must not touch it.
    Superseded,
    /// The target decoded and the audio thread started it, but a queue edit
    /// removed it before the acknowledgement. The transport is now `Stopped`;
    /// the caller must retire exactly the play id the crossfade started.
    Orphaned {
        /// The length to keep on the stopped progress bar.
        duration: f64,
    },
    /// The target did not decode. `current` and the playback state are untouched
    /// and the prefetch pin was dropped if it still named this target.
    Failed,
}

/// Apply the audio thread's decode acknowledgement for a prefetched crossfade
/// target.
///
/// The command has already been sent, so the only reasons not to commit are
/// that the prefetch was superseded or that the decode failed. Playback being
/// paused is *not* a reason: the audio thread has already loaded (and paused)
/// the target, so committing is what keeps the UI and the audio in agreement;
/// the paused state is preserved so the commit does not resume playback behind
/// the user's back.
///
/// On a failed decode `current` is left alone, so a natural end still advances
/// from the track the user is actually hearing. The pin is dropped only while
/// this generation still owns it, so the natural end does not consume a pick
/// that never became current; a newer command's pin is never touched. In queue
/// order that retries the target at its position through the normal advance path
/// (whose failure handling applies); under shuffle the already-consumed pick is
/// not retried this cycle — the error toast is what keeps the drop from being
/// silent.
///
/// `fallback_duration` is the length reported by the fetch (0.0 when unknown);
/// the queue row's metadata length is the last resort when the decoder reports
/// none.
pub(super) fn finish_crossfade(
    core: &mut PlayerCore,
    anchor_generation: u64,
    index: usize,
    target_id: &str,
    decoded: &Result<f64, String>,
    fallback_duration: f64,
    seek_epoch: u64,
) -> CrossfadeAckOutcome {
    // A newer play/stop/seek owns the state and already replaced the audio.
    if core.generation != anchor_generation {
        return CrossfadeAckOutcome::Superseded;
    }
    // The audio thread starts the target *before* this acknowledgement, so if
    // the decode succeeded the target is audible even when a queue/mode edit
    // cleared `pending_next` while the command was in flight. Disowning that
    // audio would leave the UI on the old track forever (its events carry the
    // uncommitted generation and the outgoing player has already faded). Commit
    // the audible track instead, relocated by id if the edit moved its row.
    let pending = core.pending_next == Some(index);
    if !pending && decoded.is_err() {
        // Nothing started, so a cleared pin simply means a newer edit won.
        return CrossfadeAckOutcome::Superseded;
    }
    let audible_index = if pending || core.queue.get(index).is_some_and(|t| t.id == target_id) {
        Some(index)
    } else {
        core.queue.iter().position(|t| t.id == target_id)
    };
    let Some(index) = audible_index else {
        // The target started but was removed from the queue in the meantime:
        // stop that audio (the caller retires only this play id) and report an
        // honest stopped transport instead of a frozen `Playing` bar.
        core.pending_next = None;
        core.state = PlaybackState::Stopped;
        core.position = 0.0;
        return CrossfadeAckOutcome::Orphaned {
            duration: core.duration,
        };
    };
    match decoded {
        Ok(decoded_duration) => {
            let paused = core.state == PlaybackState::Paused;
            // A seek that landed while the target decoded belongs to the new
            // track (the audio thread applies it after the crossfade command):
            // keep its position instead of resetting the bar to zero.
            let seeked = core.seek_epoch != seek_epoch;
            let seek_target = core.seek_target;
            commit_crossfade(core, index);
            if paused {
                core.state = PlaybackState::Paused;
            }
            if seeked {
                core.position = seek_target;
            }
            let metadata = core.queue.get(index).and_then(|track| track.duration);
            let duration = published_duration(*decoded_duration, fallback_duration, metadata);
            core.duration = duration;
            CrossfadeAckOutcome::Committed {
                index,
                generation: core.generation,
                state: core.state,
                duration,
                position: core.position,
            }
        }
        Err(message) if message == CROSSFADE_CANCELLED => {
            // The command was invalidated before it could start (a newer
            // playback/queue action moved the token, or this call timed out and
            // bumped it). Nothing is broken and the user did not ask for this
            // specific transition, so drop the pin silently instead of showing
            // a decode error. The natural end advances normally from the track
            // that is still current.
            core.pending_next = None;
            CrossfadeAckOutcome::Superseded
        }
        Err(_) => {
            // The pin still names this target (checked above): drop only this
            // crossfade's pin so the natural end redraws from the current track.
            // Leaving it would consume a pick this task has just learned is
            // unplayable. `crossfade_armed_for` is left alone on purpose:
            // re-arming would refetch and re-decode the same broken target on
            // every position tick until the old track ends.
            core.pending_next = None;
            CrossfadeAckOutcome::Failed
        }
    }
}

/// Begin a crossfade into the next track: draw it now, fetch it in the
/// background, and once its bytes are ready overlap it with the current track
/// via [`AudioCmd::Crossfade`]. Triggered by the position watcher as the current
/// track nears its end.
///
/// Nothing is committed before the audio thread acknowledges the decode:
/// `current`, the generation and the duration all move only once the target is
/// actually loaded *and* this prefetch is still the current one. A target that
/// fetches but does not decode therefore leaves the old track playing and is
/// reported as an error, instead of the UI naming a track the audio thread never
/// loaded — the state that used to make the old track's natural end advance from
/// the new index and skip the target.
///
/// If the fetch is too slow and the current track ends first, the natural
/// end-of-track path advances normally (reusing the same drawn pick), and this
/// crossfade is discarded — a graceful fall back to a hard cut.
pub(crate) fn begin_crossfade(backend: &Arc<BackendInner>) {
    let shared = Arc::clone(&backend.player);
    let library = Arc::clone(&backend.library);

    let (index, anchor_gen, fade) = {
        let mut core = shared.core.lock_safe();
        // The watcher decided to arm a moment ago, outside this lock. Re-check
        // the conditions it judged against values a concurrent command can
        // change (a pause, a repeat-mode switch, crossfade turned off in
        // between): pinning a target for a transition that can no longer
        // crossfade would hold `pending_next` and block a fresh arm later.
        // Un-arm and return instead; the next tick can arm again.
        if core.state != PlaybackState::Playing
            || core.repeat == RepeatMode::One
            || core.crossfade <= 0.0
        {
            core.crossfade_armed_for = None;
            return;
        }
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

    let runtime = backend.runtime.clone();
    let task_backend = Arc::clone(backend);
    runtime.spawn(async move {
        let backend = task_backend;
        let Some(track) = shared.core.lock_safe().queue.get(index).cloned() else {
            return;
        };
        match crate::fetch::fetch_track(&backend.downloads.dir, &track.id).await {
            Ok((data, duration)) => {
                let fade = std::time::Duration::from_secs_f64(fade.max(0.0));
                let (ack_tx, ack_rx) = tokio::sync::oneshot::channel();
                // Last chance to withhold the command: the user may have paused,
                // skipped or seeked while the fetch ran. The hold check, the
                // token fetch and the send happen under the core lock on purpose:
                // a queue edit clears `pending_next` under the same lock, so
                // either we see the edit (and defer) or the edit's `emit_queue`
                // bumps the token after we release (and the audio thread drops
                // the command before starting it).
                let (seek_epoch, send_failed) = {
                    let core = shared.core.lock_safe();
                    match crossfade_hold(&core, anchor_gen, index) {
                        CrossfadeHold::None => {}
                        CrossfadeHold::Stale => {
                            debug!("discarding stale crossfade for {}", track.id);
                            return;
                        }
                        CrossfadeHold::Paused => {
                            debug!("crossfade target ready but playback is paused; deferring");
                            return;
                        }
                        CrossfadeHold::LeftWindow => {
                            debug!(
                                "crossfade target ready but position left the overlap window; \
                                 deferring"
                            );
                            return;
                        }
                    }
                    let token = shared.crossfade_epoch.fetch_add(1, Ordering::SeqCst) + 1;
                    // The target starts on the audio thread before the manager
                    // has committed it, so its play identity must be the
                    // generation `commit_crossfade` will assign: `anchor_gen + 1`.
                    // Until then its events are dropped by the generation gate,
                    // as they are for good if this acknowledgement is superseded.
                    let play_id = crossfade_play_id(anchor_gen);
                    (
                        core.seek_epoch,
                        shared
                            .audio
                            .send(AudioCmd::Crossfade(
                                data,
                                fade,
                                Some(ack_tx),
                                token,
                                play_id,
                            ))
                            .is_err(),
                    )
                };
                if send_failed {
                    // The audio thread is gone, so the outgoing track is silent
                    // too: commit the failure rather than leave the UI on
                    // `Playing` with a frozen bar (the same rule as the
                    // hard-play path).
                    error!(
                        "audio thread is gone; crossfade to {} did not start",
                        track.id
                    );
                    fail_playback(
                        &backend,
                        &shared,
                        anchor_gen,
                        format!(
                            "Could not play \u{201c}{}\u{201d}: audio output is unavailable",
                            track.title
                        ),
                    );
                    return;
                }
                // Decoding is from memory; the bound only guards a wedged thread.
                let decoded = match tokio::time::timeout(DECODE_ACK_TIMEOUT, ack_rx).await {
                    Ok(Ok(result)) => result,
                    Ok(Err(_)) => Err("audio thread dropped the decode request".to_owned()),
                    Err(_) => Err(format!(
                        "audio thread did not crossfade within {}s",
                        DECODE_ACK_TIMEOUT.as_secs()
                    )),
                };

                // Re-lock only after the acknowledgement: never hold the core
                // mutex across the await above. `finish_crossfade` is the single
                // commit decision (success, paused-during-decode, superseded,
                // failure) so it is unit-tested without an audio device.
                let outcome = {
                    let mut core = shared.core.lock_safe();
                    // If the acknowledgement failed while this prefetch still
                    // owns the player state, the command may simply be stuck
                    // behind a wedged audio thread. Bump the token so that if it
                    // ever gets decoded, it is dropped instead of starting audio
                    // this call has already reported as not started.
                    if decoded.is_err()
                        && core.generation == anchor_gen
                        && core.pending_next == Some(index)
                    {
                        shared.crossfade_epoch.fetch_add(1, Ordering::SeqCst);
                    }
                    let outcome = finish_crossfade(
                        &mut core, anchor_gen, index, &track.id, &decoded, duration, seek_epoch,
                    );
                    // A pause that raced the send can sit *before* the crossfade
                    // in the audio thread's queue: it would pause the outgoing
                    // player, and the new player started by the crossfade would
                    // then be audible while the UI says `Paused`. Re-assert the
                    // pause. Sending under the core lock keeps a concurrent
                    // Resume ordered after it (Resume takes the same lock before
                    // sending), so a user who resumed is never silently paused.
                    if let CrossfadeAckOutcome::Committed {
                        state: PlaybackState::Paused,
                        ..
                    } = outcome
                    {
                        let _ = shared.audio.send(AudioCmd::Pause);
                    }
                    outcome
                };

                if let Err(msg) = &decoded {
                    // Log every failed decode, but only toast the one that still
                    // owns the player state: a decode that failed *after* a newer
                    // command superseded it must not put an error about a track
                    // the user already left on screen.
                    error!("crossfade to {} failed: {msg}", track.id);
                    if outcome == CrossfadeAckOutcome::Failed {
                        backend.emit(BackendEvent::Error(format!(
                            "Could not crossfade to \u{201c}{}\u{201d}: {msg}",
                            track.title
                        )));
                    }
                }

                match outcome {
                    CrossfadeAckOutcome::Committed {
                        index: committed_index,
                        generation,
                        state,
                        duration,
                        position,
                    } => {
                        backend.emit(BackendEvent::Track(track.clone()));
                        backend.discord.set_track(&track, duration);
                        emit_state(&backend, state);
                        emit_queue(&backend, &shared);
                        backend.emit(BackendEvent::Progress(Progress { position, duration }));
                        let decoded_seconds = decoded.as_ref().copied().unwrap_or(0.0);
                        let lengths = TrackLengths::new(decoded_seconds, duration, track.duration);
                        announce_track(
                            &backend,
                            &shared,
                            &library,
                            committed_index,
                            generation,
                            track,
                            lengths,
                        )
                        .await;
                    }
                    CrossfadeAckOutcome::Orphaned { duration } => {
                        // The audio thread started this target, but a queue edit
                        // removed its row before the ack. Retire exactly that play
                        // id (a newer Play has a different one and is untouched),
                        // then report the stopped transport so the UI is honest.
                        let _ = shared
                            .audio
                            .send(AudioCmd::StopIfPlaying(crossfade_play_id(anchor_gen)));
                        warn!(
                            "crossfade target {} left the queue while starting; stopped playback",
                            track.id
                        );
                        emit_state(&backend, PlaybackState::Stopped);
                        emit_queue(&backend, &shared);
                        backend.emit(BackendEvent::Progress(Progress {
                            position: 0.0,
                            duration,
                        }));
                    }
                    CrossfadeAckOutcome::Superseded => {
                        debug!(
                            "crossfade to {} was superseded before it committed",
                            track.id
                        );
                    }
                    CrossfadeAckOutcome::Failed => {}
                }
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

#[cfg(test)]
mod tests {
    use super::{
        adopt_fetched_duration, adoptable_duration, claim_play, crossfade_play_id,
        finish_crossfade, published_duration, CrossfadeAckOutcome,
    };
    use crate::player::nav::pick_next;
    use crate::player::state::PlayerCore;
    use rift_types::{PlaybackState, Track};

    fn track_with(duration: Option<u32>) -> Track {
        Track {
            id: "id".into(),
            title: "Title".into(),
            artist: "Artist".into(),
            album: None,
            duration,
            cover: String::new(),
            artists: Vec::new(),
            album_id: None,
        }
    }

    fn queued_track(id: &str) -> Track {
        Track {
            id: id.into(),
            ..track_with(Some(200))
        }
    }

    /// A playing two-track queue inside the crossfade window with the next track
    /// already prefetched and pinned: the state `begin_crossfade`'s task is in
    /// when the audio thread's acknowledgement arrives.
    fn awaiting_ack_core() -> PlayerCore {
        PlayerCore {
            queue: vec![queued_track("a"), queued_track("b")],
            current: Some(0),
            state: PlaybackState::Playing,
            position: 195.0,
            duration: 200.0,
            crossfade: 10.0,
            pending_next: Some(1),
            generation: 7,
            ..PlayerCore::default()
        }
    }

    /// The reported-bug complement: a row liked without a length adopts the
    /// decoded stream's duration when it is played.
    #[test]
    fn adopt_fetched_duration_fills_a_missing_length() {
        let mut candidate = track_with(None);
        assert!(adopt_fetched_duration(&mut candidate, 221.4));
        assert_eq!(candidate.duration, Some(221));
        let mut candidate = track_with(None);
        assert!(adopt_fetched_duration(&mut candidate, 221.6));
        assert_eq!(
            candidate.duration,
            Some(222),
            "rounds to the nearest second"
        );
    }

    /// A known length is authoritative: the decoder must not overwrite it.
    #[test]
    fn adopt_fetched_duration_never_overwrites_a_known_length() {
        let mut candidate = track_with(Some(430));
        assert!(!adopt_fetched_duration(&mut candidate, 999.0));
        assert_eq!(candidate.duration, Some(430));
    }

    /// `0.0` means "length unknown" from the fetch path, and non-finite values
    /// are not lengths at all; neither may reach the library.
    #[test]
    fn adopt_fetched_duration_ignores_unknown_or_invalid_lengths() {
        for bad in [0.0, -1.0, f64::NAN, f64::INFINITY, 0.4] {
            let mut candidate = track_with(None);
            assert!(
                !adopt_fetched_duration(&mut candidate, bad),
                "must not adopt {bad}"
            );
            assert_eq!(candidate.duration, None);
        }
    }

    /// A plausible day-long stream is adopted; an absurd probe (broken container,
    /// live stream) is not, so it cannot become permanent library data.
    #[test]
    fn adopt_fetched_duration_caps_at_a_day() {
        let mut candidate = track_with(None);
        assert!(adopt_fetched_duration(&mut candidate, 24.0 * 60.0 * 60.0));
        assert_eq!(candidate.duration, Some(86_400));

        let mut candidate = track_with(None);
        assert!(!adopt_fetched_duration(
            &mut candidate,
            f64::from(u32::MAX) // ~136 years
        ));
        assert_eq!(candidate.duration, None);
    }

    /// The preseeded length shown while Loading is not this track's length: it
    /// must never be published or adopted. With no decoder/fetch/metadata the
    /// honest answer is "unknown" (0), not the previous track's duration.
    #[test]
    fn an_unknown_length_does_not_reuse_the_previous_tracks_length() {
        // `preseed_duration(None, 200.0)` keeps 200 for the loading bar; the
        // publish/adopt helpers deliberately never see that value.
        assert_eq!(published_duration(0.0, 0.0, None), 0.0);
        assert_eq!(adoptable_duration(0.0, 0.0), 0.0);

        // Known metadata is the display fallback, but is never "adopted".
        assert_eq!(published_duration(0.0, 0.0, Some(187)), 187.0);
        assert_eq!(adoptable_duration(0.0, 0.0), 0.0);

        // A real decoder length wins over the fetch and the metadata.
        assert_eq!(published_duration(183.5, 200.0, Some(187)), 183.5);
        assert_eq!(adoptable_duration(183.5, 200.0), 183.5);
        assert_eq!(published_duration(0.0, 200.0, Some(187)), 200.0);
        assert_eq!(adoptable_duration(0.0, 200.0), 200.0);

        // Non-finite/zero values are not lengths.
        assert_eq!(published_duration(f64::NAN, 0.0, None), 0.0);
        assert_eq!(adoptable_duration(f64::INFINITY, 0.0), 0.0);
    }

    /// The reported bug: a crossfade target that fetched but failed to decode
    /// must never be committed. The track the user is actually hearing stays
    /// current (so the natural end cannot advance from the new index and skip
    /// the target), and the failed pin is dropped so the natural end redraws
    /// from that track.
    #[test]
    fn a_failed_crossfade_decode_does_not_commit() {
        let mut core = awaiting_ack_core();
        let anchor = core.generation;
        let armed = core.crossfade_armed_for;
        let outcome = finish_crossfade(
            &mut core,
            anchor,
            1,
            "b",
            &Err("decode error: unsupported codec".to_owned()),
            0.0,
            0,
        );

        assert_eq!(outcome, CrossfadeAckOutcome::Failed);
        assert_eq!(
            core.current,
            Some(0),
            "the track the user hears stays current"
        );
        assert_eq!(core.state, PlaybackState::Playing, "and stays playing");
        assert_eq!(
            core.generation, anchor,
            "no commit means no generation bump"
        );
        assert_eq!(core.duration, 200.0, "the old track's length is untouched");
        assert_eq!(
            core.position, 195.0,
            "the old track's position is untouched"
        );
        assert_eq!(core.pending_next, None, "the failed pick's pin is dropped");
        assert_eq!(
            core.crossfade_armed_for, armed,
            "the track stays un-armed so the broken target is not refetched"
        );

        // The natural end advances from the still-current track: queue order
        // lands on the failed target's position (a retry), not one past it.
        assert_eq!(pick_next(&mut core, false), Some(1));
    }

    /// Regression: a queue/mode edit clears `pending_next` but does not bump
    /// the generation. The audio thread has already started the target by the
    /// time its `Ok` ack arrives, so disowning it would leave the target audible
    /// with all its events dropped and the outgoing player faded out — a
    /// permanently frozen `Playing` bar. The audible target must be committed.
    #[test]
    fn a_cleared_pin_still_commits_the_audible_crossfade_target() {
        let mut core = awaiting_ack_core();
        let anchor = core.generation;
        // A queue edit (e.g. removing an unrelated row) cleared the pin while the
        // crossfade command was in flight; the generation is unchanged.
        core.pending_next = None;

        let outcome = finish_crossfade(&mut core, anchor, 1, "b", &Ok(183.0), 200.0, 0);

        assert!(
            matches!(outcome, CrossfadeAckOutcome::Committed { .. }),
            "the already-started target must be committed, got {outcome:?}"
        );
        assert_eq!(core.current, Some(1), "the audible track is now current");
        assert_eq!(core.generation, anchor + 1, "the play id advances");
        assert_eq!(core.state, PlaybackState::Playing);
    }

    /// If the started target was genuinely removed from the queue, there is
    /// nothing to commit: the transport must become `Stopped` (the caller then
    /// retires exactly that play id) instead of pretending the old track still
    /// plays over the target's audio.
    #[test]
    fn a_removed_crossfade_target_stops_the_transport() {
        let mut core = awaiting_ack_core();
        let anchor = core.generation;
        core.queue.pop(); // the prefetched target is gone
        core.pending_next = None;

        let outcome = finish_crossfade(&mut core, anchor, 1, "b", &Ok(183.0), 200.0, 0);

        assert!(
            matches!(outcome, CrossfadeAckOutcome::Orphaned { .. }),
            "a removed target cannot be committed, got {outcome:?}"
        );
        assert_eq!(core.state, PlaybackState::Stopped);
        assert_eq!(core.current, Some(0), "the old row stays selected");
        assert_eq!(core.position, 0.0);
        assert_eq!(core.pending_next, None);
    }

    /// The audio thread reports a token cancellation with a distinguished
    /// message; that is a harmless supersession, not a decode failure, so it
    /// must not clear the player or surface an error. The pin is dropped so the
    /// natural end re-draws from the still-current track.
    #[test]
    fn a_cancelled_crossfade_ack_is_not_a_decode_error() {
        let mut core = awaiting_ack_core();
        let anchor = core.generation;
        let outcome = finish_crossfade(
            &mut core,
            anchor,
            1,
            "b",
            &Err(crate::audio::CROSSFADE_CANCELLED.to_owned()),
            0.0,
            0,
        );

        assert_eq!(outcome, CrossfadeAckOutcome::Superseded);
        assert_eq!(core.current, Some(0), "the old track stays current");
        assert_eq!(core.state, PlaybackState::Playing);
        assert_eq!(core.generation, anchor, "no commit, no generation bump");
        assert_eq!(core.pending_next, None, "the cancelled pick is dropped");
    }

    /// A queue edit may move the started target while its decode is in flight.
    /// The commit must report the relocated index so the caller announces and
    /// adopts against the row that is actually current, not the stale index.
    #[test]
    fn a_relocated_crossfade_commit_reports_the_new_index() {
        let mut core = awaiting_ack_core();
        let anchor = core.generation;
        core.pending_next = None;
        core.queue = vec![queued_track("a"), queued_track("x"), queued_track("b")];

        let outcome = finish_crossfade(&mut core, anchor, 1, "b", &Ok(180.0), 200.0, 0);

        let CrossfadeAckOutcome::Committed { index, .. } = outcome else {
            panic!("the audible target must commit, got {outcome:?}");
        };
        assert_eq!(index, 2, "the target's new queue position is reported");
        assert_eq!(
            core.current,
            Some(2),
            "the commit moved current to the new row"
        );
    }

    /// A successful acknowledgement is the only thing that commits the target;
    /// the decoder's length is authoritative, and the play bookkeeping is the
    /// same a committed crossfade always produced.
    #[test]
    fn a_successful_crossfade_ack_commits() {
        let mut core = awaiting_ack_core();
        let anchor = core.generation;
        let outcome = finish_crossfade(&mut core, anchor, 1, "b", &Ok(183.25), 200.0, 0);

        assert_eq!(
            outcome,
            CrossfadeAckOutcome::Committed {
                index: 1,
                generation: anchor + 1,
                state: PlaybackState::Playing,
                duration: 183.25,
                position: 0.0,
            }
        );
        assert_eq!(core.current, Some(1));
        assert_eq!(core.state, PlaybackState::Playing);
        assert_eq!(core.generation, anchor + 1);
        assert_eq!(core.duration, 183.25, "the decoder length wins");
        assert_eq!(core.position, 0.0, "the new track starts at the beginning");
        assert_eq!(core.pending_next, None, "the commit consumes the pin");
        assert_eq!(core.failures, 0);
    }

    /// Pausing while the decode is in flight is not a cancellation: the audio
    /// thread already loaded (and paused) the target, so the state must follow it
    /// there — as `Paused`, without resuming playback behind the user's back.
    #[test]
    fn a_crossfade_paused_during_decode_commits_paused() {
        let mut core = awaiting_ack_core();
        let anchor = core.generation;
        core.state = PlaybackState::Paused;

        // The decoder reports no length: the fetch's length is the fallback.
        let outcome = finish_crossfade(&mut core, anchor, 1, "b", &Ok(0.0), 201.5, 0);

        assert_eq!(
            outcome,
            CrossfadeAckOutcome::Committed {
                index: 1,
                generation: anchor + 1,
                state: PlaybackState::Paused,
                duration: 201.5,
                position: 0.0,
            }
        );
        assert_eq!(core.current, Some(1), "the loaded track is still committed");
        assert_eq!(
            core.state,
            PlaybackState::Paused,
            "the commit must not resume playback"
        );
        assert_eq!(core.duration, 201.5);
    }

    /// A crossfade reserves `anchor + 1` as the play id it starts the target
    /// under. A hard play claimed while that command is in flight must skip
    /// that value; otherwise its `Play`/events/`StopIfPlaying` would carry the
    /// same id as the in-flight crossfade.
    #[test]
    fn a_hard_play_after_a_sent_crossfade_cannot_reuse_its_play_id() {
        let mut core = awaiting_ack_core();
        let anchor = core.generation;
        let crossfade_id = crossfade_play_id(anchor);

        let request = claim_play(&mut core, 0).expect("the queue has index 0");

        assert_eq!(
            request.generation,
            anchor + 2,
            "hard plays reserve two values"
        );
        assert_ne!(
            request.generation, crossfade_id,
            "never reuse a crossfade id"
        );
        assert_eq!(
            core.pending_next, None,
            "the hard play drops the prefetch pin"
        );
    }

    /// A newer command owns the state: once the generation or the pin changed,
    /// the late acknowledgement must neither commit the old target nor touch the
    /// newer pin.
    #[test]
    fn a_superseded_crossfade_ack_never_commits() {
        // A new play request bumped the generation while the decode was in
        // flight (here the new pin happens to name the same index).
        let mut core = awaiting_ack_core();
        let anchor = core.generation;
        core.generation += 2;
        let outcome = finish_crossfade(&mut core, anchor, 1, "b", &Ok(180.0), 200.0, 0);

        assert_eq!(outcome, CrossfadeAckOutcome::Superseded);
        assert_eq!(
            core.current,
            Some(0),
            "the newer command's current track is untouched"
        );
        assert_eq!(core.generation, anchor + 2, "its generation is untouched");
        assert_eq!(core.pending_next, Some(1), "its pin is not consumed");

        // A newer prefetch changed the pin under the same generation: this
        // acknowledgement must not commit, clear the newer pin, or show an error
        // state change.
        let mut core = awaiting_ack_core();
        let anchor = core.generation;
        core.pending_next = Some(0);
        let outcome = finish_crossfade(
            &mut core,
            anchor,
            1,
            "b",
            &Err("decode error: unsupported codec".to_owned()),
            0.0,
            0,
        );

        assert_eq!(outcome, CrossfadeAckOutcome::Superseded);
        assert_eq!(core.current, Some(0));
        assert_eq!(core.state, PlaybackState::Playing);
        assert_eq!(core.generation, anchor);
        assert_eq!(core.pending_next, Some(0), "the newer pin is not dropped");
    }

    /// A seek that lands while the target is decoding belongs to the incoming
    /// track: the commit must keep its position instead of resetting the bar to
    /// zero. The audio thread applies the queued `Seek` after the crossfade, so
    /// zero here would disagree with the audio until the next tick.
    #[test]
    fn a_seek_during_the_crossfade_ack_keeps_its_position() {
        let mut core = awaiting_ack_core();
        let anchor = core.generation;
        let seek_epoch_at_send = core.seek_epoch;
        // The user seeks during the decode wait: the seek command records both
        // the epoch and the target under the core lock.
        core.seek_epoch += 1;
        core.seek_target = 42.5;

        let outcome = finish_crossfade(
            &mut core,
            anchor,
            1,
            "b",
            &Ok(183.0),
            200.0,
            seek_epoch_at_send,
        );

        assert_eq!(
            outcome,
            CrossfadeAckOutcome::Committed {
                index: 1,
                generation: anchor + 1,
                state: PlaybackState::Playing,
                duration: 183.0,
                position: 42.5,
            }
        );
        assert_eq!(core.position, 42.5, "the seek target survives the commit");
        assert_eq!(core.current, Some(1));
    }

    /// With no decoder and no fetch length, the queue row's metadata length is
    /// the last fallback, exactly as the pre-ack commit used it.
    #[test]
    fn a_crossfade_commit_falls_back_to_the_queue_row_duration() {
        let mut core = awaiting_ack_core();
        core.queue[1].duration = Some(204);
        let anchor = core.generation;
        let outcome = finish_crossfade(&mut core, anchor, 1, "b", &Ok(0.0), 0.0, 0);

        assert_eq!(
            outcome,
            CrossfadeAckOutcome::Committed {
                index: 1,
                generation: anchor + 1,
                state: PlaybackState::Playing,
                duration: 204.0,
                position: 0.0,
            }
        );
        assert_eq!(core.duration, 204.0);
    }
}
