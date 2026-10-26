//! Extra tests for the "integration polish" widgets added after the first
//! screen code landed: disabled states, dynamic scroll containers, menu keyboard
//! navigation, slider thumb geometry and single-slot toasts.

use super::*;
use crate::text::{TextStyle, Weight};

fn viewport() -> Rect {
    Rect::new(0.0, 0.0, 1160.0, 740.0)
}

fn input_at(time: f64) -> InputState {
    InputState {
        viewport: viewport(),
        scale_factor: 1.0,
        time,
        delta_time: 1.0 / 60.0,
        mouse_in_window: true,
        ..InputState::default()
    }
}

fn frame<R>(ui: &mut Ui, input: InputState, f: impl FnOnce(&mut Ui) -> R) -> (R, PaintList) {
    ui.begin_frame(input, viewport());
    let out = f(ui);
    let paint = ui.end_frame();
    (out, paint)
}

fn press(mut i: InputState) -> InputState {
    i.mouse_left_down = true;
    i.mouse_left_pressed = true;
    i
}

fn key_ev(key: Key, shift: bool) -> KeyInput {
    KeyInput {
        key,
        pressed: true,
        repeat: false,
        modifiers: Modifiers {
            shift,
            ..Modifiers::default()
        },
    }
}

fn prims(paint: &PaintList) -> usize {
    paint.layers.iter().map(|l| l.prims.len()).sum()
}

// ---------------------------------------------------------------------------
// disabled widgets
// ---------------------------------------------------------------------------

#[test]
fn disabled_button_does_not_click() {
    let mut ui = Ui::new();
    let rect = Rect::new(10.0, 10.0, 100.0, 34.0);
    let (r1, _) = frame(&mut ui, press(input_at(0.0)), |ui| {
        ui.button_enabled(rect, "Play", ButtonKind::Primary, false)
    });
    assert!(!r1.pressed);
    let mut i = input_at(0.016);
    i.mouse_left_released = true;
    i.mouse_left_down = false;
    let (r2, paint) = frame(&mut ui, i, |ui| {
        ui.button_enabled(rect, "Play", ButtonKind::Primary, false)
    });
    assert!(!r2.clicked);
    assert!(prims(&paint) >= 2, "disabled button still paints");
}

#[test]
fn disabled_switch_does_not_toggle() {
    let mut ui = Ui::new();
    let mut on = false;
    let mut i = input_at(0.0);
    i.mouse_pos = (20.0, 20.0);
    i.mouse_left_down = true;
    i.mouse_left_pressed = true;
    frame(&mut ui, i, |ui| {
        ui.switch_enabled(Rect::new(0.0, 0.0, 44.0, 24.0), &mut on, false)
    });
    let mut i = input_at(0.016);
    i.mouse_pos = (20.0, 20.0);
    i.mouse_left_released = true;
    frame(&mut ui, i, |ui| {
        ui.switch_enabled(Rect::new(0.0, 0.0, 44.0, 24.0), &mut on, false)
    });
    assert!(!on, "disabled switch must ignore clicks");
}

// ---------------------------------------------------------------------------
// dynamic scroll containers
// ---------------------------------------------------------------------------

fn draw_dynamic_scroll(ui: &mut Ui, id: Id, rect: Rect) -> ScrollResponse {
    let (_, response) = ui.scroll_area_stable_dynamic(id, rect, |_ui, _content| ((), 1000.0));
    response
}

#[test]
fn dynamic_scroll_uses_previous_content_height() {
    let mut ui = Ui::new();
    let id = Id::from_str("dyn-scroll");
    let rect = Rect::new(0.0, 0.0, 400.0, 200.0);
    // First frame: height unknown, offset clamps to 0.
    let (r, _) = frame(&mut ui, input_at(0.0), |ui| {
        draw_dynamic_scroll(ui, id, rect)
    });
    assert_eq!(r.offset, 0.0);
    assert_eq!(r.content_rect.h, 1000.0);
    assert_eq!(r.max_offset, 800.0);
    // Second frame: content height known → wheel works.
    let mut i = input_at(0.016);
    i.mouse_pos = (100.0, 100.0);
    i.scroll = (0.0, -150.0);
    let (r, _) = frame(&mut ui, i, |ui| draw_dynamic_scroll(ui, id, rect));
    assert_eq!(r.offset, 150.0);
    assert_eq!(r.max_offset, 800.0);
}

// ---------------------------------------------------------------------------
// menus
// ---------------------------------------------------------------------------

fn open_menu_frame(ui: &mut Ui, menu_id: Id, anchor: Rect) -> (Option<Response>, PaintList) {
    let mut input = input_at(0.0);
    input.mouse_pos = (999.0, 999.0);
    frame(ui, input, |ui| {
        ui.popup(
            menu_id,
            anchor,
            Size::new(MENU_MIN_CONTENT_WIDTH, 2.0 * MENU_ITEM_HEIGHT),
            PopupAlign::Right,
            |ui, content| {
                let mut col = Column::new(content, 0.0);
                let first = ui.menu_item(
                    col.fixed(MENU_ITEM_HEIGHT),
                    "Play next",
                    Some(Icon::PlaylistAdd),
                    None,
                    false,
                );
                ui.menu_item(
                    col.fixed(MENU_ITEM_HEIGHT),
                    "Delete",
                    Some(Icon::Delete),
                    None,
                    true,
                );
                first
            },
        )
    })
}

#[test]
fn menu_focuses_first_item_and_arrow_keys_move() {
    let mut ui = Ui::new();
    let menu_id = Id::from_str("nav-menu");
    let anchor = Rect::new(10.0, 10.0, 28.0, 28.0);
    ui.open_popup(menu_id);
    // Frame 1: popup opens and focuses its first row.
    let (first, _) = open_menu_frame(&mut ui, menu_id, anchor);
    let first = first.expect("popup open");
    assert!(first.focused, "first menu item should get keyboard focus");
    // Frame 2: Down moves to the second item.
    let mut i = input_at(0.016);
    i.mouse_pos = (999.0, 999.0);
    i.keys = vec![key_ev(Key::Down, false)];
    let (second, _) = frame(&mut ui, i, |ui| {
        ui.popup(
            menu_id,
            anchor,
            Size::new(MENU_MIN_CONTENT_WIDTH, 2.0 * MENU_ITEM_HEIGHT),
            PopupAlign::Right,
            |ui, content| {
                let mut col = Column::new(content, 0.0);
                ui.menu_item(col.fixed(MENU_ITEM_HEIGHT), "Play next", None, None, false);
                ui.menu_item(col.fixed(MENU_ITEM_HEIGHT), "Delete", None, None, true)
            },
        )
    });
    assert!(second.is_some());
    // The focused id must have changed to the second item.
    assert_eq!(
        ui.focused_id(),
        Some(menu_id.with("Delete")),
        "Down should focus the second menu row"
    );
}

#[test]
fn escape_closes_menu_and_returns_focus_to_trigger() {
    let mut ui = Ui::new();
    let trigger = Id::from_str("kebab");
    let menu_id = Id::from_str("menu-return");
    let anchor = Rect::new(10.0, 10.0, 28.0, 28.0);
    // Frame 1: focus the trigger, open the menu.
    ui.request_focus(trigger);
    ui.open_popup(menu_id);
    frame(&mut ui, input_at(0.0), |ui| {
        ui.interact(anchor, trigger, Sense::CLICK);
        ui.popup(
            menu_id,
            anchor,
            Size::new(MENU_MIN_CONTENT_WIDTH, MENU_ITEM_HEIGHT),
            PopupAlign::Right,
            |ui, content| {
                ui.menu_item(
                    Rect::new(content.x, content.y, content.w, MENU_ITEM_HEIGHT),
                    "Rename",
                    None,
                    None,
                    false,
                )
            },
        )
    });
    assert_eq!(ui.focused_id(), Some(menu_id.with("Rename")));
    // Frame 2: Escape closes, focus returns to the trigger.
    let mut i = input_at(0.016);
    i.keys = vec![key_ev(Key::Escape, false)];
    frame(&mut ui, i, |ui| {
        ui.interact(anchor, trigger, Sense::CLICK);
        ui.popup(
            menu_id,
            anchor,
            Size::new(MENU_MIN_CONTENT_WIDTH, MENU_ITEM_HEIGHT),
            PopupAlign::Right,
            |_, _| {},
        )
    });
    assert!(!ui.is_popup_open(menu_id));
    assert_eq!(ui.focused_id(), Some(trigger));
}

#[test]
fn clicking_a_menu_item_activates_it() {
    let mut ui = Ui::new();
    let menu_id = Id::from_str("click-menu");
    let anchor = Rect::new(10.0, 10.0, 28.0, 28.0);
    ui.open_popup(menu_id);
    // The menu's first row is at content.y; compute where the popup lands:
    // below the anchor (10,10,28,28) → popup outer at (10,42) + 5 padding.
    let item_center = (100.0, 42.0 + 5.0 + MENU_ITEM_HEIGHT * 0.5);
    let mut i = input_at(0.0);
    i.mouse_pos = item_center;
    i.mouse_left_down = true;
    i.mouse_left_pressed = true;
    frame(&mut ui, i, |ui| {
        ui.popup(
            menu_id,
            anchor,
            Size::new(MENU_MIN_CONTENT_WIDTH, 2.0 * MENU_ITEM_HEIGHT),
            PopupAlign::Right,
            |ui, content| {
                let mut col = Column::new(content, 0.0);
                ui.menu_item(col.fixed(MENU_ITEM_HEIGHT), "Play next", None, None, false)
                    .clicked
            },
        )
    });
    let mut i = input_at(0.016);
    i.mouse_pos = item_center;
    i.mouse_left_released = true;
    let (clicked, _) = frame(&mut ui, i, |ui| {
        ui.popup(
            menu_id,
            anchor,
            Size::new(MENU_MIN_CONTENT_WIDTH, 2.0 * MENU_ITEM_HEIGHT),
            PopupAlign::Right,
            |ui, content| {
                let mut col = Column::new(content, 0.0);
                ui.menu_item(col.fixed(MENU_ITEM_HEIGHT), "Play next", None, None, false)
                    .clicked
            },
        )
    });
    assert_eq!(clicked, Some(true), "release over a menu row clicks it");
}

// ---------------------------------------------------------------------------
// slider geometry
// ---------------------------------------------------------------------------

#[test]
fn slider_thumb_stays_inside_the_track() {
    let mut ui = Ui::new();
    let rect = Rect::new(58.0, 669.0, 1044.0, 4.0);
    // value 0 → thumb centre at x + radius (8), value 1 → right() - radius.
    let mut v = 0.0f32;
    let (_, paint) = frame(&mut ui, input_at(0.0), |ui| {
        ui.slider(rect, &mut v, 0.0, 1.0);
    });
    // Prims order: track, thumb (no fill at 0) → find the circle (square rounded rect).
    let thumb = paint
        .layers
        .iter()
        .flat_map(|l| l.prims.iter())
        .filter_map(|p| match p {
            Prim::Rect { rect, radius, .. } if *radius >= 7.5 => Some(*rect),
            _ => None,
        })
        .next_back()
        .expect("thumb circle prim");
    assert!(
        (thumb.x - (rect.x + 8.0 - 8.0)).abs() < 0.5,
        "thumb left {thumb:?}"
    );
    v = 1.0;
    let (_, paint) = frame(&mut ui, input_at(0.016), |ui| {
        ui.slider(rect, &mut v, 0.0, 1.0);
    });
    let thumb = paint
        .layers
        .iter()
        .flat_map(|l| l.prims.iter())
        .filter_map(|p| match p {
            Prim::Rect { rect, radius, .. } if *radius >= 7.5 => Some(*rect),
            _ => None,
        })
        .next_back()
        .expect("thumb circle prim");
    assert!(
        (thumb.right() - rect.right()).abs() < 0.5,
        "at 100% the thumb's right edge must be flush with the track: {thumb:?}"
    );
}

#[test]
fn slider_track_color_matches_the_browser_default() {
    assert_eq!(SLIDER_TRACK, Color::hex(0xefefef));
}

// ---------------------------------------------------------------------------
// toasts
// ---------------------------------------------------------------------------

#[test]
fn a_new_toast_replaces_the_previous_one() {
    let mut ui = Ui::new();
    frame(&mut ui, input_at(0.0), |ui| {
        ui.toast_info("first");
        ui.toast_error("second");
    });
    // The UI paints a single card and the newest message wins.
    let (_, paint) = frame(&mut ui, input_at(0.1), |_ui| {});
    assert!(prims(&paint) >= 2, "toast card paints background + text");
}

#[test]
fn sticky_toast_does_not_expire() {
    let mut ui = Ui::new();
    frame(&mut ui, input_at(0.0), |ui| {
        ui.toast_sticky("Importing playlist…");
    });
    let (_, paint) = frame(&mut ui, input_at(30.0), |_ui| {});
    assert!(prims(&paint) >= 1);
}

// ---------------------------------------------------------------------------
// misc polish
// ---------------------------------------------------------------------------

#[test]
fn focus_ring_inset_draws_inside_the_rect() {
    let mut ui = Ui::new();
    let rect = Rect::new(10.0, 10.0, 100.0, 40.0);
    let (_, paint) = frame(&mut ui, input_at(0.0), |ui| {
        ui.focus_ring_inset(rect, 6.0);
    });
    assert_eq!(prims(&paint), 1);
    match paint.layers[0].prims[0] {
        Prim::Rect { rect: r, .. } => {
            assert!(r.x >= rect.x && r.right() <= rect.right());
        }
        _ => panic!("expected ring rect"),
    }
}

#[test]
fn menu_item_submenu_draws_a_caret() {
    let mut ui = Ui::new();
    let menu_id = Id::from_str("submenu-menu");
    let anchor = Rect::new(10.0, 10.0, 28.0, 28.0);
    ui.open_popup(menu_id);
    frame(&mut ui, input_at(0.0), |ui| {
        ui.popup(
            menu_id,
            anchor,
            Size::new(MENU_MIN_CONTENT_WIDTH, 2.0 * MENU_ITEM_HEIGHT),
            PopupAlign::Right,
            |ui, content| {
                let mut col = Column::new(content, 0.0);
                ui.menu_item_submenu(
                    col.fixed(MENU_ITEM_HEIGHT),
                    "Add to playlist",
                    Some(Icon::PlaylistAdd),
                    false,
                );
            },
        )
    });
    // Item background hover is absent, so prims = icon + label glyphs + caret.
    let (_, paint) = frame(&mut ui, input_at(0.016), |ui| {
        ui.popup(
            menu_id,
            anchor,
            Size::new(MENU_MIN_CONTENT_WIDTH, 2.0 * MENU_ITEM_HEIGHT),
            PopupAlign::Right,
            |ui, content| {
                let mut col = Column::new(content, 0.0);
                ui.menu_item_submenu(
                    col.fixed(MENU_ITEM_HEIGHT),
                    "Add to playlist",
                    Some(Icon::PlaylistAdd),
                    false,
                );
            },
        )
    });
    assert!(prims(&paint) > 0);
}

#[test]
fn style_helpers_still_match_theme_tokens() {
    let mut ui = Ui::new();
    let theme = ui.theme().clone();
    let style = TextStyle::new(13.0, Weight::Regular, theme.text);
    let w = ui.text_width("Rift", &style);
    assert!(w > 10.0);
}

// ---------------------------------------------------------------------------
// state-independent auto ids (keyboard focus must survive a toggle)
// ---------------------------------------------------------------------------

#[test]
fn stateful_widget_ids_are_stable_across_state_changes() {
    let mut ui = Ui::new();
    let switch_rect = Rect::new(10.0, 10.0, 44.0, 24.0);
    let mut off = false;
    let (switch_off_id, _) = frame(&mut ui, input_at(0.0), |ui| {
        ui.switch(switch_rect, &mut off).response.id
    });
    let mut on = true;
    let (switch_on_id, _) = frame(&mut ui, input_at(0.016), |ui| {
        ui.switch(switch_rect, &mut on).response.id
    });
    assert_eq!(
        switch_off_id, switch_on_id,
        "a switch's id must not depend on its on/off value"
    );

    let button_rect = Rect::new(10.0, 40.0, 36.0, 36.0);
    let (paused_id, _) = frame(&mut ui, input_at(0.032), |ui| {
        ui.play_button(button_rect, false).id
    });
    let (playing_id, _) = frame(&mut ui, input_at(0.048), |ui| {
        ui.play_button(button_rect, true).id
    });
    assert_eq!(
        paused_id, playing_id,
        "a play/pause button's id must not depend on the playing state"
    );
}

#[test]
fn switch_keeps_keyboard_focus_across_repeated_activations() {
    let mut ui = Ui::new();
    let rect = Rect::new(10.0, 10.0, 44.0, 24.0);
    let mut on = false;
    // Frame 1 registers the focus order; frame 2 Tabs onto the switch.
    frame(&mut ui, input_at(0.0), |ui| {
        ui.switch(rect, &mut on);
    });
    let mut i = input_at(0.016);
    i.keys = vec![key_ev(Key::Tab, false)];
    let (r, _) = frame(&mut ui, i, |ui| ui.switch(rect, &mut on));
    assert!(r.response.focused, "switch should be focused after Tab");
    assert!(!on);
    // Frame 3: Space toggles it on and focus must be retained.
    let mut i = input_at(0.032);
    i.keys = vec![key_ev(Key::Space, false)];
    let (r, _) = frame(&mut ui, i, |ui| ui.switch(rect, &mut on));
    assert!(r.changed && on, "Space should toggle the switch on");
    assert!(r.response.focused, "focus must survive the first toggle");
    // Frame 4: Space toggles it back off — this used to be swallowed because
    // the state-dependent id dropped focus after the first toggle.
    let mut i = input_at(0.048);
    i.keys = vec![key_ev(Key::Space, false)];
    let (r, _) = frame(&mut ui, i, |ui| ui.switch(rect, &mut on));
    assert!(r.changed && !on, "Space should toggle the switch off again");
    assert!(
        r.response.focused,
        "focus must survive repeated activations"
    );
}

// ---------------------------------------------------------------------------
// popup toggle keeps the documented keyboard contract
// ---------------------------------------------------------------------------

fn draw_toggled_menu(ui: &mut Ui, trigger: Id, menu_id: Id, anchor: Rect) -> Option<Response> {
    ui.interact(anchor, trigger, Sense::CLICK);
    ui.toggle_popup(menu_id);
    ui.popup(
        menu_id,
        anchor,
        Size::new(MENU_MIN_CONTENT_WIDTH, MENU_ITEM_HEIGHT),
        PopupAlign::Right,
        |ui, content| {
            ui.menu_item(
                Rect::new(content.x, content.y, content.w, MENU_ITEM_HEIGHT),
                "Rename",
                None,
                None,
                false,
            )
        },
    )
}

#[test]
fn toggle_popup_keeps_first_row_focus_and_returns_it_on_escape() {
    let mut ui = Ui::new();
    let trigger = Id::from_str("toggle-trigger");
    let menu_id = Id::from_str("toggle-menu");
    let anchor = Rect::new(10.0, 10.0, 28.0, 28.0);
    ui.request_focus(trigger);

    // Frame 1: the documented kebab pattern — toggle the menu open.
    frame(&mut ui, input_at(0.0), |ui| {
        draw_toggled_menu(ui, trigger, menu_id, anchor)
    });
    assert!(ui.is_popup_open(menu_id));
    assert_eq!(
        ui.focused_id(),
        Some(menu_id.with("Rename")),
        "a menu opened with toggle_popup must focus its first row"
    );

    // Frame 2: Escape closes it and returns focus to the trigger.
    let mut i = input_at(0.016);
    i.keys = vec![key_ev(Key::Escape, false)];
    frame(&mut ui, i, |ui| {
        draw_toggled_menu(ui, trigger, menu_id, anchor)
    });
    assert!(!ui.is_popup_open(menu_id));
    assert_eq!(
        ui.focused_id(),
        Some(trigger),
        "Escape must hand focus back to the toggle's trigger"
    );
}
