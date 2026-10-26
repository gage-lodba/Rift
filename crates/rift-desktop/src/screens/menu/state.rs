//! Retained open-menu state (thread-local), the public menu operations, the
//! painter/dispatcher and the test lock/reset hooks.
//!
//! Entries are built when the menu opens and retained here, so the layer needs
//! no screen context. A submenu ("Add to playlist ▸") expands in place
//! (`.menu-sub`, 36 px indent) and its state lives here too.

use std::cell::{Cell, RefCell};

use crate::screens::cards;
use crate::state::Action;
use crate::text::Weight;
use crate::ui::{Column, CursorShape, Icon, Id, Key, PopupAlign, Rect, Size, Ui};

use super::entries::{
    Entry, Item, MENU_ITEM_HEIGHT, MENU_MIN_CONTENT_WIDTH, MENU_SEPARATOR_HEIGHT, SUBMENU_INDENT,
    SUBMENU_MAX_VISIBLE,
};

#[derive(Clone, Debug)]
struct OpenMenu {
    id: Id,
    anchor: Rect,
    entries: Vec<Entry>,
    /// Index of the expanded submenu entry, if any.
    expanded: Option<usize>,
}

thread_local! {
    /// The open menu, per UI thread (the app has one UI thread; tests get one
    /// harness thread each, which keeps them isolated from each other).
    static OPEN: RefCell<Option<OpenMenu>> = const { RefCell::new(None) };
    /// True for the whole frame in which a menu was open (including the frame
    /// its backdrop closes it), so content drawn later in the same frame does
    /// not also act on that click.
    static CONSUMED: Cell<bool> = const { Cell::new(false) };
    /// `ui.time()` of the last frame [`layer`] ran, to detect the frame boundary.
    static LAST_FRAME: Cell<Option<f64>> = const { Cell::new(None) };
    /// (frame time, menu id) of the last menu drawn this frame. The root may
    /// call [`layer`] twice in one frame — once early (to consume the outside
    /// click) and once after the content (for a menu opened by that content this
    /// frame) — and the menu must only be painted/interacted once.
    static DRAWN_THIS_FRAME: Cell<Option<(f64, Id)>> = const { Cell::new(None) };
    /// Card rect of the menu drawn this frame, for [`pin_backdrop_cursor`].
    /// `None` when no menu was drawn this frame.
    static CARD: Cell<Option<Rect>> = const { Cell::new(None) };
}

/// Run `f` on the current UI thread's open-menu slot.
fn with_open<R>(f: impl FnOnce(&mut Option<OpenMenu>) -> R) -> R {
    OPEN.with(|open| f(&mut open.borrow_mut()))
}

fn consumed() -> bool {
    CONSUMED.with(|c| c.get())
}

fn set_consumed(value: bool) {
    CONSUMED.with(|c| c.set(value));
}

/// Open a menu anchored at `anchor`. The entries are retained until the menu
/// closes, so the layer can draw them without screen context.
pub fn open(ui: &mut Ui, id: Id, anchor: Rect, entries: Vec<Entry>) {
    with_open(|open| {
        *open = Some(OpenMenu {
            id,
            anchor,
            entries,
            expanded: None,
        });
    });
    ui.open_popup(id);
}

/// Close the open menu (and tell the framework).
pub fn close(ui: &mut Ui) {
    ui.close_popup();
    with_open(|open| *open = None);
}

/// Whether a menu is open right now (or was open when this frame's [`layer`]
/// ran). Lists whose rows act on clicks check this so the click that dismisses
/// the menu does not also activate a row.
pub fn is_open() -> bool {
    consumed() || with_open(|open| open.is_some())
}

/// The id of the open menu, if any.
pub fn open_id() -> Option<Id> {
    with_open(|open| open.as_ref().map(|m| m.id))
}

/// Draw and dispatch the open menu, if any. Call once per frame from the root,
/// outside any scroll clip. Returns whether a menu was open (~whether the frame's
/// click belonged to it).
pub fn layer(ui: &mut Ui, actions: &mut Vec<Action>) -> bool {
    // Frame boundary: reset the consumption flag once per frame.
    let frame = LAST_FRAME.with(|last| {
        if last.get() != Some(ui.time()) {
            last.set(Some(ui.time()));
            false
        } else {
            true
        }
    });
    if !frame {
        set_consumed(false);
    }
    let state = with_open(|open| open.clone());
    let Some(state) = state else {
        CARD.with(|card| card.set(None));
        return false;
    };
    // Escape / click-outside closed it in the framework; drop our copy too.
    if !ui.is_popup_open(state.id) {
        with_open(|open| *open = None);
        CARD.with(|card| card.set(None));
        return false;
    }
    // The menu owns this frame's clicks (rows check `is_open`).
    set_consumed(true);
    // Second `layer` call in the same frame for the same menu: it is already
    // painted and its items already interacted; do not double-paint.
    let already_drawn = DRAWN_THIS_FRAME.with(|drawn| {
        if drawn.get() == Some((ui.time(), state.id)) {
            true
        } else {
            drawn.set(Some((ui.time(), state.id)));
            false
        }
    });
    if already_drawn {
        return true;
    }

    let theme = ui.theme().clone();
    let label_style = cards::style(13.0, Weight::Regular, theme.text);
    let shortcut_style = cards::style(11.0, Weight::Regular, theme.text_dim);

    // ---- measure content ----
    let mut width: f32 = MENU_MIN_CONTENT_WIDTH;
    let mut height: f32 = 0.0;
    for entry in &state.entries {
        match entry {
            Entry::Item { item, .. } => {
                width = width.max(item_width(ui, item, &label_style, &shortcut_style));
                height += MENU_ITEM_HEIGHT;
            }
            Entry::Separator => height += MENU_SEPARATOR_HEIGHT,
            Entry::Sub {
                icon,
                label,
                options,
                ..
            } => {
                let icon_w = if icon.is_some() { 26.0 } else { 0.0 };
                width = width.max(
                    10.0 + icon_w
                        + ui.text_width(label, &label_style)
                        + 17.0 // caret
                        + 10.0,
                );
                height += MENU_ITEM_HEIGHT;
                for (option_label, _) in options {
                    width = width.max(
                        10.0 + SUBMENU_INDENT + ui.text_width(option_label, &label_style) + 10.0,
                    );
                }
            }
        }
    }
    // Add the expanded submenu's visible option rows.
    if let Some(expanded) = state.expanded {
        if let Some(Entry::Sub { options, .. }) = state.entries.get(expanded) {
            height += options.len().min(SUBMENU_MAX_VISIBLE) as f32 * MENU_ITEM_HEIGHT;
        }
    }

    let mut hit: Option<Action> = None;
    let mut toggle: Option<Option<usize>> = None;
    // The card rect, captured from the closure so the backdrop cursor override
    // below can tell "outside the menu" from "on a row".
    let mut card_rect: Option<Rect> = None;

    let result = ui.popup(
        state.id,
        state.anchor,
        Size::new(width, height),
        PopupAlign::Right,
        |ui, content| {
            card_rect = Some(content.expand(5.0));
            let mut col = Column::new(content, 0.0);
            for (idx, entry) in state.entries.iter().enumerate() {
                let entry_id = state.id.with(("entry", idx as u64));
                match entry {
                    Entry::Separator => {
                        let r = col.fixed(MENU_SEPARATOR_HEIGHT);
                        ui.menu_separator(r);
                    }
                    Entry::Item { item, action } => {
                        let r = col.fixed(MENU_ITEM_HEIGHT);
                        let resp = ui.menu_item_with_id(
                            r,
                            entry_id,
                            &item.label,
                            item.icon,
                            item.shortcut.as_deref(),
                            item.danger,
                        );
                        if resp.hovered && !item.enabled {
                            // Disabled rows are inert: the arrow, not the hand.
                            ui.set_cursor(CursorShape::Default);
                        }
                        if resp.clicked && item.enabled {
                            if let Some(action) = action {
                                hit = Some(action.clone());
                            }
                            ui.close_popup();
                        }
                    }
                    Entry::Sub {
                        icon,
                        label,
                        enabled,
                        options,
                    } => {
                        let r = col.fixed(MENU_ITEM_HEIGHT);
                        let expanded = state.expanded == Some(idx);
                        let resp = ui.menu_item_with_id(r, entry_id, label, *icon, None, false);
                        let caret_rect = Rect::new(
                            r.right() - 10.0 - 14.0,
                            r.y + (r.h - 14.0) * 0.5,
                            14.0,
                            14.0,
                        );
                        ui.icon(
                            caret_rect,
                            if expanded {
                                Icon::ExpandMore
                            } else {
                                Icon::ChevronRight
                            },
                            theme.text_dim,
                            14.0,
                        );
                        if resp.hovered && !*enabled {
                            // Disabled submenu rows are inert too.
                            ui.set_cursor(CursorShape::Default);
                        }
                        if resp.clicked && *enabled {
                            toggle = Some(if expanded { None } else { Some(idx) });
                        }
                        if expanded {
                            for (oi, (option_label, action)) in
                                options.iter().enumerate().take(SUBMENU_MAX_VISIBLE)
                            {
                                let r = col.fixed(MENU_ITEM_HEIGHT);
                                let resp = ui.menu_item_with_id(
                                    r,
                                    entry_id.with(oi as u64),
                                    option_label,
                                    None,
                                    None,
                                    false,
                                );
                                if resp.clicked {
                                    hit = Some(action.clone());
                                    ui.close_popup();
                                }
                            }
                        }
                    }
                }
            }
        },
    );

    // Remember the card for the post-content backdrop pin. The menu layer may
    // run before the rest of the content (the root draws it early to consume the
    // dismissing click), and content drawn later can still raise the pointer, so
    // the arrow override must run after everything else — see
    // [`pin_backdrop_cursor`].
    CARD.with(|card| card.set(if result.is_some() { card_rect } else { None }));

    // An item was activated: dispatch exactly one action and close.
    if let Some(action) = hit {
        actions.push(action);
        with_open(|open| *open = None);
        return true;
    }

    // The framework closes the popup itself on Escape / an outside click / an
    // item calling `close_popup`. Our retained copy has to follow it.
    if result.is_none() || !ui.is_popup_open(state.id) {
        with_open(|open| *open = None);
        // An outside click is swallowed for the rest of this frame (content
        // drawn later checks `is_open`); Escape leaves the frame alone.
        if ui.input().key_clicked(Key::Escape) {
            set_consumed(false);
        }
        return false;
    }

    if let Some(expanded) = toggle {
        with_open(|open| {
            if let Some(open) = open.as_mut() {
                open.expanded = expanded;
            }
        });
    }
    true
}

/// Pin the arrow over the open menu's inert backdrop.
///
/// Must be called from the root **after** all content and overlays: the menu
/// layer can run before the content (the root draws it early to consume the
/// dismissing click), and content drawn later can raise the pointer with its
/// automatic hand. This final pass makes the region outside the menu card an
/// arrow again, while rows inside the card keep the pointer (or `Grabbing` while
/// a drag is held).
///
/// The menu has no dimming scrim: its backdrop is the popup machinery's
/// click-outside dismissal, not an interactive surface. Pinning the cursor is
/// what keeps an open menu from turning the rest of the screen into a hand.
pub fn pin_backdrop_cursor(ui: &mut Ui) {
    let Some(card) = CARD.with(|card| card.get()) else {
        return;
    };
    let Some(id) = with_open(|open| open.as_ref().map(|menu| menu.id)) else {
        return;
    };
    // The framework may have closed the popup on this frame's outside click;
    // then it no longer owns the pointer.
    if !ui.is_popup_open(id) {
        return;
    }
    let pos = ui.input().mouse_pos;
    if !card.contains(pos.0, pos.1) {
        ui.set_cursor(CursorShape::Default);
    }
}

fn item_width(
    ui: &mut Ui,
    item: &Item,
    label_style: &crate::text::TextStyle,
    shortcut_style: &crate::text::TextStyle,
) -> f32 {
    let icon_w = if item.icon.is_some() { 26.0 } else { 0.0 };
    let shortcut_w = item
        .shortcut
        .as_ref()
        .map_or(0.0, |s| ui.text_width(s, shortcut_style));
    10.0 + icon_w + ui.text_width(&item.label, label_style) + shortcut_w + 10.0
}

/// Serializes the interaction tests across screen modules and resets the current
/// thread's menu state. The state itself is thread-local, so this is about not
/// interleaving synthetic frames within a thread, not about shared memory.
#[cfg(test)]
pub(crate) fn lock_for_tests() -> std::sync::MutexGuard<'static, ()> {
    static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    reset_for_tests();
    guard
}

#[cfg(test)]
pub(crate) fn reset_for_tests() {
    with_open(|open| *open = None);
    set_consumed(false);
    LAST_FRAME.with(|last| last.set(None));
    DRAWN_THIS_FRAME.with(|drawn| drawn.set(None));
    CARD.with(|card| card.set(None));
}
