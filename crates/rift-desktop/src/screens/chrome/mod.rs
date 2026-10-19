//! App chrome: titlebar, sidebar, player bar, queue panel, toast and update
//! banner. This module directory replaces the former 1,961-line `chrome.rs`;
//! `mod.rs` only wires the submodules and re-exports their public entry points,
//! so `screens::chrome::*` call sites (root.rs) are unchanged.
//!
//! Every rectangle is derived from `screens::layout`, and every visual number
//! comes from `docs/vulkan-migration/ui-spec.md` §6 (measured off the archived
//! old-UI capture `docs/vulkan-migration/reference-old-ui.png` at 1160×740 and
//! the removed `ui/styles.css`). Keep it that way: the acceptance test is a
//! screenshot comparison.

mod overlays;
mod player_bar;
mod queue_panel;
mod sidebar;
mod titlebar;

pub use overlays::{banner_text, toast, update_banner};
pub use player_bar::player_bar;
pub use queue_panel::queue_panel;
pub use sidebar::sidebar;
pub use titlebar::titlebar;

/// Fixtures shared by the chrome submodules' interaction tests.
///
/// The player-bar and sidebar scripts both drive the real renderer with a
/// synthetic [`crate::ui::InputState`]; before this module the `VIEWPORT` and
/// `input_at` helpers existed twice, once per test module (split-review finding
/// #2). Test-only: no production behaviour or rendered output changes.
#[cfg(test)]
pub(crate) mod test_support {
    use crate::state::State;
    use crate::ui::{InputState, Rect};

    /// The reference window size every screenshot and chrome test lays out for.
    pub(crate) const VIEWPORT: Rect = Rect::new(0.0, 0.0, 1160.0, 740.0);

    /// A synthetic one-frame input snapshot with the pointer at `pos`.
    pub(crate) fn input_at(pos: (f32, f32), time: f64) -> InputState {
        InputState {
            viewport: VIEWPORT,
            mouse_pos: pos,
            mouse_in_window: true,
            time,
            ..Default::default()
        }
    }

    /// The deterministic preview state positioned like the reference screenshot
    /// (Home view, 335 px sidebar). Tests layer their own fields on top.
    pub(crate) fn preview_state() -> State {
        let mut state = crate::preview::preview_state();
        state.view = crate::state::View::Home;
        state.sidebar_width = 335.0;
        state
    }
}
