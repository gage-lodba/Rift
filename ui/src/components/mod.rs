//! Presentational components for the Rift UI.
//!
//! Each screen area lives in its own submodule; shared primitives sit in
//! [`icons`] (SVGs + formatting), [`menu`] (popup menu), and [`reorder`]
//! (drag-to-reorder state). The names the app shell reaches for are re-exported
//! here so `use crate::components::*` picks them all up.

mod home;
mod icons;
mod menu;
mod player_bar;
mod queue;
mod reorder;
mod settings;
mod sidebar;
mod titlebar;
mod track_list;

pub use home::HomeView;
pub use icons::{cover, icon};
pub use menu::{MenuAction, MenuButton};
pub use player_bar::PlayerBar;
pub use queue::QueuePanel;
pub use settings::SettingsView;
pub use sidebar::Sidebar;
pub use titlebar::Titlebar;
pub use track_list::TrackList;

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
