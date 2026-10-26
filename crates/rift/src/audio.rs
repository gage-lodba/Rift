//! Dedicated audio thread owning the rodio output device.
//!
//! rodio's output stream is not `Send`, so it lives on its own OS thread and
//! is driven through a channel. Position/end-of-track notifications flow back
//! through a tokio channel to the player manager.

use std::io::Cursor;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use rodio::stream::DeviceSinkBuilder;
use rodio::{Decoder, Player, Source};
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::oneshot;
use tracing::{error, info, warn};

pub enum AudioCmd {
    /// Decode and play a full in-memory audio file, replacing whatever plays.
    ///
    /// The optional ack reports whether the bytes decoded, so the caller can
    /// commit `Playing` only once there is actually audio — a decode failure
    /// must never leave the UI advancing a progress bar over silence.
    Play(Vec<u8>, Option<oneshot::Sender<Result<f64, String>>>),
    /// Decode and play a new track, fading it in over the given duration while
    /// the current track fades out over the same window (a crossfade).
    Crossfade(Vec<u8>, Duration),
    Pause,
    Resume,
    Stop,
    /// Seek to an absolute position in seconds.
    Seek(f64),
    /// Volume in 0.0..=1.0.
    Volume(f32),
    /// Shut the thread down: stop playback and leave the loop. Sent by the
    /// backend when it is dropped so the thread and its output device are
    /// released deterministically instead of at process exit.
    Shutdown,
}

/// A track on its way out during a crossfade: its player's volume is ramped
/// from the master volume down to zero, then dropped (which stops it).
struct FadeOut {
    player: Player,
    start: Instant,
    dur: Duration,
}

/// Decode a full in-memory audio file. The explicit byte length marks the
/// source as seekable; without it symphonia treats the stream as forward-only,
/// and a backward seek in a fragmented MP4 (`YouTube` m4a) leaves the demuxer
/// unable to rewind — the source drains and the track appears to have ended.
fn decode(data: Vec<u8>) -> Result<Decoder<Cursor<Vec<u8>>>, rodio::decoder::DecoderError> {
    let len = data.len() as u64;
    Decoder::builder()
        .with_data(Cursor::new(data))
        .with_byte_len(len)
        .with_seekable(true)
        .build()
}

pub enum AudioEvent {
    /// Authoritative track length in seconds, read from the decoder.
    Duration(f64),
    /// Current playback position in seconds.
    Position(f64),
    /// The current track finished playing.
    Ended,
    /// The current track's bytes wouldn't decode (e.g. a corrupt offline
    /// file). The track is unplayable, but the queue can still advance.
    DecodeFailed(String),
    /// Device-level failure; playback can't continue at all.
    Failed(String),
}

/// `instant - duration`, falling back to `instant` when the instant is too close
/// to its epoch to subtract.
///
/// An `Instant` is a monotonic count since boot (QPC on Windows, CLOCK_MONOTONIC
/// elsewhere), so `Instant::now() - 1s` panics on a machine that started Rift
/// within a second of booting. The tick clock just needs to be "one interval in
/// the past" — if that predates the epoch, "now" is the safe answer.
fn checked_earlier(instant: Instant, duration: Duration) -> Instant {
    instant.checked_sub(duration).unwrap_or(instant)
}

/// Convert a seek target in seconds into a `Duration` the player can seek to.
///
/// `Duration::from_secs_f64` panics on NaN, infinities and out-of-range values,
/// and this conversion runs on the audio thread, which has no supervisor: one
/// absurd target would silence playback (and drop the output device) for the
/// rest of the session. `None` means "not a position" and the seek is skipped.
fn seek_target(secs: f64) -> Option<Duration> {
    // `f64::max` ignores NaN, so a NaN target becomes a seek to the start, which
    // is the same treatment the player's own clamp gives a negative position.
    Duration::try_from_secs_f64(secs.max(0.0)).ok()
}

/// Spawn the audio thread, returning the command sender.
///
/// A failure to spawn the thread is logged rather than panicking: the backend
/// still starts (browsing, the library and settings all work) and every audio
/// command is silently dropped, which is the same degraded state as a machine
/// with no output device. The returned sender stays valid either way.
pub fn spawn(events: UnboundedSender<AudioEvent>) -> Sender<AudioCmd> {
    let (tx, rx) = mpsc::channel();
    if let Err(e) = std::thread::Builder::new()
        .name("rift-audio".into())
        .spawn(move || run(rx, events))
    {
        error!("could not spawn the audio thread, playback disabled: {e}");
    }
    tx
}

fn run(rx: Receiver<AudioCmd>, events: UnboundedSender<AudioEvent>) {
    let opened = std::time::Instant::now();
    let device = match DeviceSinkBuilder::open_default_sink() {
        Ok(d) => d,
        Err(e) => {
            error!("failed to open audio output device: {e}");
            let _ = events.send(AudioEvent::Failed(format!("no audio output device: {e}")));
            return;
        }
    };
    let mut player = Player::connect_new(device.mixer());
    info!(
        "audio thread started (+{:.1} ms opening the default device)",
        opened.elapsed().as_secs_f64() * 1000.0
    );

    // True while a track is loaded and we should watch for it ending.
    let mut active = false;
    // Master volume (0.0..=1.0), applied to every player including fade-outs.
    let mut volume = 1.0f32;
    // Tracks currently fading out behind the current one.
    let mut fading: Vec<FadeOut> = Vec::new();
    // Paces position ticks at ~4 Hz: a crossfade tightens the loop to 30 ms
    // for smooth volume ramps, but the UI doesn't need 33 progress events/s.
    // Seeded one interval back so the first tick is not delayed; the fallback in
    // `checked_earlier` is what keeps a machine that booted moments ago from
    // panicking here.
    let mut last_position = checked_earlier(Instant::now(), Duration::from_secs(1));

    loop {
        // Tick fast enough for smooth fades while one is in progress; idle
        // slower the rest of the time.
        let timeout = if fading.is_empty() {
            Duration::from_millis(250)
        } else {
            Duration::from_millis(30)
        };
        match rx.recv_timeout(timeout) {
            Ok(cmd) => match cmd {
                AudioCmd::Play(data, ack) => {
                    // Decode *before* touching the live player. Clearing first
                    // would silence the outgoing audio (and discard any
                    // crossfade) even when the new payload turns out not to
                    // decode — leaving the UI reporting Playing with an
                    // advancing bar over silence until the ~250 ms tick notices
                    // `player.empty()`. The Crossfade arm below has always been
                    // ordered this way; this matches it.
                    match decode(data) {
                        Ok(source) => {
                            player.clear();
                            // A hard cut discards any in-progress crossfade.
                            fading.clear();
                            // Report the real length so the seek bar is
                            // accurate even when metadata duration is missing.
                            let dur = source.total_duration().map(|d| d.as_secs_f64());
                            player.append(source);
                            player.set_volume(volume);
                            player.play();
                            active = true;
                            // Restart the position-tick clock with the new audio.
                            // Without this, the loop's end-of-iteration tick can
                            // fire immediately (the clock is still armed from the
                            // outgoing track) and report the new player's position
                            // before it has actually started — which arrives as a
                            // stale end-of-previous-track position and can re-anchor
                            // Discord's time bar at the end of the song.
                            last_position = Instant::now();
                            if let Some(d) = dur.filter(|d| *d > 0.0) {
                                let _ = events.send(AudioEvent::Duration(d));
                            }
                            // The caller commits `Playing` on this, rather than
                            // before it sent the command.
                            if let Some(ack) = ack {
                                let _ = ack.send(Ok(dur.unwrap_or(0.0)));
                            }
                        }
                        Err(e) => {
                            // The live player (and `active`) are left exactly as
                            // they were — a previous track keeps playing and
                            // keeps being reported — so the UI and the audio can
                            // never disagree. In the player's own flow a `Stop`
                            // arrives before every `Play`, so there is normally
                            // nothing playing to preserve.
                            error!("failed to decode audio: {e}");
                            let msg = format!("decode error: {e}");
                            match ack {
                                // A caller is waiting on the outcome and owns
                                // the skip/stop decision; reporting it here as
                                // well would advance the queue twice.
                                Some(ack) => {
                                    let _ = ack.send(Err(msg));
                                }
                                None => {
                                    let _ = events.send(AudioEvent::DecodeFailed(msg));
                                }
                            }
                        }
                    }
                }
                AudioCmd::Crossfade(data, fade) => match decode(data) {
                    Ok(source) => {
                        let dur = source.total_duration().map(|d| d.as_secs_f64());
                        // Retire the current track onto its own fade-out and
                        // bring the new one up on a fresh player so the two
                        // overlap.
                        let outgoing =
                            std::mem::replace(&mut player, Player::connect_new(device.mixer()));
                        if active {
                            fading.push(FadeOut {
                                player: outgoing,
                                start: Instant::now(),
                                dur: fade,
                            });
                        }
                        player.append(source.fade_in(fade));
                        player.set_volume(volume);
                        player.play();
                        active = true;
                        // Same fresh-clock rule as the hard `Play` arm: the new
                        // track's first position must not be the outgoing one's.
                        last_position = Instant::now();
                        if let Some(d) = dur.filter(|d| *d > 0.0) {
                            let _ = events.send(AudioEvent::Duration(d));
                        }
                    }
                    Err(e) => {
                        // Leave the current track playing on decode failure; the
                        // queue will advance normally when it ends.
                        error!("failed to decode crossfade audio: {e}");
                    }
                },
                AudioCmd::Pause => {
                    player.pause();
                    for f in &fading {
                        f.player.pause();
                    }
                }
                AudioCmd::Resume => {
                    player.play();
                    for f in &fading {
                        f.player.play();
                    }
                }
                AudioCmd::Stop => {
                    player.clear();
                    fading.clear();
                    active = false;
                }
                AudioCmd::Seek(secs) => match seek_target(secs) {
                    Some(target) => {
                        if let Err(e) = player.try_seek(target) {
                            error!("seek failed: {e}");
                        }
                    }
                    None => warn!("ignoring an invalid seek target: {secs}s"),
                },
                AudioCmd::Volume(v) => {
                    // `f32::clamp` propagates NaN, and a NaN master volume would
                    // poison every fade volume; NaN is not a level, so treat it
                    // as silence.
                    volume = if v.is_nan() { 0.0 } else { v.clamp(0.0, 1.0) };
                    player.set_volume(volume);
                }
                // The backend is shutting down: silence playback and leave the
                // loop, dropping the output device on this thread.
                AudioCmd::Shutdown => break,
            },
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }

        // Advance any fade-outs, dropping (and so silencing) the ones that have
        // run their course.
        fading.retain(|f| {
            let t = f.start.elapsed().as_secs_f32() / f.dur.as_secs_f32().max(0.001);
            if t >= 1.0 || f.player.empty() {
                false
            } else {
                f.player.set_volume(volume * (1.0 - t));
                true
            }
        });

        if active {
            if player.empty() {
                active = false;
                let _ = events.send(AudioEvent::Ended);
            } else if !player.is_paused() && last_position.elapsed() >= Duration::from_millis(200) {
                last_position = Instant::now();
                let _ = events.send(AudioEvent::Position(player.get_pos().as_secs_f64()));
            }
        }
    }
    info!("audio thread stopped");
}

#[cfg(test)]
mod tests {
    use super::{checked_earlier, seek_target};
    use std::time::{Duration, Instant};

    /// The tick clock is seeded one interval back; subtracting more than the
    /// machine's uptime must degrade to "now" instead of panicking. This test
    /// would panic against the plain `Instant - Duration` form.
    #[test]
    fn seeding_the_tick_clock_cannot_underflow_an_instant() {
        let now = Instant::now();
        assert_eq!(
            checked_earlier(now, Duration::from_secs(1)),
            now.checked_sub(Duration::from_secs(1)).unwrap_or(now)
        );
        // More than any plausible uptime: `Instant` subtraction would overflow,
        // the fallback just starts from now.
        assert_eq!(checked_earlier(now, Duration::from_secs(u64::MAX)), now);
    }

    /// The seek conversion is the audio thread's only `f64` → `Duration` bridge,
    /// and `Duration::from_secs_f64` panics on values the thread can never
    /// recover from. Only representable, non-negative targets may become seeks.
    #[test]
    fn seek_targets_are_converted_without_panicking() {
        assert_eq!(seek_target(12.5), Some(Duration::from_secs_f64(12.5)));
        assert_eq!(seek_target(0.0), Some(Duration::ZERO));
        // A negative target seeks to the start, exactly as the player clamps a
        // negative position.
        assert_eq!(seek_target(-3.0), Some(Duration::ZERO));
        // NaN is not a position; `max(0.0)` maps it to the start.
        assert_eq!(seek_target(f64::NAN), Some(Duration::ZERO));
        // Unrepresentable values are dropped instead of panicking the thread.
        assert_eq!(seek_target(f64::INFINITY), None);
        assert_eq!(seek_target(f64::NEG_INFINITY), Some(Duration::ZERO));
        assert_eq!(seek_target(f64::MAX), None);
    }
}
