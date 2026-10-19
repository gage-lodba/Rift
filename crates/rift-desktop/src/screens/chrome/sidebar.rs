//! The left navigation: Home/Liked nav, the reorderable playlist list, the
//! import actions, Settings, and the drag-resize handle.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use crate::screens::{cards, menu};
use crate::state::{Action, State, View};
use crate::text::Weight;
use crate::theme::Theme;
use crate::ui::{CursorShape, Icon, Id, Key, Rect, Sense, Ui};

mod editors;
mod row;
#[cfg(test)]
mod tests;

use self::editors::{eq_bars, import_playlist_input, new_playlist_input};
use self::row::{
    playlist_menu_entries, side_row, sidebar_pinned_rows, RowBehavior, DIVIDER_MARGIN,
    FILE_TO_DIVIDER, IMPORT_TO_FILE, SETTINGS_FROM_BOTTOM, SIDE_ITEM_GAP, SIDE_ITEM_H, SIDE_PAD,
    SIDE_PAD_V,
};

/// Row pitch of the playlist list (38 px row + 3 px gap).
const PLAYLIST_PITCH: f32 = SIDE_ITEM_H + SIDE_ITEM_GAP;

/// Slot-spring stiffness/velocity damping for the reorder animation.
///
/// This is the native analogue of the old UI's `transition: transform .15s
/// ease` on `.reordering` rows: ζ = C / (2√K) ≈ 0.56, and at 60 Hz the
/// semi-implicit Euler step peaks at ~7.9 % overshoot and settles (2 % band)
/// in ~0.32 s. Stiff enough to feel snappy, soft enough that rows visibly glide
/// into their slots (measured by `spring_slot_overshoots_slightly_and_settles_quickly`).
pub(super) const SLOT_SPRING_K: f32 = 500.0;
pub(super) const SLOT_SPRING_C: f32 = 25.0;

/// One playlist row's animated slot position/velocity (slot units; 1 slot =
/// one 41 px row pitch).
#[derive(Clone, Copy, Debug, Default)]
struct SlotSpring {
    x: f32,
    v: f32,
}

/// Expected playlist order retained briefly after a drop/keyboard move so the
/// released rows glide into their slots *before* the backend event reorders the
/// library — without this the springs would snap back to the old order and jump
/// again when the data lands.
#[derive(Clone, Debug)]
struct CommittedOrder {
    ids: Vec<String>,
    until: f64,
}

thread_local! {
    /// Index of the playlist row being dragged (0-based), if a drag is in
    /// progress. Per UI thread, like the queue panel's `DRAG_FROM`; tests reset
    /// it through [`reset_reorder_animation_for_tests`].
    static PLAYLIST_DRAG_FROM: Cell<Option<usize>> = const { Cell::new(None) };
    /// Animated slot positions/velocities, keyed by playlist id so a row keeps
    /// its spring while the list is reordered underneath it.
    static SLOT_SPRINGS: RefCell<HashMap<String, SlotSpring>> = RefCell::new(HashMap::new());
    /// Post-drop committed order + deadline (see [`CommittedOrder`]).
    static COMMITTED_ORDER: RefCell<Option<CommittedOrder>> = const { RefCell::new(None) };
}

/// Step one damped-spring frame (semi-implicit Euler, `dt` clamped so a janky
/// frame can never blow the integration up).
///
/// Returns the new `(position, velocity)`. Pure so the tuning can be unit
/// tested: from 0 toward 1 at 60 Hz it peaks slightly above 1 and settles in
/// well under half a second.
pub(super) fn spring_slot(current: f32, velocity: f32, target: f32, dt: f32) -> (f32, f32) {
    // A paused/first frame (or a bad clock) must not move the row: only real
    // elapsed time advances the spring. Clamp *after* this check so dt = 0 is
    // an exact no-op instead of being floored to 1/240.
    if !dt.is_finite() || dt <= 0.0 {
        return (current, velocity);
    }
    let dt = dt.clamp(1.0 / 240.0, 1.0 / 30.0);
    let accel = SLOT_SPRING_K * (target - current) - SLOT_SPRING_C * velocity;
    let v = velocity + accel * dt;
    (current + v * dt, v)
}

/// Playlist-id order after moving the row at `from` to `to`.
///
/// Remove-then-insert, exactly the backend `move_playlist` semantics, so this
/// is the order the library will have once the move event lands.
pub(super) fn moved_order(ids: &[String], from: usize, to: usize) -> Vec<String> {
    if from >= ids.len() || to >= ids.len() || from == to {
        return ids.to_vec();
    }
    let mut out = ids.to_vec();
    let moved = out.remove(from);
    out.insert(to, moved);
    out
}

/// Reset the retained drag/spring/hold state. Tests serialize through
/// `FLOW_LOCK` and call this so a failed assertion cannot leak animation state
/// into the next test.
#[cfg(test)]
pub(super) fn reset_reorder_animation_for_tests() {
    PLAYLIST_DRAG_FROM.with(|from| from.set(None));
    SLOT_SPRINGS.with(|springs| springs.borrow_mut().clear());
    COMMITTED_ORDER.with(|hold| *hold.borrow_mut() = None);
}

/// Visual row index for row `i` while row `from` is dragged over slot `over`.
///
/// The dragged row sits at the target slot, and rows between the two positions
/// slide one slot toward the vacated one — the old UI's live
/// `use_reorder::drag_shift_style` preview (rows reorder under the pointer while
/// the drag is held, not just an insertion marker). Immediate-mode 60 Hz pointer
/// updates are the animation; nothing is lerped.
pub(super) fn drag_visual_index(i: usize, from: usize, over: usize) -> usize {
    if i == from {
        over
    } else if from < over && i > from && i <= over {
        i - 1
    } else if from > over && i >= over && i < from {
        i + 1
    } else {
        i
    }
}

/// The left navigation: Home/Liked nav, the playlist list, import actions and
/// Settings, with the drag-resize handle on its right edge.
pub fn sidebar(ui: &mut Ui, state: &mut State, actions: &mut Vec<Action>, rect: Rect) {
    let theme: Theme = ui.theme().clone();
    ui.fill(rect, theme.sidebar_bg);
    // 1 px border on the right edge (drawn inside the sidebar's width).
    ui.vline(rect.right() - 1.0, rect.y, rect.h, 1.0, theme.border);

    // ---- drag-resize handle (5 px on the right edge) ----
    let handle = Rect::new(rect.right() - 5.0, rect.y, 5.0, rect.h);
    let handle_resp = ui.interact(handle, Id::from_str("sidebar/resizer"), Sense::DRAG);
    if handle_resp.hovered || handle_resp.active {
        ui.set_cursor(CursorShape::ResizeH);
        ui.fill(handle, theme.accent.with_alpha(89));
    }
    if handle_resp.dragged {
        let width = ui
            .input()
            .mouse_pos
            .0
            .clamp(theme.sidebar_width_min, theme.sidebar_width_max);
        state.sidebar_width = width;
    }
    // Keyboard resize (the old UI made the handle a separator too).
    if handle_resp.focused {
        let step = if ui.input().modifiers.shift {
            32.0
        } else {
            8.0
        };
        let mut width = state.sidebar_width;
        if ui.input().key_clicked(Key::Left) {
            width -= step;
        }
        if ui.input().key_clicked(Key::Right) {
            width += step;
        }
        if ui.input().key_clicked(Key::Home) {
            width = theme.sidebar_width_min;
        }
        if ui.input().key_clicked(Key::End) {
            width = theme.sidebar_width_max;
        }
        state.sidebar_width = width.clamp(theme.sidebar_width_min, theme.sidebar_width_max);
    }

    let x = rect.x + SIDE_PAD;
    let w = (rect.w - SIDE_PAD * 2.0).max(0.0);
    let mut y = rect.y + SIDE_PAD_V;

    // ---- nav rows ----
    let home_active = state.view == View::Home;
    if side_row(
        ui,
        Rect::new(x, y, w, SIDE_ITEM_H),
        Some(Icon::Home),
        "Home",
        home_active,
        None,
        RowBehavior::NAV,
    )
    .response
    .clicked
    {
        actions.push(Action::Navigate(View::Home));
    }
    y += SIDE_ITEM_H + SIDE_ITEM_GAP;

    // Liked Songs: the lead icon is the equalizer while it is the playing source.
    // The equalizer **replaces** the heart (the old `.eq` was the row's only lead
    // icon while playing), so the heart glyph is suppressed when the bars take
    // the slot. The heart is the solid `favorite` glyph, exactly like the old
    // sidebar's `lead("liked", "heart")`; the outline variant is only the
    // "not liked" state on track rows.
    let liked_active = state.view == View::Liked;
    let liked_playing = state.playing_source() == Some("liked");
    let liked = side_row(
        ui,
        Rect::new(x, y, w, SIDE_ITEM_H),
        if liked_playing {
            None
        } else {
            Some(Icon::HeartFilled)
        },
        "Liked Songs",
        liked_active,
        None,
        RowBehavior::NAV,
    );
    if liked_playing {
        eq_bars(ui, liked.icon_rect, theme.accent, state.is_playing());
    }
    if liked.response.clicked {
        actions.push(Action::Navigate(View::Liked));
    }
    // The label sits 18 px below the Liked row: in the old `.side-scroll` block
    // the row's 3 px margin collapses with the label's 18 px top margin.
    y += SIDE_ITEM_H;

    // ---- PLAYLISTS label (10 px, letter-spacing 1.5, margins 18 top / 6 bottom) ----
    y += 20.0;
    let label_style =
        cards::style_lh(10.0, Weight::Bold, theme.text_dim, 13.0).with_letter_spacing(1.5);
    ui.text(
        Rect::new(x + SIDE_PAD, y, w, 13.0),
        "PLAYLISTS",
        &label_style,
    );
    y += 13.0 + 6.0;

    // Scrollable playlist region, clipped between here and the pinned bottom.
    let pinned_import_y = rect.bottom()
        - SETTINGS_FROM_BOTTOM
        - DIVIDER_MARGIN * 2.0
        - 1.0
        - FILE_TO_DIVIDER
        - IMPORT_TO_FILE
        - SIDE_ITEM_H;
    let region = Rect::new(x, y, w, (pinned_import_y - 8.0 - y).max(0.0));
    let playlists = state.sidebar_playlists();
    let playlist_count = playlists.len();
    let playlist_ids: Vec<String> = playlists.iter().map(|(id, _, _)| id.clone()).collect();
    // Window focus loss: the release edge for a held drag will never arrive, so
    // cancel it without committing (task-50). The frame is marked handled so the
    // stale click cannot navigate; `Ui::begin_frame` has already dropped its
    // active latch, so nothing is latched after refocusing either.
    let mut nav_suppressed = false;
    if ui.input().focus_lost && PLAYLIST_DRAG_FROM.with(|from| from.get().is_some()) {
        PLAYLIST_DRAG_FROM.with(|from| from.set(None));
        nav_suppressed = true;
    }
    // A shrinking/emptied list invalidates a retained drag index (e.g. the
    // playlist was deleted while the button was held).
    if PLAYLIST_DRAG_FROM.with(|from| {
        from.get()
            .is_some_and(|from| playlist_count == 0 || from >= playlist_count)
    }) {
        PLAYLIST_DRAG_FROM.with(|from| from.set(None));
    }

    // ---- committed-order hold (no snap-back) ----
    // After a drop (or Alt+Up/Down) the expected order is retained for a short
    // grace period: rows glide toward it immediately while the backend event is
    // in flight, then the hold clears when the real order matches (or after
    // ~2 s, or when the list changed incompatibly).
    COMMITTED_ORDER.with(|hold| {
        let mut hold = hold.borrow_mut();
        if let Some(held) = hold.as_ref() {
            let expired = ui.time() > held.until;
            let caught_up = held.ids == playlist_ids;
            let incompatible = held.ids.len() != playlist_count
                || held.ids.iter().any(|id| !playlist_ids.contains(id));
            if expired || caught_up || incompatible {
                *hold = None;
            }
        }
    });
    let committed: Option<Vec<String>> =
        COMMITTED_ORDER.with(|hold| hold.borrow().as_ref().map(|held| held.ids.clone()));

    // ---- target slot per row ----
    // While a drag is held, the row under the pointer's slot defines the live
    // preview (dragged row at the target slot, rows between slide one slot);
    // while the committed-order hold is active the target is the held order's
    // index for the row's id; otherwise the row's real index.
    let drag_from = PLAYLIST_DRAG_FROM.with(|from| from.get());
    let drag_over = drag_from.and_then(|_| {
        (playlist_count > 0).then(|| {
            (((ui.input().mouse_pos.1 - region.y) / PLAYLIST_PITCH).round() as isize)
                .clamp(0, playlist_count.saturating_sub(1) as isize) as usize
        })
    });
    let target_slot = |i: usize, id: &str| -> f32 {
        if let Some(from) = drag_from {
            drag_visual_index(i, from, drag_over.unwrap_or(from)) as f32
        } else if let Some(order) = committed.as_ref() {
            order.iter().position(|held| held == id).unwrap_or(i) as f32
        } else {
            i as f32
        }
    };

    // ---- step the slot springs ----
    let dt = ui.input().delta_time;
    let mut slot_x: Vec<f32> = Vec::with_capacity(playlist_count);
    let mut slot_target: Vec<f32> = Vec::with_capacity(playlist_count);
    for (i, id) in playlist_ids.iter().enumerate() {
        let target = target_slot(i, id);
        slot_target.push(target);
        // New rows appear settled at their target; existing rows glide.
        let x = SLOT_SPRINGS.with(|springs| {
            let mut springs = springs.borrow_mut();
            match springs.get_mut(id) {
                Some(spring) => {
                    let (x, v) = spring_slot(spring.x, spring.v, target, dt);
                    spring.x = x;
                    spring.v = v;
                    x
                }
                None => {
                    springs.insert(id.clone(), SlotSpring { x: target, v: 0.0 });
                    target
                }
            }
        });
        slot_x.push(x);
    }
    // Deleted playlists must not leave springs behind.
    SLOT_SPRINGS.with(|springs| {
        springs
            .borrow_mut()
            .retain(|id, _| playlist_ids.iter().any(|current| current == id));
    });

    // After a drop/Alt move the rows glide to their new slots for ~0.3 s. While
    // they are still moving, paint order (ascending index) does not match the
    // visual order, so a click could latch the visually-lower row mid-glide:
    // settling rows paint normally but are not interactive until they arrive.
    let settling = drag_from.is_none()
        && slot_x
            .iter()
            .zip(&slot_target)
            .any(|(x, target)| (x - target).abs() > 0.01);
    let interactive = !settling;

    let mut clicked_playlist: Option<String> = None;
    // Draw the dragged row last so it overlays the rows it glides across.
    let mut order: Vec<usize> = (0..playlist_count).collect();
    if let Some(from) = drag_from {
        order.retain(|i| *i != from);
        order.push(from);
    }
    let new_row_y = region.y + playlist_count as f32 * PLAYLIST_PITCH;
    ui.with_clip(region, |ui| {
        for i in order {
            let (id, name, count) = &playlists[i];
            // Animated position (fractional row slots); ids stay bound to the
            // row's identity, so hover/kebab/drag all follow the moving row.
            let row = Rect::new(x, region.y + slot_x[i] * PLAYLIST_PITCH, w, SIDE_ITEM_H);
            let active = matches!(&state.view, View::Playlist(v) if v == id);
            let playing = state.playing_source() == Some(format!("playlist:{id}").as_str());
            // Playing rows show the equalizer *instead of* the music-note glyph.
            let lead = if playing { None } else { Some(Icon::MusicNote) };
            let behavior = if interactive {
                RowBehavior::playlist()
            } else {
                RowBehavior::SETTLING
            };
            let resp = side_row(ui, row, lead, name, active, Some(*count), behavior);
            if resp.drag_started {
                PLAYLIST_DRAG_FROM.with(|from| from.set(Some(i)));
                // A fresh drag supersedes any in-flight committed order.
                COMMITTED_ORDER.with(|hold| *hold.borrow_mut() = None);
                // A drag that started this frame swallows the frame's click.
                nav_suppressed = true;
            }
            if playing {
                eq_bars(ui, resp.icon_rect, theme.accent, state.is_playing());
            }
            // The dragged row is "lifted": the normal hover background from
            // `side_row` plus a subtle accent outline (drawn after the content
            // so it never covers the label/kebab).
            if drag_from == Some(i) {
                ui.stroke(row, theme.radius_md, 1.0, theme.accent);
            }
            // Keyboard parity with the old UI: Alt+Up/Down on the focused row
            // moves it one slot; edges are a no-op. The expected order is held
            // just like a drop so the row glides instead of snapping back.
            // Alt+Up/Down is ignored while a post-drop committed order is in
            // flight (its indices would refer to the stale library order).
            if resp.response.focused && ui.input().modifiers.alt && committed.is_none() {
                let target = if i > 0 && ui.input().key_clicked(Key::Up) {
                    Some(i - 1)
                } else if i + 1 < playlist_count && ui.input().key_clicked(Key::Down) {
                    Some(i + 1)
                } else {
                    None
                };
                if let Some(target) = target {
                    actions.push(Action::MovePlaylist {
                        from: i,
                        to: target,
                    });
                    let expected = moved_order(&playlist_ids, i, target);
                    COMMITTED_ORDER.with(|hold| {
                        *hold.borrow_mut() = Some(CommittedOrder {
                            ids: expected,
                            until: ui.time() + 2.0,
                        });
                    });
                }
            }
            // Kebab / right-click → Rename · Export · Delete (old sidebar menu).
            // The kebab is interacted before the row, so it never starts a drag.
            let menu_id = Id::from_str("sidebar/playlist-menu").with(id);
            if resp.kebab_clicked || resp.secondary_clicked {
                if resp.kebab_clicked && ui.is_popup_open(menu_id) {
                    menu::close(ui);
                } else {
                    let anchor = if resp.secondary_clicked {
                        row
                    } else {
                        resp.kebab_rect
                    };
                    menu::open(ui, menu_id, anchor, playlist_menu_entries(id, name));
                }
            } else if resp.response.clicked && !menu::is_open() {
                // Deferred: a drag release in this frame must not navigate.
                clicked_playlist = Some(id.clone());
            }
        }

        // New playlist row / inline draft (static slot below the list).
        match state.new_playlist.clone() {
            Some(_) => {
                new_playlist_input(ui, state, actions, Rect::new(x, new_row_y, w, SIDE_ITEM_H));
            }
            None => {
                if side_row(
                    ui,
                    Rect::new(x, new_row_y, w, SIDE_ITEM_H),
                    Some(Icon::Add),
                    "New playlist",
                    false,
                    None,
                    RowBehavior::NAV,
                )
                .response
                .clicked
                {
                    actions.push(Action::BeginNewPlaylist);
                }
            }
        }

        // ---- drag-reorder ----
        // The gliding rows above are the preview (the old UI had no insertion
        // marker); keep the grabbing cursor and commit on release. The target
        // slot formula is the same one driving the live preview, and the
        // expected order is held afterwards so nothing snaps back.
        if let Some(from) = PLAYLIST_DRAG_FROM.with(|from| from.get()) {
            let target = (((ui.input().mouse_pos.1 - region.y) / PLAYLIST_PITCH).round() as isize)
                .clamp(0, playlist_count.saturating_sub(1) as isize)
                as usize;
            if ui.input().mouse_left_down {
                ui.set_cursor(CursorShape::Grabbing);
            }
            if ui.input().mouse_left_released {
                if target != from {
                    actions.push(Action::MovePlaylist { from, to: target });
                    let expected = moved_order(&playlist_ids, from, target);
                    COMMITTED_ORDER.with(|hold| {
                        *hold.borrow_mut() = Some(CommittedOrder {
                            ids: expected,
                            until: ui.time() + 2.0,
                        });
                    });
                    // Only a real move swallows the click; a release back in
                    // the origin slot still navigates.
                    nav_suppressed = true;
                }
                PLAYLIST_DRAG_FROM.with(|drag| drag.set(None));
            }
        }
    });
    // Plain clicks still navigate. Only a committed move, a drag that started
    // this frame or a focus-loss cancel swallows the frame's click.
    if let Some(id) = clicked_playlist {
        if !nav_suppressed {
            actions.push(Action::Navigate(View::Playlist(id)));
        }
    }

    // ---- pinned bottom block ----
    let (import_y, file_y, settings_y, divider_y) = sidebar_pinned_rows(rect);

    // Import playlist: a link/ID paste box replaces the row while it is open
    // (`.side-new-row` in the old CSS), exactly like "New playlist".
    match state.import_playlist.clone() {
        Some(_) => {
            import_playlist_input(ui, state, actions, Rect::new(x, import_y, w, SIDE_ITEM_H));
        }
        None => {
            if side_row(
                ui,
                Rect::new(x, import_y, w, SIDE_ITEM_H),
                Some(Icon::Download),
                "Import playlist",
                false,
                None,
                RowBehavior::NAV,
            )
            .response
            .clicked
            {
                state.import_playlist = Some(String::new());
            }
        }
    }
    if side_row(
        ui,
        Rect::new(x, file_y, w, SIDE_ITEM_H),
        Some(Icon::Album),
        "Import from file",
        false,
        None,
        RowBehavior::NAV,
    )
    .response
    .clicked
    {
        actions.push(Action::ImportPlaylistFile);
    }
    ui.hline(
        x + SIDE_PAD,
        divider_y,
        w - SIDE_PAD * 2.0,
        1.0,
        theme.border,
    );
    let settings_active = state.view == View::Settings;
    if side_row(
        ui,
        Rect::new(x, settings_y, w, SIDE_ITEM_H),
        Some(Icon::Settings),
        "Settings",
        settings_active,
        None,
        RowBehavior::NAV,
    )
    .response
    .clicked
    {
        actions.push(Action::Navigate(View::Settings));
    }
}
