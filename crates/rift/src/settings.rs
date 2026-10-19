//! JSON-persisted app settings: playback volume, Discord Rich Presence,
//! crossfade overlap, the yt-dlp binary path, and update notifications.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::util::DurableWriter;

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

/// Whether the app was launched in preview mode (a dev build with
/// `RIFT_PREVIEW=1)`: the UI renders placeholder data and settings run as
/// in-memory defaults that are never written to disk. Always false in
/// release builds so the mode can't ship.
pub fn preview_mode() -> bool {
    cfg!(debug_assertions) && std::env::var("RIFT_PREVIEW").is_ok_and(|v| !v.is_empty() && v != "0")
}

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
    /// Preview mode: settings live in memory only; nothing touches disk, and
    /// the user's real settings.json is neither read nor overwritten.
    ephemeral: bool,
    /// The backend's durable writer. Every save is queued to it, so the UI-thread
    /// setters below (slider release, toggles) return without waiting for disk.
    writer: Arc<DurableWriter>,
}

impl SettingsStore {
    /// Load settings at startup. Synchronous on purpose: the backend needs the
    /// persisted volume/discord/crossfade before it starts any threads.
    pub fn load(dir: &Path, writer: &Arc<DurableWriter>) -> Self {
        let path = dir.join("settings.json");
        if preview_mode() {
            warn!("preview mode: settings run in memory only and will not be saved");
            return Self {
                path,
                data: Settings::default(),
                ephemeral: true,
                writer: Arc::clone(writer),
            };
        }
        let mut data: Settings = crate::util::load_json(&path);
        // A hand-edited settings.json can carry out-of-range values; clamp here
        // so the volume slider and crossfade logic see sane bounds every launch.
        data.volume = data.volume.clamp(0.0, 1.0);
        data.crossfade = data.crossfade.clamp(0.0, MAX_CROSSFADE);
        Self {
            path,
            data,
            ephemeral: false,
            writer: Arc::clone(writer),
        }
    }

    /// Queue the current settings to be written to disk.
    ///
    /// Non-blocking: the setter that triggered it normally runs on the UI thread,
    /// and the durable writer serializes + atomically writes the snapshot in
    /// queue order. `DurableWriter::flush` is the barrier tests use; the backend
    /// flushes on shutdown so the last change survives a quit.
    fn save(&self) {
        if self.ephemeral {
            return;
        }
        self.writer
            .write(self.path.clone(), "settings", self.data.clone(), |data| {
                serde_json::to_vec_pretty(data).map_err(|e| e.to_string())
            });
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
    use crate::util::testing::TempDir;

    /// A store plus the durable writer it saves through.
    ///
    /// Tests hold the writer so they can `flush()` — the deterministic barrier —
    /// rather than sleeping and hoping the background write finished.
    fn temp_store(dir: &TempDir) -> (SettingsStore, Arc<DurableWriter>) {
        let writer = Arc::new(DurableWriter::spawn());
        (SettingsStore::load(dir.path(), &writer), writer)
    }

    #[test]
    fn volume_survives_a_reload() {
        let dir = TempDir::new("settings-volume");

        // A fresh store defaults to full volume.
        let (store, writer) = temp_store(&dir);
        assert_eq!(store.data.volume, 1.0);
        drop(store);

        // Saving and reloading round-trips the value (as a relaunch would).
        let mut store = SettingsStore::load(dir.path(), &writer);
        store.set_volume(0.42);
        assert!(
            writer.flush(),
            "the volume save must reach disk before the reload"
        );
        let restored = SettingsStore::load(dir.path(), &writer).data.volume;
        assert!((restored - 0.42).abs() < 1e-6, "got {restored}");
    }

    #[test]
    fn out_of_range_values_are_clamped_on_load() {
        let dir = TempDir::new("settings-clamp");
        std::fs::write(
            dir.join("settings.json"),
            r#"{"volume": 5.0, "crossfade": 999.0}"#,
        )
        .unwrap();

        let s = temp_store(&dir).0.data;
        assert_eq!(s.volume, 1.0);
        assert_eq!(s.crossfade, MAX_CROSSFADE);
    }

    /// The UI-thread contract: a slider release or toggle queues a save and
    /// returns. Several rapid changes are several queued snapshots, applied FIFO,
    /// so the *last* change is what a relaunch reads — not an intermediate one.
    #[test]
    fn rapid_settings_changes_are_written_in_order_and_the_last_one_wins() {
        let dir = TempDir::new("settings-rapid-writes");
        let (mut store, writer) = temp_store(&dir);

        store.set_volume(0.25);
        store.set_volume(0.75);
        store.set_crossfade(3.5);
        store.set_discord_rpc(false);

        assert!(writer.flush(), "every queued save must be drained");
        let restored = SettingsStore::load(dir.path(), &writer).data;
        assert!(
            (restored.volume - 0.75).abs() < 1e-6,
            "the last volume must win, got {}",
            restored.volume
        );
        assert!((restored.crossfade - 3.5).abs() < 1e-6);
        assert!(!restored.discord_rpc, "the last toggle must win");
    }
}
