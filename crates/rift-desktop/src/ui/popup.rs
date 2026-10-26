//! Popups (context/kebab menus), tooltips and toasts.
//!
//! Overlays are painted into the UI's *deferred* buffer, which is appended after
//! all normal content in `end_frame`, so they always land on top even though the
//! immediate-mode code that declares them runs in the middle of a screen. While
//! a popup is open, the rectangles it covered last frame are registered as input
//! blockers, so the row underneath it neither hovers nor clicks.
//!
//! ```ignore
//! let kebab = ui.icon_button(kebab_rect, Icon::MoreVert);
//! if kebab.clicked { ui.toggle_popup(menu_id); }
//! ui.popup(menu_id, kebab_rect, Size::new(180.0, 2.0 * MENU_ITEM_HEIGHT),
//!          PopupAlign::Right, |ui, content| {
//!     let mut col = Column::new(content, 0.0);
//!     if ui.menu_item(col.fixed(MENU_ITEM_HEIGHT), "Play next", Some(Icon::PlaylistAdd), None, false).clicked {
//!         ui.close_popup();
//!         // … emit action …
//!     }
//! });
//! ```

use super::{Color, Icon, Id, Key, Padding, Rect, Response, Sense, Size, TextStyle, Ui, Vec2};
use crate::text::Weight;

/// A standard menu row height (`.menu-item`: 8px vertical padding + 17px line
/// box = 33px).
pub const MENU_ITEM_HEIGHT: f32 = 33.0;
/// The menu's outer min-width is 190px; content = 190 - 10px padding.
pub const MENU_MIN_CONTENT_WIDTH: f32 = 180.0;

/// Horizontal attachment of a popup to its anchor.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PopupAlign {
    /// Left edges aligned (`.menu` `left: 0`).
    #[default]
    Left,
    /// Right edges aligned (`.menu-right`).
    Right,
}

/// Visual variant of a toast. `.toast` is the error style; info uses the
/// neutral elevated card.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ToastKind {
    Error,
    #[default]
    Info,
}

/// A queued toast. Auto-dismisses after `duration` seconds.
#[derive(Clone, Debug)]
pub struct Toast {
    pub message: String,
    pub kind: ToastKind,
    pub created: f64,
    pub duration: f64,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct TooltipState {
    pub hover_start: f64,
}

#[derive(Clone, Debug)]
pub(crate) struct PendingTooltip {
    pub anchor: Rect,
    pub text: String,
}

impl Ui {
    // ------------------------------------------------------------- popups

    /// The currently open popup, if any.
    pub fn popup_open(&self) -> Option<Id> {
        self.open_popup
    }

    pub fn is_popup_open(&self, id: Id) -> bool {
        self.open_popup == Some(id)
    }

    /// Open `id` as the one popup drawn this frame, remembering the currently
    /// focused widget so [`Ui::close_popup`] can hand focus back to it.
    pub fn open_popup(&mut self, id: Id) {
        if self.open_popup != Some(id) {
            // Remember the trigger so Escape can hand focus back to it.
            self.popup_return_focus = self.focus;
        }
        self.open_popup = Some(id);
        self.popup_needs_initial_focus = true;
    }

    /// Close the open popup, returning keyboard focus to the widget that had it
    /// when the popup was opened (unless another widget took focus this frame).
    pub fn close_popup(&mut self) {
        if self.open_popup.is_none() {
            return;
        }
        self.open_popup = None;
        self.popup_needs_initial_focus = false;
        let return_focus = self.popup_return_focus.take();
        if !self.focus_taken_this_frame {
            if let Some(id) = return_focus {
                self.focus = Some(id);
                self.focus_visible = self.keyboard_used;
            }
        }
    }

    /// Toggle `id`: close it if it is the open popup, otherwise open it.
    ///
    /// Opening goes through [`Ui::open_popup`] (not just assigning
    /// `open_popup`) so a menu opened without the explicit `open_popup` call
    /// still gets the documented keyboard contract: its first row takes focus
    /// and Escape returns focus to the trigger.
    pub fn toggle_popup(&mut self, id: Id) {
        if self.open_popup == Some(id) {
            self.close_popup();
        } else {
            self.open_popup(id);
        }
    }

    /// Draw a popup anchored to `anchor` while it is open.
    ///
    /// `content_size` is the size of the *content* inside the popup (the 4px
    /// padding and 1px border are added for you). The closure receives the
    /// content rectangle. Clicking outside the popup and its anchor closes it;
    /// Escape closes it; call [`Ui::close_popup`] from inside the closure when an
    /// item is activated.
    pub fn popup<R>(
        &mut self,
        id: Id,
        anchor: Rect,
        content_size: Size,
        align: PopupAlign,
        f: impl FnOnce(&mut Ui, Rect) -> R,
    ) -> Option<R> {
        if self.open_popup != Some(id) {
            return None;
        }
        self.popup_drawn = true;
        if self.input().key_clicked(Key::Escape) {
            self.close_popup();
            return None;
        }
        let theme = self.theme().clone();
        let outer = Size::new((content_size.w + 10.0).max(80.0), content_size.h + 10.0);
        let vp = self.viewport();
        let mut x = match align {
            PopupAlign::Left => anchor.x,
            PopupAlign::Right => anchor.right() - outer.w,
        };
        let mut y = anchor.bottom() + 4.0;
        if y + outer.h > vp.bottom() - 8.0 {
            let above = anchor.y - outer.h - 4.0;
            y = if above >= 8.0 {
                above
            } else {
                (vp.bottom() - 8.0 - outer.h).max(8.0)
            };
        }
        x = x.clamp(8.0, (vp.right() - outer.w - 8.0).max(8.0));
        let menu_rect = Rect::new(x, y, outer.w, outer.h);

        // Frame first, then content: both deferred.
        self.deferred(|ui| {
            ui.shadow(menu_rect, 8.0, 1.0);
            ui.bordered_rect(menu_rect, theme.bg_raised, 8.0, 1.0, theme.border);
        });
        let content = menu_rect.inset(5.0);
        self.popup_depth += 1;
        self.popup_items.clear();
        let out = self.deferred(|ui| f(ui, content));
        self.popup_depth -= 1;
        self.next_blockers.push(menu_rect);

        // ---- keyboard navigation between the rows drawn above ----
        let items = std::mem::take(&mut self.popup_items);
        if !items.is_empty() {
            if self.popup_needs_initial_focus
                || self
                    .focus
                    .map(|f| !items.iter().any(|(id, _)| *id == f))
                    .unwrap_or(true)
            {
                self.focus = Some(items[0].0);
                self.focus_visible = true;
                self.popup_needs_initial_focus = false;
            }
            if let Some(pos) = items.iter().position(|(id, _)| Some(*id) == self.focus) {
                let last = items.len() - 1;
                let mut next = pos;
                if self.input().key_clicked(Key::Down) {
                    next = (pos + 1).min(last);
                } else if self.input().key_clicked(Key::Up) {
                    next = pos.saturating_sub(1);
                } else if self.input().key_clicked(Key::Home) {
                    next = 0;
                } else if self.input().key_clicked(Key::End) {
                    next = last;
                }
                if next != pos {
                    self.focus = Some(items[next].0);
                    self.focus_visible = true;
                }
            }
        }
        self.popup_items = items;

        // Tab leaves the menu (focus moved by `begin_frame`).
        if self.input().key_clicked(Key::Tab) {
            self.close_popup();
            return Some(out);
        }

        // Click-outside dismissal (the anchor itself is excluded so its button
        // can toggle the menu closed).
        let pos = Vec2::from(self.input().mouse_pos);
        let outside = self.input().mouse_left_pressed
            && !menu_rect.contains(pos.x, pos.y)
            && !anchor.contains(pos.x, pos.y);
        if outside {
            self.close_popup();
        }
        Some(out)
    }

    // ---------------------------------------------------------- menu items

    /// One `.menu-item`: icon (optional), label, right-aligned shortcut hint.
    /// Rendered only inside a [`Ui::popup`] closure; the id is scoped to the
    /// open popup so identical labels in different menus cannot collide.
    pub fn menu_item(
        &mut self,
        rect: Rect,
        label: &str,
        icon: Option<Icon>,
        shortcut: Option<&str>,
        danger: bool,
    ) -> Response {
        let id = self.open_popup.unwrap_or(Id::NONE).with(label);
        self.menu_item_full(rect, id, label, icon, shortcut, danger, 10.0)
    }

    /// Menu item with an explicit id (needed when the label changes between
    /// frames, e.g. "Play"/"Pause").
    pub fn menu_item_with_id(
        &mut self,
        rect: Rect,
        id: Id,
        label: &str,
        icon: Option<Icon>,
        shortcut: Option<&str>,
        danger: bool,
    ) -> Response {
        self.menu_item_full(rect, id, label, icon, shortcut, danger, 10.0)
    }

    /// Indented menu item (`.menu-sub-option`: 36px left padding).
    pub fn menu_item_indented(
        &mut self,
        rect: Rect,
        label: &str,
        icon: Option<Icon>,
        shortcut: Option<&str>,
        danger: bool,
    ) -> Response {
        let id = self.open_popup.unwrap_or(Id::NONE).with(("sub", label));
        self.menu_item_full(rect, id, label, icon, shortcut, danger, 36.0)
    }

    #[allow(clippy::too_many_arguments)]
    fn menu_item_full(
        &mut self,
        rect: Rect,
        id: Id,
        label: &str,
        icon: Option<Icon>,
        shortcut: Option<&str>,
        danger: bool,
        indent: f32,
    ) -> Response {
        let mut response = self.interact(rect, id, Sense::CLICK);
        if self.popup_depth > 0 {
            self.popup_items.push((id, rect));
            // The first row of a freshly opened menu takes keyboard focus on the
            // same frame it is drawn, so its focus ring is visible immediately.
            if self.popup_needs_initial_focus {
                self.popup_needs_initial_focus = false;
                self.focus = Some(id);
                self.focus_visible = true;
                response.focused = true;
                response.focus_visible = true;
            }
        }
        let theme = self.theme().clone();
        let t = if response.active {
            1.0
        } else {
            response.hover_t
        };
        if t > 0.0 {
            self.rounded_rect(rect, theme.bg_hover.fade(t), theme.radius_md);
        }
        let fg = if danger { theme.accent } else { theme.text };
        let mut x = rect.x + indent;
        if let Some(icon) = icon {
            let icon_rect = Rect::new(x, rect.y + (rect.h - 16.0) * 0.5, 16.0, 16.0);
            self.icon(icon_rect, icon, theme.text_dim, 16.0);
            x += 26.0;
        }
        let shortcut_style = TextStyle::new(11.0, Weight::Regular, theme.text_dim);
        let shortcut_w = shortcut
            .map(|s| self.text_width(s, &shortcut_style))
            .unwrap_or(0.0);
        let label_style = TextStyle::new(13.0, Weight::Regular, fg);
        let label_w = (rect.right() - 10.0 - shortcut_w - x).max(0.0);
        self.text(Rect::new(x, rect.y, label_w, rect.h), label, &label_style);
        if let Some(shortcut) = shortcut {
            self.text_right(
                Rect::new(rect.right() - 10.0 - shortcut_w, rect.y, shortcut_w, rect.h),
                shortcut,
                &shortcut_style,
            );
        }
        if response.focus_visible {
            // Menu options use the inset ring from ui-spec §2.4.
            self.focus_ring_inset(rect, theme.radius_md);
        }
        response
    }

    /// Menu item with a trailing submenu caret (`▸` collapsed / `▾` expanded)
    /// rendered as the Material chevron glyphs, 14px dim. Expanding content is
    /// drawn inline below the item (like `.menu-sub-list` in the CSS).
    pub fn menu_item_submenu(
        &mut self,
        rect: Rect,
        label: &str,
        icon: Option<Icon>,
        expanded: bool,
    ) -> Response {
        let response = self.menu_item(rect, label, icon, None, false);
        let theme = self.theme().clone();
        let caret = if expanded {
            Icon::ExpandLess
        } else {
            Icon::ChevronRight
        };
        let caret_rect = Rect::new(
            rect.right() - 10.0 - 14.0,
            rect.y + (rect.h - 14.0) * 0.5,
            14.0,
            14.0,
        );
        self.icon(caret_rect, caret, theme.text_dim, 14.0);
        response
    }

    /// A 1px separator row (`.menu-sep`: 1px line, 4px vertical / 6px
    /// horizontal margin). Pass the separator row's rectangle (typically 9px
    /// tall).
    pub fn menu_separator(&mut self, rect: Rect) {
        let theme = self.theme().clone();
        let y = rect.y + (rect.h - 1.0) * 0.5;
        self.fill(
            Rect::new(rect.x + 6.0, y, (rect.w - 12.0).max(0.0), 1.0),
            theme.border,
        );
    }

    // ------------------------------------------------------------ tooltips

    /// Register a tooltip for this frame. Shows after a 450ms hover delay above
    /// `anchor`; call once per frame while `hovered` is true (widgets pass their
    /// [`Response`] via [`Ui::tooltip_for`]).
    pub fn tooltip(&mut self, id: Id, anchor: Rect, hovered: bool, text: &str) {
        if !hovered || text.is_empty() {
            self.tooltips.remove(&id);
            return;
        }
        let now = self.time();
        let state = self
            .tooltips
            .entry(id)
            .or_insert(TooltipState { hover_start: now });
        if now - state.hover_start >= 0.45 {
            self.pending_tooltip = Some(PendingTooltip {
                anchor,
                text: text.to_string(),
            });
        }
        if self.tooltips.len() > 512 {
            self.tooltips.clear();
        }
    }

    /// Tooltip for a widget response (uses its id and rectangle).
    pub fn tooltip_for(&mut self, response: &Response, text: &str) {
        self.tooltip(response.id, response.rect, response.hovered, text);
    }

    // -------------------------------------------------------------- toasts

    /// Queue a toast. One toast is visible at a time: a newer toast replaces the
    /// previous one and restarts the 5s timer (`.toast`, ui-spec §6.5).
    pub fn toast(&mut self, message: impl Into<String>, kind: ToastKind) {
        self.toast_with_duration(message, kind, 5.0);
    }

    /// Toast with an explicit lifetime (e.g. 6s for command errors).
    pub fn toast_with_duration(
        &mut self,
        message: impl Into<String>,
        kind: ToastKind,
        duration_secs: f64,
    ) {
        self.toasts.clear();
        self.toasts.push(Toast {
            message: message.into(),
            kind,
            created: self.time(),
            duration: duration_secs,
        });
    }

    /// Toast that stays until replaced (`"Importing playlist…"`).
    pub fn toast_sticky(&mut self, message: impl Into<String>) {
        self.toast_with_duration(message, ToastKind::Info, f64::INFINITY);
    }

    pub fn toast_error(&mut self, message: impl Into<String>) {
        self.toast(message, ToastKind::Error);
    }

    pub fn toast_info(&mut self, message: impl Into<String>) {
        self.toast(message, ToastKind::Info);
    }
}

// ---------------------------------------------------------------------------
// end-of-frame overlay painting (called from `Ui::end_frame`)
// ---------------------------------------------------------------------------

/// Close an open popup that was not drawn during the frame (view switch, its
/// screen stopped rendering): the popup must never leak onto another screen.
/// Deliberately does not restore focus — the trigger widget is gone too.
pub(crate) fn close_popup_if_not_drawn(ui: &mut Ui) {
    ui.open_popup = None;
    ui.popup_needs_initial_focus = false;
    ui.popup_return_focus = None;
}

pub(crate) fn draw_pending_tooltip(ui: &mut Ui) {
    let Some(pending) = ui.pending_tooltip.take() else {
        return;
    };
    let theme = ui.theme().clone();
    let style = TextStyle::new(12.0, Weight::Regular, theme.text);
    let max_w = 320.0;
    let size = ui.measure_text_wrapped(&pending.text, &style, max_w);
    let w = (size.w + 20.0).min(max_w).max(24.0);
    let h = size.h + 12.0;
    let vp = ui.viewport();
    let mut x = pending.anchor.x + (pending.anchor.w - w) * 0.5;
    x = x.clamp(6.0, (vp.right() - w - 6.0).max(6.0));
    let mut y = pending.anchor.y - h - 6.0;
    if y < 6.0 {
        y = pending.anchor.bottom() + 6.0;
    }
    let rect = Rect::new(x, y, w, h);
    let content = rect.inset_pad(Padding::symmetric(10.0, 6.0));
    ui.deferred(|ui| {
        ui.bordered_rect(rect, Color::hex(0x1c1c20), 6.0, 1.0, theme.border);
        ui.text_wrapped(content, &pending.text, &style);
    });
}

pub(crate) fn draw_toasts(ui: &mut Ui) {
    if ui.toasts.is_empty() {
        return;
    }
    let now = ui.time();
    let mut toasts = std::mem::take(&mut ui.toasts);
    toasts.retain(|t| now < t.created + t.duration);
    if toasts.is_empty() {
        ui.toasts = toasts;
        return;
    }
    let vp = ui.viewport();
    let mut y = vp.bottom() - 110.0;
    for toast in toasts.iter().rev() {
        // ui-spec §6.5: the CSS only defines the error palette, and the old app
        // reused this one component for informational notices too.
        let (fg, bg, border) = match toast.kind {
            ToastKind::Error | ToastKind::Info => (
                Color::hex(0xffb3bb),
                Color::hex(0x2a1216),
                Color::hex(0xb81e30),
            ),
        };
        let style = TextStyle::new(13.0, Weight::Regular, fg);
        let max_w = 420.0;
        let size = ui.measure_text_wrapped(&toast.message, &style, max_w - 28.0);
        let w = (size.w + 28.0).min(max_w).max(60.0);
        let h = size.h + 20.0;
        let rect = Rect::new(18.0, y - h, w, h);
        let content = rect.inset_pad(Padding::symmetric(14.0, 10.0));
        let message = toast.message.clone();
        ui.deferred(|ui| {
            ui.bordered_rect(rect, bg, 8.0, 1.0, border);
            ui.text_wrapped(content, &message, &style);
        });
        y = rect.y - 8.0;
        if y < 8.0 {
            break;
        }
    }
    ui.toasts = toasts;
}
