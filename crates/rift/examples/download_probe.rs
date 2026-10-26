//! Manual live end-to-end probe: download one track through the **real
//! backend** and print the per-track progress snapshots the desktop UI consumes.
//!
//! This is the Phase 4 companion to `native_probe` (which exercises the fetch
//! functions directly): it constructs `RiftBackend` against a throwaway data
//! directory, calls `download_tracks` exactly as the UI's Download action does,
//! drains the event bus, and reports the `DownloadState.progress` values. It
//! then decodes the saved offline file with the playback decoder, constructs a
//! **second backend** on the same directory to prove the startup scan discovers
//! the copy, and replays it through `fetch::fetch_track` (the offline-first path
//! playback uses). Not a CI test: it needs network.
//!
//! ```text
//! cargo run -p rift --example download_probe            # default track
//! cargo run -p rift --example download_probe -- <video-id>
//! ```
//!
//! Audio is resolved and downloaded natively; the probe needs no external
//! binary. Exit status: 0 when the track was claimed, reported progress,
//! finished `downloaded`, and the saved file decodes; 1 otherwise. The throwaway
//! data directory is deleted afterwards, so the user's real library is never
//! touched.

use std::io::Cursor;
use std::path::Path;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context};
use rift::{BackendConfig, BackendEvent, RiftBackend};
use rift_types::Track;
use rodio::{Decoder, Source};

/// ~3.4 MB / 3.5 min: long enough to observe several progress snapshots on a
/// normal connection, small enough to keep the probe quick.
const DEFAULT_ID: &str = "dQw4w9WgXcQ";
/// Whole-probe ceiling: resolution + transfer of a full track on a slow link.
const PROBE_TIMEOUT: Duration = Duration::from_secs(120);

fn main() -> anyhow::Result<()> {
    // Surface the backend's own logs (`native stream: downloaded …`) when the
    // caller sets RUST_LOG, so the probe report shows the served path.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init();

    let id = std::env::args()
        .skip(1)
        .find(|arg| !arg.starts_with('-'))
        .unwrap_or_else(|| DEFAULT_ID.to_owned());

    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before the Unix epoch")?
        .as_millis();
    let data_dir = std::env::temp_dir().join(format!(
        "rift-download-probe-{}-{unique}",
        std::process::id()
    ));
    std::fs::create_dir_all(&data_dir).context("could not create the throwaway data directory")?;
    // An upgrading install can still carry the legacy yt-dlp surface: a
    // `yt_dlp_path` key in settings.json and a binary under bin/. The backend
    // must load the settings and leave the files alone (it never calls or
    // deletes them). Seed both so this probe proves that on the real backend.
    seed_legacy_install(&data_dir)?;

    println!("download_probe: id={id} data_dir={}\n", data_dir.display(),);

    let result = run(&id, &data_dir);
    // `run` cleans up on its own exit paths; this covers an early failure
    // before it takes ownership of the store (e.g. backend construction).
    let _ = std::fs::remove_dir_all(&data_dir);
    result
}

fn run(id: &str, data_dir: &std::path::Path) -> anyhow::Result<()> {
    let (backend, mut events) = RiftBackend::new(BackendConfig::new().with_data_dir(data_dir))
        .map_err(|error| anyhow::anyhow!("could not construct the backend: {error:?}"))?;
    // Legacy-install compatibility, checked on the real backend: the unknown
    // `yt_dlp_path` key must not stop settings from loading (volume survives),
    // and the stale binary under bin/ must still be there afterwards.
    let bootstrap = backend.bootstrap();
    anyhow::ensure!(
        (bootstrap.volume - 0.5).abs() < f32::EPSILON,
        "the legacy settings.json should still load (volume {}, expected 0.5)",
        bootstrap.volume
    );
    anyhow::ensure!(
        data_dir.join("bin").join("yt-dlp.exe").is_file(),
        "the backend must not delete the legacy bin/yt-dlp.exe"
    );
    println!("legacy install: settings loaded, stale bin/yt-dlp.exe left untouched");
    let handle = backend.handle();

    let track = Track {
        id: id.to_owned(),
        title: format!("probe {id}"),
        artist: "probe".to_owned(),
        album: None,
        duration: None,
        cover: String::new(),
        artists: Vec::new(),
        album_id: None,
    };
    handle.download_tracks(vec![track]);

    let deadline = Instant::now() + PROBE_TIMEOUT;
    let mut samples = 0usize;
    let mut last_progress: Option<u16> = None;
    let mut downloaded = false;
    let mut failure: Option<String> = None;
    while Instant::now() < deadline {
        for event in events.drain() {
            match event {
                BackendEvent::Downloads(state) => {
                    if let Some(permille) = state.progress.get(id) {
                        if last_progress != Some(*permille) {
                            println!("progress {permille:>4}‰");
                            last_progress = Some(*permille);
                            samples += 1;
                        }
                    }
                    if state.downloaded.contains(id) {
                        downloaded = true;
                    }
                    if state.failed.contains(id) {
                        failure = Some("the backend marked the download failed".to_owned());
                    }
                }
                BackendEvent::Error(message) => {
                    eprintln!("backend error: {message}");
                    failure = Some(message);
                }
                _ => {}
            }
        }
        if downloaded || failure.is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }

    let file = data_dir.join("downloads").join(format!("{id}.m4a"));
    let saved = std::fs::read(&file).ok();
    let decoded = saved.as_deref().map(decode_length);
    let bytes = saved.as_ref().map_or(0, Vec::len);
    backend.shutdown();

    // Restart-equivalent checks, after shutdown so nothing holds the files: a
    // second backend must discover the copy from its startup scan, and
    // `fetch_track` must serve it from disk (the offline-first path playback
    // uses after a relaunch).
    let offline_check = saved.as_deref().map_or_else(
        || Err("the offline file is missing".to_owned()),
        |saved_bytes| restart_and_replay(data_dir, id, saved_bytes),
    );

    let summary = match (&failure, downloaded, &decoded) {
        (Some(error), ..) => format!("FAILED: {error}"),
        (None, false, _) => "FAILED: timed out before the track was reported downloaded".to_owned(),
        (None, true, Some(Err(error))) => format!("FAILED: saved file does not decode: {error:#}"),
        (None, true, None) => format!("FAILED: reported downloaded but {id}.m4a is missing"),
        // The feature under test is the determinate bar: a download that
        // completed without a single progress fraction reaching the event bus
        // is a regression, not a pass.
        (None, true, Some(Ok(_))) if samples == 0 => {
            "FAILED: the download completed without reporting a progress fraction".to_owned()
        }
        (None, true, Some(Ok(secs))) => match &offline_check {
            Err(error) => format!("FAILED: restart/offline replay check: {error}"),
            Ok(()) => format!(
                "PASS: downloaded {bytes} bytes, {samples} progress snapshot(s), decoded {secs:.2}s; \
                 restart detects the offline copy and fetch_track replays it without the network"
            ),
        },
    };
    let ok = summary.starts_with("PASS");

    // Best-effort cleanup: both backends are shut down, so nothing holds the files.
    let _ = std::fs::remove_dir_all(data_dir);
    println!("\n{summary}");
    if ok {
        Ok(())
    } else {
        bail!("{summary}");
    }
}

/// Restart-equivalent checks: a fresh backend discovers `id` from its startup
/// directory scan, then `fetch_track` returns the saved bytes from disk.
fn restart_and_replay(data_dir: &Path, id: &str, expected: &[u8]) -> Result<(), String> {
    let (backend, _events) = RiftBackend::new(BackendConfig::new().with_data_dir(data_dir))
        .map_err(|error| format!("could not construct a second backend: {error:?}"))?;
    let discovered = backend.downloads_state().downloaded.contains(id);
    backend.shutdown();
    if !discovered {
        return Err("a restarted backend did not discover the saved offline file".to_owned());
    }

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("could not build a runtime for the offline replay: {error}"))?;
    let downloads_dir = data_dir.join("downloads");
    let (bytes, _) = runtime
        .block_on(rift::fetch::fetch_track(&downloads_dir, id))
        .map_err(|error| format!("offline replay failed: {error:#}"))?;
    if bytes != expected {
        return Err(format!(
            "offline replay returned {} bytes, expected {}",
            bytes.len(),
            expected.len()
        ));
    }
    Ok(())
}

/// Seed a throwaway data dir the way an upgrading install looks: a legacy
/// `settings.json` with the removed `yt_dlp_path` key, and a stale binary in
/// `bin/`. Neither may stop the native backend.
fn seed_legacy_install(data_dir: &Path) -> anyhow::Result<()> {
    let bin = data_dir.join("bin");
    std::fs::create_dir_all(&bin).context("could not create the legacy bin directory")?;
    std::fs::write(
        bin.join("yt-dlp.exe"),
        b"stale legacy binary, never executed",
    )
    .context("could not seed the legacy binary")?;
    std::fs::write(
        data_dir.join("settings.json"),
        br#"{ "volume": 0.5, "yt_dlp_path": "C:\\tools\\yt-dlp.exe" }"#,
    )
    .context("could not seed the legacy settings.json")?;
    Ok(())
}

/// Length of the saved offline file as the playback decoder sees it.
fn decode_length(bytes: &[u8]) -> anyhow::Result<f64> {
    let len = bytes.len() as u64;
    let decoder = Decoder::builder()
        .with_data(Cursor::new(bytes.to_vec()))
        .with_byte_len(len)
        .with_seekable(true)
        .build()
        .context("decoder construction failed")?;
    let secs = decoder
        .total_duration()
        .context("the decoded stream reports no total duration")?;
    Ok(secs.as_secs_f64())
}
