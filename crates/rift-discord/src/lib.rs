//! Rift's in-repo Discord Rich Presence client.
//!
//! Implements the small subset of the Discord RPC protocol Rift needs, as
//! documented in the official RPC reference (<https://docs.discord.com/developers/topics/rpc.md>):
//! connect to the local IPC socket, send the version 1 `HANDSHAKE`, read the
//! `READY` dispatch, then send `SET_ACTIVITY` commands (including a `null`
//! activity to clear the presence).
//!
//! Deliberately **out of scope**: OAuth (`AUTHORIZE`/`AUTHENTICATE`), voice,
//! guild/channel/message RPC, join/spectate secrets, and subscriptions. Rift only
//! needs to advertise the current track, so the crate keeps the protocol surface
//! and its dependency tree small.
//!
//! # Why in-repo
//!
//! Rift needs exact control over *when* an activity update is material. A
//! restarted track (repeat/replay) has the same title, artist and artwork as the
//! previous play, so an activity that differs only in its `timestamps` fields may
//! not be treated as a new activity by Discord's client. The backend therefore
//! clears and re-sets the activity when the same track starts over. Owning the
//! transport keeps that behaviour testable and removes a third-party crate from
//! the trusted surface.
//!
//! # Wire format
//!
//! Every frame is `opcode: u32 little-endian`, `length: u32 little-endian`, then
//! a UTF-8 JSON body. Opcodes: `0` handshake, `1` frame (commands, replies and
//! events), `2` close.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod activity;
mod client;
mod error;
mod frame;

pub use activity::{Activity, ActivityType, Assets, Button, Timestamps};
pub use client::Client;
pub use error::Error;
