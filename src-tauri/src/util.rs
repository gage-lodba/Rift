//! Small shared helpers.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};
use std::sync::{Mutex, MutexGuard};

use tracing::warn;

/// Read and parse a JSON state file, defaulting when missing. On a parse
/// failure the corrupt file is renamed aside (`*.json.bad`) so a later save
/// can't silently overwrite — and permanently destroy — the user's data.
pub(crate) fn load_json<T: serde::de::DeserializeOwned + Default>(path: &Path) -> T {
    let Ok(s) = std::fs::read_to_string(path) else {
        return T::default();
    };
    match serde_json::from_str(&s) {
        Ok(v) => v,
        Err(e) => {
            warn!("could not parse {}: {e}", path.display());
            let backup = path.with_extension("json.bad");
            match std::fs::rename(path, &backup) {
                Ok(()) => warn!("moved the corrupt file to {}", backup.display()),
                Err(e) => warn!("could not preserve the corrupt file: {e}"),
            }
            T::default()
        }
    }
}

/// Serialize `value` and write it to `path` atomically, logging (not
/// propagating) failures. `what` names the file in log messages.
pub(crate) fn save_json<T: serde::Serialize>(path: &Path, value: &T, what: &str) {
    match serde_json::to_string_pretty(value) {
        Ok(json) => {
            if let Err(e) = atomic_write(path, json.as_bytes()) {
                warn!("failed to save {what}: {e}");
            }
        }
        Err(e) => warn!("failed to serialize {what}: {e}"),
    }
}

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
///
/// Two further things keep a high-frequency caller (the queue snapshot) from
/// turning into a write burst: serialization happens *on the writer thread*,
/// and a payload identical to the one last written for that path is dropped
/// rather than written again.
///
/// Known loss: the final snapshot is still in flight when the process exits, so
/// the last change before a quit may not be persisted. Acceptable for the
/// playback snapshot (a restored queue is a convenience, not user data) — but
/// it is a real window, not a guarantee.
#[derive(Clone)]
pub struct Persister(Sender<Job>);

type Job = (PathBuf, Box<dyn FnOnce() -> Vec<u8> + Send>);

impl Persister {
    pub fn spawn() -> Self {
        let (tx, rx) = mpsc::channel::<Job>();
        std::thread::Builder::new()
            .name("rift-persist".into())
            .spawn(move || {
                // The payload last written per path, so a repeat of the same
                // snapshot is skipped instead of hitting the disk again.
                let mut last: std::collections::HashMap<PathBuf, Vec<u8>> =
                    std::collections::HashMap::new();
                while let Ok((path, serialize)) = rx.recv() {
                    let bytes = serialize();
                    if last.get(&path) == Some(&bytes) {
                        continue;
                    }
                    if let Err(e) = atomic_write(&path, &bytes) {
                        warn!("failed to persist {}: {e}", path.display());
                        // Don't record it as written: a later identical snapshot
                        // should get another chance at the disk.
                        continue;
                    }
                    last.insert(path, bytes);
                }
            })
            .expect("failed to spawn persist thread");
        Self(tx)
    }

    /// Queue `value` to be written to `path`, serialized on the writer thread.
    /// Returns immediately. A payload equal to the one last written for this
    /// path is dropped. `what` names the file in log messages.
    pub fn write<T, F>(&self, path: PathBuf, value: T, serialize: F) -> std::io::Result<()>
    where
        T: Send + 'static,
        F: FnOnce(&T) -> Vec<u8> + Send + 'static,
    {
        self.0
            .send((path, Box::new(move || serialize(&value))))
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::BrokenPipe, "persister stopped"))
    }
}

/// Write via a temp file + rename so a crash mid-write can't leave a truncated
/// file behind.
fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

/// Check network connectivity with a lightweight HTTP request. Used to skip
/// retries when the machine is offline (e.g. laptop woke up without Wi-Fi),
/// so a kept-offline playlist doesn't spin through backoff delays for minutes.
pub(crate) async fn is_online(http: &reqwest::Client) -> bool {
    // A HEAD request to the service Rift actually streams from (rather than an
    // unrelated third-party host); 2s timeout so we don't block long.
    match http
        .head("https://www.youtube.com/")
        .timeout(std::time::Duration::from_secs(2))
        .send()
        .await
    {
        Ok(resp) => came_from_youtube(resp.status().as_u16(), |name| {
            resp.headers()
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned)
        }),
        // Transport failure: genuinely offline (or YouTube is unreachable).
        Err(_) => false,
    }
}

/// Whether a probe response really came from YouTube.
///
/// A captive portal or transparent proxy answers the HEAD with its own
/// 200/302/403, and `.is_success()` alone would call that "online" — so the
/// function meant to *skip* pointless retries would instead let the kept-offline
/// loop burn its full ~7.5-minute backoff ladder against a portal. Requiring a
/// success status *and* YouTube's own client header is what distinguishes the
/// real service from whatever intercepted the request. Split out from
/// [`is_online`] so the rule is testable without a network round trip.
fn came_from_youtube(status: u16, header: impl Fn(&str) -> Option<String>) -> bool {
    const CLIENT_HEADER: &str = "x-youtube-client-name";
    (200..300).contains(&status) && header(CLIENT_HEADER).is_some()
}

#[cfg(test)]
pub(crate) mod testing {
    use std::path::{Path, PathBuf};

    /// A scratch directory that deletes itself when dropped.
    ///
    /// The test helpers used to create `%TEMP%\rift-*` directories and remove
    /// them on the last line, so every assertion failure — which is exactly when
    /// a developer runs the suite repeatedly — left one behind. `Drop` runs
    /// during unwinding, so a failing test cleans up like a passing one.
    pub struct TempDir(PathBuf);

    impl TempDir {
        pub fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "rift-{tag}-{}-{:016x}",
                std::process::id(),
                rand::random::<u64>()
            ));
            std::fs::create_dir_all(&dir).expect("create temp dir");
            Self(dir)
        }

        pub fn path(&self) -> &Path {
            &self.0
        }

        /// Join a name onto the directory.
        pub fn join(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::TempDir;
    use super::*;

    #[test]
    fn a_200_without_youtube_headers_is_a_captive_portal() {
        // The exact shape of a hotel/airport portal: a perfectly good 200 that
        // did not come from YouTube. This is the case `.is_success()` got wrong.
        assert!(!came_from_youtube(200, |_| None));
    }

    #[test]
    fn a_youtube_client_header_identifies_the_real_service() {
        assert!(came_from_youtube(200, |name| {
            (name == "x-youtube-client-name").then(|| "1".to_owned())
        }));
    }

    #[test]
    fn non_success_statuses_are_offline_even_from_youtube() {
        // A redirect to a portal login page, or an outright rejection. (2xx
        // stays "online" — the header check is what catches a portal's 200.)
        for status in [301, 302, 403, 429, 500, 503] {
            assert!(
                !came_from_youtube(status, |_| Some("1".to_owned())),
                "status {status} must not count as online"
            );
        }
    }

    #[test]
    fn json_round_trips_through_disk() {
        let dir = TempDir::new("util-round-trip");
        let path = dir.join("round-trip.json");

        #[derive(serde::Serialize, serde::Deserialize, Default, PartialEq, Debug)]
        struct State {
            volume: f32,
            name: String,
            ids: Vec<String>,
        }

        let original = State {
            volume: 0.75,
            name: "Rift".into(),
            ids: vec!["a".into(), "b".into()],
        };
        save_json(&path, &original, "round-trip test");
        let loaded: State = load_json(&path);
        assert_eq!(loaded, original);

        // A missing file is not an error, just a default.
        let absent: State = load_json(&dir.join("never-written.json"));
        assert_eq!(absent, State::default());
    }

    #[test]
    fn a_corrupt_file_is_backed_up_rather_than_lost() {
        let dir = TempDir::new("util-corrupt");
        let path = dir.join("library.json");
        std::fs::write(&path, b"{ this is not json").expect("seed corrupt file");

        let loaded: serde_json::Value = load_json(&path);
        assert_eq!(loaded, serde_json::Value::Null);
        assert!(!path.exists(), "the corrupt file must be moved aside");
        assert!(
            path.with_extension("json.bad").exists(),
            "the user's data must survive as a .json.bad backup"
        );
    }

    #[test]
    fn atomic_write_replaces_the_target_and_leaves_no_temp_behind() {
        let dir = TempDir::new("util-atomic");
        let path = dir.join("playback.json");

        atomic_write(&path, b"first").expect("write");
        assert_eq!(std::fs::read(&path).expect("read"), b"first");
        atomic_write(&path, b"second").expect("overwrite");
        assert_eq!(std::fs::read(&path).expect("read"), b"second");

        // The temp file lives *beside* the target (a lexical `with_extension`),
        // which is what makes the rename atomic within one filesystem.
        assert_eq!(path.with_extension("tmp").parent(), path.parent());
        assert!(
            !path.with_extension("tmp").exists(),
            "the temp file must be renamed away, not left behind"
        );
    }
}
