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

/// Shortest spacing between pushes to Discord. Discord's IPC rate-limits
/// activity updates (~5 per 20s); bursts past that are dropped, which is how the
/// presence gets stuck on a track you already skipped. Commands that arrive
/// inside this window are coalesced into a single push of the latest snapshot.
const MIN_SEND: Duration = Duration::from_secs(2);

/// How often the current snapshot is re-asserted even without a new command.
/// This is the self-heal: a push Discord dropped (rate limit) or that failed (a
/// transient socket drop, or Discord launched after Rift) is re-sent within this
/// window, and a reconnect is retried, so the presence can't stay stale.
const HEARTBEAT: Duration = Duration::from_secs(15);

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
                // Anchor the elapsed bar only while actually playing a track of
                // known length; recomputed here (and only here) so a resume or
                // seek re-syncs it, while heartbeats leave it fixed.
                n.anchor = if matches!(state, PlaybackState::Playing) && n.duration > 0.0 {
                    let start = unix_millis() - (position * 1000.0) as i64;
                    Some((start, start + (n.duration * 1000.0) as i64))
                } else {
                    None
                };
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
    // Seed so the first push isn't held back by the min-send window.
    let mut last_send = Instant::now() - HEARTBEAT;
    // Wake at the coalescing granularity: frequent enough to flush a debounced
    // change promptly, and the same clock the heartbeat is measured against.
    let mut ticker = tokio::time::interval(MIN_SEND);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            cmd = rx.recv() => {
                let Some(cmd) = cmd else { break }; // handle dropped: shut down
                fold(cmd, &mut now, &mut enabled);
                // Fold any commands already queued behind it, so a burst (rapid
                // skips, track+state for one song) collapses into one push of
                // the final state instead of several that Discord would drop.
                while let Ok(cmd) = rx.try_recv() {
                    fold(cmd, &mut now, &mut enabled);
                }
                dirty = true;
            }
            _ = ticker.tick() => {}
        }

        let elapsed = last_send.elapsed();
        // Push a pending change once the min-send window has passed, or
        // re-assert the current snapshot on the heartbeat so a dropped or failed
        // update self-heals (and a lazy reconnect is retried).
        if (dirty && elapsed >= MIN_SEND) || elapsed >= HEARTBEAT {
            apply(&mut client, &mut connected, enabled, now.as_ref());
            last_send = Instant::now();
            dirty = false;
        }
    }
}

/// Push the current snapshot to Discord, clearing the presence when disabled,
/// stopped, or idle. Connects on demand and drops the connection flag on
/// failure so the next update retries.
fn apply(client: &mut DiscordIpcClient, connected: &mut bool, enabled: bool, now: Option<&Now>) {
    let show = enabled
        && matches!(
            now.map(|n| n.state),
            Some(PlaybackState::Playing | PlaybackState::Paused | PlaybackState::Loading)
        );
    if !show {
        if *connected && client.clear_activity().is_err() {
            *connected = false;
        }
        return;
    }
    let Some(n) = now else { return };

    if !*connected {
        if let Err(e) = client.connect() {
            debug!("discord not available: {e}");
            return;
        }
        *connected = true;
    }

    if client.set_activity(build_activity(n)).is_err() {
        // The socket likely went away (Discord closed); retry on the next tick.
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
