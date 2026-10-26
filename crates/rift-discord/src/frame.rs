//! Discord RPC wire framing.
//!
//! The official RPC reference documents the IPC frame as an opcode, a length and
//! a JSON body; both numbers are 32-bit little-endian (`[00 00 00 00]` for the
//! handshake opcode, followed by `[2D 00 00 00]` for a 45-byte body in the
//! reference's example).

use std::io::{Read, Write};

use serde_json::Value;

use crate::Error;

/// The RPC opcodes Rift sends or answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Opcode {
    /// Starts the session; the body carries `{"v": 1, "client_id": "..."}`.
    Handshake = 0,
    /// Commands, replies and dispatched events.
    Frame = 1,
    /// Closes the session.
    Close = 2,
    /// Keepalive request; the peer expects the same payload back as a PONG.
    Ping = 3,
    /// Keepalive answer; no action needed beyond acknowledging it.
    Pong = 4,
}

/// Ceiling on a frame body read from Discord.
///
/// Replies and events are small JSON objects; this bound keeps a corrupt or
/// hostile local socket from making Rift allocate an arbitrary buffer from an
/// 8-byte header alone.
pub(crate) const MAX_FRAME_BYTES: u32 = 1024 * 1024;

/// Write one frame: opcode and body length (both `u32` little-endian) followed by
/// the JSON body, then flush.
pub(crate) fn write(writer: &mut impl Write, opcode: Opcode, payload: &Value) -> Result<(), Error> {
    let body = serde_json::to_vec(payload)?;
    let length = u32::try_from(body.len())
        .map_err(|_| Error::Protocol("outgoing JSON body is larger than u32".to_owned()))?;
    if length > MAX_FRAME_BYTES {
        return Err(Error::Protocol(format!(
            "outgoing frame body is {length} bytes, over the {MAX_FRAME_BYTES}-byte limit"
        )));
    }

    writer.write_all(&(opcode as u32).to_le_bytes())?;
    writer.write_all(&length.to_le_bytes())?;
    writer.write_all(&body)?;
    writer.flush()?;
    Ok(())
}

/// Read one frame, returning its raw opcode and parsed JSON body.
///
/// The opcode is returned as a raw `u32` rather than an [`Opcode`] because
/// Discord may send other documented opcodes (PING/PONG); callers validate the
/// ones they expect.
pub(crate) fn read(reader: &mut impl Read) -> Result<(u32, Value), Error> {
    let mut header = [0u8; 8];
    reader.read_exact(&mut header)?;

    let opcode = u32::from_le_bytes([header[0], header[1], header[2], header[3]]);
    let length = u32::from_le_bytes([header[4], header[5], header[6], header[7]]);
    if length > MAX_FRAME_BYTES {
        return Err(Error::Protocol(format!(
            "reply frame body is {length} bytes, over the {MAX_FRAME_BYTES}-byte limit"
        )));
    }

    let mut body = vec![0u8; length as usize];
    reader.read_exact(&mut body)?;
    let value = serde_json::from_slice(&body)?;
    Ok((opcode, value))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_frame_is_an_opcode_then_a_length_then_the_json_body() {
        let payload = json!({ "v": 1, "client_id": "123" });
        let body = serde_json::to_vec(&payload).expect("serialize");
        let mut wire = Vec::new();
        write(&mut wire, Opcode::Handshake, &payload).expect("write");

        assert_eq!(
            &wire[0..4],
            &[0, 0, 0, 0],
            "HANDSHAKE opcode, little-endian"
        );
        assert_eq!(
            &wire[4..8],
            &(body.len() as u32).to_le_bytes(),
            "body length, little-endian"
        );
        assert_eq!(&wire[8..], body.as_slice(), "UTF-8 JSON body");

        let (opcode, value) = read(&mut wire.as_slice()).expect("read");
        assert_eq!(opcode, Opcode::Handshake as u32);
        assert_eq!(value, payload);
    }

    #[test]
    fn a_reply_body_over_the_size_limit_is_rejected_before_allocating() {
        let mut header = Vec::new();
        header.extend_from_slice(&1u32.to_le_bytes());
        header.extend_from_slice(&(MAX_FRAME_BYTES + 1).to_le_bytes());
        let error = read(&mut header.as_slice()).expect_err("oversized frame must be rejected");
        assert!(
            format!("{error}").contains("over the"),
            "the error must name the limit: {error}"
        );
    }

    #[test]
    fn a_truncated_frame_is_an_io_error_not_a_partial_parse() {
        let payload = json!({ "opcode": true });
        let mut wire = Vec::new();
        write(&mut wire, Opcode::Frame, &payload).expect("write");
        wire.truncate(wire.len() - 1);
        let error = read(&mut wire.as_slice()).expect_err("truncated frame must fail");
        assert!(
            format!("{error}").contains("I/O") || format!("{error}").contains("io"),
            "the error must mention I/O: {error}"
        );
    }

    #[test]
    fn an_invalid_json_body_is_a_parse_error() {
        let mut wire = Vec::new();
        wire.extend_from_slice(&1u32.to_le_bytes());
        wire.extend_from_slice(&4u32.to_le_bytes());
        wire.extend_from_slice(b"nope");
        let error = read(&mut wire.as_slice()).expect_err("invalid JSON must fail");
        assert!(
            format!("{error}").to_lowercase().contains("json"),
            "the error must mention JSON: {error}"
        );
    }
}
