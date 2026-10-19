//! yt-dlp diagnostics commands used by the Settings view: detect, set a custom
//! path, and download the binary when it's missing.

use rift_types::YtDlpStatus;
use tracing::warn;

use crate::backend::BackendHandle;
use crate::event::BackendEvent;
use crate::util::LockExt;

impl BackendHandle {
    /// Probe the system for yt-dlp, the load-bearing streaming fallback. Used by
    /// the Settings view to confirm playback will work.
    ///
    /// Spawns a short-lived `yt-dlp --version` subprocess (bounded by a timeout)
    /// but performs no network I/O beyond the local binary.
    pub async fn check_ytdlp(&self) -> YtDlpStatus {
        crate::fetch::detect_ytdlp().await
    }

    /// Set (or clear, with a blank value) a custom yt-dlp location, persist it,
    /// apply it immediately, and re-probe so the caller sees the new status.
    ///
    /// The path is validated before it is stored: this is the one command that
    /// turns a user-supplied string into a process Rift spawns, and the value is
    /// persisted, so it is checked here rather than at each spawn. See
    /// [`crate::fetch::validate_ytdlp_override`].
    pub async fn set_yt_dlp_path(&self, path: Option<String>) -> Result<YtDlpStatus, String> {
        // A blank value clears the override and falls back to auto-detection.
        let validated = match path.as_deref().map(str::trim).filter(|p| !p.is_empty()) {
            Some(p) => match crate::fetch::validate_ytdlp_override(p) {
                Ok(resolved) => Some(resolved),
                Err(e) => {
                    let msg = format!("That yt-dlp path can't be used: {e:#}");
                    warn!("{msg}");
                    // Leave the stored value alone: a bad paste must not clobber
                    // a working configuration.
                    return Err(msg);
                }
            },
            None => None,
        };
        let configured = {
            let mut settings = self.inner.settings.lock_safe();
            settings.set_yt_dlp_path(validated.as_ref().map(|p| p.to_string_lossy().to_string()));
            settings.data.yt_dlp_path.clone()
        };
        crate::fetch::set_ytdlp_override(configured.map(std::path::PathBuf::from));
        Ok(crate::fetch::detect_ytdlp().await)
    }

    /// Download yt-dlp for the current platform into the app data dir's `bin/`,
    /// set it as the active binary, and re-probe. Used by Settings when yt-dlp
    /// isn't found.
    ///
    /// Network I/O: downloads the release asset and verifies its published
    /// SHA-256 before writing anything. Run it from a spawned task.
    pub async fn download_ytdlp(&self) -> Result<YtDlpStatus, String> {
        let dir = self.inner.data_dir.join("bin");
        let http = self.inner.player.http.clone();
        match crate::fetch::download_ytdlp(&dir, &http).await {
            Ok(path) => {
                let path_str = path.to_string_lossy().to_string();
                self.inner
                    .settings
                    .lock_safe()
                    .set_yt_dlp_path(Some(path_str));
                crate::fetch::set_ytdlp_override(Some(path));
            }
            Err(e) => {
                let msg = format!("Could not download yt-dlp: {e:#}");
                warn!("{msg}");
                self.inner.emit(BackendEvent::Error(msg.clone()));
                return Err(msg);
            }
        }
        Ok(crate::fetch::detect_ytdlp().await)
    }
}
