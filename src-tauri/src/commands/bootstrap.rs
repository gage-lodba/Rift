//! Startup snapshot handed to the frontend on launch.

use rift_types::{Bootstrap, Progress};
use tauri::State;

use crate::player::snapshot;
use crate::util::LockExt;
use crate::AppState;

#[tauri::command(rename_all = "snake_case")]
pub fn bootstrap(state: State<'_, AppState>) -> Bootstrap {
    let core = state.player.core.lock_safe();
    let lib = state.library.lock_safe();
    // Read all settings under a single lock: two `lock_safe()` calls in the same
    // struct literal would both stay alive until the literal is built and
    // deadlock on the non-reentrant mutex.
    let (discord_rpc, crossfade, yt_dlp_path, update_notifications) = {
        let s = state.settings.lock_safe();
        (
            s.data.discord_rpc,
            s.data.crossfade,
            s.data.yt_dlp_path.clone(),
            s.data.update_notifications,
        )
    };
    Bootstrap {
        library: lib.data.clone(),
        queue: snapshot(&core),
        state: core.state,
        volume: core.volume,
        discord_rpc,
        crossfade,
        yt_dlp_path,
        update_notifications,
        track: core.current.and_then(|i| core.queue.get(i).cloned()),
        progress: Progress {
            position: core.position,
            duration: core.duration,
        },
        downloads: state.downloads.state(),
        // Screenshot aid, deliberately dev-only (see settings::preview_mode).
        preview: crate::settings::preview_mode(),
    }
}
