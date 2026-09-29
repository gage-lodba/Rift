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

/// Ceiling on a single IPC exchange with Discord.
///
/// `DiscordIpcClient`'s reads block with no timeout of their own, and the client
/// is not `Send` (it holds a `File` on Windows), so it cannot be moved into
/// `spawn_blocking` and an async timeout on its own thread cannot interrupt it.
/// Every socket operation therefore runs on a dedicated worker thread that
/// applies this deadline itself; the event loop only ever waits on a channel.
/// Without that, one unanswered read would stall the loop outright — commands
/// would stop coalescing, the heartbeat would stop firing, and the unbounded
/// command channel would grow for the rest of the session. Comfortably under
/// [`POLL`] so a slow exchange cannot eat a whole tick.
const IPC_TIMEOUT: Duration = Duration::from_millis(500);

/// Rift's Discord application ID (Developer Portal → General Information). It
/// sets the app name shown as "Listening to Rift" and hosts uploaded art.
///
/// **Forks:** you need your own Discord application for Rich Presence to work.
/// Create one at <https://discord.com/developers/applications>, copy its
/// Application ID here, and upload a logo under Rich Presence → Art Assets
/// (keyed as [`LOGO_ASSET`]) if you want the badge.
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
    // Every socket operation goes through this worker; see [`IpcWorker`]. Held as
    // an `Option` so a stall can abandon the worker and start a fresh one.
    let mut ipc = None;
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
            if apply(&mut ipc, &mut connected, enabled, now.as_ref()).await {
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
async fn apply(
    ipc: &mut Option<IpcWorker>,
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
            let req = IpcRequest::Clear;
            match dispatch(ipc, req).await {
                Ok(_) => {}
                Err(e) => {
                    warn!("discord clear failed, reconnecting next tick: {e}");
                    *connected = false;
                }
            }
            return true;
        }
        return false;
    }
    let Some(n) = now else { return false };

    match dispatch(ipc, IpcRequest::Push(activity_payload(build_activity(n)))).await {
        Ok(Some(reply)) => {
            if reply.get("evt").and_then(|e| e.as_str()) == Some("ERROR") {
                warn!("discord rejected activity: {reply}");
            } else {
                debug!(
                    "discord push: \"{}\" state={:?} bar={}",
                    n.title,
                    n.state,
                    n.anchor.is_some()
                );
            }
            *connected = true;
        }
        // The worker ran the request but the socket was unusable (Discord not
        // running, or the pipe broke). Reconnect on the next paced attempt.
        Ok(None) => *connected = false,
        Err(e) => {
            warn!("discord ipc stalled ({e}); starting a fresh worker");
            // The abandoned worker is parked on a blocking read we cannot
            // interrupt; `dispatch` spawns a replacement on the next request.
            *ipc = None;
            *connected = false;
        }
    }
    true
}

/// Run one request on the worker, spawning a replacement first if the previous
/// one was abandoned.
async fn dispatch(
    ipc: &mut Option<IpcWorker>,
    req: IpcRequest,
) -> Result<Option<serde_json::Value>, String> {
    let worker = ipc.get_or_insert_with(IpcWorker::spawn);
    worker.call(req).await
}

/// One socket operation for the IPC worker. Carries its own reply channel so the
/// worker never needs a second queue.
enum IpcRequest {
    /// Connect if needed, push `activity`, then read Discord's reply frame.
    /// The reply is that frame, or `None` when Discord sent nothing.
    ///
    /// The activity travels as an already-serialized object because it borrows
    /// from [`Now`] and so cannot outlive this call, while the worker is a
    /// `'static` thread.
    Push(serde_json::Value),
    /// Clear the presence, then read and discard the reply frame.
    Clear,
}

/// Owns the IPC socket on its own thread and answers requests.
///
/// `DiscordIpcClient` is not `Send` (it holds a `File` on Windows) and its reads
/// block with no timeout of their own, so it cannot live in `spawn_blocking` and
/// an async timeout cannot interrupt it from the loop's thread. Isolating it here
/// is what keeps the event loop responsive: a wedged socket stalls only this
/// worker, not command coalescing, the heartbeat, or the command channel.
///
/// Each request is answered or times out within [`IPC_TIMEOUT`]. A worker that
/// misses that deadline is **abandoned** rather than awaited — its thread stays
/// parked on the blocking read (there is no way to interrupt it safely: closing
/// the handle underneath a read is the one thing that would be worse than a
/// leak), so `IpcWorker::call` drops it and the next call spawns a fresh worker
/// with a fresh socket. That bounds the leak to one parked thread per stall.
struct IpcWorker {
    tx: UnboundedSender<(
        IpcRequest,
        tokio::sync::oneshot::Sender<Option<serde_json::Value>>,
    )>,
}

impl IpcWorker {
    /// Spawn a worker with a fresh, unconnected client.
    fn spawn() -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        std::thread::Builder::new()
            .name("rift-discord-ipc".into())
            .spawn(move || serve(rx))
            .expect("failed to spawn discord ipc thread");
        Self { tx }
    }

    /// Run one request, bounded by [`IPC_TIMEOUT`].
    async fn call(&self, req: IpcRequest) -> Result<Option<serde_json::Value>, String> {
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        self.tx
            .send((req, reply_tx))
            .map_err(|_| "discord ipc worker is gone".to_owned())?;
        match tokio::time::timeout(IPC_TIMEOUT, reply_rx).await {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(_)) => Err("discord ipc worker dropped the request".to_owned()),
            Err(_) => Err(format!(
                "discord ipc timed out after {}ms",
                IPC_TIMEOUT.as_millis()
            )),
        }
    }
}

/// The worker's thread body: connect lazily, run requests until the loop drops
/// its sender (app shutdown), then close the socket.
fn serve(
    mut rx: UnboundedReceiver<(
        IpcRequest,
        tokio::sync::oneshot::Sender<Option<serde_json::Value>>,
    )>,
) {
    let mut client = DiscordIpcClient::new(APP_ID);
    let mut connected = false;
    while let Some((req, reply)) = rx.blocking_recv() {
        // A dropped receiver means `call` already timed out and abandoned us.
        let _ = reply.send(run_request(&mut client, &mut connected, req));
    }
    if connected {
        let _ = client.close();
    }
}

/// Execute one request against the socket. `None` means the socket is not usable
/// (Discord isn't running, or the pipe broke), which the loop treats as "retry
/// later". Replies are dropped rather than propagated: the caller never acts on
/// a connect or write error differently from any other push failure.
fn run_request(
    client: &mut DiscordIpcClient,
    connected: &mut bool,
    req: IpcRequest,
) -> Option<serde_json::Value> {
    if !*connected {
        if let Err(e) = client.connect() {
            debug!("discord not available: {e}");
            return None;
        }
        debug!("discord connected");
        *connected = true;
    }

    let send = match &req {
        IpcRequest::Push(activity) => client.send(set_activity_frame(activity.clone()), 1),
        IpcRequest::Clear => client.clear_activity(),
    };
    if let Err(e) = send {
        // The socket likely went away (Discord closed); the next request
        // reconnects from scratch.
        warn!("discord write failed, dropping the connection: {e}");
        *connected = false;
        return None;
    }

    // Read Discord's reply. The library's `set_activity`/`clear_activity` only
    // write and never read it, so left unread the pipe's buffer backs up until
    // Discord stalls applying our updates — writes keep returning Ok while the
    // presence freezes on an old track. Reading it also surfaces rate-limit
    // ERROR replies and detects a dead pipe (a read error drops the connection).
    match client.recv() {
        Ok((_, val)) => Some(val),
        Err(e) => {
            warn!("discord read failed, dropping the connection: {e}");
            *connected = false;
            None
        }
    }
}

/// Serialize an activity into an owned JSON object, so it can be handed to the
/// IPC thread. Equivalent to what the library's own `set_activity` serializes;
/// the only change is that the data no longer borrows from [`Now`].
fn activity_payload(activity: Activity<'_>) -> serde_json::Value {
    serde_json::to_value(activity).unwrap_or(serde_json::Value::Null)
}

/// Wrap a serialized activity in the `SET_ACTIVITY` command frame.
///
/// This mirrors the library's `set_activity` exactly, including the `pid` that
/// tells Discord which process owns the presence. It exists only because the
/// library's version takes a borrowing `Activity`, which cannot cross a thread
/// boundary. The `nonce` the library sends is omitted: Discord does not require
/// it, and `set_activity`'s own reply is read positionally rather than matched
/// against it.
fn set_activity_frame(activity: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "cmd": "SET_ACTIVITY",
        "args": { "pid": std::process::id(), "activity": activity },
    })
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
