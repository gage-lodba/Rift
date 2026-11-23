//! State owned by the audio thread and one handler per command.
//!
//! `run` in the parent module owns only the receive loop; the output device,
//! the live player and the crossfade tails live here so each command's ordering
//! rules sit next to the state they protect. Private to `audio`: the module's
//! public surface is unchanged.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rodio::stream::MixerDeviceSink;
use rodio::{Player, Source};
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::oneshot;
use tracing::{debug, error, warn};

use super::{
    checked_earlier, decode, fade_progress, seek_target, AudioEvent, CROSSFADE_CANCELLED,
    IDLE_PLAY_ID,
};

/// Decoder acknowledgement shared by [`super::AudioCmd::Play`] and
/// [`super::AudioCmd::Crossfade`].
type PlayAck = Option<oneshot::Sender<Result<f64, String>>>;

/// A track on its way out during a crossfade: its player's volume is ramped
/// from the master volume down to zero, then dropped (which stops it).
struct FadeOut {
    player: Player,
    start: Instant,
    dur: Duration,
    /// Time the fade spent paused. The ramp is wall-clock driven, so without
    /// this a pause keeps advancing it: pausing mid-crossfade then waiting
    /// would destroy the outgoing tail, and resuming would jump the ramp
    /// forward.
    paused_for: Duration,
    /// When the current pause began, if the fade is paused right now.
    paused_at: Option<Instant>,
}

/// Everything the audio thread's command loop mutates.
///
/// The output device is kept here for the whole thread: dropping the state when
/// the loop exits releases the OS sink on the thread that opened it. Keeping
/// the live player and the fade tails together is what lets `Pause`/`Resume`
/// and `Stop` act on all of them atomically.
pub(super) struct AudioState {
    /// The open OS sink, kept alive for as long as the thread runs.
    device: MixerDeviceSink,
    player: Player,
    events: UnboundedSender<AudioEvent>,
    crossfade_epoch: Arc<AtomicU64>,
    /// Tracks currently fading out behind the current one.
    fading: Vec<FadeOut>,
    /// True while a track is loaded and we should watch for it ending.
    active: bool,
    /// Play identity (the manager's generation) of the audio currently loaded.
    /// `IDLE_PLAY_ID` while nothing is playing. It is set only when a `Play` or
    /// `Crossfade` actually starts — a failed decode leaves the previous audio's
    /// identity in place — and every position/duration/end event echoes it, so
    /// the manager can discard events queued for a track it has already left.
    play_id: u64,
    /// Master volume (0.0..=1.0), applied to every player including fade-outs.
    volume: f32,
    /// Paces position ticks at ~4 Hz: a crossfade tightens the loop to 30 ms
    /// for smooth volume ramps, but the UI doesn't need 33 progress events/s.
    /// Seeded one interval back so the first tick is not delayed; the fallback in
    /// `checked_earlier` is what keeps a machine that booted moments ago from
    /// panicking here.
    last_position: Instant,
}

impl AudioState {
    /// Connect the live player to the already-open device and seed the idle
    /// loop state. The caller logs the startup line, so its timing still covers
    /// the device open plus this connect.
    pub(super) fn new(
        device: MixerDeviceSink,
        events: UnboundedSender<AudioEvent>,
        crossfade_epoch: Arc<AtomicU64>,
    ) -> Self {
        let player = Player::connect_new(device.mixer());
        Self {
            device,
            player,
            events,
            crossfade_epoch,
            fading: Vec::new(),
            active: false,
            play_id: IDLE_PLAY_ID,
            volume: 1.0,
            last_position: checked_earlier(Instant::now(), Duration::from_secs(1)),
        }
    }

    /// Tick fast enough for smooth fades while one is in progress; idle slower
    /// the rest of the time.
    pub(super) fn tick_interval(&self) -> Duration {
        if self.fading.is_empty() {
            Duration::from_millis(250)
        } else {
            Duration::from_millis(30)
        }
    }

    /// Start a hard-cut play.
    ///
    /// The decode-before-clear order is load-bearing: a payload that fails to
    /// decode must leave the previous track audible and being reported.
    pub(super) fn play(&mut self, data: Vec<u8>, ack: PlayAck, requested_play_id: u64) {
        // Decode *before* touching the live player. Clearing first
        // would silence the outgoing audio (and discard any
        // crossfade) even when the new payload turns out not to
        // decode — leaving the UI reporting Playing with an
        // advancing bar over silence until the ~250 ms tick notices
        // `player.empty()`. The Crossfade arm below has always been
        // ordered this way; this matches it.
        match decode(data) {
            Ok(source) => {
                self.player.clear();
                // A hard cut discards any in-progress crossfade.
                self.fading.clear();
                // Report the real length so the seek bar is accurate even when
                // metadata duration is missing.
                let dur = source.total_duration().map(|d| d.as_secs_f64());
                self.player.append(source);
                self.player.set_volume(self.volume);
                self.player.play();
                self.active = true;
                // The audio this thread is now playing belongs to
                // `requested_play_id`. Set before the first event is sent so
                // `Duration` and every later tick carry it.
                self.play_id = requested_play_id;
                // Restart the position-tick clock with the new audio. Without
                // this, the loop's end-of-iteration tick can fire immediately
                // (the clock is still armed from the outgoing track) and report
                // the new player's position before it has actually started —
                // which arrives as a stale end-of-previous-track position and
                // can re-anchor Discord's time bar at the end of the song.
                self.last_position = Instant::now();
                if let Some(d) = dur.filter(|d| *d > 0.0) {
                    let _ = self.events.send(AudioEvent::Duration(d, self.play_id));
                }
                // The caller commits `Playing` on this, rather than before it
                // sent the command.
                if let Some(ack) = ack {
                    let _ = ack.send(Ok(dur.unwrap_or(0.0)));
                }
            }
            Err(e) => {
                // The live player (and `active`) are left exactly as they were
                // — a previous track keeps playing and keeps being reported —
                // so the UI and the audio can never disagree. In the player's
                // own flow a `Stop` arrives before every `Play`, so there is
                // normally nothing playing to preserve.
                error!("failed to decode audio: {e}");
                let msg = format!("decode error: {e}");
                match ack {
                    // A caller is waiting on the outcome and owns the
                    // skip/stop decision; reporting it here as well would
                    // advance the queue twice.
                    Some(ack) => {
                        let _ = ack.send(Err(msg));
                    }
                    None => {
                        let _ = self.events.send(AudioEvent::DecodeFailed(msg));
                    }
                }
            }
        }
    }

    /// Start a crossfade target.
    ///
    /// The epoch check after decoding is load-bearing: a superseded or
    /// timed-out token must not touch the live player, or audio would start for
    /// a track the manager has already disowned.
    pub(super) fn crossfade(
        &mut self,
        data: Vec<u8>,
        fade: Duration,
        ack: PlayAck,
        token: u64,
        requested_play_id: u64,
    ) {
        match decode(data) {
            // The caller superseded or timed out this crossfade while it
            // was decoding: do not touch the live player, or audio would
            // start for a track the manager has already disowned.
            Ok(_) if self.crossfade_epoch.load(Ordering::SeqCst) != token => {
                debug!(
                    "dropping a crossfade that was superseded or timed out before it \
                     started"
                );
                if let Some(ack) = ack {
                    let _ = ack.send(Err(CROSSFADE_CANCELLED.to_owned()));
                }
            }
            Ok(source) => {
                let dur = source.total_duration().map(|d| d.as_secs_f64());
                // Retire the current track onto its own fade-out and
                // bring the new one up on a fresh player so the two overlap.
                let outgoing =
                    std::mem::replace(&mut self.player, Player::connect_new(self.device.mixer()));
                if self.active {
                    self.fading.push(FadeOut {
                        player: outgoing,
                        start: Instant::now(),
                        dur: fade,
                        paused_for: Duration::ZERO,
                        paused_at: None,
                    });
                }
                self.player.append(source.fade_in(fade));
                self.player.set_volume(self.volume);
                self.player.play();
                self.active = true;
                // The incoming track is now what is playing; the manager
                // sent the generation its future `commit_crossfade` will
                // assign, so events carry the identity they will have once
                // the acknowledgement is committed.
                self.play_id = requested_play_id;
                // Same fresh-clock rule as the hard `Play` arm: the new
                // track's first position must not be the outgoing one's.
                self.last_position = Instant::now();
                // With an ack, the caller commits the track only after
                // this message and applies the same length; sending
                // `Duration` too would race that commit and briefly
                // apply the incoming length to the outgoing track's
                // progress bar. The ack is the authoritative report.
                if ack.is_none() {
                    if let Some(d) = dur.filter(|d| *d > 0.0) {
                        let _ = self.events.send(AudioEvent::Duration(d, self.play_id));
                    }
                }
                // The caller commits the crossfade only on this, rather
                // than before it sent the command.
                if let Some(ack) = ack {
                    let _ = ack.send(Ok(dur.unwrap_or(0.0)));
                }
            }
            Err(e) => {
                // Leave the current track playing on decode failure, and
                // report the failure to the caller that is waiting on it:
                // the old track is still the current one, so the caller
                // must not commit the target (and must drop the prefetch
                // pin so the natural end can advance normally).
                error!("failed to decode crossfade audio: {e}");
                let msg = format!("decode error: {e}");
                match ack {
                    Some(ack) => {
                        let _ = ack.send(Err(msg));
                    }
                    // No acknowledgement was requested (nothing does
                    // this today): fall back to the event the player
                    // already handles for an unplayable track.
                    None => {
                        let _ = self.events.send(AudioEvent::DecodeFailed(msg));
                    }
                }
            }
        }
    }

    /// Pause the live player and freeze the wall-clock fade ramps at one
    /// instant, so a pause cannot consume the outgoing tail.
    pub(super) fn pause(&mut self) {
        self.player.pause();
        let now = Instant::now();
        for f in &mut self.fading {
            f.player.pause();
            // Freeze the ramp: remember when this pause started.
            // A second Pause while already paused must not restart
            // the measurement.
            if f.paused_at.is_none() {
                f.paused_at = Some(now);
            }
        }
    }

    /// Resume the live player and fold each fade's paused wall time into its
    /// accumulated pause, so the ramp continues where it stopped.
    pub(super) fn resume(&mut self) {
        self.player.play();
        for f in &mut self.fading {
            f.player.play();
            // Fold the pause into the accumulated paused time; the
            // ramp then resumes where it stopped.
            if let Some(at) = f.paused_at.take() {
                f.paused_for = f.paused_for.saturating_add(at.elapsed());
            }
        }
    }

    /// Retire the live player and every fade tail, and drop the play identity so
    /// queued events from the stopped audio cannot act on the next play.
    pub(super) fn stop(&mut self) {
        self.player.clear();
        self.fading.clear();
        self.active = false;
        // Nothing is playing any more: an event still queued from the
        // stopped audio is tagged with its old identity and must not
        // act on whatever play the manager starts next.
        self.play_id = IDLE_PLAY_ID;
    }

    /// Retire only the audio started by `expected`.
    ///
    /// A crossfade starts its target before the manager commits it; this keeps
    /// that specific audio from surviving a disown while a newer `Play` that
    /// replaced it keeps playing untouched.
    pub(super) fn stop_if_playing(&mut self, expected: u64) {
        // Only the audio started by the disowned crossfade may be
        // retired: if a newer `Play` already replaced the player, its
        // identity differs and it keeps playing untouched.
        if self.play_id == expected {
            self.stop();
        }
    }

    /// Seek the live player; an invalid target is ignored rather than panicking
    /// the unsupervised audio thread.
    pub(super) fn seek(&mut self, secs: f64) {
        match seek_target(secs) {
            Some(target) => {
                if let Err(e) = self.player.try_seek(target) {
                    error!("seek failed: {e}");
                }
            }
            None => warn!("ignoring an invalid seek target: {secs}s"),
        }
    }

    /// Set the master volume; NaN is treated as silence rather than being
    /// allowed to poison every fade volume.
    pub(super) fn volume(&mut self, v: f32) {
        // `f32::clamp` propagates NaN, and a NaN master volume would
        // poison every fade volume; NaN is not a level, so treat it
        // as silence.
        self.volume = if v.is_nan() { 0.0 } else { v.clamp(0.0, 1.0) };
        self.player.set_volume(self.volume);
    }

    /// Advance the wall-clock fade ramps and drop (and so silence) the ones that
    /// have run their course.
    ///
    /// This runs even while paused: a paused ramp's effective elapsed time does
    /// not move, so it neither advances nor completes until playback resumes.
    fn advance_fades(&mut self) {
        let now = Instant::now();
        let volume = self.volume;
        self.fading.retain(|f| {
            // The ramp is wall-clock based, so subtract the time spent paused.
            // While paused the *effective* elapsed time does not move, so the
            // fade neither advances nor completes until playback resumes.
            let wall = now.saturating_duration_since(f.start);
            let elapsed = wall.saturating_sub(f.paused_for);
            let t = fade_progress(elapsed, f.dur);
            if f.paused_at.is_none() && (t >= 1.0 || f.player.empty()) {
                false
            } else {
                f.player.set_volume(volume * (1.0 - t));
                true
            }
        });
    }

    /// One periodic pass after each command or receive timeout: ramp the fades,
    /// then report end-of-track or a position tick for the live player.
    ///
    /// The end transition clears `play_id` before sending `Ended`, so the event
    /// carries the finished track's identity and later events cannot claim it.
    pub(super) fn tick(&mut self) {
        self.advance_fades();

        if self.active {
            if self.player.empty() {
                self.active = false;
                // The ended event belongs to the track that just finished, but
                // the thread is idle again: later events (there are none until
                // the next `Play`) must not claim that identity.
                let ended_play_id = self.play_id;
                self.play_id = IDLE_PLAY_ID;
                let _ = self.events.send(AudioEvent::Ended(ended_play_id));
            } else if !self.player.is_paused()
                && self.last_position.elapsed() >= Duration::from_millis(200)
            {
                self.last_position = Instant::now();
                let _ = self.events.send(AudioEvent::Position(
                    self.player.get_pos().as_secs_f64(),
                    self.play_id,
                ));
            }
        }
    }
}
