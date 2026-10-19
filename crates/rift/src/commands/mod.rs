//! The backend's command surface: the former legacy IPC commands as plain methods.
//!
//! Each former `#[command]` is now an inherent method on
//! [`crate::BackendHandle`] (reachable directly on [`crate::RiftBackend`] via
//! `Deref`). Parameters and return types are unchanged, so the mapping from the
//! old frontend's `invoke("name", args)` calls is mechanical:
//!
//! | Old call | New call |
//! |---|---|
//! | `invoke("search", json!({ "query": q }))` | `backend.search(q).await` |
//! | `invoke("set_volume", json!({ "volume": v }))` | `backend.set_volume(v)` |
//!
//! Each area lives in its own submodule; the methods attach to the same type,
//! so no re-exports are needed. See `docs/backend-api.md` for
//! the full signature table.

mod bootstrap;
mod connectivity;
mod convert;
mod downloads;
pub(crate) mod images;
mod library;
mod playback;
mod queue;
mod search;
mod updates;
mod ytdlp;
