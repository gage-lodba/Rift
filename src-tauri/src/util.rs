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
    let tmp = temp_path_for(path)?;
    let written = std::fs::write(&tmp, bytes).and_then(|()| std::fs::rename(&tmp, path));
    if written.is_err() {
        // Neither a failed write nor a failed rename should leave scratch next
        // to the user's data — it would accumulate for every failed save.
        let _ = std::fs::remove_file(&tmp);
    }
    written
}

/// The temp path [`atomic_write`] publishes from: a unique name *constructed in
/// the target's own directory*, so the rename stays within one filesystem (the
/// property the atomicity depends on) by construction rather than by relying on
/// `Path::with_extension` happening to be lexical.
///
/// The process id plus an in-process counter keep the name unique per call.
/// A predictable shared temp name — what `with_extension("tmp")` produced —
/// lets two writers to the same target interleave: the winner's rename can
/// publish the loser's half-written bytes, or fail outright because the loser
/// already renamed the one temp file away. Unique names also mean a temp orphaned
/// by a hard crash is never picked up and published by a later write.
fn temp_path_for(path: &Path) -> std::io::Result<PathBuf> {
    let Some(name) = path.file_name() else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{} has no file name to write", path.display()),
        ));
    };
    // A bare relative name has `""` as its parent, which joins to a sibling
    // resolved against the same working directory — the same directory either way.
    let dir = path.parent().unwrap_or_else(|| Path::new(""));

    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let unique = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    Ok(dir.join(format!(
        ".{}.{}-{unique}.tmp",
        name.to_string_lossy(),
        std::process::id()
    )))
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

        // Nothing but the target may be left in the directory: a leftover temp
        // means a rename was skipped, and junk next to the user's state is a bug.
        assert_eq!(
            dir_names(&dir),
            vec!["playback.json".to_owned()],
            "the temp file must be renamed away, not left behind"
        );
        // The rewrite replaces the target in place rather than, say, appending.
        assert_eq!(std::fs::read(&path).expect("read"), b"second");
    }

    #[test]
    fn the_temp_path_is_a_unique_sibling_of_the_target() {
        let dir = TempDir::new("util-temp-path");
        let path = dir.join("settings.json");

        let first = temp_path_for(&path).expect("temp path");
        let second = temp_path_for(&path).expect("temp path");

        // Beside the target by construction: same directory, so the rename
        // cannot cross a filesystem boundary and lose its atomicity.
        assert_eq!(
            first.parent(),
            path.parent(),
            "the temp file must be constructed in the target's own directory"
        );
        assert_ne!(first, path, "the temp file must not be the target itself");
        assert!(
            first
                .file_name()
                .is_some_and(|n| n.to_string_lossy().contains("settings.json")),
            "the temp name should still identify the file it belongs to: {}",
            first.display()
        );
        // Unique: a shared temp name is what lets two concurrent writers publish
        // each other's half-written bytes.
        assert_ne!(first, second, "every call must get its own temp path");

        // A bare relative name has no parent directory component, and still
        // resolves to a sibling in the same working directory.
        let bare = temp_path_for(Path::new("settings.json")).expect("temp path");
        assert_eq!(bare.parent(), Some(Path::new("")));
    }

    #[test]
    fn a_path_with_no_file_name_is_rejected_rather_than_guessed_at() {
        // `.`, `..` and the empty path have no file name, so there is no sibling
        // to write beside; erroring beats inventing a name in some other folder.
        for target in [PathBuf::from("."), PathBuf::from(".."), PathBuf::new()] {
            let err =
                temp_path_for(&target).expect_err("a path with no file name must be rejected");
            assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
        }
    }

    #[test]
    fn a_failed_publish_does_not_leave_scratch_behind() {
        let dir = TempDir::new("util-atomic-fail");
        // A directory sitting at the target path makes the final rename fail on
        // every platform, which pins the cleanup instead of an OS-specific error.
        let path = dir.join("target");
        std::fs::create_dir(&path).expect("seed directory");

        atomic_write(&path, b"data").expect_err("renaming a file over a directory must fail");

        assert_eq!(
            dir_names(&dir),
            vec!["target".to_owned()],
            "a failed publish must not leave its temp file behind"
        );
    }

    #[test]
    fn the_persister_writes_and_drops_an_identical_payload() {
        let dir = TempDir::new("util-persister");
        let path = dir.join("playback.json");
        let persister = Persister::spawn();

        persister
            .write(path.clone(), vec![1u8, 2, 3], |v| v.clone())
            .expect("queue write");
        drain(&persister, &dir);
        assert_eq!(std::fs::read(&path).expect("read"), [1, 2, 3]);

        // An identical payload is dropped, and that is observable: delete the
        // file, re-queue the same bytes, and it must not come back. (M8's write
        // burst came from the queue snapshot being emitted twice unchanged.)
        std::fs::remove_file(&path).expect("remove");
        persister
            .write(path.clone(), vec![1u8, 2, 3], |v| v.clone())
            .expect("queue duplicate");
        drain(&persister, &dir);
        assert!(
            !path.exists(),
            "an unchanged payload must not be written to disk again"
        );

        // A changed payload is written, so the dedupe can't wedge the file.
        persister
            .write(path.clone(), vec![9u8], |v| v.clone())
            .expect("queue change");
        drain(&persister, &dir);
        assert_eq!(std::fs::read(&path).expect("read"), [9]);
    }

    /// Block until the persister has finished every job queued so far.
    ///
    /// Jobs are drained in order on one thread, so a marker job's serialization
    /// closure running means everything before it is already on disk. The older
    /// tests slept and hoped; this cannot race.
    fn drain(persister: &Persister, dir: &TempDir) {
        let (tx, rx) = std::sync::mpsc::channel();
        persister
            .write(dir.join("drain-marker"), (), move |_| {
                let _ = tx.send(());
                Vec::new()
            })
            .expect("queue drain marker");
        rx.recv_timeout(std::time::Duration::from_secs(30))
            .expect("the persister thread did not drain in time");
    }

    fn dir_names(dir: &TempDir) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir.path())
            .expect("read dir")
            .map(|e| {
                e.expect("dir entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        names.sort();
        names
    }
}
