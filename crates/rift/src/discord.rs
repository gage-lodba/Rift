//! Discord Rich Presence: advertises the current track as a "Listening to
//! Rift" status on the user's Discord profile.
//!
//! Mirrors the [`crate::media`] design. The [`rift_discord::Client`] blocks on a
//! local socket and is cheapest to own from a single place, so it lives entirely
//! on a dedicated thread; the player talks to that thread through the cheap,
//! `Send + Sync` [`DiscordHandle`] stored in `AppState`. The thread keeps the
//! last-known track/state and rebuilds the activity on every update, so progress
//! is carried by start/end timestamps rather than per-second ticks.
//!
//! A restarted track (repeat-one, replay) is a special case: its title, artist
//! and artwork are unchanged, so an activity that differs only in `timestamps`
//! may not be treated as new by Discord's client and the displayed timer keeps
//! counting. The thread marks such a restart for a forced refresh and clears the
//! activity before re-posting it, which makes Discord start a fresh activity
//! (and therefore a fresh timer).

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rift_discord::{Activity, Assets, Button, Client, Timestamps};
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

/// Gap between clearing and re-posting the presence when the same track starts
/// over.
///
/// Discord's client collapses a clear and a re-set that arrive in the same
/// processing tick: the presence is never actually observed as absent, the new
/// activity looks like a timestamp-only update of the old one, and the displayed
/// elapsed timer keeps counting. A short gap makes the clear a distinct presence
/// update, so the re-set creates a genuinely new activity (and a new timer).
/// The cost is that the presence is briefly absent — well under one Discord
/// render tick — only on a repeat/replay.
const PRESENCE_REFRESH_GAP: Duration = Duration::from_millis(500);

/// Ceiling on a single IPC exchange with Discord.
///
/// `rift_discord::Client`'s reads block with no timeout of their own, and the
/// client is not `Send` (it holds a `File` on Windows), so it cannot be moved
/// into `spawn_blocking` and an async timeout on its own thread cannot interrupt
/// it.
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
    /// Backend shutdown: leave the event loop and close the IPC socket.
    Shutdown,
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

    /// Ask the Discord thread to shut down cleanly (closing the IPC socket).
    /// Used by the backend's [`Drop`](crate::RiftBackend) implementation.
    pub(crate) fn shutdown(&self) {
        if let Some(tx) = &self.0 {
            let _ = tx.send(DiscordCmd::Shutdown);
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
    /// `true` when this snapshot is the *same* track starting over (repeat-one
    /// or a replay), not a new track.
    ///
    /// The presence payload would otherwise be identical to the previous play
    /// except for `timestamps`, which Discord's client does not reliably treat
    /// as a new activity — the displayed timer keeps counting. The next push
    /// clears the activity first so Discord starts a fresh one and re-anchors the
    /// timer at 0:00. Cleared once that push has been accepted.
    forced_refresh: bool,
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
///
/// Rejects non-finite lengths/positions and saturates the arithmetic: this runs
/// on the Discord thread, where a panic would kill Rich Presence for the rest of
/// the session, and the inputs come from decoder/metadata probes that a corrupt
/// stream can make absurd.
fn anchor(state: PlaybackState, duration: f64, position: f64) -> Option<(i64, i64)> {
    if !matches!(state, PlaybackState::Playing) || !duration.is_finite() || duration <= 0.0 {
        return None;
    }
    let position = if position.is_finite() {
        position.max(0.0)
    } else {
        0.0
    };
    let start = unix_millis().saturating_sub((position * 1000.0) as i64);
    Some((start, start.saturating_add((duration * 1000.0) as i64)))
}

/// Apply one command to the retained snapshot. Kept separate so a burst can be
/// drained and folded in before a single push.
///
/// Returns `false` for [`DiscordCmd::Shutdown`]: the caller leaves the loop and
/// closes the IPC socket. Every other command returns `true`.
fn fold(cmd: DiscordCmd, now: &mut Option<Now>, enabled: &mut bool) -> bool {
    match cmd {
        DiscordCmd::Track {
            title,
            artist,
            cover,
            duration,
        } => {
            // Starting the same track again is a replay, not a new track: the
            // activity would be identical except for its timestamps. Mark it so
            // the next Playing push can clear the presence first and make Discord
            // re-create the activity (which is what resets its displayed timer).
            //
            // The Loading state is preserved across the second `Track` command of
            // the same start (the playback code announces the track once before
            // the fetch and again with the decoded duration); only a command that
            // arrives while a *playable* track is shown can be a replay, and a
            // different identity abandons an in-progress restart.
            let same_identity = now.as_ref().is_some_and(|previous| {
                previous.title == title && previous.artist == artist && previous.cover == cover
            });
            let forced_refresh = match now.as_ref() {
                Some(previous) if previous.state == PlaybackState::Loading => {
                    previous.forced_refresh && same_identity
                }
                Some(_) => same_identity,
                None => false,
            };
            *now = Some(Now {
                title,
                artist,
                cover,
                duration,
                // A fresh track starts loading from the top (no bar yet); the
                // following State update flips it to Playing and anchors the bar.
                state: PlaybackState::Loading,
                anchor: None,
                forced_refresh,
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
        DiscordCmd::Shutdown => return false,
    }
    true
}

/// Whether a pending change is due to be pushed, given how long ago the last
/// push went out (`None` = nothing pushed yet) and whether anything changed.
///
/// Pure, so the pacing rule — never inside Discord's refresh window, a change is
/// due immediately before the first push, and an idle re-assert on the slower
/// heartbeat — is testable without driving the event loop. Taking the elapsed
/// time (rather than an `Instant`) also means this rule does no `Instant`
/// subtraction of its own, so it cannot underflow on a machine that booted less
/// than [`HEARTBEAT`] ago.
fn push_due(elapsed_since_last_push: Option<Duration>, dirty: bool) -> bool {
    match elapsed_since_last_push {
        // Nothing has been sent yet: a change goes out immediately.
        None => dirty,
        Some(elapsed) => (dirty && elapsed >= MIN_INTERVAL) || elapsed >= HEARTBEAT,
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
    // `None` means "nothing pushed yet": the first change goes out immediately
    // instead of being held for MIN_INTERVAL. It is not seeded with
    // `Instant::now() - HEARTBEAT`: an `Instant` is a count since boot, so that
    // subtraction underflows (and panics) on a machine that started Rift less
    // than HEARTBEAT after booting.
    let mut last_send: Option<Instant> = None;

    let mut ticker = tokio::time::interval(POLL);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            cmd = rx.recv() => {
                let Some(cmd) = cmd else { break }; // handle dropped: shut down
                if !fold(cmd, &mut now, &mut enabled) {
                    break; // backend shutdown: close the socket cleanly
                }
                // Fold any commands already queued behind it, so a burst (rapid
                // skips, track+state for one song) collapses into one push of
                // the final state instead of several.
                let mut shutdown = false;
                while let Ok(cmd) = rx.try_recv() {
                    shutdown |= !fold(cmd, &mut now, &mut enabled);
                }
                if shutdown {
                    break;
                }
                dirty = true;
            }
            _ = ticker.tick() => {}
        }

        // Send a pending change once MIN_INTERVAL has passed since the last push
        // (so it never lands inside Discord's refresh window), or re-assert on
        // the slower heartbeat to retry a reconnect. A change arriving during
        // the window waits here and coalesces to the latest, so back-to-back
        // switches send only the final song, once the window clears. Before the
        // first push (`None`) a change is due immediately.
        let elapsed = last_send.map(|last| last.elapsed());
        if push_due(elapsed, dirty) {
            // Only advance the interval clock when a push actually went out. A
            // Loading snapshot is skipped (the previous track stays up), so if it
            // reset `last_send` the real Playing push moments later would be held
            // for a whole MIN_INTERVAL — the new song wouldn't show for ~15s.
            if apply(&mut ipc, &mut connected, enabled, now.as_mut()).await {
                last_send = Some(Instant::now());
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
    now: Option<&mut Now>,
) -> bool {
    let state = now.as_deref().map(|n| n.state);
    // While a new track is still loading, leave the current presence untouched
    // rather than push a bar-less entry (the bar is withheld until Playing so it
    // can't run ahead during the buffer). The Playing push that follows replaces
    // it with the real time bar, so a switch reads old-track-with-bar ->
    // new-track-with-bar instead of flashing a barless name in between.
    if enabled && matches!(state, Some(PlaybackState::Loading)) {
        return false;
    }
    let show = enabled && matches!(state, Some(PlaybackState::Playing | PlaybackState::Paused));
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

    // A restarted track produces an activity that differs only in its
    // timestamps, which Discord's client does not reliably treat as a new
    // activity: the displayed elapsed timer keeps counting from the previous
    // play. Clear first so the following push creates a genuinely new activity
    // and Discord re-anchors the timer at 0:00. The flag is only cleared once the
    // push has been accepted, so a failed attempt retries the whole refresh.
    if n.forced_refresh {
        debug!("same track restarted; clearing the Discord presence before re-posting");
        match dispatch(ipc, IpcRequest::Clear).await {
            Ok(_) => {}
            Err(e) => {
                warn!("discord refresh clear failed ({e}); retrying on a later update");
                *ipc = None;
                *connected = false;
                return true;
            }
        }
        // Let Discord process the clear as its own presence state before the new
        // activity arrives; see [`PRESENCE_REFRESH_GAP`].
        tokio::time::sleep(PRESENCE_REFRESH_GAP).await;
    }

    match dispatch(ipc, IpcRequest::Push(Box::new(build_activity(n)))).await {
        Ok(Some(reply)) => {
            if reply.get("evt").and_then(|e| e.as_str()) == Some("ERROR") {
                warn!("discord rejected activity: {reply}");
            } else {
                n.forced_refresh = false;
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
    /// Post `activity`, then read Discord's reply frame. The reply is that
    /// frame, or `None` when Discord sent nothing.
    ///
    /// The activity is owned rather than borrowed so it can cross to the
    /// worker's `'static` thread; boxed because the other variant is tiny.
    Push(Box<Activity>),
    /// Clear the presence, then read and discard the reply frame.
    Clear,
}

/// Owns the IPC socket on its own thread and answers requests.
///
/// The IPC client is not `Send` on every platform (it holds a `File` on Windows)
/// and its reads block with no timeout of their own, so it cannot live in
/// `spawn_blocking` and an async timeout cannot interrupt it from the loop's
/// thread. Isolating it here is what keeps the event loop responsive: a wedged
/// socket stalls only this worker, not command coalescing, the heartbeat, or the
/// command channel.
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
    ///
    /// A failed thread spawn is logged rather than panicked: the worker's request
    /// channel is then already disconnected, so the next push takes the ordinary
    /// "IPC unavailable" path (warn + retry later) — the same degradation as
    /// Discord not running.
    fn spawn() -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        if let Err(e) = std::thread::Builder::new()
            .name("rift-discord-ipc".into())
            .spawn(move || serve(rx))
        {
            warn!("could not spawn the discord ipc thread; presence unavailable: {e}");
        }
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
    let mut client = Client::new(APP_ID);
    let mut connected = false;
    while let Some((req, reply)) = rx.blocking_recv() {
        // A dropped receiver means `call` already timed out and abandoned us.
        let _ = reply.send(run_request(&mut client, &mut connected, req));
    }
    if connected {
        client.close();
    }
}

/// Execute one request against the socket. `None` means the socket is not usable
/// (Discord isn't running, or the pipe broke), which the loop treats as "retry
/// later". Replies are dropped rather than propagated: the caller never acts on
/// a connect or write error differently from any other push failure.
fn run_request(
    client: &mut Client,
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

    // Both requests send the `SET_ACTIVITY` command and read Discord's reply
    // inside the client. The reply is read (rather than left in the pipe) so a
    // backed-up buffer cannot stall Discord's processing of our updates; an
    // `evt: "ERROR"` reply is returned as-is for the caller to log, while an I/O
    // error means the socket should be considered dead.
    let reply = match &req {
        IpcRequest::Push(activity) => client.set_activity(activity),
        IpcRequest::Clear => client.clear_activity(),
    };
    match reply {
        Ok(value) => Some(value),
        Err(e) => {
            warn!("discord request failed, dropping the connection: {e}");
            *connected = false;
            client.close();
            None
        }
    }
}

/// Build the Rich Presence activity for the current snapshot.
///
/// Field choices mirror the old third-party model exactly: the cover is the
/// large image, Rift's uploaded logo is the small image, and a `(start, end)`
/// pair drives Discord's elapsed/remaining bar.
fn build_activity(n: &Now) -> Activity {
    let mut assets = Assets::new();
    let mut has_assets = false;
    if !n.cover.is_empty() {
        assets = assets.large_image(n.cover.as_str());
        has_assets = true;
    }
    // `const_is_empty` is right that this is always true as the constant stands,
    // and the check stays anyway: `LOGO_ASSET` is the documented hook for a fork
    // with no uploaded badge (see its declaration), so clearing it must drop the
    // small image rather than request an asset that does not exist. Allowing the
    // lint here is more honest than deleting the branch that makes that work.
    #[allow(clippy::const_is_empty)]
    if !LOGO_ASSET.is_empty() {
        assets = assets.small_image(LOGO_ASSET).small_text("Rift");
        has_assets = true;
    }

    let mut activity = Activity::listening()
        .details(n.title.as_str())
        .state(n.artist.as_str())
        .buttons(vec![Button::new(GITHUB_LABEL, GITHUB_URL)]);
    if has_assets {
        activity = activity.assets(assets);
    }

    // A start+end pair renders Discord's elapsed/remaining time bar. The
    // endpoints are anchored once per state change (see [`Now::anchor`]) so
    // heartbeat re-asserts keep the exact same bar.
    if let Some((start, end)) = n.anchor {
        activity = activity.timestamps(Timestamps::start_end(start, end));
    }
    activity
}

fn unix_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::{
        anchor, build_activity, fold, push_due, unix_millis, DiscordCmd, Now, HEARTBEAT,
        MIN_INTERVAL,
    };
    use rift_types::PlaybackState;

    /// A `Track` command with just the identity fields the presence cares about.
    fn track_command(title: &str, artist: &str, cover: &str) -> DiscordCmd {
        DiscordCmd::Track {
            title: title.to_owned(),
            artist: artist.to_owned(),
            cover: cover.to_owned(),
            duration: 200.0,
        }
    }

    /// The state command the player sends once a track is actually decoding.
    fn playing() -> DiscordCmd {
        DiscordCmd::State {
            state: PlaybackState::Playing,
            position: 0.0,
        }
    }

    /// The Discord pacing rule: a change after a quiet spell goes out at once, a
    /// change inside Discord's refresh window is held, and an idle period
    /// re-asserts on the heartbeat. `None` (nothing pushed yet) sends the first
    /// change immediately instead of subtracting a heartbeat from
    /// `Instant::now()`, which would panic on a machine that booted less than a
    /// minute ago.
    #[test]
    fn changes_are_paced_outside_the_discord_refresh_window() {
        use std::time::Duration;

        assert!(push_due(None, true), "the first change is due immediately");
        assert!(!push_due(None, false), "nothing to push without a change");
        assert!(!push_due(Some(Duration::ZERO), true), "inside the window");
        assert!(
            !push_due(Some(MIN_INTERVAL - Duration::from_secs(1)), true),
            "still inside the window"
        );
        assert!(push_due(Some(MIN_INTERVAL), true), "the window has passed");
        assert!(!push_due(Some(MIN_INTERVAL), false), "no change, no push");
        assert!(
            push_due(Some(HEARTBEAT), false),
            "an idle heartbeat re-asserts the presence"
        );
    }

    /// The elapsed/remaining bar: it is shown only while playing a known length
    /// and anchored from the current position. A corrupt probe (non-finite
    /// length) must hide the bar rather than panic the Discord thread.
    #[test]
    fn the_time_bar_is_anchored_only_for_a_playing_known_length() {
        let before = unix_millis();
        let (start, end) = anchor(PlaybackState::Playing, 200.0, 50.0).expect("a bar is shown");
        assert_eq!(end - start, 200_000, "the bar spans the track length");
        let anchored_at = before - 50_000;
        assert!(
            (start - anchored_at).abs() < 5_000,
            "start {start} must be anchored 50s in the past (expected ~{anchored_at})"
        );

        for state in [
            PlaybackState::Paused,
            PlaybackState::Loading,
            PlaybackState::Stopped,
        ] {
            assert_eq!(anchor(state, 200.0, 50.0), None, "no bar while {state:?}");
        }
        for bad in [0.0, -1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(
                anchor(PlaybackState::Playing, bad, 0.0),
                None,
                "no bar for length {bad}"
            );
        }
    }

    /// An absurd position/length (a broken container can report them) must
    /// saturate instead of overflowing i64 — the arithmetic runs on the Discord
    /// thread, where a panic would end Rich Presence for the session.
    #[test]
    fn an_absurd_position_or_length_cannot_overflow_the_bar() {
        // A huge length saturates the end timestamp rather than panicking.
        assert!(anchor(PlaybackState::Playing, f64::MAX, 0.0).is_some());
        // A huge position saturates the start timestamp rather than panicking.
        assert!(anchor(PlaybackState::Playing, 100.0, f64::MAX).is_some());
        // A non-finite position is sanitized to "at the start".
        let before = unix_millis();
        let (start, end) = anchor(PlaybackState::Playing, 100.0, f64::NAN).expect("a bar is shown");
        assert!((start - before).abs() < 5_000);
        assert_eq!(end - start, 100_000);
    }

    /// A replay has the same title/artist/artwork as the play that just ended,
    /// so it must be flagged for a forced refresh: an activity that differs only
    /// in timestamps leaves Discord's displayed timer counting from the previous
    /// play instead of restarting at 0:00.
    #[test]
    fn a_restarted_track_forces_a_fresh_presence() {
        let mut now = None;
        let mut enabled = true;

        fold(
            track_command("Song", "Artist", "cover"),
            &mut now,
            &mut enabled,
        );
        fold(playing(), &mut now, &mut enabled);
        assert!(
            !now.as_ref().expect("a track").forced_refresh,
            "the first play is not a restart"
        );

        // The player announces a track twice: once before fetching and once
        // with the decoded duration. Both belong to the same (restarting) play.
        fold(
            track_command("Song", "Artist", "cover"),
            &mut now,
            &mut enabled,
        );
        assert!(
            now.as_ref().expect("a track").forced_refresh,
            "the restart is detected on the first announcement"
        );
        fold(
            DiscordCmd::State {
                state: PlaybackState::Loading,
                position: 0.0,
            },
            &mut now,
            &mut enabled,
        );
        fold(
            track_command("Song", "Artist", "cover"),
            &mut now,
            &mut enabled,
        );
        assert!(
            now.as_ref().expect("a track").forced_refresh,
            "the second announcement of the same play keeps the refresh flag"
        );
        fold(playing(), &mut now, &mut enabled);
        assert!(
            now.as_ref().expect("a track").forced_refresh,
            "the flag survives until the Playing push has been accepted"
        );
    }

    /// A different track is a genuinely different activity (details/state/art
    /// change), so it needs no forced refresh.
    #[test]
    fn a_different_track_does_not_force_a_presence_refresh() {
        let mut now = None;
        let mut enabled = true;

        fold(
            track_command("Song", "Artist", "cover"),
            &mut now,
            &mut enabled,
        );
        fold(playing(), &mut now, &mut enabled);
        fold(
            track_command("Song", "Artist", "cover"),
            &mut now,
            &mut enabled,
        );
        fold(
            DiscordCmd::State {
                state: PlaybackState::Loading,
                position: 0.0,
            },
            &mut now,
            &mut enabled,
        );
        fold(
            track_command("Another Song", "Artist", "cover"),
            &mut now,
            &mut enabled,
        );
        assert!(
            !now.as_ref().expect("a track").forced_refresh,
            "a changed identity is a new activity, not a restart"
        );
    }

    /// The activity maps the snapshot fields exactly as before: title to
    /// `details`, artist to `state`, cover to the large image, and the anchored
    /// pair to the time-bar timestamps.
    #[test]
    fn the_presence_payload_matches_the_track_and_anchor() {
        let now = Now {
            title: "Song".into(),
            artist: "Artist".into(),
            cover: "https://example.test/cover.jpg".into(),
            duration: 200.0,
            state: PlaybackState::Playing,
            anchor: Some((1_700_000_000_000, 1_700_000_200_000)),
            forced_refresh: false,
        };
        let value = serde_json::to_value(build_activity(&now)).expect("activity serializes");
        assert_eq!(value["type"], 2, "Listening is activity type 2");
        assert_eq!(value["details"], "Song");
        assert_eq!(value["state"], "Artist");
        assert_eq!(
            value["assets"]["large_image"],
            "https://example.test/cover.jpg"
        );
        assert_eq!(
            value["timestamps"],
            serde_json::json!({ "start": 1_700_000_000_000_i64, "end": 1_700_000_200_000_i64 })
        );

        let paused = Now {
            state: PlaybackState::Paused,
            anchor: None,
            ..now
        };
        let paused = serde_json::to_value(build_activity(&paused)).expect("serializes");
        assert!(
            paused.get("timestamps").is_none(),
            "no bar without an anchor: {paused}"
        );
    }
}
