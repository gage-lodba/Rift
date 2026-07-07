//! yt-dlp diagnostics commands used by the Settings view: detect, set a custom
//! path, and download the binary when it's missing.

use rift_types::{events, YtDlpStatus};
use tauri::{AppHandle, Emitter, Manager, State};
use tracing::warn;

use crate::util::LockExt;
use crate::AppState;

/// Probe the system for yt-dlp, the load-bearing streaming fallback. Used by
/// the Settings view to confirm playback will work.
#[tauri::command(rename_all = "snake_case")]
pub async fn check_ytdlp() -> YtDlpStatus {
    rift::fetch::detect_ytdlp().await
}

/// Set (or clear, with a blank value) a custom yt-dlp location, persist it,
/// apply it immediately, and re-probe so the caller sees the new status.
#[tauri::command(rename_all = "snake_case")]
pub async fn set_yt_dlp_path(
    path: Option<String>,
    state: State<'_, AppState>,
) -> Result<YtDlpStatus, String> {
    let configured = {
        let mut settings = state.settings.lock_safe();
        settings.set_yt_dlp_path(path);
        settings.data.yt_dlp_path.clone()
    };
    rift::fetch::set_ytdlp_override(configured.map(std::path::PathBuf::from));
    Ok(rift::fetch::detect_ytdlp().await)
}

/// Download yt-dlp for the current platform into the app data dir, set it as the
/// active binary, and re-probe. Used by Settings when yt-dlp isn't found.
#[tauri::command(rename_all = "snake_case")]
pub async fn download_ytdlp(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<YtDlpStatus, String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("no app data dir: {e}"))?
        .join("bin");
    let http = state.player.http.clone();
    match rift::fetch::download_ytdlp(&dir, &http).await {
        Ok(path) => {
            let path_str = path.to_string_lossy().to_string();
            state.settings.lock_safe().set_yt_dlp_path(Some(path_str));
            rift::fetch::set_ytdlp_override(Some(path));
        }
        Err(e) => {
            let msg = format!("Could not download yt-dlp: {e:#}");
            warn!("{msg}");
            let _ = app.emit(events::ERROR, msg.clone());
            return Err(msg);
        }
    }
    Ok(rift::fetch::detect_ytdlp().await)
}
