//! Presentational components for the Rift UI.
//!
//! Each screen area lives in its own submodule; shared primitives sit in
//! [`icons`] (SVGs + formatting), [`menu`] (popup menu), and [`reorder`]
//! (drag-to-reorder state), plus [`key_on_nested_control`] for the rows that
//! activate on a key themselves. The names the app shell reaches for are
//! re-exported here so `use crate::components::*` picks them all up.

use wasm_bindgen::JsCast;
use web_sys::{Element, KeyboardEvent};

mod grids;
mod home;
mod icons;
mod menu;
mod player_bar;
mod queue;
// `pub(crate)` so the test tree can reach `Reorder`'s pure index math; the
// module is not re-exported below, so nothing outside the crate sees it.
pub(crate) mod reorder;
mod settings;
mod sidebar;
mod titlebar;
mod track_list;
mod update_banner;

pub use grids::{album_grid, artist_grid, results_view};
pub use home::HomeView;
pub use icons::{cover, icon};
pub use menu::{MenuAction, MenuButton};
pub use player_bar::PlayerBar;
pub use queue::QueuePanel;
pub use settings::SettingsView;
pub use sidebar::Sidebar;
pub use titlebar::Titlebar;
pub use track_list::{TrackList, TrackListCtx};
pub use update_banner::UpdateBanner;

// The props types the component tests construct. `cfg(test)` because the
// modules above are private, so these are unreachable from outside the crate
// and would be dead re-exports in a real build.
#[cfg(test)]
pub(crate) use player_bar::{track_key, PlayerBarProps};
#[cfg(test)]
pub(crate) use queue::QueuePanelProps;

/// The main view routed to by the sidebar and search.
#[derive(Clone, PartialEq)]
pub enum View {
    Home,
    Search,
    Liked,
    Settings,
    Playlist(String),
    Artist(String),
    /// All songs by an artist; the inner value is the artist ID.
    ArtistSongs(String),
    Album(String),
}

/// Whether a `keydown` was aimed at one of a row's own nested controls rather
/// than at the row itself.
///
/// The rows that act on a key (`TrackRow`, a queue row, a sidebar playlist row,
/// the queue header) are `role="button"` containers *because* they nest real
/// buttons — see the comment on `TrackRow` — and that nesting is exactly why
/// this check has to exist. A keystroke on the nested control bubbles to the
/// row's `onkeydown`, which activates the row *and* calls `prevent_default`;
/// that cancels the control's own activation, because a button's Enter/Space
/// click is the default action of the keydown. Measured consequence: `Enter` on
/// a row's kebab played the track instead of opening the menu, and `Enter` on
/// the queue's "Clear queue" collapsed the panel instead of clearing it. Those
/// controls were reachable by Tab and dead once focus was on them.
///
/// The tag is what decides, not `current_target`: Yew delegates events to the
/// subtree root, so `current_target` is the renderer's host element rather than
/// the element the handler belongs to. This mirrors the tag list `App`'s global
/// shortcut handler uses to detect that it is running inside a focused control.
pub(crate) fn key_on_nested_control(e: &KeyboardEvent) -> bool {
    e.target()
        .and_then(|t| t.dyn_into::<Element>().ok())
        .is_some_and(|el| {
            matches!(
                el.tag_name().as_str(),
                "BUTTON" | "INPUT" | "SELECT" | "TEXTAREA" | "A"
            )
        })
}
