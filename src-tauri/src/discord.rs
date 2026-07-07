//! Discord Rich Presence: advertises the current track as a "Listening to
//! Rift" status on the user's Discord profile.
//!
//! Mirrors the [`crate::media`] design. The `DiscordIpcClient` blocks on a
//! local socket and is cheapest to own from a single place, so it lives
//! entirely on a dedicated thread; the player talks to that thread through the
//! cheap, `Send + Sync` [`DiscordHandle`] stored in `AppState`. The thread
//! keeps the last-known track/state and rebuilds the activity on every update,
//! so progress is carried by start/end timestamps rather than per-second ticks.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use discord_rich_presence::activity::{Activity, ActivityType, Assets, Button, Timestamps};
use discord_rich_presence::{DiscordIpc, DiscordIpcClient};
use rift_types::{PlaybackState, Track};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tracing::{debug, warn};

/// Shortest spacing between pushes to Discord. Discord only re-renders the
/// displayed activity on an internal ~15s cadence: an update sent *inside* that
/// window is accepted (`evt:null`) but never shown, and — because it becomes the
/// "last received" payload — identical re-sends afterward are deduped, so the
/// presence sticks on the previous song until a genuinely different activity
/// arrives. So we never send inside the window: a rapid switch is held and only
/// the latest snapshot is sent once the window has passed, guaranteeing Discord's
/// first receipt of the new song lands past the cadence and actually renders.
/// The cost is up to this much lag on back-to-back switches — unavoidable, it's
/// Discord's own refresh rate. An isolated switch after a quiet spell still goes
/// out immediately.
const MIN_INTERVAL: Duration = Duration::from_secs(15);

/// Poll/coalesce granularity: how often the loop wakes with no new command, to
/// flush a change that was waiting on the interval and to check the heartbeat.
const POLL: Duration = Duration::from_secs(1);

/// Idle re-assert: with nothing changing we still re-send occasionally so a
/// reconnect is retried after Discord is launched/restarted. Kept well above
/// [`MIN_INTERVAL`] so it rarely delays a real change, and long because the
/// re-send is otherwise a no-op (Discord dedups an unchanged activity).
const HEARTBEAT: Duration = Duration::from_secs(60);

/// Rift's Discord application ID (Developer Portal → General Information). It
/// sets the app name shown as "Listening to Rift" and hosts uploaded art.
const APP_ID: &str = "1504731045847634042";

/// Asset key of a logo uploaded under the app's Rich Presence → Art Assets,
/// shown as a small badge over the cover. Leave empty to show only the cover.
const LOGO_ASSET: &str = "rift_logo";

/// Rich Presence button linking viewers to the project.
const GITHUB_LABEL: &str = "View on GitHub";
const GITHUB_URL: &str = "https://github.com/gage-lodba/Rift";

enum DiscordCmd {
    Track {
        title: String,
        artist: String,
        cover: String,
        duration: f64,
    },
    State {
        state: PlaybackState,
        position: f64,
    },
    /// The authoritative length read from the decoder, which can arrive after
    /// playback started (metadata duration was missing or wrong). Carries the
    /// current position so the elapsed bar anchors correctly.
    Duration {
        duration: f64,
        position: f64,
    },
    Enabled(bool),
}

/// Cloneable handle the player uses to update the Discord presence.
#[derive(Clone)]
pub struct DiscordHandle(Option<UnboundedSender<DiscordCmd>>);

impl DiscordHandle {
    pub fn set_track(&self, track: &Track, duration: f64) {
        if let Some(tx) = &self.0 {
            let _ = tx.send(DiscordCmd::Track {
                title: track.title.clone(),
                artist: track.artist.clone(),
                cover: track.cover.clone(),
                duration,
            });
        }
    }

    pub fn set_state(&self, state: PlaybackState, position: f64) {
        if let Some(tx) = &self.0 {
            let _ = tx.send(DiscordCmd::State { state, position });
        }
    }

    pub fn set_duration(&self, duration: f64, position: f64) {
        if let Some(tx) = &self.0 {
            let _ = tx.send(DiscordCmd::Duration { duration, position });
        }
    }

    pub fn set_enabled(&self, enabled: bool) {
        if let Some(tx) = &self.0 {
            let _ = tx.send(DiscordCmd::Enabled(enabled));
        }
    }
}

pub fn spawn(enabled: bool) -> DiscordHandle {
    let (tx, rx) = mpsc::unbounded_channel();
    match std::thread::Builder::new()
        .name("rift-discord".into())
        .spawn(move || run(rx, enabled))
    {
        Ok(_) => DiscordHandle(Some(tx)),
        Err(e) => {
            warn!("could not spawn discord thread: {e}");
            DiscordHandle(None)
        }
    }
}

/// The last-known now-playing snapshot the thread rebuilds the activity from.
struct Now {
    title: String,
    artist: String,
    cover: String,
    duration: f64,
    state: PlaybackState,
    /// Absolute `(start_ms, end_ms)` for Discord's elapsed/remaining bar, in
    /// Unix time. Computed once when playback (re)anchors — a track change, a
    /// resume, or a seek — and then left alone: Discord animates the bar itself
    /// from these fixed endpoints, so re-asserting the same activity on the
    /// heartbeat must reuse them rather than recompute from a stale position
    /// (which would jerk the bar back a step every [`HEARTBEAT`]). `None`
    /// whenever no bar should show (loading, paused, or unknown duration).
    anchor: Option<(i64, i64)>,
}

fn run(rx: UnboundedReceiver<DiscordCmd>, enabled: bool) {
    // The IPC client blocks, but the loop needs timers (coalescing + heartbeat),
    // so drive it on a tiny current-thread runtime rather than `blocking_recv`.
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            warn!("could not start discord runtime, presence disabled: {e}");
            return;
        }
    };
    rt.block_on(event_loop(rx, enabled));
}

/// Absolute `(start_ms, end_ms)` for Discord's elapsed/remaining bar, or `None`
/// when no bar should show (not playing, or unknown length). Anchored to wall
/// time from the current position so Discord animates the rest itself.
fn anchor(state: PlaybackState, duration: f64, position: f64) -> Option<(i64, i64)> {
    if matches!(state, PlaybackState::Playing) && duration > 0.0 {
        let start = unix_millis() - (position * 1000.0) as i64;
        Some((start, start + (duration * 1000.0) as i64))
    } else {
        None
    }
}

/// Apply one command to the retained snapshot. Kept separate so a burst can be
/// drained and folded in before a single push.
fn fold(cmd: DiscordCmd, now: &mut Option<Now>, enabled: &mut bool) {
    match cmd {
        DiscordCmd::Track {
            title,
            artist,
            cover,
            duration,
        } => {
            *now = Some(Now {
                title,
                artist,
                cover,
                duration,
                // A fresh track starts loading from the top (no bar yet); the
                // following State update flips it to Playing and anchors the bar.
                state: PlaybackState::Loading,
                anchor: None,
            });
        }
        DiscordCmd::State { state, position } => {
            if let Some(n) = now {
                n.state = state;
                // Recomputed on every state change so a resume or seek re-syncs
                // the bar, while heartbeats leave it fixed.
                n.anchor = anchor(state, n.duration, position);
            }
        }
        DiscordCmd::Duration { duration, position } => {
            if let Some(n) = now {
                n.duration = duration;
                // The true length can land after the Playing transition, when
                // the bar was anchored with a zero/placeholder duration (so it
                // didn't show). Re-anchor now that the length is known.
                n.anchor = anchor(n.state, duration, position);
            }
        }
        DiscordCmd::Enabled(on) => *enabled = on,
    }
}

async fn event_loop(mut rx: UnboundedReceiver<DiscordCmd>, mut enabled: bool) {
    let mut client = DiscordIpcClient::new(APP_ID);
    // Whether the IPC socket is currently connected. Discord may be closed when
    // Rift starts (or restarted later), so we connect lazily and reconnect on
    // the next push if one fails.
    let mut connected = false;
    let mut now: Option<Now> = None;

    // A change is pending a push; `last_send` gates how soon it can go out.
    let mut dirty = false;
    // Seed so the first change goes out immediately (not held for MIN_INTERVAL).
    let mut last_send = Instant::now() - HEARTBEAT;

    let mut ticker = tokio::time::interval(POLL);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            cmd = rx.recv() => {
                let Some(cmd) = cmd else { break }; // handle dropped: shut down
                fold(cmd, &mut now, &mut enabled);
                // Fold any commands already queued behind it, so a burst (rapid
                // skips, track+state for one song) collapses into one push of
                // the final state instead of several.
                while let Ok(cmd) = rx.try_recv() {
                    fold(cmd, &mut now, &mut enabled);
                }
                dirty = true;
            }
            _ = ticker.tick() => {}
        }

        // Send a pending change once MIN_INTERVAL has passed since the last push
        // (so it never lands inside Discord's refresh window), or re-assert on
        // the slower heartbeat to retry a reconnect. A change arriving during
        // the window waits here and coalesces to the latest, so back-to-back
        // switches send only the final song, once the window clears.
        let elapsed = last_send.elapsed();
        if (dirty && elapsed >= MIN_INTERVAL) || elapsed >= HEARTBEAT {
            // Only advance the interval clock when a push actually went out. A
            // Loading snapshot is skipped (the previous track stays up), so if it
            // reset `last_send` the real Playing push moments later would be held
            // for a whole MIN_INTERVAL — the new song wouldn't show for ~15s.
            if apply(&mut client, &mut connected, enabled, now.as_ref()) {
                last_send = Instant::now();
            }
            dirty = false;
        }
    }
}

/// Push the current snapshot to Discord, clearing the presence when disabled,
/// stopped, or idle. Connects on demand and drops the connection flag on
/// failure so the next update retries. Returns whether an IPC operation was
/// attempted, so the caller only paces the interval on a real push (a skipped
/// Loading frame must not consume the send slot).
fn apply(
    client: &mut DiscordIpcClient,
    connected: &mut bool,
    enabled: bool,
    now: Option<&Now>,
) -> bool {
    // While a new track is still loading, leave the current presence untouched
    // rather than push a bar-less entry (the bar is withheld until Playing so it
    // can't run ahead during the buffer). The Playing push that follows replaces
    // it with the real time bar, so a switch reads old-track-with-bar ->
    // new-track-with-bar instead of flashing a barless name in between.
    if enabled && matches!(now.map(|n| n.state), Some(PlaybackState::Loading)) {
        return false;
    }
    let show = enabled
        && matches!(
            now.map(|n| n.state),
            Some(PlaybackState::Playing | PlaybackState::Paused)
        );
    if !show {
        if *connected {
            match client.clear_activity() {
                Ok(()) => drain(client, connected),
                Err(_) => *connected = false,
            }
            return true;
        }
        return false;
    }
    let Some(n) = now else { return false };

    if !*connected {
        if let Err(e) = client.connect() {
            debug!("discord not available: {e}");
            // A connect attempt is paced so a closed Discord isn't probed every
            // poll; the heartbeat retries it.
            return true;
        }
        debug!("discord connected");
        *connected = true;
    }

    if let Err(e) = client.set_activity(build_activity(n)) {
        // The socket likely went away (Discord closed); retry on the next tick.
        warn!("discord set_activity failed: {e}");
        *connected = false;
        return true;
    }
    // Drain Discord's response to this command. The library's set_activity only
    // writes and never reads the reply, so left unread the pipe's buffer backs
    // up until Discord stalls applying our updates — writes keep returning Ok
    // while the presence freezes on an old track. Reading it also surfaces
    // rate-limit ERROR replies and detects a dead pipe (recv Err -> reconnect).
    match client.recv() {
        Ok((_, val)) => {
            if val.get("evt").and_then(|e| e.as_str()) == Some("ERROR") {
                warn!("discord rejected activity: {val}");
            } else {
                debug!(
                    "discord push: \"{}\" state={:?} bar={}",
                    n.title,
                    n.state,
                    n.anchor.is_some()
                );
            }
        }
        Err(e) => {
            warn!("discord recv failed, reconnecting next tick: {e}");
            *connected = false;
        }
    }
    true
}

/// Read and discard one response frame (after a `clear_activity`), dropping the
/// connection on a broken pipe so the next push reconnects.
fn drain(client: &mut DiscordIpcClient, connected: &mut bool) {
    if client.recv().is_err() {
        *connected = false;
    }
}

fn build_activity(n: &Now) -> Activity<'_> {
    let mut assets = Assets::new();
    if !n.cover.is_empty() {
        assets = assets.large_image(n.cover.as_str());
    }
    if !LOGO_ASSET.is_empty() {
        assets = assets.small_image(LOGO_ASSET).small_text("Rift");
    }

    let mut activity = Activity::new()
        .activity_type(ActivityType::Listening)
        .details(n.title.as_str())
        .state(n.artist.as_str())
        .assets(assets)
        .buttons(vec![Button::new(GITHUB_LABEL, GITHUB_URL)]);

    // A start+end pair renders Discord's elapsed/remaining time bar. The
    // endpoints are anchored once per state change (see [`Now::anchor`]) so
    // heartbeat re-asserts keep the exact same bar.
    if let Some((start, end)) = n.anchor {
        activity = activity.timestamps(Timestamps::new().start(start).end(end));
    }
    activity
}

fn unix_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
