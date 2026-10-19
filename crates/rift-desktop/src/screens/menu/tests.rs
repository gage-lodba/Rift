//! Menu behaviour tests: item/entry builders, submenu cap, and scripted
//! open/click/outside/Escape sequences through the real layer.

use super::lock_for_tests as lock;
use super::*;

use crate::screens::cards;
use crate::state::Action;
use crate::text::Weight;
use crate::ui::{Icon, Id, Rect, Ui};

#[test]
fn item_builders_set_state() {
    let plain = Item::new("Play next", Some(Icon::SkipNext));
    assert!(plain.enabled);
    assert!(!plain.danger);
    let danger = Item::new("Delete", Some(Icon::Delete)).danger();
    assert!(danger.danger);
    let inert = Item::new("Downloading…", None).disabled();
    assert!(!inert.enabled);
    let shortcut = Item::new("Play", None).shortcut("Space");
    assert_eq!(shortcut.shortcut.as_deref(), Some("Space"));
}

#[test]
fn submenu_visible_rows_cap_at_200px() {
    assert_eq!(SUBMENU_MAX_VISIBLE, 6);
    assert!(SUBMENU_MAX_VISIBLE as f32 * MENU_ITEM_HEIGHT <= 200.0);
}

#[test]
fn entry_helpers_keep_the_action() {
    let entry = Entry::item(
        Item::new("Go to artist", None),
        Action::OpenArtist("a".into()),
    );
    match entry {
        Entry::Item { item, action } => {
            assert!(item.enabled);
            assert!(matches!(action, Some(Action::OpenArtist(id)) if id == "a"));
        }
        _ => panic!("expected item"),
    }
    match Entry::inert(Item::new("Downloading…", None)) {
        Entry::Item { item, action } => {
            assert!(!item.enabled);
            assert!(action.is_none());
        }
        _ => panic!("expected inert item"),
    }
}

fn frame(ui: &mut Ui, time: f64, mouse: (f32, f32), pressed: bool, released: bool) {
    let viewport = Rect::new(0.0, 0.0, 1160.0, 740.0);
    let input = crate::ui::InputState {
        viewport,
        mouse_pos: mouse,
        mouse_in_window: true,
        mouse_left_pressed: pressed,
        mouse_left_down: pressed,
        mouse_left_released: released,
        time,
        ..Default::default()
    };
    ui.begin_frame(input, viewport);
}

/// A click on a menu item pushes exactly the entry's action and closes.
#[test]
fn one_item_click_pushes_exactly_one_action() {
    let _guard = lock();
    let mut ui = Ui::new();
    let viewport = Rect::new(0.0, 0.0, 1160.0, 740.0);
    let anchor = Rect::new(100.0, 100.0, 28.0, 28.0);
    let label = "Go to artist";
    let id = Id::from_str("test-menu");
    let entries = vec![Entry::item(
        Item::new(label, Some(Icon::SkipNext)),
        Action::OpenArtist("artist-1".into()),
    )];

    // Frame 1: open the menu (records entries, no paint yet).
    frame(&mut ui, 1.0, (0.0, 0.0), false, false);
    open(&mut ui, id, anchor, entries);
    let mut actions = Vec::new();
    assert!(layer(&mut ui, &mut actions), "menu layer draws while open");
    let _ = ui.end_frame();
    assert!(actions.is_empty());

    // Derive the menu item's rect from the popup placement rules.
    let content_w = {
        let theme = ui.theme().clone();
        let style = cards::style(13.0, Weight::Regular, theme.text);
        MENU_MIN_CONTENT_WIDTH.max(10.0 + 26.0 + ui.text_width(label, &style) + 10.0)
    };
    let outer_w = (content_w + 10.0).max(80.0);
    let outer_h = MENU_ITEM_HEIGHT + 10.0;
    let mut x = anchor.right() - outer_w;
    x = x.clamp(8.0, (viewport.right() - outer_w - 8.0).max(8.0));
    let menu_rect = Rect::new(x, anchor.bottom() + 4.0, outer_w, outer_h);
    let item_center = (
        menu_rect.x + 5.0 + content_w * 0.5,
        menu_rect.y + 5.0 + MENU_ITEM_HEIGHT * 0.5,
    );

    // Frame 2: press on the item.
    frame(&mut ui, 2.0, item_center, true, false);
    let mut actions = Vec::new();
    assert!(layer(&mut ui, &mut actions));
    let _ = ui.end_frame();
    assert!(actions.is_empty(), "press alone must not activate");

    // Frame 3: release on the item → exactly one action, menu closes.
    frame(&mut ui, 3.0, item_center, false, true);
    let mut actions = Vec::new();
    let _ = layer(&mut ui, &mut actions);
    let _ = ui.end_frame();
    assert_eq!(actions.len(), 1, "expected exactly one action");
    assert!(matches!(&actions[0], Action::OpenArtist(id) if id == "artist-1"));
    assert!(open_id().is_none(), "menu closes after activating an item");
    assert!(is_open(), "the rest of the activating frame stays consumed");

    // Frame 4: nothing open any more.
    frame(&mut ui, 4.0, item_center, false, false);
    let mut actions = Vec::new();
    assert!(!layer(&mut ui, &mut actions));
    let _ = ui.end_frame();
    assert!(actions.is_empty());
    assert!(!is_open());
}

/// Escape closes the open menu without dispatching anything.
#[test]
fn escape_closes_the_menu_without_emitting() {
    let _guard = lock();
    let mut ui = Ui::new();
    let viewport = Rect::new(0.0, 0.0, 1160.0, 740.0);
    let id = Id::from_str("test-menu-escape");
    frame(&mut ui, 1.0, (0.0, 0.0), false, false);
    open(
        &mut ui,
        id,
        Rect::new(100.0, 100.0, 28.0, 28.0),
        vec![Entry::inert(Item::new("Inert", None))],
    );
    let mut actions = Vec::new();
    let _ = layer(&mut ui, &mut actions);
    let _ = ui.end_frame();

    // Frame with Escape.
    ui.begin_frame(
        crate::ui::InputState {
            viewport,
            time: 2.0,
            keys: vec![crate::ui::KeyInput {
                key: crate::ui::Key::Escape,
                pressed: true,
                repeat: false,
                modifiers: crate::ui::Modifiers::default(),
            }],
            ..Default::default()
        },
        viewport,
    );
    let mut actions = Vec::new();
    assert!(!layer(&mut ui, &mut actions), "menu closed by Escape");
    let _ = ui.end_frame();
    assert!(actions.is_empty());
    assert!(!is_open());
}

/// A press outside the menu closes it and is marked as consumed for the
/// frame, so content drawn later does not also act on the click.
#[test]
fn outside_press_closes_and_consumes_the_frame() {
    let _guard = lock();
    let mut ui = Ui::new();
    let anchor = Rect::new(300.0, 300.0, 28.0, 28.0);
    let id = Id::from_str("test-menu-outside");
    frame(&mut ui, 1.0, (0.0, 0.0), false, false);
    open(
        &mut ui,
        id,
        anchor,
        vec![Entry::inert(Item::new("Inert", None))],
    );
    let mut actions = Vec::new();
    let _ = layer(&mut ui, &mut actions);
    let _ = ui.end_frame();
    assert!(is_open());

    // Press far away from the popup.
    frame(&mut ui, 2.0, (900.0, 200.0), true, false);
    let mut actions = Vec::new();
    let _ = layer(&mut ui, &mut actions);
    assert!(open_id().is_none(), "outside press closed the menu");
    // Content drawn later this frame must see the click as consumed.
    assert!(is_open(), "is_open stays true for the rest of the frame");
    let _ = ui.end_frame();
    assert!(actions.is_empty());

    // Next frame: the layer resets the flag and reports nothing open.
    frame(&mut ui, 3.0, (0.0, 0.0), false, false);
    assert!(!layer(&mut ui, &mut Vec::new()));
    let _ = ui.end_frame();
    assert!(!is_open());
}

#[test]
fn opening_and_closing_tracks_state() {
    let _guard = lock();
    let mut ui = Ui::new();
    ui.begin_frame(
        crate::ui::InputState {
            viewport: Rect::new(0.0, 0.0, 1160.0, 740.0),
            ..Default::default()
        },
        Rect::new(0.0, 0.0, 1160.0, 740.0),
    );
    assert!(!is_open());
    open(
        &mut ui,
        Id::from_str("test-menu"),
        Rect::new(10.0, 10.0, 28.0, 28.0),
        vec![Entry::inert(Item::new("x", None))],
    );
    assert!(is_open());
    assert_eq!(open_id(), Some(Id::from_str("test-menu")));
    close(&mut ui);
    assert!(!is_open());
    let _ = ui.end_frame();
}

// ---------------------------------------------------------------- cursors

use crate::ui::{CursorShape, Sense};

/// Placement `Ui::popup` computes for a one-row menu anchored at `anchor`.
/// Mirrors the framework's anchor/clamp rules for the fixed 1160x740 viewport.
fn single_item_menu_rect(ui: &mut Ui, anchor: Rect, label: &str, has_icon: bool) -> Rect {
    let theme = ui.theme().clone();
    let style = cards::style(13.0, Weight::Regular, theme.text);
    let icon_w = if has_icon { 26.0 } else { 0.0 };
    let content_w = MENU_MIN_CONTENT_WIDTH.max(10.0 + icon_w + ui.text_width(label, &style) + 10.0);
    let outer_w = (content_w + 10.0).max(80.0);
    let outer_h = MENU_ITEM_HEIGHT + 10.0;
    let viewport = Rect::new(0.0, 0.0, 1160.0, 740.0);
    let x = (anchor.right() - outer_w).clamp(8.0, (viewport.right() - outer_w - 8.0).max(8.0));
    let y = anchor.bottom() + 4.0;
    Rect::new(x, y, outer_w, outer_h)
}

fn item_center(menu: Rect) -> (f32, f32) {
    (
        menu.x + 5.0 + (menu.w - 10.0) * 0.5,
        menu.y + 5.0 + MENU_ITEM_HEIGHT * 0.5,
    )
}

/// The menu card keeps the hand, but the inert area outside it (the menu's
/// "backdrop") requests the arrow even when clickable content under the pointer
/// raised it. This uses the **production ordering**: the root draws the menu
/// layer first (to consume the dismissing click), then the content, then pins
/// the backdrop cursor last.
#[test]
fn open_menu_backdrop_keeps_the_arrow_while_rows_show_the_hand() {
    let _guard = lock();
    let mut ui = Ui::new();
    let anchor = Rect::new(300.0, 100.0, 28.0, 28.0);
    let label = "Go to artist";
    let id = Id::from_str("test-menu-cursor-backdrop");
    frame(&mut ui, 1.0, (0.0, 0.0), false, false);
    open(
        &mut ui,
        id,
        anchor,
        vec![Entry::item(
            Item::new(label, Some(Icon::SkipNext)),
            Action::OpenArtist("artist-1".into()),
        )],
    );
    let mut actions = Vec::new();
    let _ = layer(&mut ui, &mut actions);
    let _ = ui.end_frame();
    assert!(is_open());
    let menu = single_item_menu_rect(&mut ui, anchor, label, true);

    // Production ordering: menu layer -> content -> pin. The content is a
    // clickable surface under the pointer (as a titlebar/player button would be)
    // drawn *after* the layer, so its auto-pointer is raised last...
    frame(&mut ui, 2.0, (900.0, 600.0), false, false);
    let mut actions = Vec::new();
    let _ = layer(&mut ui, &mut actions);
    let clickable = Rect::new(880.0, 580.0, 120.0, 40.0);
    let hovered = ui.interact(
        clickable,
        Id::from_str("cursor/backdrop-content"),
        Sense::CLICK,
    );
    assert!(hovered.hovered);
    assert_eq!(
        ui.cursor_shape(),
        CursorShape::Pointer,
        "test setup: content after the layer raised the hand"
    );
    // ...and the post-content pin must win it back for the inert backdrop.
    pin_backdrop_cursor(&mut ui);
    assert_eq!(
        ui.cursor_shape(),
        CursorShape::Default,
        "the menu backdrop shows the arrow, not the hand"
    );
    let _ = ui.end_frame();

    // On the actual row the hand is back (rows keep the auto-pointer, and the
    // pin only touches the area outside the card).
    frame(&mut ui, 3.0, item_center(menu), false, false);
    let mut actions = Vec::new();
    let _ = layer(&mut ui, &mut actions);
    pin_backdrop_cursor(&mut ui);
    assert_eq!(
        ui.cursor_shape(),
        CursorShape::Pointer,
        "menu rows keep the normal hand"
    );
    let _ = ui.end_frame();
}

/// With no menu open, the post-content pin is a no-op: hovered clickable content
/// keeps the hand.
#[test]
fn backdrop_pin_does_nothing_when_no_menu_is_open() {
    let _guard = lock();
    let mut ui = Ui::new();
    frame(&mut ui, 1.0, (900.0, 600.0), false, false);
    let clickable = Rect::new(880.0, 580.0, 120.0, 40.0);
    let hovered = ui.interact(
        clickable,
        Id::from_str("cursor/no-menu-content"),
        Sense::CLICK,
    );
    assert!(hovered.hovered);
    pin_backdrop_cursor(&mut ui);
    assert_eq!(
        ui.cursor_shape(),
        CursorShape::Pointer,
        "without an open menu the pin must not force the arrow"
    );
    let _ = ui.end_frame();
}

/// A disabled/inert menu row does not act, so it shows the arrow instead of the
/// hand (the enabled-row pointer is covered by the backdrop test above).
#[test]
fn disabled_menu_rows_keep_the_arrow() {
    let _guard = lock();
    let mut ui = Ui::new();
    let anchor = Rect::new(300.0, 100.0, 28.0, 28.0);
    let label = "Downloading…";
    let id = Id::from_str("test-menu-cursor-disabled");
    frame(&mut ui, 1.0, (0.0, 0.0), false, false);
    open(
        &mut ui,
        id,
        anchor,
        vec![Entry::inert(Item::new(label, None))],
    );
    let mut actions = Vec::new();
    let _ = layer(&mut ui, &mut actions);
    let _ = ui.end_frame();
    assert!(is_open());
    let menu = single_item_menu_rect(&mut ui, anchor, label, false);

    frame(&mut ui, 2.0, item_center(menu), false, false);
    let mut actions = Vec::new();
    let _ = layer(&mut ui, &mut actions);
    assert_eq!(
        ui.cursor_shape(),
        CursorShape::Default,
        "a disabled menu row must not show the hand"
    );
    let _ = ui.end_frame();
}
