//! Settings view (`View::Settings`). This module directory replaces the former
//! 1,225-line `settings.rs`; `mod.rs` wires the submodules and re-exports the
//! public surface (`height`, `view`) so `screens::settings::*` is unchanged.
//!
//! Spec: `ui-spec.md` §7.8. Rows are `.settings-row` cards:
//! `max-width: 640`, `padding: 16px 18px`, flex `gap: 20`, raised background,
//! 1 px border, radius 10, `+ 12px` between rows. Left column: 15/600 label +
//! 13 dim description (wrapped); right column: the control.
//!
//! Rows in order: Discord Rich Presence (switch), Crossfade (switch + 1..12 s
//! slider when on), Updates (status line, "Check now" / "Download update",
//! notification switch).

mod layout;
#[cfg(test)]
mod tests;
mod updates;
mod view;

pub use layout::{
    height, row_heights, BTN_H, DESC_GAP, LABEL_H, ROW_GAP, ROW_MAX_W, ROW_PAD_X, ROW_PAD_Y,
    ROW_RADIUS, ROW_SPACING, SWITCH_H, SWITCH_W,
};
pub use view::view;
