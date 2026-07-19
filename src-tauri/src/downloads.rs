//! Offline download tracking. Audio files live as `<id>.m4a` under the
//! downloads directory; the on-disk presence of a file is the source of
//! truth for "downloaded". A separate set tracks in-flight downloads so the
//! UI can show progress.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Mutex;

use rift_types::DownloadState;
use tracing::{info, warn};

use crate::util::LockExt;

pub struct Downloads {
    pub dir: PathBuf,
    downloaded: Mutex<HashSet<String>>,
    downloading: Mutex<HashSet<String>>,
    /// Tracks whose download was abandoned after repeated failures. Purely a
    /// session-scoped UI hint (rows show a retry affordance); cleared the moment
    /// a track is re-attempted, finished, or removed.
    failed: Mutex<HashSet<String>>,
}

impl Downloads {
    pub fn load(dir: PathBuf) -> Self {
        if let Err(e) = std::fs::create_dir_all(&dir) {
            warn!("could not create downloads dir: {e}");
        }
        let mut downloaded = HashSet::new();
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                match path.extension().and_then(|e| e.to_str()) {
                    Some("m4a") => {
                        if let Some(id) = path.file_stem().and_then(|s| s.to_str()) {
                            downloaded.insert(id.to_string());
                        }
                    }
                    // A temp file left by a crash mid-download; never valid.
                    Some("part") => {
                        let _ = std::fs::remove_file(&path);
                    }
                    _ => {}
                }
            }
        }
        info!(
            "{} tracks available offline at {}",
            downloaded.len(),
            dir.display()
        );
        Self {
            dir,
            downloaded: Mutex::new(downloaded),
            downloading: Mutex::new(HashSet::new()),
            failed: Mutex::new(HashSet::new()),
        }
    }

    pub fn path(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.m4a"))
    }

    pub fn is_downloaded(&self, id: &str) -> bool {
        self.downloaded.lock_safe().contains(id)
    }

    pub fn is_downloading(&self, id: &str) -> bool {
        self.downloading.lock_safe().contains(id)
    }

    /// Claim a download. Returns `true` if it was newly started, `false` if one
    /// was already in flight — this atomic check-and-set is what prevents the
    /// same track being fetched twice concurrently. Starting (or restarting) a
    /// download clears any prior "failed" mark.
    pub fn begin(&self, id: &str) -> bool {
        self.failed.lock_safe().remove(id);
        self.downloading.lock_safe().insert(id.to_string())
    }

    pub fn finish(&self, id: &str) {
        self.downloading.lock_safe().remove(id);
        self.failed.lock_safe().remove(id);
        self.downloaded.lock_safe().insert(id.to_string());
    }

    /// Release an in-flight claim after a failed pass, without marking the track
    /// as permanently failed (a retry may still be scheduled).
    pub fn fail(&self, id: &str) {
        self.downloading.lock_safe().remove(id);
    }

    /// Give up on a track after repeated failures: drop any in-flight claim and
    /// record it as failed so the UI can offer a manual retry.
    pub fn mark_failed(&self, id: &str) {
        self.downloading.lock_safe().remove(id);
        self.failed.lock_safe().insert(id.to_string());
    }

    /// Delete a downloaded file. Returns true if it existed.
    pub fn remove(&self, id: &str) -> bool {
        self.downloaded.lock_safe().remove(id);
        self.failed.lock_safe().remove(id);
        match std::fs::remove_file(self.path(id)) {
            Ok(()) => true,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
            Err(e) => {
                warn!("could not delete download {id}: {e}");
                false
            }
        }
    }

    pub fn state(&self) -> DownloadState {
        DownloadState {
            downloaded: self.downloaded.lock_safe().iter().cloned().collect(),
            downloading: self.downloading.lock_safe().iter().cloned().collect(),
            failed: self.failed.lock_safe().iter().cloned().collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_downloads() -> Downloads {
        let dir = std::env::temp_dir().join(format!("rift-dl-{:016x}", rand::random::<u64>()));
        Downloads::load(dir)
    }

    #[test]
    fn begin_claims_once_so_concurrent_calls_dont_double_fetch() {
        let d = temp_downloads();
        assert!(d.begin("a"), "first claim succeeds");
        assert!(!d.begin("a"), "second claim is rejected");
        assert!(d.is_downloading("a"));
    }

    #[test]
    fn finish_moves_from_in_flight_to_downloaded() {
        let d = temp_downloads();
        d.begin("a");
        d.finish("a");
        assert!(!d.is_downloading("a"));
        assert!(d.is_downloaded("a"));
        // After finishing it can be claimed again (e.g. re-download).
        assert!(d.begin("a"));
    }

    #[test]
    fn fail_releases_the_claim_without_marking_downloaded() {
        let d = temp_downloads();
        d.begin("a");
        d.fail("a");
        assert!(!d.is_downloading("a"));
        assert!(!d.is_downloaded("a"));
    }

    #[test]
    fn remove_deletes_the_file_and_clears_state() {
        let d = temp_downloads();
        std::fs::write(d.path("a"), b"audio").unwrap();
        d.finish("a");
        assert!(d.is_downloaded("a"));

        assert!(d.remove("a"), "removing an existing download reports true");
        assert!(!d.is_downloaded("a"));
        assert!(!d.path("a").exists());
        assert!(!d.remove("a"), "removing a missing download reports false");
    }

    #[test]
    fn mark_failed_records_and_is_cleared_by_a_retry_or_success() {
        let d = temp_downloads();
        d.begin("a");
        d.mark_failed("a");
        assert!(!d.is_downloading("a"), "the in-flight claim is dropped");
        assert!(d.state().failed.contains("a"));

        // Re-attempting clears the failed mark (the row stops showing retry).
        assert!(d.begin("a"));
        assert!(!d.state().failed.contains("a"));

        // Finishing also clears it, and removing does too.
        d.mark_failed("a");
        d.finish("a");
        assert!(!d.state().failed.contains("a"));
        assert!(d.is_downloaded("a"));

        std::fs::write(d.path("a"), b"x").unwrap();
        d.mark_failed("a");
        d.remove("a");
        assert!(!d.state().failed.contains("a"));
    }

    #[test]
    fn load_discovers_existing_offline_files() {
        let d = temp_downloads();
        std::fs::write(d.dir.join("song.m4a"), b"x").unwrap();
        std::fs::write(d.dir.join("notes.txt"), b"x").unwrap();
        let reloaded = Downloads::load(d.dir.clone());
        assert!(reloaded.is_downloaded("song"));
        assert!(!reloaded.is_downloaded("notes"));
    }
}
