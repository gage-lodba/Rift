//! Small shared helpers.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};
use std::sync::{Mutex, MutexGuard};

use tracing::warn;

/// Lock a mutex, recovering the guard even if a previous holder panicked.
///
/// Playback and library state are guarded by plain `Mutex`es; if one thread
/// panics while holding a lock, the default `.unwrap()` would poison it and
/// every later access would panic too, bricking the whole app. The data behind
/// these locks is plain values (no broken invariants on panic), so recovering
/// the poisoned guard and carrying on is strictly better than cascading.
pub trait LockExt<T> {
    fn lock_safe(&self) -> MutexGuard<'_, T>;
}

impl<T> LockExt<T> for Mutex<T> {
    fn lock_safe(&self) -> MutexGuard<'_, T> {
        self.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Serializes best-effort disk writes onto a single background thread, so
/// callers never block on I/O and repeated writes to the same file can't race
/// or land out of order. Intended for state that's fine to lose on a crash
/// (e.g. the playback snapshot used to restore the queue next launch) — not for
/// durable user data, which is written synchronously elsewhere.
#[derive(Clone)]
pub struct Persister(Sender<(PathBuf, Vec<u8>)>);

impl Persister {
    pub fn spawn() -> Self {
        let (tx, rx) = mpsc::channel::<(PathBuf, Vec<u8>)>();
        std::thread::Builder::new()
            .name("rift-persist".into())
            .spawn(move || {
                while let Ok((path, bytes)) = rx.recv() {
                    if let Err(e) = atomic_write(&path, &bytes) {
                        warn!("failed to persist {}: {e}", path.display());
                    }
                }
            })
            .expect("failed to spawn persist thread");
        Self(tx)
    }

    /// Queue `bytes` to be written to `path`. Returns immediately.
    pub fn write(&self, path: PathBuf, bytes: Vec<u8>) {
        let _ = self.0.send((path, bytes));
    }
}

/// Write via a temp file + rename so a crash mid-write can't leave a truncated
/// file behind.
fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}
