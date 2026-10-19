//! "UP NEXT" queue panel body (`pages::queue_panel`).
//!
//! Spec: `ui-spec.md` §6.4.
//!
//! The panel surface and 1 px top border are part of this module because `area`
//! is the whole panel rectangle. Header 44 px (`padding: 8px 16px`): caret
//! (`expand_more` 14, pointing right when collapsed), `UP NEXT (n)` at 10/700
//! with 1.5 px tracking in dim, and the 28 px clear-queue trash button at the
//! right. The list is 190 px tall, rows are 44 px (`padding: 5px 8px`, 34 px
//! cover, 10 px gap, radius 6): current-track title accent, hover background,
//! hover-revealed remove `X`, click-to-jump and drag-to-reorder.
//!
//! Collapse state is per-session. The old app kept it in the `QueuePanel`
//! component; the native `State` has no field yet, so it lives here (and
//! `screens::queue_height` can ask [`collapsed`] once screens-chrome wires it).

use std::cell::Cell;

use crate::screens::cards;
use crate::state::{Action, State};
use crate::text::Weight;
use crate::ui::{Id, Rect, Sense, Ui};

/// `.queue-head` height: `padding: 8px` + a 28 px row.
pub const HEADER_H: f32 = 44.0;
/// `.qrow` height: 34 px cover + `padding: 5px`.
pub const ROW_H: f32 = 44.0;
/// `.qrow-cover` 34x34, radius 4.
pub const COVER: f32 = 34.0;
/// `.queue-list` bottom padding 8 px.
pub const LIST_PAD_BOTTOM: f32 = 8.0;
/// `.queue-list { padding: 0 8px 8px }`.
pub const LIST_PAD_X: f32 = 8.0;
/// `.qrow { gap: 10px }`.
pub const ROW_GAP: f32 = 10.0;
/// The clear/remove `.ibtn` is 28 px.
pub const IBTN: f32 = 28.0;

thread_local! {
    /// Whether the panel is collapsed for this session (per UI thread; the app
    /// has exactly one, tests get an isolated harness thread each).
    static COLLAPSED: Cell<bool> = const { Cell::new(false) };
    /// Index of the row being dragged (0-based), if a drag is in progress.
    static DRAG_FROM: Cell<Option<usize>> = const { Cell::new(None) };
}

/// Whether the panel is collapsed for this session. `mod.rs`'s queue height can
/// read this so main grows when the panel is collapsed.
pub fn collapsed() -> bool {
    COLLAPSED.with(|c| c.get())
}

/// Collapse/expand the panel.
pub fn set_collapsed(value: bool) {
    COLLAPSED.with(|c| c.set(value));
}

/// Toggle the panel's collapsed state.
pub fn toggle_collapsed() {
    COLLAPSED.with(|c| c.set(!c.get()));
}

/// Height of the panel when expanded (235 px in the reference).
pub fn expanded_height(ui: &Ui) -> f32 {
    ui.theme().queue_height
}

/// Height of the panel when collapsed (just the header; 45 px with the border).
pub fn collapsed_height(ui: &Ui) -> f32 {
    ui.theme().queue_collapsed_height
}

/// Draw the queue panel body into `area` (the full panel rect including its 1 px
/// top border).
pub fn panel(ui: &mut Ui, state: &mut State, actions: &mut Vec<Action>, area: Rect) {
    let theme = ui.theme().clone();

    // ---- retained drag state, validated before anything paints/interacts ----
    //
    // Two things can leave `DRAG_FROM` pointing at a row that no longer makes
    // sense, and both must be dropped at the top of the frame:
    //  * window focus loss — the release edge for a held drag is never delivered
    //    (`main.rs` sends no synthetic release and `Ui::begin_frame` drops the
    //    press latch), so the drag has to be cancelled instead of staying
    //    pending across refocus;
    //  * a shrinking/emptied queue — the index can outlive the row it named
    //    (e.g. the track finished while the button was held), and a drop would
    //    reference a removed row.
    // `drag_cancelled` marks the frame as drag-handled: a release that lands in
    // this same frame emits neither the cancelled drag's `QueueMove` nor a click.
    let track_count = state.queue.tracks.len();
    let mut drag_cancelled = false;
    if let Some(from) = DRAG_FROM.with(|from| from.get()) {
        let stale = track_count == 0 || from >= track_count;
        if ui.input().focus_lost || stale {
            DRAG_FROM.with(|from| from.set(None));
            drag_cancelled = true;
        }
    }

    ui.fill(area, theme.queue_bg);
    ui.hline(area.x, area.y, area.w, 1.0, theme.border);

    // ---- header ----
    let header = Rect::new(area.x, area.y + 1.0, area.w, HEADER_H);
    let is_collapsed = collapsed();

    // The clear button is interacted first so its press latches before the
    // header's click-to-collapse (the header covers the whole band).
    let clear_rect = Rect::new(
        header.right() - 16.0 - IBTN,
        header.y + (HEADER_H - IBTN) * 0.5,
        IBTN,
        IBTN,
    );
    let clear_clicked = cards::icon_button(
        ui,
        clear_rect,
        Id::from_str("queue/clear"),
        crate::ui::Icon::Delete,
        16.0,
        theme.text_dim,
    )
    .clicked;
    let header_resp = ui.interact(header, Id::from_str("queue/header"), Sense::CLICK);
    let caret_rect = Rect::new(
        header.x + 16.0,
        header.y + (HEADER_H - 14.0) * 0.5,
        14.0,
        14.0,
    );
    ui.icon(
        caret_rect,
        if is_collapsed {
            crate::ui::Icon::ChevronRight
        } else {
            crate::ui::Icon::ExpandMore
        },
        theme.text_dim,
        14.0,
    );
    let title_style =
        cards::style_lh(10.0, Weight::Bold, theme.text_dim, 13.0).with_letter_spacing(1.5);
    ui.text(
        Rect::new(caret_rect.right() + 6.0, header.y, 320.0, HEADER_H),
        &format!("UP NEXT ({})", state.queue.tracks.len()),
        &title_style.with_color(if header_resp.hovered {
            theme.text
        } else {
            theme.text_dim
        }),
    );
    if clear_clicked {
        actions.push(Action::QueueClear);
        return;
    }
    if header_resp.clicked {
        toggle_collapsed();
        actions.push(Action::ToggleQueueCollapsed);
        return;
    }
    if is_collapsed {
        return;
    }

    // ---- list ----
    let list = Rect::new(
        area.x,
        header.bottom(),
        area.w,
        (area.bottom() - header.bottom()).max(0.0),
    );
    let tracks = state.queue.tracks.clone();
    let current = state.queue.current;
    let count = track_count;
    let content_h = count as f32 * ROW_H + LIST_PAD_BOTTOM;
    let list_id = Id::from_str("queue/list");
    let mut clicked_row: Option<usize> = None;
    let mut removed_row: Option<usize> = None;

    let ((), scroll) = ui.scroll_area(list_id, list, content_h, |ui, content| {
        for (i, track) in tracks.iter().enumerate() {
            let row = Rect::new(
                content.x + LIST_PAD_X,
                content.y + i as f32 * ROW_H,
                (content.w - LIST_PAD_X * 2.0).max(0.0),
                ROW_H,
            );
            if row.y > list.bottom() + ROW_H {
                break;
            }
            let row_id = list_id.child(i as u64);
            let hovered = ui.hovered(row);
            let is_current = current == Some(i);
            if hovered {
                ui.rounded_rect(row, theme.bg_hover, theme.radius_md);
            }
            let cover_rect = Rect::new(row.x + 8.0, row.y + 5.0, COVER, COVER);
            let texture = state.request_cover(&track.cover);
            cards::cover_tile(ui, state, cover_rect, texture, 4.0);
            let meta_x = cover_rect.right() + ROW_GAP;
            let meta_w = (row.right() - 8.0 - meta_x - IBTN - 8.0).max(0.0);
            let meta_y = row.y + (ROW_H - (17.0 + 16.0)) * 0.5;
            ui.text(
                Rect::new(meta_x, meta_y, meta_w, 17.0),
                &track.title,
                &cards::style_lh(
                    13.0,
                    Weight::Medium,
                    if is_current { theme.accent } else { theme.text },
                    17.0,
                ),
            );
            ui.text(
                Rect::new(meta_x, meta_y + 17.0, meta_w, 16.0),
                &track.artist,
                &cards::style_lh(12.0, Weight::Regular, theme.text_dim, 16.0),
            );

            // Remove (nested control) interacts before the row so pressing it
            // never also jumps the queue.
            let mut remove_clicked = false;
            if hovered {
                let remove_rect = Rect::new(
                    row.right() - 8.0 - IBTN,
                    row.y + (ROW_H - IBTN) * 0.5,
                    IBTN,
                    IBTN,
                );
                remove_clicked = cards::icon_button(
                    ui,
                    remove_rect,
                    list_id.with(("remove", i as u64)),
                    crate::ui::Icon::Close,
                    16.0,
                    theme.text_dim,
                )
                .clicked;
            }
            let resp = ui.interact(row, row_id, Sense::DRAG);
            if resp.drag_started && !drag_cancelled {
                DRAG_FROM.with(|from| from.set(Some(i)));
            }
            if remove_clicked {
                removed_row = Some(i);
            } else if resp.clicked {
                clicked_row = Some(i);
            }
            // Cursor: the framework's auto-pointer gives the normal hand over
            // the row and its buttons; `Grabbing` is set only while a drag is
            // actually held (see the drag-reorder block below), matching the
            // sidebar playlist list.
        }
    });

    // ---- drag-reorder ----
    let dragging = if drag_cancelled {
        None
    } else {
        DRAG_FROM.with(|from| from.get())
    };
    if let Some(from) = dragging {
        let mouse_y = ui.input().mouse_pos.1;
        let target = (((mouse_y - scroll.content_rect.y) / ROW_H).round() as isize)
            .clamp(0, count.saturating_sub(1) as isize) as usize;
        if ui.input().mouse_left_down {
            ui.set_cursor(crate::ui::CursorShape::Grabbing);
            // Insertion indicator.
            let y = scroll.content_rect.y + target as f32 * ROW_H;
            ui.hline(
                list.x + LIST_PAD_X,
                y,
                (list.w - LIST_PAD_X * 2.0).max(0.0),
                2.0,
                theme.accent,
            );
        }
        if ui.input().mouse_left_released {
            if target != from {
                actions.push(Action::QueueMove { from, to: target });
            }
            DRAG_FROM.with(|drag| drag.set(None));
        }
    }

    // A frame that cancelled a retained drag has already "handled" the drag:
    // a release landing in that same frame must not turn into a click either.
    if drag_cancelled {
        return;
    }

    if let Some(i) = removed_row {
        actions.push(Action::QueueRemove(i));
    } else if let Some(i) = clicked_row {
        actions.push(Action::QueueJump(i));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard};

    /// The collapse/drag flags are per-thread; serialize these tests so the
    /// synthetic frames never interleave and start from a known state.
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn lock() -> MutexGuard<'static, ()> {
        let guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        set_collapsed(false);
        DRAG_FROM.with(|drag| drag.set(None));
        guard
    }

    use crate::screens::menu::harness::Sim;

    fn queue_area() -> Rect {
        Rect::new(335.0, 421.0, 825.0, 235.0)
    }

    /// Row `i`'s clickable centre inside the queue list (rows start at 466).
    fn queue_row_center(i: usize) -> (f32, f32) {
        (543.0, 466.0 + i as f32 * ROW_H + ROW_H * 0.5)
    }

    #[test]
    fn row_click_emits_queue_jump() {
        let _guard = lock();
        let mut state = crate::preview::preview_state();
        state.queue = crate::preview::preview_queue();
        let area = queue_area();
        let mut sim = Sim::new();
        let mut draw = |ui: &mut Ui, actions: &mut Vec<Action>| {
            panel(ui, &mut state, actions, area);
        };
        let actions = sim.click(queue_row_center(2), &mut draw);
        assert!(
            actions.iter().any(|a| matches!(a, Action::QueueJump(2))),
            "row click must jump: {actions:?}"
        );
    }

    #[test]
    fn remove_x_emits_queue_remove() {
        let _guard = lock();
        let mut state = crate::preview::preview_state();
        state.queue = crate::preview::preview_queue();
        let area = queue_area();
        let mut sim = Sim::new();
        let mut draw = |ui: &mut Ui, actions: &mut Vec<Action>| {
            panel(ui, &mut state, actions, area);
        };
        // Hover the row first (the X is hover-revealed), then click it.
        let row_center = queue_row_center(1);
        sim.move_to(row_center, &mut draw);
        // X: row.right() - 8 - 28 .. ; row right = 343 + 799 = 1142.
        let remove = (1142.0 - 8.0 - IBTN * 0.5, row_center.1);
        let actions = sim.click(remove, &mut draw);
        assert!(
            actions.iter().any(|a| matches!(a, Action::QueueRemove(1))),
            "X must remove row 1: {actions:?}"
        );
    }

    #[test]
    fn clear_button_emits_queue_clear() {
        let _guard = lock();
        let mut state = crate::preview::preview_state();
        state.queue = crate::preview::preview_queue();
        let area = queue_area();
        let mut sim = Sim::new();
        let mut draw = |ui: &mut Ui, actions: &mut Vec<Action>| {
            panel(ui, &mut state, actions, area);
        };
        // Clear ibtn: 16px from the right edge, 28px wide, centred in the header.
        let clear = (
            area.right() - 16.0 - IBTN * 0.5,
            area.y + 1.0 + HEADER_H * 0.5,
        );
        let actions = sim.click(clear, &mut draw);
        assert!(
            actions.iter().any(|a| matches!(a, Action::QueueClear)),
            "clear button: {actions:?}"
        );
    }

    #[test]
    fn header_click_toggles_collapse() {
        let _guard = lock();
        let mut state = crate::preview::preview_state();
        state.queue = crate::preview::preview_queue();
        let area = queue_area();
        let mut sim = Sim::new();
        let mut draw = |ui: &mut Ui, actions: &mut Vec<Action>| {
            panel(ui, &mut state, actions, area);
        };
        let header = (600.0, area.y + 1.0 + HEADER_H * 0.5);
        let actions = sim.click(header, &mut draw);
        assert!(collapsed(), "header click collapses the panel");
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, Action::ToggleQueueCollapsed)),
            "collapse action: {actions:?}"
        );
        set_collapsed(false);
    }

    #[test]
    fn dragging_row_zero_over_row_two_emits_queue_move() {
        let _guard = lock();
        let mut state = crate::preview::preview_state();
        state.queue = crate::preview::preview_queue();
        let area = queue_area();
        let mut sim = Sim::new();
        let mut draw = |ui: &mut Ui, actions: &mut Vec<Action>| {
            panel(ui, &mut state, actions, area);
        };
        let from = queue_row_center(0);
        let over = queue_row_center(2);
        sim.press(from, &mut draw);
        sim.move_to(over, &mut draw); // held: starts the drag
        let actions = sim.release(over, &mut draw);
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, Action::QueueMove { from: 0, to: 3 })),
            "drag 0 -> row 2 (insertion after it = 3): {actions:?}"
        );
    }
    #[test]
    fn row_and_cover_sizes_match_the_css() {
        let _guard = lock();
        assert_eq!(COVER, 34.0);
        assert_eq!(ROW_H, 44.0);
        assert_eq!(HEADER_H, 44.0);
        assert_eq!(IBTN, 28.0);
    }

    fn paint_queue(state: &mut State, area: Rect) -> crate::gfx::paint::PaintList {
        let mut ui = Ui::new();
        let viewport = Rect::new(0.0, 0.0, 1160.0, 740.0);
        let input = crate::ui::InputState {
            viewport,
            mouse_pos: (900.0, 500.0),
            mouse_in_window: true,
            time: 1.0,
            ..Default::default()
        };
        ui.begin_frame(input, viewport);
        let mut actions = Vec::new();
        panel(&mut ui, state, &mut actions, area);
        ui.end_frame()
    }

    /// Expanded: six 34 px cover tiles inside the 190 px list, one per queue row.
    #[test]
    fn expanded_panel_paints_one_cover_per_queue_row() {
        let _guard = lock();
        set_collapsed(false);
        let mut state = crate::preview::preview_state();
        state.queue = crate::preview::preview_queue();
        let area = Rect::new(335.0, 421.0, 825.0, 235.0);
        let paint = paint_queue(&mut state, area);
        let covers = paint
            .layers
            .iter()
            .flat_map(|l| l.prims.iter())
            .filter(|p| matches!(p, crate::gfx::paint::Prim::Image { rect, .. } if (rect.w - 34.0).abs() < 0.01))
            .count();
        assert_eq!(covers, 6, "one 34px cover per queue track");

        // The current row's title is accent (title 1 is `queue.current == Some(0)`).
        let accent = paint.layers.iter().flat_map(|l| l.prims.iter()).any(|p| {
            matches!(
                p,
                crate::gfx::paint::Prim::Image { tint, .. }
                    if *tint == crate::theme::Theme::default().accent
            )
        });
        assert!(accent, "current track title should be accent");
    }

    /// Collapsed: only the 44 px header band; no queue rows.
    #[test]
    fn collapsed_panel_paints_the_header_only() {
        let _guard = lock();
        set_collapsed(true);
        let mut state = crate::preview::preview_state();
        state.queue = crate::preview::preview_queue();
        let area = Rect::new(335.0, 421.0, 825.0, 235.0);
        let paint = paint_queue(&mut state, area);
        let covers = paint
            .layers
            .iter()
            .flat_map(|l| l.prims.iter())
            .filter(|p| matches!(p, crate::gfx::paint::Prim::Image { rect, .. } if (rect.w - 34.0).abs() < 0.01))
            .count();
        assert_eq!(covers, 0, "collapsed panel has no rows");
        assert!(!paint.is_empty(), "header still painted");
        set_collapsed(false);
    }

    #[test]
    fn cursors_are_pointer_on_hover_and_grabbing_only_while_dragging() {
        let _guard = lock();
        let mut state = crate::preview::preview_state();
        state.queue = crate::preview::preview_queue();
        let area = queue_area();
        let mut sim = Sim::new();
        let mut draw = |ui: &mut Ui, actions: &mut Vec<Action>| {
            panel(ui, &mut state, actions, area);
        };
        let from = queue_row_center(0);
        let over = queue_row_center(2);

        sim.move_to(from, &mut draw);
        assert_eq!(
            sim.ui.cursor_shape(),
            crate::ui::CursorShape::Pointer,
            "plain row hover shows the normal pointer"
        );

        sim.press(from, &mut draw);
        assert_eq!(
            sim.ui.cursor_shape(),
            crate::ui::CursorShape::Pointer,
            "pressing without movement is not a drag"
        );

        sim.move_to(over, &mut draw); // held: starts the drag
        assert_eq!(
            sim.ui.cursor_shape(),
            crate::ui::CursorShape::Grabbing,
            "a held queue drag shows the grabbing hand"
        );

        sim.release(over, &mut draw);
        assert_ne!(
            sim.ui.cursor_shape(),
            crate::ui::CursorShape::Grabbing,
            "the grabbing hand disappears on release"
        );
    }

    #[test]
    fn collapse_flag_toggles() {
        let _guard = lock();
        set_collapsed(false);
        assert!(!collapsed());
        toggle_collapsed();
        assert!(collapsed());
        set_collapsed(false);
        assert!(!collapsed());
    }

    #[test]
    fn panel_heights_come_from_the_theme() {
        let _guard = lock();
        let ui = Ui::new();
        assert_eq!(expanded_height(&ui), ui.theme().queue_height);
        assert_eq!(collapsed_height(&ui), ui.theme().queue_collapsed_height);
        // Reference: 235 expanded, 45 collapsed.
        assert_eq!(expanded_height(&ui), 235.0);
        assert_eq!(collapsed_height(&ui), 45.0);
    }

    // ------------------------------------------------------------------
    // retained-drag cancellation (window focus loss / shrinking queue)
    // ------------------------------------------------------------------

    /// Run one frame on `sim`'s UI with a custom `InputState`.
    ///
    /// The `Sim` harness has no focus-loss knob and it lives outside this module's
    /// scope, so this builds the frame from the sim's public pointer/time and lets
    /// the caller edit the input (e.g. set `focus_lost`). Time advances exactly
    /// like [`Sim::step`]; the frame's pointer stays where the sim left it.
    fn sim_frame_with(
        sim: &mut Sim,
        edit: impl FnOnce(&mut crate::ui::InputState),
        draw: &mut impl FnMut(&mut Ui, &mut Vec<Action>),
    ) -> Vec<Action> {
        let viewport = Rect::new(0.0, 0.0, 1160.0, 740.0);
        let mut input = crate::ui::InputState {
            viewport,
            mouse_pos: sim.pointer,
            mouse_in_window: true,
            time: sim.time,
            delta_time: 0.2,
            ..Default::default()
        };
        edit(&mut input);
        sim.ui.begin_frame(input, viewport);
        let mut actions = Vec::new();
        draw(&mut sim.ui, &mut actions);
        let _ = sim.ui.end_frame();
        sim.time += 1.0;
        actions
    }

    /// Window focus loss drops the release edge of a held drag. The retained drag
    /// index must be cancelled without emitting `QueueMove`, the later release
    /// frame must stay silent, and the first click after refocus must still work
    /// (no stale active latch).
    #[test]
    fn focus_loss_cancels_a_retained_drag() {
        let _guard = lock();
        let mut state = crate::preview::preview_state();
        state.queue = crate::preview::preview_queue();
        let area = queue_area();
        let mut sim = Sim::new();
        let mut draw = |ui: &mut Ui, actions: &mut Vec<Action>| {
            panel(ui, &mut state, actions, area);
        };
        let from = queue_row_center(0);
        let over = queue_row_center(2);
        sim.press(from, &mut draw);
        sim.move_to(over, &mut draw); // held: starts the drag
        assert!(
            DRAG_FROM.with(|from| from.get()).is_some(),
            "the drag is retained before focus loss"
        );

        // Window focus is lost: `main.rs` sends no synthetic release edge.
        let actions = sim_frame_with(&mut sim, |input| input.focus_lost = true, &mut draw);
        assert!(
            actions.is_empty(),
            "the focus-loss frame emits nothing: {actions:?}"
        );
        assert!(
            DRAG_FROM.with(|from| from.get()).is_none(),
            "focus loss clears the retained drag"
        );

        // The physical release happened while unfocused, so a release afterwards
        // must not reorder the queue.
        let actions = sim.release(over, &mut draw);
        assert!(
            !actions
                .iter()
                .any(|a| matches!(a, Action::QueueMove { .. })),
            "release after focus loss must not emit a move: {actions:?}"
        );

        // The first click after refocusing is not swallowed by the old latch.
        let actions = sim.click(queue_row_center(1), &mut draw);
        assert!(
            actions.iter().any(|a| matches!(a, Action::QueueJump(1))),
            "the first click after refocus must jump: {actions:?}"
        );
    }

    /// If the release and the focus loss land in the SAME frame, the queue must
    /// treat the frame as drag-handled: neither a move nor a click may escape.
    #[test]
    fn focus_loss_and_release_in_one_frame_cannot_drop() {
        let _guard = lock();
        let mut state = crate::preview::preview_state();
        state.queue = crate::preview::preview_queue();
        let area = queue_area();
        let mut sim = Sim::new();
        let mut draw = |ui: &mut Ui, actions: &mut Vec<Action>| {
            panel(ui, &mut state, actions, area);
        };
        let from = queue_row_center(0);
        let over = queue_row_center(2);
        sim.press(from, &mut draw);
        sim.move_to(over, &mut draw);
        assert!(DRAG_FROM.with(|from| from.get()).is_some());

        let actions = sim_frame_with(
            &mut sim,
            |input| {
                input.focus_lost = true;
                input.mouse_left_released = true;
            },
            &mut draw,
        );
        assert!(
            !actions
                .iter()
                .any(|a| matches!(a, Action::QueueMove { .. } | Action::QueueJump(_))),
            "a same-frame focus loss + release emits neither move nor jump: {actions:?}"
        );
        assert!(DRAG_FROM.with(|from| from.get()).is_none());
    }

    /// A queue that shrinks under a held drag invalidates the retained index (the
    /// same rule the sidebar's playlist drag follows): no drop may reference the
    /// removed row.
    #[test]
    fn shrinking_queue_clears_a_stale_drag_index() {
        let _guard = lock();
        let mut state = crate::preview::preview_state();
        state.queue = crate::preview::preview_queue();
        let area = queue_area();
        let mut sim = Sim::new();
        let from = queue_row_center(2);
        let over = queue_row_center(0);
        {
            let mut draw = |ui: &mut Ui, actions: &mut Vec<Action>| {
                panel(ui, &mut state, actions, area);
            };
            sim.press(from, &mut draw);
            sim.move_to(over, &mut draw); // held: starts the drag on row 2
            assert_eq!(DRAG_FROM.with(|from| from.get()), Some(2));
        }

        // The queue shrinks under the held drag (e.g. the track finished).
        state.queue.tracks.truncate(1);
        let mut draw = |ui: &mut Ui, actions: &mut Vec<Action>| {
            panel(ui, &mut state, actions, area);
        };
        let actions = sim.step(&mut draw);
        assert!(
            actions.is_empty(),
            "the stale-index frame emits nothing: {actions:?}"
        );
        assert!(
            DRAG_FROM.with(|from| from.get()).is_none(),
            "a shrunken queue clears the retained drag index"
        );
        let actions = sim.release(over, &mut draw);
        assert!(
            !actions
                .iter()
                .any(|a| matches!(a, Action::QueueMove { .. })),
            "no drop may reference the removed row: {actions:?}"
        );
    }
}
