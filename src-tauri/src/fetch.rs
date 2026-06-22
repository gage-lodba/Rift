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

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Mutex, OnceLock};

use anyhow::{anyhow, bail, Context};
use reqwest::header;

/// Windows process creation flag: don't allocate a console for the child.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
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
    let mut cmd = tokio::process::Command::new(ytdlp_path());
    cmd.arg("-f")
        .arg("140/bestaudio[ext=m4a]")
        .arg("--no-playlist")
        .arg("--quiet")
        .arg("--no-warnings")
        .arg("--force-overwrites")
        .arg("-o")
        .arg(&path)
        .arg(format!("https://www.youtube.com/watch?v={video_id}"));

    // Without CREATE_NO_WINDOW, spawning a console subprocess from a GUI app
    // flashes a console window on Windows. No-op elsewhere.
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);

    let output = cmd
        .output()
        .await
        .context("could not run yt-dlp — install it (https://github.com/yt-dlp/yt-dlp#installation), or set RIFT_YTDLP to its full path")?;

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

/// The yt-dlp binary name (platform-specific).
#[cfg(windows)]
const YTDLP_BIN: &str = "yt-dlp.exe";
#[cfg(not(windows))]
const YTDLP_BIN: &str = "yt-dlp";

/// Resolve the yt-dlp executable to an absolute path, cached for the session.
///
/// GUI-launched apps don't inherit the interactive shell's `PATH` (a
/// desktop-menu launch on Linux/macOS sees only the session `PATH`, which
/// usually omits `~/.local/bin`), so `Command::new("yt-dlp")` finds nothing
/// even though it works under `cargo tauri dev` from a terminal. We look in an
/// explicit override, then `PATH`, then the usual install dirs, and fall back
/// to the bare name so the "not found" error still reads sensibly.
/// Probe the system for a usable yt-dlp: resolve its path, then run
/// `yt-dlp --version` to confirm it actually executes (a resolved path that
/// fails to run — wrong arch, missing perms — reports `found: false`).
pub async fn detect_ytdlp() -> rift_types::YtDlpStatus {
    let path = ytdlp_path();
    let path_str = path.display().to_string();

    let mut cmd = tokio::process::Command::new(&path);
    cmd.arg("--version");
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);

    match cmd.output().await {
        Ok(out) if out.status.success() => rift_types::YtDlpStatus {
            found: true,
            path: Some(path_str),
            version: Some(String::from_utf8_lossy(&out.stdout).trim().to_string()),
        },
        _ => rift_types::YtDlpStatus {
            found: false,
            path: path.is_file().then_some(path_str),
            version: None,
        },
    }
}

/// The standalone yt-dlp release asset for the current platform (no Python
/// runtime needed on Windows/macOS/Linux; the generic zipapp is a last resort).
fn ytdlp_release_asset() -> &'static str {
    if cfg!(target_os = "windows") {
        "yt-dlp.exe"
    } else if cfg!(target_os = "macos") {
        "yt-dlp_macos"
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        "yt-dlp_linux_aarch64"
    } else if cfg!(target_os = "linux") {
        "yt-dlp_linux"
    } else {
        "yt-dlp"
    }
}

/// Download the latest yt-dlp standalone binary for this platform into
/// `dest_dir`, mark it executable, and return its path. Used by Settings when
/// yt-dlp isn't already installed.
pub async fn download_ytdlp(dest_dir: &Path, http: &reqwest::Client) -> anyhow::Result<PathBuf> {
    let url = format!(
        "https://github.com/yt-dlp/yt-dlp/releases/latest/download/{}",
        ytdlp_release_asset()
    );
    info!("downloading yt-dlp from {url}");
    let bytes = http
        .get(&url)
        .header(header::USER_AGENT, "rift")
        .send()
        .await
        .context("download request failed")?
        .error_for_status()
        .context("download request returned an error status")?
        .bytes()
        .await
        .context("could not read the downloaded yt-dlp")?;

    tokio::fs::create_dir_all(dest_dir)
        .await
        .context("could not create the destination directory")?;
    let dest = dest_dir.join(YTDLP_BIN);
    tokio::fs::write(&dest, &bytes)
        .await
        .context("could not write the yt-dlp binary")?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = tokio::fs::metadata(&dest).await?.permissions();
        perms.set_mode(0o755);
        tokio::fs::set_permissions(&dest, perms)
            .await
            .context("could not mark yt-dlp executable")?;
    }

    info!("downloaded yt-dlp to {}", dest.display());
    Ok(dest)
}

/// User-configured custom yt-dlp location, set from Settings at startup and
/// whenever it changes. Takes priority over PATH/common-location detection.
static CUSTOM_YTDLP: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Set (or clear with `None`) the custom yt-dlp path. Applied on the next fetch
/// or probe; an invalid path falls back to auto-detection.
pub fn set_ytdlp_override(path: Option<PathBuf>) {
    *CUSTOM_YTDLP
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = path;
}

/// Resolve the yt-dlp executable: the user's configured path if set and valid,
/// otherwise the auto-detected location.
fn ytdlp_path() -> PathBuf {
    if let Some(p) = CUSTOM_YTDLP
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
    {
        if p.is_file() {
            return p;
        }
        warn!(
            "configured yt-dlp path {} is not a file; falling back to auto-detection",
            p.display()
        );
    }
    autodetect_ytdlp()
}

fn autodetect_ytdlp() -> PathBuf {
    // Cache only a *successful* resolution. A not-found result is deliberately
    // not memoized so installing yt-dlp at runtime (e.g. via a package manager)
    // is picked up without an app restart.
    static RESOLVED: OnceLock<PathBuf> = OnceLock::new();
    if let Some(p) = RESOLVED.get() {
        return p.clone();
    }

    if let Some(found) = probe_ytdlp_locations() {
        // First writer wins; either way return the cached value.
        let _ = RESOLVED.set(found);
        return RESOLVED.get().expect("just set").clone();
    }

    // Give up gracefully — let the spawn fail with a clear error. Not cached, so
    // the next call re-probes.
    warn!("yt-dlp not found in PATH or common locations; falling back to bare name");
    PathBuf::from(YTDLP_BIN)
}

/// Look for yt-dlp in the override env var, then PATH, then common install dirs.
/// Returns the first existing file, or `None` if nothing is found.
fn probe_ytdlp_locations() -> Option<PathBuf> {
    // 1. Explicit override (set in the app's launcher or environment).
    if let Some(p) = std::env::var_os("RIFT_YTDLP") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
    }

    // 2. Honor PATH if it happens to contain yt-dlp (e.g. terminal launch).
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            let candidate = dir.join(YTDLP_BIN);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }

    // 3. Common install locations not always on a GUI session's PATH.
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        dirs.push(PathBuf::from(&home).join(".local/bin"));
        dirs.push(PathBuf::from(&home).join("bin"));
    }
    dirs.extend(
        ["/usr/local/bin", "/usr/bin", "/bin", "/opt/homebrew/bin"]
            .iter()
            .map(PathBuf::from),
    );
    for dir in dirs {
        let candidate = dir.join(YTDLP_BIN);
        if candidate.is_file() {
            info!("resolved yt-dlp at {}", candidate.display());
            return Some(candidate);
        }
    }

    None
}
