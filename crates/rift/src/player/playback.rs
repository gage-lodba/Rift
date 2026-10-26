//! Track resolution and playback: fetching stream bytes, starting a track,
//! backfilling credits, publishing now-playing surfaces, and crossfading.

use std::sync::Arc;

use rift_types::{ArtistRef, PlaybackState, Progress, RepeatMode, Track};
use tracing::{debug, error, info};

use super::nav::{pick_next, play_next};
use super::state::{emit_queue, emit_state, PlayerCore, PlayerShared};
use crate::audio::AudioCmd;
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

/// Start playing the track at `index`, treating it as a fresh anchor for the
/// shuffle history. Used by direct jumps — clicking a track, starting a
/// collection, resuming a stopped track. Sequential and shuffle Next/Previous
/// go through [`play_next`]/[`super::nav::play_prev`], which preserve the history
/// instead.
pub(crate) fn play_index(backend: &Arc<BackendInner>, index: usize) {
    {
        let mut core = backend.player.core.lock_safe();
        if index >= core.queue.len() {
            return;
        }
        // A deliberate jump is a fresh start: give the queue a full retry budget.
        core.failures = 0;
        super::nav::anchor_playing(&mut core, index);
    }
    start_playback(backend, index);
}

/// Resolve and play the track at `index`. Assumes the shuffle history has
/// already been positioned by the caller.
pub(crate) fn start_playback(backend: &Arc<BackendInner>, index: usize) {
    let shared = Arc::clone(&backend.player);
    let library = Arc::clone(&backend.library);

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
        core.duration = preseed_duration(track.duration, core.duration);
        (track, core.generation)
    };
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
                    .send(AudioCmd::Play(data, Some(ack_tx)))
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

                let dur = {
                    let mut core = shared.core.lock_safe();
                    if core.generation != generation {
                        debug!("discarding stale decode for {}", track.id);
                        return;
                    }
                    core.state = PlaybackState::Playing;
                    core.failures = 0;
                    if stream_dur > 0.0 {
                        core.duration = stream_dur;
                    } else if duration > 0.0 {
                        core.duration = duration;
                    }
                    core.duration
                };
                // Refresh Discord with the accurate stream/decoder duration: the
                // initial set_track (before Loading) only had the metadata guess,
                // which may be 0 or slightly off. The OS media session gets the
                // same correction in announce_track. Sent before the Playing
                // state so the presence's time bar (shown only while Playing)
                // uses the right length. set_track renders without timestamps, so
                // this refresh doesn't flicker the bar.
                backend.discord.set_track(&track, dur);
                emit_state(&backend, PlaybackState::Playing);
                backend.emit(BackendEvent::Progress(Progress {
                    position: 0.0,
                    duration: dur,
                }));
                announce_track(&backend, &shared, &library, index, generation, track, dur).await;
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
    dur: f64,
) {
    let still_current = || is_current_generation(&shared.core.lock_safe(), generation);

    // A row liked from an album page can have no length; the stream we just
    // decoded does. Adopt it before announcing, write it into the queue row so
    // the restored session has it on the next launch, and re-publish the
    // now-playing payload: `Track` was emitted before the stream resolved, so
    // without this the UI/OS session would keep the duration-less copy.
    if adopt_fetched_duration(&mut track, dur) {
        {
            let mut core = shared.core.lock_safe();
            if let Some(slot) = core.queue.get_mut(index) {
                if slot.id == track.id && slot.duration.is_none() {
                    slot.duration = track.duration;
                }
            }
        }
        emit_queue(backend, shared);
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
        emit_queue(backend, shared);
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

/// Why a fetched crossfade target cannot be committed yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CrossfadeHold {
    /// Nothing blocks the commit: the target is still the current prefetch and
    /// playback is still inside the overlap window.
    None,
    /// Playback advanced while the target was fetching (a skip, a natural end,
    /// or a new prefetch consumed/changed the pin). This prefetch is obsolete.
    Stale,
    /// Playback was paused (or stopped) during the prefetch. Committing would
    /// call `player.play()` on the audio thread and resume against the user's
    /// wish; the pin is left for the natural end-of-track path instead.
    Paused,
    /// A backward seek pulled the position back out of the overlap window.
    /// Committing would cut the song short, so the pin is left for the natural
    /// end-of-track path (a hard cut).
    LeftWindow,
}

/// Decide whether a ready crossfade target may be committed now.
///
/// Pure, so the exactly-once rule this enforces is testable without a network
/// fetch or an audio device: a target may become current only while the prefetch
/// is still the one that is current (same generation, same pin), playback is
/// still `Playing`, and the position is still inside the overlap window. Every
/// rejection leaves the pin in `pending_next` for the natural end-of-track path
/// (or, when stale, the advance that superseded it has already taken over).
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

/// Commit a prefetched crossfade pick: `index` becomes the playing track. The
/// caller has already established that the prefetch is still current (the
/// generation is unchanged, `pending_next` still names it) and that playback is
/// still inside the overlap window. Pure so the bookkeeping it touches is
/// testable on its own; the fetched duration is applied by the caller.
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
    core.generation += 1;
    core.failures = 0;
    core.position = 0.0;
}

/// Begin a crossfade into the next track: draw it now, fetch it in the
/// background, and once its bytes are ready overlap it with the current track
/// via [`AudioCmd::Crossfade`]. Triggered by the position watcher as the current
/// track nears its end.
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
                let (dur, committed_generation) = {
                    let mut core = shared.core.lock_safe();
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
                    commit_crossfade(&mut core, index);
                    let committed_generation = core.generation;
                    core.duration = if duration > 0.0 {
                        duration
                    } else {
                        f64::from(track.duration.unwrap_or(0))
                    };
                    (core.duration, committed_generation)
                };

                let fade = std::time::Duration::from_secs_f64(fade.max(0.0));
                if shared.audio.send(AudioCmd::Crossfade(data, fade)).is_err() {
                    // The audio thread died between the prefetch and the commit.
                    // The bookkeeping already names the new track, but no audio
                    // will ever start for it: report the failure rather than
                    // leaving the UI on `Playing` with a frozen bar.
                    error!(
                        "audio thread is gone; crossfade to {} did not start",
                        track.id
                    );
                    fail_playback(
                        &backend,
                        &shared,
                        committed_generation,
                        format!(
                            "Could not play \u{201c}{}\u{201d}: audio output is unavailable",
                            track.title
                        ),
                    );
                    return;
                }
                // Playback never left the Playing state, so only the track and
                // its surfaces need refreshing.
                backend.emit(BackendEvent::Track(track.clone()));
                backend.discord.set_track(&track, dur);
                emit_state(&backend, PlaybackState::Playing);
                emit_queue(&backend, &shared);
                backend.emit(BackendEvent::Progress(Progress {
                    position: 0.0,
                    duration: dur,
                }));
                announce_track(
                    &backend,
                    &shared,
                    &library,
                    index,
                    committed_generation,
                    track,
                    dur,
                )
                .await;
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
    use super::adopt_fetched_duration;
    use rift_types::Track;

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
}
