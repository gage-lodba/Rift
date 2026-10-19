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
//! Clipboard support uses the UI's clipboard: the native OS clipboard by
//! default, or an explicitly injected backend
//! ([`Ui::set_clipboard`](crate::ui::Ui::set_clipboard)); with neither,
//! Ctrl+C/X/V are no-ops.
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

use super::{Align, Color, Id, Key, Padding, Rect, Response, Sense, Ui};
use crate::text::{TextStyle, Weight};

/// Editing state retained per text field.
#[derive(Clone, Copy, Debug)]
pub(crate) struct TextEditState {
    /// Caret position (byte index into the bound string).
    pub cursor: usize,
    /// Selection anchor (byte index); `None` when there is no selection.
    pub anchor: Option<usize>,
    /// Horizontal scroll for text wider than the field.
    pub scroll_x: f32,
    /// When the caret blink phase started (seconds).
    pub blink_start: f64,
    /// Caret position last frame, to detect keyboard activity.
    pub last_cursor: usize,
}

impl Default for TextEditState {
    fn default() -> Self {
        Self {
            cursor: 0,
            anchor: None,
            scroll_x: 0.0,
            blink_start: 0.0,
            last_cursor: 0,
        }
    }
}

/// Appearance of a text field. Defaults mirror `.searchbox input` /
/// `.ytdlp-input` in `styles.css`.
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
    /// 13px text, 7x12 padding (`.ytdlp-input`, `.side-new-input`).
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
    /// The field had focus last frame but not this frame.
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
        let was_focused = self.is_focused(id);
        let now = self.time();
        let response = self.interact(rect, id, Sense::DRAG);
        let now_focused = self.is_focused(id);
        let mut out = TextInputResponse {
            response,
            lost_focus: was_focused && !now_focused,
            ..Default::default()
        };

        // Retained editing state: take the entry out of the map for the duration
        // of this call and re-insert it before returning (there are no early
        // returns below). A missing entry means "first frame for this id", not
        // an invariant violation, so this cannot panic.
        let mut st = self.text_edits.remove(&id).unwrap_or_default();

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
            let (changed, submitted, cancelled) = self.edit_text(&mut st, text, style);
            out.changed = changed;
            out.submitted = submitted;
            out.cancelled = cancelled;
            if cancelled {
                self.clear_focus();
            }
        }

        // ---- paint ----
        // Only the focused field changes its border (`.ytdlp-input:focus`); hover
        // alone must not tint it (unlike buttons).
        let border = if now_focused {
            style.border_focused
        } else {
            style.border
        };
        self.bordered_rect(rect, style.bg, style.radius, 1.0, border);
        if now_focused && style.border.a == 0 && style.bg.a == 0 {
            // Borderless style (search box): nothing extra; the wrapper draws it.
        }

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

        // Commit the retained editing state for the next frame.
        self.text_edits.insert(id, st);
        out.response = response;
        out
    }

    /// Process this frame's key/text events for the focused field.
    /// Returns `(changed, submitted, cancelled)`.
    fn edit_text(
        &mut self,
        st: &mut TextEditState,
        text: &mut String,
        _style: &TextInputStyle,
    ) -> (bool, bool, bool) {
        let mut changed = false;
        let mut submitted = false;
        let mut cancelled = false;
        let command = self.input().command();
        let shift = self.input().modifiers.shift;
        let keys = self.input().keys.clone();
        let typed = self.input().text.clone();
        let now = self.time();

        // Character input first (IME/typing), replacing any selection.
        if !typed.is_empty() {
            let mut insert = String::new();
            for ch in typed.chars() {
                if !ch.is_control() {
                    insert.push(ch);
                }
            }
            if !insert.is_empty() {
                let (start, end) =
                    selection_range(text, st.cursor, st.anchor).unwrap_or((st.cursor, st.cursor));
                text.replace_range(start..end, &insert);
                st.cursor = start + insert.len();
                st.anchor = None;
                st.blink_start = now;
                changed = true;
            }
        }

        for key in keys {
            if !key.pressed {
                continue;
            }
            match key.key {
                Key::Char(_) => {} // handled through `input.text`
                Key::Backspace => {
                    let (start, end) =
                        selection_range(text, st.cursor, st.anchor).unwrap_or_else(|| {
                            let prev = prev_boundary(text, st.cursor);
                            (prev, st.cursor)
                        });
                    if start != end {
                        text.replace_range(start..end, "");
                        st.cursor = start;
                        st.anchor = None;
                        changed = true;
                    }
                }
                Key::Delete => {
                    let (start, end) =
                        selection_range(text, st.cursor, st.anchor).unwrap_or_else(|| {
                            let next = next_boundary(text, st.cursor);
                            (st.cursor, next)
                        });
                    if start != end {
                        text.replace_range(start..end, "");
                        st.cursor = start;
                        st.anchor = None;
                        changed = true;
                    }
                }
                Key::Left => {
                    let mut next = if shift {
                        st.anchor.unwrap_or(st.cursor)
                    } else {
                        selection_range(text, st.cursor, st.anchor)
                            .map(|(s, _)| s)
                            .unwrap_or(st.cursor)
                    };
                    if !shift || st.anchor.is_none() {
                        next = prev_boundary(text, st.cursor);
                    }
                    if shift {
                        if st.anchor.is_none() {
                            st.anchor = Some(st.cursor);
                        }
                    } else {
                        st.anchor = None;
                    }
                    st.cursor = next;
                    st.blink_start = now;
                }
                Key::Right => {
                    let mut next = if shift {
                        st.anchor.unwrap_or(st.cursor)
                    } else {
                        selection_range(text, st.cursor, st.anchor)
                            .map(|(_, e)| e)
                            .unwrap_or(st.cursor)
                    };
                    if !shift || st.anchor.is_none() {
                        next = next_boundary(text, st.cursor);
                    }
                    if shift {
                        if st.anchor.is_none() {
                            st.anchor = Some(st.cursor);
                        }
                    } else {
                        st.anchor = None;
                    }
                    st.cursor = next;
                    st.blink_start = now;
                }
                Key::Home => {
                    if shift && st.anchor.is_none() {
                        st.anchor = Some(st.cursor);
                    } else if !shift {
                        st.anchor = None;
                    }
                    st.cursor = 0;
                    st.blink_start = now;
                }
                Key::End => {
                    if shift && st.anchor.is_none() {
                        st.anchor = Some(st.cursor);
                    } else if !shift {
                        st.anchor = None;
                    }
                    st.cursor = text.len();
                    st.blink_start = now;
                }
                Key::Enter => submitted = true,
                Key::Escape => cancelled = true,
                Key::Tab => {} // focus traversal is global
                _ => {}
            }
            // Ctrl+A / Ctrl+C / Ctrl+X / Ctrl+V.
            if command {
                match key.key {
                    Key::Char('a' | 'A') => {
                        st.anchor = Some(0);
                        st.cursor = text.len();
                        st.blink_start = now;
                    }
                    Key::Char('c' | 'C') => {
                        if let Some((s, e)) = selection_range(text, st.cursor, st.anchor) {
                            let selected = text[s..e].to_string();
                            self.set_clipboard_text(&selected);
                        }
                    }
                    Key::Char('x' | 'X') => {
                        if let Some((s, e)) = selection_range(text, st.cursor, st.anchor) {
                            let selected = text[s..e].to_string();
                            self.set_clipboard_text(&selected);
                            text.replace_range(s..e, "");
                            st.cursor = s;
                            st.anchor = None;
                            changed = true;
                        }
                    }
                    Key::Char('v' | 'V') => {
                        if let Some(mut paste) = self.clipboard_text() {
                            paste.retain(|c| !c.is_control());
                            if !paste.is_empty() {
                                let (s, e) = selection_range(text, st.cursor, st.anchor)
                                    .unwrap_or((st.cursor, st.cursor));
                                text.replace_range(s..e, &paste);
                                st.cursor = s + paste.len();
                                st.anchor = None;
                                changed = true;
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        (changed, submitted, cancelled)
    }

    /// Caret byte index closest to local x (measured from the text block's left).
    fn caret_from_x(&mut self, text: &str, style: &TextStyle, x: f32) -> usize {
        if x <= 0.0 {
            return 0;
        }
        let mut last_boundary = 0;
        let mut last_x = 0.0;
        for (idx, _) in text.char_indices().skip(1) {
            let w = self.text_width(&text[..idx], style);
            if w >= x {
                // Pick whichever boundary is closer.
                return if (w - x) < (x - last_x) {
                    idx
                } else {
                    last_boundary
                };
            }
            last_boundary = idx;
            last_x = w;
        }
        text.len()
    }
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn floor_boundary(text: &str, mut i: usize) -> usize {
    if i >= text.len() {
        return text.len();
    }
    while i > 0 && !text.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn prev_boundary(text: &str, i: usize) -> usize {
    let mut i = floor_boundary(text, i.min(text.len()));
    if i == 0 {
        return 0;
    }
    i -= 1;
    while i > 0 && !text.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn next_boundary(text: &str, i: usize) -> usize {
    let mut i = floor_boundary(text, i.min(text.len()));
    if i >= text.len() {
        return text.len();
    }
    i += 1;
    while i < text.len() && !text.is_char_boundary(i) {
        i += 1;
    }
    i
}

/// `(start, end)` of the selection if there is one, else `None`.
fn selection_range(text: &str, cursor: usize, anchor: Option<usize>) -> Option<(usize, usize)> {
    let anchor = anchor?;
    let cursor = floor_boundary(text, cursor.min(text.len()));
    let anchor = floor_boundary(text, anchor.min(text.len()));
    if cursor == anchor {
        return None;
    }
    Some((cursor.min(anchor), cursor.max(anchor)))
}

fn word_start(text: &str, i: usize) -> usize {
    let i = floor_boundary(text, i);
    let mut start = i;
    for (idx, ch) in text[..i].char_indices().rev() {
        if !ch.is_alphanumeric() && ch != '_' && ch != '-' {
            break;
        }
        start = idx;
    }
    start
}

fn word_end(text: &str, i: usize) -> usize {
    let i = floor_boundary(text, i);
    let mut end = i;
    for (idx, ch) in text[i..].char_indices() {
        if !ch.is_alphanumeric() && ch != '_' && ch != '-' {
            break;
        }
        end = i + idx + ch.len_utf8();
    }
    end
}
