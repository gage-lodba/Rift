//! Offline-download commands and the background download job, including the
//! retry/backoff logic that keeps a fully-downloaded playlist offline.
//!
//! Transfers go through the native streaming downloader
//! ([`crate::stream::fetch_to_path`]), which writes the caller's temporary
//! `.part` file; the worker publishes it by rename, emits throttled lightweight
//! progress events, and emits a whole [`DownloadState`] snapshot on each
//! lifecycle transition (`docs/native-streaming.md` §3.2–3.4).

use std::sync::Arc;

use rift_types::{DownloadState, Playlist, Track};
use tracing::{info, warn};

use crate::backend::{BackendHandle, BackendInner};
use crate::downloads::Downloads;
use crate::event::BackendEvent;
use crate::util::LockExt;

impl BackendHandle {
    /// Download a set of tracks for offline listening. Already-downloaded and
    /// already-in-flight tracks are skipped; the rest are fetched sequentially
    /// with a [`BackendEvent::Downloads`] event after each one.
    pub fn download_tracks(&self, tracks: Vec<Track>) {
        start_downloads(&self.inner, tracks);
    }

    /// Remove offline copies of the given tracks.
    pub fn remove_downloads(&self, ids: Vec<String>) {
        // Preview mode renders placeholder libraries and must not delete the
        // user's real offline files (the app does not call backend commands in
        // preview, but the skip keeps that true if it ever starts to).
        if crate::settings::preview_mode() {
            return;
        }
        for id in &ids {
            self.inner.downloads.remove(id);
        }
        emit_downloads(&self.inner, &self.inner.downloads.state());
    }
}

fn emit_downloads(backend: &BackendInner, state: &DownloadState) {
    backend.emit(BackendEvent::Downloads(state.clone()));
}

/// Minimum gap between the progress updates the download worker emits for one
/// active track.
///
/// The UI updates one fraction from each lightweight
/// [`BackendEvent::DownloadProgress`]; 100 ms keeps a 3 px bar smooth without
/// flooding the event bus with one event per network chunk
/// (`docs/native-streaming.md` §3.4). Whole `Downloads` snapshots are reserved
/// for lifecycle transitions (`begin`, `finish`, `fail`, `remove`), which the
/// caller still emits immediately.
const PROGRESS_EMIT_INTERVAL: std::time::Duration = std::time::Duration::from_millis(100);

/// Whether a progress snapshot is due at `now`: the first callback always emits
/// (so the bar appears at its known start fraction), then at most one snapshot
/// per [`PROGRESS_EMIT_INTERVAL`].
///
/// Pure (instants are passed in) so the throttle contract is unit-testable
/// without timing a real transfer.
fn progress_emit_due(last_emit: Option<std::time::Instant>, now: std::time::Instant) -> bool {
    last_emit.is_none_or(|at| now.saturating_duration_since(at) >= PROGRESS_EMIT_INTERVAL)
}

/// What one track's transfer did.
///
/// `Canceled` is a normal outcome, not a failure: the user removed the download
/// while it was in flight, so the completed `.part` is discarded and the batch
/// moves on without a "downloaded" mark and without a misleading error toast.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DownloadOutcome {
    /// The file is in place and the store records the track as downloaded.
    Published,
    /// The user removed the download mid-transfer; nothing was published and
    /// the store's tombstone keeps the stale transition out.
    Canceled,
}

/// Collects the one-off failures of a single download pass so the pass can
/// surface one notice instead of one toast per track.
///
/// "One-off" means a failed track that is not a kept-offline playlist's
/// responsibility: its row is marked `failed` for a manual retry, but there is
/// no automatic retry to keep quiet about. The collector is bounded on purpose:
/// it keeps the failure count and only the first (representative) reason, not a
/// per-track error list a large batch could grow without bound.
#[derive(Debug, Default)]
struct OneOffFailures {
    count: usize,
    first: Option<String>,
}

impl OneOffFailures {
    /// Record one failed track's user-facing reason, keeping the first.
    fn record(&mut self, reason: String) {
        if self.first.is_none() {
            self.first = Some(reason);
        }
        self.count = self.count.saturating_add(1);
    }

    /// The single notice for the pass, or `None` when nothing failed.
    ///
    /// Exactly one failure keeps its own specific message — coalescing it into
    /// a vague count would hide the only reason the user has. Several failures
    /// become one summary with the count and the first reason; the per-track
    /// `failed` marks (not this notice) are what offer the retries.
    fn notice(&self) -> Option<String> {
        let first = self.first.as_deref()?;
        if self.count == 1 {
            return Some(first.to_owned());
        }
        Some(format!(
            "Couldn\u{2019}t download {} tracks. First failure: {first}",
            self.count
        ))
    }
}

/// Publish a transfer's completed `.part` file as `<safe_stem>.m4a`, unless the
/// user removed the download while it was running.
///
/// The transfer itself cannot be interrupted (it owns the open `.part` file), so
/// cancellation is enforced at the publication boundary: the tombstone is
/// re-checked immediately before the rename, a canceled transfer's part is
/// deleted instead of published, and a `remove` that lands between the check and
/// the rename is caught by a second check which deletes what was just renamed.
/// Split out from [`download_track`] so this final decision is unit-testable
/// without a network transfer.
async fn publish_part(
    downloads: &Downloads,
    track: &Track,
    attempt: u64,
    part: &std::path::Path,
    dest: &std::path::Path,
) -> Result<DownloadOutcome, String> {
    // Canceled *or* superseded by a retry (the user removed and re-downloaded):
    // the retry has its own part path and claim token, so this worker must
    // neither publish its bytes nor touch the retry's state.
    if downloads.is_canceled(&track.id) || !downloads.is_current_attempt(&track.id, attempt) {
        let _ = tokio::fs::remove_file(part).await;
        return Ok(DownloadOutcome::Canceled);
    }
    // A capped id's name cannot be decoded, so record its stem in the name index
    // *before* publishing: a crash between the two leaves a file the next load
    // cannot map, whereas a stale index entry for a file that never appeared is
    // pruned harmlessly. A normal (decodable) stem needs no entry.
    if !downloads.record_name(&track.id) {
        let _ = tokio::fs::remove_file(part).await;
        return Err(format!(
            "Could not save \u{201c}{}\u{201d}: the offline file name index could not be written",
            track.title
        ));
    }
    // Never replace a non-empty offline file. The job only starts when
    // `is_downloaded` is false, so a file here is either a pre-P5 name that
    // differs only by case on a case-insensitive filesystem (a different track's
    // audio) or a copy whose index was lost. Replacing it would destroy audio
    // Rift cannot reconstruct; failing leaves the row retryable and the file
    // intact (an explicit remove deletes it first).
    if tokio::fs::metadata(dest)
        .await
        .is_ok_and(|meta| meta.len() > 0)
    {
        let _ = tokio::fs::remove_file(part).await;
        return Err(format!(
            "Could not save \u{201c}{}\u{201d}: an offline file with that name already exists",
            track.title
        ));
    }
    // A pre-P5 file under a different name is the same download. The store's
    // `is_downloaded` already skips it, but a publish that still reached this
    // point must not create a second copy under the current name: after an
    // upgrade that would leave two files for one id and make `remove` ambiguous.
    if let Some(legacy) = downloads.legacy_path(&track.id) {
        if !downloads.legacy_owned_elsewhere(&track.id)
            && tokio::fs::metadata(&legacy)
                .await
                .is_ok_and(|meta| meta.len() > 0)
        {
            // This id's own pre-P5 copy is the offline file; publishing a second
            // name would make `remove` ambiguous. A `_` stem owned by a
            // *different* id is that track's audio, not this id's copy, so it
            // must not block this download.
            let _ = tokio::fs::remove_file(part).await;
            return Err(format!(
                "Could not save \u{201c}{}\u{201d}: an offline copy already exists under its previous name",
                track.title
            ));
        }
    }
    match tokio::fs::rename(part, dest).await {
        Ok(()) => {
            // `remove` may have run between the check above and the rename (it
            // could only delete a final file that did not exist yet). If its
            // tombstone appeared, delete what this publish just created rather
            // than leaving an unlisted `<safe_stem>.m4a` for the startup scan to trust.
            if downloads.is_canceled(&track.id) || !downloads.is_current_attempt(&track.id, attempt)
            {
                let _ = tokio::fs::remove_file(dest).await;
                return Ok(DownloadOutcome::Canceled);
            }
            downloads.finish_attempt(&track.id, attempt);
            Ok(DownloadOutcome::Published)
        }
        Err(e) => {
            // A failed rename can leave the `.part` behind; remove it now rather
            // than leaving scratch for the next launch's scan to clear.
            let _ = tokio::fs::remove_file(part).await;
            // `remove` may have deleted the part between the check above and the
            // rename; that is a cancellation, not a save failure to report.
            if downloads.is_canceled(&track.id) {
                return Ok(DownloadOutcome::Canceled);
            }
            Err(format!(
                "Could not save \u{201c}{}\u{201d}: {e}",
                track.title
            ))
        }
    }
}

/// Download one track into its final `<safe_stem>.m4a` path with the native streaming
/// downloader, emitting throttled [`BackendEvent::DownloadProgress`] events as
/// bytes arrive.
///
/// On success the completed `.part` file is published by [`publish_part`] and
/// the track is marked finished. On any failure the `.part` file is removed and
/// the user-facing failure message is returned; the caller records the failure,
/// and its lifecycle transition clears the track's progress fraction. A
/// cancellation (`remove` during the transfer) is an `Ok(Canceled)`: the batch
/// continues and no error is surfaced.
async fn download_track(
    backend: &BackendInner,
    downloads: &Downloads,
    track: &Track,
    attempt: u64,
) -> Result<DownloadOutcome, String> {
    let dest = downloads.path(&track.id);
    // Temp file + rename: a crash mid-write must not leave a truncated .m4a,
    // which the next launch's directory scan would treat as a valid offline
    // copy. The downloader writes this part path directly and never renames it.
    // The attempt token in the name gives each claim its own scratch file, so a
    // worker abandoned by `remove` can never rename a retry's mid-write bytes.
    let part = downloads.part_path_for(&track.id, attempt);

    // Only the active track has a progress entry: `begin` cleared every entry
    // for this batch, and `finish`/`fail`/`mark_failed` clear it when the
    // transfer ends.
    let mut last_emit: Option<std::time::Instant> = None;
    let mut progress = |received: u64, total: u64| {
        // `None` means the transfer is not (or no longer) claimable, or the
        // length is unknown: no fraction to publish, and never a stale one.
        let Some(permille) = downloads.set_progress_attempt(&track.id, attempt, received, total)
        else {
            return;
        };
        // Emit the first update immediately (the bar appears at its known
        // start fraction) and then at most every `PROGRESS_EMIT_INTERVAL`.
        let now = std::time::Instant::now();
        if progress_emit_due(last_emit, now) {
            last_emit = Some(now);
            // One id + one fraction: this deliberately avoids cloning the
            // downloaded/downloading/failed sets on every network chunk.
            backend.emit(BackendEvent::DownloadProgress {
                id: track.id.clone(),
                permille,
            });
        }
    };
    match crate::stream::fetch_to_path(&track.id, &part, &mut progress).await {
        Ok(()) => publish_part(downloads, track, attempt, &part, &dest).await,
        Err(error) => {
            // Never leave a partial transfer behind: a retry writes the same
            // part path from scratch.
            let _ = tokio::fs::remove_file(&part).await;
            // A cancellation can also surface here: `remove` deleted the open
            // part file (or the transfer aborted while it was being deleted), so
            // the expected outcome is a silent cancel, not a download error.
            if downloads.is_canceled(&track.id) {
                return Ok(DownloadOutcome::Canceled);
            }
            Err(format!(
                "Could not download \u{201c}{}\u{201d}: {error:#}",
                track.title
            ))
        }
    }
}

/// Base pause between consecutive failed retry passes for downloads that a
/// fully-downloaded ("kept offline") playlist depends on. Doubles per failed
/// pass, capped at [`PIN_RETRY_DELAY_MAX`]. Retries continue until
/// [`PIN_RETRY_MAX_PASSES`]; other failures are not retried at all.
const PIN_RETRY_DELAY: std::time::Duration = std::time::Duration::from_secs(30);
const PIN_RETRY_DELAY_MAX: std::time::Duration = std::time::Duration::from_secs(300);

/// How many retry passes a kept-offline download gets before Rift gives up and
/// marks it failed (surfaced in the UI for a manual retry). With the backoff
/// (0, 30s, 1m, 2m, 4m) this spans roughly 7–8 minutes of trying — long enough
/// to ride out a transient outage, short enough that a permanently-unavailable
/// track (removed/private/blocked) doesn't spin forever.
const PIN_RETRY_MAX_PASSES: u32 = 5;

/// Backoff after pass `attempt` (0-based). The first requeue runs immediately:
/// the rest of the list downloading after the failure already spaced it out,
/// and the user watching the list finish expects the stragglers to go next.
/// Only consecutive failed retry passes back off: 30s, 1m, 2m, then 4m (the cap).
fn pin_retry_delay(attempt: u32) -> std::time::Duration {
    match attempt {
        0 => std::time::Duration::ZERO,
        n => (PIN_RETRY_DELAY * 2u32.saturating_pow((n - 1).min(8))).min(PIN_RETRY_DELAY_MAX),
    }
}

/// Whether every track of `p` is downloaded (and it has at least one) — the
/// "kept fully offline" rule shared by the auto-download on add
/// ([`BackendHandle::add_to_playlist`]) and the batch-aware retry logic below.
pub(crate) fn playlist_fully_downloaded(
    p: &Playlist,
    is_downloaded: impl Fn(&str) -> bool,
) -> bool {
    !p.tracks.is_empty() && p.tracks.iter().all(|t| is_downloaded(&t.id))
}

/// Whether `id`'s playlist is being *kept* fully offline — i.e. it had an
/// established offline copy that the current download `batch` is only topping
/// up. A playlist qualifies when it contains `id` and every one of its tracks
/// that is NOT part of `batch` is already downloaded on disk, with at least one
/// such established track.
///
/// The out-of-batch requirement is what stops a fresh bulk download of a
/// never-downloaded playlist from retrying forever: there every track is in
/// `batch`, so no established-offline track remains and the playlist doesn't
/// qualify (a fully-failed first-time download just toasts and stops). A
/// genuine kept-offline playlist — one already downloaded, now re-fetching a
/// deleted track, or an auto-download after adding one song to a full playlist
/// — has all its other tracks on disk and so does qualify.
fn kept_offline(backend: &BackendInner, id: &str, batch: &[Track]) -> bool {
    let lib = backend.library.lock_safe();
    playlist_kept_offline(
        &lib.data.playlists,
        |tid| backend.downloads.is_downloaded(tid),
        id,
        |tid| batch.iter().any(|b| b.id == tid),
    )
}

/// Pure core of [`kept_offline`], split out so the "established offline" rule is
/// testable without a running app. Returns true when some playlist contains
/// `id` and every one of its tracks that is not `in_batch` is downloaded, with
/// at least one such established track.
fn playlist_kept_offline(
    playlists: &[Playlist],
    is_downloaded: impl Fn(&str) -> bool,
    id: &str,
    in_batch: impl Fn(&str) -> bool,
) -> bool {
    playlists.iter().any(|p| {
        if !p.tracks.iter().any(|t| t.id == id) {
            return false;
        }
        let mut established = false;
        for t in &p.tracks {
            if in_batch(&t.id) {
                continue;
            }
            if !is_downloaded(&t.id) {
                return false;
            }
            established = true;
        }
        established
    })
}

/// Split the failed batch into the tracks still worth retrying and the ids to
/// give up on.
///
/// Pure, so the requeue rule is testable without a running app. This is the
/// mechanism only — callers go through [`partition_and_release`], which acts on
/// the split, so the ids it returns can never be dropped on the floor.
fn partition_retryable(
    failed: Vec<Track>,
    is_kept_offline: impl Fn(&str) -> bool,
) -> (Vec<Track>, Vec<String>) {
    let mut retry = Vec::with_capacity(failed.len());
    let mut dropped = Vec::new();
    for t in failed {
        if is_kept_offline(&t.id) {
            retry.push(t);
        } else {
            dropped.push(t.id);
        }
    }
    (retry, dropped)
}

/// Split the failed batch, release the tracks that did not survive it, and hand
/// the resulting state to `announce` when anything was given up on.
///
/// The split, the release and the announcement are one decision, so they are one
/// function: a caller that can take `retry` without acting on `dropped` is the
/// defect itself. A dropped track is not merely un-queued — the failed pass
/// called `downloads.fail` on it, which releases the in-flight claim but leaves
/// it with no offline copy and no record of what happened. Its row would then
/// look like a track the user never downloaded, with no retry on offer, and
/// nothing would revisit it: the playlist was edited out from under it, which is
/// why it was dropped. Marking it failed puts the manual retry back on the row.
///
/// The announcement is a callback rather than the caller's job because a release
/// the UI is never told about is invisible: the row keeps offering nothing until
/// some later, unrelated emit happens to carry the state. Taking the store and a
/// closure (rather than the backend) is what lets a test drive both halves.
fn partition_and_release(
    downloads: &Downloads,
    failed: Vec<Track>,
    is_kept_offline: impl Fn(&str) -> bool,
    announce: impl FnOnce(&DownloadState),
) -> (Vec<Track>, Vec<String>) {
    let (retry, dropped) = partition_retryable(failed, is_kept_offline);
    for id in &dropped {
        downloads.mark_failed(id);
    }
    if !dropped.is_empty() {
        announce(&downloads.state());
    }
    (retry, dropped)
}

/// Spawn a background job that downloads `tracks`, skipping ones already on disk
/// or in flight. Shared by the explicit Download action and the automatic
/// "keep a fully-downloaded playlist offline" path.
pub(crate) fn start_downloads(backend: &Arc<BackendInner>, tracks: Vec<Track>) {
    // Preview mode must never touch the real offline store: the app renders
    // placeholder rows, and this is the single entry point for every download
    // (the explicit action and the "keep a playlist offline" auto-top-up), so
    // the guard lives here rather than only on `download_tracks`.
    if crate::settings::preview_mode() {
        return;
    }
    start_downloads_attempt(backend, tracks, 0);
}

/// One download pass. `attempt` counts consecutive retry passes for tracks a
/// kept-offline playlist depends on; those are requeued (with backoff) for as
/// long as the playlist stays marked as downloaded, instead of giving up.
fn start_downloads_attempt(backend: &Arc<BackendInner>, tracks: Vec<Track>, attempt: u32) {
    let downloads = Arc::clone(&backend.downloads);
    // `begin` atomically claims each id and returns false if one was already in
    // flight, so concurrent or repeated calls never fetch the same track twice.
    let pending: Vec<(Track, u64)> = tracks
        .into_iter()
        .filter_map(|t| {
            if downloads.is_downloaded(&t.id) || !downloads.begin(&t.id) {
                return None;
            }
            // The token identifies this claim: a later remove/retry invalidates
            // it so this worker's progress and publish become no-ops.
            downloads.claim_token(&t.id).map(|token| (t, token))
        })
        .collect();
    if pending.is_empty() {
        return;
    }
    emit_downloads(backend, &downloads.state());

    let runtime = backend.runtime.clone();
    let task_backend = Arc::clone(backend);
    runtime.spawn(async move {
        let backend = task_backend;
        let batch: Vec<Track> = pending.iter().map(|(track, _)| track.clone()).collect();
        let mut failed: Vec<Track> = Vec::new();
        // One-off failures are collected across the whole pass and toasted once
        // at the end; the per-track `mark_failed` below is what offers a retry.
        let mut one_off_failures = OneOffFailures::default();
        for (track, attempt) in pending {
            // A canceled track is a normal outcome: nothing to record, nothing
            // to toast, and the rest of the batch keeps downloading.
            if let Err(msg) = download_track(&backend, &downloads, &track, attempt).await {
                warn!("download failed for {}: {msg}", track.id);
                // A track a kept-offline playlist depends on gets requeued; one
                // coalesced notice is emitted after the pass (per-track toasts
                // would flood a large playlist). The retries stay quiet.
                if kept_offline(&backend, &track.id, &batch) {
                    downloads.fail_attempt(&track.id, attempt);
                    failed.push(track);
                } else {
                    // A one-off download that failed: surface it as failed so the
                    // row offers a retry, and remember the reason for the one
                    // summary notice emitted after the batch.
                    downloads.mark_failed_attempt(&track.id, attempt);
                    one_off_failures.record(msg);
                }
            }
            emit_downloads(&backend, &downloads.state());
        }

        // One notice per pass, not per failed track. A single failure keeps its
        // exact reason; several are summarized with the first reason. A mixed
        // pass can still emit this plus the kept-offline retry notice below:
        // they describe different classes of failure and different remedies.
        if let Some(notice) = one_off_failures.notice() {
            backend.emit(BackendEvent::Error(notice));
        }

        if failed.is_empty() {
            return;
        }
        // First pass: one coalesced notice that the stragglers are being retried
        // in the background (per-track toasts would flood a large playlist).
        if attempt == 0 {
            let noun = if failed.len() == 1 { "track" } else { "tracks" };
            backend.emit(BackendEvent::Error(format!(
                "Couldn\u{2019}t download {} {noun} \u{2014} retrying in the background.",
                failed.len()
            )));
        }
        // Give up after enough consecutive failed passes: mark the stragglers
        // failed (the UI then offers a manual retry) instead of retrying forever.
        if attempt >= PIN_RETRY_MAX_PASSES {
            for t in &failed {
                downloads.mark_failed(&t.id);
            }
            emit_downloads(&backend, &downloads.state());
            let titles = failed
                .iter()
                .map(|t| format!("\u{201c}{}\u{201d}", t.title))
                .collect::<Vec<_>>()
                .join(", ");
            let noun = if failed.len() == 1 { "track" } else { "tracks" };
            backend.emit(BackendEvent::Error(format!(
                "Gave up downloading {} {noun} after {} tries \u{2014} {titles} may be unavailable. Retry from a track's menu.",
                failed.len(),
                attempt + 1,
            )));
            return;
        }
        let delay = pin_retry_delay(attempt);
        info!(
            "requeueing {} failed kept-offline download(s) in {}s (pass {})",
            failed.len(),
            delay.as_secs(),
            attempt + 2
        );
        // Skip the wait and retry entirely when offline: a kept-offline playlist
        // whose tracks vanished (e.g. removed from YouTube) would otherwise spin
        // through ~7.5 minutes of backoff on a machine with no network.
        if !crate::util::is_online(&backend.player.http).await {
            warn!(
                "offline; giving up on {} kept-offline download(s)",
                failed.len()
            );
            for t in &failed {
                downloads.mark_failed(&t.id);
            }
            emit_downloads(&backend, &downloads.state());
            backend.emit(BackendEvent::Error(
                "Couldn't download \u{2014} you appear to be offline. Reconnect and retry from a track's menu.".to_string(),
            ));
            return;
        }
        if !delay.is_zero() {
            tokio::time::sleep(delay).await;
        }
        // The playlist may have been un-marked or edited during the wait; keep
        // only the tracks it still depends on, and give up on the rest.
        let batch = failed.clone();
        let (retry, dropped) = partition_and_release(
            &downloads,
            failed,
            |t| kept_offline(&backend, t, &batch),
            |state| emit_downloads(&backend, state),
        );
        if !dropped.is_empty() {
            info!(
                "gave up on {} kept-offline download(s) whose playlist no longer needs them",
                dropped.len()
            );
        }
        if retry.is_empty() {
            return;
        }
        start_downloads_attempt(&backend, retry, attempt + 1);
    });
}

#[cfg(test)]
mod tests {
    use super::{
        partition_and_release, partition_retryable, playlist_kept_offline, progress_emit_due,
        publish_part, DownloadOutcome, OneOffFailures, PROGRESS_EMIT_INTERVAL,
    };
    use crate::downloads::Downloads;
    use rift_types::{Playlist, Track};

    /// The UI redraws a single fraction from lightweight progress events, so
    /// the worker must emit the first callback immediately and then throttle to
    /// one event per 100 ms — not one per network chunk. Pure instants keep the
    /// test deterministic.
    #[test]
    fn progress_updates_are_emitted_immediately_then_throttled() {
        let start = std::time::Instant::now();
        assert!(
            progress_emit_due(None, start),
            "the first callback emits immediately so the bar appears"
        );
        assert!(
            !progress_emit_due(Some(start), start + PROGRESS_EMIT_INTERVAL / 2),
            "a snapshot inside the interval is throttled"
        );
        assert!(
            progress_emit_due(Some(start), start + PROGRESS_EMIT_INTERVAL),
            "the interval boundary is due"
        );
        assert!(
            progress_emit_due(Some(start), start + PROGRESS_EMIT_INTERVAL * 3),
            "a long gap is due"
        );
        // A clock that appears to move backwards (or the same instant) must not
        // panic or emit: `unchecked_duration_subtraction` is a warned lint and
        // the saturating form is the bug-free one.
        assert!(
            !progress_emit_due(Some(start + PROGRESS_EMIT_INTERVAL), start),
            "a backwards clock is treated as not due"
        );
    }

    /// The publication boundary is where a removal wins. A transfer can finish
    /// its bytes after `remove` ran (the worker cannot be interrupted once
    /// `stream::fetch_to_path` owns the part file); publishing it anyway would
    /// recreate a file the user deleted and a "downloaded" row they dismissed.
    #[tokio::test]
    async fn a_remove_during_the_transfer_discards_the_completed_part() {
        let dir = crate::util::testing::TempDir::new("remove-during-transfer");
        let downloads = Downloads::load(dir.path().to_path_buf());
        let t = track("a");
        let dest = downloads.path(&t.id);
        let part = downloads.part_path(&t.id);

        assert!(downloads.begin(&t.id));
        let attempt = downloads.claim_token(&t.id).expect("the claim has a token");
        std::fs::write(&part, b"complete audio").expect("write the in-flight part");
        // The user removes the row while the transfer is still running.
        assert!(downloads.remove(&t.id));
        assert!(!part.exists(), "remove cleans the in-flight part");
        // The worker's transfer finishes regardless (remove only unlinked the
        // file; the open handle kept writing on Unix) and reaches publication.
        std::fs::write(&part, b"complete audio").expect("the transfer rewrote the part");

        let outcome = publish_part(&downloads, &t, attempt, &part, &dest)
            .await
            .expect("a cancellation is not a save error");
        assert_eq!(outcome, DownloadOutcome::Canceled);
        assert!(
            !dest.exists(),
            "a canceled transfer must not be published under the track's stem"
        );
        assert!(!part.exists(), "the canceled transfer's part is removed");
        let state = downloads.state();
        assert!(
            !state.downloaded.contains("a"),
            "the tombstone keeps a stale finish out of the snapshot"
        );
        assert!(!state.downloading.contains("a"));
        assert!(!state.progress.contains_key("a"));
        assert!(downloads.is_canceled("a"));
    }

    /// Regression for the remove-then-retry race: the abandoned worker and the
    /// retry must never share a `.part` path, and the stale worker must not be
    /// able to publish (or mark finished) over the retry's transfer. Before the
    /// per-claim token/part path, the stale worker could rename the retry's
    /// partially written file as a complete download.
    #[tokio::test]
    async fn a_stale_worker_cannot_publish_over_a_retry() {
        let dir = crate::util::testing::TempDir::new("stale-publish");
        let downloads = Downloads::load(dir.path().to_path_buf());
        let t = track("a");
        let dest = downloads.path(&t.id);

        assert!(downloads.begin(&t.id));
        let first = downloads.claim_token(&t.id).expect("first token");
        let first_part = downloads.part_path_for(&t.id, first);
        std::fs::write(&first_part, b"first attempt").expect("write the first part");

        // The user removes and immediately re-downloads the same id.
        assert!(downloads.remove(&t.id));
        assert!(downloads.begin(&t.id));
        let second = downloads.claim_token(&t.id).expect("retry token");
        assert_ne!(first, second, "a retry must get a fresh claim token");
        assert!(
            !downloads.is_current_attempt(&t.id, first),
            "the abandoned worker's token is dead"
        );
        let retry_part = downloads.part_path_for(&t.id, second);
        assert_ne!(
            first_part, retry_part,
            "attempts must not share one .part path"
        );
        std::fs::write(&retry_part, b"retry partial").expect("write the retry part");

        // The stale worker reaches publication after the retry started writing.
        let outcome = publish_part(&downloads, &t, first, &first_part, &dest)
            .await
            .expect("a superseded publish is a cancellation, not an error");
        assert_eq!(outcome, DownloadOutcome::Canceled);
        assert!(!dest.exists(), "stale bytes must not be published");
        assert_eq!(
            std::fs::read(&retry_part).expect("the retry part is untouched"),
            b"retry partial",
            "the stale worker must not delete or truncate the retry's part"
        );
        assert!(
            downloads.state().downloading.contains("a"),
            "the retry is still in flight"
        );

        // The retry publishes normally afterwards.
        let outcome = publish_part(&downloads, &t, second, &retry_part, &dest)
            .await
            .expect("the retry publishes");
        assert_eq!(outcome, DownloadOutcome::Published);
        assert_eq!(
            std::fs::read(&dest).expect("read the file"),
            b"retry partial"
        );
        assert!(!retry_part.exists());
        assert!(downloads.is_downloaded("a"));
    }

    /// The normal path stays intact: a completed part is renamed into place and
    /// recorded as downloaded (the transfer itself is only covered live, but the
    /// publish half runs here against a real file and a real store).
    #[tokio::test]
    async fn a_completed_part_is_published_and_recorded() {
        let dir = crate::util::testing::TempDir::new("publish-part");
        let downloads = Downloads::load(dir.path().to_path_buf());
        let t = track("a");
        let dest = downloads.path(&t.id);
        let part = downloads.part_path(&t.id);

        assert!(downloads.begin(&t.id));
        let attempt = downloads.claim_token(&t.id).expect("the claim has a token");
        downloads.set_progress(&t.id, 900, 1000);
        std::fs::write(&part, b"complete audio").expect("write the completed part");

        let outcome = publish_part(&downloads, &t, attempt, &part, &dest)
            .await
            .expect("publishing a completed part succeeds");
        assert_eq!(outcome, DownloadOutcome::Published);
        assert_eq!(
            std::fs::read(&dest).expect("read the published file"),
            b"complete audio"
        );
        assert!(!part.exists(), "the part is moved, not copied");
        assert!(downloads.is_downloaded("a"));
        assert!(!downloads.is_downloading("a"));
        assert!(downloads.state().progress.is_empty());
    }

    /// A capped id's filename cannot be decoded, so the publish boundary must
    /// put it in the name index *before* the rename; otherwise the next startup
    /// would see the file but be unable to map it back to the id.
    #[tokio::test]
    async fn publishing_a_capped_id_records_the_name_index_for_the_next_load() {
        let dir = crate::util::testing::TempDir::new("publish-capped-index");
        let downloads = Downloads::load(dir.path().to_path_buf());
        let id = "z".repeat(400);
        let t = track(&id);
        let dest = downloads.path(&t.id);
        let part = downloads.part_path(&t.id);

        assert!(downloads.begin(&t.id));
        let attempt = downloads.claim_token(&t.id).expect("the claim has a token");
        std::fs::write(&part, b"complete audio").expect("write the completed part");

        let outcome = publish_part(&downloads, &t, attempt, &part, &dest)
            .await
            .expect("publishing a capped name succeeds");
        assert_eq!(outcome, DownloadOutcome::Published);
        assert!(dir.path().join(".names.json").exists());

        let reloaded = Downloads::load(dir.path().to_path_buf());
        assert!(reloaded.is_downloaded(&id));
        assert!(reloaded.path(&id).exists());
    }

    /// A pre-P5 file under the old name *is* the offline copy. Publishing a
    /// second file under the current name would leave two files for one id, so
    /// the publication boundary refuses and keeps the legacy copy untouched.
    #[tokio::test]
    async fn publishing_refuses_to_create_a_second_name_for_a_legacy_copy() {
        let dir = crate::util::testing::TempDir::new("publish-legacy-exists");
        let downloads = Downloads::load(dir.path().to_path_buf());
        let id = "dQw4w9WgXcQ";
        let t = track(id);
        let legacy = downloads
            .legacy_path(id)
            .expect("the old name differs from the current one");
        std::fs::write(&legacy, b"legacy audio").expect("write the legacy copy");
        let dest = downloads.path(id);
        let part = downloads.part_path(id);
        std::fs::write(&part, b"new audio").expect("write the completed part");
        assert!(downloads.begin(id));
        let attempt = downloads.claim_token(id).expect("the claim has a token");

        let error = publish_part(&downloads, &t, attempt, &part, &dest)
            .await
            .expect_err("a legacy copy must not be duplicated");
        assert!(
            error.contains("previous name"),
            "the error names the existing copy: {error}"
        );
        assert!(!dest.exists(), "no second file is published");
        assert_eq!(
            std::fs::read(&legacy).expect("read the legacy copy"),
            b"legacy audio"
        );
        assert!(!part.exists(), "the refused transfer's scratch is removed");
    }

    /// A `_` stem owned by a *different* id is not this id's legacy copy. A
    /// download must not be refused (forever) just because another track's owned
    /// file shares this id's pre-P5 fold.
    #[tokio::test]
    async fn an_owned_legacy_name_for_another_id_does_not_block_a_download() {
        let dir = crate::util::testing::TempDir::new("publish-owned-legacy");
        let downloads = Downloads::load(dir.path().to_path_buf());
        // `a_b` owns its own `_` stem (recorded in the name index).
        assert!(downloads.record_name("a_b"));
        let other = downloads.path("a_b");
        std::fs::write(&other, b"other track audio").expect("write the other track's file");

        let id = "a/b";
        let t = track(id);
        let dest = downloads.path(id);
        let legacy = downloads.legacy_path(id).expect("old name differs");
        assert_eq!(legacy, other, "the fold of a/b is a_b");
        let part = downloads.part_path(id);
        std::fs::write(&part, b"new audio").expect("write the completed part");
        assert!(downloads.begin(id));
        let attempt = downloads.claim_token(id).expect("the claim has a token");

        let outcome = publish_part(&downloads, &t, attempt, &part, &dest)
            .await
            .expect("another id's owned file must not block this publish");
        assert_eq!(outcome, DownloadOutcome::Published);
        assert_eq!(
            std::fs::read(&dest).expect("read the new file"),
            b"new audio"
        );
        assert_eq!(
            std::fs::read(&other).expect("the other track's file is untouched"),
            b"other track audio"
        );
    }

    /// A publish must never replace a non-empty offline file it did not create:
    /// the only way one is present although `is_downloaded` was false is a
    /// legacy name that differs only by case (another track's audio) or a lost
    /// index, and replacing it would destroy data Rift cannot fetch back.
    #[tokio::test]
    async fn publishing_never_overwrites_an_existing_non_empty_file() {
        let dir = crate::util::testing::TempDir::new("publish-no-overwrite");
        let downloads = Downloads::load(dir.path().to_path_buf());
        let t = track("a");
        let dest = downloads.path(&t.id);
        let part = downloads.part_path(&t.id);
        std::fs::write(&dest, b"existing audio").expect("write the existing copy");
        std::fs::write(&part, b"new audio").expect("write the completed part");
        assert!(downloads.begin(&t.id));
        let attempt = downloads.claim_token(&t.id).expect("the claim has a token");

        let error = publish_part(&downloads, &t, attempt, &part, &dest)
            .await
            .expect_err("an existing non-empty file must not be replaced");
        assert!(
            error.contains("already exists"),
            "the error explains the refusal: {error}"
        );
        assert_eq!(
            std::fs::read(&dest).expect("read the existing copy"),
            b"existing audio"
        );
        assert!(
            !part.exists(),
            "the failed transfer's scratch file is removed"
        );
        assert!(
            !downloads.is_downloaded(&t.id),
            "a refused publish must not mark the track downloaded"
        );
    }

    fn track(id: &str) -> Track {
        Track {
            id: id.into(),
            title: String::new(),
            artist: String::new(),
            album: None,
            duration: None,
            cover: String::new(),
            artists: Vec::new(),
            album_id: None,
        }
    }

    fn playlist(ids: &[&str]) -> Playlist {
        Playlist {
            id: "pl".into(),
            name: "pl".into(),
            tracks: ids.iter().map(|i| track(i)).collect(),
        }
    }

    /// The coalescing rule: exactly one failed one-off track keeps its own
    /// specific text (a count-only summary would be strictly less useful), and
    /// zero failures produce no notice at all.
    #[test]
    fn a_single_one_off_failure_keeps_its_specific_reason() {
        let mut failures = OneOffFailures::default();
        assert_eq!(failures.notice(), None, "a clean pass must not toast");

        failures.record("Could not download \u{201c}A\u{201d}: 403".into());
        assert_eq!(
            failures.notice().as_deref(),
            Some("Could not download \u{201c}A\u{201d}: 403"),
            "one failure must not be collapsed into a vague summary"
        );
    }

    /// Several one-off failures in one pass are one notice: count + the first
    /// reason. The later per-track reasons are deliberately not retained (the
    /// per-row `failed` marks are the retry mechanism), which also keeps the
    /// collector's memory bounded by one message.
    #[test]
    fn a_failed_one_off_batch_is_coalesced_into_one_summary() {
        let mut failures = OneOffFailures::default();
        failures.record("first reason".into());
        failures.record("second reason".into());
        failures.record("third reason".into());

        let notice = failures
            .notice()
            .expect("a failed pass produces one summary");
        assert!(
            notice.contains('3'),
            "the summary names the count: {notice}"
        );
        assert!(
            notice.contains("first reason"),
            "the summary keeps a representative reason: {notice}"
        );
        assert!(
            !notice.contains("second reason") && !notice.contains("third reason"),
            "later reasons are not accumulated: {notice}"
        );
    }

    #[test]
    fn fresh_full_download_that_fully_fails_is_not_kept_offline() {
        // A never-downloaded playlist the user just clicked Download on: the
        // whole playlist is the batch and nothing is on disk. Must NOT qualify,
        // or a total failure would retry forever.
        let pls = vec![playlist(&["a", "b", "c"])];
        let downloaded = |_: &str| false;
        let batch: std::collections::HashSet<&str> = ["a", "b", "c"].into_iter().collect();
        let in_batch = |t: &str| batch.contains(t);
        assert!(!playlist_kept_offline(&pls, downloaded, "a", in_batch));
    }

    #[test]
    fn topping_up_a_fully_downloaded_playlist_is_kept_offline() {
        // Playlist already fully offline; one track ("c") is being re-fetched
        // (e.g. it was deleted). The others are on disk, so it qualifies.
        let pls = vec![playlist(&["a", "b", "c"])];
        let downloaded = |t: &str| t == "a" || t == "b";
        let in_batch = |t: &str| t == "c";
        assert!(playlist_kept_offline(&pls, downloaded, "c", in_batch));
    }

    #[test]
    fn partially_downloaded_playlist_is_not_kept_offline() {
        // "a" on disk, "b" missing and not in the batch → the playlist was
        // never fully offline, so a failure of "c" must not retry forever.
        let pls = vec![playlist(&["a", "b", "c"])];
        let downloaded = |t: &str| t == "a";
        let in_batch = |t: &str| t == "c";
        assert!(!playlist_kept_offline(&pls, downloaded, "c", in_batch));
    }

    #[test]
    fn track_in_no_kept_offline_playlist_is_not_kept_offline() {
        let pls = vec![playlist(&["a", "b"])];
        let downloaded = |_: &str| true;
        let in_batch = |t: &str| t == "z";
        assert!(!playlist_kept_offline(&pls, downloaded, "z", in_batch));
    }

    /// The regression guard for the dropped half of the split: the ids that did
    /// not survive the requeue rule must come *back* to the caller rather than
    /// being filtered away, because the release and the failure record both
    /// happen outside this function. There is no claim left to release —
    /// `downloads.fail` removed it already — so what is missing without the
    /// returned ids is the record that lets the row offer a retry at all.
    /// [`partition_and_release`] is the caller that acts on them, and the test
    /// below drives it through a real [`Downloads`].
    #[test]
    fn a_track_dropped_from_the_retry_set_is_reported_rather_than_filtered_away() {
        let failed = vec![track("keep"), track("drop")];
        let (retry, dropped) = partition_retryable(failed, |id| id == "keep");

        assert_eq!(
            retry.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(),
            ["keep"],
            "the still-kept-offline track is requeued"
        );
        assert_eq!(
            dropped,
            ["drop"],
            "the abandoned track must be handed back, not silently filtered away"
        );
    }

    #[test]
    fn partition_retryable_keeps_everything_when_nothing_was_dropped() {
        let failed = vec![track("a"), track("b")];
        let (retry, dropped) = partition_retryable(failed, |_| true);
        assert_eq!(retry.len(), 2);
        assert!(dropped.is_empty(), "nothing to give up on");
    }

    /// The other half of M3, and the one the split tests above cannot see: the
    /// tracks the split gives up on have to be *recorded as failed* in the store
    /// and *announced* to the UI. The in-flight claim was already released by
    /// `fail`, so without the record the row is indistinguishable from one the
    /// user never downloaded; without the announcement the row keeps looking
    /// that way until some later, unrelated emit happens to carry the state.
    /// Deleting either used to leave the whole suite green, because the tests
    /// stopped at `partition_retryable` and never touched the store or the
    /// callback — so this drives the call path the job actually uses, through a
    /// real [`Downloads`].
    ///
    /// The state is the exact one the retry pass leaves a track in: claimed, then
    /// released by `fail` for a requeue that the playlist edit then cancelled.
    #[test]
    fn a_track_dropped_from_the_retry_set_is_recorded_and_announced() {
        let dir = crate::util::testing::TempDir::new("dropped-claim");
        let downloads = Downloads::load(dir.path().to_path_buf());
        for id in ["kept", "drop"] {
            assert!(downloads.begin(id));
            downloads.fail(id);
        }

        let mut announced = false;
        let (retry, dropped) = partition_and_release(
            &downloads,
            vec![track("kept"), track("drop")],
            |id| id == "kept",
            |state| {
                announced = true;
                assert!(
                    !state.downloading.iter().any(|id| id == "drop"),
                    "no in-flight claim may be left behind"
                );
                assert!(
                    state.failed.iter().any(|id| id == "drop"),
                    "the row must offer a manual retry, not look like a track that was never \
                     downloaded"
                );
            },
        );

        assert_eq!(
            retry.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(),
            ["kept"],
            "the still-kept-offline track is requeued"
        );
        assert_eq!(dropped, ["drop"]);
        assert!(
            announced,
            "the give-up has to reach the UI, or the retry affordance never appears"
        );

        assert!(
            downloads.begin("drop"),
            "the announced retry must be able to claim it again"
        );
        assert!(
            !downloads.state().failed.iter().any(|id| id == "drop"),
            "re-attempting clears the failed mark"
        );

        // Nothing dropped means nothing changed for the UI to hear about.
        let mut announced_again = false;
        let (_, nothing) = partition_and_release(
            &downloads,
            vec![track("kept")],
            |_| true,
            |_| announced_again = true,
        );
        assert!(nothing.is_empty());
        assert!(
            !announced_again,
            "an unchanged state must not be re-emitted"
        );

        // The kept track is untouched: the requeue below re-claims it.
        assert!(downloads.begin("kept"));
    }
}
