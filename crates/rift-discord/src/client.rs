//! Discord IPC transport: connect to the local socket, handshake, and send
//! `SET_ACTIVITY` commands.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

use crate::activity::Activity;
use crate::frame::{self, Opcode};
use crate::Error;

/// The platform transport to Discord's local RPC socket.
#[cfg(windows)]
type Transport = std::fs::File;
/// The platform transport to Discord's local RPC socket.
#[cfg(unix)]
type Transport = std::os::unix::net::UnixStream;

/// The number of sockets Discord may have bound, per the RPC reference's
/// `discord-ipc-{n}` naming: the client tries them in order.
const SOCKET_COUNT: u32 = 10;

/// Maximum reply frames a command will skip while waiting for its own answer.
///
/// Discord may interleave an unrelated dispatch with a command's reply. Skipping
/// a bounded number of frames keeps a peer that never sends the expected reply
/// from spinning forever; 8 is far more than a healthy client interleaves.
const MAX_SKIPPED_FRAMES: usize = 8;

/// A Discord Rich Presence IPC client.
///
/// One client owns one socket. It connects lazily, sends `SET_ACTIVITY`
/// commands, and can be dropped at any time; dropping closes the socket without
/// the close handshake, which Discord treats the same way as a crashed client.
///
/// [`Client::connect`] is blocking and has no timeout of its own, matching the
/// blocking local socket API; callers that cannot block (the backend's UI/event
/// loops) should own the client on a dedicated thread.
pub struct Client {
    client_id: String,
    transport: Option<Transport>,
}

impl Client {
    /// Create a client for the Discord application `client_id`.
    ///
    /// The id must be an application id from the Discord Developer Portal; it is
    /// sent in the handshake and Discord shows that application's name.
    pub fn new(client_id: impl Into<String>) -> Self {
        Self {
            client_id: client_id.into(),
            transport: None,
        }
    }

    /// Whether a socket is currently open. This does not probe liveness; a
    /// dropped Discord client is detected by the next failed read/write.
    pub fn is_connected(&self) -> bool {
        self.transport.is_some()
    }

    /// Connect to Discord's local RPC socket and perform the version 1
    /// handshake, waiting for the `READY` dispatch.
    ///
    /// Calling this while already connected is a no-op (use [`Client::close`]
    /// first to reconnect).
    ///
    /// # Errors
    ///
    /// Returns [`Error::IpcUnavailable`] when no Discord socket is found (Discord
    /// is not running, or uses an unexpected location), [`Error::Io`] or
    /// [`Error::Json`] for a broken/undecodable handshake exchange, and
    /// [`Error::Handshake`] when Discord rejects or does not acknowledge it.
    pub fn connect(&mut self) -> Result<(), Error> {
        if self.transport.is_some() {
            return Ok(());
        }

        let mut transport = open_transport()?;
        let handshake = json!({ "v": 1, "client_id": self.client_id });
        frame::write(&mut transport, Opcode::Handshake, &handshake)?;

        let reply = next_frame(&mut transport)?;
        match reply.get("evt").and_then(Value::as_str) {
            Some("READY") => {}
            Some("ERROR") => {
                let code = reply
                    .pointer("/data/code")
                    .map_or_else(|| "unknown".to_owned(), Value::to_string);
                let message = reply
                    .pointer("/data/message")
                    .and_then(Value::as_str)
                    .unwrap_or("no message");
                return Err(Error::Handshake(format!(
                    "Discord rejected the handshake (code {code}): {message}"
                )));
            }
            other => {
                return Err(Error::Handshake(format!(
                    "unexpected handshake dispatch {other:?}: {reply}"
                )));
            }
        }

        tracing::debug!(
            client_id = %self.client_id,
            "discord ipc handshake complete"
        );
        self.transport = Some(transport);
        Ok(())
    }

    /// Send a Rich Presence activity and return Discord's raw reply frame.
    ///
    /// The reply is returned as-is, including an `evt` of `"ERROR"` (Discord
    /// rate-limits activity updates and reports a rejection that way); this
    /// keeps protocol-level rejections distinguishable from a dead socket,
    /// which is an [`Error::Io`]. Callers decide whether to log or retry.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotConnected`] if [`Client::connect`] has not completed,
    /// [`Error::Io`] when the socket read/write fails (the caller should drop the
    /// client and reconnect), and [`Error::Protocol`]/[`Error::Json`] for a
    /// malformed reply.
    pub fn set_activity(&mut self, activity: &Activity) -> Result<Value, Error> {
        self.set_activity_argument(Some(activity))
    }

    /// Clear the Rich Presence (send `SET_ACTIVITY` with a `null` activity) and
    /// return Discord's raw reply frame.
    ///
    /// # Errors
    ///
    /// The same conditions as [`Client::set_activity`].
    pub fn clear_activity(&mut self) -> Result<Value, Error> {
        self.set_activity_argument(None)
    }

    /// Send the `SET_ACTIVITY` command for `activity`, where `None` clears the
    /// presence (the documented `null` activity).
    fn set_activity_argument(&mut self, activity: Option<&Activity>) -> Result<Value, Error> {
        let transport = self.transport.as_mut().ok_or(Error::NotConnected)?;
        let nonce = next_nonce();
        let request = set_activity_command(activity, &nonce);
        frame::write(transport, Opcode::Frame, &request)?;
        // DISCORD can interleave an unsolicited dispatch with the reply. A frame
        // carrying another request's nonce is not this command's answer: return
        // it and the real reply stays queued, shifting every later reply by one
        // frame, so skip it (bounded) instead.
        for _ in 0..MAX_SKIPPED_FRAMES {
            let reply = next_frame(transport)?;
            if reply_matches_nonce(&reply, &nonce) {
                return Ok(reply);
            }
            tracing::debug!("skipped a Discord IPC frame carrying another nonce");
        }
        Err(Error::Protocol(format!(
            "Discord did not answer SET_ACTIVITY with the expected nonce ({nonce})"
        )))
    }

    /// Close the socket, sending the documented `CLOSE` frame first when the
    /// write succeeds. Best effort: a closed Connection cannot report anything
    /// useful, and the local pipe is reclaimed when Discord exits.
    pub fn close(&mut self) {
        if let Some(mut transport) = self.transport.take() {
            let _ = frame::write(&mut transport, Opcode::Close, &json!({}));
        }
    }
}

/// Read the next `FRAME`, servicing the RPC control opcodes on the way.
///
/// Discord's local socket is bidirectional: besides replies it may send `PING`
/// keepalives, answer with `PONG`, and close with `CLOSE`, and a future protocol
/// revision may add opcodes. Treating "not the FRAME I expected" as corruption
/// would throw away a perfectly good connection, so this loop:
///
/// * answers `PING` with a `PONG` carrying the same payload (the RPC reference's
///   keepalive handshake),
/// * ignores `PONG` (nothing to acknowledge),
/// * turns `CLOSE` into an [`Error::Protocol`] naming Discord's close payload so
///   the caller reconnects instead of retrying on a dead socket,
/// * skips opcodes it does not know, with a debug log, rather than failing.
fn next_frame(transport: &mut (impl std::io::Read + std::io::Write)) -> Result<Value, Error> {
    loop {
        let (opcode, payload) = frame::read(transport)?;
        if opcode == Opcode::Frame as u32 {
            return Ok(payload);
        }
        if opcode == Opcode::Ping as u32 {
            frame::write(transport, Opcode::Pong, &payload)?;
            tracing::debug!("answered a Discord IPC PING with a PONG");
            continue;
        }
        if opcode == Opcode::Pong as u32 {
            tracing::trace!("ignored an unsolicited Discord IPC PONG");
            continue;
        }
        if opcode == Opcode::Close as u32 {
            let code = payload
                .pointer("/data/code")
                .or_else(|| payload.get("code"))
                .map_or_else(|| "unknown".to_owned(), Value::to_string);
            let message = payload
                .pointer("/data/message")
                .or_else(|| payload.get("message"))
                .and_then(Value::as_str)
                .unwrap_or("no message");
            return Err(Error::Protocol(format!(
                "Discord closed the IPC session (code {code}): {message}"
            )));
        }
        tracing::debug!(opcode, "skipped an unknown Discord IPC opcode");
    }
}

/// Whether a frame read while waiting for `nonce` is this command's reply.
///
/// A frame carrying a *different* nonce is an unrelated dispatch/reply and is
/// skipped; a frame without a nonce field is accepted (Discord always echoes it,
/// but a missing field is not worth failing a working connection over).
fn reply_matches_nonce(reply: &Value, nonce: &str) -> bool {
    reply
        .get("nonce")
        .and_then(Value::as_str)
        .is_none_or(|other| other == nonce)
}

/// Build the documented `SET_ACTIVITY` command payload.
///
/// Split from the socket write so the shape is unit-tested: the `pid` identifies
/// the process whose presence is being set, the `nonce` is required (Discord
/// answers `code 4000, "Payload requires a nonce"` without one), and a `None`
/// activity is how the presence is cleared.
fn set_activity_command(activity: Option<&Activity>, nonce: &str) -> Value {
    json!({
        "cmd": "SET_ACTIVITY",
        "args": { "pid": std::process::id(), "activity": activity },
        "nonce": nonce,
    })
}

/// Open the first available Discord IPC socket.
///
/// Windows uses the documented named-pipe path `\\?\pipe\discord-ipc-{n}`. Unix
/// uses `discord-ipc-{n}` under the first directory that has one, in the
/// documented search order (`XDG_RUNTIME_DIR`, `TMPDIR`, `TMP`, `TEMP`, then
/// `/tmp`).
fn open_transport() -> Result<Transport, Error> {
    let mut last_error = None;
    for path in socket_paths() {
        match open_socket(&path) {
            Ok(transport) => {
                tracing::debug!(path = %path.display(), "connected to the Discord IPC socket");
                return Ok(transport);
            }
            Err(error) => last_error = Some(error),
        }
    }

    Err(Error::IpcUnavailable(last_error.map_or_else(
        || "no candidate discord-ipc socket path exists".to_owned(),
        |error| error.to_string(),
    )))
}

/// The candidate socket paths, in the documented order.
fn socket_paths() -> Vec<PathBuf> {
    let mut paths = Vec::with_capacity(SOCKET_COUNT as usize * 6);

    #[cfg(windows)]
    {
        for index in 0..SOCKET_COUNT {
            paths.push(PathBuf::from(format!(r"\\?\pipe\discord-ipc-{index}")));
        }
    }

    #[cfg(unix)]
    {
        let mut dirs: Vec<PathBuf> = ["XDG_RUNTIME_DIR", "TMPDIR", "TMP", "TEMP"]
            .into_iter()
            .filter_map(std::env::var_os)
            .map(PathBuf::from)
            .collect();
        dirs.push(PathBuf::from("/tmp"));
        for dir in dirs {
            for index in 0..SOCKET_COUNT {
                paths.push(dir.join(format!("discord-ipc-{index}")));
            }
        }
    }

    paths
}

/// Open one candidate path as a readable and writable socket.
#[cfg(windows)]
fn open_socket(path: &std::path::Path) -> std::io::Result<Transport> {
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
}

/// Connect to one candidate Unix socket path.
#[cfg(unix)]
fn open_socket(path: &std::path::Path) -> std::io::Result<Transport> {
    std::os::unix::net::UnixStream::connect(path)
}

/// A unique UUID-shaped nonce, as the RPC reference's payloads use.
///
/// Discord only requires a string that is unique per command (it echoes it in the
/// reply, which Rift does not rely on), so this is a time/process/counter mix
/// rather than a dependency on a UUID crate. The version/variant nibbles are set
/// so the output is a well-formed v4 UUID as well.
fn next_nonce() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos() as u64);
    let count = COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = u64::from(std::process::id());

    let high = now.rotate_left(17) ^ pid.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    let low = count
        .wrapping_mul(0xD1B5_4A32_D192_ED03)
        .wrapping_add(now.rotate_left(31));

    let mut hex = format!("{high:016x}{low:016x}").into_bytes();
    hex[12] = b'4'; // version 4
    hex[16] = match hex[16] {
        b'8'..=b'b' => hex[16],
        _ => b'8',
    }; // RFC 4122 variant

    // `hex` is ASCII by construction, so slicing it is byte-safe.
    let hex = String::from_utf8(hex).expect("hex digits are valid UTF-8");
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::activity::Timestamps;

    #[test]
    fn a_nonce_looks_like_a_v4_uuid_and_is_unique() {
        let mut seen = std::collections::HashSet::new();
        for _ in 0..256 {
            let nonce = next_nonce();
            assert_eq!(nonce.len(), 36, "UUID string form: {nonce}");
            assert_eq!(
                nonce.split('-').map(str::len).collect::<Vec<_>>(),
                [8, 4, 4, 4, 12],
                "group lengths: {nonce}"
            );
            assert!(nonce.chars().all(|c| c.is_ascii_hexdigit() || c == '-'));
            assert_eq!(&nonce[14..15], "4", "version nibble: {nonce}");
            assert!(
                ["8", "9", "a", "b"].contains(&&nonce[19..20]),
                "variant nibble: {nonce}"
            );
            assert!(seen.insert(nonce), "nonces must not repeat");
        }
    }

    #[test]
    fn a_set_activity_command_carries_the_pid_nonce_and_activity() {
        let activity = Activity::listening().details("Song");
        let command = set_activity_command(Some(&activity), "test-nonce");
        assert_eq!(command["cmd"], "SET_ACTIVITY");
        assert_eq!(command["nonce"], "test-nonce");
        assert_eq!(command["args"]["pid"], u64::from(std::process::id()));
        assert_eq!(command["args"]["activity"]["details"], "Song");

        let cleared = set_activity_command(None, "clear-nonce");
        assert!(
            cleared["args"]["activity"].is_null(),
            "clearing sends a null activity: {cleared}"
        );
    }

    #[test]
    fn a_request_without_a_connection_is_not_connected() {
        // Building a client does not connect; this must be an explicit error
        // rather than a panic or a silent no-op.
        let mut client = Client::new("0");
        assert!(!client.is_connected());
        let error = client
            .set_activity(&Activity::listening())
            .expect_err("an unconnected client cannot send");
        assert!(
            matches!(error, Error::NotConnected),
            "unexpected error: {error}"
        );
        assert!(
            client.clear_activity().is_err(),
            "clearing without a connection must fail"
        );
    }

    #[test]
    fn a_reply_matches_only_its_own_nonce() {
        assert!(reply_matches_nonce(
            &json!({ "nonce": "a", "evt": null }),
            "a"
        ));
        assert!(!reply_matches_nonce(
            &json!({ "nonce": "b", "evt": null }),
            "a"
        ));
        assert!(
            reply_matches_nonce(&json!({ "evt": "ACTIVITY_JOIN" }), "a"),
            "a frame without a nonce field is accepted rather than dropped"
        );
    }

    #[test]
    fn errors_render_their_cause_for_logs() {
        let io = Error::from(std::io::Error::new(
            std::io::ErrorKind::BrokenPipe,
            "broken pipe",
        ));
        let rendered = format!("{io}");
        assert!(
            rendered.contains("I/O") && rendered.contains("broken pipe"),
            "unexpected rendering: {rendered}"
        );
        let json = Error::from(serde_json::from_str::<Value>("{").expect_err("invalid JSON"));
        assert!(
            format!("{json}").to_lowercase().contains("json"),
            "unexpected rendering: {json}"
        );
    }

    /// In-memory transport: frames to read, plus the bytes the client writes.
    struct FakeTransport {
        input: std::io::Cursor<Vec<u8>>,
        output: Vec<u8>,
    }

    impl FakeTransport {
        fn new(input: Vec<u8>) -> Self {
            Self {
                input: std::io::Cursor::new(input),
                output: Vec::new(),
            }
        }
    }

    impl std::io::Read for FakeTransport {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            self.input.read(buf)
        }
    }

    impl std::io::Write for FakeTransport {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.output.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// Encode one frame with an arbitrary raw opcode (including opcodes with no
    /// [`Opcode`] variant), for the control-opcode tests.
    fn encoded_frame(opcode: u32, payload: &Value) -> Vec<u8> {
        let body = serde_json::to_vec(payload).expect("serialize test payload");
        let mut wire = Vec::new();
        wire.extend_from_slice(&opcode.to_le_bytes());
        wire.extend_from_slice(&(body.len() as u32).to_le_bytes());
        wire.extend_from_slice(&body);
        wire
    }

    #[test]
    fn a_ping_is_answered_with_the_same_payload_before_the_reply() {
        let ping = json!({ "nonce": "keepalive" });
        let reply = json!({ "cmd": "SET_ACTIVITY", "evt": null, "data": null });
        let mut input = encoded_frame(Opcode::Ping as u32, &ping);
        input.extend(encoded_frame(Opcode::Frame as u32, &reply));
        let mut transport = FakeTransport::new(input);

        let value =
            next_frame(&mut transport).expect("the ping is answered and the reply returned");
        assert_eq!(value, reply);

        let mut expected = Vec::new();
        frame::write(&mut expected, Opcode::Pong, &ping).expect("encode the expected PONG");
        assert_eq!(
            transport.output, expected,
            "the PING must be echoed back as a PONG with the same payload"
        );
    }

    #[test]
    fn control_and_unknown_opcodes_are_skipped_until_a_frame_arrives() {
        let reply = json!({ "evt": "READY", "data": {} });
        let mut input = Vec::new();
        input.extend(encoded_frame(Opcode::Pong as u32, &json!({})));
        input.extend(encoded_frame(9, &json!({ "future": true })));
        input.extend(encoded_frame(Opcode::Frame as u32, &reply));
        let mut transport = FakeTransport::new(input);

        assert_eq!(next_frame(&mut transport).expect("frame"), reply);
        assert!(
            transport.output.is_empty(),
            "PONG and unknown opcodes must not write anything back"
        );
    }

    #[test]
    fn a_close_frame_is_a_protocol_error_with_discords_reason() {
        let close = json!({ "code": 4000, "message": "invalid client id" });
        let mut transport = FakeTransport::new(encoded_frame(Opcode::Close as u32, &close));
        let error = next_frame(&mut transport).expect_err("CLOSE must end the read");
        let rendered = format!("{error}");
        assert!(
            rendered.contains("closed the IPC session")
                && rendered.contains("4000")
                && rendered.contains("invalid client id"),
            "unexpected error: {rendered}"
        );
    }

    /// Manual smoke test against a running Discord client:
    ///
    /// ```text
    /// RIFT_DISCORD_APP_ID=<application id> cargo test -p rift-discord -- --ignored
    /// ```
    ///
    /// It sets, clears and re-sets a test activity (the sequence the backend uses
    /// when a track restarts), then clears it; the machine needs Discord running.
    #[test]
    #[ignore = "requires a running Discord client and RIFT_DISCORD_APP_ID"]
    fn live_handshake_set_and_clear_activity() {
        let Ok(app_id) = std::env::var("RIFT_DISCORD_APP_ID") else {
            panic!("set RIFT_DISCORD_APP_ID to the Discord application id to test against");
        };
        let mut client = Client::new(app_id);
        client.connect().expect("connect to the Discord IPC socket");
        assert!(client.is_connected());

        let activity = Activity::listening()
            .details("Rift Discord IPC self-test")
            .state("safe to ignore — cleared immediately")
            .timestamps(Timestamps::start_end(1_700_000_000_000, 1_700_000_200_000));
        let reply = client.set_activity(&activity).expect("set activity");
        assert!(
            reply.get("evt").and_then(Value::as_str) != Some("ERROR"),
            "Discord rejected the activity: {reply}"
        );

        // The replay path: clear then re-set with fresh timestamps so Discord
        // starts a new activity (and therefore a new timer).
        let cleared = client.clear_activity().expect("clear activity");
        assert!(
            cleared.get("evt").and_then(Value::as_str) != Some("ERROR"),
            "Discord rejected the clear: {cleared}"
        );
        let replayed = activity
            .clone()
            .timestamps(Timestamps::start_end(1_700_000_300_000, 1_700_000_500_000));
        let reply = client
            .set_activity(&replayed)
            .expect("re-set activity after a refresh");
        assert!(
            reply.get("evt").and_then(Value::as_str) != Some("ERROR"),
            "Discord rejected the refreshed activity: {reply}"
        );

        let cleared = client.clear_activity().expect("final clear activity");
        assert!(
            cleared.get("evt").and_then(Value::as_str) != Some("ERROR"),
            "Discord rejected the final clear: {cleared}"
        );
        client.close();
        assert!(!client.is_connected());
    }
}
