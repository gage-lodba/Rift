//! Track resolution and playback: fetching stream bytes, starting a track,
//! backfilling credits, publishing now-playing surfaces, and crossfading.

use std::sync::Mutex;

use rift_types::{events, PlaybackState, Progress, Track};
use tauri::{AppHandle, Emitter, Manager};
use tracing::{debug, error, info};

use super::nav::{pick_next, play_next};
use super::state::{emit_queue, emit_state, PlayerCore, PlayerShared};
use crate::audio::AudioCmd;
use crate::util::LockExt;
use crate::AppState;

/// How long to wait for the audio thread to acknowledge a decode.
///
/// Decoding is from memory, so this is milliseconds in practice; the bound only
/// exists so a wedged audio thread cannot hang the playback task forever.
const DECODE_ACK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// The length to show for a track whose stream is still being fetched.
///
/// [`rift::fetch::fetch_track`] reports 0.0 when the length is unknown — it
/// hands back the raw stream rather than probing the media, and the offline path
/// short-circuits entirely — and a track whose metadata lacked a duration has
/// `None`. Taking that zero at face value zeroes the seek bar's range until the
/// decoder corrects it a moment later, which reads as the bar jumping on every
/// offline track. Pure, so the rule is testable without a decoder.
pub(crate) fn preseed_duration(track_duration: Option<u32>, previous: f64) -> f64 {
    match track_duration {
        Some(secs) => secs as f64,
        // No metadata length either: keep the length the bar already had rather
        // than collapsing it, and let the decoder's `AudioEvent::Duration`
        // supply the authoritative value.
        None => previous,
    }
}

/// Start playing the track at `index`, treating it as a fresh anchor for the
/// shuffle history. Used by direct jumps — clicking a track, starting a
/// collection, resuming a stopped track. Sequential and shuffle Next/Previous
/// go through [`play_next`]/[`super::play_prev`], which preserve the history
/// instead.
pub fn play_index(app: &AppHandle, index: usize) {
    {
        let state = app.state::<AppState>();
        let mut core = state.player.core.lock_safe();
        if index >= core.queue.len() {
            return;
        }
        // A deliberate jump is a fresh start: give the queue a full retry budget.
        core.failures = 0;
        super::nav::anchor_playing(&mut core, index);
    }
    start_playback(app, index);
}

/// Resolve and play the track at `index`. Assumes the shuffle history has
/// already been positioned by the caller.
pub(crate) fn start_playback(app: &AppHandle, index: usize) {
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
        core.duration = preseed_duration(track.duration, core.duration);
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
        match rift::fetch::fetch_track(&downloads.dir, &track.id).await {
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
                    error!("audio thread is gone; cannot play {}", track.id);
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
                        {
                            let mut core = shared.core.lock_safe();
                            if core.generation != generation {
                                return;
                            }
                            core.state = PlaybackState::Stopped;
                        }
                        let _ = app.emit(
                            events::ERROR,
                            format!("Could not play \u{201c}{}\u{201d}: {msg}", track.title),
                        );
                        emit_state(&app, PlaybackState::Stopped);
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
                app.state::<AppState>().discord.set_track(&track, dur);
                emit_state(&app, PlaybackState::Playing);
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
    if track.album_id.is_none() {
        if track.album.is_none() {
            track.album = album;
        }
        track.album_id = album_id;
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

    if enrich_track(&shared.yt, &mut track).await {
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

/// Commit a prefetched crossfade pick: `index` becomes the playing track. The
/// caller has already established that the prefetch is still current (the
/// generation is unchanged, `pending_next` still names it) and that playback is
/// still inside the overlap window. Pure so the bookkeeping it touches is
/// testable on its own; the fetched duration is applied by the caller.
pub(crate) fn commit_crossfade(core: &mut PlayerCore, index: usize) {
    core.pending_next = None;
    // Anchor the shuffle cycle on the pick before it becomes current. Normally
    // it is already the cursor's entry — the prefetch drew it through
    // `pick_next` — but the pair can be rewritten between the draw and the
    // commit (`toggle_shuffle` re-anchors the history and deliberately keeps the
    // pick), and a pick the cursor does not name stays in the draw pool, so Next
    // could redraw the track that just started.
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
pub(crate) fn begin_crossfade(app: &AppHandle) {
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
        match rift::fetch::fetch_track(&downloads.dir, &track.id).await {
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
                    // A backward seek during the prefetch may have pulled the
                    // track back out of the overlap window. Committing would cut
                    // the song short, so leave the pick pending for the natural
                    // end-of-track path (a hard cut) instead.
                    if core.duration > 0.0 && core.duration - core.position > core.crossfade {
                        debug!("crossfade target ready but position left the overlap window; deferring");
                        return;
                    }
                    commit_crossfade(&mut core, index);
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
