//! Dedicated audio thread owning the rodio output device.
//!
//! rodio's output stream is not `Send`, so it lives on its own OS thread and
//! is driven through a channel. Position/end-of-track notifications flow back
//! through a tokio channel to the player manager.
//!
//! Every notification is tagged with the *play identity* of the audio it came
//! from: the manager's player generation that a [`AudioCmd::Play`] or
//! [`AudioCmd::Crossfade`] was sent for. The audio thread keeps one identity for
//! whatever it is currently playing and only changes it when new audio actually
//! starts (or when playback stops). That lets the manager drop a queued event
//! from a track the user already stopped or skipped past instead of applying it
//! to the new track.
//!
//! The receive loop is deliberately thin: the output device, the live player,
//! the fade-out tails and the play identity live in the private `AudioState`
//! (`audio::output`), and every [`AudioCmd`] is one method there. Keeping the
//! ordering rules — decode before touching the live player, play-identity
//! assignment, acknowledgement order — next to the state they protect is the
//! point of the split.

use std::io::Cursor;
use std::sync::atomic::AtomicU64;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rodio::stream::DeviceSinkBuilder;
use rodio::Decoder;
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::oneshot;
use tracing::{error, info};

mod output;

use output::AudioState;

/// Ack reason the audio thread reports when the crossfade token moved before it
/// could start the target. The manager treats this as a harmless supersession
/// (no error toast): the command was invalidated by a newer queue/playback
/// action, or by the manager's own acknowledgement timeout.
pub(crate) const CROSSFADE_CANCELLED: &str = "crossfade was cancelled";

/// The play identity while no track is loaded. A `Play`/`Crossfade` replaces it
/// only when its audio actually starts; `Stop` restores it.
const IDLE_PLAY_ID: u64 = 0;

pub enum AudioCmd {
    /// Decode and play a full in-memory audio file, replacing whatever plays.
    ///
    /// The optional ack reports whether the bytes decoded, so the caller can
    /// commit `Playing` only once there is actually audio — a decode failure
    /// must never leave the UI advancing a progress bar over silence.
    ///
    /// `play_id` is the manager's identity for this play — its player
    /// generation. Once the bytes start, the audio thread echoes it on every
    /// [`AudioEvent::Position`], [`AudioEvent::Duration`] and
    /// [`AudioEvent::Ended`] until another request starts or playback stops. A
    /// decode failure leaves the current identity alone: the previous audio is
    /// still what is playing, so its events must keep that identity.
    Play(Vec<u8>, Option<oneshot::Sender<Result<f64, String>>>, u64),
    /// Decode and play a new track, fading it in over the given duration while
    /// the current track fades out over the same window (a crossfade).
    ///
    /// Like [`AudioCmd::Play`], the optional ack reports the decoder's length
    /// (0.0 when unknown) once the fade has started. The audio thread decodes
    /// before touching the live player: on failure it reports the error and the
    /// current track keeps playing untouched, so the caller can keep its own
    /// state on the old track instead of committing a target the user cannot
    /// hear.
    ///
    /// `token` is the crossfade cancellation token the thread re-checks against
    /// the shared epoch before starting (see [`CROSSFADE_CANCELLED`]). `play_id`
    /// is the identity the incoming target will carry: the manager sends the
    /// generation `commit_crossfade` will assign when the acknowledgement lands
    /// (`anchor_generation + 1`). The target's events are dropped by the manager
    /// until that commit makes its generation current.
    Crossfade(
        Vec<u8>,
        Duration,
        Option<oneshot::Sender<Result<f64, String>>>,
        u64,
        u64,
    ),
    Pause,
    Resume,
    Stop,
    /// Stop playback only if `play_id` is still the audio thread's live play.
    ///
    /// A crossfade starts the target before the manager commits it; if the
    /// manager then discovers the target is no longer in the queue, it must
    /// silence that specific audio without cutting off a newer `Play` that may
    /// have replaced it in the meantime. `Stop` is unconditional and would race
    /// that newer play, so this conditional retire exists for that window only.
    StopIfPlaying(u64),
    /// Seek to an absolute position in seconds.
    Seek(f64),
    /// Volume in 0.0..=1.0.
    Volume(f32),
    /// Shut the thread down: stop playback and leave the loop. Sent by the
    /// backend when it is dropped so the thread and its output device are
    /// released deterministically instead of at process exit. The play identity
    /// is dropped with the thread; no events follow.
    Shutdown,
}

/// Decode a full in-memory audio file. The explicit byte length marks the
/// source as seekable; without it symphonia treats the stream as forward-only,
/// and a backward seek in a fragmented MP4 (`YouTube` m4a) leaves the demuxer
/// unable to rewind — the source drains and the track appears to have ended.
// Compile-time guard for the Linux sound-server routing. `HostId::PulseAudio`
// only exists when the cpal instance rodio links is built with its `pulseaudio`
// host feature, which the Linux-only direct `cpal` dependency in `Cargo.toml`
// enables through feature unification. A dependency change that drops the
// feature, or that makes the two cpal versions diverge, fails this build instead
// of silently routing Linux playback back to a raw ALSA card.
#[cfg(target_os = "linux")]
const _: rodio::cpal::HostId = rodio::cpal::HostId::PulseAudio;

/// Fade-out progress in `0.0..=1.0` for an *effective* elapsed time.
///
/// Pure so the pause arithmetic and the clamp are testable without an audio
/// device. A zero duration is treated as one millisecond so the division cannot
/// produce NaN/infinity.
fn fade_progress(elapsed: Duration, dur: Duration) -> f32 {
    (elapsed.as_secs_f32() / dur.as_secs_f32().max(0.001)).clamp(0.0, 1.0)
}

fn decode(data: Vec<u8>) -> Result<Decoder<Cursor<Vec<u8>>>, rodio::decoder::DecoderError> {
    let len = data.len() as u64;
    Decoder::builder()
        .with_data(Cursor::new(data))
        .with_byte_len(len)
        .with_seekable(true)
        .build()
}

pub enum AudioEvent {
    /// Authoritative track length in seconds, read from the decoder, tagged with
    /// the play identity it belongs to.
    Duration(f64, u64),
    /// Current playback position in seconds, tagged with the play identity it
    /// belongs to.
    Position(f64, u64),
    /// The current track finished playing. The tag is the identity of the track
    /// that ended, so a queued end from the outgoing track cannot advance the
    /// playback that replaced it.
    Ended(u64),
    /// The current track's bytes wouldn't decode (e.g. a corrupt offline
    /// file). The track is unplayable, but the queue can still advance.
    DecodeFailed(String),
    /// Device-level failure; playback can't continue at all. The thread stops
    /// after reporting it, so no play identity survives this event and no tagged
    /// [`AudioEvent::Position`]/[`AudioEvent::Duration`]/[`AudioEvent::Ended`]
    /// follows it.
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
pub fn spawn(
    events: UnboundedSender<AudioEvent>,
    crossfade_epoch: Arc<AtomicU64>,
) -> Sender<AudioCmd> {
    let (tx, rx) = mpsc::channel();
    if let Err(e) = std::thread::Builder::new()
        .name("rift-audio".into())
        .spawn(move || run(rx, events, crossfade_epoch))
    {
        error!("could not spawn the audio thread, playback disabled: {e}");
    }
    tx
}

fn run(
    rx: Receiver<AudioCmd>,
    events: UnboundedSender<AudioEvent>,
    crossfade_epoch: Arc<AtomicU64>,
) {
    let opened = std::time::Instant::now();
    // Which cpal host the default sink will come from. On Linux that is the
    // session's sound server (cpal's PulseAudio host; PipeWire serves the same
    // protocol through pipewire-pulse) when one is running, and raw ALSA
    // otherwise. Only the former follows the sink the user configured in the
    // desktop mixer. Nothing used to record this — a raw-ALSA open that grabbed
    // a card directly looked exactly like a healthy device open in the logs — so
    // the choice is now part of the startup diagnostics.
    #[cfg(target_os = "linux")]
    info!(
        "audio output host: {}",
        rodio::cpal::default_host().id().name()
    );
    let device = match DeviceSinkBuilder::open_default_sink() {
        Ok(d) => d,
        Err(e) => {
            error!("failed to open audio output device: {e}");
            let _ = events.send(AudioEvent::Failed(format!("no audio output device: {e}")));
            return;
        }
    };
    let mut state = AudioState::new(device, events, crossfade_epoch);
    info!(
        "audio thread started (+{:.1} ms opening the default device)",
        opened.elapsed().as_secs_f64() * 1000.0
    );

    loop {
        // Tick fast enough for smooth fades while one is in progress; idle
        // slower the rest of the time.
        match rx.recv_timeout(state.tick_interval()) {
            Ok(cmd) => match cmd {
                AudioCmd::Play(data, ack, requested_play_id) => {
                    state.play(data, ack, requested_play_id);
                }
                AudioCmd::Crossfade(data, fade, ack, token, requested_play_id) => {
                    state.crossfade(data, fade, ack, token, requested_play_id);
                }
                AudioCmd::Pause => state.pause(),
                AudioCmd::Resume => state.resume(),
                AudioCmd::Stop => state.stop(),
                AudioCmd::StopIfPlaying(expected) => state.stop_if_playing(expected),
                AudioCmd::Seek(secs) => state.seek(secs),
                AudioCmd::Volume(v) => state.volume(v),
                // The backend is shutting down: silence playback and leave the
                // loop, dropping the output device on this thread.
                AudioCmd::Shutdown => break,
            },
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }

        state.tick();
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

    /// The Linux output path must follow the desktop's sound server.
    ///
    /// `DeviceSinkBuilder::open_default_sink()` resolves its device through
    /// `cpal::default_host()`. Without a sound-server host compiled in, that is
    /// raw ALSA, and ALSA's `default` PCM is a hardware card: playback goes
    /// wherever card enumeration happens to land instead of to the sink the
    /// user configured, and the sound server loses its own access to the card
    /// the app grabbed (`DeviceBusy`). The host is enabled by the Linux-only
    /// `cpal` dependency in `Cargo.toml`; this guard fails if a dependency
    /// change ever drops it again.
    ///
    /// Self-skips when no sound server is reachable (headless CI), exactly as
    /// the GPU tests skip without a Vulkan device: with no server to route to,
    /// raw ALSA is the correct fallback rather than a regression.
    #[cfg(target_os = "linux")]
    #[test]
    fn linux_audio_host_prefers_the_running_sound_server() {
        // Mirror `cpal::default_host()`'s own decision: a non-ALSA host only
        // wins when it can actually be constructed, not merely when its socket
        // is advertised (`is_available`). A stale server socket passes the
        // advertisement and then fails to connect, and `default_host` correctly
        // falls back to ALSA in that case — which must not fail this test.
        let sound_server_usable = rodio::cpal::available_hosts()
            .into_iter()
            .filter(|host| *host != rodio::cpal::HostId::Alsa)
            .any(|host| rodio::cpal::host_from_id(host).is_ok());
        if !sound_server_usable {
            eprintln!("skipping: no usable sound server host (PulseAudio/PipeWire)");
            return;
        }
        let selected = rodio::cpal::default_host().id();
        assert_ne!(
            selected,
            rodio::cpal::HostId::Alsa,
            "a sound server is running, but the default audio host is raw ALSA: \
             playback would open a hardware card instead of the user's sink"
        );
    }

    /// The wall-clock ramp must pause with playback: a fade that spent time
    /// paused has a smaller effective elapsed time, so it neither completes nor
    /// jumps forward while paused / on resume.
    #[test]
    fn a_paused_fade_does_not_advance_or_complete() {
        use super::fade_progress;

        let dur = Duration::from_secs(10);
        // One second of playback, then a ten-second pause: wall time is 11 s
        // but effective elapsed is still 1 s, so the tail survives the pause.
        assert!((fade_progress(Duration::from_secs(1), dur) - 0.1).abs() < 1e-4);
        let wall = Duration::from_secs(11);
        let paused_for = Duration::from_secs(10);
        let effective = wall.saturating_sub(paused_for);
        assert!((fade_progress(effective, dur) - 0.1).abs() < 1e-4);
        assert!(
            fade_progress(effective, dur) < 1.0,
            "a pause must not finish the fade"
        );

        // Past the configured duration clamps to a completed ramp.
        assert_eq!(fade_progress(Duration::from_secs(30), dur), 1.0);
        // A zero duration is a hard cut, not a division by zero.
        assert_eq!(fade_progress(Duration::from_secs(1), Duration::ZERO), 1.0);
    }
}
