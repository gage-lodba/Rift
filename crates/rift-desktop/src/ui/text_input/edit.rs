//! Keyboard editing, selection/caret movement, clipboard and boundary helpers.

use super::TextEditState;
use crate::ui::{Key, Ui};

/// Maximum number of bytes one paste may insert (64 KiB).
///
/// A single-line field is a search box or a playlist name; a larger payload is
/// almost always a mis-copied document or code buffer, and inserting it
/// wholesale would make the bound string and its per-frame shaping arbitrarily
/// large. Ctrl+V therefore inserts at most this many bytes, cut on a UTF-8 char
/// boundary, and shows the UI's standard toast when it truncates. The cap is
/// applied to the pasted payload only (a paste into an already non-empty field
/// can grow the field by the selection-replacement plus up to this much).
pub const MAX_PASTE_BYTES: usize = 64 * 1024;

impl Ui {
    /// Process this frame's key/text events for the focused field.
    /// Returns `(changed, submitted, cancelled)`.
    pub(super) fn edit_text(
        &mut self,
        st: &mut TextEditState,
        text: &mut String,
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
                        if let Some(raw) = self.clipboard_text() {
                            let (paste, truncated) = paste_text(&raw);
                            if !paste.is_empty() {
                                let (s, e) = selection_range(text, st.cursor, st.anchor)
                                    .unwrap_or((st.cursor, st.cursor));
                                text.replace_range(s..e, &paste);
                                st.cursor = s + paste.len();
                                st.anchor = None;
                                st.blink_start = now;
                                changed = true;
                            }
                            if truncated {
                                // Do not silently drop the tail of a paste.
                                self.toast_info("Pasted text was truncated to 64 KiB.");
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        (changed, submitted, cancelled)
    }
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

/// Sanitize clipboard text for one paste: strip control characters and cap the
/// result at [`MAX_PASTE_BYTES`], stopping on a UTF-8 char boundary.
///
/// Returns the text to insert and whether the cap truncated it (dropping
/// control characters is not truncation). Scanning stops at the cap, so a
/// multi-megabyte clipboard is never copied into the field or fully shaped.
pub(super) fn paste_text(raw: &str) -> (String, bool) {
    let mut out = String::new();
    for ch in raw.chars() {
        if ch.is_control() {
            continue;
        }
        if out.len() + ch.len_utf8() > MAX_PASTE_BYTES {
            return (out, true);
        }
        out.push(ch);
    }
    (out, false)
}

pub(super) fn floor_boundary(text: &str, mut i: usize) -> usize {
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
pub(super) fn selection_range(
    text: &str,
    cursor: usize,
    anchor: Option<usize>,
) -> Option<(usize, usize)> {
    let anchor = anchor?;
    let cursor = floor_boundary(text, cursor.min(text.len()));
    let anchor = floor_boundary(text, anchor.min(text.len()));
    if cursor == anchor {
        return None;
    }
    Some((cursor.min(anchor), cursor.max(anchor)))
}

pub(super) fn word_start(text: &str, i: usize) -> usize {
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

pub(super) fn word_end(text: &str, i: usize) -> usize {
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
