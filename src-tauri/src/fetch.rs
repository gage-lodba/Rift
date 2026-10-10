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

use crate::sha256;

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
/// Upper bound on the `yt-dlp --version` probe.
///
/// The probe is a local, trivial invocation, so this ceiling is not about a slow
/// one — it is about one that never finishes: a binary on a stalled network
/// mount, an executable held open by AV, or a wrapper script that waits on
/// something. `check_ytdlp` is the Settings view's "is yt-dlp usable?" check and
/// its spinner is the only feedback the view has, so an unbounded wait there is
/// a permanent "checking…" — the exact wedge [`YTDLP_TIMEOUT`] exists to prevent
/// on the fetch path, which this second subprocess wait was left out of.
const YTDLP_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

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

    let out = match run_version_probe(cmd, YTDLP_PROBE_TIMEOUT).await {
        Ok(out) => out,
        Err(ProbeError::TimedOut) => {
            warn!(
                "yt-dlp --version did not finish within {}s; reporting it as unusable",
                YTDLP_PROBE_TIMEOUT.as_secs()
            );
            return not_found(&path, path_str);
        }
        Err(ProbeError::Io(e)) => {
            debug!("could not run the yt-dlp probe at {}: {e}", path.display());
            return not_found(&path, path_str);
        }
    };

    if out.status.success() {
        rift_types::YtDlpStatus {
            found: true,
            path: Some(path_str),
            version: Some(String::from_utf8_lossy(&out.stdout).trim().to_string()),
        }
    } else {
        not_found(&path, path_str)
    }
}

/// `found: false`, keeping the resolved path visible when the file does exist so
/// Settings can say "there, but it doesn't run".
fn not_found(path: &Path, path_str: String) -> rift_types::YtDlpStatus {
    rift_types::YtDlpStatus {
        found: false,
        path: path.is_file().then_some(path_str),
        version: None,
    }
}

/// Why a version probe produced no version.
#[derive(Debug)]
enum ProbeError {
    /// The child was still running when the ceiling fired; dropping the
    /// timed-out future kills it (see [`run_version_probe`]).
    TimedOut,
    /// The child could not be spawned, or its output could not be read.
    Io(std::io::Error),
}

/// Run a version probe, waiting at most `limit` for the child.
async fn run_version_probe(
    mut cmd: tokio::process::Command,
    limit: std::time::Duration,
) -> Result<std::process::Output, ProbeError> {
    // A child that outlives the wait must not be left running: `kill_on_drop`
    // reaps it as the timed-out future is dropped.
    cmd.kill_on_drop(true);
    match tokio::time::timeout(limit, cmd.output()).await {
        Ok(Ok(out)) => Ok(out),
        Ok(Err(e)) => Err(ProbeError::Io(e)),
        Err(_) => Err(ProbeError::TimedOut),
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
/// The release the installer downloads from, and the checksum manifest that
/// comes with it.
const YTDLP_RELEASE_BASE: &str = "https://github.com/yt-dlp/yt-dlp/releases/latest/download";
/// yt-dlp publishes SHA-256 digests for every release asset here.
const YTDLP_SUMS_ASSET: &str = "SHA2-256SUMS";

pub async fn download_ytdlp(dest_dir: &Path, http: &reqwest::Client) -> anyhow::Result<PathBuf> {
    let asset = ytdlp_release_asset();
    let url = format!("{YTDLP_RELEASE_BASE}/{asset}");
    info!("downloading yt-dlp from {url}");

    // Fetch the checksum manifest *first*, so a manifest that cannot be
    // retrieved fails before anything is written to disk. Both requests use the
    // same `latest/download` base, so they resolve to the same release.
    let sums = fetch(http, &format!("{YTDLP_RELEASE_BASE}/{YTDLP_SUMS_ASSET}"))
        .await
        .context("could not download SHA2-256SUMS to verify the yt-dlp download")?;
    let sums_text = String::from_utf8_lossy(&sums);
    let Some(expected) = sha256::checksum_for(&sums_text, asset) else {
        // Named explicitly: the alternative (falling back to writing the binary
        // unverified) is exactly the behaviour this exists to prevent, and
        // "no entry for this asset" is a different problem from a bad digest.
        bail!(
            "the release's SHA2-256SUMS has no entry for {asset}; refusing to install an \
             unverified binary"
        );
    };

    let bytes = fetch(http, &url).await.context("download request failed")?;

    // Verify before anything is written or chmod'd: the file Rift later spawns
    // must be the one the manifest describes. "latest" plus no verification
    // would make a compromised release asset or TLS path into code execution.
    let actual = sha256::hex(&sha256::sha256(&bytes));
    if actual != expected {
        bail!(
            "the downloaded yt-dlp failed verification (expected {expected}, got {actual}); \
             refusing to install it"
        );
    }
    info!("verified yt-dlp {asset} (sha256 {actual})");

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

/// GET a URL fully into memory, with the installer's generous ceiling (these
/// are multi-megabyte release assets, not JSON probes).
async fn fetch(http: &reqwest::Client, url: &str) -> anyhow::Result<Vec<u8>> {
    Ok(http
        .get(url)
        .header(header::USER_AGENT, "rift")
        .timeout(YTDLP_DOWNLOAD_TIMEOUT)
        .send()
        .await?
        .error_for_status()
        .context("the server returned an error status")?
        .bytes()
        .await?
        .to_vec())
}

/// User-configured custom yt-dlp location, set from Settings at startup and
/// whenever it changes. Takes priority over PATH/common-location detection.
static CUSTOM_YTDLP: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Validate a user-supplied yt-dlp path before it is used to spawn a process.
///
/// Rift executes whatever this resolves to, and the path is persisted into
/// `settings.json`, so it is the one place the app turns a string from the
/// webview into a process. `Path::is_file()` alone accepts any path-shaped
/// string, including one carrying `..` or a symlink; canonicalizing first
/// resolves those to the real target, and requiring a regular file rejects a
/// directory or a device. A relative path is resolved against the process's
/// current directory, which for a GUI launch is not something the user chose —
/// so it is rejected outright rather than silently meaning something arbitrary.
///
/// This is defense-in-depth, not a sandbox: a caller who can already invoke
/// commands can point Rift at any *existing* executable on the machine. It
/// closes the "any path-shaped string" gap, which is what the review asked for.
pub fn validate_ytdlp_override(path: &str) -> anyhow::Result<PathBuf> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        bail!("no path given");
    }
    let raw = Path::new(trimmed);
    if !raw.is_absolute() {
        bail!("the path must be absolute");
    }
    let canonical =
        std::fs::canonicalize(raw).with_context(|| format!("could not resolve {trimmed}"))?;
    let meta = std::fs::metadata(&canonical)
        .with_context(|| format!("could not inspect {}", canonical.display()))?;
    if !meta.is_file() {
        bail!("{} is not a regular file", canonical.display());
    }
    Ok(canonical)
}

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
    use super::{run_version_probe, safe_stem, validate_ytdlp_override, ProbeError};

    /// A command that keeps running far longer than any ceiling this test
    /// applies, so only the ceiling can end the wait.
    #[cfg(windows)]
    fn sleeper_command() -> tokio::process::Command {
        // One loopback request per second, `-n` of them: no network needed.
        let mut cmd = tokio::process::Command::new("ping");
        cmd.args(["-n", "30", "127.0.0.1"]);
        cmd
    }

    #[cfg(unix)]
    fn sleeper_command() -> tokio::process::Command {
        let mut cmd = tokio::process::Command::new("sleep");
        cmd.arg("30");
        cmd
    }

    /// The Settings probe must give up on a child that never finishes.
    /// `check_ytdlp` can only report "not usable" once the probe *returns*, and
    /// its spinner is the only feedback the view has, so an unbounded wait there
    /// is a permanent "checking…". This pins the one subprocess wait H3's
    /// ceiling did not cover: revert it and this test waits out the sleeper and
    /// then fails on `Ok`.
    #[tokio::test]
    async fn a_version_probe_that_never_finishes_times_out_instead_of_waiting() {
        let started = std::time::Instant::now();
        let result =
            run_version_probe(sleeper_command(), std::time::Duration::from_millis(200)).await;

        assert!(
            matches!(result, Err(ProbeError::TimedOut)),
            "expected a timeout, got {result:?}"
        );
        assert!(
            started.elapsed() < std::time::Duration::from_secs(15),
            "the probe must return as soon as the ceiling fires, not when the child ends \
             (took {:?})",
            started.elapsed()
        );
    }

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

    /// A unique scratch directory, so parallel tests don't collide.
    fn scratch(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "rift-fetch-{tag}-{}-{:016x}",
            std::process::id(),
            rand::random::<u64>()
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    /// The path Rift will `spawn` must resolve to a real regular file. Every
    /// rejection here is a string that `Path::is_file()` on the raw input would
    /// have let through to a subprocess.
    #[test]
    fn ytdlp_override_rejects_anything_that_is_not_a_resolved_regular_file() {
        let dir = scratch("ytdlp-validate");
        let file = dir.join("yt-dlp");
        std::fs::write(&file, b"#!/bin/sh\n").expect("write fake binary");

        // A real file resolves, and comes back canonicalized.
        let resolved = validate_ytdlp_override(file.to_str().unwrap()).expect("a real file passes");
        assert_eq!(resolved, std::fs::canonicalize(&file).unwrap());

        // Whitespace around a pasted path is tolerated.
        let padded = format!("  {}  ", file.display());
        assert!(validate_ytdlp_override(&padded).is_ok());

        // A directory is not an executable.
        assert!(validate_ytdlp_override(dir.to_str().unwrap()).is_err());

        // A path that does not exist cannot be spawned.
        let missing = dir.join("nope");
        assert!(validate_ytdlp_override(missing.to_str().unwrap()).is_err());

        // A relative path would resolve against whatever the process's cwd
        // happens to be, which the user never chose.
        assert!(validate_ytdlp_override("yt-dlp").is_err());
        assert!(validate_ytdlp_override("./yt-dlp").is_err());

        // Empty and non-path input.
        assert!(validate_ytdlp_override("").is_err());
        assert!(validate_ytdlp_override("   ").is_err());

        std::fs::remove_dir_all(&dir).ok();
    }

    /// `..` segments must be resolved away rather than carried into the value
    /// that is persisted and later spawned.
    #[test]
    fn ytdlp_override_canonicalizes_parent_segments() {
        let dir = scratch("ytdlp-dots");
        let sub = dir.join("bin");
        std::fs::create_dir_all(&sub).expect("create subdir");
        let file = sub.join("yt-dlp");
        std::fs::write(&file, b"#!/bin/sh\n").expect("write fake binary");

        let roundabout = sub.join("..").join("bin").join("yt-dlp");
        let resolved = validate_ytdlp_override(roundabout.to_str().unwrap()).expect("resolves");
        assert_eq!(resolved, std::fs::canonicalize(&file).unwrap());
        assert!(
            !resolved.to_string_lossy().contains(".."),
            "the persisted path must not carry parent segments"
        );

        std::fs::remove_dir_all(&dir).ok();
    }
}
