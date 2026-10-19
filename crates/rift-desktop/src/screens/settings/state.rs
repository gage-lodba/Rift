//! Local (component-scoped) settings state: the yt-dlp custom-path draft and the
//! in-flight "Download yt-dlp" flag. These mirror the old component-local
//! `use_state` values and are process-wide single slots, like the old UI.

use std::sync::Mutex;

use crate::state::State;

// ---------------------------------------------------------------------------
// local state
// ---------------------------------------------------------------------------

/// The custom-path input's draft. `State.yt_dlp_path` is the *configured* path;
/// the input needs a draft that survives frames, like the old component-local
/// `use_state`.
pub(crate) static PATH_DRAFT: Mutex<Option<String>> = Mutex::new(None);
/// Whether the "Download yt-dlp" command is in flight (local, like the old
/// component's `downloading` flag).
pub(crate) static YTDLP_DOWNLOADING: Mutex<Option<bool>> = Mutex::new(None);

pub(super) fn path_draft(state: &State) -> String {
    let mut draft = PATH_DRAFT.lock().unwrap_or_else(|e| e.into_inner());
    draft
        .get_or_insert_with(|| state.yt_dlp_path.clone().unwrap_or_default())
        .clone()
}

pub(super) fn store_path_draft(value: String) {
    *PATH_DRAFT.lock().unwrap_or_else(|e| e.into_inner()) = Some(value);
}

pub(super) fn downloading() -> bool {
    YTDLP_DOWNLOADING
        .lock()
        .map(|v| v.unwrap_or(false))
        .unwrap_or(false)
}

pub(super) fn set_downloading(value: bool) {
    *YTDLP_DOWNLOADING.lock().unwrap_or_else(|e| e.into_inner()) = Some(value);
}
