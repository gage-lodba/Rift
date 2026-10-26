//! Startup snapshot handed to the application on launch.

use rift_types::{Bootstrap, Progress};

use crate::backend::BackendHandle;
use crate::player::snapshot;
use crate::util::LockExt;

impl BackendHandle {
    /// The complete launch snapshot: library, restored queue, playback state,
    /// volume, current track, progress, offline downloads, settings that affect
    /// the UI, and the preview flag.
    ///
    /// This is the synchronous initial read the app uses before the first frame;
    /// because it returns a consistent snapshot in one call, the app does not
    /// need to race the event bus on startup. Everything after the first frame
    /// arrives as [`BackendEvent`](crate::BackendEvent)s.
    #[must_use]
    pub fn bootstrap(&self) -> Bootstrap {
        // Read the settings first, under their own guard: the launcher snapshot
        // takes the player and library locks too, and nesting three unrelated
        // mutexes is a lock-order promise nothing here needs. All fields are
        // read in one tuple because two `lock_safe()` calls in the same struct
        // literal would both stay alive until the literal is built and deadlock
        // on the non-reentrant mutex.
        let (discord_rpc, crossfade, update_notifications) = {
            let s = self.inner.settings.lock_safe();
            (
                s.data.discord_rpc,
                s.data.crossfade,
                s.data.update_notifications,
            )
        };
        let core = self.inner.player.core.lock_safe();
        let lib = self.inner.library.lock_safe();
        Bootstrap {
            library: lib.data.clone(),
            queue: snapshot(&core),
            state: core.state,
            volume: core.volume,
            discord_rpc,
            crossfade,
            update_notifications,
            track: core.current.and_then(|i| core.queue.get(i).cloned()),
            progress: Progress {
                position: core.position,
                duration: core.duration,
            },
            downloads: self.inner.downloads.state(),
            // Screenshot aid, deliberately dev-only (see settings::preview_mode).
            preview: crate::settings::preview_mode(),
        }
    }
}
