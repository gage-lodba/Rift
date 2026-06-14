//! Discord Rich Presence: advertises the current track as a "Listening to
//! Rift" status on the user's Discord profile.
//!
//! Mirrors the [`crate::media`] design. The `DiscordIpcClient` blocks on a
//! local socket and is cheapest to own from a single place, so it lives
//! entirely on a dedicated thread; the player talks to that thread through the
//! cheap, `Send + Sync` [`DiscordHandle`] stored in `AppState`. The thread
//! keeps the last-known track/state and rebuilds the activity on every update,
//! so progress is carried by start/end timestamps rather than per-second ticks.

use std::time::{SystemTime, UNIX_EPOCH};

use discord_rich_presence::activity::{Activity, ActivityType, Assets, Timestamps};
use discord_rich_presence::{DiscordIpc, DiscordIpcClient};
use rift_types::{PlaybackState, Track};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tracing::{debug, warn};

/// Rift's Discord application ID (Developer Portal → General Information). It
/// sets the app name shown as "Listening to Rift" and hosts uploaded art.
const APP_ID: &str = "1504731045847634042";

/// Asset key of a logo uploaded under the app's Rich Presence → Art Assets,
/// shown as a small badge over the cover. Leave empty to show only the cover.
const LOGO_ASSET: &str = "rift_logo";

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
    position: f64,
}

fn run(mut rx: UnboundedReceiver<DiscordCmd>, mut enabled: bool) {
    let mut client = DiscordIpcClient::new(APP_ID);
    // Whether the IPC socket is currently connected. Discord may be closed when
    // Rift starts (or restarted later), so we connect lazily and reconnect on
    // the next update if a send fails.
    let mut connected = false;
    let mut now: Option<Now> = None;

    while let Some(cmd) = rx.blocking_recv() {
        match cmd {
            DiscordCmd::Track {
                title,
                artist,
                cover,
                duration,
            } => {
                now = Some(Now {
                    title,
                    artist,
                    cover,
                    duration,
                    // A fresh track starts loading from the top; the following
                    // State update flips it to Playing.
                    state: PlaybackState::Loading,
                    position: 0.0,
                });
            }
            DiscordCmd::State { state, position } => {
                if let Some(n) = &mut now {
                    n.state = state;
                    n.position = position;
                }
            }
            DiscordCmd::Enabled(on) => enabled = on,
        }
        apply(&mut client, &mut connected, enabled, now.as_ref());
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
        .assets(assets);

    // A start+end pair renders Discord's elapsed/remaining time bar — only
    // meaningful while actually playing and when the duration is known.
    if matches!(n.state, PlaybackState::Playing) && n.duration > 0.0 {
        let now_ms = unix_millis();
        let start = now_ms - (n.position * 1000.0) as i64;
        let end = start + (n.duration * 1000.0) as i64;
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
