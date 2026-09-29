//! Stream resolution and download via yt-dlp.
//!
//! Rift's audio pipeline shells out to yt-dlp to resolve a track's best m4a
//! stream and download it to a temp file, read back into memory for the
//! decoder. yt-dlp is actively maintained against YouTube's changes and owns
//! the signature/`n` deobfuscation and PO-token handling that a pure-Rust
//! client would have to chase constantly.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use anyhow::{bail, Context};
use reqwest::header;
use tracing::{debug, info, warn};

/// Windows process creation flag: don't allocate a console for the child.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Sanitize a video/track id into a safe single filename component. Real
/// YouTube ids are `[A-Za-z0-9_-]` and pass through unchanged; a playlist
/// imported from an arbitrary JSON file can carry any string, so everything
/// else (path separators, `..`, whitespace) folds to `_`. This keeps a
/// malicious id from escaping the downloads/temp directory when it is used to
/// build a file path.
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
/// and falling back to a network fetch. Returns the audio bytes and duration
/// in seconds (0.0 if unknown — the decoder reports the real length).
pub async fn fetch_track(
    downloads_dir: &std::path::Path,
    video_id: &str,
) -> anyhow::Result<(Vec<u8>, f64)> {
    let local = downloads_dir.join(format!("{}.m4a", safe_stem(video_id)));
    if local.exists() {
        if let Ok(data) = tokio::fs::read(&local).await {
            debug!("playing {video_id} from offline download");
            return Ok((data, 0.0));
        }
    }
    fetch_bytes(video_id).await
}

/// Resolve a YouTube video ID to its best m4a audio stream and download it
/// fully into memory. Returns the audio data and its duration in seconds
/// (0.0 if unknown — yt-dlp is asked for the stream only, so the decoder
/// reports the real length).
pub async fn fetch_bytes(video_id: &str) -> anyhow::Result<(Vec<u8>, f64)> {
    fetch_via_ytdlp(video_id).await
}

/// Upper bound on a single yt-dlp run. Generous — it covers resolving and
/// downloading a full audio stream on a slow link — but finite: without it a
/// hung yt-dlp wedges the fetch (and, for a download worker, its
/// `Downloads::begin` claim) for the rest of the session.
const YTDLP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// Wall-clock ceiling for the one-click installer's asset download. The
/// installer reuses the player's HTTP client, whose 30 s ceiling is right for
/// status calls but too tight for a multi-megabyte binary.
const YTDLP_DOWNLOAD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

async fn fetch_via_ytdlp(video_id: &str) -> anyhow::Result<(Vec<u8>, f64)> {
    // A random suffix keeps concurrent fetches of the same id from writing to —
    // and reading back — the same temp file.
    let path = std::env::temp_dir().join(format!(
        "rift-{}-{:016x}.m4a",
        safe_stem(video_id),
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
        .arg(format!("https://www.youtube.com/watch?v={video_id}"))
        // If the `timeout` below fires, the child is killed as the future is
        // dropped rather than left running against a temp file we delete.
        .kill_on_drop(true);

    // Without CREATE_NO_WINDOW, spawning a console subprocess from a GUI app
    // flashes a console window on Windows. No-op elsewhere.
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);

    let output = match tokio::time::timeout(YTDLP_TIMEOUT, cmd.output()).await {
        Ok(result) => result.context("could not run yt-dlp — install it (https://github.com/yt-dlp/yt-dlp#installation), or set RIFT_YTDLP to its full path")?,
        Err(_) => {
            // `kill_on_drop` above reaps the child as the timed-out future is
            // dropped, so nothing keeps writing to the temp file we remove.
            let _ = tokio::fs::remove_file(&path).await;
            bail!(
                "yt-dlp timed out after {}s resolving {video_id}",
                YTDLP_TIMEOUT.as_secs()
            );
        }
    };

    if !output.status.success() {
        // yt-dlp may have left a partial output file behind.
        let _ = tokio::fs::remove_file(&path).await;
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
        // Override the client's short status-call ceiling: this is a
        // multi-megabyte binary, not a JSON probe.
        .timeout(YTDLP_DOWNLOAD_TIMEOUT)
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

#[cfg(test)]
mod tests {
    use super::safe_stem;

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
}
