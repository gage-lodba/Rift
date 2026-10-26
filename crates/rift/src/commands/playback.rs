//! Playback control commands: load/play tracks, transport, volume, shuffle,
//! repeat, and related persisted settings.

use rift_types::{PlaybackState, Progress, Track};
use tracing::warn;

use crate::audio::AudioCmd;
use crate::backend::BackendHandle;
use crate::event::BackendEvent;
use crate::player;
use crate::util::LockExt;

impl BackendHandle {
    /// Replace the queue with `tracks` and start playing at `start`.
    ///
    /// This is the async command the old frontend invoked as `play_tracks`; it
    /// only queues and starts the playback task, so it returns immediately after
    /// validating the index. In preview mode it is a no-op (placeholder rows
    /// carry ids that cannot be fetched).
    ///
    /// # Errors
    ///
    /// Returns `"nothing to play"` when `tracks` is empty or `start` is out of
    /// range. Fetch/decode failures happen later in the spawned playback task
    /// and are reported through [`BackendEvent::Error`], not here.
    pub async fn play_tracks(
        &self,
        tracks: Vec<Track>,
        start: usize,
        source: Option<String>,
    ) -> Result<(), String> {
        if crate::settings::preview_mode() {
            return Ok(());
        }
        if tracks.is_empty() || start >= tracks.len() {
            return Err("nothing to play".into());
        }
        {
            let mut core = self.inner.player.core.lock_safe();
            core.queue = tracks;
            core.source = source;
            core.shuffle_history.clear();
            core.shuffle_cursor = 0;
        }
        player::play_index(&self.inner, start);
        Ok(())
    }

    /// Play a single track, replacing the queue with it.
    ///
    /// # Errors
    ///
    /// Currently never fails; the `Result` is part of the preserved command
    /// signature, and a track that cannot be fetched surfaces later through
    /// [`BackendEvent::Error`].
    pub async fn play_track(&self, track: Track) -> Result<(), String> {
        if crate::settings::preview_mode() {
            return Ok(());
        }
        {
            let mut core = self.inner.player.core.lock_safe();
            core.queue = vec![track];
            core.source = None;
            core.shuffle_history.clear();
            core.shuffle_cursor = 0;
        }
        player::play_index(&self.inner, 0);
        Ok(())
    }

    /// Toggle play/pause, resume a stopped track, or cancel an in-flight load.
    pub fn toggle_play(&self) {
        player::toggle_playback(&self.inner);
    }

    /// Skip to the next track (manual: wraps at the end, overrides repeat-one).
    pub fn next_track(&self) {
        player::play_next(&self.inner, true);
    }

    /// Go back to the previous track (restarts the current one when near its
    /// start, mirroring the old frontend behaviour).
    pub fn prev_track(&self) {
        player::play_prev(&self.inner);
    }

    /// Seek the current track to `position` seconds.
    ///
    /// No-op unless playback is playing or paused, and a non-finite (`NaN`)
    /// position is ignored rather than stored. Emits a
    /// [`BackendEvent::Progress`] so the seek bar lands immediately rather than
    /// waiting for the next audio-thread tick, and re-syncs Discord's time bar.
    pub fn seek(&self, position: f64) {
        let (progress, pstate) = {
            let mut core = self.inner.player.core.lock_safe();
            if core.state != PlaybackState::Playing && core.state != PlaybackState::Paused {
                return;
            }
            let Some(position) = sanitize_seek(position, core.duration.max(0.0)) else {
                // A NaN position would wedge the seek bar and the crossfade arm
                // permanently (every comparison with it is false, so nothing can
                // ever reset it). Do nothing rather than jumping to the start.
                warn!("ignoring a non-finite seek position");
                return;
            };
            core.position = position;
            // Re-evaluate the crossfade arm from the new position: a backward seek
            // out of the overlap window should let the crossfade arm again.
            core.crossfade_armed_for = None;
            (
                Progress {
                    position: core.position,
                    duration: core.duration,
                },
                core.state,
            )
        };
        let _ = self
            .inner
            .player
            .audio
            .send(AudioCmd::Seek(progress.position));
        self.inner.emit(BackendEvent::Progress(progress));
        // Re-sync Discord's elapsed/remaining bar to the new position.
        self.inner.discord.set_state(pstate, progress.position);
    }

    /// Live volume change (fires continuously while the slider is dragged):
    /// updates the audio thread and in-memory state but does not touch disk.
    /// Clamped to `0.0..=1.0`; `NaN` mutes (0.0) rather than being stored.
    pub fn set_volume(&self, volume: f32) {
        // The same NaN rule `SettingsStore` applies, so the player state and the
        // persisted value cannot diverge (see `clamp_setting`).
        let volume = clamp_setting(volume, 1.0);
        self.inner.player.core.lock_safe().volume = volume;
        let _ = self.inner.player.audio.send(AudioCmd::Volume(volume));
    }

    /// Persist the volume (fired once when the slider is released) so it survives
    /// a restart. Kept separate from [`set_volume`](Self::set_volume) to avoid a
    /// disk write per tick. The write is queued on the backend's durable writer,
    /// so this is safe to call from the UI thread; the backend flushes on
    /// shutdown. Clamped (and `NaN` mapped to 0.0) exactly like `set_volume`.
    pub fn save_volume(&self, volume: f32) {
        self.inner
            .settings
            .lock_safe()
            .set_volume(clamp_setting(volume, 1.0));
    }

    /// Enable or disable Discord Rich Presence and persist the choice. The
    /// Discord thread retains the last-known track, so toggling on immediately
    /// re-advertises whatever is currently playing (and toggling off clears the
    /// presence). The settings write is queued off-thread, like every other
    /// persisted setting.
    pub fn set_discord_rpc(&self, enabled: bool) {
        self.inner.settings.lock_safe().set_discord_rpc(enabled);
        self.inner.discord.set_enabled(enabled);
    }

    /// Set the crossfade overlap (in seconds; 0 disables it) and persist it.
    /// Takes effect on the next track transition. Clamped to
    /// `0.0..=MAX_CROSSFADE`; `NaN` disables it (0.0) rather than being stored.
    /// The settings write is queued on the durable writer rather than performed
    /// here, so the UI thread does not block on it.
    pub fn set_crossfade(&self, secs: f32) {
        // Same NaN rule as volume: NaN would serialize as JSON `null` and make
        // the next launch treat settings.json as corrupt.
        let secs = clamp_setting(secs, crate::settings::MAX_CROSSFADE);
        let clamped = self.inner.settings.lock_safe().set_crossfade(secs);
        self.inner.player.core.lock_safe().crossfade = f64::from(clamped);
    }

    /// Toggle shuffle mode, re-anchoring the shuffle cycle on the current track.
    pub fn toggle_shuffle(&self) {
        {
            let mut core = self.inner.player.core.lock_safe();
            crate::player::toggle_shuffle(&mut core);
        }
        player::emit_queue(&self.inner, &self.inner.player);
    }

    /// Cycle repeat mode Off → All → One → Off.
    pub fn cycle_repeat(&self) {
        {
            let mut core = self.inner.player.core.lock_safe();
            crate::player::cycle_repeat(&mut core);
        }
        player::emit_queue(&self.inner, &self.inner.player);
    }
}

/// Clamp a volume/crossfade command into `0.0..=max`, mapping NaN to the low
/// end.
///
/// `f32::clamp` propagates NaN. A NaN that reached the player core would never
/// compare equal again, and a NaN that reached `SettingsStore` would be written
/// to `settings.json` as JSON `null` (serde_json's representation of a
/// non-finite float), which cannot be read back as a number — the next launch
/// would move the user's whole settings file aside as corrupt and reset every
/// setting. `SettingsStore` already maps NaN to `0.0`; applying the same rule
/// here keeps the in-memory player state identical to the persisted value
/// instead of letting the two diverge.
fn clamp_setting(value: f32, max: f32) -> f32 {
    if value.is_nan() {
        0.0
    } else {
        value.clamp(0.0, max)
    }
}

/// Clamp a seek position into `0.0..=duration`, treating NaN as "no seek".
///
/// NaN is ignored rather than mapped to 0.0: jumping to the start on a bad
/// command is worse than not seeking at all, and (unlike volume) there is no
/// persisted value to keep consistent. Infinity clamps into the track's range.
fn sanitize_seek(position: f64, duration: f64) -> Option<f64> {
    (!position.is_nan()).then(|| position.clamp(0.0, duration))
}

#[cfg(test)]
mod tests {
    use super::{clamp_setting, sanitize_seek};

    /// A NaN must never reach `PlayerCore` or `settings.json`; finite values are
    /// clamped and infinities clamp to the ends.
    #[test]
    fn volume_and_crossfade_values_are_finite_and_in_range() {
        assert_eq!(clamp_setting(f32::NAN, 1.0), 0.0, "NaN volume mutes");
        assert_eq!(
            clamp_setting(f32::NAN, crate::settings::MAX_CROSSFADE),
            0.0,
            "NaN crossfade disables it"
        );
        assert_eq!(clamp_setting(0.5, 1.0), 0.5);
        assert_eq!(clamp_setting(-2.0, 1.0), 0.0);
        assert_eq!(clamp_setting(3.0, 1.0), 1.0);
        assert_eq!(clamp_setting(f32::INFINITY, 1.0), 1.0);
        assert_eq!(clamp_setting(f32::NEG_INFINITY, 1.0), 0.0);
    }

    #[test]
    fn a_nan_seek_is_ignored_and_infinities_clamp_into_the_track() {
        assert_eq!(sanitize_seek(f64::NAN, 100.0), None);
        assert_eq!(sanitize_seek(-5.0, 100.0), Some(0.0));
        assert_eq!(sanitize_seek(500.0, 100.0), Some(100.0));
        assert_eq!(sanitize_seek(f64::INFINITY, 100.0), Some(100.0));
        assert_eq!(sanitize_seek(f64::NEG_INFINITY, 100.0), Some(0.0));
        assert_eq!(sanitize_seek(42.5, 100.0), Some(42.5));
        // A not-yet-known duration still yields a finite position.
        assert_eq!(sanitize_seek(3.0, 0.0), Some(0.0));
    }
}
