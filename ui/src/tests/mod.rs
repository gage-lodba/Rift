//! Frontend tests. Compiled only under `cfg(test)`, so `trunk build` and the
//! shipped wasm never carry any of it.
//!
//! Run with:
//!
//! ```text
//! cargo test -p rift-ui --target wasm32-unknown-unknown
//! ```
//!
//! See `harness.rs` for what is real, what is stubbed, and why the seam is
//! where it is.

mod api;
mod app;
pub(crate) mod harness;
mod reorder;
