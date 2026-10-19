//! Queue-mutation commands: add, insert-next, move, remove, jump, clear.

use rift_types::Track;

use crate::backend::BackendHandle;
use crate::player;
use crate::util::LockExt;

impl BackendHandle {
    /// Append a track to the queue; starts playback if nothing was playing.
    pub fn queue_add(&self, track: Track) {
        let outcome = {
            let mut core = self.inner.player.core.lock_safe();
            player::append_tracks(&mut core, vec![track])
        };
        self.apply_add(outcome);
    }

    /// Insert tracks right after the current one so they play next.
    pub fn queue_play_next(&self, tracks: Vec<Track>) {
        let outcome = {
            let mut core = self.inner.player.core.lock_safe();
            player::insert_next(&mut core, tracks)
        };
        self.apply_add(outcome);
    }

    /// Reorder the queue, moving the track at `from` to `to`.
    pub fn queue_move(&self, from: usize, to: usize) {
        let moved = {
            let mut core = self.inner.player.core.lock_safe();
            player::move_in_queue(&mut core, from, to)
        };
        if moved {
            player::emit_queue(&self.inner, &self.inner.player);
        }
    }

    /// Remove the track at `index` from the queue, advancing playback if it was
    /// the playing track.
    pub fn queue_remove(&self, index: usize) {
        let outcome = {
            let mut core = self.inner.player.core.lock_safe();
            player::remove_from_queue(&mut core, index)
        };
        match outcome {
            player::RemoveOutcome::None => {}
            player::RemoveOutcome::EmitOnly => player::emit_queue(&self.inner, &self.inner.player),
            player::RemoveOutcome::PlayIndex(i) => player::play_index(&self.inner, i),
            player::RemoveOutcome::Stop => {
                player::stop(&self.inner);
                player::emit_queue(&self.inner, &self.inner.player);
            }
        }
    }

    /// Jump playback to the track at `index`.
    pub fn queue_jump(&self, index: usize) {
        player::play_index(&self.inner, index);
    }

    /// Clear the queue and stop playback.
    pub fn queue_clear(&self) {
        {
            let mut core = self.inner.player.core.lock_safe();
            core.queue.clear();
            core.current = None;
            core.source = None;
            core.shuffle_history.clear();
            core.shuffle_cursor = 0;
            core.pending_next = None;
        }
        player::stop(&self.inner);
        player::emit_queue(&self.inner, &self.inner.player);
    }

    /// Apply the result of a queue add: start playback if nothing was playing,
    /// otherwise just re-broadcast the queue.
    fn apply_add(&self, outcome: player::AddOutcome) {
        match outcome {
            player::AddOutcome::EmitOnly => player::emit_queue(&self.inner, &self.inner.player),
            player::AddOutcome::PlayIndex(i) => player::play_index(&self.inner, i),
        }
    }
}
