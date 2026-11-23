//! Pure-logic tests for the immediate-mode UI: layout, ids, interaction
//! state machines, clipping layers, scroll and popup behaviour.
//!
//! No GPU, no window: tests drive `Ui` by feeding synthetic [`InputState`]
//! frames, exactly the way `app.rs` drives it from winit events.

use super::paint::TextureId;
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

fn mouse(mut i: InputState, x: f32, y: f32) -> InputState {
    i.mouse_pos = (x, y);
    i
}

fn press(mut i: InputState) -> InputState {
    i.mouse_left_down = true;
    i.mouse_left_pressed = true;
    i
}

fn release(mut i: InputState) -> InputState {
    i.mouse_left_released = true;
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

fn typed(mut i: InputState, text: &str) -> InputState {
    i.text = text.to_string();
    i
}

/// Run one frame: begin, closure, end.
fn frame<R>(ui: &mut Ui, input: InputState, f: impl FnOnce(&mut Ui) -> R) -> (R, PaintList) {
    ui.begin_frame(input, viewport());
    let out = f(ui);
    let paint = ui.end_frame();
    (out, paint)
}

fn prims(paint: &PaintList) -> usize {
    paint.layers.iter().map(|l| l.prims.len()).sum()
}

// ---------------------------------------------------------------------------
// painting
// ---------------------------------------------------------------------------

#[test]
fn fill_emits_one_rect_prim_with_theme_clear() {
    let mut ui = Ui::new();
    let (_, paint) = frame(&mut ui, input_at(0.0), |ui| {
        ui.fill(Rect::new(10.0, 10.0, 100.0, 50.0), Color::WHITE);
    });
    assert_eq!(paint.clear, ui.theme().bg);
    assert_eq!(paint.layers.len(), 1);
    assert_eq!(paint.layers[0].prims.len(), 1);
    match paint.layers[0].prims[0] {
        Prim::Rect { rect, .. } => assert_eq!(rect, Rect::new(10.0, 10.0, 100.0, 50.0)),
        _ => panic!("expected rect prim"),
    }
}

#[test]
fn empty_rects_and_transparent_colors_are_skipped() {
    let mut ui = Ui::new();
    let (_, paint) = frame(&mut ui, input_at(0.0), |ui| {
        ui.fill(Rect::new(0.0, 0.0, 0.0, 10.0), Color::WHITE);
        ui.fill(Rect::new(0.0, 0.0, 10.0, 10.0), Color::TRANSPARENT);
        ui.rounded_rect(Rect::new(0.0, 0.0, 10.0, 10.0), Color::WHITE, 4.0);
    });
    assert_eq!(prims(&paint), 1);
}

#[test]
fn clips_become_layers_and_preserve_order() {
    let mut ui = Ui::new();
    let (_, paint) = frame(&mut ui, input_at(0.0), |ui| {
        ui.fill(Rect::new(0.0, 0.0, 10.0, 10.0), Color::WHITE);
        ui.push_clip(Rect::new(0.0, 0.0, 100.0, 100.0));
        ui.fill(Rect::new(0.0, 0.0, 10.0, 10.0), Color::WHITE);
        ui.pop_clip();
        ui.fill(Rect::new(0.0, 0.0, 10.0, 10.0), Color::WHITE);
    });
    assert_eq!(paint.layers.len(), 3, "no-clip, clipped, no-clip");
    assert_eq!(paint.layers[0].clip, None);
    assert_eq!(
        paint.layers[1].clip,
        Some(Rect::new(0.0, 0.0, 100.0, 100.0))
    );
    assert_eq!(paint.layers[2].clip, None);
    assert_eq!(paint.layers.iter().map(|l| l.prims.len()).sum::<usize>(), 3);
}

#[test]
fn nested_clips_intersect() {
    let mut ui = Ui::new();
    let (_, paint) = frame(&mut ui, input_at(0.0), |ui| {
        ui.push_clip(Rect::new(0.0, 0.0, 100.0, 100.0));
        ui.push_clip(Rect::new(50.0, 50.0, 200.0, 200.0));
        ui.fill(Rect::new(0.0, 0.0, 10.0, 10.0), Color::WHITE);
        ui.pop_clip();
        ui.pop_clip();
    });
    assert_eq!(
        paint.layers[0].clip,
        Some(Rect::new(50.0, 50.0, 50.0, 50.0)),
        "clip should be the intersection"
    );
}

#[test]
fn deferred_content_paints_after_base_content() {
    let mut ui = Ui::new();
    let (_, paint) = frame(&mut ui, input_at(0.0), |ui| {
        ui.fill(Rect::new(0.0, 0.0, 10.0, 10.0), Color::WHITE);
        ui.deferred(|ui| {
            ui.fill(Rect::new(0.0, 0.0, 10.0, 10.0), Color::BLACK);
            ui.fill(Rect::new(0.0, 0.0, 10.0, 10.0), Color::BLACK);
        });
        ui.fill(Rect::new(0.0, 0.0, 10.0, 10.0), Color::WHITE);
    });
    assert_eq!(prims(&paint), 4);
    // Base layer first with 2 prims, deferred appended last with 2.
    let first_color = match paint.layers[0].prims[0] {
        Prim::Rect { color, .. } => color,
        _ => panic!(),
    };
    let last_color = match paint.layers.last().unwrap().prims[0] {
        Prim::Rect { color, .. } => color,
        _ => panic!(),
    };
    assert_eq!(first_color, Color::WHITE);
    assert_eq!(last_color, Color::BLACK);
}

// ---------------------------------------------------------------------------
// interaction
// ---------------------------------------------------------------------------

#[test]
fn button_click_requires_press_and_release_inside() {
    let mut ui = Ui::new();
    let id = Id::from_str("test-button");
    let rect = Rect::new(10.0, 10.0, 100.0, 30.0);

    // Frame 1: hover, no press.
    let (r, _) = frame(&mut ui, mouse(input_at(0.0), 50.0, 20.0), |ui| {
        ui.button_with_id(id, rect, "Play", ButtonKind::Primary)
    });
    assert!(r.hovered);
    assert!(!r.clicked);

    // Frame 2: press.
    let (r, _) = frame(&mut ui, press(mouse(input_at(0.016), 50.0, 20.0)), |ui| {
        ui.button_with_id(id, rect, "Play", ButtonKind::Primary)
    });
    assert!(r.pressed);
    assert!(r.active);

    // Frame 3: release inside → click.
    let (r, _) = frame(&mut ui, release(mouse(input_at(0.032), 50.0, 20.0)), |ui| {
        ui.button_with_id(id, rect, "Play", ButtonKind::Primary)
    });
    assert!(r.clicked);
    assert!(!r.active);
}

#[test]
fn button_release_outside_does_not_click() {
    let mut ui = Ui::new();
    let id = Id::from_str("test-button-2");
    let rect = Rect::new(10.0, 10.0, 100.0, 30.0);
    frame(&mut ui, press(mouse(input_at(0.0), 50.0, 20.0)), |ui| {
        ui.button_with_id(id, rect, "Play", ButtonKind::Primary)
    });
    let (r, _) = frame(
        &mut ui,
        release(mouse(input_at(0.016), 500.0, 500.0)),
        |ui| ui.button_with_id(id, rect, "Play", ButtonKind::Primary),
    );
    assert!(!r.clicked);
}

#[test]
fn widgets_outside_clip_do_not_hover() {
    let mut ui = Ui::new();
    let id = Id::from_str("clipped");
    let rect = Rect::new(10.0, 10.0, 100.0, 30.0);
    let (r, _) = frame(&mut ui, mouse(input_at(0.0), 50.0, 20.0), |ui| {
        ui.push_clip(Rect::new(0.0, 0.0, 5.0, 5.0));
        let r = ui.interact(rect, id, Sense::CLICK);
        ui.pop_clip();
        r
    });
    assert!(!r.hovered);
}

#[test]
fn tab_focus_cycles_through_widgets_in_paint_order() {
    let mut ui = Ui::new();
    let a = Rect::new(0.0, 0.0, 100.0, 30.0);
    let b = Rect::new(0.0, 40.0, 100.0, 30.0);
    let id_a = Id::from_str("a");
    let id_b = Id::from_str("b");
    let draw = |ui: &mut Ui| {
        let ra = ui.interact(a, id_a, Sense::CLICK);
        let rb = ui.interact(b, id_b, Sense::CLICK);
        (ra, rb)
    };
    // Frame 1 registers focus order.
    frame(&mut ui, input_at(0.0), draw);
    // Frame 2: Tab → first widget gets focus.
    let mut i = input_at(0.016);
    i.keys = vec![key_ev(Key::Tab, false)];
    let ((ra, rb), _) = frame(&mut ui, i, draw);
    assert!(ra.focused ^ rb.focused, "exactly one widget focused");
    assert!(ra.focused);
    // Frame 3: Tab → second.
    let mut i = input_at(0.032);
    i.keys = vec![key_ev(Key::Tab, false)];
    let ((ra, rb), _) = frame(&mut ui, i, draw);
    assert!(!ra.focused && rb.focused);
    // Frame 4: Shift+Tab → back to first.
    let mut i = input_at(0.048);
    i.keys = vec![key_ev(Key::Tab, true)];
    let ((ra, rb), _) = frame(&mut ui, i, draw);
    assert!(ra.focused && !rb.focused);
}

#[test]
fn clicking_a_focusable_widget_focuses_it_without_visible_ring() {
    let mut ui = Ui::new();
    let rect = Rect::new(0.0, 0.0, 100.0, 30.0);
    let id = Id::from_str("focus-me");
    let (r, _) = frame(&mut ui, press(mouse(input_at(0.0), 10.0, 10.0)), |ui| {
        ui.interact(rect, id, Sense::CLICK)
    });
    assert!(r.focused);
    assert!(
        !r.focus_visible,
        "mouse focus does not draw the keyboard ring"
    );
    assert_eq!(ui.focused_id(), Some(id));
}

// ---------------------------------------------------------------------------
// keyboard vs pointer activation
// ---------------------------------------------------------------------------

/// Enter/Space on a focused button is still a click; it is just flagged as
/// keyboard-originated so positional widgets can ignore it.
#[test]
fn focused_button_activates_on_enter() {
    let mut ui = Ui::new();
    let id = Id::from_str("keyboard-button");
    let rect = Rect::new(10.0, 10.0, 100.0, 30.0);
    ui.request_focus(id);
    let mut i = input_at(0.0);
    i.keys = vec![key_ev(Key::Enter, false)];
    let (r, _) = frame(&mut ui, i, |ui| {
        ui.button_with_id(id, rect, "Play", ButtonKind::Primary)
    });
    assert!(r.clicked, "Enter must activate a focused button");
    assert!(r.keyboard_activated);
    assert!(!r.pointer_clicked(), "the click is not positional");
    assert!(r.focused);
}

/// A focused slider receiving Enter/Space must not commit the fraction at the
/// parked mouse position: the synthesised keyboard click carries no pointer.
#[test]
fn focused_slider_ignores_keyboard_activation() {
    let mut ui = Ui::new();
    let id = Id::from_str("keyboard-slider");
    let rect = Rect::new(10.0, 10.0, 200.0, 20.0);
    let style = SliderStyle::from_theme(ui.theme());
    let mut value = 0.0f32;
    ui.request_focus(id);
    // Park the mouse at the far right: committing `mouse_pos` would set 1.0.
    let mut i = mouse(input_at(0.0), 220.0, 20.0);
    i.keys = vec![key_ev(Key::Enter, false)];
    let (r, _) = frame(&mut ui, i, |ui| {
        ui.slider_styled(id, rect, &mut value, 0.0, 1.0, &style)
    });
    assert!(r.response.keyboard_activated);
    assert!(!r.response.pointer_clicked());
    assert!(!r.changed);
    assert_eq!(value, 0.0, "Enter must not seek to the parked mouse");

    // Space is the other activation key; same contract.
    let mut i = mouse(input_at(0.016), 220.0, 20.0);
    i.keys = vec![key_ev(Key::Space, false)];
    let (r, _) = frame(&mut ui, i, |ui| {
        ui.slider_styled(id, rect, &mut value, 0.0, 1.0, &style)
    });
    assert!(r.response.keyboard_activated);
    assert!(!r.changed);
    assert_eq!(value, 0.0, "Space must not seek to the parked mouse");
}

/// The keyboard fix must not disable ordinary pointer clicks.
#[test]
fn slider_mouse_click_still_commits_the_pointer_fraction() {
    let mut ui = Ui::new();
    let id = Id::from_str("pointer-slider");
    let rect = Rect::new(10.0, 10.0, 200.0, 20.0);
    let style = SliderStyle::from_theme(ui.theme());
    let mut value = 0.0f32;
    let (mx, my) = rect.center();
    let (r, _) = frame(&mut ui, press(mouse(input_at(0.0), mx, my)), |ui| {
        ui.slider_styled(id, rect, &mut value, 0.0, 1.0, &style)
    });
    assert!(r.response.active);
    assert!(!r.response.keyboard_activated);
    assert!(
        r.changed,
        "the press commits the fraction under the pointer"
    );

    let (r, _) = frame(&mut ui, release(mouse(input_at(0.016), mx, my)), |ui| {
        ui.slider_styled(id, rect, &mut value, 0.0, 1.0, &style)
    });
    assert!(r.response.clicked);
    assert!(!r.response.keyboard_activated);
    assert!(r.response.pointer_clicked());
    assert!((value - 0.5).abs() < 0.02, "value {value}");
}

// ---------------------------------------------------------------------------
// text
// ---------------------------------------------------------------------------

fn body_style(ui: &Ui) -> TextStyle {
    TextStyle::new(14.0, Weight::Regular, ui.theme().text)
}

#[test]
fn text_emits_one_image_prim_per_nonspace_glyph() {
    let mut ui = Ui::new();
    let (_, paint) = frame(&mut ui, input_at(0.0), |ui| {
        let style = body_style(ui);
        ui.text(Rect::new(0.0, 0.0, 300.0, 20.0), "Hi", &style);
    });
    assert_eq!(prims(&paint), 2);
    assert!(matches!(paint.layers[0].prims[0], Prim::Image { .. }));
}

#[test]
fn text_is_ellipsized_to_the_rect() {
    let mut ui = Ui::new();
    let long = "this is a very long label that cannot possibly fit";
    let narrow = Rect::new(0.0, 0.0, 40.0, 20.0);
    let (width_measure, _) = frame(&mut ui, input_at(0.0), |ui| {
        (
            ui.text_width("\u{2026}", &body_style(ui)),
            ui.text(narrow, long, &body_style(ui)),
        )
    });
    let ell_w = width_measure.0;
    assert!(ell_w > 0.0);
    // The run's total glyph extent must stay inside the rect (ellipsis fits).
    let mut ui2 = Ui::new();
    let (_, paint) = frame(&mut ui2, input_at(0.0), |ui| {
        ui.text(narrow, long, &body_style(ui));
    });
    let max_right = paint
        .layers
        .iter()
        .flat_map(|l| l.prims.iter())
        .filter_map(|p| match p {
            Prim::Image { rect, .. } => Some(rect.right()),
            _ => None,
        })
        .fold(0.0f32, f32::max);
    assert!(
        max_right <= narrow.right() + 1.0,
        "text overflowed: {max_right}"
    );
}

#[test]
fn glyph_uvs_point_at_the_font_atlas() {
    let mut ui = Ui::new();
    let (_, paint) = frame(&mut ui, input_at(0.0), |ui| {
        let style = body_style(ui);
        ui.text(Rect::new(0.0, 0.0, 300.0, 20.0), "A", &style);
    });
    match paint.layers[0].prims[0] {
        Prim::Image { texture, uv, .. } => {
            assert_eq!(texture, ATLAS_TEXTURE_ID);
            assert!(uv.w > 0.0 && uv.h > 0.0);
        }
        _ => panic!("expected glyph image prim"),
    }
}

#[test]
fn the_font_atlas_is_uploaded_once_as_texture_one() {
    let mut ui = Ui::new();
    let (_, paint) = frame(&mut ui, input_at(0.0), |ui| {
        let style = body_style(ui);
        ui.text(Rect::new(0.0, 0.0, 300.0, 20.0), "Rift", &style);
    });
    assert_eq!(
        paint.created.len(),
        1,
        "atlas created on the first text frame"
    );
    assert_eq!(paint.created[0].id, ATLAS_TEXTURE_ID);
    assert!(paint.created[0].width >= 512);
    // Second frame with no new glyphs: no re-upload.
    let (_, paint) = frame(&mut ui, input_at(0.016), |ui| {
        let style = body_style(ui);
        ui.text(Rect::new(0.0, 0.0, 300.0, 20.0), "Rift", &style);
    });
    assert!(paint.updated.is_empty());
    assert!(paint.created.is_empty());
}

#[test]
fn icons_emit_image_prims() {
    let mut ui = Ui::new();
    let (_, paint) = frame(&mut ui, input_at(0.0), |ui| {
        ui.icon(
            Rect::new(0.0, 0.0, 16.0, 16.0),
            Icon::Play,
            Color::WHITE,
            16.0,
        );
    });
    assert_eq!(prims(&paint), 1);
}

// ---------------------------------------------------------------------------
// scroll
// ---------------------------------------------------------------------------

fn draw_scroll(ui: &mut Ui, id: Id, rect: Rect) -> ScrollResponse {
    let (_, response) = ui.scroll_area(id, rect, 1000.0, |_ui, _content| {});
    response
}

#[test]
fn wheel_scrolls_the_hovered_container() {
    let mut ui = Ui::new();
    let id = Id::from_str("scroll-test");
    let rect = Rect::new(0.0, 0.0, 400.0, 200.0);
    let (r, _) = frame(&mut ui, mouse(input_at(0.0), 100.0, 100.0), |ui| {
        draw_scroll(ui, id, rect)
    });
    assert_eq!(r.offset, 0.0);
    assert_eq!(r.max_offset, 800.0);
    // Wheel down (negative y in winit convention) scrolls content down.
    let mut i = mouse(input_at(0.016), 100.0, 100.0);
    i.scroll = (0.0, -120.0);
    let (r, _) = frame(&mut ui, i, |ui| draw_scroll(ui, id, rect));
    assert_eq!(r.offset, 120.0);
    assert!(r.scrolled);
    // Clamps at the top.
    let mut i = mouse(input_at(0.032), 100.0, 100.0);
    i.scroll = (0.0, 500.0);
    let (r, _) = frame(&mut ui, i, |ui| draw_scroll(ui, id, rect));
    assert_eq!(r.offset, 0.0, "wheel up at the top clamps");
}

#[test]
fn wheel_does_not_scroll_when_pointer_is_outside() {
    let mut ui = Ui::new();
    let id = Id::from_str("scroll-outside");
    let rect = Rect::new(0.0, 0.0, 400.0, 200.0);
    let mut i = mouse(input_at(0.0), 900.0, 600.0);
    i.scroll = (0.0, -100.0);
    let (r, _) = frame(&mut ui, i, |ui| draw_scroll(ui, id, rect));
    assert_eq!(r.offset, 0.0);
}

#[test]
fn scroll_offsets_are_retained_and_resettable() {
    let mut ui = Ui::new();
    let id = Id::from_str("scroll-retain");
    let rect = Rect::new(0.0, 0.0, 400.0, 200.0);
    ui.set_scroll_offset(id, 300.0);
    let (r, _) = frame(&mut ui, mouse(input_at(0.0), 100.0, 100.0), |ui| {
        draw_scroll(ui, id, rect)
    });
    assert_eq!(r.offset, 300.0);
    assert_eq!(ui.scroll_offset(id), 300.0);
    ui.reset_scroll(id);
    let (r, _) = frame(&mut ui, input_at(0.016), |ui| draw_scroll(ui, id, rect));
    assert_eq!(r.offset, 0.0);
}

#[test]
fn scroll_content_rect_is_offset_and_stable_gutter_reserves_width() {
    let mut ui = Ui::new();
    let id = Id::from_str("scroll-content-rect");
    let rect = Rect::new(10.0, 20.0, 400.0, 200.0);
    let (r, _) = frame(&mut ui, input_at(0.0), |ui| {
        let (_, response) = ui.scroll_area_stable(id, rect, 1000.0, |_ui, _content| {});
        response
    });
    assert_eq!(r.content_rect.x, 10.0);
    assert_eq!(r.content_rect.w, 400.0 - SCROLLBAR_WIDTH);
    assert_eq!(r.content_rect.y, 20.0);
    ui.set_scroll_offset(id, 50.0);
    let (r, _) = frame(&mut ui, input_at(0.016), |ui| {
        let (_, response) = ui.scroll_area_stable(id, rect, 1000.0, |_ui, _content| {});
        response
    });
    assert_eq!(r.content_rect.y, 20.0 - 50.0);
}

/// A focused scrollbar track receiving Enter/Space must not jump to the parked
/// pointer: the synthetic keyboard click is not a positional gesture.
#[test]
fn focused_scrollbar_track_ignores_keyboard_activation() {
    let mut ui = Ui::new();
    let id = Id::from_str("keyboard-scrollbar");
    let rect = Rect::new(0.0, 0.0, 400.0, 200.0);
    let track = id.with("track");
    ui.request_focus(track);
    // Pointer near the bottom of the bar: the pointer path would scroll to the
    // end (t = 1 → offset = max_offset).
    let mut i = mouse(input_at(0.0), 395.0, 190.0);
    i.keys = vec![key_ev(Key::Space, false)];
    let (r, _) = frame(&mut ui, i, |ui| {
        let (_, response) = ui.scroll_area(id, rect, 1000.0, |_ui, _content| {});
        response
    });
    assert_eq!(r.offset, 0.0, "keyboard activation must not jump");
    assert!(!r.scrolled);
    assert_eq!(ui.scroll_offset(id), 0.0);
    assert!(ui.is_focused(track), "test setup: the track has focus");
}

// ---------------------------------------------------------------------------
// popups / toasts / tooltips
// ---------------------------------------------------------------------------

#[test]
fn popup_draws_deferred_and_click_outside_closes_it() {
    let mut ui = Ui::new();
    let id = Id::from_str("menu");
    let anchor = Rect::new(10.0, 10.0, 28.0, 28.0);
    ui.open_popup(id);
    let (_, paint) = frame(&mut ui, mouse(input_at(0.0), 100.0, 100.0), |ui| {
        ui.popup(
            id,
            anchor,
            Size::new(MENU_MIN_CONTENT_WIDTH, MENU_ITEM_HEIGHT),
            PopupAlign::Right,
            |ui, content| {
                ui.menu_item(
                    Rect::new(content.x, content.y, content.w, MENU_ITEM_HEIGHT),
                    "Play next",
                    Some(Icon::PlaylistAdd),
                    None,
                    false,
                );
            },
        )
    });
    assert!(prims(&paint) > 2, "popup frame + item should paint");
    assert!(ui.is_popup_open(id));
    // Click outside the popup and the anchor.
    frame(&mut ui, press(mouse(input_at(0.016), 900.0, 700.0)), |ui| {
        ui.popup(
            id,
            anchor,
            Size::new(MENU_MIN_CONTENT_WIDTH, MENU_ITEM_HEIGHT),
            PopupAlign::Right,
            |_, _| {},
        );
    });
    assert!(!ui.is_popup_open(id), "click outside must dismiss");
}

#[test]
fn escape_closes_an_open_popup() {
    let mut ui = Ui::new();
    let id = Id::from_str("menu-esc");
    let anchor = Rect::new(10.0, 10.0, 28.0, 28.0);
    ui.open_popup(id);
    let mut i = input_at(0.0);
    i.keys = vec![key_ev(Key::Escape, false)];
    let (_, _) = frame(&mut ui, i, |ui| {
        ui.popup(
            id,
            anchor,
            Size::new(MENU_MIN_CONTENT_WIDTH, MENU_ITEM_HEIGHT),
            PopupAlign::Left,
            |_, _| {},
        )
    });
    assert!(!ui.is_popup_open(id));
}

#[test]
fn open_popup_blocks_interaction_underneath_next_frame() {
    let mut ui = Ui::new();
    let popup_id = Id::from_str("blocking-menu");
    let anchor = Rect::new(10.0, 10.0, 28.0, 28.0);
    let row_id = Id::from_str("row-under-menu");
    let row = Rect::new(0.0, 0.0, 300.0, 300.0);
    ui.open_popup(popup_id);
    // Frame 1: popup paints and records its blocker.
    frame(&mut ui, mouse(input_at(0.0), 60.0, 60.0), |ui| {
        ui.popup(
            popup_id,
            anchor,
            Size::new(MENU_MIN_CONTENT_WIDTH, MENU_ITEM_HEIGHT),
            PopupAlign::Left,
            |_, _| {},
        );
    });
    // Frame 2: the row under the popup must not hover.
    let (r, _) = frame(&mut ui, mouse(input_at(0.016), 60.0, 60.0), |ui| {
        ui.popup(
            popup_id,
            anchor,
            Size::new(MENU_MIN_CONTENT_WIDTH, MENU_ITEM_HEIGHT),
            PopupAlign::Left,
            |_, _| {},
        );
        ui.interact(row, row_id, Sense::CLICK)
    });
    assert!(!r.hovered, "popup blocker should suppress hover underneath");
}

#[test]
fn toast_is_queued_drawn_and_expires() {
    let mut ui = Ui::new();
    frame(&mut ui, input_at(0.0), |ui| {
        ui.toast_info("downloaded");
    });
    let (_, paint) = frame(&mut ui, input_at(0.016), |_ui| {});
    assert!(prims(&paint) >= 1, "toast card should paint");
    // Past its 4s lifetime it disappears.
    let (_, paint) = frame(&mut ui, input_at(10.0), |_ui| {});
    assert_eq!(prims(&paint), 0);
}

#[test]
fn tooltip_appears_after_hover_delay() {
    let mut ui = Ui::new();
    let id = Id::from_str("tt");
    let anchor = Rect::new(100.0, 100.0, 30.0, 30.0);
    // No tooltip before the delay.
    let (_, paint) = frame(&mut ui, mouse(input_at(0.0), 110.0, 110.0), |ui| {
        ui.tooltip(id, anchor, true, "Tooltip");
    });
    assert_eq!(prims(&paint), 0);
    let (_, paint) = frame(&mut ui, mouse(input_at(0.6), 110.0, 110.0), |ui| {
        ui.tooltip(id, anchor, true, "Tooltip");
    });
    assert!(prims(&paint) > 0, "tooltip should show after the delay");
}

// ---------------------------------------------------------------------------
// text input
// ---------------------------------------------------------------------------

fn draw_input(ui: &mut Ui, id: Id, text: &mut String) -> TextInputResponse {
    let style = TextInputStyle::default_theme(&ui.theme().clone());
    ui.text_input(Rect::new(0.0, 0.0, 200.0, 30.0), id, text, "Name", &style)
}

#[test]
fn focused_text_input_accepts_typed_characters() {
    let mut ui = Ui::new();
    let id = Id::from_str("input");
    let mut text = String::new();
    ui.request_focus(id);
    let (r, _) = frame(&mut ui, typed(input_at(0.0), "Hello"), |ui| {
        draw_input(ui, id, &mut text)
    });
    assert_eq!(text, "Hello");
    assert!(r.changed);
    assert!(ui.text_input_active());
}

#[test]
fn text_input_backspace_and_enter() {
    let mut ui = Ui::new();
    let id = Id::from_str("input-edit");
    let mut text = String::from("Hey");
    ui.request_focus(id);
    // Put the caret at the end via End.
    let mut i = input_at(0.0);
    i.keys = vec![key_ev(Key::End, false)];
    frame(&mut ui, i, |ui| draw_input(ui, id, &mut text));
    let mut i = input_at(0.016);
    i.keys = vec![key_ev(Key::Backspace, false)];
    frame(&mut ui, i, |ui| draw_input(ui, id, &mut text));
    assert_eq!(text, "He");
    let mut i = input_at(0.032);
    i.keys = vec![key_ev(Key::Enter, false)];
    let (r, _) = frame(&mut ui, i, |ui| draw_input(ui, id, &mut text));
    assert!(r.submitted);
}

#[test]
fn text_input_escape_cancels_and_blurs() {
    let mut ui = Ui::new();
    let id = Id::from_str("input-esc");
    let mut text = String::from("abc");
    ui.request_focus(id);
    let mut i = input_at(0.0);
    i.keys = vec![key_ev(Key::Escape, false)];
    let (r, _) = frame(&mut ui, i, |ui| draw_input(ui, id, &mut text));
    assert!(r.cancelled);
    assert_eq!(ui.focused_id(), None);
}

/// Gaining focus without a pointer click (autofocus, Tab) puts the caret at the
/// end, so typing appends to a prefilled draft like an HTML `autofocus` input.
#[test]
fn first_focus_puts_the_caret_at_the_end() {
    let mut ui = Ui::new();
    let id = Id::from_str("input-focus-caret");
    let mut text = String::from("Mix");
    ui.request_focus(id);
    let (r, _) = frame(&mut ui, typed(input_at(0.0), " 2"), |ui| {
        draw_input(ui, id, &mut text)
    });
    assert!(r.changed);
    assert_eq!(text, "Mix 2");
}

/// A focused field is stable: it does not report a spurious `lost_focus`.
#[test]
fn focused_text_input_does_not_report_lost_focus() {
    let mut ui = Ui::new();
    let id = Id::from_str("input-lost-none");
    let mut text = String::from("abc");
    ui.request_focus(id);
    for step in 0..3 {
        let (r, _) = frame(&mut ui, input_at(step as f64 / 60.0), |ui| {
            draw_input(ui, id, &mut text)
        });
        assert!(r.response.focused, "the field keeps focus");
        assert!(!r.lost_focus, "no blur happened, so no lost_focus");
    }
}

/// Clicking empty space blurs the field in `Ui::end_frame`, after the field has
/// already run, so `lost_focus` is reported by the *next* frame's call.
#[test]
fn text_input_reports_lost_focus_one_frame_after_clicking_away() {
    let mut ui = Ui::new();
    let id = Id::from_str("input-lost-away");
    let mut text = String::from("abc");
    ui.request_focus(id);
    let (r, _) = frame(&mut ui, input_at(0.0), |ui| draw_input(ui, id, &mut text));
    assert!(r.response.focused);
    assert!(!r.lost_focus);

    // Press on empty space: nothing takes focus, so `end_frame` blurs the field.
    let (r, _) = frame(&mut ui, press(mouse(input_at(0.1), 600.0, 600.0)), |ui| {
        draw_input(ui, id, &mut text)
    });
    assert!(
        r.response.focused,
        "the blur is committed after the field ran"
    );
    assert!(!r.lost_focus, "the field cannot see end_frame's blur yet");

    // The following frame reports the blur (and the release frame is nothing
    // special: the record is consumed as soon as the field is drawn).
    let (r, _) = frame(&mut ui, release(mouse(input_at(0.2), 600.0, 600.0)), |ui| {
        draw_input(ui, id, &mut text)
    });
    assert!(!r.response.focused);
    assert!(
        r.lost_focus,
        "lost_focus must be reported on the frame after the click-away blur"
    );
}

/// Focus taken by a widget drawn *after* the field is also only visible in the
/// next frame. Re-focusing the field in that frame suppresses the report, so a
/// quick click back into it is not treated as a cancel.
#[test]
fn text_input_reports_lost_focus_when_another_widget_takes_focus() {
    let mut ui = Ui::new();
    let id = Id::from_str("input-lost-other");
    let other = Id::from_str("input-lost-other-button");
    let other_rect = Rect::new(480.0, 280.0, 100.0, 40.0);
    let mut text = String::from("abc");
    ui.request_focus(id);

    let draw_both = |ui: &mut Ui, text: &mut String| {
        let r = draw_input(ui, id, text);
        let _ = ui.interact(other_rect, other, Sense::CLICK);
        r
    };
    let (r, _) = frame(&mut ui, input_at(0.0), |ui| draw_both(ui, &mut text));
    assert!(r.response.focused);

    // The pointer presses the other widget: its `interact` takes focus during
    // this frame, after the text field already ran.
    let at = (other_rect.center().0, other_rect.center().1);
    let (r, _) = frame(&mut ui, press(mouse(input_at(0.1), at.0, at.1)), |ui| {
        draw_both(ui, &mut text)
    });
    assert!(r.response.focused, "the field still believes it is focused");
    assert!(!r.lost_focus);

    let (r, _) = frame(&mut ui, release(mouse(input_at(0.2), at.0, at.1)), |ui| {
        draw_both(ui, &mut text)
    });
    assert!(!r.response.focused);
    assert!(r.lost_focus, "the next frame reports the focus loss");
}

/// Clicking back into a blurred field in the frame that would report the loss
/// re-focuses it first, so a fast blur + refocus is not a cancel.
#[test]
fn clicking_back_into_a_blurred_field_suppresses_lost_focus() {
    let mut ui = Ui::new();
    let id = Id::from_str("input-lost-refocus");
    let mut text = String::from("abc");
    ui.request_focus(id);
    frame(&mut ui, input_at(0.0), |ui| draw_input(ui, id, &mut text));

    // Blur by pressing empty space; `end_frame` records the loss.
    frame(&mut ui, press(mouse(input_at(0.1), 600.0, 600.0)), |ui| {
        draw_input(ui, id, &mut text)
    });

    // Press back into the field on the next frame: `interact` re-focuses before
    // `lost_focus` is computed, so the record is suppressed.
    let (r, _) = frame(&mut ui, press(mouse(input_at(0.2), 20.0, 10.0)), |ui| {
        draw_input(ui, id, &mut text)
    });
    assert!(r.response.focused, "the click focused the field again");
    assert!(
        !r.lost_focus,
        "a refocused field must not report the previous blur"
    );
}

// ---------------------------------------------------------------------------
// styles / theme
// ---------------------------------------------------------------------------

#[test]
fn button_styles_use_theme_tokens() {
    let theme = Theme::default();
    let primary = ButtonStyle::for_kind(ButtonKind::Primary, &theme);
    assert_eq!(primary.bg, theme.accent);
    assert_eq!(primary.fg, Color::WHITE);
    let secondary = ButtonStyle::for_kind(ButtonKind::Secondary, &theme);
    assert_eq!(secondary.border, theme.border);
    let tab = ButtonStyle::for_kind(ButtonKind::Tab { active: true }, &theme);
    assert_eq!(tab.bg, theme.accent);
}

#[test]
fn id_derivation_is_stable_across_frames() {
    let mut ui = Ui::new();
    let rect = Rect::new(10.0, 10.0, 100.0, 30.0);
    let (a, _) = frame(&mut ui, input_at(0.0), |ui| {
        ui.button(rect, "Same", ButtonKind::Primary).id
    });
    let (b, _) = frame(&mut ui, input_at(0.016), |ui| {
        ui.button(rect, "Same", ButtonKind::Primary).id
    });
    assert_eq!(a, b);
}

#[test]
fn cover_widget_uses_fallback_glyph_without_texture() {
    let mut ui = Ui::new();
    let (_, paint) = frame(&mut ui, input_at(0.0), |ui| {
        ui.cover(Rect::new(0.0, 0.0, 160.0, 160.0), None, 6.0);
    });
    assert_eq!(prims(&paint), 2, "fallback background + music note");
}

#[test]
fn texture_lifecycle_is_forwarded_to_the_paint_list() {
    let mut ui = Ui::new();
    let tex: TextureId = FIRST_IMAGE_TEXTURE_ID;
    let (_, paint) = frame(&mut ui, input_at(0.0), |ui| {
        ui.upload_texture(tex, 2, 2, vec![255; 16]);
        ui.image(Rect::new(0.0, 0.0, 10.0, 10.0), tex, Color::WHITE, 0.0);
        ui.drop_texture(tex);
    });
    let created = paint
        .created
        .iter()
        .find(|t| t.id == tex)
        .expect("image texture created");
    assert_eq!(created.width, 2);
    assert_eq!(paint.dropped, vec![tex]);
}

// ---------------------------------------------------------------------------
// texture lifecycle across the inter-frame upload window
// ---------------------------------------------------------------------------

#[test]
fn inter_frame_texture_upload_reaches_the_next_paint_list() {
    // app.rs uploads cover-art textures BETWEEN frames (drained controller
    // events) before begin_frame. They must not be discarded by begin_frame.
    let mut ui = Ui::new();
    ui.upload_texture(4242, 1, 1, vec![255, 255, 255, 255]);
    frame(&mut ui, input_at(0.0), |_ui| {});
    // Re-do through the raw API so we can inspect the exact paint list.
    let mut ui = Ui::new();
    ui.upload_texture(4242, 1, 1, vec![255; 4]);
    ui.begin_frame(input_at(0.0), viewport());
    let paint = ui.end_frame();
    assert!(
        paint.created.iter().any(|t| t.id == 4242),
        "inter-frame upload lost: {:?}",
        paint.created
    );
}

/// The OS IME candidate window needs the focused field's caret rectangle. It
/// must be reported while a text input is focused (even with the caret blink
/// off) and cleared once focus leaves the field.
#[test]
fn the_focused_text_input_reports_its_ime_caret_area() {
    let mut ui = Ui::new();
    let id = Id::from_str("ime/input");
    let rect = Rect::new(20.0, 30.0, 200.0, 33.0);
    let style = TextInputStyle::default_theme(&ui.theme().clone());
    let mut text = String::from("hello");
    ui.request_focus(id);
    let (_, _) = frame(&mut ui, input_at(0.0), |ui| {
        ui.text_input(rect, id, &mut text, "Placeholder", &style);
    });
    let area = ui
        .ime_cursor_area()
        .expect("a focused text input must report its caret");
    assert!(
        area.x >= rect.x - 1.0 && area.x <= rect.right() + 1.0,
        "the caret anchor must be inside the field: {area:?} vs {rect:?}"
    );
    assert!(area.y >= rect.y && area.y <= rect.bottom());
    assert!(area.h > 0.0 && area.w > 0.0);

    // Blur: the next frame has no focused input, so there is nothing to anchor.
    ui.clear_focus();
    let (_, _) = frame(&mut ui, input_at(0.1), |ui| {
        ui.text_input(rect, id, &mut text, "Placeholder", &style);
    });
    assert_eq!(ui.ime_cursor_area(), None);
}

#[test]
fn in_frame_texture_upload_appears_exactly_once() {
    let mut ui = Ui::new();
    let (_, paint) = frame(&mut ui, input_at(0.0), |ui| {
        ui.upload_texture(9001, 2, 2, vec![7; 16]);
        ui.update_texture(9002, 2, 2, vec![8; 16]);
    });
    assert_eq!(paint.created.iter().filter(|t| t.id == 9001).count(), 1);
    assert_eq!(
        paint
            .updated
            .iter()
            .filter(|t| matches!(t, TextureUpdate::Whole(upload) if upload.id == 9002))
            .count(),
        1
    );
    // The next frame must not re-send them.
    let (_, paint) = frame(&mut ui, input_at(0.016), |_ui| {});
    assert!(paint.created.is_empty());
    assert!(paint.updated.is_empty());
}

#[test]
fn end_frame_without_begin_frame_is_tolerated_and_flushes_uploads() {
    let mut ui = Ui::new();
    // A never-begun UI: screens' tests do this to drain an empty frame.
    let paint = ui.end_frame();
    assert!(paint.created.is_empty() && paint.updated.is_empty());
    assert_eq!(paint.clear, ui.theme().bg);
    // Uploads queued while no frame is running are still flushed.
    ui.upload_texture(77, 1, 1, vec![1, 2, 3, 4]);
    let paint = ui.end_frame();
    assert!(paint.created.iter().any(|t| t.id == 77));
}

#[test]
fn dropped_textures_survive_into_the_next_frame() {
    let mut ui = Ui::new();
    ui.drop_texture(FIRST_IMAGE_TEXTURE_ID + 3);
    ui.begin_frame(input_at(0.0), viewport());
    let paint = ui.end_frame();
    assert_eq!(paint.dropped, vec![FIRST_IMAGE_TEXTURE_ID + 3]);
}

// ---------------------------------------------------------------- cursors

#[test]
fn clickable_hover_requests_the_pointer_cursor() {
    let mut ui = Ui::new();
    ui.begin_frame(mouse(input_at(0.0), 20.0, 20.0), viewport());
    let rect = Rect::new(10.0, 10.0, 100.0, 30.0);
    let r = ui.interact(rect, Id::from_str("cursor/click"), Sense::CLICK);
    assert!(r.hovered, "test setup: pointer is over the widget");
    assert_eq!(
        ui.cursor_shape(),
        CursorShape::Pointer,
        "a hovered clickable widget must show the normal pointer"
    );
    let _ = ui.end_frame();
}

#[test]
fn hover_only_and_disabled_widgets_keep_the_default_cursor() {
    let mut ui = Ui::new();
    ui.begin_frame(mouse(input_at(0.0), 20.0, 20.0), viewport());
    let rect = Rect::new(10.0, 10.0, 100.0, 30.0);
    let hover = ui.interact(rect, Id::from_str("cursor/hover"), Sense::HOVER);
    assert!(hover.hovered);
    assert_eq!(
        ui.cursor_shape(),
        CursorShape::Default,
        "hover-only regions keep the arrow"
    );
    let none = ui.interact(rect, Id::from_str("cursor/none"), Sense::NONE);
    assert!(none.hovered);
    assert_eq!(
        ui.cursor_shape(),
        CursorShape::Default,
        "inert regions keep the arrow"
    );
    let _ = ui.end_frame();

    // The real disabled-widget path (`button_enabled` passes `Sense::NONE`).
    let mut ui = Ui::new();
    ui.begin_frame(mouse(input_at(0.0), 20.0, 20.0), viewport());
    let resp = ui.button_enabled(rect, "Save", ButtonKind::Primary, false);
    assert!(
        resp.hovered,
        "test setup: pointer is over the disabled button"
    );
    assert_eq!(
        ui.cursor_shape(),
        CursorShape::Default,
        "a disabled button must not show the hand"
    );
    let _ = ui.end_frame();
}

#[test]
fn explicit_cursor_requests_win_over_the_automatic_pointer() {
    let mut ui = Ui::new();
    ui.begin_frame(mouse(input_at(0.0), 20.0, 20.0), viewport());
    let rect = Rect::new(10.0, 10.0, 100.0, 30.0);
    let r = ui.interact(rect, Id::from_str("cursor/explicit"), Sense::CLICK);
    assert!(r.hovered);
    assert_eq!(ui.cursor_shape(), CursorShape::Pointer);
    // Widgets override the default after interacting (text field -> Text,
    // resizer -> ResizeH, held drag -> Grabbing): last writer wins.
    ui.set_cursor(CursorShape::Text);
    assert_eq!(
        ui.cursor_shape(),
        CursorShape::Text,
        "set_cursor after interact must win"
    );
    let _ = ui.end_frame();

    // ...and the ordering works the other way too: an interact after an explicit
    // cursor writes the pointer (last writer wins), which is how a clickable
    // widget hovered late in the frame recovers the default hand.
    let mut ui = Ui::new();
    ui.begin_frame(mouse(input_at(0.0), 20.0, 20.0), viewport());
    ui.set_cursor(CursorShape::ResizeH);
    let r = ui.interact(
        Rect::new(10.0, 10.0, 100.0, 30.0),
        Id::from_str("cursor/later"),
        Sense::CLICK,
    );
    assert!(r.hovered);
    assert_eq!(
        ui.cursor_shape(),
        CursorShape::Pointer,
        "a later clickable hover wins by last-writer-wins"
    );
    let _ = ui.end_frame();
}

#[test]
fn cursor_resets_to_default_on_every_frame() {
    let mut ui = Ui::new();
    let rect = Rect::new(10.0, 10.0, 100.0, 30.0);
    let id = Id::from_str("cursor/reset");
    ui.begin_frame(mouse(input_at(0.0), 20.0, 20.0), viewport());
    ui.interact(rect, id, Sense::CLICK);
    assert_eq!(ui.cursor_shape(), CursorShape::Pointer);
    let _ = ui.end_frame();

    // begin_frame resets the cursor before any widget has drawn this frame.
    ui.begin_frame(mouse(input_at(0.016), 900.0, 500.0), viewport());
    assert_eq!(
        ui.cursor_shape(),
        CursorShape::Default,
        "the cursor must reset to Default at the start of each frame"
    );
    let _ = ui.end_frame();
}

#[test]
fn focus_loss_clears_the_active_latch_so_the_next_click_works() {
    let mut ui = Ui::new();
    let rect = Rect::new(10.0, 10.0, 100.0, 30.0);
    let id = Id::from_str("focus/latch");

    // Frame 1: a press latches the widget active (as before an alt-tab).
    let (r, _) = frame(&mut ui, press(mouse(input_at(0.0), 20.0, 20.0)), |ui| {
        ui.interact(rect, id, Sense::CLICK)
    });
    assert!(r.pressed && r.active, "press must latch the widget");
    assert!(!r.clicked);

    // Frame 2: the window lost focus at some point; the release edge never
    // arrives. `begin_frame` must drop the stale latch.
    let mut lost = mouse(input_at(0.016), 20.0, 20.0);
    lost.focus_lost = true;
    let (r, _) = frame(&mut ui, lost, |ui| ui.interact(rect, id, Sense::CLICK));
    assert!(!r.active, "focus loss must clear the stale active latch");
    assert!(!r.clicked && !r.pressed, "no fake release/click frame");

    // Frame 3: the first real click after refocusing is registered normally.
    let (r, _) = frame(&mut ui, press(mouse(input_at(0.032), 20.0, 20.0)), |ui| {
        ui.interact(rect, id, Sense::CLICK)
    });
    assert!(
        r.pressed,
        "the first post-refocus press must not be swallowed by the old latch"
    );
}

#[test]
fn single_line_text_with_a_newline_still_ellipsizes_at_the_first_line() {
    // `ui.text` is the single-line label path: a hard newline must not draw the
    // second line. With the wide rect the first line fits, so the result is the
    // first line plus the ellipsis ("first…" = 6 glyphs), never "second".
    let mut ui = Ui::new();
    let (_, paint) = frame(&mut ui, input_at(0.0), |ui| {
        let style = body_style(ui);
        ui.text(Rect::new(0.0, 0.0, 300.0, 20.0), "first\nsecond", &style);
    });
    assert_eq!(prims(&paint), 6, "first line + ellipsis glyphs");
}
