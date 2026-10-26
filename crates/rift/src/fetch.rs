//! Playback audio fetching: resolution and streaming are native.
//!
//! Rift resolves a `YouTube` video ID to a directly downloadable audio URL with
//! the JS-less `VISIONOS` innertube client and streams it over ranged HTTP (the
//! crate-private `stream` module). There is no external downloader binary and no
//! fallback pipeline: the pre-native implementation shelled out to yt-dlp, and it
//! was removed once the native path shipped (`docs/native-streaming.md`, Phase 5).
//!
//! [`fetch_track`] is the playback entry point: it prefers an offline copy under
//! the downloads directory and otherwise fetches the stream over the network
//! through [`fetch_bytes`].

use anyhow::{bail, Context};
use tracing::{debug, info};

/// Sanitize a video/track id into a safe single filename component. Real
/// `YouTube` ids are `[A-Za-z0-9_-]` and pass through unchanged; a playlist
/// imported from an arbitrary JSON file can carry any string, so everything
/// else (path separators, `..`, whitespace) folds to `_`. This keeps a
/// malicious id from escaping the downloads/temp directory when it is used to
/// build a file path.
#[must_use]
pub fn safe_stem(id: &str) -> String {
    let cleaned: String = id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if cleaned.is_empty() {
        "_".into()
    } else {
        cleaned
    }
}

/// Load a track for playback, preferring an offline copy under `downloads_dir`
/// and otherwise fetching it natively over the network. Returns the audio bytes
/// and duration in seconds (0.0 if unknown — the decoder reports the real
/// length).
///
/// # Errors
///
/// Returns the native resolution/download failure when there is no usable
/// offline copy: the resolver reports the video unavailable or offers no
/// directly downloadable audio format, a network request fails, or the transfer
/// cannot be completed. The error names that single cause — there is no fallback
/// backend whose failure could mask it. A local file that cannot be read is
/// treated as "not downloaded" and falls back to the network.
pub async fn fetch_track(
    downloads_dir: &std::path::Path,
    video_id: &str,
) -> anyhow::Result<(Vec<u8>, f64)> {
    let local = downloads_dir.join(format!("{}.m4a", safe_stem(video_id)));
    if local.exists() {
        if let Ok(data) = tokio::fs::read(&local).await {
            // An empty file is not an offline copy (it can only come from a
            // failed/aborted write that predates the temp-file-then-rename
            // path). Fall through to the network instead of handing the decoder
            // zero bytes.
            if !data.is_empty() {
                debug!("playing {video_id} from offline download");
                return Ok((data, 0.0));
            }
            debug!(
                "ignoring the empty offline file {}; fetching from the network",
                local.display()
            );
        }
    }
    fetch_bytes(video_id).await
}

/// Resolve a `YouTube` video ID to its audio stream and download it fully into
/// memory over the native ranged-HTTP pipeline.
///
/// Returns the audio data and duration in seconds (0.0 if unknown — the stream
/// is fetched as audio only, so the decoder reports the real length).
///
/// # Errors
///
/// Errors when the native resolver cannot produce a direct audio URL for
/// `video_id` (unavailable or removed video, a response with no directly
/// downloadable audio format, or a failure while resolving), or when the ranged
/// download fails (an HTTP error, a URL that is still rejected after one
/// re-resolve, a broken body, or a stream that does not match its reported
/// length).
pub async fn fetch_bytes(video_id: &str) -> anyhow::Result<(Vec<u8>, f64)> {
    let data = crate::stream::fetch_bytes(video_id)
        .await
        .with_context(|| format!("could not fetch the native audio stream for {video_id}"))?;
    // `stream` already rejects an empty stream; this guard keeps a future
    // regression there from serving silence as playback.
    if data.is_empty() {
        bail!("the native audio stream for {video_id} was empty");
    }
    info!("{video_id}: native stream served playback");
    Ok((data, 0.0))
}

#[cfg(test)]
mod tests {
    use super::{fetch_track, safe_stem};
    use crate::util::testing::TempDir;

    #[test]
    fn safe_stem_preserves_real_ids_and_neutralizes_traversal() {
        // Real YouTube ids are already a safe single component and pass through.
        assert_eq!(safe_stem("dQw4w9WgXcQ"), "dQw4w9WgXcQ");
        assert_eq!(safe_stem("a-b_c"), "a-b_c");
        // Separators, `..`, and whitespace fold to `_` instead of escaping.
        assert_eq!(safe_stem(".."), "__");
        assert_eq!(safe_stem("../etc/passwd"), "___etc_passwd");
        // An empty id still yields a safe name.
        assert_eq!(safe_stem(""), "_");
    }

    /// The playback promise for an offline track: the local copy is served
    /// without any network access. The fixture is the only possible source of
    /// these bytes, so this cannot pass by fetching them.
    #[tokio::test]
    async fn fetch_track_serves_an_offline_copy_before_touching_the_network() {
        let dir = TempDir::new("fetch-offline-first");
        let bytes = b"offline audio bytes".to_vec();
        std::fs::write(dir.path().join("track-id.m4a"), &bytes).expect("write the offline copy");

        let (data, duration) = fetch_track(dir.path(), "track-id")
            .await
            .expect("an offline copy needs no network");
        assert_eq!(data, bytes);
        assert_eq!(duration, 0.0, "the decoder reports the real length");
    }
}
