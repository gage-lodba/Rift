//! Frontend tests. Compiled only under `cfg(test)`, so `trunk build` and the
//! shipped wasm never carry any of it.
//!
//! Run with:
//!
//! ```text
//! cargo test -p rift-ui --target wasm32-unknown-unknown
//! ```
//!
//! Two layers, and it is worth knowing which one a given test sits in:
//!
//! - [`harness`] and everything beside it — the IPC boundary, the reorder math,
//!   `App`'s decisions — runs with the Tauri bridge replaced and **no DOM at
//!   all**. It covers logic and first renders.
//! - [`dom`] and [`render`] supply a real DOM (`jsdom`, on the Node side) and
//!   drive Yew's actual client renderer, which is the only way to see a
//!   **second** render. That is where `C1` and `H4` live.
//!
//! See `harness.rs` for what is real and what is stubbed at the first seam, and
//! `dom.rs` for the second.

mod api;
mod app;
mod components;
pub(crate) mod dom;
pub(crate) mod harness;
mod hook_order;
mod render;
mod reorder;
