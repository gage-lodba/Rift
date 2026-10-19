//! Playback control commands: load/play tracks, transport, volume, shuffle,
//! repeat, and related persisted settings.

use rift_types::{PlaybackState, Progress, Track};

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
    /// No-op unless playback is playing or paused. Emits a
    /// [`BackendEvent::Progress`] so the seek bar lands immediately rather than
    /// waiting for the next audio-thread tick, and re-syncs Discord's time bar.
    pub fn seek(&self, position: f64) {
        let (progress, pstate) = {
            let mut core = self.inner.player.core.lock_safe();
            if core.state != PlaybackState::Playing && core.state != PlaybackState::Paused {
                return;
            }
            core.position = position.clamp(0.0, core.duration.max(0.0));
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
    pub fn set_volume(&self, volume: f32) {
        let volume = volume.clamp(0.0, 1.0);
        self.inner.player.core.lock_safe().volume = volume;
        let _ = self.inner.player.audio.send(AudioCmd::Volume(volume));
    }

    /// Persist the volume (fired once when the slider is released) so it survives
    /// a restart. Kept separate from [`set_volume`](Self::set_volume) to avoid a
    /// disk write per tick. The write is queued on the backend's durable writer,
    /// so this is safe to call from the UI thread; the backend flushes on
    /// shutdown.
    pub fn save_volume(&self, volume: f32) {
        self.inner.settings.lock_safe().set_volume(volume);
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
    /// Takes effect on the next track transition. The settings write is queued on
    /// the durable writer rather than performed here, so the UI thread does not
    /// block on it.
    pub fn set_crossfade(&self, secs: f32) {
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
