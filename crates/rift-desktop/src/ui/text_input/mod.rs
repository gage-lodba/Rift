//! Single-line text input: caret, selection, editing keys and clipboard.
//!
//! The widget edits a caller-owned `String` in place and reports what happened
//! through a [`TextInputResponse`]. Editing state (caret position, selection
//! anchor, blink phase) lives in the [`Ui`] context keyed by the widget's [`Id`].
//!
//! Binding convention for screens:
//!
//! ```ignore
//! let r = ui.text_input(rect, Id::from_str("playlist-rename"), &mut draft, "Name", &TextInputStyle::default_theme(&theme));
//! if r.changed { /* mark dirty */ }
//! if r.submitted { commit(draft.clone()); }
//! if r.cancelled || r.lost_focus { cancel(); }
//! ```
//!
//! `lost_focus` is reported with **one frame of latency**: keyboard focus is
//! committed in [`Ui::end_frame`] (where a click-away press blurs, a later
//! widget may have taken focus, and a focused widget that vanished is dropped),
//! which is after every [`Ui::text_input`] call of that frame. `end_frame`
//! therefore records the id that lost focus, and the field reports it on its
//! next `text_input` call. Re-focusing the field in that frame (e.g. clicking
//! back into it) suppresses the report, so a fast blur + refocus is not a
//! cancel.
//!
//! Clipboard support uses the UI's clipboard: the native OS clipboard by
//! default, or an explicitly injected backend
//! ([`Ui::set_clipboard`](crate::ui::Ui::set_clipboard)); with neither,
//! Ctrl+C/X/V are no-ops. On Linux the native backend uses the Wayland
//! compositor clipboard when the session provides it and falls back to X11
//! otherwise — see [`crate::ui::clipboard`].
//!
//! # Retained editing state
//!
//! Per-field state (caret, selection anchor, horizontal scroll, blink phase)
//! lives in [`Ui::text_edits`], keyed by the widget [`Id`]. Each
//! [`Ui::text_input`] call *takes that entry out of the map* (defaulting to a
//! fresh state for a field's first frame), works on the owned local
//! [`TextEditState`] for the rest of the call, and puts it back before
//! returning. A missing entry therefore means "first frame", never an invariant
//! violation, so the widget needs no `get(..).unwrap()` and cannot panic on its
//! own bookkeeping. There are no early returns between taking and re-inserting
//! the state.
//!
//! # Module layout
//!
//! This file owns the public API and draw/paint; `state` owns [`TextEditState`]
//! and pointer placement, `edit` keyboard/clipboard helpers, `tests` the unit tests.

use super::{Align, Color, Id, InputState, Padding, Rect, Response, Sense, Ui};
use crate::text::{TextStyle, Weight};

mod edit;
mod state;
#[cfg(test)]
mod tests;

use self::edit::{floor_boundary, selection_range, word_end, word_start};
pub(crate) use state::TextEditState;

/// Append OS/IME text to [`InputState::text`], dropping control characters.
///
/// `main.rs` uses this for both platform text paths — `WindowEvent::Ime(
/// Ime::Commit(..))` (CJK compositions and soft-keyboard commits) and
/// `KeyEvent::text` (direct/character key presses). The two are complementary
/// in winit 0.30: a composition is delivered only as `Ime::Commit` (no
/// `KeyboardInput` while preediting), while a plain key press is delivered only
/// through `KeyEvent::text`, so appending both cannot double-type a character.
/// Control characters are stripped, matching the old `KeyboardInput` path; the
/// rest of the string is appended byte-for-byte, so multi-byte (CJK, emoji)
/// commits survive.
pub fn push_ime_text(input: &mut InputState, text: &str) {
    for ch in text.chars() {
        if !ch.is_control() {
            input.text.push(ch);
        }
    }
}

/// Appearance of a text field. Defaults mirror `.searchbox input` in
/// `styles.css`.
#[derive(Clone, Debug)]
pub struct TextInputStyle {
    pub text: TextStyle,
    pub placeholder_color: Color,
    pub bg: Color,
    pub border: Color,
    pub border_focused: Color,
    pub radius: f32,
    pub padding: Padding,
    pub selection: Color,
    pub caret: Color,
}

impl TextInputStyle {
    /// The default field look: raised background, 1px border, 6px radius,
    /// 13px text, 7x12 padding (`.side-new-input`).
    pub fn default_theme(theme: &crate::theme::Theme) -> Self {
        Self {
            text: TextStyle::new(13.0, Weight::Regular, theme.text),
            placeholder_color: theme.text_dim,
            bg: theme.bg_raised,
            border: theme.border,
            border_focused: theme.accent,
            radius: theme.radius_md,
            padding: Padding::symmetric(12.0, 7.0),
            selection: Color::rgba(139, 61, 240, 110),
            caret: theme.accent,
        }
    }

    /// The centered titlebar search box (13px, transparent, borderless inside a
    /// wrapper — draw the wrapper with `ui.panel`/`bordered_rect` yourself).
    pub fn search(theme: &crate::theme::Theme) -> Self {
        Self {
            bg: Color::TRANSPARENT,
            border: Color::TRANSPARENT,
            border_focused: Color::TRANSPARENT,
            padding: Padding::symmetric(12.0, 7.0),
            ..Self::default_theme(theme)
        }
    }

    /// A large title field (`.rename-input`): 17px bold, accent border.
    pub fn rename(theme: &crate::theme::Theme) -> Self {
        Self {
            text: TextStyle::new(17.0, Weight::Bold, theme.text),
            border: theme.accent,
            border_focused: theme.accent,
            radius: theme.radius_md,
            padding: Padding::symmetric(10.0, 4.0),
            ..Self::default_theme(theme)
        }
    }
}

/// Outcome of [`Ui::text_input`].
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TextInputResponse {
    pub response: Response,
    /// The text changed this frame.
    pub changed: bool,
    /// Enter was pressed while focused.
    pub submitted: bool,
    /// Escape was pressed while focused.
    pub cancelled: bool,
    /// The field lost keyboard focus at the end of the previous frame.
    ///
    /// Focus changes are committed by [`Ui::end_frame`], after this widget has
    /// already run, so this is reported on the **next** frame's call (see the
    /// module docs). A field that regained focus in this frame is not reported,
    /// so clicking back into a field does not look like a blur.
    pub lost_focus: bool,
}

impl Ui {
    /// Single-line editable text field bound to `text`.
    ///
    /// `rect` is the whole field including its padding. The widget draws its own
    /// background/border (use [`TextInputStyle::search`] for a borderless field
    /// inside a wrapper).
    pub fn text_input(
        &mut self,
        rect: Rect,
        id: Id,
        text: &mut String,
        placeholder: &str,
        style: &TextInputStyle,
    ) -> TextInputResponse {
        let now = self.time();
        let response = self.interact(rect, id, Sense::DRAG);
        let now_focused = self.is_focused(id);
        let mut out = TextInputResponse {
            response,
            // Focus loss is recorded by `end_frame` (previous frame) because a
            // click-away blur and focus taken by a later widget both happen
            // after this call. See the module docs for the one-frame latency.
            // Re-focusing here (e.g. clicking back into the field) wins.
            lost_focus: self.focus_lost_id == Some(id) && !now_focused,
            ..Default::default()
        };

        // Retained editing state: take the entry out of the map for the duration
        // of this call and re-insert it before returning (there are no early
        // returns below). A missing entry means "first frame for this id", not
        // an invariant violation, so this cannot panic.
        let mut st = self.text_edits.remove(&id).unwrap_or_default();

        // Focus gained without a pointer click (editor autofocus, Tab focus,
        // focus returned by a closing popup): put the caret at the end so typing
        // appends, like an HTML `autofocus` field. A click in this frame is
        // placed from the pointer below and therefore still wins.
        let focus_gained = now_focused && !st.focused;
        if focus_gained {
            st.cursor = text.len();
            st.anchor = None;
            st.blink_start = now;
        }

        // Keep the caret inside the (possibly changed) text.
        st.cursor = floor_boundary(text, st.cursor.min(text.len()));
        if let Some(a) = st.anchor {
            st.anchor = Some(floor_boundary(text, a.min(text.len())));
        }
        if st.cursor != st.last_cursor {
            st.blink_start = now;
            st.last_cursor = st.cursor;
        }

        // ---- pointer placement (click / drag / double-click) ----
        let content = rect.inset_pad(style.padding);
        if response.hovered && self.input().mouse_left_pressed {
            let x = self.input().mouse_pos.0 - content.x + st.scroll_x;
            let idx = self.caret_from_x(text, &style.text, x);
            let double = {
                // A double click selects the word under the pointer.
                matches!(self.last_click, Some((last_id, t)) if last_id == id && self.time() - t < 0.35)
            };
            let shift_down = self.input().modifiers.shift;
            st.cursor = idx;
            st.anchor = if double {
                Some(word_start(text, idx))
            } else if shift_down {
                st.anchor
            } else {
                None
            };
            st.blink_start = now;
            if double {
                st.cursor = word_end(text, idx);
            }
        } else if response.active && self.input().mouse_left_down {
            let x = self.input().mouse_pos.0 - content.x + st.scroll_x;
            let idx = self.caret_from_x(text, &style.text, x);
            if st.anchor.is_none() {
                st.anchor = Some(st.cursor);
            }
            st.cursor = idx;
            st.blink_start = now;
        }

        // ---- keyboard editing (only while focused) ----
        if now_focused {
            self.text_input_active = true;
            let (changed, submitted, cancelled) = self.edit_text(&mut st, text);
            out.changed = changed;
            out.submitted = submitted;
            out.cancelled = cancelled;
            if cancelled {
                self.clear_focus();
            }
        }

        // ---- paint ----
        // Only the focused field changes its border; hover alone must not tint it
        // (unlike buttons).
        let border = if now_focused {
            style.border_focused
        } else {
            style.border
        };
        self.bordered_rect(rect, style.bg, style.radius, 1.0, border);

        // Keep the caret visible horizontally.
        let caret_cursor = floor_boundary(text, st.cursor.min(text.len()));
        let caret_x_local = self.text_width(&text[..caret_cursor], &style.text);
        if caret_x_local - st.scroll_x > content.w - 2.0 {
            st.scroll_x = caret_x_local - content.w + 2.0;
        } else if caret_x_local < st.scroll_x {
            st.scroll_x = caret_x_local;
        }
        st.scroll_x = st.scroll_x.max(0.0);

        let color = if text.is_empty() {
            style.placeholder_color
        } else {
            style.text.color
        };
        let mut draw_style = style.text;
        draw_style.color = color;
        let line_h = self.line_height(&style.text);
        let text_rect = Rect::new(content.x, content.y, content.w, content.h);
        self.push_clip(text_rect);
        let block = Rect::new(
            content.x - st.scroll_x,
            content.y + (content.h - line_h) * 0.5,
            content.w + st.scroll_x,
            line_h,
        );
        if now_focused {
            // Record the caret for the OS IME candidate window. This is
            // independent of the blink phase (hidden blink must not move the
            // candidate list) and works for the empty field too.
            let caret_h = (line_h * 0.9).max(10.0);
            let y = block.y + (line_h - caret_h) * 0.5;
            self.ime_cursor = Some(Rect::new(block.x + caret_x_local, y, 1.5, caret_h));
        }
        if text.is_empty() {
            self.text_aligned(
                Rect::new(block.x, block.y, content.w, line_h),
                placeholder,
                &draw_style,
                Align::Left,
            );
        } else {
            // Selection highlight behind the glyphs.
            let range = selection_range(text, st.cursor, st.anchor);
            if let Some((start, end)) = range {
                let x0 = self.text_width(&text[..start], &style.text);
                let x1 = self.text_width(&text[..end], &style.text);
                self.fill(
                    Rect::new(
                        block.x + x0,
                        block.y + (line_h - 16.0) * 0.5,
                        (x1 - x0).max(1.0),
                        16.0,
                    ),
                    style.selection,
                );
            }
            self.text_raw(block, text, &draw_style);
            // Caret.
            if now_focused {
                let phase = (self.time() - st.blink_start) % 1.06;
                if phase < 0.53 {
                    let caret_h = (line_h * 0.9).max(10.0);
                    let y = block.y + (line_h - caret_h) * 0.5;
                    self.fill(
                        Rect::new(block.x + caret_x_local, y, 1.5, caret_h),
                        style.caret,
                    );
                }
            }
        }
        self.pop_clip();

        if response.focus_visible || (now_focused && self.keyboard_mode()) {
            self.focus_ring(rect, style.radius);
        }
        if response.hovered {
            self.set_cursor(super::CursorShape::Text);
        }

        // Commit the retained editing state for the next frame. `focused` is
        // sampled after editing (Escape clears focus inside this call).
        st.focused = self.is_focused(id);
        self.text_edits.insert(id, st);
        out.response = response;
        out
    }
}
