//! JSON-persisted app settings (currently just playback volume).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tracing::warn;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Playback volume in 0.0..=1.0.
    pub volume: f32,
    /// Whether to advertise the current track as a Discord Rich Presence.
    pub discord_rpc: bool,
    /// Crossfade overlap between tracks, in seconds. 0 disables it (hard cut).
    #[serde(default)]
    pub crossfade: f32,
    /// Custom path to the yt-dlp binary. When set, it takes priority over
    /// PATH/common-location detection. `None` means auto-detect.
    #[serde(default)]
    pub yt_dlp_path: Option<String>,
    /// Whether to check for a newer release on launch and notify the user.
    #[serde(default = "default_true")]
    pub update_notifications: bool,
}

fn default_true() -> bool {
    true
}

/// Largest crossfade overlap the UI offers and the backend accepts.
pub const MAX_CROSSFADE: f32 = 12.0;

impl Default for Settings {
    fn default() -> Self {
        Self {
            volume: 1.0,
            discord_rpc: true,
            crossfade: 0.0,
            yt_dlp_path: None,
            update_notifications: true,
        }
    }
}

pub struct SettingsStore {
    path: PathBuf,
    pub data: Settings,
}

impl SettingsStore {
    pub fn load(dir: &Path) -> Self {
        let path = dir.join("settings.json");
        let data = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| match serde_json::from_str(&s) {
                Ok(settings) => Some(settings),
                Err(e) => {
                    warn!("could not parse {}: {e}", path.display());
                    None
                }
            })
            .unwrap_or_default();
        Self { path, data }
    }

    fn save(&self) {
        match serde_json::to_string_pretty(&self.data) {
            Ok(json) => {
                if let Err(e) = std::fs::write(&self.path, json) {
                    warn!("failed to save settings: {e}");
                }
            }
            Err(e) => warn!("failed to serialize settings: {e}"),
        }
    }

    pub fn set_volume(&mut self, volume: f32) {
        self.data.volume = volume.clamp(0.0, 1.0);
        self.save();
    }

    pub fn set_discord_rpc(&mut self, on: bool) {
        self.data.discord_rpc = on;
        self.save();
    }

    /// Persist the crossfade overlap, returning the clamped value actually stored.
    pub fn set_crossfade(&mut self, secs: f32) -> f32 {
        self.data.crossfade = secs.clamp(0.0, MAX_CROSSFADE);
        self.save();
        self.data.crossfade
    }

    pub fn set_update_notifications(&mut self, on: bool) {
        self.data.update_notifications = on;
        self.save();
    }

    pub fn set_yt_dlp_path(&mut self, path: Option<String>) {
        // Treat blank/whitespace-only input as "unset" (auto-detect).
        self.data.yt_dlp_path = path.map(|p| p.trim().to_string()).filter(|p| !p.is_empty());
        self.save();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn volume_survives_a_reload() {
        let dir = std::env::temp_dir().join(format!("rift-test-{:016x}", rand::random::<u64>()));
        std::fs::create_dir_all(&dir).unwrap();

        // A fresh store defaults to full volume.
        assert_eq!(SettingsStore::load(&dir).data.volume, 1.0);

        // Saving and reloading round-trips the value (as a relaunch would).
        SettingsStore::load(&dir).set_volume(0.42);
        let restored = SettingsStore::load(&dir).data.volume;
        assert!((restored - 0.42).abs() < 1e-6, "got {restored}");

        std::fs::remove_dir_all(&dir).ok();
    }
}
