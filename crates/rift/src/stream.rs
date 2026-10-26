//! Native audio streaming: resolve a video id to a directly downloadable audio
//! stream and download it over ranged HTTP.
//!
//! `YouTube` throttles a plain `GET` of a googlevideo URL to roughly 32 KB/s,
//! while the same URL answers a `Range` request at full speed
//! (`docs/native-streaming.md` §2.3). Every request this module makes is
//! therefore ranged, in chunks of [`CHUNK_BYTES`]; a server that ignores `Range`
//! and answers `200` with the whole body is still handled.
//!
//! Two entry points share the machinery:
//!
//! * [`fetch_bytes`] — the playback path: the stream is buffered in memory,
//!   because the decoder needs the bytes;
//! * [`fetch_to_path`] — the offline path: the stream is written to the caller's
//!   temporary `.part` path (this module never renames it).
//!
//! Resolution goes through `rift_ytmusic::player::resolve` (the JS-less
//! `VISIONOS` innertube path), wrapped by [`resolve`] here so the downloader is
//! built against a single seam: the transfer code only needs a URL and an
//! optional length.
//!
//! Errors are returned as-is — never swallowed — so the caller (the playback
//! fetch, or the download worker) can surface a single actionable failure.

use std::future::poll_fn;
use std::path::Path;
use std::pin::Pin;
use std::sync::OnceLock;
use std::time::Duration;

use anyhow::{bail, Context};
use reqwest::header::{self, CONTENT_RANGE};
use reqwest::StatusCode;
use tokio::io::AsyncWrite;
use tracing::{debug, info, warn};

/// Bytes requested per ranged GET (`Range: bytes=start-end`).
///
/// 8 MiB covers a typical music track in one or two requests, and the `Range`
/// header is what lifts `YouTube`'s plain-GET throttle (§2.3).
const CHUNK_BYTES: u64 = 8 * 1024 * 1024;

/// Ceiling for establishing the TCP/TLS connection to the edge.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Ceiling for each *read* while a body is being consumed.
///
/// Deliberately a read timeout, not a total one: a multi-megabyte audio stream
/// legitimately takes longer than the backend's 30 s total JSON timeout, but no
/// single socket read should ever stall for this long. Applying a total timeout
/// here would abort valid downloads on a slow link.
const READ_TIMEOUT: Duration = Duration::from_secs(30);

/// User agent sent with the audio requests. The resolved URL is already
/// authorized, so this only needs to be a plausible non-empty client identity.
const USER_AGENT: &str = "rift";

/// Ceiling on a single **in-memory** audio body.
///
/// Playback buffers the whole track, and the resolver's length (or the
/// `Content-Range` total) is the normal bound. The one place a server chooses
/// the size is the `200`-instead-of-`206` fallback — it ignored `Range` and may
/// not have declared a usable length; without a cap a broken or hostile edge
/// could grow the playback buffer until the process is OOM-killed. File sinks
/// need no such cap: the disk is the bound.
const MAX_MEMORY_BYTES: u64 = 256 * 1024 * 1024;

/// Host suffix of the direct audio URLs the resolver returns.
const AUDIO_HOST_SUFFIX: &str = "googlevideo.com";

/// The downloader's own HTTP client.
///
/// The backend's shared client (`player.http`) is built for small JSON requests
/// with a 30 s **total** timeout; a 3.9 MB audio range at full speed can exceed
/// that on a slow link, so it must not be reused here
/// (`docs/native-streaming.md` §3.2). This one has a connect timeout and a read
/// timeout but no total deadline, and a connection pool of its own.
static CLIENT: OnceLock<Result<reqwest::Client, String>> = OnceLock::new();

/// The process-wide audio client, built on first use.
fn http() -> anyhow::Result<&'static reqwest::Client> {
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .connect_timeout(CONNECT_TIMEOUT)
                .read_timeout(READ_TIMEOUT)
                // Audio bytes are opaque: automatic decompression would break
                // the byte-offset/`Content-Range` arithmetic (a compressed media
                // response should never happen anyway).
                .gzip(false)
                .brotli(false)
                // Signed media URLs are not expected to redirect. If one does,
                // only follow hops that still satisfy the stream URL rules, so a
                // redirect cannot turn the audio fetch into a request to another
                // host.
                .redirect(reqwest::redirect::Policy::custom(|attempt| {
                    if attempt.previous().len() >= 2 {
                        attempt.error("too many redirects for the native audio stream")
                    } else if allowed_stream_url(attempt.url()) {
                        attempt.follow()
                    } else {
                        attempt.error(
                            "the native audio stream redirected outside HTTPS googlevideo.com",
                        )
                    }
                }))
                .build()
                .map_err(|e| format!("could not build the native audio HTTP client: {e}"))
        })
        .as_ref()
        .map_err(|message| anyhow::anyhow!("{message}"))
}

/// Whether a URL satisfies the downloader's transport rules: HTTPS, on
/// `googlevideo.com` (exactly or as a subdomain), with no embedded credentials.
///
/// Used both for the resolver's URL ([`validate_stream_url`]) and for every
/// redirect hop in [`http`]'s client policy.
fn allowed_stream_url(url: &reqwest::Url) -> bool {
    if url.scheme() != "https" {
        return false;
    }
    if !url.username().is_empty() || url.password().is_some() {
        return false;
    }
    url.host_str().is_some_and(|host| {
        host == AUDIO_HOST_SUFFIX || host.ends_with(&format!(".{AUDIO_HOST_SUFFIX}"))
    })
}

/// Validate a URL returned by the resolver before requesting any bytes.
///
/// The URL is signed and fetched with the app's identity, so it must be an
/// HTTPS googlevideo.com URL: a hostile or buggy resolver response must not be
/// able to point the downloader at an arbitrary host (SSRF) or a plaintext
/// transport. Redirects are checked separately by [`http`]'s redirect policy.
fn validate_stream_url(video_id: &str, url: &str) -> anyhow::Result<()> {
    let parsed = reqwest::Url::parse(url).with_context(|| {
        format!("the native audio stream URL for {video_id} is not a valid URL")
    })?;
    if allowed_stream_url(&parsed) {
        return Ok(());
    }
    bail!(
        "the native audio stream for {video_id} points at {} ({}), but only HTTPS URLs on \
         {AUDIO_HOST_SUFFIX} are accepted",
        parsed.host_str().unwrap_or("<no host>"),
        parsed.scheme()
    )
}

/// One directly downloadable audio stream, reduced to what the transfer code
/// needs. Production fills this from `rift_ytmusic::player::resolve`; tests fill
/// it from a loopback server.
struct ResolvedStream {
    /// Direct googlevideo URL (signed; expires).
    url: String,
    /// Total bytes as reported by the resolver, when it was reported.
    content_length: Option<u64>,
    /// Format id reported by the resolver. Used only to decide whether a
    /// re-resolved stream is the same media as the one already partly written:
    /// splicing a different format's bytes onto the existing sink would publish
    /// corrupt audio even though every length check passes.
    itag: u32,
    /// MIME type reported by the resolver; part of the same identity check.
    mime_type: String,
}

/// Resolve `video_id` to a direct audio stream.
///
/// This is the single integration seam with the resolver crate: the downloader
/// below only needs the URL and the reported length, so the resolver's richer
/// [`rift_ytmusic::player::AudioStream`] is mapped onto [`ResolvedStream`] here
/// (its itag/mime are logged, since they are the first thing to check when a
/// native fetch misbehaves).
async fn resolve(video_id: &str) -> anyhow::Result<ResolvedStream> {
    let stream = rift_ytmusic::player::resolve(video_id)
        .await
        .with_context(|| format!("could not resolve a native audio stream for {video_id}"))?;
    validate_stream_url(video_id, &stream.url)?;
    debug!(
        "resolved {video_id} natively: itag {} {} ({} bytes)",
        stream.itag,
        stream.mime_type,
        match stream.content_length {
            Some(length) => length.to_string(),
            None => "unknown length".to_owned(),
        }
    );
    Ok(ResolvedStream {
        url: stream.url,
        content_length: stream.content_length,
        itag: stream.itag,
        mime_type: stream.mime_type,
    })
}

/// Resolve and stream the audio fully into memory (the playback path).
///
/// # Errors
///
/// Returns the resolver's error (unavailable video, rejected visitor, no
/// directly downloadable audio format), or a transport/validation error: the
/// server rejected the URL after one re-resolve, answered with an unexpected
/// status, broke mid-body, or the byte count did not match the stream's reported
/// length. Every one of these is returned to the caller as the single cause of
/// the failure (there is no fallback backend whose error could mask it).
pub async fn fetch_bytes(video_id: &str) -> anyhow::Result<Vec<u8>> {
    let client = http()?;
    // `resolve` already contextualizes its failure with the video id; don't
    // wrap it again (the UI would show the same sentence twice).
    let resolved = resolve(video_id).await?;
    let mut sink = Sink::memory();
    let mut progress = |_: u64, _: u64| {};
    let refresh_id = video_id.to_owned();
    download_with_refresh(
        client,
        video_id,
        resolved,
        &mut sink,
        CHUNK_BYTES,
        &mut progress,
        move || {
            let id = refresh_id.clone();
            async move { resolve(&id).await }
        },
    )
    .await?;
    match sink {
        Sink::Memory(data) => Ok(data),
        Sink::File { .. } => bail!("internal error: the in-memory audio sink became a file sink"),
    }
}

/// Resolve and stream the audio to `dest` (the offline path).
///
/// `dest` is the caller's temporary `.part` path: this function creates or
/// truncates it and writes every byte as it arrives, but never renames it. The
/// download worker publishes the file only after a successful return, so a
/// crash or a native failure can never leave a truncated file under the real
/// `<id>.m4a` name.
///
/// `progress(received, total)` is invoked once before the first request and
/// after every received chunk; `total` is `0` until/unless the stream's length
/// is known. Throttling the resulting UI events is the caller's job.
///
/// # Errors
///
/// As [`fetch_bytes`], plus an error when `dest` cannot be created or written.
pub async fn fetch_to_path(
    video_id: &str,
    dest: &Path,
    mut progress: impl FnMut(u64, u64) + Send,
) -> anyhow::Result<()> {
    let client = http()?;
    // `resolve` already contextualizes its failure; see `fetch_bytes`.
    let resolved = resolve(video_id).await?;
    debug!("streaming {video_id} natively to {}", dest.display());
    let mut sink = Sink::file(dest)
        .await
        .with_context(|| format!("could not create {}", dest.display()))?;
    let refresh_id = video_id.to_owned();
    download_with_refresh(
        client,
        video_id,
        resolved,
        &mut sink,
        CHUNK_BYTES,
        &mut progress,
        move || {
            let id = refresh_id.clone();
            async move { resolve(&id).await }
        },
    )
    .await
    .context("the native audio download failed")?;
    info!(
        "native stream: downloaded {video_id} to {} ({} bytes)",
        dest.display(),
        sink.len()
    );
    Ok(())
}

/// Where downloaded bytes go: memory for playback, the caller's `.part` file for
/// offline downloads.
enum Sink {
    /// Playback: the decoder needs the whole stream, so it is buffered.
    Memory(Vec<u8>),
    /// Offline: appended straight to the temporary file as chunks arrive.
    File { file: tokio::fs::File, written: u64 },
}

impl Sink {
    fn memory() -> Self {
        Self::Memory(Vec::new())
    }

    /// Create (or truncate) the destination and stream into it.
    async fn file(path: &Path) -> std::io::Result<Self> {
        let file = tokio::fs::File::create(path).await?;
        Ok(Self::File { file, written: 0 })
    }

    /// Bytes written so far; also the offset of the next range request.
    fn len(&self) -> u64 {
        match self {
            Self::Memory(data) => data.len() as u64,
            Self::File { written, .. } => *written,
        }
    }

    /// Whether this sink buffers the whole stream in memory (the playback path),
    /// which is the one sink [`MAX_MEMORY_BYTES`] bounds.
    fn is_memory(&self) -> bool {
        matches!(self, Self::Memory(_))
    }

    /// Append one received chunk.
    async fn append(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        match self {
            Self::Memory(data) => data.extend_from_slice(bytes),
            Self::File { file, written } => {
                write_all(file, bytes).await?;
                *written += bytes.len() as u64;
            }
        }
        Ok(())
    }
}

/// Refuse to grow an in-memory sink past [`MAX_MEMORY_BYTES`].
///
/// Split out so the bound is unit-testable without allocating hundreds of
/// megabytes; production calls it before every append with the incoming chunk.
fn check_memory_cap(video_id: &str, current: u64, incoming: usize) -> Result<(), AttemptError> {
    let grown = current.saturating_add(incoming as u64);
    if grown <= MAX_MEMORY_BYTES {
        return Ok(());
    }
    Err(failed(format!(
        "the audio server sent more than the {MAX_MEMORY_BYTES}-byte in-memory limit for \
         {video_id}; refusing an unbounded body"
    )))
}

/// Write every byte of `bytes` to `file`.
///
/// Hand-rolled because the crate enables only tokio's `fs`/`process`/`sync`/
/// `time` features, not `io-util`, so `AsyncWriteExt` is unavailable; the
/// `AsyncWrite` trait itself is core and is enough.
async fn write_all(file: &mut tokio::fs::File, mut bytes: &[u8]) -> std::io::Result<()> {
    while !bytes.is_empty() {
        let written = poll_fn(|cx| Pin::new(&mut *file).poll_write(cx, bytes)).await?;
        if written == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::WriteZero,
                "the audio file stopped accepting writes",
            ));
        }
        bytes = &bytes[written..];
    }
    Ok(())
}

/// Why one download attempt did not finish.
enum AttemptError {
    /// The server rejected the URL (`403`/`410`). A signed URL may simply have
    /// expired, so one re-resolve is worth trying — with the bytes already on
    /// the sink kept and the transfer resumed from there.
    Expired,
    /// Anything else; returned to the caller as the single cause of failure.
    Failed(anyhow::Error),
}

/// Build an [`AttemptError::Failed`] from a message.
fn failed(message: String) -> AttemptError {
    AttemptError::Failed(anyhow::anyhow!(message))
}

/// One transfer attempt: request ranges from `resolved` until the stream is
/// complete, appending to `sink` starting at its current length.
///
/// `chunk_bytes` is a parameter only so tests can exercise multi-chunk
/// transfers with tiny bodies; production always passes [`CHUNK_BYTES`].
async fn download_from(
    client: &reqwest::Client,
    resolved: &ResolvedStream,
    video_id: &str,
    sink: &mut Sink,
    chunk_bytes: u64,
    progress: &mut (dyn FnMut(u64, u64) + Send),
) -> Result<Option<u64>, AttemptError> {
    let mut offset = sink.len();
    let mut total = resolved.content_length;
    if let Some(t) = total {
        // A resolved stream that reports zero bytes is a failed resolution, not
        // audio: reject it before writing anything (and before the caller can
        // publish the empty file as an offline copy).
        if t == 0 {
            return Err(failed(format!(
                "the native audio stream for {video_id} is empty (0 bytes)"
            )));
        }
        if offset > t {
            return Err(failed(format!(
                "the native audio stream for {video_id} is {t} bytes, shorter than the {offset} \
                 bytes already downloaded"
            )));
        }
    }
    // The "this transfer started" milestone: before the first byte arrives the
    // UI would otherwise keep its indeterminate spinner even though the length
    // is known.
    progress(offset, total.unwrap_or(0));

    loop {
        if total.is_some_and(|t| offset >= t) {
            break;
        }
        let range_end = offset.saturating_add(chunk_bytes.saturating_sub(1));
        let range = format!("bytes={offset}-{range_end}");
        debug!(%range, "native audio range request");
        let mut response = client
            .get(&resolved.url)
            .header(header::RANGE, &range)
            .header(header::USER_AGENT, USER_AGENT)
            .send()
            .await
            .map_err(|e| {
                AttemptError::Failed(anyhow::Error::new(e).context(format!(
                    "could not request bytes {offset}-{range_end} of {video_id}"
                )))
            })?;

        let status = response.status();
        let whole_body = match status {
            StatusCode::PARTIAL_CONTENT => false,
            StatusCode::OK => {
                // The server ignored `Range`. Only recoverable for the first
                // request: there the body is the whole stream. A `200` in
                // response to a later range would silently restart the stream at
                // byte 0, corrupting the file, so it is an error instead.
                if offset != 0 {
                    return Err(failed(format!(
                        "the audio server ignored Range mid-download ({video_id}): HTTP 200 at \
                         byte {offset}"
                    )));
                }
                if total.is_none() {
                    total = response.content_length();
                }
                true
            }
            StatusCode::FORBIDDEN | StatusCode::GONE => return Err(AttemptError::Expired),
            // Asking for a range that starts past the end of an unknown-length
            // stream is how the transfer learns it is already complete.
            StatusCode::RANGE_NOT_SATISFIABLE if offset > 0 => break,
            other => {
                return Err(failed(format!(
                    "the native audio request for {video_id} failed with HTTP {other}"
                )))
            }
        };
        if !whole_body {
            verify_content_range(&response, offset, &mut total, video_id)?;
        }

        let mut this_response: u64 = 0;
        loop {
            let next = response.chunk().await.map_err(|e| {
                AttemptError::Failed(anyhow::Error::new(e).context(format!(
                    "the native audio stream for {video_id} broke while reading"
                )))
            })?;
            let Some(bytes) = next else { break };
            if bytes.is_empty() {
                continue;
            }
            if sink.is_memory() {
                check_memory_cap(video_id, offset, bytes.len())?;
            }
            sink.append(&bytes).await.map_err(|e| {
                AttemptError::Failed(
                    anyhow::Error::new(e).context("could not write the audio stream to disk"),
                )
            })?;
            offset = sink.len();
            this_response += bytes.len() as u64;
            progress(offset, total.unwrap_or(0));
            if let Some(t) = total {
                if offset > t {
                    return Err(failed(format!(
                        "the audio server sent more than the {t} bytes reported for {video_id}"
                    )));
                }
            }
        }

        if whole_body {
            break;
        }
        if this_response == 0 {
            return Err(failed(format!(
                "the audio server returned an empty range at byte {offset} for {video_id}"
            )));
        }
        if total.is_some() {
            // A known length decides completion: loop until `offset == total`
            // (the head of the loop breaks then).
            continue;
        }
        // Unknown length: the stream ended when a range came back shorter than
        // requested. A full-size range means there may be more.
        if this_response < chunk_bytes {
            break;
        }
    }
    Ok(total)
}

/// Validate a `206` response's `Content-Range` against the offset requested and
/// the length the resolver reported, learning the length when neither side knew
/// it beforehand.
fn verify_content_range(
    response: &reqwest::Response,
    offset: u64,
    total: &mut Option<u64>,
    video_id: &str,
) -> Result<(), AttemptError> {
    let Some(value) = response
        .headers()
        .get(CONTENT_RANGE)
        .and_then(|value| value.to_str().ok())
    else {
        // A 206 without `Content-Range` is unusual but not fatal: the body is
        // appended at the tracked offset either way, and the final length check
        // still catches a wrong stream.
        return Ok(());
    };
    let Some((start, _end, range_total)) = parse_content_range(value) else {
        debug!("ignoring unparsable Content-Range {value:?} for {video_id}");
        return Ok(());
    };
    if start != offset {
        return Err(failed(format!(
            "the audio server answered a request for byte {offset} of {video_id} with a range \
             starting at {start}"
        )));
    }
    match (*total, range_total) {
        (Some(known), Some(served)) if known != served => Err(failed(format!(
            "the audio server reports {served} bytes for {video_id} but the resolver reported \
             {known}"
        ))),
        (None, served) => {
            *total = served;
            Ok(())
        }
        _ => Ok(()),
    }
}

/// Parse `Content-Range: bytes <start>-<end>/<total>` (`*` for an unknown total).
fn parse_content_range(value: &str) -> Option<(u64, u64, Option<u64>)> {
    let value = value.trim();
    let rest = value.strip_prefix("bytes")?.trim_start();
    let (range, total) = rest.split_once('/')?;
    let (start, end) = range.split_once('-')?;
    let start = start.trim().parse().ok()?;
    let end = end.trim().parse().ok()?;
    let total = match total.trim() {
        "*" => None,
        total => Some(total.parse().ok()?),
    };
    Some((start, end, total))
}

/// Verify a finished transfer against what should have been received.
///
/// An empty stream is always a failure (an empty file must never be published as
/// an offline copy), and a stream whose length was known must be exactly that
/// long — a truncated download that happens to end before its last range must
/// not pass as complete.
fn verify_complete(video_id: &str, received: u64, total: Option<u64>) -> anyhow::Result<()> {
    if received == 0 {
        bail!("the native audio stream for {video_id} was empty");
    }
    if let Some(total) = total {
        if received != total {
            bail!("the native audio stream for {video_id} ended after {received} of {total} bytes");
        }
    }
    Ok(())
}

/// Run one download, re-resolving (and resuming from the bytes already written)
/// once if the signed URL is rejected as expired.
///
/// A refresh is only spliced onto the existing sink when it is provably the same
/// media: the re-resolved stream must report the same itag, MIME type and (when
/// both sides know it) total length. A different format or length may not be
/// resumed — the bytes already written came from another rendition, so a splice
/// would produce a file that passes every length check while decoding to
/// corrupt audio. That case fails instead; the caller's retry re-downloads from
/// zero.
#[allow(clippy::too_many_arguments)] // the constructor-style split keeps each step testable
async fn download_with_refresh<F, Fut>(
    client: &reqwest::Client,
    video_id: &str,
    mut resolved: ResolvedStream,
    sink: &mut Sink,
    chunk_bytes: u64,
    progress: &mut (dyn FnMut(u64, u64) + Send),
    mut refresh: F,
) -> anyhow::Result<()>
where
    F: FnMut() -> Fut + Send,
    Fut: std::future::Future<Output = anyhow::Result<ResolvedStream>> + Send,
{
    let mut refreshed = false;
    // The total the successful attempt learned or was handed. The final check
    // must use *this*, not `resolved.content_length`: a `Content-Range` total
    // learned during the transfer is the authority once it exists.
    let effective_total;
    loop {
        match download_from(client, &resolved, video_id, sink, chunk_bytes, progress).await {
            Ok(total) => {
                effective_total = total;
                break;
            }
            Err(AttemptError::Expired) if !refreshed => {
                refreshed = true;
                warn!(
                    "the native audio URL for {video_id} was rejected (expired?); re-resolving once"
                );
                let previous = resolved;
                resolved = refresh()
                    .await
                    .context("could not re-resolve the native audio stream")?;
                if resolved.itag != previous.itag || resolved.mime_type != previous.mime_type {
                    bail!(
                        "the re-resolved native audio stream for {video_id} changed format \
                         (itag {}/{} → {}/{}); refusing to splice two renditions",
                        previous.itag,
                        previous.mime_type,
                        resolved.itag,
                        resolved.mime_type
                    );
                }
                if let (Some(old), Some(new)) = (previous.content_length, resolved.content_length) {
                    if old != new {
                        bail!(
                            "the re-resolved native audio stream for {video_id} reports {new} \
                             bytes but the first resolve reported {old}; refusing to splice"
                        );
                    }
                }
                if let Some(total) = resolved.content_length {
                    if total < sink.len() {
                        bail!(
                            "the re-resolved native audio stream for {video_id} is {total} bytes, \
                             shorter than the {} bytes already downloaded",
                            sink.len()
                        );
                    }
                }
            }
            Err(AttemptError::Expired) => bail!(
                "the native audio URL for {video_id} is still rejected after re-resolving once"
            ),
            Err(AttemptError::Failed(error)) => return Err(error),
        }
    }
    // Publishing requires a verifiable length: an empty-file check alone would
    // let a cleanly truncated unknown-length body pass as complete.
    let Some(total) = effective_total else {
        bail!("the native audio stream for {video_id} ended without reporting a verifiable length");
    };
    verify_complete(video_id, sink.len(), Some(total))
}

#[cfg(test)]
mod tests {
    use super::{
        check_memory_cap, download_with_refresh, parse_content_range, validate_stream_url,
        verify_complete, AttemptError, ResolvedStream, Sink, MAX_MEMORY_BYTES,
    };
    use crate::util::testing::TempDir;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    /// A loopback audio server's answer to one request.
    #[derive(Debug)]
    struct Captured {
        /// The `Range` request header, if the client sent one.
        range: Option<String>,
    }

    /// How the loopback server behaves.
    enum Behaviour {
        /// Honour `Range` with a `206` (optionally sending `Content-Range`).
        Ranges { body: Vec<u8>, content_range: bool },
        /// Ignore `Range` and answer `200` with the whole body.
        WholeBody(Vec<u8>),
        /// Serve the first request, then reject every later one with `403`
        /// (a URL that expires mid-transfer).
        RangesThenForbidden { body: Vec<u8>, served: AtomicUsize },
        /// Serve one range but declare a larger total, then answer every later
        /// request with `416` — the shape of a body that ends before the total
        /// its own `Content-Range` reported.
        FirstRangeThenUnsatisfiable {
            body: Vec<u8>,
            declared_total: u64,
            served: AtomicUsize,
        },
        /// Reject every request with `403`.
        Forbidden,
    }

    /// A running loopback audio server plus the requests it received.
    ///
    /// One detached thread per server (as the existing crate tests do): it
    /// serves until the process exits, and the test process ends after the
    /// suite. No committed test touches the network — the only addresses are
    /// `127.0.0.1` ports the test itself bound.
    struct FakeAudio {
        base: String,
        captured: Arc<Mutex<Vec<Captured>>>,
    }

    impl FakeAudio {
        fn ranges(&self) -> Vec<String> {
            self.captured
                .lock()
                .expect("request log")
                .iter()
                .map(|request| request.range.clone().unwrap_or_default())
                .collect()
        }

        fn request_count(&self) -> usize {
            self.captured.lock().expect("request log").len()
        }
    }

    /// Start a loopback server with the given behaviour.
    fn fake_audio(behaviour: Behaviour) -> FakeAudio {
        use std::io::Read;

        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("bind loopback");
        let base = format!(
            "http://127.0.0.1:{}/audio",
            listener.local_addr().expect("bound address").port()
        );
        let captured = Arc::new(Mutex::new(Vec::new()));
        let recorder = Arc::clone(&captured);
        std::thread::spawn(move || loop {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            // Drain the request head before answering: closing while the peer is
            // still sending resets the connection on Windows, and the client
            // would report a transport error instead of reading the reply.
            let mut head = Vec::new();
            let mut buf = [0u8; 2048];
            while head.len() < 16 * 1024 && !head.windows(4).any(|window| window == b"\r\n\r\n") {
                match stream.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => head.extend_from_slice(&buf[..n]),
                }
            }
            let request = String::from_utf8_lossy(&head).into_owned();
            let range = request.lines().find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("range")
                    .then(|| value.trim().to_owned())
            });
            recorder.lock().expect("request log").push(Captured {
                range: range.clone(),
            });

            match &behaviour {
                Behaviour::Forbidden => respond(&mut stream, 403, &[], None),
                Behaviour::WholeBody(body) => respond(&mut stream, 200, body, None),
                Behaviour::Ranges {
                    body,
                    content_range,
                } => {
                    serve_range(&mut stream, body, range.as_deref(), *content_range);
                }
                Behaviour::RangesThenForbidden { body, served } => {
                    if served.fetch_add(1, Ordering::SeqCst) == 0 {
                        serve_range(&mut stream, body, range.as_deref(), true);
                    } else {
                        respond(&mut stream, 403, &[], None);
                    }
                }
                Behaviour::FirstRangeThenUnsatisfiable {
                    body,
                    declared_total,
                    served,
                } => {
                    if served.fetch_add(1, Ordering::SeqCst) == 0 {
                        let (start, _end) = parse_range_header(range.as_deref().unwrap_or(""))
                            .unwrap_or((0, body.len().saturating_sub(1) as u64));
                        let slice = &body[start.min(body.len() as u64 - 1) as usize..];
                        let value = format!(
                            "bytes {start}-{}/{declared_total}",
                            start + slice.len() as u64 - 1
                        );
                        respond(&mut stream, 206, slice, Some(&value));
                    } else {
                        let value = format!("bytes */{declared_total}");
                        respond(&mut stream, 416, &[], Some(&value));
                    }
                }
            }
        });
        FakeAudio { base, captured }
    }

    /// Parse a `bytes=start-end` request header.
    fn parse_range_header(value: &str) -> Option<(u64, u64)> {
        let rest = value.trim().strip_prefix("bytes=")?;
        let (start, end) = rest.split_once('-')?;
        Some((start.trim().parse().ok()?, end.trim().parse().ok()?))
    }

    /// Answer one request with a `206` slice of `body` (or `416` when the range
    /// starts past the end), optionally including `Content-Range`.
    fn serve_range(
        stream: &mut std::net::TcpStream,
        body: &[u8],
        range: Option<&str>,
        content_range: bool,
    ) {
        let total = body.len() as u64;
        let (start, end) = range
            .and_then(parse_range_header)
            .unwrap_or((0, total.saturating_sub(1)));
        if total == 0 || start >= total {
            let value = format!("bytes */{total}");
            respond(stream, 416, &[], content_range.then_some(value.as_str()));
            return;
        }
        let end = end.min(total - 1);
        let slice = &body[start as usize..=end as usize];
        let value = format!("bytes {start}-{end}/{total}");
        respond(stream, 206, slice, content_range.then_some(value.as_str()));
    }

    /// Write one HTTP response and close the connection.
    fn respond(
        stream: &mut std::net::TcpStream,
        status: u16,
        body: &[u8],
        content_range: Option<&str>,
    ) {
        use std::io::Write;

        let reason = match status {
            200 => "OK",
            206 => "Partial Content",
            403 => "Forbidden",
            416 => "Range Not Satisfiable",
            _ => "Error",
        };
        let mut head = format!("HTTP/1.1 {status} {reason}\r\nConnection: close\r\n");
        if let Some(value) = content_range {
            head.push_str(&format!("Content-Range: {value}\r\n"));
        }
        head.push_str(&format!("Content-Length: {}\r\n\r\n", body.len()));
        let _ = stream.write_all(head.as_bytes());
        let _ = stream.write_all(body);
        let _ = stream.flush();
    }

    fn test_http() -> reqwest::Client {
        reqwest::Client::builder()
            .no_proxy()
            .connect_timeout(Duration::from_secs(5))
            .read_timeout(Duration::from_secs(30))
            .build()
            .expect("build the test client")
    }

    /// A refresh callback for transfers that must not need one.
    fn no_refresh() -> impl FnMut() -> std::future::Ready<anyhow::Result<ResolvedStream>> + Send {
        || std::future::ready(Err(anyhow::anyhow!("no refresh expected in this test")))
    }

    /// A resolved stream for the transfer tests: identity fixed to the shipped
    /// audio format (itag 140, `audio/mp4`) so refresh-compatibility checks are
    /// exercised by default. Tests that need a different identity build the
    /// struct explicitly.
    fn test_stream(url: impl Into<String>, content_length: Option<u64>) -> ResolvedStream {
        ResolvedStream {
            url: url.into(),
            content_length,
            itag: 140,
            mime_type: "audio/mp4".to_owned(),
        }
    }

    /// The core of the native path: ranged requests walk the stream in offset
    /// order, every byte lands in the caller's `.part` file, and the progress
    /// callback starts at a known 0 and ends at 100%.
    #[tokio::test]
    async fn ranged_requests_walk_the_stream_in_offsets_and_stream_to_a_file() {
        let body = b"abcdefghij".to_vec();
        let server = fake_audio(Behaviour::Ranges {
            body: body.clone(),
            content_range: true,
        });
        let dir = TempDir::new("native-stream-ranges");
        let dest = dir.join("track.m4a.part");
        let mut sink = Sink::file(&dest).await.expect("create the part file");
        let mut calls: Vec<(u64, u64)> = Vec::new();

        download_with_refresh(
            &test_http(),
            "vid",
            test_stream(server.base.clone(), Some(body.len() as u64)),
            &mut sink,
            4,
            &mut |received, total| calls.push((received, total)),
            no_refresh(),
        )
        .await
        .expect("the ranged download succeeds");

        assert_eq!(
            std::fs::read(&dest).expect("read the part file"),
            body,
            "every byte lands in the caller's part file"
        );
        assert_eq!(
            server.ranges(),
            ["bytes=0-3", "bytes=4-7", "bytes=8-11"],
            "requests follow the byte offsets"
        );
        assert_eq!(
            calls.first(),
            Some(&(0, 10)),
            "the transfer announces a known 0% before its first byte"
        );
        assert_eq!(calls.last(), Some(&(10, 10)), "the transfer ends at 100%");
        assert!(
            calls.windows(2).all(|pair| pair[0].0 <= pair[1].0),
            "progress never goes backwards: {calls:?}"
        );
    }

    /// A server that ignores `Range` and answers `200` with the whole body is
    /// the documented fallback: the stream still downloads, in one request, and
    /// the length learned from the response reaches the progress callback.
    #[tokio::test]
    async fn a_server_that_ignores_range_still_downloads_the_whole_body() {
        let body = b"0123456789".to_vec();
        let server = fake_audio(Behaviour::WholeBody(body.clone()));

        // No length known up front: the 200 response's Content-Length supplies it.
        let mut sink = Sink::memory();
        let mut calls: Vec<(u64, u64)> = Vec::new();
        download_with_refresh(
            &test_http(),
            "vid",
            test_stream(server.base.clone(), None),
            &mut sink,
            4,
            &mut |received, total| calls.push((received, total)),
            no_refresh(),
        )
        .await
        .expect("a 200 whole-body response downloads");

        assert_eq!(sink.len(), body.len() as u64);
        assert_eq!(
            server.request_count(),
            1,
            "a full-body response needs no further ranges"
        );
        assert_eq!(
            calls.last(),
            Some(&(10, 10)),
            "the length is learned from the response and reported: {calls:?}"
        );
    }

    /// A stream that ends before its reported length must fail rather than be
    /// mistaken for a complete download (the `.part` file is discarded by the
    /// caller and its retry/error path takes over).
    #[tokio::test]
    async fn a_truncated_stream_is_rejected_as_a_size_mismatch() {
        // No `Content-Range` on the replies: the resolver's 100-byte length is
        // the only total, so the short body has to be caught by the final length
        // check rather than by a length disagreement up front.
        let server = fake_audio(Behaviour::Ranges {
            body: vec![7u8; 8],
            content_range: false,
        });
        let mut sink = Sink::memory();
        let error = download_with_refresh(
            &test_http(),
            "vid",
            test_stream(server.base.clone(), Some(100)),
            &mut sink,
            4,
            &mut |_, _| {},
            no_refresh(),
        )
        .await
        .expect_err("a stream shorter than its declared length must fail");
        let message = format!("{error:#}");
        assert!(
            message.contains("ended after 8 of 100 bytes"),
            "the error must name the shortfall: {message}"
        );
    }

    /// A resolved stream with no bytes, or a server that never serves one, is a
    /// failed resolution: an empty file must never be published offline.
    #[tokio::test]
    async fn an_empty_stream_is_rejected() {
        let mut sink = Sink::memory();
        let error = download_with_refresh(
            &test_http(),
            "vid",
            test_stream("http://127.0.0.1:9/never-requested".to_owned(), Some(0)),
            &mut sink,
            4,
            &mut |_, _| {},
            no_refresh(),
        )
        .await
        .expect_err("a zero-length stream must be rejected");
        assert!(
            format!("{error:#}").contains("empty (0 bytes)"),
            "the error must name the empty stream: {error:#}"
        );

        // The no-length form: the server serves no bytes at all, so the range
        // request is unsatisfiable and the transfer must not come back "done".
        let server = fake_audio(Behaviour::Ranges {
            body: Vec::new(),
            content_range: true,
        });
        let error = download_with_refresh(
            &test_http(),
            "vid",
            test_stream(server.base, None),
            &mut sink,
            4,
            &mut |_, _| {},
            no_refresh(),
        )
        .await
        .expect_err("an empty server response must be rejected");
        assert!(
            format!("{error:#}").contains("HTTP 416"),
            "the error must name the unsatisfiable range: {error:#}"
        );
    }

    /// A rejected URL triggers exactly one re-resolve, and the new attempt
    /// resumes at the offset the first one reached — not at byte 0, and without
    /// re-downloading anything.
    #[tokio::test]
    async fn an_expired_url_is_re_resolved_once_and_the_download_resumes() {
        let body = b"abcdefghij".to_vec();
        let expired = fake_audio(Behaviour::RangesThenForbidden {
            body: body.clone(),
            served: AtomicUsize::new(0),
        });
        let fresh = fake_audio(Behaviour::Ranges {
            body: body.clone(),
            content_range: true,
        });
        let fresh_url = fresh.base.clone();
        let mut refresh = move || {
            let url = fresh_url.clone();
            async move { Ok(test_stream(url, Some(10))) }
        };

        let dir = TempDir::new("native-stream-refresh");
        let dest = dir.join("track.m4a.part");
        let mut sink = Sink::file(&dest).await.expect("create the part file");
        download_with_refresh(
            &test_http(),
            "vid",
            test_stream(expired.base.clone(), Some(body.len() as u64)),
            &mut sink,
            4,
            &mut |_, _| {},
            &mut refresh,
        )
        .await
        .expect("the re-resolved URL completes the download");

        assert_eq!(std::fs::read(&dest).expect("read the part file"), body);
        assert_eq!(
            expired.request_count(),
            2,
            "one served range, then the 403 that triggers the refresh"
        );
        assert_eq!(
            fresh.ranges(),
            ["bytes=4-7", "bytes=8-11"],
            "the fresh URL resumes where the expired one stopped"
        );
    }

    /// A second rejection ends the native attempt; the error is returned rather
    /// than retried forever.
    #[tokio::test]
    async fn a_second_rejection_is_an_error_not_an_infinite_re_resolve() {
        let first = fake_audio(Behaviour::Forbidden);
        let second = fake_audio(Behaviour::Forbidden);
        let second_url = second.base.clone();
        let mut refresh = move || {
            let url = second_url.clone();
            async move { Ok(test_stream(url, Some(10))) }
        };
        let mut sink = Sink::memory();
        let error = download_with_refresh(
            &test_http(),
            "vid",
            test_stream(first.base.clone(), Some(10)),
            &mut sink,
            4,
            &mut |_, _| {},
            &mut refresh,
        )
        .await
        .expect_err("a second rejection must end the native attempt");
        assert!(
            format!("{error:#}").contains("still rejected after re-resolving once"),
            "the error must name the give-up: {error:#}"
        );
        assert_eq!(first.request_count(), 1);
        assert_eq!(second.request_count(), 1);
    }

    /// A server whose declared total disagrees with the resolver is rejected
    /// before its body can be mistaken for the right audio.
    #[tokio::test]
    async fn a_length_disagreement_with_the_resolver_is_rejected() {
        let server = fake_audio(Behaviour::Ranges {
            body: vec![0u8; 10],
            content_range: true,
        });
        let mut sink = Sink::memory();
        let error = download_with_refresh(
            &test_http(),
            "vid",
            test_stream(server.base, Some(12)),
            &mut sink,
            4,
            &mut |_, _| {},
            no_refresh(),
        )
        .await
        .expect_err("a length disagreement must fail");
        assert!(
            format!("{error:#}").contains("reports 10 bytes"),
            "the error must name the disagreement: {error:#}"
        );
    }

    /// A refresh that comes back as a different format must not be spliced onto
    /// the bytes already written: the prefix came from another rendition, and
    /// every length check would still pass on the corrupt result.
    #[tokio::test]
    async fn a_re_resolve_that_changes_the_format_is_rejected_before_splicing() {
        let body = b"abcdefghij".to_vec();
        let expired = fake_audio(Behaviour::RangesThenForbidden {
            body: body.clone(),
            served: AtomicUsize::new(0),
        });
        let fresh = fake_audio(Behaviour::Ranges {
            body,
            content_range: true,
        });
        let fresh_url = fresh.base.clone();
        let mut refresh = move || {
            let url = fresh_url.clone();
            async move {
                Ok(ResolvedStream {
                    url,
                    content_length: Some(10),
                    itag: 251,
                    mime_type: "audio/webm".to_owned(),
                })
            }
        };
        let mut sink = Sink::memory();
        let error = download_with_refresh(
            &test_http(),
            "vid",
            test_stream(expired.base.clone(), Some(10)),
            &mut sink,
            4,
            &mut |_, _| {},
            &mut refresh,
        )
        .await
        .expect_err("a different re-resolved format must not be spliced");
        assert!(
            format!("{error:#}").contains("changed format"),
            "the error must name the format change: {error:#}"
        );
    }

    /// A refresh that reports a different total length is the same hazard in
    /// another shape: the remainder would not line up with the prefix.
    #[tokio::test]
    async fn a_re_resolve_that_changes_the_length_is_rejected() {
        let body = b"abcdefghij".to_vec();
        let expired = fake_audio(Behaviour::RangesThenForbidden {
            body,
            served: AtomicUsize::new(0),
        });
        let fresh = fake_audio(Behaviour::Ranges {
            body: b"abcdefghijk".to_vec(),
            content_range: true,
        });
        let fresh_url = fresh.base.clone();
        let mut refresh = move || {
            let url = fresh_url.clone();
            async move { Ok(test_stream(url, Some(11))) }
        };
        let mut sink = Sink::memory();
        let error = download_with_refresh(
            &test_http(),
            "vid",
            test_stream(expired.base.clone(), Some(10)),
            &mut sink,
            4,
            &mut |_, _| {},
            &mut refresh,
        )
        .await
        .expect_err("a different re-resolved length must not be spliced");
        assert!(
            format!("{error:#}").contains("reports 11 bytes"),
            "the error must name the length change: {error:#}"
        );
    }

    /// The final check must use the total learned during the transfer, not only
    /// the resolver's initial value. Here the server declares 100 bytes via
    /// `Content-Range`, serves 4 and then 416s: that must fail, not publish.
    #[tokio::test]
    async fn a_stream_that_ends_before_its_learned_total_is_rejected() {
        let server = fake_audio(Behaviour::FirstRangeThenUnsatisfiable {
            body: b"abcd".to_vec(),
            declared_total: 100,
            served: AtomicUsize::new(0),
        });
        let mut sink = Sink::memory();
        let error = download_with_refresh(
            &test_http(),
            "vid",
            test_stream(server.base, None),
            &mut sink,
            4,
            &mut |_, _| {},
            no_refresh(),
        )
        .await
        .expect_err("a body shorter than its learned total must fail");
        assert!(
            format!("{error:#}").contains("ended after 4 of 100 bytes"),
            "the error must name the shortfall: {error:#}"
        );
    }

    /// A transfer with no authoritative total at all is not publishable: nothing
    /// could tell a complete stream from a cleanly truncated one.
    #[tokio::test]
    async fn an_unknown_length_stream_without_a_verifiable_total_is_rejected() {
        let server = fake_audio(Behaviour::Ranges {
            body: vec![7u8; 8],
            content_range: false,
        });
        let mut sink = Sink::memory();
        let error = download_with_refresh(
            &test_http(),
            "vid",
            test_stream(server.base, None),
            &mut sink,
            4,
            &mut |_, _| {},
            no_refresh(),
        )
        .await
        .expect_err("an unverifiable length must not be published");
        assert!(
            format!("{error:#}").contains("verifiable length"),
            "the error must name the missing length: {error:#}"
        );
    }

    #[test]
    fn content_range_parsing_uses_start_and_total_and_ignores_end() {
        assert_eq!(parse_content_range("bytes 0-3/10"), Some((0, 3, Some(10))));
        assert_eq!(
            parse_content_range("bytes 8388608-16777215/6953417"),
            Some((8_388_608, 16_777_215, Some(6_953_417))),
            "an `end` past the declared total is ignored, not treated as corruption: the \
             transfer appends by offset and the final length check is the authority"
        );
        assert_eq!(parse_content_range(" bytes 8-9/* "), Some((8, 9, None)));
        assert_eq!(parse_content_range("bytes 0-3"), None, "no total");
        assert_eq!(parse_content_range("items 0-3/10"), None, "wrong unit");
        assert_eq!(
            parse_content_range("bytes 0-3/ten"),
            None,
            "non-numeric total"
        );
    }

    #[test]
    fn only_https_googlevideo_urls_are_accepted_for_audio() {
        for accepted in [
            "https://rr1---sn-abc123.googlevideo.com/videoplayback?itag=140",
            "https://googlevideo.com/videoplayback",
        ] {
            assert!(
                validate_stream_url("vid", accepted).is_ok(),
                "{accepted} must be accepted"
            );
        }
        for rejected in [
            "http://rr1---sn-abc123.googlevideo.com/videoplayback",
            "https://evil.example/videoplayback",
            "https://googlevideo.com.evil.example/videoplayback",
            "https://evilgooglevideo.com/videoplayback",
            "https://user:pass@rr1---sn-abc123.googlevideo.com/videoplayback",
        ] {
            let error = validate_stream_url("vid", rejected).expect_err(rejected);
            assert!(
                format!("{error:#}").contains("only HTTPS URLs on googlevideo.com"),
                "{rejected} must be rejected with the rule: {error:#}"
            );
        }
        let error = validate_stream_url("vid", "not a url").expect_err("invalid URL");
        assert!(
            format!("{error:#}").contains("not a valid URL"),
            "unexpected error: {error:#}"
        );
    }

    #[test]
    fn an_in_memory_body_is_capped_at_the_memory_limit() {
        assert!(check_memory_cap("vid", 0, 1024).is_ok());
        assert!(
            check_memory_cap("vid", MAX_MEMORY_BYTES - 1, 1).is_ok(),
            "exactly at the cap is fine"
        );
        match check_memory_cap("vid", MAX_MEMORY_BYTES, 1) {
            Err(AttemptError::Failed(error)) => assert!(
                format!("{error:#}").contains("in-memory limit"),
                "unexpected error: {error:#}"
            ),
            _ => panic!("a body over the in-memory cap must be refused"),
        }
    }

    /// The final check is what makes a "successful" transfer trustworthy: empty
    /// is always wrong, and a known length must match exactly.
    #[test]
    fn a_finished_transfer_must_be_non_empty_and_exactly_the_known_length() {
        assert!(verify_complete("vid", 10, Some(10)).is_ok());
        assert!(verify_complete("vid", 10, None).is_ok(), "unknown length");
        assert!(verify_complete("vid", 0, Some(0)).is_err(), "empty");
        assert!(verify_complete("vid", 0, None).is_err(), "empty");
        assert!(
            verify_complete("vid", 9, Some(10)).is_err(),
            "one byte short is not complete"
        );
        assert!(
            verify_complete("vid", 11, Some(10)).is_err(),
            "one byte over is not complete"
        );
    }
}
