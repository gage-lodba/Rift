//! The shared track list used by every view, plus the collection header
//! (metadata line + Play/Download actions) and the track context-menu builder.
//! This module directory replaces the former 1,113-line `track_list.rs`;
//! `mod.rs` only wires the submodules and re-exports the public surface, so
//! `screens::track_list::*` call sites are unchanged.
//!
//! Everything here is measured from `ui-spec.md`:
//!
//! * rows are [`cards::track_row`] (54 px: `padding: 7px 10px`, 40 px cover,
//!   title 13/500, artist+album 12 dim, duration 40 px right-aligned, heart and
//!   a hover-revealed kebab);
//! * the collection meta line is 13 px dim, line box 17;
//! * the head actions row has `margin: 18px 0 16px`, a pill Play button
//!   (`.play-all`: `padding: 8px 22px`, 14/700, icon 16, gap 7, radius 999) and a
//!   `.head-btn` Download button (`padding: 8px 18px`, 14/600, icon 16, gap 7).
//!
//! The context menu lives in [`crate::screens::menu`]; this module only builds
//! the track-row menu entries (and opens them from a row's kebab / right-click).

use rift_types::Track;

use crate::state::{DownloadStatus, State};

mod header;
mod list;
mod menu;
#[cfg(test)]
mod tests;

pub use header::{
    collection_meta, collection_meta_text, download_head_state, head_actions,
    is_current_collection, DownloadHeadState, HeadActions, COLLECTION_META_H, HEAD_ACTIONS_GAP,
    HEAD_ACTIONS_H, HEAD_ACTIONS_MARGIN_BOTTOM, HEAD_ACTIONS_MARGIN_TOP, HEAD_BTN_H,
    HEAD_BTN_PAD_X, PLAY_ALL_PAD_X,
};
pub use list::{list_height, track_list, TrackListOpts};
pub use menu::{open_track_menu, track_menu_entries};

// A row's download status is also available through `State::download_status`;
// keep the import used so the module documents the mapping.
const _: fn(&State, &Track) -> DownloadStatus = State::download_status;
