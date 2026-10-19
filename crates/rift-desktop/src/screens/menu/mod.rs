//! Reusable immediate-mode context menu (kebab / right-click menus). This module
//! directory replaces the former 940-line `menu.rs`; `mod.rs` keeps the docs,
//! the kebab helper and the re-exports, so `screens::menu::*` paths (including
//! the test `harness`) are unchanged.
//!
//! The framework already owns the hard parts of popup layering
//! (`crates/rift-desktop/src/ui/popup.rs`): `Ui::popup` positions the card next
//! to its anchor, paints it in the deferred buffer (so it is always on top),
//! registers input blockers for the frame after it opens, closes on Escape /
//! click-outside, and draws the raised background, 1 px border, 8 px radius,
//! shadow and 4 px padding from `ui-spec.md` §6.8. This module wraps that with
//! the screens' vocabulary: entries carry their [`Action`] directly, so the menu
//! can dispatch itself and the caller never has to remember which menu is open.
//!
//! ## Usage
//!
//! From a row/card/header kebab (or a right-click):
//!
//! ```ignore
//! let kebab = menu::kebab(ui, kebab_rect, Id::from_str("row/kebab").child(i));
//! if kebab.clicked || row_resp.secondary_clicked {
//!     menu::open(ui, Id::from_str("row-menu").child(i), kebab.rect, entries);
//! }
//! ```
//!
//! Once per frame, the **root** draws the open menu outside any scroll clip:
//!
//! ```ignore
//! menu::layer(ui, actions);   // after main content, before overlays
//! ```
//!
//! Lists whose rows are interactive must skip their row responses while a menu
//! is open, because the menu's backdrop swallows that frame's click:
//!
//! ```ignore
//! let blocked = menu::is_open();
//! ```

use crate::screens::cards;
use crate::ui::{Icon, Id, Rect, Response, Ui};

mod entries;
#[cfg(test)]
pub(crate) mod harness;
mod state;
#[cfg(test)]
mod tests;

pub use entries::{
    Entry, Item, MENU_ITEM_HEIGHT, MENU_MIN_CONTENT_WIDTH, MENU_SEPARATOR_HEIGHT, SUBMENU_INDENT,
    SUBMENU_MAX_VISIBLE,
};
#[cfg(test)]
pub(crate) use state::lock_for_tests;
pub use state::{close, is_open, layer, open, open_id, pin_backdrop_cursor};

/// Draw a `.ibtn` kebab (`more_vert`, 15px) and return its interaction. The
/// caller opens a menu when it is clicked (or on the row's right-click).
pub fn kebab(ui: &mut Ui, rect: Rect, id: Id) -> Response {
    let theme = ui.theme().clone();
    cards::icon_button(ui, rect, id, Icon::MoreVert, 15.0, theme.text_dim)
}
