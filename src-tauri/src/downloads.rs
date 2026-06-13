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
                if path.extension().and_then(|e| e.to_str()) == Some("m4a") {
                    if let Some(id) = path.file_stem().and_then(|s| s.to_str()) {
                        downloaded.insert(id.to_string());
                    }
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
    /// same track being fetched twice concurrently.
    pub fn begin(&self, id: &str) -> bool {
        self.downloading.lock_safe().insert(id.to_string())
    }

    pub fn finish(&self, id: &str) {
        self.downloading.lock_safe().remove(id);
        self.downloaded.lock_safe().insert(id.to_string());
    }

    pub fn fail(&self, id: &str) {
        self.downloading.lock_safe().remove(id);
    }

    /// Delete a downloaded file. Returns true if it existed.
    pub fn remove(&self, id: &str) -> bool {
        self.downloaded.lock_safe().remove(id);
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
        }
    }
}
