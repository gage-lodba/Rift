//! Retained per-field editing state and pointer-to-caret placement.

use crate::text::TextStyle;
use crate::ui::Ui;

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
    /// Whether the field held keyboard focus on its previous call. Used to put
    /// the caret at the end when focus is gained without a pointer click
    /// (autofocus / Tab), so typing appends like an HTML autofocused input.
    pub focused: bool,
}

impl Default for TextEditState {
    fn default() -> Self {
        Self {
            cursor: 0,
            anchor: None,
            scroll_x: 0.0,
            blink_start: 0.0,
            last_cursor: 0,
            focused: false,
        }
    }
}

impl Ui {
    /// Caret byte index closest to local x (measured from the text block's left).
    ///
    /// Walks the string once, accumulating per-character advances, instead of
    /// re-measuring every prefix: drag-selecting calls this every frame, so the
    /// old `text_width(&text[..idx])` loop was quadratic in glyph-metric
    /// lookups over long fields.
    pub(super) fn caret_from_x(&mut self, text: &str, style: &TextStyle, x: f32) -> usize {
        if x <= 0.0 {
            return 0;
        }
        let spacing = if style.letter_spacing.is_finite() {
            style.letter_spacing
        } else {
            0.0
        };
        // Width of the prefix ending at the current char boundary (tracking
        // included, but no trailing tracking) — the same definition
        // `Fonts::line_width` uses.
        let mut width = 0.0f32;
        let mut last_boundary = 0usize;
        let mut last_width = 0.0f32;
        for (idx, ch) in text.char_indices() {
            if idx > 0 && width >= x {
                // Pick whichever boundary is closer.
                return if (width - x) < (x - last_width) {
                    idx
                } else {
                    last_boundary
                };
            }
            last_boundary = idx;
            last_width = width;
            // Mirror Fonts::line_width: hard breaks contribute no advance
            // and no tracking (the field is single-line, but the bound text
            // can contain them).
            if ch != '\n' && ch != '\r' {
                width += self.fonts_mut().advance(ch, style) + spacing;
            }
        }
        text.len()
    }
}
