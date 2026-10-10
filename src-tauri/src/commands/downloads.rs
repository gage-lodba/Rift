//! Offline-download commands and the background download job, including the
//! retry/backoff logic that keeps a fully-downloaded playlist offline.

use rift_types::{events, DownloadState, Playlist, Track};
use tauri::{AppHandle, Emitter, Manager, State};
use tracing::{info, warn};

use crate::downloads::Downloads;
use crate::util::LockExt;
use crate::AppState;

fn emit_downloads(app: &AppHandle, state: &DownloadState) {
    let _ = app.emit(events::DOWNLOADS, state);
}

/// Download a set of tracks for offline listening. Already-downloaded and
/// already-in-flight tracks are skipped; the rest are fetched sequentially with
/// progress emitted after each one.
#[tauri::command(rename_all = "snake_case")]
pub fn download_tracks(tracks: Vec<Track>, app: AppHandle, state: State<'_, AppState>) {
    if crate::settings::preview_mode() {
        return;
    }
    start_downloads(tracks, app, state.downloads.clone(), state.player.clone());
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
/// Only consecutive failed retry passes back off: 30s, 1m, 2m, 4m, then 5m.
fn pin_retry_delay(attempt: u32) -> std::time::Duration {
    match attempt {
        0 => std::time::Duration::ZERO,
        n => (PIN_RETRY_DELAY * 2u32.saturating_pow((n - 1).min(8))).min(PIN_RETRY_DELAY_MAX),
    }
}

/// Whether every track of `p` is downloaded (and it has at least one) — the
/// "kept fully offline" rule shared by the auto-download on add
/// ([`crate::commands::library::add_to_playlist`]) and the batch-aware retry
/// logic below.
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
fn kept_offline(app: &AppHandle, id: &str, batch: &[Track]) -> bool {
    let state = app.state::<AppState>();
    let lib = state.library.lock_safe();
    playlist_kept_offline(
        &lib.data.playlists,
        |tid| state.downloads.is_downloaded(tid),
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
/// closure (rather than the `AppHandle`) is what lets a test drive both halves.
fn partition_and_release(
    downloads: &Downloads,
    failed: Vec<Track>,
    is_kept_offline: impl Fn(&str) -> bool,
    announce: impl FnOnce(&rift_types::DownloadState),
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
pub(crate) fn start_downloads(
    tracks: Vec<Track>,
    app: AppHandle,
    downloads: std::sync::Arc<crate::downloads::Downloads>,
    player: std::sync::Arc<crate::player::PlayerShared>,
) {
    start_downloads_attempt(tracks, app, downloads, player, 0);
}

/// One download pass. `attempt` counts consecutive retry passes for tracks a
/// kept-offline playlist depends on; those are requeued (with backoff) for as
/// long as the playlist stays marked as downloaded, instead of giving up.
fn start_downloads_attempt(
    tracks: Vec<Track>,
    app: AppHandle,
    downloads: std::sync::Arc<crate::downloads::Downloads>,
    player: std::sync::Arc<crate::player::PlayerShared>,
    attempt: u32,
) {
    // `begin` atomically claims each id and returns false if one was already in
    // flight, so concurrent or repeated calls never fetch the same track twice.
    let pending: Vec<Track> = tracks
        .into_iter()
        .filter(|t| !downloads.is_downloaded(&t.id) && downloads.begin(&t.id))
        .collect();
    if pending.is_empty() {
        return;
    }
    emit_downloads(&app, &downloads.state());

    tauri::async_runtime::spawn(async move {
        let batch = pending.clone();
        let mut failed: Vec<Track> = Vec::new();
        for track in pending {
            let dest = downloads.path(&track.id);
            let err = match rift::fetch::fetch_bytes(&track.id).await {
                Ok((data, _)) => {
                    // Temp file + rename: a crash mid-write must not leave a
                    // truncated .m4a, which the next launch's directory scan
                    // would treat as a valid offline copy.
                    let tmp = dest.with_extension("m4a.part");
                    let write = async {
                        tokio::fs::write(&tmp, &data).await?;
                        tokio::fs::rename(&tmp, &dest).await
                    };
                    match write.await {
                        Ok(()) => {
                            downloads.finish(&track.id);
                            None
                        }
                        Err(e) => Some(format!(
                            "Could not save \u{201c}{}\u{201d}: {e}",
                            track.title
                        )),
                    }
                }
                Err(e) => Some(format!(
                    "Could not download \u{201c}{}\u{201d}: {e:#}",
                    track.title
                )),
            };
            if let Some(msg) = err {
                warn!("download failed for {}: {msg}", track.id);
                // A track a kept-offline playlist depends on gets requeued; one
                // coalesced notice is emitted after the pass (per-track toasts
                // would flood a large playlist). The retries stay quiet.
                if kept_offline(&app, &track.id, &batch) {
                    downloads.fail(&track.id);
                    failed.push(track);
                } else {
                    // A one-off download that failed: surface it as failed so the
                    // row offers a retry, and toast the reason.
                    downloads.mark_failed(&track.id);
                    let _ = app.emit(events::ERROR, msg);
                }
            }
            emit_downloads(&app, &downloads.state());
        }

        if failed.is_empty() {
            return;
        }
        // First pass: one coalesced notice that the stragglers are being retried
        // in the background (per-track toasts would flood a large playlist).
        if attempt == 0 {
            let noun = if failed.len() == 1 { "track" } else { "tracks" };
            let _ = app.emit(
                events::ERROR,
                format!(
                    "Couldn\u{2019}t download {} {noun} \u{2014} retrying in the background.",
                    failed.len()
                ),
            );
        }
        // Give up after enough consecutive failed passes: mark the stragglers
        // failed (the UI then offers a manual retry) instead of retrying forever.
        if attempt >= PIN_RETRY_MAX_PASSES {
            for t in &failed {
                downloads.mark_failed(&t.id);
            }
            emit_downloads(&app, &downloads.state());
            let titles = failed
                .iter()
                .map(|t| format!("\u{201c}{}\u{201d}", t.title))
                .collect::<Vec<_>>()
                .join(", ");
            let noun = if failed.len() == 1 { "track" } else { "tracks" };
            let _ = app.emit(
                events::ERROR,
                format!(
                    "Gave up downloading {} {noun} after {} tries \u{2014} {titles} may be unavailable. Retry from a track's menu.",
                    failed.len(),
                    attempt + 1,
                ),
            );
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
        if !crate::util::is_online(&player.http).await {
            warn!(
                "offline; giving up on {} kept-offline download(s)",
                failed.len()
            );
            for t in &failed {
                downloads.mark_failed(&t.id);
            }
            emit_downloads(&app, &downloads.state());
            let _ = app.emit(
                events::ERROR,
                "Couldn't download — you appear to be offline. Reconnect and retry from a track's menu.",
            );
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
            |t| kept_offline(&app, t, &batch),
            |state| emit_downloads(&app, state),
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
        start_downloads_attempt(retry, app, downloads, player, attempt + 1);
    });
}

/// Remove offline copies of the given tracks.
#[tauri::command(rename_all = "snake_case")]
pub fn remove_downloads(ids: Vec<String>, app: AppHandle, state: State<'_, AppState>) {
    for id in &ids {
        state.downloads.remove(id);
    }
    emit_downloads(&app, &state.downloads.state());
}

#[cfg(test)]
mod tests {
    use super::{partition_and_release, partition_retryable, playlist_kept_offline};
    use crate::downloads::Downloads;
    use rift_types::{Playlist, Track};

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
