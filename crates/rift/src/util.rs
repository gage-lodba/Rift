//! Small shared helpers.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};
use std::sync::{Mutex, MutexGuard};

use tracing::{debug, warn};

/// Read and parse a JSON state file, defaulting when missing. On a parse
/// failure the corrupt file is renamed aside (`*.json.bad`, with a numeric
/// suffix when that backup already exists, so an earlier corrupt copy is never
/// overwritten) so a later save can't silently overwrite — and permanently
/// destroy — the user's data.
pub(crate) fn load_json<T: serde::de::DeserializeOwned + Default>(path: &Path) -> T {
    match std::fs::read(path) {
        // Reading bytes rather than a `String` is what makes a file that is not
        // valid UTF-8 take the corrupt-file path: `read_to_string` fails on it,
        // and the old code treated every read failure as "no file", so a
        // corrupt library was silently replaced by an empty one on the next
        // save instead of being preserved. JSON is UTF-8 by definition, so any
        // such file *is* the corrupt one this function exists to quarantine.
        Ok(bytes) => match serde_json::from_slice::<T>(&bytes) {
            Ok(v) => v,
            Err(e) => {
                warn!("could not parse {}: {e}", path.display());
                quarantine_corrupt(path);
                T::default()
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => T::default(),
        Err(e) => {
            // A read failure is not "no file": the state may still be there but
            // temporarily unreadable (a sharing violation from a backup/AV tool,
            // a permission problem). Say so loudly instead of silently starting
            // from defaults — the caller's next save would then overwrite data
            // this load never managed to read.
            warn!(
                "could not read {} ({e}); starting from defaults — check the file is readable \
                 before changing anything, because a save will overwrite it",
                path.display()
            );
            T::default()
        }
    }
}

/// Move a corrupt state file aside, never overwriting an earlier backup.
fn quarantine_corrupt(path: &Path) {
    let backup = free_backup_path(&path.with_extension("json.bad"));
    match std::fs::rename(path, &backup) {
        Ok(()) => warn!("moved the corrupt file to {}", backup.display()),
        Err(e) => warn!("could not preserve the corrupt file: {e}"),
    }
}

/// The first free `*.json.bad` name: the historical single backup first, then
/// `.1`, `.2`, … so a second corruption cannot destroy the copy an earlier
/// repair (or the user) was relying on.
fn free_backup_path(base: &Path) -> PathBuf {
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
/// durable user data, which uses [`DurableWriter`]: ordered the same way, but
/// with a flush barrier and a shutdown drain instead of a dropped final write.
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
/// order, so a healthy writer drains in milliseconds. The ceiling exists so a
/// wedged disk (or a stopped writer thread) cannot hang application shutdown
/// forever. Reaching it is logged and reported to the caller — never silent.
const FLUSH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Serializes **durable** state writes (`library.json`, `settings.json`) onto a
/// single background thread.
///
/// This is [`Persister`]'s durable sibling, and the differences are the reason
/// it exists as a separate type:
///
/// - **Ordered, per path and across paths**: every job goes through one FIFO
///   queue and one thread performs each [`atomic_write`] in that order, so two
///   rapid saves to one file can neither race nor land out of order, and the
///   last queued mutation wins.
/// - **The caller does no I/O**: a command thread only moves a snapshot value
///   and a path into the queue; serialization happens on the writer thread and
///   the disk is touched there. A UI-thread command therefore cannot block on a
///   slow or contended disk.
/// - **Durable at a known point**: [`flush`](Self::flush) is a hard barrier —
///   when it returns `true`, every write queued before the call has been
///   attempted. [`Drop`] performs a final bounded drain too, so a normal
///   shutdown does not drop the last write the way [`Persister`] can. These are
///   what the tests use instead of sleeping, and what
///   [`RiftBackend::shutdown`](crate::RiftBackend::shutdown) relies on.
///
/// Deliberate limits, matching the synchronous path this replaces:
///
/// - a write that fails is logged (`failed to save {what}: {e}`) and **not
///   retried** — the disk refusing a write cannot be fixed by waiting, and
///   [`atomic_write`] has already guaranteed no half-written file is left;
/// - serialization failures are logged likewise (`failed to serialize
///   {what}: {e}`) on the writer thread and that job is dropped;
/// - a serializer that *panics* is caught on the writer thread, logged, and only
///   that job is dropped: the writer keeps draining the jobs queued behind it
///   (a panic here is a bug, not a reason to lose the rest of the queue);
/// - a hard crash (power loss, kill) before the queue drains can still lose the
///   last write. A normal shutdown flushes; a crash is a crash;
/// - the high-frequency, loss-tolerant playback snapshot stays on [`Persister`],
///   which additionally drops unchanged payloads.
///
/// A thread-spawn failure degrades rather than failing startup: writes then run
/// synchronously on the caller's thread, which is slower but never silently lost.
pub(crate) struct DurableWriter {
    /// `None` when the writer thread could not be spawned: writes run inline.
    tx: Option<Sender<DurableJob>>,
}

enum DurableJob {
    Write {
        path: PathBuf,
        what: &'static str,
        serialize: Box<dyn FnOnce() -> Result<Vec<u8>, String> + Send>,
    },
    /// Drain the queue and acknowledge: every job queued before this one has
    /// been attempted by the time the ack is sent.
    Flush(Sender<()>),
}

impl DurableWriter {
    /// Start the durable writer thread.
    pub(crate) fn spawn() -> Self {
        let (tx, rx) = mpsc::channel::<DurableJob>();
        match std::thread::Builder::new()
            .name("rift-durable".into())
            .spawn(move || run_durable_writer(rx))
        {
            Ok(_) => Self { tx: Some(tx) },
            Err(e) => {
                warn!(
                    "could not spawn the durable writer thread; library and settings saves \
                     will run on the caller's thread: {e}"
                );
                Self { tx: None }
            }
        }
    }

    /// Queue `value` to be serialized and atomically written to `path`.
    ///
    /// Returns as soon as the job is queued; the caller's thread never touches
    /// the disk unless the writer thread is unavailable (spawn failed or it
    /// stopped), in which case the write runs inline rather than being dropped.
    /// `what` names the file in log messages, exactly as the old synchronous
    /// `save_json` helper did.
    pub(crate) fn write<T, F>(&self, path: PathBuf, what: &'static str, value: T, serialize: F)
    where
        T: Send + 'static,
        F: FnOnce(&T) -> Result<Vec<u8>, String> + Send + 'static,
    {
        let job = DurableJob::Write {
            path,
            what,
            serialize: Box::new(move || serialize(&value)),
        };
        match &self.tx {
            Some(tx) => {
                if let Err(mpsc::SendError(job)) = tx.send(job) {
                    // The writer thread stopped (a panicking job no longer stops
                    // it, but it can still be gone). Durability beats latency
                    // here: do the write now rather than drop the user's change.
                    warn!("durable writer thread stopped; saving {what} on the caller's thread");
                    run_durable_job_catching(job);
                }
            }
            None => run_durable_job_catching(job),
        }
    }

    /// A writer with no background thread: every [`write`](Self::write) runs
    /// synchronously on the calling thread.
    ///
    /// This is the degraded mode [`spawn`](Self::spawn) falls back to when the
    /// thread cannot be created; the constructor exists so tests can pin that
    /// path without having to make thread creation fail.
    #[cfg(test)]
    pub(crate) fn spawn_inline() -> Self {
        Self { tx: None }
    }

    /// Block until every write queued before this call has been attempted.
    ///
    /// Returns `true` when the queue was drained, `false` when the writer thread
    /// is gone or did not drain within [`FLUSH_TIMEOUT`] (both are logged). A
    /// `true` does not promise every write *succeeded*: a failed write is logged
    /// on the writer thread, exactly as the old synchronous `save_json` warned,
    /// because an unwritable disk cannot be waited into working order.
    pub(crate) fn flush(&self) -> bool {
        let Some(tx) = &self.tx else {
            // Inline fallback: nothing is ever queued, every write already ran.
            return true;
        };
        let (ack_tx, ack_rx) = mpsc::channel();
        if tx.send(DurableJob::Flush(ack_tx)).is_err() {
            warn!("durable writer thread is gone; the last writes may not have reached disk");
            return false;
        }
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
    }
}

/// The writer thread: one FIFO queue, so the order jobs are queued in is exactly
/// the order their files are written in.
fn run_durable_writer(rx: mpsc::Receiver<DurableJob>) {
    while let Ok(job) = rx.recv() {
        match job {
            // The ack promises only what was queued before this job; a caller
            // that queues another write concurrently may still be in flight
            // after the flush returns, which is exactly what `flush` documents.
            DurableJob::Flush(ack) => {
                let _ = ack.send(());
            }
            job @ DurableJob::Write { .. } => run_durable_job_catching(job),
        }
    }
}

/// Serialize and atomically publish one job. Runs on the writer thread, or on
/// the caller's thread only when the writer is unavailable (degraded mode).
fn run_durable_job(job: DurableJob) {
    match job {
        DurableJob::Write {
            path,
            what,
            serialize,
        } => match serialize() {
            Ok(bytes) => {
                if let Err(e) = atomic_write(&path, &bytes) {
                    warn!("failed to save {what}: {e}");
                }
            }
            Err(e) => warn!("failed to serialize {what}: {e}"),
        },
        // Only `flush` queues this; an inline fallback only sees `Write`. The
        // ack is cheap to send and keeps the two paths identical.
        DurableJob::Flush(ack) => {
            let _ = ack.send(());
        }
    }
}

/// Run one job, catching a panic from its serializer.
///
/// Without this, a panicking serializer kills the writer thread and with it
/// every job already queued behind it: none of those would be written and
/// nothing would say so. Catching keeps the fatal-panic blast radius to the one
/// job (the same semantic as a serialization `Err`) and, because shutdown
/// waits on this thread's flush barrier, it also stops a bad job from turning a
/// quit into a 10-second flush timeout. `AssertUnwindSafe` is sound here: the
/// job owns its payload, so a panic mid-job cannot leave shared state behind.
fn run_durable_job_catching(job: DurableJob) {
    let what = match &job {
        DurableJob::Write { what, .. } => *what,
        DurableJob::Flush(_) => "flush",
    };
    let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || run_durable_job(job)));
    if ran.is_err() {
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

    /// Two rapid mutations must land in the order they were made, so the *last*
    /// one is what the next launch reads. The serializer records its own run
    /// order, which is proof the queue (not chance) decides the order.
    #[test]
    fn durable_writes_keep_their_queued_order_and_the_last_one_wins() {
        let dir = TempDir::new("util-durable-order");
        let path = dir.join("settings.json");
        let writer = DurableWriter::spawn();
        let (order_tx, order_rx) = std::sync::mpsc::channel();

        for value in [1u8, 2, 3] {
            let order_tx = order_tx.clone();
            writer.write(path.clone(), "settings", value, move |v: &u8| {
                order_tx.send(*v).expect("record serialization order");
                Ok(vec![*v])
            });
        }
        assert!(writer.flush());

        let order: Vec<u8> = order_rx.try_iter().collect();
        assert_eq!(order, vec![1, 2, 3], "writes must run in queued order");
        assert_eq!(
            std::fs::read(&path).expect("read"),
            [3],
            "the last queued write must be the one on disk"
        );
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
