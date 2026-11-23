//! Small shared helpers.
//!
//! The load-bearing part is the state-file layer: [`load_json_with_status`]
//! quarantines state it cannot read instead of overwriting it, and
//! [`DurableWriter`] publishes durable saves from one ordered thread while
//! coalescing pending whole-file snapshots. Both exist so user data survives a
//! crash and the UI thread never waits on the disk.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};

use tracing::{debug, error, warn};

/// How loading a persisted state file went, so a store knows whether writing
/// its in-memory state back to that path is safe.
///
/// "Missing" and "corrupt" are safe to save over: there is no file, or the
/// corrupt one has been moved aside. A **read failure** or a corrupt file that
/// could not be preserved is different — the file is still there and may be the
/// user's only copy — so it is only writable once the file has been preserved
/// under a `.json.bad` name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LoadStatus {
    /// The file was read and parsed.
    Loaded,
    /// The document was usable, but some of its rows could not be understood.
    /// The value keeps every readable row; the caller should save it back after
    /// taking its pre-repair snapshot. Safe to overwrite, exactly like a clean
    /// load.
    Repaired,
    /// There was no file; the defaults are the state.
    Missing,
    /// The bytes were not valid state (bad JSON or not UTF-8) and the file was
    /// moved aside. A parse failure leaves the store writable so a fresh file
    /// can still be written.
    Corrupt,
    /// The bytes were not valid state **and** the corrupt file could not be
    /// moved aside. Saving the defaults would overwrite the only copy, so the
    /// store must not save — the same rule as [`Self::UnreadableUnpreserved`].
    CorruptUnpreserved,
    /// The file could not be read, but it was moved aside intact. The defaults
    /// are now safe to save — the original survives as a `.json.bad` backup.
    UnreadablePreserved,
    /// The file could not be read **and** could not be moved aside. Saving the
    /// defaults would overwrite the only copy, so the store must not save.
    UnreadableUnpreserved,
}

impl LoadStatus {
    /// Whether a save may replace the file. False only when the load could
    /// neither read nor preserve it: the original must not be lost.
    pub(crate) fn may_overwrite(self) -> bool {
        !matches!(self, Self::CorruptUnpreserved | Self::UnreadableUnpreserved)
    }

    /// A user-facing explanation for a load that did not read the file, or
    /// `None` for a clean load, a repairable one, or a missing file (all
    /// expected).
    ///
    /// The text lives here so the stores and the backend's startup toast
    /// describe the same outcome identically. A corrupt file that was moved
    /// aside stays silent, as today; a corruption or read failure that could not
    /// be preserved is news the user did not have a way to learn.
    pub(crate) fn warning(self, path: &Path) -> Option<String> {
        match self {
            Self::UnreadablePreserved => Some(format!(
                "Rift could not read {}; the unreadable file was moved aside to a .json.bad \
                 backup and Rift started from defaults",
                path.display()
            )),
            Self::CorruptUnpreserved => Some(format!(
                "Rift could not parse {}; the corrupt file could not be preserved as a \
                 .json.bad backup, so changes will not be saved this session — fix the file's \
                 permissions and restart Rift",
                path.display()
            )),
            Self::UnreadableUnpreserved => Some(format!(
                "Rift could not read {} and could not move it aside; changes will not be saved \
                 this session so the existing file is not lost — fix the file's permissions and \
                 restart Rift",
                path.display()
            )),
            Self::Loaded | Self::Repaired | Self::Missing | Self::Corrupt => None,
        }
    }
}

/// Read and parse a JSON state file, defaulting when missing or unusable. On a
/// parse failure the corrupt file is renamed aside (`*.json.bad`, with a numeric
/// suffix when that backup already exists, so an earlier corrupt copy is never
/// overwritten) so a later save can't silently overwrite — and permanently
/// destroy — the user's data.
///
/// Test-only convenience for call sites that do not care how the file was
/// loaded: production stores need [`load_json_with_status`] and must honour
/// [`LoadStatus::may_overwrite`], or an unreadable file that could not be
/// quarantined would be replaced by defaults.
#[cfg(test)]
pub(crate) fn load_json<T: serde::de::DeserializeOwned + Default>(path: &Path) -> T {
    load_json_with_status(path).0
}

/// [`load_json`] plus how the file was loaded, so a store can tell "there was
/// no state" apart from "the state could not be read, and maybe not preserved".
pub(crate) fn load_json_with_status<T: serde::de::DeserializeOwned + Default>(
    path: &Path,
) -> (T, LoadStatus) {
    load_json_from(path, |path| std::fs::read(path))
}

/// [`load_json_with_status`] with the document parser injected.
///
/// `parse` returns the value plus whether it had to drop any rows. `Err` means
/// the document is not usable state at all and takes the corrupt/quarantine
/// path; a successful parse that dropped rows becomes [`LoadStatus::Repaired`],
/// which is writable and tells the caller to save the repaired value back. The
/// library uses this to keep one malformed row from quarantining the whole
/// file; settings has no row structure and uses the strict parser above.
pub(crate) fn load_json_with_parser<T: Default>(
    path: &Path,
    parse: impl FnOnce(&[u8]) -> Result<(T, bool), String>,
) -> (T, LoadStatus) {
    load_json_from_with(path, |path| std::fs::read(path), parse)
}

/// [`load_json_with_status`] with the byte source injected.
///
/// The production caller passes [`std::fs::read`]. The seam exists so the
/// read-error branch — the one that decides read-only vs writable — can be
/// exercised on every platform without depending on Unix permission bits (or
/// Windows sharing semantics), which is otherwise the only way to produce a
/// real read failure.
fn load_json_from<T: serde::de::DeserializeOwned + Default>(
    path: &Path,
    read: impl FnOnce(&Path) -> std::io::Result<Vec<u8>>,
) -> (T, LoadStatus) {
    load_json_from_with(path, read, |bytes| {
        serde_json::from_slice(bytes)
            .map(|value| (value, false))
            .map_err(|e| e.to_string())
    })
}

/// The one loader behind both public shapes: read, parse, and decide whether
/// the original had to be quarantined.
fn load_json_from_with<T: Default>(
    path: &Path,
    read: impl FnOnce(&Path) -> std::io::Result<Vec<u8>>,
    parse: impl FnOnce(&[u8]) -> Result<(T, bool), String>,
) -> (T, LoadStatus) {
    match read(path) {
        // Reading bytes rather than a `String` is what makes a file that is not
        // valid UTF-8 take the corrupt-file path: `read_to_string` fails on it,
        // and the old code treated every read failure as "no file", so a
        // corrupt library was silently replaced by an empty one on the next
        // save instead of being preserved. JSON is UTF-8 by definition, so any
        // such file *is* the corrupt one this function exists to quarantine.
        Ok(bytes) => match parse(&bytes) {
            Ok((value, repaired)) => (
                value,
                if repaired {
                    LoadStatus::Repaired
                } else {
                    LoadStatus::Loaded
                },
            ),
            Err(e) => {
                warn!("could not parse {}: {e}", path.display());
                if quarantine_file(path) {
                    (T::default(), LoadStatus::Corrupt)
                } else {
                    // The corrupt file is still at its path and is the user's
                    // only copy; the store reads `may_overwrite` and refuses to
                    // save, and the startup warning says why (see `LoadStatus`).
                    error!(
                        "could not parse {} ({e}) and could not preserve it; changes will not be \
                         saved so the existing file is not lost",
                        path.display()
                    );
                    (T::default(), LoadStatus::CorruptUnpreserved)
                }
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (T::default(), LoadStatus::Missing),
        Err(e) => {
            // A read failure is not "no file": the state may still be there but
            // temporarily unreadable (a sharing violation from a backup/AV tool,
            // a permission problem). Preserve it first, exactly as a corrupt file
            // is preserved — the caller's next save would otherwise overwrite
            // data this load never managed to read.
            if quarantine_file(path) {
                warn!(
                    "could not read {} ({e}); the unreadable file was moved aside, starting from \
                     defaults",
                    path.display()
                );
                (T::default(), LoadStatus::UnreadablePreserved)
            } else {
                // The file survives only if nothing writes to its path again, so
                // say that plainly: the store refuses to save (see `LoadStatus`).
                error!(
                    "could not read {} ({e}) and could not move it aside; changes will not be \
                     saved so the existing file is not lost",
                    path.display()
                );
                (T::default(), LoadStatus::UnreadableUnpreserved)
            }
        }
    }
}

/// Move an unusable state file aside, never overwriting an earlier backup.
///
/// Returns whether the file was preserved (renamed to the free `.json.bad`
/// name). The caller decides what a failure means: for corrupt state the store
/// becomes read-only (the corrupt original is the only copy); for a file that
/// could not be read it makes the store read-only too.
fn quarantine_file(path: &Path) -> bool {
    let backup = free_backup_path(&path.with_extension("json.bad"));
    match std::fs::rename(path, &backup) {
        Ok(()) => {
            warn!("moved the unusable file to {}", backup.display());
            true
        }
        Err(e) => {
            warn!("could not preserve {}: {e}", path.display());
            false
        }
    }
}

/// The first free numbered backup name: the historical bare name first, then
/// `.1`, `.2`, … so a second corruption (or a second pre-repair snapshot) cannot
/// destroy the copy an earlier repair (or the user) was relying on.
///
/// `base` is the whole sibling name, e.g. `library.json.bad` or
/// `library.json.pre-collapse`; the suffix is appended as text so the result
/// keeps the documented shape instead of replacing a dotted "extension".
pub(crate) fn free_backup_path(base: &Path) -> PathBuf {
    if !base.exists() {
        return base.to_path_buf();
    }
    // Append to the file name as text. `with_extension` would treat the ".bad"
    // as the extension and produce `library.json.json.bad.1`, which both reads
    // wrong and would not match the documented `*.json.bad` shape.
    let Some(name) = base.file_name() else {
        return base.to_path_buf();
    };
    for n in 1u32..=999 {
        let mut candidate_name = name.to_os_string();
        candidate_name.push(format!(".{n}"));
        let candidate = base.with_file_name(candidate_name);
        if !candidate.exists() {
            return candidate;
        }
    }
    // A thousand backups of one file is not a real state; fall back to the
    // historical name rather than looping forever.
    base.to_path_buf()
}

/// Serialize `value` and write it to `path` atomically, logging (not
/// propagating) failures. `what` names the file in log messages.
///
/// Only tests use this now: runtime writes of durable state (the library and
/// settings) go through [`DurableWriter`], which keeps them off the caller's
/// thread. It stays as the synchronous one-shot the tests use to seed a file
/// before a store loads.
#[cfg(test)]
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
        self.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Serializes best-effort disk writes onto a single background thread, so
/// callers never block on I/O and repeated writes to the same file can't race
/// or land out of order. Intended for state that's fine to lose on a crash
/// (e.g. the playback snapshot used to restore the queue next launch) — not for
/// durable user data, which uses [`DurableWriter`]: also one ordered thread, but
/// it coalesces pending snapshots per file, exposes a flush barrier and drains
/// at shutdown instead of dropping the final write.
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
        // A failed thread spawn is logged rather than panicked: the backend still
        // starts (this only affects best-effort playback-snapshot persistence),
        // and writes are dropped exactly as if the disk were unwritable.
        if let Err(e) = std::thread::Builder::new()
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
        {
            warn!("could not spawn the persist thread; playback snapshots will not be saved: {e}");
        }
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

/// How long [`DurableWriter::flush`] waits for the queue to drain before giving
/// up.
///
/// The queued writes are small JSON documents and one thread performs them in
/// order, so a healthy writer drains in milliseconds; coalescing keeps a burst
/// on one file down to a single pending snapshot, so the backlog is bounded by
/// the number of distinct files rather than by the mutation rate. The ceiling
/// exists so a wedged disk (or a stopped writer thread) cannot hang application
/// shutdown forever. Reaching it is logged and reported to the caller — never
/// silent.
const FLUSH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Longest fragment of a failed save's error retained for the user-facing
/// notice. An `io::Error` can quote a very long path; a toast only needs the
/// start of the reason, and the record is kept for the whole session.
const MAX_SAVE_FAILURE_CHARS: usize = 240;

/// The hard ceiling on distinct destination files with a snapshot waiting in
/// [`DurableWriter`]'s queue.
///
/// The app has exactly two durable files (`library.json`, `settings.json`), so
/// in normal use this cap is never approached. It exists because the queue's
/// memory cost is one *whole snapshot* per pending path and a library snapshot
/// can hold thousands of tracks: coalescing bounds a burst on one file to one
/// snapshot, but a caller inventing a distinct path per write would otherwise
/// grow the queue without bound. At the cap a new path is saved on the caller's
/// thread instead (see [`DurableWriter::write`]) — slower, but the change is
/// written rather than dropped, the same durability-over-latency trade the
/// writer already makes when its thread is unavailable. 64 leaves room for
/// tests and future files while keeping the worst case (64 × one large
/// library) far below the rest of the process's memory budget.
const MAX_PENDING_PATHS: usize = 64;

/// The most recent failed durable save and the total number recorded since the
/// writer started.
///
/// The writer thread (or an inline fallback) records this; the backend polls
/// [`DurableWriter::last_failure`] from a small watchdog task and turns a new
/// `count` into one `BackendEvent::Error` per burst. The count is monotonic so
/// the backend can tell a repeated poll from a new failure without the writer
/// having to know anything about the event bus.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DurableSaveFailure {
    /// File label the failed job carried (`"library"`, `"settings"`).
    pub(crate) what: &'static str,
    /// Why the last save failed (bounded to [`MAX_SAVE_FAILURE_CHARS`]).
    pub(crate) error: String,
    /// How many durable saves have failed since the writer started.
    pub(crate) count: u64,
}

/// Thread-safe slot behind [`DurableSaveFailure`].
///
/// One lock, one last message, one counter: recording a failure cannot grow
/// memory with a disk that stays broken, and the writer thread never waits on
/// anything but another recorder (the watchdog only reads a clone).
#[derive(Debug, Default)]
struct SaveFailureRecord {
    last: Option<(&'static str, String)>,
    count: u64,
}

/// Record one failed durable save in `slot`.
fn record_save_failure(slot: &Mutex<SaveFailureRecord>, what: &'static str, error: &str) {
    let mut record = slot.lock_safe();
    record.count = record.count.saturating_add(1);
    let mut error = error.to_owned();
    if error.chars().count() > MAX_SAVE_FAILURE_CHARS {
        // Bound the retained reason: an io error can quote a path long enough
        // to matter over a long session, and only its start is toast-usable.
        error = error.chars().take(MAX_SAVE_FAILURE_CHARS).collect();
        error.push('\u{2026}');
    }
    record.last = Some((what, error));
}

/// Clone the current failure record out of `slot`, if a save has ever failed.
fn last_save_failure(slot: &Mutex<SaveFailureRecord>) -> Option<DurableSaveFailure> {
    let record = slot.lock_safe();
    let (what, error) = record.last.as_ref()?;
    Some(DurableSaveFailure {
        what,
        error: error.clone(),
        count: record.count,
    })
}

/// Serializes **durable** state writes (`library.json`, `settings.json`) onto a
/// single background thread, coalescing pending snapshots of each file.
///
/// This is [`Persister`]'s durable sibling, and the differences are the reason
/// it exists as a separate type:
///
/// - **The caller does no I/O**: a command thread only replaces a snapshot in
///   the pending map; serialization happens on the writer thread and the disk is
///   touched there. A UI-thread command therefore cannot block on a slow or
///   contended disk.
/// - **One pending snapshot per destination**: both durable files are written as
///   whole, cumulative snapshots, so if a `library.json` save is still pending
///   when the next mutation queues another, only the newer snapshot matters — it
///   already contains every earlier change. The writer keeps at most one pending
///   snapshot per path and replaces the value *and its serializer* in place,
///   dropping the replaced closure without running it. N rapid mutations
///   therefore cost one queued snapshot, not N: memory does not grow with the
///   mutation rate, and the shutdown flush is not stuck behind writes whose
///   payloads a newer snapshot has already superseded.
/// - **Durable at a known point**: [`flush`](Self::flush) is a barrier — when it
///   returns `true`, every write queued before the call has been attempted (with
///   the newest replacement standing in for the snapshot it superseded).
///   [`Drop`] performs a final bounded drain too. These are what the tests use
///   instead of sleeping, and what
///   [`RiftBackend::shutdown`](crate::RiftBackend::shutdown) relies on.
///
/// Rejected alternative: the earlier version of this type queued every snapshot
/// on an unbounded `std::sync::mpsc` channel. That is the wrong bound for whole
/// snapshots: a burst of mutations while the disk was slow could retain one full
/// library clone per mutation (memory growing with the mutation rate), and made
/// the 10 s shutdown barrier wait for writes whose payloads were already
/// superseded. A bounded channel whose `send` blocks was rejected too, because
/// it would block the UI thread waiting for a slow writer; dropping a write when
/// full was rejected because losing the user's change is never acceptable.
/// Coalescing is safe precisely because the snapshots are cumulative: the newest
/// contains every older change, so replacing the old one loses nothing.
///
/// Ordering and the flush barrier:
///
/// - One FIFO thread publishes the files. A path's pending snapshot keeps the
///   queue position it received when the path first became pending; replacing
///   the payload does not move the entry. Per path the last queued mutation
///   wins; across different paths the order is the order each path first became
///   pending (the files are independent, so only per-path order is observable).
/// - Because a replacement does not move the entry, a flush marker queued after
///   a write cannot be acknowledged before that path's newest snapshot has been
///   attempted — even when the replacement arrives while the flush is waiting.
/// - A write queued after the flush marker sits behind it and is not promised to
///   be included (a write racing the flush may land either side). A `true` never
///   promises every write *succeeded*, only that it was attempted: a failed save
///   is logged and recorded for the backend's failure watchdog.
///
/// The queue is bounded: at most one pending snapshot per distinct path plus one
/// marker per in-flight [`flush`](Self::flush) caller, and a hard cap of
/// [`MAX_PENDING_PATHS`] distinct pending files. The cap is unreachable in the
/// real app; it exists for a pathological caller that keeps inventing paths.
/// When it is reached, a new path is saved inline on the caller's thread —
/// logged as a warning, with a failed inline save recorded exactly like a
/// writer-thread failure — durability over latency, the same fallback as a
/// writer thread that could not be spawned, rather than dropped or queued
/// forever. A newer snapshot for the path the writer is currently publishing is
/// queued even past the soft cap (bounded by one extra): running it inline could
/// race the older publish and leave the older bytes on disk.
///
/// Deliberate limits, matching the synchronous path this replaces:
///
/// - a write that fails is logged (`failed to save {what}: {e}`), recorded for
///   the backend's save-failure watchdog (so the user learns about it), and
///   **not retried** — the disk refusing a write cannot be fixed by waiting, and
///   [`atomic_write`] has already guaranteed no half-written file is left;
/// - serialization failures are logged likewise (`failed to serialize
///   {what}: {e}`) on the writer thread and that snapshot is dropped; they are
///   recorded in the same failure slot because the user's change was lost just
///   as surely;
/// - a serializer that *panics* is caught on the writer thread, logged, recorded
///   as a failed save, and only that snapshot is dropped: the writer keeps
///   draining the jobs queued behind it (a panic here is a bug, not a reason to
///   lose the rest of the queue);
/// - a hard crash (power loss, kill) before the queue drains can still lose the
///   last write. A normal shutdown flushes; a crash is a crash;
/// - the high-frequency, loss-tolerant playback snapshot stays on [`Persister`],
///   which tolerates a dropped final write and additionally drops unchanged
///   payloads.
///
/// A thread-spawn failure degrades rather than failing startup: writes then run
/// synchronously on the caller's thread, which is slower but never silently
/// lost.
pub(crate) struct DurableWriter {
    /// `None` when the writer thread could not be spawned: writes run inline.
    shared: Option<Arc<WriterShared>>,
    /// Last failed save + count, shared with the writer thread and read by the
    /// backend's watchdog. Kept on the writer (not in the job payload) so a
    /// failure report survives even when the queue is being drained by `Drop`.
    failures: Arc<Mutex<SaveFailureRecord>>,
}

/// A queued save's serializer. It is called at most once and, when a newer
/// snapshot for the same path replaces it first, never at all.
type DurableSerialize = Box<dyn FnOnce() -> Result<Vec<u8>, String> + Send>;

/// The newest unclaimed snapshot of one destination file.
struct PendingWrite {
    /// File label the failure record and log lines carry (`"library"`,
    /// `"settings"`).
    what: &'static str,
    serialize: DurableSerialize,
}

/// One place in the writer's FIFO.
enum QueueEntry {
    /// Publish the newest snapshot in [`DurableQueue::pending`] for this path.
    Path(PathBuf),
    /// Answer a [`DurableWriter::flush`] once every entry queued ahead of this
    /// one has been attempted.
    Flush(Sender<()>),
}

/// The queue shared by a writer's handle and its thread.
///
/// `pending` and `order` move together: every path in `pending` has exactly one
/// `QueueEntry::Path` in `order`, and that entry stays where it is while the
/// path remains pending. A newer write to the same path replaces the map value
/// in place and drops the old closure without running it. Keeping the queue
/// position (rather than moving the entry to the back) is what makes the flush
/// barrier linearizable: a path that was already ahead of a flush marker has its
/// newest snapshot attempted before that marker can be acknowledged.
#[derive(Default)]
struct DurableQueue {
    pending: HashMap<PathBuf, PendingWrite>,
    order: VecDeque<QueueEntry>,
    /// The path the writer is publishing right now. The write path checks this
    /// under the same lock as the cap, so an overflow save can never race an
    /// older snapshot of the same file: if this is the path, the new snapshot is
    /// queued (one over the soft cap) instead of running inline.
    in_flight: Option<PathBuf>,
    /// Set by [`DurableWriter`]'s `Drop` after the final flush; the writer exits
    /// once the queue is empty rather than parking a thread for the rest of the
    /// process's life.
    closed: bool,
    /// Set by the writer thread just before it exits, so a late `flush` reports
    /// a gone writer immediately instead of waiting out [`FLUSH_TIMEOUT`] for an
    /// ack that can never arrive.
    exited: bool,
}

/// State shared between a [`DurableWriter`] handle and its thread.
#[derive(Default)]
struct WriterShared {
    queue: Mutex<DurableQueue>,
    /// Wakes the one writer thread; every wait is guarded by `queue`, so a
    /// notification cannot be lost between the state change and the wait.
    ready: Condvar,
}

impl DurableWriter {
    /// Start the durable writer thread.
    pub(crate) fn spawn() -> Self {
        let failures = Arc::new(Mutex::new(SaveFailureRecord::default()));
        let shared = Arc::new(WriterShared::default());
        let thread_shared = Arc::clone(&shared);
        let thread_failures = Arc::clone(&failures);
        match std::thread::Builder::new()
            .name("rift-durable".into())
            .spawn(move || run_durable_writer(thread_shared, thread_failures))
        {
            Ok(_) => Self {
                shared: Some(shared),
                failures,
            },
            Err(e) => {
                warn!(
                    "could not spawn the durable writer thread; library and settings saves \
                     will run on the caller's thread: {e}"
                );
                Self {
                    shared: None,
                    failures,
                }
            }
        }
    }

    /// Queue `value` to be serialized and atomically written to `path`.
    ///
    /// Returns as soon as the snapshot is in the pending map; the caller's
    /// thread never touches the disk unless the writer thread is unavailable
    /// (the spawn failed, it stopped, or the queue is at [`MAX_PENDING_PATHS`]),
    /// in which case the write runs inline rather than being dropped. A pending
    /// snapshot for the same `path` is *replaced* — its value and serializer are
    /// dropped without being run — because a whole-file snapshot already contains
    /// the older change. `what` names the file in log messages, exactly as the
    /// old synchronous `save_json` helper did.
    pub(crate) fn write<T, F>(&self, path: PathBuf, what: &'static str, value: T, serialize: F)
    where
        T: Send + 'static,
        F: FnOnce(&T) -> Result<Vec<u8>, String> + Send + 'static,
    {
        let write = PendingWrite {
            what,
            serialize: Box::new(move || serialize(&value)),
        };
        let Some(shared) = &self.shared else {
            // Degraded mode: no writer thread exists, so the write runs now.
            run_durable_write_catching(path, write, &self.failures);
            return;
        };
        let mut queue = shared.queue.lock_safe();
        if queue.exited {
            // Defensive, matching the old channel-send failure branch: a writer
            // that is already gone must not swallow the change.
            drop(queue);
            warn!("durable writer thread stopped; saving {what} on the caller's thread");
            run_durable_write_catching(path, write, &self.failures);
            return;
        }
        if let Some(slot) = queue.pending.get_mut(&path) {
            // The snapshots are cumulative, so the pending payload is obsolete:
            // swap in the newer value and serializer. The queue position stays as
            // it is; see `DurableQueue` (that is what the flush barrier depends
            // on). Taking the old value lets it be freed after the lock is
            // released, so deallocating a whole library does not stall the writer
            // or another command thread.
            let replaced = std::mem::replace(slot, write);
            drop(queue);
            drop(replaced);
            shared.ready.notify_one();
            return;
        }
        let at_cap = queue.pending.len() >= MAX_PENDING_PATHS;
        let publishing_this_path = queue.in_flight.as_deref() == Some(path.as_path());
        if at_cap && !publishing_this_path {
            // Durability over latency: write now on the caller's thread rather
            // than drop the change or grow an unbounded queue. Inline is safe
            // here because no older snapshot of this path is in flight (that
            // case is excluded above), so this cannot race the writer for the
            // same file.
            drop(queue);
            warn!(
                "durable writer queue already holds {} pending files; saving {what} on the \
                 caller's thread instead of dropping it",
                MAX_PENDING_PATHS
            );
            run_durable_write_catching(path, write, &self.failures);
            return;
        }
        // A new destination (or the one the writer is publishing right now, in
        // which case queueing one over the soft cap is what keeps the newest
        // snapshot from being published before the older one).
        queue.pending.insert(path.clone(), write);
        queue.order.push_back(QueueEntry::Path(path));
        drop(queue);
        shared.ready.notify_one();
    }

    /// The most recent failed durable save, if any, with the total failure
    /// count since the writer started.
    ///
    /// The backend's save-failure watchdog polls this and emits one user-facing
    /// notice per new burst; `count` is what distinguishes a new failure from a
    /// repeated observation of the same one. The writer itself never touches the
    /// event bus (it is created before the bus exists).
    pub(crate) fn last_failure(&self) -> Option<DurableSaveFailure> {
        last_save_failure(&self.failures)
    }

    /// A writer with no background thread: every [`write`](Self::write) runs
    /// synchronously on the calling thread.
    ///
    /// This is the degraded mode [`spawn`](Self::spawn) falls back to when the
    /// thread cannot be created; the constructor exists so tests can pin that
    /// path without having to make thread creation fail.
    #[cfg(test)]
    pub(crate) fn spawn_inline() -> Self {
        Self {
            shared: None,
            failures: Arc::new(Mutex::new(SaveFailureRecord::default())),
        }
    }

    /// The queue depths, for tests that gate the writer thread and want to prove
    /// a burst backs up without growing: `(pending snapshots, FIFO entries)`.
    #[cfg(test)]
    fn queue_depths(&self) -> (usize, usize) {
        match &self.shared {
            Some(shared) => {
                let queue = shared.queue.lock_safe();
                (queue.pending.len(), queue.order.len())
            }
            None => (0, 0),
        }
    }

    /// Block until every write queued before this call has been attempted.
    ///
    /// "Queued" is the instant this method inserts its marker: every
    /// [`write`](Self::write) whose insertion completed first is ahead of it in
    /// the FIFO and is attempted before the ack. Coalescing changes what
    /// "attempted" means for a file with several writes: the older snapshots were
    /// replaced and are never run, and the newest snapshot for that path is
    /// attempted before the ack — a replacement that arrives while the flush is
    /// waiting keeps the path's position ahead of the marker. A write queued
    /// after the marker, including one racing this call and landing behind it, is
    /// not promised. A write that runs inline (degraded mode or queue overflow)
    /// has already completed before it returns, so it is trivially before any
    /// later flush.
    ///
    /// Returns `true` when the queue was drained, `false` when the writer thread
    /// is gone or did not drain within [`FLUSH_TIMEOUT`] (both are logged). A
    /// `true` does not promise every write *succeeded*: a failed write is logged
    /// and recorded in [`last_failure`](Self::last_failure) on the writer thread,
    /// exactly as the old synchronous `save_json` warned, because an unwritable
    /// disk cannot be waited into working order.
    pub(crate) fn flush(&self) -> bool {
        let Some(shared) = &self.shared else {
            // Inline fallback: nothing is ever queued, every write already ran.
            return true;
        };
        let (ack_tx, ack_rx) = mpsc::channel();
        {
            let mut queue = shared.queue.lock_safe();
            if queue.exited {
                warn!("durable writer thread is gone; the last writes may not have reached disk");
                return false;
            }
            // The marker sits behind every write inserted before this call. A
            // replacement never moves its path's entry, so the newest snapshot of
            // an already-queued path is still ahead of this marker.
            queue.order.push_back(QueueEntry::Flush(ack_tx));
        }
        shared.ready.notify_one();
        match ack_rx.recv_timeout(FLUSH_TIMEOUT) {
            Ok(()) => true,
            Err(e) => {
                warn!("durable writes did not flush within {FLUSH_TIMEOUT:?}: {e}");
                false
            }
        }
    }
}

impl Drop for DurableWriter {
    /// Best-effort final drain.
    ///
    /// The explicit flush in `RiftBackend`'s shutdown is the real guarantee, but
    /// a store dropped without one (tests, a future caller) must not leave a
    /// queued write racing process exit. Bounded by [`FLUSH_TIMEOUT`] so a
    /// wedged disk cannot hang the process on its way out.
    fn drop(&mut self) {
        let _ = self.flush();
        if let Some(shared) = &self.shared {
            // The flush above attempted everything queued; tell the writer
            // thread to leave once it finishes anything still in hand, instead
            // of parking it for the rest of the process's life. Jobs still queued
            // after a timed-out flush are drained before the thread exits.
            shared.queue.lock_safe().closed = true;
            shared.ready.notify_one();
        }
    }
}

/// The writer thread: one FIFO over [`QueueEntry`]s. It claims one snapshot at
/// a time and never holds the queue lock across serialization or disk I/O, so a
/// caller only contends for the short bookkeeping critical section.
fn run_durable_writer(shared: Arc<WriterShared>, failures: Arc<Mutex<SaveFailureRecord>>) {
    loop {
        // Claiming and marking the path in-flight happen in one critical
        // section: `write`'s overflow check uses `in_flight` to prove that no
        // older snapshot of a path is mid-publish before deciding to write that
        // path inline.
        let claim = {
            let mut queue = shared.queue.lock_safe();
            loop {
                match queue.order.pop_front() {
                    Some(QueueEntry::Path(path)) => {
                        let write = queue.pending.remove(&path);
                        queue.in_flight = Some(path.clone());
                        break Some((path, write));
                    }
                    // Every entry ahead of this marker was claimed and completed
                    // in earlier iterations, which is exactly what the ack
                    // promises. A send error means the flushing caller gave up
                    // (timeout) and has already logged it.
                    Some(QueueEntry::Flush(ack)) => {
                        let _ = ack.send(());
                    }
                    None => {
                        if queue.closed {
                            break None;
                        }
                        queue = shared
                            .ready
                            .wait(queue)
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                    }
                }
            }
        };
        let Some((path, write)) = claim else {
            shared.queue.lock_safe().exited = true;
            return;
        };
        if let Some(write) = write {
            run_durable_write_catching(path, write, &failures);
        }
        // Clear in-flight only after the publish finished (or the snapshot was
        // lost to a panic, which `run_durable_write_catching` recorded). Taking
        // the lock again orders this after any concurrent `write`, which is why
        // the overflow check can rely on it.
        shared.queue.lock_safe().in_flight = None;
    }
}

/// Serialize and atomically publish one snapshot. Runs on the writer thread, or
/// on the caller's thread in degraded/overflow mode. Every failure is also
/// recorded in `failures` for the backend's watchdog — logging alone is
/// invisible to the user, who otherwise keeps believing the change persisted.
fn run_durable_write(path: &Path, write: PendingWrite, failures: &Mutex<SaveFailureRecord>) {
    let PendingWrite { what, serialize } = write;
    match serialize() {
        Ok(bytes) => {
            if let Err(e) = atomic_write(path, &bytes) {
                record_save_failure(failures, what, &e.to_string());
                warn!("failed to save {what}: {e}");
            }
        }
        Err(e) => {
            record_save_failure(failures, what, &e);
            warn!("failed to serialize {what}: {e}");
        }
    }
}

/// Run one snapshot, catching a panic from its serializer.
///
/// Without this, a panicking serializer kills the writer thread and with it
/// every job already queued behind it: none of those would be written and
/// nothing would say so. Catching keeps the fatal-panic blast radius to the one
/// snapshot (the same semantic as a serialization `Err`) and, because shutdown
/// waits on the writer's flush barrier, it also stops a bad snapshot from
/// turning a quit into a 10-second flush timeout. `AssertUnwindSafe` is sound
/// here: the snapshot owns its payload, so a panic mid-job cannot leave shared
/// state behind. The panic is recorded as a failed save too: that change was
/// lost.
fn run_durable_write_catching(
    path: PathBuf,
    write: PendingWrite,
    failures: &Mutex<SaveFailureRecord>,
) {
    let what = write.what;
    let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run_durable_write(&path, write, failures)
    }));
    if ran.is_err() {
        record_save_failure(failures, what, "the write job panicked");
        warn!("durable writer job for {what} panicked; dropped that job and kept the queue alive");
    }
}

/// Write via a temp file + rename so a crash mid-write can't leave a truncated
/// file behind.
///
/// The bytes are flushed to the storage device before the rename, so when the
/// filesystem supports it the *contents* of the published file are durable, not
/// merely visible in the page cache; a crash can then lose the rename itself,
/// leaving the previous complete version — never a mix of old and new. A flush
/// failure is logged and the rename still happens (a filesystem that cannot
/// flush should not turn every save into a lost write), and the cost is paid on
/// the writer thread: no command or UI thread calls this directly.
pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = temp_path_for(path)?;
    let written = write_and_publish(&tmp, path, bytes);
    if written.is_err() {
        // Neither a failed write nor a failed rename should leave scratch next
        // to the user's data — it would accumulate for every failed save.
        let _ = std::fs::remove_file(&tmp);
    }
    written
}

/// The temp-file half of [`atomic_write`]: create, write, flush, then rename.
///
/// Split out so the ordering (flush before publish, close before rename on
/// Windows) reads as the sequence it is, instead of a chained expression whose
/// evaluation order a future edit could rearrange.
fn write_and_publish(tmp: &Path, path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;

    {
        let mut file = std::fs::File::create(tmp)?;
        file.write_all(bytes)?;
        // The file is closed before the rename below (`File` drops at the end
        // of this scope), which Windows requires before a file can be renamed.
        if let Err(e) = file.sync_all() {
            // Durability degrades, correctness does not: the complete bytes are
            // still in the file, and the rename either publishes them or fails
            // (in which case the caller keeps the old file). Log instead of
            // failing the save, because a filesystem that cannot flush would
            // otherwise lose every write.
            warn!(
                "could not flush {} to disk before publishing it: {e}",
                tmp.display()
            );
        }
    }
    std::fs::rename(tmp, path)?;
    // Make the rename durable where the platform exposes a directory handle:
    // without this a crash right after the rename can leave the old name. On
    // Windows a directory cannot be opened as a `File`; NTFS journals the rename,
    // and the file data is already flushed above.
    #[cfg(unix)]
    if let Some(parent) = path.parent() {
        if let Ok(dir) = std::fs::File::open(parent) {
            let _ = dir.sync_all();
        }
    }
    Ok(())
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
        // reqwest follows redirects, so `resp.url()` is where the request
        // actually ended up — not just where it was aimed. The status is
        // recorded, not judged: see [`came_from_youtube`].
        Ok(resp) => {
            let status = resp.status().as_u16();
            debug!("online probe: {status} from {}", resp.url());
            came_from_youtube(Some(status), resp.url().host_str())
        }
        // Transport failure: genuinely offline (or YouTube is unreachable).
        Err(e) => {
            debug!("online probe failed: {e}");
            came_from_youtube(None, None)
        }
    }
}

/// Whether a probe result proves Rift reached `YouTube`.
///
/// No response at all (`None`, `None`) is the one thing that really is
/// "offline". Otherwise the host is the whole answer, and **the status is
/// deliberately not part of the gate** — it is in the signature so the caller can
/// log it and so the tests below can state the indifference explicitly, and it is
/// never consulted:
///
/// - Reaching `YouTube` at all is what this probe asks about. A 403 or 429 is the
///   *most* likely status while a rate-limited download retry pass runs, and
///   calling that "offline" tells the user to reconnect when their connection is
///   fine — the false "offline" this rule has already been wrong about twice.
///   The asymmetry decides it: a false "online" costs one background backoff
///   ladder; a false "offline" costs a misleading error and no retries at all.
/// - **The host is what catches an interception.** A captive portal or
///   transparent proxy answers the probe itself — with its own 200 directly, or
///   by redirecting to its login page — and its host is not `YouTube`'s. Unlike a
///   response header it is a property of the request/redirect chain rather than a
///   string Google can rename out from under us.
///
/// That header is how this rule got here: `x-youtube-client-name` (an *innertube
/// API* request header) is on no response the HTML root sends, so every online
/// machine read as offline; `server: ESF` fixed that but was a single mutable
/// string sitting in the gate. Both are recorded in this history so the next
/// person tempted to gate on a header — or on a status — knows what it costs.
///
/// Accepted residual: a proxy that MITMs HTTPS with a trusted certificate and
/// answers the probe without redirecting keeps the requested host, so it reads
/// as online. That is the cheap direction by the asymmetry above.
///
/// Split out from [`is_online`] so the rule is testable without a network round
/// trip.
fn came_from_youtube(status: Option<u16>, final_host: Option<&str>) -> bool {
    // Recorded by the caller, never a gate: the tests assert that a 403/429 from
    // YouTube's own edge is still online, which is the behaviour this line is
    // about. Re-adding a status check here fails them.
    let _ = status;
    final_host.is_some_and(is_youtube_host)
}

/// Whether `host` is `youtube.com` or a subdomain of it.
///
/// The boundary matters: a bare `ends_with("youtube.com")` also accepts
/// `notyoutube.com` and `youtube.com.example.net`, which a portal could be
/// served from.
fn is_youtube_host(host: &str) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    host == "youtube.com" || host.ends_with(".youtube.com")
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

    /// Whether this test process is running as root, detected from the owner of
    /// the scratch directory it just created.
    ///
    /// Root ignores the permission bits the unreadable-file tests depend on, so
    /// they skip rather than fail. `std` has no `geteuid` and this crate forbids
    /// `unsafe`, so the effective uid is read back from a file the process owns
    /// (the kernel stamps it with the creator's effective uid).
    #[cfg(unix)]
    pub(crate) fn running_as_root(dir: &TempDir) -> bool {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(dir.path()).is_ok_and(|meta| meta.uid() == 0)
    }

    /// Restricts a state file to `0o000`, restoring it (and the directory, if
    /// [`lock_parent_dir`](Self::lock_parent_dir) was used) on drop.
    ///
    /// [`lock_parent_dir`](Self::lock_parent_dir) is the Windows
    /// sharing-violation shape on Unix: the file cannot be read (`EACCES`), and
    /// the quarantine's rename cannot create its backup because the directory is
    /// not writable. The directory must be restored before [`TempDir`]'s own
    /// `Drop` can delete it, hence the guard rather than a cleanup line that an
    /// assertion failure would skip.
    #[cfg(unix)]
    pub(crate) struct UnreadableFile {
        dir: PathBuf,
        file: PathBuf,
    }

    #[cfg(unix)]
    impl UnreadableFile {
        /// Make the state file unreadable; the directory stays writable, so the
        /// quarantine can still move the file aside.
        pub(crate) fn apply(dir: &Path, file: &Path) -> Self {
            use std::os::unix::fs::PermissionsExt;
            let guard = Self {
                dir: dir.to_path_buf(),
                file: file.to_path_buf(),
            };
            std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o000))
                .expect("make the state file unreadable");
            guard
        }

        /// Also make the parent directory non-writable, so the quarantine's
        /// rename fails and the file cannot be preserved.
        pub(crate) fn lock_parent_dir(&self) {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&self.dir, std::fs::Permissions::from_mode(0o500))
                .expect("make the data directory non-writable");
        }

        /// Restore just enough to read the file back while the directory stays
        /// non-writable (so a retried save would still fail).
        pub(crate) fn make_file_readable(&self) {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&self.file, std::fs::Permissions::from_mode(0o600))
                .expect("restore read access to the state file");
        }
    }

    #[cfg(unix)]
    impl Drop for UnreadableFile {
        fn drop(&mut self) {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&self.file, std::fs::Permissions::from_mode(0o600));
            let _ = std::fs::set_permissions(&self.dir, std::fs::Permissions::from_mode(0o700));
        }
    }

    /// Makes the scratch directory read-only while leaving its files readable,
    /// restoring the mode on drop.
    ///
    /// This is the shape that distinguishes the two corruption outcomes: the
    /// bytes can be read and the parser can reject them, but creating the
    /// `.json.bad` sibling (the quarantine rename) fails with no write permission
    /// on the directory. Root ignores permission bits, so tests using it skip via
    /// [`running_as_root`]. The restore-on-drop is what lets [`TempDir`] clean up
    /// after a failing assertion too.
    #[cfg(unix)]
    pub(crate) struct ReadOnlyDir {
        path: PathBuf,
    }

    #[cfg(unix)]
    impl ReadOnlyDir {
        pub(crate) fn apply(path: &Path) -> Self {
            use std::os::unix::fs::PermissionsExt;
            let guard = Self {
                path: path.to_path_buf(),
            };
            std::fs::set_permissions(&guard.path, std::fs::Permissions::from_mode(0o500))
                .expect("make the data directory read-only");
            guard
        }
    }

    #[cfg(unix)]
    impl Drop for ReadOnlyDir {
        fn drop(&mut self) {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(0o700));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::TempDir;
    use super::*;

    #[test]
    fn the_live_youtube_response_shape_counts_as_online() {
        // Captured 2026-10-10 through this same client (HTTP/2, no User-Agent):
        // `HEAD https://www.youtube.com/` answers 200 OK from
        // `final_url=https://www.youtube.com/` with `server: ESF`,
        // `content-type: text/html`, and **no** `x-youtube-client-name`.
        //
        // Neither the status nor any header is part of the rule: the host is a
        // property of the request/redirect chain, so a response from YouTube with
        // no header this code has ever heard of still counts.
        assert!(came_from_youtube(Some(200), Some("www.youtube.com")));
        assert!(came_from_youtube(Some(200), Some("youtube.com")));
        assert!(came_from_youtube(Some(200), Some("m.youtube.com")));
    }

    /// The residual this rule was refined for: a response from `YouTube`'s own edge
    /// counts whatever it says. Rate-limiting is the likeliest reason a download
    /// retry pass is running at all, and "you appear to be offline — reconnect"
    /// is the wrong thing to tell a user whose connection is fine. A false
    /// "online" only costs a background backoff ladder.
    #[test]
    fn any_status_from_youtube_is_online() {
        for status in [200, 204, 301, 302, 403, 429, 500, 503] {
            assert!(
                came_from_youtube(Some(status), Some("www.youtube.com")),
                "status {status} from YouTube must not read as offline"
            );
        }
    }

    /// The portal case this item exists for: the request is answered by
    /// something that is not `YouTube`. A redirect to the portal's own https login
    /// page is 2xx and the most common shape, so the host is what catches it —
    /// and no status it invents rescues it.
    #[test]
    fn an_answer_that_did_not_end_at_youtube_is_not_online() {
        for host in [
            "portal.example.net",
            "login.captive.app",
            // Names that merely contain "youtube.com" are not YouTube, and a
            // portal can be served from one.
            "notyoutube.com",
            "youtube.com.evil.example.net",
            "www.youtube.com.phish.net",
        ] {
            for status in [200, 302, 429] {
                assert!(
                    !came_from_youtube(Some(status), Some(host)),
                    "{host} (status {status}) must not count as online"
                );
            }
        }
    }

    /// A transport failure is the only genuine "offline": no response at all, so
    /// no host to check.
    #[test]
    fn no_response_at_all_is_offline() {
        assert!(!came_from_youtube(None, None));
    }

    /// A trailing dot is a legal spelling of the same fully-qualified name.
    #[test]
    fn a_fully_qualified_youtube_host_is_still_youtube() {
        assert!(came_from_youtube(Some(200), Some("www.youtube.com.")));
        assert!(came_from_youtube(Some(200), Some("YOUTUBE.COM")));
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

    /// A file that is not valid UTF-8 never reached the JSON parser before:
    /// `read_to_string` failed first, and the old code treated *every* read
    /// failure as "no file". That returned defaults silently — and the next
    /// save overwrote the unreadable library. It is corrupt state like any
    /// other, so it takes the quarantine path.
    #[test]
    fn a_state_file_that_is_not_valid_utf8_is_quarantined_too() {
        let dir = TempDir::new("util-corrupt-utf8");
        let path = dir.join("settings.json");
        std::fs::write(&path, [b'{', 0xFF, 0xFE, 0x00, b'}']).expect("seed invalid utf-8");

        let loaded: serde_json::Value = load_json(&path);
        assert_eq!(loaded, serde_json::Value::Null);
        assert!(!path.exists(), "the unreadable file must be moved aside");
        assert!(
            path.with_extension("json.bad").exists(),
            "the invalid-UTF-8 file must survive as a .json.bad backup"
        );
    }

    /// The historical single `.json.bad` name must not make a second corruption
    /// destroy the backup the first one produced: the old code renamed over it.
    #[test]
    fn a_second_corrupt_file_does_not_overwrite_the_first_backup() {
        let dir = TempDir::new("util-corrupt-twice");
        let path = dir.join("library.json");

        std::fs::write(&path, b"{ first corruption").expect("seed first");
        let _: serde_json::Value = load_json(&path);
        std::fs::write(&path, b"{ second corruption").expect("seed second");
        let _: serde_json::Value = load_json(&path);

        assert_eq!(
            std::fs::read(path.with_extension("json.bad")).expect("first backup"),
            b"{ first corruption",
            "the first corrupt copy must stay intact"
        );
        assert_eq!(
            std::fs::read(path.with_extension("json.bad.1")).expect("second backup"),
            b"{ second corruption",
            "the second corrupt copy gets its own numbered backup"
        );
    }

    /// A missing file is normal, so it loads defaults and a later save is
    /// expected to create the file.
    /// The read-only decision is a property of the load status, not of an OS
    /// error path: pin the whole matrix so a future status cannot silently
    /// become writable (or a clean load read-only).
    #[test]
    fn only_an_unpreserved_failure_disables_saving() {
        use LoadStatus::{
            Corrupt, CorruptUnpreserved, Loaded, Missing, Repaired, UnreadablePreserved,
            UnreadableUnpreserved,
        };
        let path = Path::new("library.json");
        for status in [Loaded, Repaired, Missing, Corrupt, UnreadablePreserved] {
            assert!(
                status.may_overwrite(),
                "{status:?} must allow a save (the original is absent or preserved)"
            );
        }
        for status in [CorruptUnpreserved, UnreadableUnpreserved] {
            assert!(
                !status.may_overwrite(),
                "{status:?} could not be preserved and is the only read-only class"
            );
        }
        assert!(Loaded.warning(path).is_none());
        assert!(Missing.warning(path).is_none());
        assert!(
            Repaired.warning(path).is_none(),
            "a repairable file is loaded successfully and is not a startup warning"
        );
        assert!(
            Corrupt.warning(path).is_none(),
            "quarantined parse corruption stays silent, as before"
        );
        assert!(UnreadablePreserved
            .warning(path)
            .is_some_and(|w| w.contains("moved aside")));
        assert!(CorruptUnpreserved
            .warning(path)
            .is_some_and(|w| w.contains("corrupt file could not be preserved")
                && w.contains("changes will not be saved")));
        assert!(UnreadableUnpreserved
            .warning(path)
            .is_some_and(|w| w.contains("changes will not be saved")));
    }

    /// The injected-parser seam: a document that parsed but had to drop rows is
    /// `Repaired`, not `Corrupt`, and it is writable so the caller can persist
    /// the repaired value.
    #[test]
    fn a_repaired_document_is_writable_and_not_a_warning() {
        let dir = TempDir::new("util-repaired-status");
        let path = dir.join("library.json");
        std::fs::write(&path, br#"{"whatever":"the tolerant parser reads"}"#)
            .expect("seed document");

        let (value, status) = load_json_with_parser(&path, |_| Ok((vec!["kept"], true)));

        assert_eq!(value, vec!["kept"]);
        assert_eq!(status, LoadStatus::Repaired);
        assert!(status.may_overwrite());
        assert!(status.warning(&path).is_none());
        assert!(
            path.exists(),
            "a repairable document must not be moved aside as corrupt"
        );
        assert!(!path.with_extension("json.bad").exists());
    }

    /// Windows' real equivalent of the Unix permission tests: hold the state
    /// file open with `share_mode(0)`, which denies every subsequent open
    /// (including this process's own read and the quarantine rename, which needs
    /// `FILE_SHARE_DELETE`). The loader must treat it as an unreadable,
    /// unpreserved file and refuse to overwrite the only copy.
    ///
    /// The platform-independent injected-reader test above models the same
    /// branch everywhere; this one proves the actual Win32 sharing path that
    /// models, because Rust maps the sharing violation to an OS error and the
    /// rename failure depends on the holder's share mode. CI's Windows job runs
    /// it; Linux/macOS compile it out.
    #[cfg(windows)]
    #[test]
    fn a_windows_sharing_violation_is_unreadable_and_refuses_to_overwrite() {
        use std::os::windows::fs::OpenOptionsExt;

        let dir = TempDir::new("util-windows-sharing");
        let path = dir.join("library.json");
        let original = br#"{"liked":[],"playlists":[],"recently_played":[]}"#;
        std::fs::write(&path, original).expect("write a state file");
        let held = std::fs::OpenOptions::new()
            .read(true)
            // No FILE_SHARE_READ/WRITE/DELETE: the next open and the quarantine
            // rename both fail with ERROR_SHARING_VIOLATION.
            .share_mode(0)
            .open(&path)
            .expect("open the state file with no sharing");
        let (_value, status): (serde_json::Value, LoadStatus) = load_json_with_status(&path);
        assert_eq!(
            status,
            LoadStatus::UnreadableUnpreserved,
            "a sharing violation must not be mistaken for a missing file, and the \
             quarantine rename cannot preserve it while the file is held"
        );
        assert!(
            !status.may_overwrite(),
            "an unpreserved file is the only copy; the store must become read-only"
        );
        drop(held);
        assert_eq!(
            std::fs::read(&path).expect("the file is readable again after release"),
            original,
            "the loader must leave the held file byte-identical"
        );
        assert!(
            !path.with_extension("json.bad").exists(),
            "the quarantine rename must not have created a backup"
        );
    }

    /// Exercise the read-error branch with an injected error, independent of
    /// Exercise the read-error branch with an injected error, independent of
    /// the host OS: the file exists, reading it "fails", and the loader must
    /// preserve it before starting from defaults.
    #[test]
    fn an_injected_read_error_preserves_the_file_before_defaulting() {
        let dir = TempDir::new("util-injected-read");
        let path = dir.join("state.json");
        std::fs::write(&path, br#"{"volume":0.75}"#).expect("seed valid file");

        let (loaded, status): (serde_json::Value, LoadStatus) = load_json_from(&path, |_| {
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "injected read failure",
            ))
        });

        assert_eq!(loaded, serde_json::Value::Null, "defaults are the value");
        assert_eq!(status, LoadStatus::UnreadablePreserved);
        assert!(status.may_overwrite(), "the preserved original is a backup");
        assert!(
            !path.exists(),
            "the unreadable file must be moved out of the live path"
        );
        assert_eq!(
            std::fs::read(path.with_extension("json.bad")).expect("backup exists"),
            br#"{"volume":0.75}"#,
            "the bytes must survive the read failure"
        );
    }

    #[test]
    fn a_missing_file_loads_defaults_and_stays_writable() {
        let dir = TempDir::new("util-missing-status");
        let path = dir.join("never-written.json");

        let (loaded, status): (serde_json::Value, LoadStatus) = load_json_with_status(&path);
        assert_eq!(loaded, serde_json::Value::Null);
        assert_eq!(status, LoadStatus::Missing);
        assert!(status.may_overwrite(), "a missing file must stay writable");
        assert!(
            status.warning(&path).is_none(),
            "a first launch is not a read error"
        );
    }

    /// Parse/UTF-8 corruption keeps its historical contract: quarantined, the
    /// defaults are the state, and the store stays writable.
    #[test]
    fn a_corrupt_file_quarantines_and_stays_writable() {
        let dir = TempDir::new("util-corrupt-status");
        let path = dir.join("settings.json");
        std::fs::write(&path, b"{ not json").expect("seed corrupt file");

        let (loaded, status): (serde_json::Value, LoadStatus) = load_json_with_status(&path);
        assert_eq!(loaded, serde_json::Value::Null);
        assert_eq!(status, LoadStatus::Corrupt);
        assert!(status.may_overwrite(), "corrupt JSON must stay writable");
        assert!(
            status.warning(&path).is_none(),
            "parse corruption did not grow a startup toast in this change"
        );
        assert!(path.with_extension("json.bad").is_file());
    }

    /// The other half of parse corruption: if the `.json.bad` rename cannot
    /// happen, the corrupt file is the user's only copy. The status must forbid
    /// saving (the store becomes read-only for the session) and the startup
    /// warning must say the corrupt file could not be preserved.
    ///
    /// A read-only directory is the portable-in-shape way to make the rename fail
    /// without deleting the file: the bytes are readable (so the parser is what
    /// fails), but no new name can be created beside them.
    #[cfg(unix)]
    #[test]
    fn a_corrupt_file_that_cannot_be_quarantined_is_read_only() {
        for (tag, bytes) in [
            ("util-corrupt-unpreserved", b"{ not json".to_vec()),
            (
                "util-corrupt-unpreserved-utf8",
                vec![b'{', 0xFF, 0xFE, 0x00, b'}'],
            ),
        ] {
            let dir = TempDir::new(tag);
            if testing::running_as_root(&dir) {
                return; // permission bits do not stop root
            }
            let path = dir.join("library.json");
            std::fs::write(&path, &bytes).expect("seed corrupt file");

            let guard = testing::ReadOnlyDir::apply(dir.path());
            let (loaded, status): (serde_json::Value, LoadStatus) = load_json_with_status(&path);
            assert_eq!(loaded, serde_json::Value::Null, "defaults are the value");
            assert_eq!(status, LoadStatus::CorruptUnpreserved);
            assert!(
                !status.may_overwrite(),
                "a corrupt file that could not be preserved must never be overwritten"
            );
            assert!(
                status
                    .warning(&path)
                    .is_some_and(|w| w.contains("corrupt file could not be preserved")
                        && w.contains("changes will not be saved")),
                "the user must learn that the corrupt file could not be preserved: {:?}",
                status.warning(&path)
            );
            assert!(
                path.exists(),
                "the failed quarantine must not remove the only copy"
            );
            assert!(
                !path.with_extension("json.bad").exists(),
                "a failed quarantine must not leave a bogus backup behind"
            );

            drop(guard);
            assert_eq!(
                std::fs::read(&path).expect("the original must still be there"),
                bytes,
                "the corrupt bytes must be left exactly as they were"
            );
        }
    }

    /// The bug this change is about: an unreadable file takes the preservation
    /// path, so the defaults that replace it can no longer destroy the original.
    #[cfg(unix)]
    #[test]
    fn an_unreadable_file_is_quarantined_before_defaults_are_used() {
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new("util-unreadable-preserved");
        if testing::running_as_root(&dir) {
            return; // permission bits do not stop root
        }
        let path = dir.join("library.json");
        std::fs::write(&path, b"{\"liked\":[]}").expect("seed valid file");

        let _guard = testing::UnreadableFile::apply(dir.path(), &path);
        let (loaded, status): (serde_json::Value, LoadStatus) = load_json_with_status(&path);
        assert_eq!(loaded, serde_json::Value::Null);
        assert_eq!(status, LoadStatus::UnreadablePreserved);
        assert!(
            status.may_overwrite(),
            "the original is safe once it has been moved aside"
        );
        assert!(
            status
                .warning(&path)
                .is_some_and(|w| w.contains("moved aside")),
            "the user must learn that the file was moved aside"
        );
        assert!(!path.exists(), "the unreadable file must be moved aside");

        // The rename keeps the file's mode, so restore read access to check the
        // bytes that survived.
        let backup = path.with_extension("json.bad");
        std::fs::set_permissions(&backup, std::fs::Permissions::from_mode(0o600))
            .expect("make the backup readable");
        assert_eq!(std::fs::read(&backup).expect("backup"), b"{\"liked\":[]}");
    }

    /// When the file cannot even be renamed aside, saving defaults would destroy
    /// the only copy: the status must forbid the store from writing.
    #[cfg(unix)]
    #[test]
    fn an_unreadable_file_that_cannot_be_quarantined_is_read_only() {
        let dir = TempDir::new("util-unreadable-unpreserved");
        if testing::running_as_root(&dir) {
            return;
        }
        let path = dir.join("settings.json");
        std::fs::write(&path, b"{\"volume\":0.5}").expect("seed valid file");

        let guard = testing::UnreadableFile::apply(dir.path(), &path);
        guard.lock_parent_dir();
        let (loaded, status): (serde_json::Value, LoadStatus) = load_json_with_status(&path);
        assert_eq!(loaded, serde_json::Value::Null);
        assert_eq!(status, LoadStatus::UnreadableUnpreserved);
        assert!(
            !status.may_overwrite(),
            "an unpreserved file must never be overwritten by defaults"
        );
        assert!(
            status
                .warning(&path)
                .is_some_and(|w| w.contains("changes will not be saved")),
            "the user must be told that saves are disabled: {:?}",
            status.warning(&path)
        );

        guard.make_file_readable();
        assert_eq!(
            std::fs::read(&path).expect("the original is still there"),
            b"{\"volume\":0.5}"
        );
        assert!(
            !path.with_extension("json.bad").exists(),
            "a failed quarantine must not leave a bogus backup behind"
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

    /// M8's other half. `emit_queue` calls this on the event loop and the
    /// snapshot can hold 1000 tracks, so `write` must hand the serialization to
    /// the persist thread rather than doing it in the caller. Serialize inline
    /// (the pre-M8 shape) and the closure runs on *this* thread, which is what
    /// the thread-id assertion pins.
    #[test]
    fn the_persister_serializes_off_the_callers_thread() {
        let dir = TempDir::new("util-persister-thread");
        let path = dir.join("snapshot.json");
        let persister = Persister::spawn();

        let caller = std::thread::current().id();
        let (ran_tx, ran_rx) = std::sync::mpsc::channel();
        persister
            .write(path.clone(), (), move |_| {
                let _ = ran_tx.send(std::thread::current().id());
                b"payload".to_vec()
            })
            .expect("queue write");

        let serialized_on = ran_rx
            .recv_timeout(std::time::Duration::from_secs(30))
            .expect("the persister thread should have serialized the payload");
        assert_ne!(
            serialized_on, caller,
            "serialization must happen on the persister's own thread, not the caller's"
        );

        drain(&persister, &dir);
        assert_eq!(std::fs::read(&path).expect("read"), b"payload");
    }

    /// The durable writer's contract in one test: the caller queues, `flush` is
    /// the only wait, and after it returns the bytes are on disk. No sleeping.
    #[test]
    fn a_durable_write_is_on_disk_after_flush() {
        let dir = TempDir::new("util-durable-flush");
        let path = dir.join("library.json");
        let writer = DurableWriter::spawn();

        writer.write(path.clone(), "library", vec![1u8, 2, 3], |v: &Vec<u8>| {
            Ok(v.clone())
        });

        assert!(writer.flush(), "a healthy writer must drain its queue");
        assert_eq!(std::fs::read(&path).expect("read"), [1, 2, 3]);
    }

    /// A failed save must leave a record the backend can turn into a
    /// user-visible notice: the log line alone is invisible to the user, who
    /// otherwise keeps believing the change persisted.
    #[test]
    fn a_failed_durable_save_is_recorded_with_its_reason_and_count() {
        let dir = TempDir::new("util-durable-failure");
        let writer = DurableWriter::spawn();
        assert!(
            writer.last_failure().is_none(),
            "a fresh writer has nothing to report"
        );

        // A file where the target's parent directory should be makes
        // `atomic_write` fail while creating its temp file — the same shape as
        // an unwritable data directory.
        let blocker = dir.join("blocked");
        std::fs::write(&blocker, b"not a directory").expect("write the blocker");
        writer.write(
            blocker.join("library.json"),
            "library",
            b"payload".to_vec(),
            |v: &Vec<u8>| Ok(v.clone()),
        );
        assert!(writer.flush(), "the failed job must still drain");

        let failure = writer
            .last_failure()
            .expect("the failed write is recorded for the backend");
        assert_eq!(failure.what, "library");
        assert_eq!(failure.count, 1);
        assert!(
            !failure.error.is_empty(),
            "the io error is kept as the user-facing reason"
        );

        // A serialization failure is also a lost change, and increments the
        // count rather than replacing the record silently.
        writer.write(dir.join("settings.json"), "settings", (), |_| {
            Err("serialize boom".to_owned())
        });
        assert!(writer.flush());
        let failure = writer.last_failure().expect("the second failure recorded");
        assert_eq!(failure.count, 2, "the count survives across failed saves");
        assert_eq!(failure.what, "settings", "the last failed file is named");
        assert!(
            failure.error.contains("serialize boom"),
            "the last reason is kept: {:?}",
            failure.error
        );

        // A successful save must not clear or inflate the record: the watchdog
        // needs the count to still distinguish the already-reported failure.
        writer.write(
            dir.join("library.json"),
            "library",
            b"ok".to_vec(),
            |v: &Vec<u8>| Ok(v.clone()),
        );
        assert!(writer.flush());
        let after_success = writer.last_failure().expect("the record remains");
        assert_eq!(after_success.count, 2);
        assert_eq!(after_success.what, "settings");
    }

    /// The retained reason is bounded so a very long path in an io error cannot
    /// grow the slot for the whole session; the ellipsis marks the cut.
    #[test]
    fn a_failed_save_record_keeps_only_a_bounded_reason() {
        let dir = TempDir::new("util-durable-failure-bound");
        let writer = DurableWriter::spawn();
        let long_reason = "x".repeat(MAX_SAVE_FAILURE_CHARS * 2);
        writer.write(dir.join("settings.json"), "settings", (), move |_| {
            Err(long_reason.clone())
        });
        assert!(writer.flush());

        let failure = writer.last_failure().expect("the failure is recorded");
        assert!(
            failure.error.chars().count() <= MAX_SAVE_FAILURE_CHARS + 1,
            "the stored reason is bounded ({} chars)",
            failure.error.chars().count()
        );
        assert!(
            failure.error.ends_with('\u{2026}'),
            "the cut is marked with an ellipsis"
        );
    }

    /// A [`DurableWriter`] whose thread is parked inside a "gate" snapshot until
    /// [`release`](Self::release) fires.
    ///
    /// Queueing the gate first makes every later write back up in the queue
    /// deterministically — no sleeps — so a test can observe coalescing and the
    /// path cap before the writer drains. The gate file is `gate.json` in `dir`.
    /// `release` is declared before `writer` so an assertion panic drops the
    /// release sender first, which lets the writer's own `Drop` flush instead of
    /// timing out behind a gate nobody will open.
    struct GatedWriter {
        release: std::sync::mpsc::Sender<()>,
        writer: DurableWriter,
    }

    impl GatedWriter {
        fn new(dir: &TempDir) -> Self {
            let writer = DurableWriter::spawn();
            let (started_tx, started_rx) = std::sync::mpsc::channel();
            let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
            writer.write(dir.join("gate.json"), "gate", (), move |_| {
                let _ = started_tx.send(());
                let _ = release_rx.recv();
                Ok(b"gate".to_vec())
            });
            started_rx
                .recv_timeout(std::time::Duration::from_secs(30))
                .expect("the writer thread should be parked in the gate snapshot");
            Self {
                release: release_tx,
                writer,
            }
        }

        fn release(self) -> DurableWriter {
            self.release.send(()).expect("release the writer gate");
            self.writer
        }
    }

    /// Writes to different destinations stay in the order they were queued. The
    /// gate proves both are pending at once; the serializers record the order the
    /// FIFO runs them in.
    #[test]
    fn durable_writes_to_different_paths_keep_their_queued_order() {
        let dir = TempDir::new("util-durable-order");
        let first = dir.join("library.json");
        let second = dir.join("settings.json");
        let gated = GatedWriter::new(&dir);
        let (order_tx, order_rx) = std::sync::mpsc::channel();

        let library_tx = order_tx.clone();
        gated.writer.write(first.clone(), "library", (), move |_| {
            let _ = library_tx.send("library");
            Ok(b"library".to_vec())
        });
        gated
            .writer
            .write(second.clone(), "settings", (), move |_| {
                let _ = order_tx.send("settings");
                Ok(b"settings".to_vec())
            });

        let writer = gated.release();
        assert!(writer.flush(), "a healthy writer must drain its queue");
        let order: Vec<&str> = order_rx.try_iter().collect();
        assert_eq!(
            order,
            vec!["library", "settings"],
            "writes must run in queued order"
        );
        assert_eq!(std::fs::read(&first).expect("read"), b"library");
        assert_eq!(std::fs::read(&second).expect("read"), b"settings");
    }

    /// Writes separated by a flush keep per-path order: each flush makes its
    /// write observable before the next one is queued, so all three snapshots are
    /// serialized and the queue — not chance — decides their order.
    #[test]
    fn sequential_writes_to_one_path_are_written_in_order() {
        let dir = TempDir::new("util-durable-order-path");
        let path = dir.join("settings.json");
        let writer = DurableWriter::spawn();
        let (order_tx, order_rx) = std::sync::mpsc::channel();

        for value in [1u8, 2, 3] {
            let order_tx = order_tx.clone();
            writer.write(path.clone(), "settings", value, move |v: &u8| {
                order_tx.send(*v).expect("record serialization order");
                Ok(vec![*v])
            });
            assert!(writer.flush());
        }

        let order: Vec<u8> = order_rx.try_iter().collect();
        assert_eq!(
            order,
            vec![1, 2, 3],
            "sequential writes must run in queued order"
        );
        assert_eq!(
            std::fs::read(&path).expect("read"),
            [3],
            "the last queued write must be the one on disk"
        );
    }

    /// The barrier after a burst: `flush` returns true and the newest snapshot is
    /// on disk. This holds whether the writer kept up or coalesced the burst.
    #[test]
    fn a_burst_flush_returns_true_with_the_newest_snapshot() {
        let dir = TempDir::new("util-durable-burst");
        let path = dir.join("library.json");
        let writer = DurableWriter::spawn();

        for value in 0..128u32 {
            writer.write(path.clone(), "library", value, |v: &u32| {
                Ok(v.to_le_bytes().to_vec())
            });
        }

        assert!(writer.flush(), "the barrier must drain a coalesced burst");
        assert_eq!(
            std::fs::read(&path).expect("read"),
            127u32.to_le_bytes(),
            "the newest mutation of the burst must be on disk"
        );
    }

    /// A burst on one path coalesces: 100 queued mutations must not retain 100
    /// full-library snapshots. The gated writer guarantees the burst backs up;
    /// the counter proves the replaced serializers never ran. Removing the
    /// coalescing makes this test see 100 serializations and a 100-entry queue.
    #[test]
    fn a_burst_on_one_path_coalesces_to_the_newest_snapshot() {
        let dir = TempDir::new("util-durable-coalesce");
        let path = dir.join("library.json");
        let gated = GatedWriter::new(&dir);
        let serialized = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));

        for value in 0..100u32 {
            let serialized = std::sync::Arc::clone(&serialized);
            gated
                .writer
                .write(path.clone(), "library", value, move |v: &u32| {
                    serialized.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    Ok(v.to_le_bytes().to_vec())
                });
        }

        let (pending, queued) = gated.writer.queue_depths();
        assert_eq!(pending, 1, "one pending snapshot per destination path");
        assert!(
            queued <= 2,
            "the queue depth must not grow with the mutation count: {queued} entries"
        );

        let writer = gated.release();
        assert!(writer.flush(), "the coalesced queue must still drain");
        assert_eq!(
            serialized.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "only the newest snapshot may be serialized; replaced closures must not run"
        );
        assert_eq!(
            std::fs::read(&path).expect("read"),
            99u32.to_le_bytes(),
            "the newest snapshot must be the one on disk"
        );
    }

    /// A newer snapshot for a path whose older snapshot is already being
    /// serialized must be queued, not mistaken for "already handled": the older
    /// bytes are published first, then the replacement wins.
    #[test]
    fn a_newer_write_for_a_path_already_being_written_is_not_lost() {
        let dir = TempDir::new("util-durable-in-flight");
        let path = dir.join("library.json");
        let writer = DurableWriter::spawn();

        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (gate_tx, gate_rx) = std::sync::mpsc::channel::<()>();
        writer.write(path.clone(), "library", 1u8, move |v: &u8| {
            let _ = started_tx.send(());
            let _ = gate_rx.recv();
            Ok(vec![*v])
        });
        started_rx
            .recv_timeout(std::time::Duration::from_secs(30))
            .expect("the first library write should be running");

        writer.write(path.clone(), "library", 2u8, |v: &u8| Ok(vec![*v]));
        assert_eq!(
            writer.queue_depths().0,
            1,
            "the replacement is pending while the older snapshot runs"
        );

        gate_tx.send(()).expect("release the in-flight snapshot");
        assert!(writer.flush());
        assert_eq!(
            std::fs::read(&path).expect("read"),
            [2],
            "the newest write must not be lost behind the older in-flight one"
        );
    }

    /// The path cap: `MAX_PENDING_PATHS` distinct files may wait, the next new
    /// path is saved inline on the caller's thread (never dropped, never queued
    /// without bound), and a newer snapshot for the path the writer is currently
    /// publishing is queued (one over the soft cap) rather than raced inline.
    #[test]
    fn a_queue_at_the_path_cap_saves_overflow_inline() {
        let dir = TempDir::new("util-durable-overflow");
        let gated = GatedWriter::new(&dir);
        let gate_path = dir.join("gate.json");

        for i in 0..MAX_PENDING_PATHS {
            gated.writer.write(
                dir.path().join(format!("path-{i}.json")),
                "library",
                i,
                |v: &usize| Ok(v.to_le_bytes().to_vec()),
            );
        }
        let (pending, queued) = gated.writer.queue_depths();
        assert_eq!(
            pending, MAX_PENDING_PATHS,
            "the cap allows one snapshot per distinct path"
        );
        assert_eq!(queued, MAX_PENDING_PATHS);

        // `gate.json` is the path the writer is currently publishing: its newer
        // snapshot must queue even past the cap, because running it inline while
        // the older bytes are mid-publish could leave the older bytes on disk.
        gated
            .writer
            .write(gate_path.clone(), "gate", (), |_| Ok(b"gate-2".to_vec()));
        assert_eq!(
            gated.writer.queue_depths().0,
            MAX_PENDING_PATHS + 1,
            "an in-flight path's replacement is queued, not raced inline"
        );

        // A brand-new path past the cap has no older snapshot in flight, so the
        // only non-dropping option left is to write it on this thread now.
        let overflow_path = dir.join("overflow.json");
        let caller = std::thread::current().id();
        let (ran_tx, ran_rx) = std::sync::mpsc::channel();
        gated
            .writer
            .write(overflow_path.clone(), "library", (), move |_| {
                let _ = ran_tx.send(std::thread::current().id());
                Ok(b"inline".to_vec())
            });
        let ran_on = ran_rx
            .recv_timeout(std::time::Duration::from_secs(30))
            .expect("an overflow save must run immediately on the caller");
        assert_eq!(
            ran_on, caller,
            "the overflow policy is inline, not queue-and-wait or drop"
        );
        assert_eq!(std::fs::read(&overflow_path).expect("read"), b"inline");
        assert_eq!(
            gated.writer.queue_depths().0,
            MAX_PENDING_PATHS + 1,
            "the inline save must not grow the pending map"
        );
        assert!(
            gated.writer.last_failure().is_none(),
            "a successful inline save is not a save failure"
        );

        let writer = gated.release();
        assert!(writer.flush(), "the capped queue must drain");
        assert_eq!(
            std::fs::read(&gate_path).expect("read"),
            b"gate-2",
            "the newest snapshot for the in-flight path must win"
        );
        for i in 0..MAX_PENDING_PATHS {
            assert_eq!(
                std::fs::read(dir.path().join(format!("path-{i}.json"))).expect("read"),
                i.to_le_bytes(),
                "every path queued before the cap must still be written"
            );
        }
    }

    /// Same rule as the persister: a big `Library` snapshot must be serialized
    /// on the writer thread, not walked on the command/UI thread that queued it.
    #[test]
    fn the_durable_writer_serializes_off_the_callers_thread() {
        let dir = TempDir::new("util-durable-thread");
        let path = dir.join("library.json");
        let writer = DurableWriter::spawn();

        let caller = std::thread::current().id();
        let (ran_tx, ran_rx) = std::sync::mpsc::channel();
        writer.write(path.clone(), "library", (), move |_| {
            let _ = ran_tx.send(std::thread::current().id());
            Ok(b"payload".to_vec())
        });

        let serialized_on = ran_rx
            .recv_timeout(std::time::Duration::from_secs(30))
            .expect("the durable writer should have serialized the payload");
        assert_ne!(
            serialized_on, caller,
            "serialization must happen on the writer's own thread, not the caller's"
        );

        assert!(writer.flush());
        assert_eq!(std::fs::read(&path).expect("read"), b"payload");
    }

    /// `flush` must be a *barrier*, not a roll of the dice: while the writer
    /// thread is stuck inside an earlier job, a queued flush cannot report
    /// success. The gate makes that deterministic — the flush only completes
    /// after the blocked write is released.
    #[test]
    fn flush_waits_for_a_write_that_is_still_in_flight() {
        let dir = TempDir::new("util-durable-barrier");
        let path = dir.join("library.json");
        let writer = DurableWriter::spawn();

        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (gate_tx, gate_rx) = std::sync::mpsc::channel::<()>();
        writer.write(path.clone(), "library", (), move |_| {
            let _ = started_tx.send(());
            // Hold the writer thread inside the first job until the test says so.
            let _ = gate_rx.recv();
            Ok(b"written".to_vec())
        });
        // The serializer started, so every later job is behind it in the queue.
        started_rx
            .recv_timeout(std::time::Duration::from_secs(30))
            .expect("the first write should have started");

        let (done_tx, done_rx) = std::sync::mpsc::channel();
        std::thread::scope(|scope| {
            scope.spawn(|| {
                let _ = done_tx.send(writer.flush());
            });
            // Still blocked on the gate: release it and only then can the flush
            // behind the write finish.
            gate_tx.send(()).expect("release the blocked write");
            assert!(
                done_rx
                    .recv_timeout(std::time::Duration::from_secs(30))
                    .expect("the flush should complete once the write finishes"),
                "flush must report a drained queue"
            );
        });

        assert_eq!(std::fs::read(&path).expect("read"), b"written");
    }

    /// The shutdown safety net: dropping the last writer handle drains what is
    /// still queued, so a store dropped without an explicit flush cannot lose
    /// its final write to a process exit.
    #[test]
    fn dropping_the_writer_drains_the_queue() {
        let dir = TempDir::new("util-durable-drop");
        let path = dir.join("settings.json");
        {
            let writer = DurableWriter::spawn();
            writer.write(
                path.clone(),
                "settings",
                b"queued".to_vec(),
                |v: &Vec<u8>| Ok(v.clone()),
            );
        } // Drop must flush before returning.

        assert_eq!(std::fs::read(&path).expect("read"), b"queued");
    }

    /// The degraded mode (thread spawn failed) must not fall through the queue
    /// logic: writes run synchronously on the caller and `flush` reports success
    /// with nothing left to wait for.
    #[test]
    fn the_inline_fallback_writes_synchronously_and_flushes_true() {
        let dir = TempDir::new("util-durable-inline");
        let path = dir.join("library.json");
        let writer = DurableWriter::spawn_inline();

        writer.write(
            path.clone(),
            "library",
            b"inline".to_vec(),
            |v: &Vec<u8>| Ok(v.clone()),
        );
        // No background thread: the file must already be there before flush.
        assert_eq!(
            std::fs::read(&path).expect("inline write must be synchronous"),
            b"inline"
        );
        assert!(writer.flush(), "inline mode has nothing to wait for");

        writer.write(
            path.clone(),
            "library",
            b"inline again".to_vec(),
            |v: &Vec<u8>| Ok(v.clone()),
        );
        assert_eq!(std::fs::read(&path).expect("read"), b"inline again");
    }

    /// A panicking serializer is a bug, but it must not take the rest of the
    /// queue with it: the writer catches it, logs, and keeps draining. Without
    /// the catch, the write queued behind this one would be silently lost and
    /// the shutdown barrier would time out.
    #[test]
    fn a_panicking_job_does_not_kill_the_writer_thread() {
        let dir = TempDir::new("util-durable-panic");
        let bad = dir.join("bad.json");
        let good = dir.join("good.json");
        let writer = DurableWriter::spawn();

        writer.write(bad, "bad", (), |_| -> Result<Vec<u8>, String> {
            panic!("serializer exploded");
        });
        writer.write(
            good.clone(),
            "good",
            b"after-panic".to_vec(),
            |v: &Vec<u8>| Ok(v.clone()),
        );

        assert!(
            writer.flush(),
            "the writer thread must survive a panicking job"
        );
        assert_eq!(
            std::fs::read(&good).expect("the job behind the panic must still run"),
            b"after-panic"
        );
        let failure = writer
            .last_failure()
            .expect("a panicked job is a lost save and must be recorded");
        assert_eq!(failure.what, "bad");
        assert_eq!(failure.count, 1);
        assert!(
            failure.error.contains("panicked"),
            "the recorded reason names the panic: {:?}",
            failure.error
        );
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
