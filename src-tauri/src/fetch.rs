//! Stream resolution and download.
//!
//! Two strategies, tried in order:
//!
//! 1. **rustypipe** (pure Rust): resolve via the iOS innertube client and
//!    download in small ranged chunks. As of mid-2026 YouTube only serves
//!    ~1 MB per URL to clients without a proof-of-origin token, and the
//!    clients that accept one need signature deobfuscation, which is broken
//!    in rustypipe 0.11.4 (upstream is dormant). Kept first so playback
//!    automatically returns to pure Rust when upstream catches up.
//! 2. **yt-dlp** (subprocess): actively maintained against YouTube changes;
//!    downloads the m4a to a temp file which we read back into memory.
//!
//! A failed rustypipe attempt backs off to yt-dlp for the next several tracks
//! and then re-probes, so a transient failure doesn't pin the whole session to
//! the subprocess path.

use std::sync::atomic::{AtomicU32, Ordering};

use anyhow::{anyhow, bail, Context};
use reqwest::header;
use rustypipe::client::{ClientType, RustyPipe};
use rustypipe::model::AudioFormat;
use tracing::{debug, info, warn};

// The CDN 403s range requests much larger than 1 MB.
const CHUNK_SIZE_MIN: u64 = 800_000;
const CHUNK_JITTER: u64 = 200_000;
const MAX_ATTEMPTS: usize = 2;

/// After a rustypipe failure, fall back to yt-dlp for this many fetches before
/// re-probing rustypipe (so playback recovers automatically when upstream or
/// the network does, instead of giving up for the whole session).
const RUSTYPIPE_RETRY_AFTER: u32 = 50;

/// Fetches remaining before rustypipe is retried. 0 means "try rustypipe now".
static RUSTYPIPE_COOLDOWN: AtomicU32 = AtomicU32::new(0);

/// Load a track for playback, preferring an offline copy under `downloads_dir`
/// and falling back to a network fetch. Returns the audio bytes and duration
/// in seconds (0.0 if unknown — the decoder reports the real length).
pub async fn fetch_track(
    rp: &RustyPipe,
    http: &reqwest::Client,
    downloads_dir: &std::path::Path,
    video_id: &str,
) -> anyhow::Result<(Vec<u8>, f64)> {
    let local = downloads_dir.join(format!("{video_id}.m4a"));
    if local.exists() {
        if let Ok(data) = tokio::fs::read(&local).await {
            debug!("playing {video_id} from offline download");
            return Ok((data, 0.0));
        }
    }
    fetch_bytes(rp, http, video_id).await
}

/// Resolve a YouTube video ID to its best m4a audio stream and download it
/// fully into memory. Returns the audio data and its duration in seconds
/// (0.0 if unknown).
pub async fn fetch_bytes(
    rp: &RustyPipe,
    http: &reqwest::Client,
    video_id: &str,
) -> anyhow::Result<(Vec<u8>, f64)> {
    if RUSTYPIPE_COOLDOWN.load(Ordering::Relaxed) == 0 {
        match fetch_via_rustypipe(rp, http, video_id).await {
            Ok(out) => return Ok(out),
            Err(e) => {
                warn!(
                    "rustypipe streaming failed ({e:#}); using yt-dlp for the next {RUSTYPIPE_RETRY_AFTER} tracks"
                );
                RUSTYPIPE_COOLDOWN.store(RUSTYPIPE_RETRY_AFTER, Ordering::Relaxed);
            }
        }
    } else {
        // Count down toward the next re-probe.
        let _ = RUSTYPIPE_COOLDOWN.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
            Some(n.saturating_sub(1))
        });
    }
    fetch_via_ytdlp(video_id).await
}

async fn fetch_via_rustypipe(
    rp: &RustyPipe,
    http: &reqwest::Client,
    video_id: &str,
) -> anyhow::Result<(Vec<u8>, f64)> {
    let q = rp.query();
    let mut last_err = anyhow!("could not fetch stream");

    for attempt in 0..MAX_ATTEMPTS {
        // The iOS client serves unciphered stream URLs; all other clients
        // require signature deobfuscation (broken in rustypipe 0.11.4).
        let player = q
            .player_from_clients(video_id, &[ClientType::Ios])
            .await
            .context("could not resolve stream")?;

        // Pick the best m4a (AAC) stream; symphonia has no Opus decoder,
        // so webm streams are out.
        let stream = player
            .audio_streams
            .iter()
            .filter(|s| s.format == AudioFormat::M4a)
            .max_by_key(|s| s.bitrate)
            .ok_or_else(|| anyhow!("no m4a audio stream available"))?;
        // The CDN rejects downloads whose User-Agent doesn't match the
        // client that requested the stream URL.
        let user_agent = q.user_agent(player.client_type).into_owned();
        debug!(
            "attempt {attempt}: client {:?}, itag {} ({} kbit/s, {} bytes)",
            player.client_type,
            stream.itag,
            stream.bitrate / 1000,
            stream.size
        );

        match fetch_chunked(http, &stream.url, &user_agent, stream.size).await {
            Ok(data) => {
                let duration = stream
                    .duration_ms
                    .map(|ms| f64::from(ms) / 1000.0)
                    .unwrap_or(0.0);
                return Ok((data, duration));
            }
            Err(e) if is_forbidden(&e) => {
                debug!("download forbidden, retrying with fresh visitor data");
                if let Some(vd) = &player.visitor_data {
                    q.remove_visitor_data(vd);
                }
                last_err = e;
            }
            Err(e) => return Err(e),
        }
    }
    Err(last_err)
}

fn is_forbidden(e: &anyhow::Error) -> bool {
    e.chain().any(|c| {
        c.downcast_ref::<reqwest::Error>()
            .is_some_and(|re| re.status() == Some(reqwest::StatusCode::FORBIDDEN))
    })
}

async fn fetch_chunked(
    http: &reqwest::Client,
    url: &str,
    user_agent: &str,
    size: u64,
) -> anyhow::Result<Vec<u8>> {
    let mut data: Vec<u8> = Vec::with_capacity(size as usize);
    let mut offset: u64 = 0;

    while size == 0 || offset < size {
        let chunk = CHUNK_SIZE_MIN + rand::random::<u64>() % CHUNK_JITTER;
        let end = match size {
            0 => offset + chunk - 1,
            s => (offset + chunk - 1).min(s - 1),
        };
        let requested = end - offset + 1;
        debug!("fetching bytes={offset}-{end} of {size}");

        let resp = http
            .get(url)
            .header(header::USER_AGENT, user_agent)
            .header(header::RANGE, format!("bytes={offset}-{end}"))
            .send()
            .await
            .context("stream request failed")?
            .error_for_status()
            .context("stream request rejected")?;
        let bytes = resp.bytes().await.context("stream download failed")?;
        if bytes.is_empty() {
            break;
        }
        offset += bytes.len() as u64;
        data.extend_from_slice(&bytes);
        // A short read means the server reached end of file.
        if (bytes.len() as u64) < requested {
            break;
        }
    }
    // A known content length we didn't reach means the response was truncated
    // (e.g. CDN throttling) — error out so the caller falls back to yt-dlp
    // instead of playing or caching a half-finished track.
    if size > 0 && (data.len() as u64) < size {
        bail!("incomplete stream: got {} of {size} bytes", data.len());
    }
    Ok(data)
}

async fn fetch_via_ytdlp(video_id: &str) -> anyhow::Result<(Vec<u8>, f64)> {
    // A random suffix keeps concurrent fetches of the same id from writing to —
    // and reading back — the same temp file.
    let path = std::env::temp_dir().join(format!(
        "rift-{video_id}-{:016x}.m4a",
        rand::random::<u64>()
    ));
    let _ = tokio::fs::remove_file(&path).await;

    info!("fetching {video_id} via yt-dlp");
    let output = tokio::process::Command::new("yt-dlp")
        .arg("-f")
        .arg("140/bestaudio[ext=m4a]")
        .arg("--no-playlist")
        .arg("--quiet")
        .arg("--no-warnings")
        .arg("--force-overwrites")
        .arg("-o")
        .arg(&path)
        .arg(format!("https://www.youtube.com/watch?v={video_id}"))
        .output()
        .await
        .context("could not run yt-dlp — is it installed and on your PATH? (https://github.com/yt-dlp/yt-dlp#installation)")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("yt-dlp failed: {}", stderr.trim());
    }

    let data = tokio::fs::read(&path)
        .await
        .context("yt-dlp produced no output file")?;
    let _ = tokio::fs::remove_file(&path).await;
    Ok((data, 0.0))
}
