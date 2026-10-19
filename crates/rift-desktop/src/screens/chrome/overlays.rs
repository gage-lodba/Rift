//! Toast / update-banner wrappers. The cards themselves are owned by
//! `screens::overlays` (screens-pages); these wrappers keep the root's call
//! sites stable and expose the banner text helper.

use rift_types::UpdateStatus;

use crate::screens::overlays;
use crate::state::{Action, State};
use crate::ui::{Rect, Ui};

// ---------------------------------------------------------------------------
// overlays
// ---------------------------------------------------------------------------

/// The transient toast — drawn by `overlays.rs` (screens-pages owns the card;
/// this wrapper keeps the root's call sites stable).
pub fn toast(ui: &mut Ui, state: &mut State, actions: &mut Vec<Action>, viewport: Rect) {
    overlays::toast(ui, state, actions, viewport);
}

/// The dismissible "update available" banner — drawn by `overlays.rs`.
pub fn update_banner(ui: &mut Ui, state: &mut State, actions: &mut Vec<Action>, viewport: Rect) {
    overlays::update_banner(ui, state, actions, viewport);
}

/// The status shown by the update banner's text (kept public for tests).
pub fn banner_text(status: &UpdateStatus) -> String {
    format!(
        "Rift v{} is available.",
        status.latest.clone().unwrap_or_default()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn banner_text_matches_the_old_component() {
        let status = UpdateStatus {
            current: "0.4.0".into(),
            latest: Some("0.5.0".into()),
            update_available: true,
            url: None,
        };
        assert_eq!(banner_text(&status), "Rift v0.5.0 is available.");
    }
}
