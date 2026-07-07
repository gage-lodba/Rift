//! Tauri commands invoked from the Yew frontend.
//!
//! All commands use `rename_all = "snake_case"` so argument names match the
//! snake_case keys the Rust/WASM frontend serializes. Each area lives in its
//! own submodule; the command functions are re-exported here so
//! `generate_handler![commands::foo]` in `main` keeps resolving unchanged.

mod bootstrap;
mod convert;
mod downloads;
mod library;
mod playback;
mod queue;
mod search;
mod updates;
mod window;
mod ytdlp;

pub use bootstrap::*;
pub use downloads::*;
pub use library::*;
pub use playback::*;
pub use queue::*;
pub use search::*;
pub use updates::*;
pub use window::*;
pub use ytdlp::*;
