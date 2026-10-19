//! Menu entry vocabulary: [`Item`] (label/icon/state) and [`Entry`] (item,
//! separator or submenu), plus the measured `.menu-*` metrics.

use crate::state::Action;
use crate::ui::Icon;

/// `.menu-item` row height (8px padding + 16px line + rounding).
pub const MENU_ITEM_HEIGHT: f32 = 32.0;
/// `.menu-sep` row: 1px line with 4px vertical margins.
pub const MENU_SEPARATOR_HEIGHT: f32 = 9.0;
/// `.menu` min-width 190 - the popup's 10px horizontal padding = 180.
pub const MENU_MIN_CONTENT_WIDTH: f32 = 180.0;
/// Left padding of an indented submenu option (`.menu-sub-option`).
pub const SUBMENU_INDENT: f32 = 36.0;
/// `.menu-sub` `max-height: 200px`; options beyond this many are not shown (a
/// scrolling submenu is not worth the complexity for the preview library).
pub const SUBMENU_MAX_VISIBLE: usize = (200.0 / MENU_ITEM_HEIGHT) as usize;

/// One menu entry's label/icon state, without its action.
#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub icon: Option<Icon>,
    pub label: String,
    /// `.menu-item.danger`: text and icon in the accent color.
    pub danger: bool,
    /// Inert status rows (spinner states, "Kept offline by a playlist") are
    /// drawn normally but ignore clicks — exactly like the old `Callback::noop`
    /// entries.
    pub enabled: bool,
    /// Right-aligned hint text (e.g. a keyboard shortcut). Usually `None`.
    pub shortcut: Option<String>,
}

impl Item {
    pub fn new(label: impl Into<String>, icon: Option<Icon>) -> Self {
        Self {
            icon,
            label: label.into(),
            danger: false,
            enabled: true,
            shortcut: None,
        }
    }

    pub fn danger(mut self) -> Self {
        self.danger = true;
        self
    }

    pub fn disabled(mut self) -> Self {
        self.enabled = false;
        self
    }

    pub fn shortcut(mut self, shortcut: impl Into<String>) -> Self {
        self.shortcut = Some(shortcut.into());
        self
    }
}

/// One row of a context menu.
///
/// `Entry` carries an owned [`Action`] (itself an enum with track lists); the
/// size difference between variants is not worth boxing because menus are built
/// a handful of times per frame and never stored in bulk.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug)]
pub enum Entry {
    /// A normal item; `action` is `None` for an inert status row.
    Item { item: Item, action: Option<Action> },
    /// `.menu-sep`.
    Separator,
    /// A submenu ("Add to playlist ▸"). Options are `(label, action)` pairs.
    Sub {
        icon: Option<Icon>,
        label: String,
        enabled: bool,
        options: Vec<(String, Action)>,
    },
}

impl Entry {
    pub fn item(item: Item, action: Action) -> Self {
        Entry::Item {
            item,
            action: Some(action),
        }
    }

    pub fn inert(item: Item) -> Self {
        Entry::Item {
            item: item.disabled(),
            action: None,
        }
    }
}
