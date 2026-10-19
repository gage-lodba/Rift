//! The queue-panel band wrapper: paints the band surface and delegates the
//! panel contents to `screens::queue` (owned by screens-pages).

use crate::screens::queue;
use crate::state::{Action, State};
use crate::theme::Theme;
use crate::ui::{Rect, Ui};

// ---------------------------------------------------------------------------
// queue panel
// ---------------------------------------------------------------------------

/// The collapsible "UP NEXT (n)" panel. The panel contents (header + list) are
/// owned by `screens-pages`' `queue.rs`; this is the single owner wrapper the
/// root calls, and it also paints the band background/border the panel sits on.
pub fn queue_panel(ui: &mut Ui, state: &mut State, actions: &mut Vec<Action>, rect: Rect) {
    let theme: Theme = ui.theme().clone();
    ui.fill(rect, theme.queue_bg);
    ui.hline(rect.x, rect.y, rect.w, 1.0, theme.border);
    queue::panel(ui, state, actions, rect);
}
