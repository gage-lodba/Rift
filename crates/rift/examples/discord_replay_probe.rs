//! Manual live probe for the Discord Rich Presence **replay timer reset**.
//!
//! Plays one short real track through the real backend with repeat-one enabled
//! and waits for it to end and loop **naturally** (no seeking — a seek would put
//! the presence's progress bar at an artificial position and muddy the check).
//! The probe proves the state-machine half of the fix: the player really does
//! restart the same track and emit a second `Track`/`Playing` pair, which is what
//! marks the presence for a forced refresh.
//!
//! Run with the Discord log enabled to see the presence side of the fix:
//!
//! ```text
//! # Windows PowerShell
//! $env:RUST_LOG='rift=debug'
//! cargo run -p rift --example discord_replay_probe
//! ```
//!
//! Expected log lines around the replay:
//!
//! ```text
//! track ended, advancing queue
//! same track restarted; clearing the Discord presence before re-posting
//! discord push: "<title>" state=Playing bar=true
//! ```
//!
//! Watch your Discord profile: when the track loops, the presence's elapsed timer
//! must restart (within Discord's update cadence; the probe's short track means
//! the refresh is not held by Rift's own 15 s pacing) instead of continuing from
//! the previous play. Audio is muted for the probe (`set_volume(0.0)`), so it is
//! safe to run while listening to something else. The throwaway data directory is
//! deleted afterwards; the real library is never touched.
//!
//! Exit status: 0 when two track starts and two Playing transitions were
//! observed, 1 otherwise. Not a CI test: it needs network, an audio device and a
//! running Discord client.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context};
use rift::{BackendConfig, BackendEvent, RiftBackend};
use rift_types::{PlaybackState, RepeatMode, Track};

/// "Me at the zoo": a real ~19 s video whose audio resolves and decodes. A short
/// track lets the probe observe a **natural** end + repeat quickly, without the
/// artificial position jump a seek would introduce.
const DEFAULT_ID: &str = "jNQXAC9IVRw";
/// Whole-probe ceiling: resolution + fetch + decode + the short playback window.
const PROBE_TIMEOUT: Duration = Duration::from_secs(90);
/// How long to keep draining after the replay is seen, so the Discord thread has
/// time to run the (paced) forced refresh and any post-refresh push.
const POST_REPLAY_GRACE: Duration = Duration::from_secs(10);
const POLL: Duration = Duration::from_millis(20);

fn main() -> anyhow::Result<()> {
    // `RUST_LOG=rift=debug` surfaces the player and Discord-thread lines quoted
    // in the module docs; without it the probe still reports its own PASS/FAIL.
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
        "rift-discord-replay-probe-{}-{unique}",
        std::process::id()
    ));
    std::fs::create_dir_all(&data_dir).context("could not create the throwaway data directory")?;

    println!(
        "discord_replay_probe: id={id} data_dir={}\n",
        data_dir.display()
    );
    println!("Run with RUST_LOG=rift=debug to see the forced-refresh line.\n");

    let result = run(&id, &data_dir);
    let _ = std::fs::remove_dir_all(&data_dir);
    result
}

fn run(id: &str, data_dir: &std::path::Path) -> anyhow::Result<()> {
    let (backend, mut events) = RiftBackend::new(BackendConfig::new().with_data_dir(data_dir))
        .map_err(|error| anyhow::anyhow!("could not construct the backend: {error:?}"))?;

    let handle = backend.handle();
    // Silent: this probe verifies the playback state machine and the presence
    // update, not the audio output.
    handle.set_volume(0.0);
    // Repeat Off -> All -> One.
    handle.cycle_repeat();
    handle.cycle_repeat();
    let snapshot = backend.queue_snapshot();
    if snapshot.repeat != RepeatMode::One {
        backend.shutdown();
        bail!(
            "could not enable repeat-one (mode is {:?})",
            snapshot.repeat
        );
    }

    let track = Track {
        id: id.to_owned(),
        title: "Discord replay probe".to_owned(),
        artist: "Rift".to_owned(),
        album: None,
        // Unknown on purpose: the decoder's real length lands after playback
        // starts, which is the normal path for tracks without metadata length.
        duration: None,
        cover: String::new(),
        artists: Vec::new(),
        album_id: None,
    };

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("could not build a runtime")?;
    runtime
        .block_on(handle.play_tracks(vec![track], 0, None))
        .map_err(|error| anyhow::anyhow!("play_tracks failed: {error}"))?;

    let deadline = Instant::now() + PROBE_TIMEOUT;
    let mut starts = 0u32;
    let mut playing = 0u32;
    let mut replay_seen = false;
    let mut replay_grace_end: Option<Instant> = None;
    let mut errors: Vec<String> = Vec::new();

    while Instant::now() < deadline {
        while let Some(event) = events.try_recv() {
            match event {
                BackendEvent::Track(track) => {
                    starts += 1;
                    println!(
                        "track start #{starts}: {} ({}), repeat={:?}",
                        track.title,
                        track.id,
                        backend.queue_snapshot().repeat
                    );
                }
                BackendEvent::State(PlaybackState::Playing) => {
                    playing += 1;
                    println!("playing #{playing}");
                    if playing >= 2 && !replay_seen {
                        replay_seen = true;
                        replay_grace_end = Some(Instant::now() + POST_REPLAY_GRACE);
                        println!(
                            "replay observed; waiting briefly for the Discord forced refresh \
                             (watch the presence timer now)"
                        );
                    }
                }
                BackendEvent::Error(message) => errors.push(message),
                _ => {}
            }
        }
        if let Some(grace_end) = replay_grace_end {
            if Instant::now() >= grace_end {
                break;
            }
        }
        // Do not spin forever before the first play starts (a bad id or a slow
        // network should fail through the error path above / the timeout).
        std::thread::sleep(POLL);
    }

    println!();
    if !errors.is_empty() {
        backend.shutdown();
        bail!("the backend reported errors: {errors:?}");
    }
    if !replay_seen || playing < 2 {
        backend.shutdown();
        bail!(
            "did not observe a natural repeat: {starts} track start(s), \
             {playing} Playing transition(s); check RUST_LOG=rift=debug"
        );
    }

    backend.shutdown();
    println!(
        "PASS: the same track restarted naturally through the real backend \
         ({starts} track start(s), {playing} Playing transition(s)). With \
         RUST_LOG=rift=debug the log must contain \
         \"same track restarted; clearing the Discord presence before re-posting\" \
         after the replay. Watch Discord: the elapsed timer must restart near 0:00."
    );
    Ok(())
}
