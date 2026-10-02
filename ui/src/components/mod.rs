//! Presentational components for the Rift UI.
//!
//! Each screen area lives in its own submodule; shared primitives sit in
//! [`icons`] (SVGs + formatting), [`menu`] (popup menu), and [`reorder`]
//! (drag-to-reorder state). The names the app shell reaches for are re-exported
//! here so `use crate::components::*` picks them all up.

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
pub(crate) use player_bar::PlayerBarProps;
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
