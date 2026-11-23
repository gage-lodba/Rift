//! Offline download tracking. Audio files live as `<safe_stem>.m4a` under the
//! downloads directory; the on-disk presence of a file is the source of
//! truth for "downloaded". A separate set tracks in-flight downloads so the
//! UI can show progress.
//!
//! Two name shapes are readable: the current injective encoding
//! ([`crate::fetch::safe_stem`]) and the pre-P5 `_`-folded names
//! ([`crate::fetch::legacy_safe_stem`]), which are never renamed or deleted at
//! load. `downloads/.names.json` (written atomically by a dedicated writer
//! thread; `remove` queues the update instead of fsyncing on the UI thread)
//! records `stem -> id` for names that are not self-identifying: a capped stem
//! (undecodable) and a stem containing `_` (which another id's pre-P5 fold could
//! also produce).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use rift_types::DownloadState;
use tracing::{info, warn};

use crate::util::LockExt;

/// Sidecar in the downloads directory mapping a name whose id is not
/// self-describing (an overlong id capped to a prefix + hash, or a stem with
/// `_` that a pre-P5 fold could also produce) back to that id. Kept next to the
/// audio so it moves with the directory, and written through `util::atomic_write`
/// so a crash cannot leave a half-written index.
const NAMES_FILE: &str = ".names.json";

/// Serialized shape of [`NAMES_FILE`]. A struct (rather than a bare map) leaves
/// room to add fields without breaking an older index.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct NameIndex {
    /// `stem -> id` for names that are not self-identifying.
    #[serde(default)]
    stems: BTreeMap<String, String>,
}

/// Read [`NAMES_FILE`], or an empty map when it is absent.
///
/// A parse/read failure is logged and treated as absent: the index is derived
/// metadata, so this is not allowed to fail a startup or touch the audio files.
/// The unreadable file is left on disk untouched by this load (no quarantine
/// write, no delete); it is only replaced if a capped name is published later.
/// Even then the audio files are preserved, they just stay unrecognised until
/// the id is downloaded again.
fn load_names(dir: &Path) -> BTreeMap<String, String> {
    let path = dir.join(NAMES_FILE);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return BTreeMap::new(),
        Err(e) => {
            warn!("could not read {}: {e}", path.display());
            return BTreeMap::new();
        }
    };
    match serde_json::from_slice::<NameIndex>(&bytes) {
        Ok(index) => index.stems,
        Err(e) => {
            warn!(
                "ignoring the unreadable download name index {}: {e}",
                path.display()
            );
            BTreeMap::new()
        }
    }
}

/// Publish [`NAMES_FILE`] atomically (temp file in the downloads directory,
/// flush, rename). BTreeMap ordering keeps the file deterministic across runs.
/// Returns `false` only when the index could not be written, so the caller can
/// avoid publishing a capped file whose id would not survive a restart.
fn save_names(dir: &Path, stems: &BTreeMap<String, String>) -> bool {
    let path = dir.join(NAMES_FILE);
    let index = NameIndex {
        stems: stems.clone(),
    };
    let Ok(json) = serde_json::to_vec_pretty(&index) else {
        warn!("could not serialize {}", path.display());
        return false;
    };
    if let Err(e) = crate::util::atomic_write(&path, &json) {
        warn!("could not write {}: {e}", path.display());
        return false;
    }
    true
}

/// Whether the name index says `stem` belongs to a different id.
///
/// `is_downloaded` refuses such a `_`-folded legacy file for another id; playback
/// must apply the same rule before serving the file, or the UI would say "not
/// downloaded" while `fetch_track` played the owner's audio. A missing/absent
/// index entry means an unowned pre-P5 file and keeps the old fallback.
pub(crate) fn legacy_name_owned_by_other(dir: &Path, stem: &str, id: &str) -> bool {
    load_names(dir).get(stem).is_some_and(|owner| owner != id)
}

/// Serializes `.names.json` writes off the caller's thread.
///
/// `remove` is called from the UI thread and must not fsync an index inline.
/// Every submitted snapshot is written in submission order (the callers submit
/// while holding `Downloads::names`, so the order matches the map mutations);
/// `Flush` is a barrier used by tests/shutdown. `record_name` waits for its
/// snapshot's acknowledgement because it must be durable before the caller
/// renames the `.part` file into place.
struct NamesWriter {
    dir: PathBuf,
    tx: Option<std::sync::mpsc::Sender<NamesWrite>>,
    handle: Option<std::thread::JoinHandle<()>>,
}

enum NamesWrite {
    Snapshot {
        stems: BTreeMap<String, String>,
        ack: Option<std::sync::mpsc::Sender<bool>>,
    },
    Flush(std::sync::mpsc::Sender<()>),
    Shutdown,
}

impl NamesWriter {
    fn spawn(dir: PathBuf) -> Self {
        let (tx, rx) = std::sync::mpsc::channel::<NamesWrite>();
        let thread_dir = dir.clone();
        match std::thread::Builder::new()
            .name("rift-download-names".into())
            .spawn(move || {
                while let Ok(message) = rx.recv() {
                    match message {
                        NamesWrite::Snapshot { stems, ack } => {
                            let saved = save_names(&thread_dir, &stems);
                            if let Some(ack) = ack {
                                let _ = ack.send(saved);
                            }
                        }
                        NamesWrite::Flush(ack) => {
                            let _ = ack.send(());
                        }
                        NamesWrite::Shutdown => break,
                    }
                }
            }) {
            Ok(handle) => Self {
                dir,
                tx: Some(tx),
                handle: Some(handle),
            },
            Err(e) => {
                warn!(
                    "could not spawn the download-name writer; index writes will run on the \
                     caller's thread: {e}"
                );
                Self {
                    dir,
                    tx: None,
                    handle: None,
                }
            }
        }
    }

    /// Submit a snapshot. When a writer thread exists this only enqueues the
    /// work; a failed send (the thread stopped) falls back to an inline write so
    /// the index change is never dropped.
    fn submit(&self, stems: BTreeMap<String, String>, ack: Option<std::sync::mpsc::Sender<bool>>) {
        if let Some(tx) = &self.tx {
            if tx
                .send(NamesWrite::Snapshot {
                    stems: stems.clone(),
                    ack: ack.clone(),
                })
                .is_ok()
            {
                return;
            }
            warn!("download-name writer stopped; saving the index on the caller's thread");
        }
        let saved = save_names(&self.dir, &stems);
        if let Some(ack) = ack {
            let _ = ack.send(saved);
        }
    }

    /// Wait until every snapshot submitted before this call has been attempted.
    fn flush(&self) {
        let Some(tx) = &self.tx else {
            return;
        };
        let (ack_tx, ack_rx) = std::sync::mpsc::channel();
        if tx.send(NamesWrite::Flush(ack_tx)).is_ok() {
            let _ = ack_rx.recv();
        }
    }
}

impl Drop for NamesWriter {
    fn drop(&mut self) {
        if let Some(tx) = self.tx.take() {
            let _ = tx.send(NamesWrite::Shutdown);
        }
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// Live download-claim identities.
///
/// A claim token is captured by the worker when [`Downloads::begin`] succeeds.
/// `remove` drops the id's token, so a worker from the removed attempt can no
/// longer update progress, mark the id finished/failed, or publish: a retry
/// that claims the id gets a *new* token. Without this, a removed worker that
/// was already transferring could interleave with the retry's transfer (they
/// used the same `.part` path) and publish a truncated file.
#[derive(Debug, Default)]
struct Claims {
    /// Monotonic token source.
    next: u64,
    /// The token of the attempt currently allowed to mutate each id.
    current: HashMap<String, u64>,
}

pub struct Downloads {
    pub dir: PathBuf,
    downloaded: Mutex<HashSet<String>>,
    /// Every non-empty `.m4a` stem found at load, keyed by its raw on-disk name.
    /// This is what makes a pre-P5 `_`-folded file (`a_b.m4a` for id `a/b`)
    /// recognizable without guessing an id: [`is_downloaded`](Self::is_downloaded)
    /// consults `legacy_safe_stem(id)` against this set. It is only a lookup set
    /// — new files are always written under [`path`](Self::path). Sourced once in
    /// [`load`](Self::load), so a query does no disk I/O.
    on_disk_stems: Mutex<HashSet<String>>,
    /// The in-memory copy of [`NAMES_FILE`]: capped `stem -> id` mappings.
    names: Mutex<BTreeMap<String, String>>,
    downloading: Mutex<HashSet<String>>,
    /// Byte progress of each in-flight download, in permille (`0..=1000`). Only
    /// ids currently in `downloading` may have an entry; every lifecycle
    /// transition clears it so a stale bar can never outlive a claim. Absent =
    /// unknown (a queued track, or the transfer has not reported yet) and the UI
    /// shows its indeterminate spinner.
    progress: Mutex<HashMap<String, u16>>,
    /// Tracks whose download was abandoned after repeated failures. Purely a
    /// session-scoped UI hint (rows show a retry affordance); cleared the moment
    /// a track is re-attempted, finished, or removed.
    failed: Mutex<HashSet<String>>,
    /// Tombstones for ids the user removed while a download was in flight (or
    /// immediately after one completed). A worker that was already running
    /// `stream::fetch_to_path` cannot be interrupted, so it may still call
    /// `set_progress`, `finish`, `fail` or `mark_failed` for this id after
    /// `remove` returned: those calls must not resurrect the row or re-publish
    /// the file, and the worker re-checks this set immediately before renaming
    /// its completed `.part` into place. Cleared by [`begin`](Self::begin), so a
    /// retry of a removed track is allowed.
    canceled: Mutex<HashSet<String>>,
    /// Per-id claim tokens, so a worker abandoned by `remove` cannot mutate the
    /// state of a newer retry (see [`Claims`]). Locked after `canceled` and
    /// before `names` in the store's fixed order.
    claims: Mutex<Claims>,
    /// Serializes `NAMES_FILE` writes off the UI thread; see [`NamesWriter`].
    names_writer: NamesWriter,
}

/// Map a byte count to the permille fraction the UI contract uses
/// (`0..=1000`), clamped so an over-reporting transfer still shows 100%.
///
/// `None` when `total` is 0 — the length is not known, so there is no fraction
/// to report and the track stays indeterminate. Integer permille (not an `f32`)
/// keeps `rift_types::DownloadState: Eq`.
pub(crate) fn progress_permille(received: u64, total: u64) -> Option<u16> {
    if total == 0 {
        return None;
    }
    // `u128` because the product overflows `u64` for multi-gigabyte streams.
    let permille = u128::from(received) * 1000 / u128::from(total);
    Some(permille.min(1000) as u16)
}

impl Downloads {
    pub fn load(dir: PathBuf) -> Self {
        if let Err(e) = std::fs::create_dir_all(&dir) {
            warn!("could not create downloads dir: {e}");
        }
        let mut names = load_names(&dir);
        let mut downloaded = HashSet::new();
        let mut on_disk_stems = HashSet::new();
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                match path.extension().and_then(|e| e.to_str()) {
                    Some("m4a") => {
                        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                            // A non-UTF-8 name cannot map to an id. Leave it
                            // alone: it is user data we simply cannot recognise.
                            continue;
                        };
                        // A zero-byte `.m4a` cannot be audio: it is the
                        // residue of an old aborted write. Marking it
                        // "downloaded" would make the UI offer an offline
                        // copy that `fetch_track` (correctly) refuses to
                        // play, and would hide it from Download's repair
                        // path. Clean it up instead.
                        let empty = entry.metadata().is_ok_and(|meta| meta.len() == 0);
                        if empty {
                            warn!("removing empty offline file {}", path.display());
                            let _ = std::fs::remove_file(&path);
                            continue;
                        }
                        on_disk_stems.insert(stem.to_owned());
                        // The name index is authoritative for a stem whose id
                        // is not self-describing: a capped name is undecodable,
                        // and a `_` stem could otherwise be claimed by every id
                        // that folds to it.
                        if let Some(id) = names.get(stem) {
                            if crate::fetch::safe_stem(id) == stem {
                                downloaded.insert(id.clone());
                                continue;
                            }
                            warn!(
                                "download name index entry for {} does not match its stem \
                                 (expected {:?}); ignoring it",
                                path.display(),
                                crate::fetch::safe_stem(id)
                            );
                        }
                        if let Some(id) = crate::fetch::stem_id(stem) {
                            downloaded.insert(id);
                        } else if crate::fetch::is_legacy_stem(stem) && !stem.contains('!') {
                            // A pre-P5 name: its id is not recoverable, so record
                            // the stem itself as the id — exactly what the old
                            // load did, which keeps existing UI rows marked
                            // downloaded. `on_disk_stems` additionally makes
                            // `is_downloaded(<the real id>)` true through the
                            // `legacy_safe_stem` fallback.
                            downloaded.insert(stem.to_owned());
                        } else {
                            warn!(
                                "offline file {} cannot be mapped back to an id; keeping it",
                                path.display()
                            );
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
        // Drop index entries whose file disappeared (removed outside Rift, or a
        // zero-byte file just cleaned up) so a stale entry cannot claim a track
        // is downloaded. A corrupt/unreadable index is left in place untouched:
        // the audio files are never deleted, they just stay unrecognised.
        let before = names.len();
        names.retain(|stem, id| {
            on_disk_stems.contains(stem) && crate::fetch::safe_stem(id) == *stem
        });
        if names.len() != before {
            info!(
                "pruned {} stale download name index entr{}",
                before - names.len(),
                if before - names.len() == 1 {
                    "y"
                } else {
                    "ies"
                }
            );
            save_names(&dir, &names);
        }
        info!(
            "{} tracks available offline at {}",
            downloaded.len(),
            dir.display()
        );
        let names_writer = NamesWriter::spawn(dir.clone());
        Self {
            dir,
            downloaded: Mutex::new(downloaded),
            on_disk_stems: Mutex::new(on_disk_stems),
            names: Mutex::new(names),
            downloading: Mutex::new(HashSet::new()),
            progress: Mutex::new(HashMap::new()),
            failed: Mutex::new(HashSet::new()),
            canceled: Mutex::new(HashSet::new()),
            claims: Mutex::new(Claims::default()),
            names_writer,
        }
    }

    pub fn path(&self, id: &str) -> PathBuf {
        self.dir
            .join(format!("{}.m4a", crate::fetch::safe_stem(id)))
    }

    /// The pre-P5 name for `id`, when the naming fix changed it. `None` when the
    /// current and old names coincide (every plain lowercase id), so the caller
    /// does not inspect the same path twice.
    pub(crate) fn legacy_path(&self, id: &str) -> Option<PathBuf> {
        let legacy = crate::fetch::legacy_safe_stem(id);
        (legacy != crate::fetch::safe_stem(id)).then(|| self.dir.join(format!("{legacy}.m4a")))
    }

    /// Temporary path a transfer writes before publication.
    ///
    /// The worker creates this `.part` file and renames it onto
    /// [`path`](Self::path) on success; [`remove`](Self::remove) deletes it too,
    /// so a remove during a transfer cannot leave scratch bytes behind.
    pub fn part_path(&self, id: &str) -> PathBuf {
        self.path(id).with_extension("m4a.part")
    }

    /// Whether the pre-P5 name for `id` is owned by a *different* id in the name
    /// index (a `_` stem published after the naming fix).
    ///
    /// The publish guard uses this to avoid refusing a download merely because
    /// another id's owned file happens to share this id's legacy fold: that file
    /// belongs to the other track, not this one.
    pub(crate) fn legacy_owned_elsewhere(&self, id: &str) -> bool {
        let legacy = crate::fetch::legacy_safe_stem(id);
        self.names
            .lock_safe()
            .get(&legacy)
            .is_some_and(|owner| owner != id)
    }

    /// The per-attempt temporary path a transfer writes before publication.
    ///
    /// Each claim gets its own token (`begin`), and its `.part` name carries it,
    /// so a worker abandoned by `remove` can only ever rename *its own* fully
    /// downloaded file. If it shared the retry's `.part` path, it could publish
    /// the retry's mid-write bytes as a complete download.
    pub fn part_path_for(&self, id: &str, token: u64) -> PathBuf {
        self.path(id).with_extension(format!("{token}.m4a.part"))
    }

    /// Whether `id` has a usable offline copy.
    ///
    /// Looks at the loaded set first (current names and the pre-P5 stems the old
    /// load registered), then falls back to comparing
    /// [`legacy_safe_stem`](crate::fetch::legacy_safe_stem) against the stems
    /// found at load. That fallback is the only way an old `_`-folded file
    /// (`a_b.m4a` for id `a/b`) can become recognizable again: the folding is not
    /// reversible, so the id is looked up against the set instead of decoded
    /// from the name. A stem whose owner is known from the name index (a file
    /// published under `a_b` for the id `a_b` after the fix) belongs only to
    /// that owner, so a lookup for `a/b` must not claim it; a pre-P5 file has no
    /// owner and keeps the old fallback. Both reads are in-memory;
    /// [`load`](Self::load) is the only place that stats the directory.
    pub fn is_downloaded(&self, id: &str) -> bool {
        if self.downloaded.lock_safe().contains(id) {
            return true;
        }
        let legacy = crate::fetch::legacy_safe_stem(id);
        // Lock order names → on_disk_stems, matching `remove`'s order.
        let names = self.names.lock_safe();
        let on_disk = self.on_disk_stems.lock_safe();
        if !on_disk.contains(&legacy) {
            return false;
        }
        names.get(&legacy).is_none_or(|owner| owner == id)
    }

    /// Record `id`'s on-disk stem in [`NAMES_FILE`] when the name alone is not
    /// enough to identify it. That is the case for a capped id (undecodable)
    /// and for any stem containing `_`: the pre-P5 fold maps every unsafe
    /// character to `_`, so another id could fold to the same name. Normal
    /// stems (no `_`, under the cap) need no entry — `stem_id` recovers them.
    ///
    /// The publish path calls this *before* renaming its `.part` into place: if
    /// the process dies after the rename, the index entry is already durable, so
    /// the next load still maps the file back to its id, and `remove` can tell
    /// an owned alias from an ambiguous pre-P5 copy. A write failure returns
    /// `false` (and rolls the in-memory entry back) so the publisher can refuse
    /// to create a file whose identity would not survive a restart.
    pub(crate) fn record_name(&self, id: &str) -> bool {
        let stem = crate::fetch::safe_stem(id);
        if crate::fetch::stem_id(&stem).is_some() && !stem.contains('_') {
            return true;
        }
        let (ack_tx, ack_rx) = std::sync::mpsc::channel();
        {
            let mut names = self.names.lock_safe();
            if names.get(&stem).is_some_and(|known| known == id) {
                return true;
            }
            names.insert(stem.clone(), id.to_owned());
            // Submit under the names lock so the writer's snapshot order
            // matches the map's mutation order. The write itself happens on the
            // writer thread; this caller (a download worker) waits for the ack
            // because the index must be durable before the `.part` rename.
            self.names_writer.submit(names.clone(), Some(ack_tx));
        }
        let saved = ack_rx.recv().unwrap_or(false);
        if !saved {
            // Do not let a stale in-memory entry make a later retry think the
            // name is already durable. Only remove our own mapping: a concurrent
            // remove/record for the same stem may have changed it already.
            let mut names = self.names.lock_safe();
            if names.get(&stem).is_some_and(|owner| owner == id) {
                names.remove(&stem);
            }
        }
        saved
    }

    /// Wait until every queued [`NAMES_FILE`] write has been attempted.
    ///
    /// `remove` queues its index update instead of fsyncing on the UI thread, so
    /// tests (and shutdown callers) use this barrier before re-reading the file
    /// or dropping the last handle.
    pub(crate) fn flush_names(&self) {
        self.names_writer.flush();
    }

    /// Whether a download for `id` is currently in flight.
    ///
    /// Not read by the production paths (the UI gets the whole set from
    /// [`state`](Self::state)), but it is the natural single-item query next to
    /// [`is_downloaded`](Self::is_downloaded) and the store tests pin its
    /// behaviour.
    #[allow(dead_code)]
    pub fn is_downloading(&self, id: &str) -> bool {
        self.downloading.lock_safe().contains(id)
    }

    /// Whether `id` carries a removal tombstone: the user deleted it (or its
    /// in-flight download) and no lifecycle call may resurrect it until
    /// [`begin`](Self::begin) is called again for a deliberate retry.
    ///
    /// The download worker checks this immediately before publishing a completed
    /// `.part` file.
    pub fn is_canceled(&self, id: &str) -> bool {
        self.canceled.lock_safe().contains(id)
    }

    /// Record the byte progress of the active transfer for `id`.
    ///
    /// Returns the stored permille fraction, or `None` when nothing was recorded:
    /// the id is not currently claimed by [`begin`](Self::begin), the user
    /// canceled it, or `total` is 0 (length unknown). The caller uses the return
    /// value to publish a lightweight
    /// [`DownloadProgress`](crate::BackendEvent::DownloadProgress) event rather
    /// than cloning a whole state snapshot, so an ignored or indeterminate
    /// update never reaches the UI as a stale fraction. Clamped into `0..=1000`.
    #[cfg(test)]
    pub fn set_progress(&self, id: &str, received: u64, total: u64) -> Option<u16> {
        let token = self.claim_token(id)?;
        self.set_progress_attempt(id, token, received, total)
    }

    /// [`set_progress`](Self::set_progress) for a specific claim attempt.
    ///
    /// The download worker uses this so a stale attempt cannot move the bar of a
    /// retry that has since claimed the id.
    pub fn set_progress_attempt(
        &self,
        id: &str,
        token: u64,
        received: u64,
        total: u64,
    ) -> Option<u16> {
        // Lock order (downloading → progress → canceled → claims) is the one
        // every multi-lock method here follows; see [`state`](Self::state).
        let downloading = self.downloading.lock_safe();
        if !downloading.contains(id) {
            return None;
        }
        let mut progress = self.progress.lock_safe();
        if self.canceled.lock_safe().contains(id) {
            return None;
        }
        if !self.is_current_attempt(id, token) {
            return None;
        }
        match progress_permille(received, total) {
            Some(permille) => {
                progress.insert(id.to_string(), permille);
                Some(permille)
            }
            None => {
                progress.remove(id);
                None
            }
        }
    }

    /// Claim a download. Returns `true` if it was newly started, `false` if one
    /// was already in flight — this atomic check-and-set is what prevents the
    /// same track being fetched twice concurrently. Starting (or restarting) a
    /// download clears any prior "failed" mark, any stale progress fraction and
    /// any removal tombstone (a retry after [`remove`](Self::remove) is allowed).
    ///
    /// The sets are updated under all guards because this is one logical
    /// transition ("a retry clears the failure mark and the tombstone *and*
    /// claims the track"); the order (downloading, progress, failed, canceled)
    /// matches [`state`](Self::state)'s, so a snapshot can never observe the
    /// half-transition. A duplicate claim leaves an active transfer's progress
    /// alone — it is the same download, not a restart.
    pub fn begin(&self, id: &str) -> bool {
        let mut downloading = self.downloading.lock_safe();
        let claimed = downloading.insert(id.to_string());
        if claimed {
            self.progress.lock_safe().remove(id);
        }
        self.failed.lock_safe().remove(id);
        self.canceled.lock_safe().remove(id);
        if claimed {
            // A fresh token invalidates every worker from an earlier attempt of
            // this id, even though the tombstone above was just cleared for the
            // retry. `remove` dropped the old token; this is the new authority.
            let mut claims = self.claims.lock_safe();
            let token = claims.next.saturating_add(1);
            claims.next = token;
            claims.current.insert(id.to_string(), token);
        }
        claimed
    }

    /// The token of the attempt currently allowed to mutate `id`.
    ///
    /// Captured by the download worker immediately after a successful
    /// [`begin`](Self::begin); every progress/finish/fail/publish check compares
    /// it so a worker abandoned by `remove` becomes a no-op even when a retry
    /// has already cleared the tombstone and claimed a new token.
    pub fn claim_token(&self, id: &str) -> Option<u64> {
        self.claims.lock_safe().current.get(id).copied()
    }

    /// Whether `token` is still the live claim for `id`.
    pub fn is_current_attempt(&self, id: &str, token: u64) -> bool {
        self.claims.lock_safe().current.get(id) == Some(&token)
    }

    /// Move a finished download from in-flight to downloaded, clearing every
    /// other status for it in the same snapshot-visible step.
    ///
    /// A canceled id is ignored: the user removed it while the transfer was
    /// running, and a stale worker finishing afterwards must not re-add a row
    /// (or a file) the user deleted.
    #[cfg(test)]
    pub fn finish(&self, id: &str) {
        if let Some(token) = self.claim_token(id) {
            self.finish_attempt(id, token);
        }
    }

    /// [`finish`](Self::finish) for a specific claim attempt. A stale worker
    /// (its id was removed and re-claimed) is ignored.
    pub fn finish_attempt(&self, id: &str, token: u64) {
        let mut downloaded = self.downloaded.lock_safe();
        let mut downloading = self.downloading.lock_safe();
        let mut progress = self.progress.lock_safe();
        let mut failed = self.failed.lock_safe();
        let canceled = self.canceled.lock_safe();
        let mut claims = self.claims.lock_safe();
        if canceled.contains(id) || claims.current.get(id) != Some(&token) {
            return;
        }
        claims.current.remove(id);
        downloading.remove(id);
        progress.remove(id);
        failed.remove(id);
        downloaded.insert(id.to_string());
    }

    /// Release an in-flight claim after a failed pass, without marking the track
    /// as permanently failed (a retry may still be scheduled). Ignored for a
    /// canceled id: the claim was already released by [`remove`](Self::remove),
    /// and a stale failure must not disturb a retry that has since claimed it.
    #[cfg(test)]
    pub fn fail(&self, id: &str) {
        if let Some(token) = self.claim_token(id) {
            self.fail_attempt(id, token);
        }
    }

    /// [`fail`](Self::fail) for a specific claim attempt.
    pub fn fail_attempt(&self, id: &str, token: u64) {
        let mut downloading = self.downloading.lock_safe();
        let mut progress = self.progress.lock_safe();
        let canceled = self.canceled.lock_safe();
        let claims = self.claims.lock_safe();
        if canceled.contains(id) || claims.current.get(id) != Some(&token) {
            return;
        }
        // Keep the token: the attempt stays identified until the retry's `begin`
        // replaces it or `remove` drops it, which is what lets the pass-level
        // retry logic call the id-only `mark_failed`/`fail` helpers without
        // racing a stale worker. Only `finish`/`mark_failed` (terminal states)
        // clear the token.
        progress.remove(id);
        downloading.remove(id);
    }

    /// Give up on a track after repeated failures: drop any in-flight claim and
    /// record it as failed so the UI can offer a manual retry. Ignored for a
    /// canceled id, for the same reason as [`fail`](Self::fail).
    pub fn mark_failed(&self, id: &str) {
        if let Some(token) = self.claim_token(id) {
            self.mark_failed_attempt(id, token);
        }
    }

    /// [`mark_failed`](Self::mark_failed) for a specific claim attempt.
    pub fn mark_failed_attempt(&self, id: &str, token: u64) {
        let mut downloading = self.downloading.lock_safe();
        let mut progress = self.progress.lock_safe();
        let mut failed = self.failed.lock_safe();
        let canceled = self.canceled.lock_safe();
        let mut claims = self.claims.lock_safe();
        if canceled.contains(id) || claims.current.get(id) != Some(&token) {
            return;
        }
        claims.current.remove(id);
        progress.remove(id);
        downloading.remove(id);
        failed.insert(id.to_string());
    }

    /// Cancel `id` and delete its offline copy: the final `<safe_stem>.m4a`,
    /// the in-flight `<safe_stem>.m4a.part`, and the pre-P5
    /// `<legacy_safe_stem>.m4a` when the naming fix gave the id a different
    /// name.
    ///
    /// The id is tombstoned first, under the claim lock, so a worker that was
    /// already transferring cannot re-publish it (see
    /// [`canceled`](Self::is_canceled)). The unlinks happen while the guards are
    /// still held: releasing the claim before deleting would let a retry claim
    /// the id and recreate the `.part` file that this remove is about to unlink.
    ///
    /// A legacy file is inherently ambiguous (`a_b.m4a` may have been written
    /// for `a/b`, `a_b` or another id that folds the same way). When the name
    /// index knows the stem belongs to a different id (it was published for
    /// that id after the fix), this remove leaves it untouched — it is that
    /// track's audio. A pre-P5 file has no owner, so removing any id that folds
    /// to it deletes the shared file and unmarks the aliases, the same thing
    /// the pre-P5 `remove` did when it knew only the folded name.
    ///
    /// Returns true if an offline copy or a partial transfer existed.
    pub fn remove(&self, id: &str) -> bool {
        let mut existed = false;
        let current = self.path(id);
        let part = self.part_path(id);
        let legacy_stem = crate::fetch::legacy_safe_stem(id);
        let index_changed;
        {
            let mut downloaded = self.downloaded.lock_safe();
            let mut downloading = self.downloading.lock_safe();
            let mut progress = self.progress.lock_safe();
            let mut failed = self.failed.lock_safe();
            let mut canceled = self.canceled.lock_safe();
            let mut claims = self.claims.lock_safe();
            let mut names = self.names.lock_safe();
            let mut on_disk = self.on_disk_stems.lock_safe();
            let current_stem = crate::fetch::safe_stem(id);
            // The attempt that may still be transferring owns this `.part`.
            // Dropping the token first makes its late lifecycle calls no-ops;
            // its unique part path is deleted here too (and its own publish
            // refuses to rename it, so a canceled transfer can never publish).
            let attempt_part = claims
                .current
                .remove(id)
                .map(|token| self.part_path_for(id, token));
            // A name-index entry means the file was published for a specific
            // id. If the pre-P5 alias belongs to a *different* id (`a_b.m4a`
            // published for `a_b`) it is that track's copy, not this one's, so
            // removing `a/b` must leave it alone.
            let legacy_owned_elsewhere = names.get(&legacy_stem).is_some_and(|owner| owner != id);
            downloaded.remove(id);
            // `load` registers a pre-P5 file under its raw stem as well (the
            // old behaviour); drop that pseudo-id too, or the deleted file would
            // still count as downloaded for the stem string.
            if !legacy_owned_elsewhere {
                downloaded.remove(&legacy_stem);
                on_disk.remove(&legacy_stem);
            }
            downloading.remove(id);
            progress.remove(id);
            failed.remove(id);
            canceled.insert(id.to_string());
            on_disk.remove(&current_stem);

            let mut paths = vec![current, part];
            if let Some(attempt_part) = attempt_part {
                if !paths.contains(&attempt_part) {
                    paths.push(attempt_part);
                }
            }
            if !legacy_owned_elsewhere {
                if let Some(legacy) = self.legacy_path(id) {
                    paths.push(legacy);
                }
            }
            for path in paths {
                match std::fs::remove_file(&path) {
                    Ok(()) => existed = true,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => {
                        warn!("could not delete download file {}: {e}", path.display());
                    }
                }
            }
            index_changed = names.remove(&current_stem).is_some();
        }
        if index_changed {
            // Queue the metadata update; the UI thread must not fsync here. The
            // writer writes snapshots in submission order, and the names lock
            // orders this snapshot against a worker's `record_name`. A crash
            // before the write is safe: `load` prunes an entry whose file is
            // gone, and a stale entry for a still-present file is the same
            // state the pre-existing best-effort save could leave.
            let names = self.names.lock_safe();
            self.names_writer.submit(names.clone(), None);
        }
        existed
    }

    /// A snapshot of all status sets and the active transfers' progress.
    ///
    /// The sets are locked together in a fixed order (downloaded → downloading
    /// → progress → failed → canceled → claims → names → on_disk_stems, which every
    /// multi-lock method here follows; single-query methods take an in-order
    /// subsequence). Locking them one at a time — what this used to do — let a
    /// track appear in `downloading` and `downloaded` in the same snapshot while a
    /// download finished, so the UI could briefly show two contradictory states
    /// for one row. The progress map is taken between `downloading` and `failed`
    /// so a snapshot can never contain a fraction for a track that is no longer
    /// downloading. Canceled ids are filtered out defensively: the transitions
    /// already remove them, and a tombstone must never leak back into the UI as
    /// a live status.
    pub fn state(&self) -> DownloadState {
        let downloaded = self.downloaded.lock_safe();
        let downloading = self.downloading.lock_safe();
        let progress = self.progress.lock_safe();
        let failed = self.failed.lock_safe();
        let canceled = self.canceled.lock_safe();
        let live = |id: &String| !canceled.contains(id);
        DownloadState {
            downloaded: downloaded.iter().filter(|id| live(id)).cloned().collect(),
            downloading: downloading.iter().filter(|id| live(id)).cloned().collect(),
            failed: failed.iter().filter(|id| live(id)).cloned().collect(),
            progress: progress
                .iter()
                .filter(|(id, _)| live(id))
                .map(|(id, permille)| (id.clone(), *permille))
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::testing::TempDir;

    /// `dir` must outlive the store: the guard deletes the directory on drop, so
    /// the test owns it and the helper only borrows it. Every test therefore
    /// starts by making one.
    fn temp_downloads(dir: &TempDir) -> Downloads {
        Downloads::load(dir.path().to_path_buf())
    }

    #[test]
    fn begin_claims_once_so_concurrent_calls_dont_double_fetch() {
        let dir = TempDir::new("downloads");
        let d = temp_downloads(&dir);
        assert!(d.begin("a"), "first claim succeeds");
        assert!(!d.begin("a"), "second claim is rejected");
        assert!(d.is_downloading("a"));
    }

    #[test]
    fn finish_moves_from_in_flight_to_downloaded() {
        let dir = TempDir::new("downloads");
        let d = temp_downloads(&dir);
        d.begin("a");
        d.finish("a");
        assert!(!d.is_downloading("a"));
        assert!(d.is_downloaded("a"));
        // After finishing it can be claimed again (e.g. re-download).
        assert!(d.begin("a"));
    }

    /// A remove followed by a retry must invalidate the abandoned worker: its
    /// stale token may not move progress, finish, fail or publish, and the two
    /// attempts must never share one `.part` path (the corruption this guards
    /// against: the stale worker renaming the retry's mid-write file).
    #[test]
    fn a_removed_claim_invalidates_its_worker_and_retries_get_a_new_token() {
        let dir = TempDir::new("downloads-claims");
        let d = temp_downloads(&dir);
        assert!(d.begin("a"));
        let first = d.claim_token("a").expect("first token");
        let first_part = d.part_path_for("a", first);
        d.remove("a");
        assert!(d.begin("a"));
        let second = d.claim_token("a").expect("retry token");
        assert_ne!(first, second, "a retry gets a fresh claim token");
        assert!(!d.is_current_attempt("a", first));
        assert_ne!(
            first_part,
            d.part_path_for("a", second),
            "attempts must not share a part file"
        );

        // Every stale lifecycle call is a no-op.
        assert_eq!(d.set_progress_attempt("a", first, 900, 1000), None);
        d.finish_attempt("a", first);
        assert!(
            !d.is_downloaded("a"),
            "a stale finish cannot mark downloaded"
        );
        d.mark_failed_attempt("a", first);
        assert!(
            !d.state().failed.contains("a"),
            "a stale failure cannot mark the retry failed"
        );

        // The live retry still works normally.
        assert_eq!(d.set_progress_attempt("a", second, 500, 1000), Some(500));
        d.finish_attempt("a", second);
        assert!(d.is_downloaded("a"));
        assert_eq!(d.claim_token("a"), None, "a finished claim is released");
    }

    #[test]
    fn fail_releases_the_claim_without_marking_downloaded() {
        let dir = TempDir::new("downloads");
        let d = temp_downloads(&dir);
        d.begin("a");
        d.fail("a");
        assert!(!d.is_downloading("a"));
        assert!(!d.is_downloaded("a"));
    }

    #[test]
    fn remove_deletes_the_file_and_clears_state() {
        let dir = TempDir::new("downloads");
        let d = temp_downloads(&dir);
        std::fs::write(d.path("a"), b"audio").unwrap();
        assert!(d.begin("a"));
        d.finish("a");
        assert!(d.is_downloaded("a"));

        assert!(d.remove("a"), "removing an existing download reports true");
        assert!(!d.is_downloaded("a"));
        assert!(!d.path("a").exists());
        assert!(!d.remove("a"), "removing a missing download reports false");
    }

    /// Removing an id while its transfer is running is a cancellation, not just
    /// a delete: the claim, the fraction and the failed mark all go, the id is
    /// tombstoned so the still-running worker cannot re-add it, and both the
    /// final file and the in-flight `.part` are deleted.
    #[test]
    fn remove_cancels_an_in_flight_transfer_and_deletes_both_files() {
        let dir = TempDir::new("downloads-cancel");
        let d = temp_downloads(&dir);
        assert!(d.begin("a"));
        d.set_progress("a", 500, 1000);
        std::fs::write(d.part_path("a"), b"partial").expect("write the part file");
        // Residue from an earlier copy: remove must delete this too.
        std::fs::write(d.path("a"), b"old audio").expect("write the final file");

        assert!(d.remove("a"), "an in-flight part counts as existing");
        assert!(!d.is_downloading("a"), "the in-flight claim is dropped");
        assert!(d.is_canceled("a"), "the id is tombstoned for the worker");
        assert!(!d.path("a").exists(), "the final file is deleted");
        assert!(!d.part_path("a").exists(), "the part file is deleted");

        let state = d.state();
        assert!(!state.downloading.contains("a"));
        assert!(!state.downloaded.contains("a"));
        assert!(!state.failed.contains("a"));
        assert!(!state.progress.contains_key("a"));
    }

    /// A tombstone only blocks the stale worker; an explicit retry must be able
    /// to download the track again.
    #[test]
    fn a_retry_clears_the_cancel_tombstone() {
        let dir = TempDir::new("downloads-cancel-retry");
        let d = temp_downloads(&dir);
        d.remove("a");
        assert!(d.is_canceled("a"));

        assert!(d.begin("a"), "a removed id can be claimed again");
        assert!(!d.is_canceled("a"), "the retry clears the tombstone");
        d.set_progress("a", 250, 1000);
        assert_eq!(d.state().progress.get("a"), Some(&250));
        d.finish("a");
        assert!(d.is_downloaded("a"), "the retry can publish normally");
    }

    /// Every lifecycle call a stale worker can make after `remove` must be
    /// ignored: no status set may regain the id and no fraction may reappear.
    #[test]
    fn lifecycle_calls_after_cancel_cannot_resurrect_the_track() {
        let dir = TempDir::new("downloads-cancel-stale");
        let d = temp_downloads(&dir);
        assert!(d.begin("a"));
        d.set_progress("a", 400, 1000);
        std::fs::write(d.part_path("a"), b"partial").expect("write the part file");
        assert!(d.remove("a"));

        d.set_progress("a", 900, 1000);
        d.finish("a");
        d.fail("a");
        d.mark_failed("a");

        let state = d.state();
        assert!(!state.downloaded.contains("a"), "finish-after-cancel");
        assert!(!state.downloading.contains("a"));
        assert!(!state.failed.contains("a"), "mark_failed-after-cancel");
        assert!(!state.progress.contains_key("a"));
        assert_eq!(
            d.set_progress("a", 900, 1000),
            None,
            "a canceled track records no fraction and must publish no progress event"
        );
        assert!(d.is_canceled("a"), "the tombstone survives stale calls");
    }

    #[test]
    fn mark_failed_records_and_is_cleared_by_a_retry_or_success() {
        let dir = TempDir::new("downloads");
        let d = temp_downloads(&dir);
        d.begin("a");
        d.mark_failed("a");
        assert!(!d.is_downloading("a"), "the in-flight claim is dropped");
        assert!(d.state().failed.contains("a"));

        // Re-attempting clears the failed mark (the row stops showing retry).
        assert!(d.begin("a"));
        assert!(!d.state().failed.contains("a"));

        // Finishing also clears it, and removing does too.
        d.mark_failed("a");
        assert!(d.begin("a"));
        d.finish("a");
        assert!(!d.state().failed.contains("a"));
        assert!(d.is_downloaded("a"));

        std::fs::write(d.path("a"), b"x").unwrap();
        assert!(d.begin("a"));
        d.mark_failed("a");
        d.remove("a");
        assert!(!d.state().failed.contains("a"));
    }

    #[test]
    fn load_discovers_existing_offline_files() {
        let dir = TempDir::new("downloads");
        let d = temp_downloads(&dir);
        std::fs::write(d.dir.join("song.m4a"), b"x").unwrap();
        std::fs::write(d.dir.join("notes.txt"), b"x").unwrap();
        let reloaded = Downloads::load(d.dir.clone());
        assert!(reloaded.is_downloaded("song"));
        assert!(!reloaded.is_downloaded("notes"));
    }

    /// `state` is the UI's whole view of downloads, so it must not be a
    /// halfway blend of the three sets: a track moving between them appears in
    /// exactly one of `downloading`/`downloaded`/`failed` at every step.
    #[test]
    fn load_drops_a_zero_byte_m4a_instead_of_marking_it_downloaded() {
        let dir = TempDir::new("downloads-empty-copy");
        let downloads = Downloads::load(dir.path().to_path_buf());
        let empty = downloads.path("empty");
        std::fs::write(&empty, b"").expect("write the empty offline file");

        let reloaded = Downloads::load(downloads.dir.clone());
        assert!(
            !reloaded.is_downloaded("empty"),
            "a zero-byte file is not an offline copy"
        );
        assert!(
            !empty.exists(),
            "the startup scan cleans the unusable file up so Download can repair it"
        );
    }

    #[test]
    fn state_is_consistent_across_the_download_lifecycle() {
        let dir = TempDir::new("downloads-state");
        let d = temp_downloads(&dir);

        assert_eq!(d.state(), DownloadState::default());

        assert!(d.begin("a"));
        let s = d.state();
        assert!(s.downloading.contains("a"));
        assert!(!s.downloaded.contains("a"));
        assert!(!s.failed.contains("a"));

        d.finish("a");
        let s = d.state();
        assert!(s.downloaded.contains("a"));
        assert!(!s.downloading.contains("a"));
        assert!(!s.failed.contains("a"));

        assert!(d.begin("b"));
        d.mark_failed("b");
        let s = d.state();
        assert!(s.failed.contains("b"));
        assert!(!s.downloading.contains("b"));
        assert!(!s.downloaded.contains("b"));
    }

    /// The permille contract (`docs/native-streaming.md` §3.4): integer
    /// 0..=1000, clamped, and "unknown" for a zero total. Pinned here as a pure
    /// function because the UI reads the same numbers as an `f32` fraction.
    #[test]
    fn progress_permille_maps_bytes_to_the_ui_contract() {
        assert_eq!(super::progress_permille(0, 100), Some(0));
        assert_eq!(super::progress_permille(1, 1000), Some(1));
        assert_eq!(super::progress_permille(1, 3), Some(333));
        assert_eq!(super::progress_permille(2, 3), Some(666));
        assert_eq!(super::progress_permille(999, 1000), Some(999));
        assert_eq!(super::progress_permille(1000, 1000), Some(1000));
        assert_eq!(
            super::progress_permille(2_000, 1_000),
            Some(1000),
            "an over-reporting transfer is clamped to 100%, never wrapped"
        );
        assert_eq!(
            super::progress_permille(u64::MAX, u64::MAX),
            Some(1000),
            "the mapping cannot overflow for multi-gigabyte streams"
        );
        assert_eq!(super::progress_permille(5, 0), None, "unknown length");
        assert_eq!(super::progress_permille(0, 0), None, "unknown length");
    }

    /// A fraction may only be visible for a track that is actively downloading:
    /// setting it before the claim is ignored, and the snapshot stops carrying
    /// it the moment the track stops downloading.
    #[test]
    fn progress_is_recorded_only_while_the_track_is_downloading() {
        let dir = TempDir::new("downloads-progress-claim");
        let d = temp_downloads(&dir);

        // Not in flight: ignored, so a queued track never gets a bar. The
        // `None` is also what keeps the worker from publishing a progress event.
        assert_eq!(d.set_progress("a", 250, 1000), None);
        assert!(!d.state().progress.contains_key("a"));

        assert!(d.begin("a"));
        assert_eq!(
            d.set_progress("a", 250, 1000),
            Some(250),
            "the recorded fraction is returned for the lightweight progress event"
        );
        assert_eq!(d.state().progress.get("a"), Some(&250));

        // Finished: the fraction is gone even though the track is now
        // downloaded (a determinate bar is only for in-flight transfers).
        d.finish("a");
        let state = d.state();
        assert!(state.downloaded.contains("a"));
        assert!(!state.progress.contains_key("a"));

        // Setting it after the claim was released is ignored, and reports that
        // nothing was recorded.
        assert_eq!(d.set_progress("a", 500, 1000), None);
        assert!(!d.state().progress.contains_key("a"));
        assert_eq!(
            d.set_progress("a", 500, 0),
            None,
            "an unknown length records nothing (indeterminate spinner)"
        );
    }

    /// Every way a claim ends must clear the fraction in the same
    /// snapshot-visible step, so a stale bar can never outlive a claim.
    #[test]
    fn every_lifecycle_transition_clears_progress() {
        let dir = TempDir::new("downloads-progress-clear");
        let d = temp_downloads(&dir);

        let mid_transfer = |id: &str| {
            assert!(d.begin(id), "claim {id}");
            d.set_progress(id, 500, 1000);
            assert_eq!(d.state().progress.get(id), Some(&500));
        };

        // fail: released for a possible retry, no fraction left over.
        mid_transfer("fail");
        d.fail("fail");
        assert!(!d.state().progress.contains_key("fail"));

        // mark_failed: given up, nothing for the retry row to show.
        mid_transfer("mark_failed");
        d.mark_failed("mark_failed");
        assert!(!d.state().progress.contains_key("mark_failed"));

        // finish: published as downloaded, bar gone.
        mid_transfer("finish");
        d.finish("finish");
        assert!(!d.state().progress.contains_key("finish"));

        // remove: a re-download starts from a clean slate.
        mid_transfer("remove");
        d.remove("remove");
        assert!(!d.state().progress.contains_key("remove"));

        // begin: a *new* claim clears anything stale from a previous one.
        assert!(d.begin("begin"));
        d.set_progress("begin", 700, 1000);
        d.fail("begin");
        assert!(!d.state().progress.contains_key("begin"));
        assert!(d.begin("begin"), "re-claim");
        assert!(
            !d.state().progress.contains_key("begin"),
            "a fresh claim starts indeterminate"
        );

        // A duplicate claim is not a restart: the active transfer's fraction
        // must survive it, or concurrent callers would wipe a live bar.
        d.set_progress("begin", 400, 1000);
        assert!(!d.begin("begin"), "the second claim is rejected");
        assert_eq!(
            d.state().progress.get("begin"),
            Some(&400),
            "a rejected duplicate claim leaves the live progress alone"
        );
    }

    /// The snapshot is internally consistent: a track can never appear with a
    /// fraction after it stopped being `downloading`, and a total of zero means
    /// unknown (the entry disappears rather than showing a 0% bar forever).
    #[test]
    fn the_state_snapshot_never_has_progress_for_a_finished_track() {
        let dir = TempDir::new("downloads-progress-state");
        let d = temp_downloads(&dir);

        assert!(d.begin("a"));
        assert!(d.begin("b"));
        d.set_progress("a", 125, 1000);
        d.set_progress("b", 900, 1000);
        let state = d.state();
        assert_eq!(state.progress.get("a"), Some(&125));
        assert_eq!(state.progress.get("b"), Some(&900));

        d.finish("a");
        let state = d.state();
        assert!(state.downloading.contains("b"));
        assert_eq!(
            state.progress.get("b"),
            Some(&900),
            "the other in-flight track keeps its fraction"
        );
        assert!(
            !state.progress.contains_key("a"),
            "the finished track's bar is gone in the same snapshot"
        );

        // An unknown length drops the entry (indeterminate), not a stale bar.
        d.set_progress("b", 950, 0);
        assert!(!d.state().progress.contains_key("b"));
    }

    /// Regression for the reported defect: an id that had to be sanitised was
    /// stored under the folded stem, so `load` keyed the downloaded set by that
    /// stem while `is_downloaded` checked the raw id. After a restart the track
    /// looked un-downloaded and the file was invisible to the offline path.
    #[test]
    fn load_recognizes_a_sanitising_id_under_its_current_name() {
        let dir = TempDir::new("downloads-restart-sanitising");
        let id = "a/b";
        let d = temp_downloads(&dir);
        let path = d.path(id);
        assert_eq!(
            path.file_name().and_then(|name| name.to_str()),
            Some("a~2fb.m4a"),
            "the current name encodes the separator instead of folding it"
        );
        std::fs::write(&path, b"audio").expect("write the offline copy");

        let reloaded = Downloads::load(d.dir.clone());
        assert!(reloaded.is_downloaded(id));
        assert!(reloaded.path(id).exists());
        assert!(
            reloaded.state().downloaded.contains(id),
            "the UI snapshot must agree with is_downloaded"
        );
    }

    /// An install from before the naming fix has `a_b.m4a` for `a/b`. It must be
    /// recognized without guessing, and the file must not be renamed or removed
    /// by the startup scan.
    #[test]
    fn load_recognizes_a_pre_p5_folded_name_and_never_renames_it() {
        let dir = TempDir::new("downloads-legacy-folded");
        let id = "a/b";
        let legacy = dir
            .path()
            .join(format!("{}.m4a", crate::fetch::legacy_safe_stem(id)));
        std::fs::write(&legacy, b"old audio").expect("write the legacy copy");

        let d = Downloads::load(dir.path().to_path_buf());
        assert!(
            d.is_downloaded(id),
            "the legacy stem must be found through the load-time stem set"
        );
        assert!(legacy.exists(), "a non-empty legacy file is never removed");
        assert!(
            !d.path(id).exists(),
            "load never renames an old file, it only reads it"
        );
        assert_eq!(
            std::fs::read(&legacy).expect("read the legacy copy"),
            b"old audio"
        );

        // The explicit remove must still delete the file the store counted.
        assert!(d.remove(id));
        assert!(!legacy.exists(), "remove deletes the legacy name too");
        assert!(!d.is_downloaded(id));
    }

    /// The end-to-end upgrade promise: after a restart the store recognizes the
    /// old folded file *and* the playback lookup serves it without the network.
    /// The file is the only possible source of the bytes, so a network fallback
    /// cannot make this pass.
    #[tokio::test]
    async fn a_legacy_file_is_recognized_after_load_and_served_offline() {
        let dir = TempDir::new("downloads-legacy-playback");
        let id = "a/b";
        let legacy = dir
            .path()
            .join(format!("{}.m4a", crate::fetch::legacy_safe_stem(id)));
        std::fs::write(&legacy, b"legacy audio").expect("write the legacy copy");

        let d = Downloads::load(dir.path().to_path_buf());
        assert!(d.is_downloaded(id));
        let (data, _) = crate::fetch::fetch_track(&d.dir, id)
            .await
            .expect("a legacy offline copy needs no network");
        assert_eq!(data, b"legacy audio");
    }

    /// Fresh downloads of two ids that used to collide (`a/b` and `a_b`) now
    /// get separate names. The plain id's name contains `_`, so its publication
    /// is recorded in the name index; removing the folded id must then leave
    /// that owned file — it is the other track's only copy. Before the ownership
    /// entry existed, `remove("a/b")` would have deleted `a_b.m4a` and lost
    /// `a_b`'s audio.
    #[test]
    fn remove_of_a_folded_alias_spares_another_ids_owned_file() {
        let dir = TempDir::new("downloads-owned-alias");
        let d = temp_downloads(&dir);
        let plain_path = d.path("a_b");
        let folded_path = d.path("a/b");
        assert_eq!(
            plain_path.file_name().and_then(|name| name.to_str()),
            Some("a_b.m4a")
        );
        assert_ne!(plain_path, folded_path);
        std::fs::write(&plain_path, b"a_b audio").expect("write the plain copy");
        assert!(d.record_name("a_b"), "a `_` stem needs an ownership entry");

        // Only the owned plain file exists: it must not make the folded id
        // look downloaded.
        let owner_only = Downloads::load(d.dir.clone());
        assert!(owner_only.is_downloaded("a_b"));
        assert!(
            !owner_only.is_downloaded("a/b"),
            "an owned `a_b.m4a` belongs to a_b, not to every folded alias"
        );

        // Now both tracks have their own copy. Removing the folded id deletes
        // only its file; the owned plain file is `a_b`'s audio.
        std::fs::write(&folded_path, b"a/b audio").expect("write the folded copy");
        let reloaded = Downloads::load(d.dir.clone());
        assert!(reloaded.is_downloaded("a_b"));
        assert!(reloaded.is_downloaded("a/b"));

        assert!(reloaded.remove("a/b"));
        assert!(!folded_path.exists());
        assert!(
            plain_path.exists(),
            "another id's owned file must survive the remove"
        );
        assert!(reloaded.is_downloaded("a_b"));
        assert!(!reloaded.is_downloaded("a/b"));
        assert_eq!(
            std::fs::read(&plain_path).expect("read the preserved copy"),
            b"a_b audio"
        );

        // Removing the owner still deletes it, and drops the index entry.
        assert!(reloaded.remove("a_b"));
        assert!(!plain_path.exists());
        assert!(!reloaded.is_downloaded("a_b"));
    }

    /// Pre-P5 files kept uppercase ids literally. The current encoding escapes
    /// them, so the two names differ; the old one must stay recognized.
    #[test]
    fn load_recognizes_a_pre_p5_uppercase_id_file() {
        let dir = TempDir::new("downloads-legacy-uppercase");
        let id = "dQw4w9WgXcQ";
        let legacy = dir.path().join("dQw4w9WgXcQ.m4a");
        std::fs::write(&legacy, b"old audio").expect("write the legacy copy");

        let d = Downloads::load(dir.path().to_path_buf());
        assert!(d.is_downloaded(id));
        assert!(
            d.state().downloaded.contains(id),
            "the old load keyed the set by the stem; the UI must keep doing so"
        );
        assert_ne!(d.path(id), legacy, "new writes use the escaped name");
        assert!(d.remove(id));
        assert!(!legacy.exists());
        assert!(!d.is_downloaded(id));
    }

    /// Both names for one id can exist (a re-download after an upgrade, or a
    /// pre-P5 file plus a current one). They count as one download, playback
    /// prefers the current name, and removing the track deletes both — it must
    /// not leave one name that a later load would resurrect.
    #[test]
    fn a_current_name_and_a_legacy_name_for_one_id_are_one_download() {
        let dir = TempDir::new("downloads-two-names");
        let id = "dQw4w9WgXcQ";
        let d = temp_downloads(&dir);
        let current = d.path(id);
        let legacy = dir.path().join("dQw4w9WgXcQ.m4a");
        std::fs::write(&current, b"new audio").expect("write the current copy");
        std::fs::write(&legacy, b"old audio").expect("write the legacy copy");

        let reloaded = Downloads::load(d.dir.clone());
        assert!(reloaded.is_downloaded(id));
        assert_eq!(
            reloaded
                .state()
                .downloaded
                .iter()
                .filter(|known| known.as_str() == id)
                .count(),
            1,
            "two files for one id are one downloaded id"
        );
        assert_eq!(reloaded.path(id), current, "the current name is canonical");

        assert!(reloaded.remove(id));
        assert!(!current.exists(), "remove deletes the current name");
        assert!(!legacy.exists(), "remove deletes the legacy name");
        let after_remove = Downloads::load(reloaded.dir.clone());
        assert!(
            !after_remove.is_downloaded(id),
            "a leftover name must not resurrect a removed download"
        );
    }

    /// A capped (overlong) id cannot be decoded from its name, so the name index
    /// is what makes it survive a restart. Losing the index must leave the audio
    /// on disk (never deleted), just unrecognized.
    #[test]
    fn load_maps_a_capped_name_back_through_the_name_index() {
        let dir = TempDir::new("downloads-capped-index");
        let id = "x".repeat(400);
        let d = temp_downloads(&dir);
        let path = d.path(&id);
        std::fs::write(&path, b"audio").expect("write the capped audio");
        d.record_name(&id);
        assert!(
            d.dir.join(NAMES_FILE).exists(),
            "recording a capped name writes the index"
        );

        let reloaded = Downloads::load(d.dir.clone());
        assert!(reloaded.is_downloaded(&id));
        assert!(reloaded.path(&id).exists());

        // The index is derived metadata: without it the name cannot be mapped,
        // but the audio file is user data and must survive untouched.
        std::fs::remove_file(d.dir.join(NAMES_FILE)).expect("drop the index");
        let without_index = Downloads::load(d.dir.clone());
        assert!(!without_index.is_downloaded(&id));
        assert!(
            path.exists(),
            "an unrecognized capped file is kept, not deleted"
        );
        assert_eq!(
            without_index.state().downloaded.len(),
            0,
            "a capped stem is never guessed back to an id"
        );
    }

    /// Playback consults the same ownership rule as `is_downloaded`: an index-
    /// owned `_` stem belongs to its recorded id, not to every folded alias.
    #[test]
    fn legacy_ownership_helper_rejects_another_ids_owned_stem() {
        let dir = TempDir::new("downloads-legacy-owner");
        let mut names = BTreeMap::new();
        names.insert("a_b".to_owned(), "a_b".to_owned());
        assert!(save_names(dir.path(), &names));

        assert!(legacy_name_owned_by_other(dir.path(), "a_b", "a/b"));
        assert!(!legacy_name_owned_by_other(dir.path(), "a_b", "a_b"));

        std::fs::remove_file(dir.path().join(NAMES_FILE)).expect("drop the index");
        assert!(
            !legacy_name_owned_by_other(dir.path(), "a_b", "a/b"),
            "an unowned pre-P5 file keeps the legacy fallback"
        );
    }

    /// Removing a capped track deletes its audio and drops its index entry, so a
    /// restart cannot re-add it from a stale mapping.
    #[test]
    fn remove_deletes_a_capped_file_and_its_name_index_entry() {
        let dir = TempDir::new("downloads-capped-remove");
        let id = "y".repeat(400);
        let d = temp_downloads(&dir);
        let path = d.path(&id);
        std::fs::write(&path, b"audio").expect("write the capped audio");
        d.record_name(&id);

        assert!(d.remove(&id));
        assert!(!path.exists());
        assert!(!d.is_downloaded(&id));
        d.flush_names();
        let reloaded = Downloads::load(d.dir.clone());
        assert!(!reloaded.is_downloaded(&id));
    }

    /// The startup scan still deletes zero-byte `.m4a` and leftover `.part`
    /// files exactly as before — the naming change must not affect that.
    #[test]
    fn load_still_cleans_zero_byte_and_part_files() {
        let dir = TempDir::new("downloads-cleanup");
        let d = temp_downloads(&dir);
        let empty = d.path("z");
        let part = d.part_path("z");
        std::fs::write(&empty, b"").expect("write an empty m4a");
        std::fs::write(&part, b"partial").expect("write a part file");

        let reloaded = Downloads::load(d.dir.clone());
        assert!(!empty.exists());
        assert!(!part.exists());
        assert!(!reloaded.is_downloaded("z"));
    }
}
