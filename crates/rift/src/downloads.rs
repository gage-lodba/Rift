//! Offline download tracking. Audio files live as `<id>.m4a` under the
//! downloads directory; the on-disk presence of a file is the source of
//! truth for "downloaded". A separate set tracks in-flight downloads so the
//! UI can show progress.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Mutex;

use rift_types::DownloadState;
use tracing::{info, warn};

use crate::util::LockExt;

pub struct Downloads {
    pub dir: PathBuf,
    downloaded: Mutex<HashSet<String>>,
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
        let mut downloaded = HashSet::new();
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                match path.extension().and_then(|e| e.to_str()) {
                    Some("m4a") => {
                        if let Some(id) = path.file_stem().and_then(|s| s.to_str()) {
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
                            } else {
                                downloaded.insert(id.to_string());
                            }
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
            progress: Mutex::new(HashMap::new()),
            failed: Mutex::new(HashSet::new()),
        }
    }

    pub fn path(&self, id: &str) -> PathBuf {
        self.dir
            .join(format!("{}.m4a", crate::fetch::safe_stem(id)))
    }

    pub fn is_downloaded(&self, id: &str) -> bool {
        self.downloaded.lock_safe().contains(id)
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

    /// Record the byte progress of the active transfer for `id`.
    ///
    /// Clamped into `0..=1000`; a `total` of 0 means the stream length is
    /// unknown, which removes the entry so the UI falls back to its
    /// indeterminate spinner. Ignored unless `id` is currently claimed by
    /// [`begin`](Self::begin), so no snapshot can report a fraction for a track
    /// that is not downloading.
    pub fn set_progress(&self, id: &str, received: u64, total: u64) {
        // Lock order (downloading → progress) is the one every multi-lock
        // method here follows; see [`state`](Self::state).
        let downloading = self.downloading.lock_safe();
        if !downloading.contains(id) {
            return;
        }
        let mut progress = self.progress.lock_safe();
        match progress_permille(received, total) {
            Some(permille) => {
                progress.insert(id.to_string(), permille);
            }
            None => {
                progress.remove(id);
            }
        }
    }

    /// Claim a download. Returns `true` if it was newly started, `false` if one
    /// was already in flight — this atomic check-and-set is what prevents the
    /// same track being fetched twice concurrently. Starting (or restarting) a
    /// download clears any prior "failed" mark and any stale progress fraction.
    ///
    /// The sets are updated under all guards because this is one logical
    /// transition ("a retry clears the failure mark *and* claims the track");
    /// the order (downloading, progress, failed) matches
    /// [`state`](Self::state)'s, so a snapshot can never observe the
    /// half-transition. A duplicate claim leaves an active transfer's progress
    /// alone — it is the same download, not a restart.
    pub fn begin(&self, id: &str) -> bool {
        let mut downloading = self.downloading.lock_safe();
        let claimed = downloading.insert(id.to_string());
        if claimed {
            self.progress.lock_safe().remove(id);
        }
        self.failed.lock_safe().remove(id);
        claimed
    }

    /// Move a finished download from in-flight to downloaded, clearing every
    /// other status for it in the same snapshot-visible step.
    pub fn finish(&self, id: &str) {
        let mut downloaded = self.downloaded.lock_safe();
        let mut downloading = self.downloading.lock_safe();
        let mut progress = self.progress.lock_safe();
        let mut failed = self.failed.lock_safe();
        downloading.remove(id);
        progress.remove(id);
        failed.remove(id);
        downloaded.insert(id.to_string());
    }

    /// Release an in-flight claim after a failed pass, without marking the track
    /// as permanently failed (a retry may still be scheduled).
    pub fn fail(&self, id: &str) {
        let mut downloading = self.downloading.lock_safe();
        self.progress.lock_safe().remove(id);
        downloading.remove(id);
    }

    /// Give up on a track after repeated failures: drop any in-flight claim and
    /// record it as failed so the UI can offer a manual retry.
    pub fn mark_failed(&self, id: &str) {
        let mut downloading = self.downloading.lock_safe();
        self.progress.lock_safe().remove(id);
        let mut failed = self.failed.lock_safe();
        downloading.remove(id);
        failed.insert(id.to_string());
    }

    /// Delete a downloaded file. Returns true if it existed.
    pub fn remove(&self, id: &str) -> bool {
        {
            let mut downloaded = self.downloaded.lock_safe();
            self.progress.lock_safe().remove(id);
            self.failed.lock_safe().remove(id);
            downloaded.remove(id);
        }
        match std::fs::remove_file(self.path(id)) {
            Ok(()) => true,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
            Err(e) => {
                warn!("could not delete download {id}: {e}");
                false
            }
        }
    }

    /// A snapshot of all status sets and the active transfers' progress.
    ///
    /// The sets are locked together in a fixed order (downloaded → downloading
    /// → progress → failed, which every multi-lock method here follows). Locking
    /// them one at a time — what this used to do — let a track appear in
    /// `downloading` and `downloaded` in the same snapshot while a download
    /// finished, so the UI could briefly show two contradictory states for one
    /// row. The progress map is taken between `downloading` and `failed` so a
    /// snapshot can never contain a fraction for a track that is no longer
    /// downloading.
    pub fn state(&self) -> DownloadState {
        let downloaded = self.downloaded.lock_safe();
        let downloading = self.downloading.lock_safe();
        let progress = self.progress.lock_safe();
        let failed = self.failed.lock_safe();
        DownloadState {
            downloaded: downloaded.iter().cloned().collect(),
            downloading: downloading.iter().cloned().collect(),
            failed: failed.iter().cloned().collect(),
            progress: progress.clone(),
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
        d.finish("a");
        assert!(d.is_downloaded("a"));

        assert!(d.remove("a"), "removing an existing download reports true");
        assert!(!d.is_downloaded("a"));
        assert!(!d.path("a").exists());
        assert!(!d.remove("a"), "removing a missing download reports false");
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

        // Not in flight: ignored, so a queued track never gets a bar.
        d.set_progress("a", 250, 1000);
        assert!(!d.state().progress.contains_key("a"));

        assert!(d.begin("a"));
        d.set_progress("a", 250, 1000);
        assert_eq!(d.state().progress.get("a"), Some(&250));

        // Finished: the fraction is gone even though the track is now
        // downloaded (a determinate bar is only for in-flight transfers).
        d.finish("a");
        let state = d.state();
        assert!(state.downloaded.contains("a"));
        assert!(!state.progress.contains_key("a"));

        // Setting it after the claim was released is ignored.
        d.set_progress("a", 500, 1000);
        assert!(!d.state().progress.contains_key("a"));
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
}
