//! Text shaping and rasterisation (owned by the `imgui` teammate).
//!
//! The UI never draws a string directly. It asks [`Fonts`] to *shape* the string
//! into a [`TextShape`] — a flat list of positioned quads with atlas UVs — and
//! emits those quads as ordinary `Prim::Image` primitives, the exact same
//! primitive type the renderer uses for cover art. The renderer therefore needs
//! to know nothing about fonts.
//!
//! Fonts are embedded (see [`embedded`](self)): Inter for UI text and Material
//! Icons for glyph icons. Rasterisation happens lazily on first use and is
//! cached in [`GlyphAtlas`]; measuring (`measure`, `wrap_ranges`, `ellipsize`)
//! uses the font's metrics only and never rasterises.
//!
//! Coordinates in the returned shapes are **logical pixels** (the UI's layout
//! space). Glyph bitmaps are rasterised at `size_px * scale` (physical pixels,
//! where `scale` is the window DPI scale) and their quads are reported at
//! `bitmap_px / scale`, so text stays crisp on `HiDPI` displays without the UI
//! having to think about it. [`Fonts::set_scale`] is called once per frame (or
//! whenever the window scale changes).

mod atlas;
mod embedded;
#[cfg(test)]
mod tests;

pub use atlas::{GlyphAtlas, IRect, INITIAL_SIZE as ATLAS_INITIAL_SIZE};

use std::borrow::Cow;
use std::collections::HashMap;
use std::ops::Range;

use fontdue::{Font, FontSettings};

use crate::gfx::paint::{Color, TextureId, TextureUpload};

/// Reserved texture id for the glyph atlas (ids below 1024 are reserved for
/// framework textures; cover art starts at 1024).
pub const ATLAS_TEXTURE_ID: TextureId = 1;

// ---------------------------------------------------------------------------
// geometry
// ---------------------------------------------------------------------------

/// A width/height pair in logical pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Size {
    pub w: f32,
    pub h: f32,
}

impl Size {
    pub const ZERO: Size = Size { w: 0.0, h: 0.0 };

    pub const fn new(w: f32, h: f32) -> Self {
        Self { w, h }
    }

    pub fn is_empty(&self) -> bool {
        self.w <= 0.0 || self.h <= 0.0
    }
}

// ---------------------------------------------------------------------------
// styles
// ---------------------------------------------------------------------------

/// Which static Inter file to use. Rift only ever needs these four weights
/// (the CSS uses 400/500/600/700 and 800 for the wordmark, which maps to Bold).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Weight {
    #[default]
    Regular,
    Medium,
    SemiBold,
    Bold,
}

impl Weight {
    pub const ALL: [Weight; 4] = [
        Weight::Regular,
        Weight::Medium,
        Weight::SemiBold,
        Weight::Bold,
    ];

    pub fn face_index(self) -> u8 {
        match self {
            Weight::Regular => 0,
            Weight::Medium => 1,
            Weight::SemiBold => 2,
            Weight::Bold => 3,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Weight::Regular => "Inter Regular",
            Weight::Medium => "Inter Medium",
            Weight::SemiBold => "Inter SemiBold",
            Weight::Bold => "Inter Bold",
        }
    }
}

/// Everything needed to shape one run of Inter text.
///
/// `line_height` of `0.0` means "the font's default line height" (`normal` in
/// CSS). Set it explicitly when a screen needs a hard line pitch (track rows,
/// wrapped descriptions).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextStyle {
    pub size_px: f32,
    pub weight: Weight,
    pub color: Color,
    /// Logical line pitch; `0.0` = font default.
    pub line_height: f32,
    /// Extra tracking in logical px added after every glyph.
    pub letter_spacing: f32,
}

impl TextStyle {
    pub const fn new(size_px: f32, weight: Weight, color: Color) -> Self {
        Self {
            size_px,
            weight,
            color,
            line_height: 0.0,
            letter_spacing: 0.0,
        }
    }

    pub const fn with_color(mut self, color: Color) -> Self {
        self.color = color;
        self
    }

    pub const fn with_weight(mut self, weight: Weight) -> Self {
        self.weight = weight;
        self
    }

    pub const fn with_line_height(mut self, line_height: f32) -> Self {
        self.line_height = line_height;
        self
    }

    pub const fn with_letter_spacing(mut self, letter_spacing: f32) -> Self {
        self.letter_spacing = letter_spacing;
        self
    }
}

impl Default for TextStyle {
    fn default() -> Self {
        Self::new(14.0, Weight::Regular, Color::hex(0xececef))
    }
}

// ---------------------------------------------------------------------------
// shaped output
// ---------------------------------------------------------------------------

/// One positioned glyph quad, ready to be emitted as a tinted image primitive.
/// All values are logical pixels relative to the top-left of the text block;
/// `uv` is normalized atlas texture space.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ShapedGlyph {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub uv: [f32; 4],
}

/// A shaped text block. Glyph order is back-to-front paint order.
#[derive(Clone, Debug, Default)]
pub struct TextShape {
    pub glyphs: Vec<ShapedGlyph>,
    pub size: Size,
    pub lines: u32,
    /// Baseline of the first line, measured down from the top of the block.
    pub baseline: f32,
}

impl TextShape {
    pub fn new() -> Self {
        Self {
            lines: 1,
            ..Self::default()
        }
    }

    /// Reset for reuse; keeps the glyph allocation.
    pub fn clear(&mut self) {
        self.glyphs.clear();
        self.size = Size::ZERO;
        self.lines = 1;
        self.baseline = 0.0;
    }
}

/// Font line metrics in logical pixels (`descent` is negative).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LineMetrics {
    pub ascent: f32,
    pub descent: f32,
    pub line_height: f32,
}

// ---------------------------------------------------------------------------
// font faces
// ---------------------------------------------------------------------------

const FACE_ICONS_FILLED: u8 = 4;
const FACE_ICONS_OUTLINED: u8 = 5;

/// Parse one embedded font.
///
/// Returns `None` instead of panicking if the baked-in bytes were ever corrupt:
/// [`Fonts::new`] then falls back to the first face that did load (and to
/// glyph-less layout if none did), so an asset/build error degrades the UI
/// instead of aborting the process at startup.
fn load_font(bytes: &'static [u8]) -> Option<Font> {
    Font::from_bytes(bytes, FontSettings::default()).ok()
}

// ---------------------------------------------------------------------------
// glyph cache
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct GlyphKey {
    face: u8,
    glyph: u16,
    /// Physical pixel size (already rounded).
    px: u16,
}

/// Metrics in physical pixels, y-up relative to the baseline (fontdue
/// convention: `ymin` is the distance from the baseline up to the glyph bottom).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct GlyphMetrics {
    advance: f32,
    xmin: f32,
    ymin: f32,
    w: f32,
    h: f32,
}

// ---------------------------------------------------------------------------
// Fonts
// ---------------------------------------------------------------------------

/// Ascent/descent ratios of the reference screenshot's UI font (Segoe UI: hhea
/// ascent 2210, descent 514, 2048 units/em). The old app rendered with Segoe UI;
/// we draw Inter, but `ui-spec.md` §4.2 requires the *line boxes* of the
/// reference. CSS positions glyphs by half-leading inside that line box, so to
/// reproduce the reference ink position we position Inter's baseline with the
/// reference font's vertical metrics. Without this every label sits ~2px high.
const REF_ASCENT_RATIO: f32 = 2210.0 / 2048.0;
const REF_DESCENT_RATIO: f32 = 514.0 / 2048.0;

/// Owns the embedded font faces, the glyph cache and the atlas.
///
/// One instance lives for the lifetime of the UI (inside the
/// [`Ui`](crate::ui::Ui) context). It is deliberately independent of any GPU
/// object: the only thing it produces for the renderer is a [`TextureUpload`]
/// for the atlas.
pub struct Fonts {
    /// One optional face per weight/icon slot; `None` if it failed to parse.
    faces: [Option<Font>; 6],
    metrics: HashMap<GlyphKey, GlyphMetrics>,
    slots: HashMap<GlyphKey, atlas::AtlasSlot>,
    atlas: GlyphAtlas,
    scale: f32,
    rasterized: u64,
    overflowed: bool,
}

impl Fonts {
    /// Load the embedded fonts.
    ///
    /// Embedded font data can only fail to parse if the build baked in corrupt
    /// assets; that is treated as a missing face (logged, not fatal). See
    /// [`load_font`] for the fallback behaviour.
    pub fn new() -> Self {
        let faces = [
            load_font(embedded::INTER_REGULAR),
            load_font(embedded::INTER_MEDIUM),
            load_font(embedded::INTER_SEMIBOLD),
            load_font(embedded::INTER_BOLD),
            load_font(embedded::ICONS_FILLED),
            load_font(embedded::ICONS_OUTLINED),
        ];
        if faces.iter().any(Option::is_none) {
            tracing::warn!(
                "one or more embedded fonts failed to parse; text/icon rendering is degraded"
            );
        }
        Self {
            faces,
            metrics: HashMap::new(),
            slots: HashMap::new(),
            atlas: GlyphAtlas::new(),
            scale: 1.0,
            rasterized: 0,
            overflowed: false,
        }
    }

    /// Update the DPI scale (physical pixels per logical pixel). Cached glyphs
    /// are rasterised at the new physical size, so text stays sharp after a
    /// monitor change.
    pub fn set_scale(&mut self, scale: f32) {
        let scale = if scale.is_finite() && scale > 0.0 {
            scale
        } else {
            1.0
        };
        if (scale - self.scale).abs() > f32::EPSILON {
            self.scale = scale;
            self.metrics.clear();
            self.slots.clear();
            self.atlas = GlyphAtlas::new();
        }
    }

    pub fn scale(&self) -> f32 {
        self.scale
    }

    pub fn atlas(&self) -> &GlyphAtlas {
        &self.atlas
    }

    /// Number of glyphs rasterised since construction (diagnostics/tests).
    pub fn rasterized_glyphs(&self) -> u64 {
        self.rasterized
    }

    /// Whether any glyph failed to fit the atlas this frame.
    pub fn overflowed(&self) -> bool {
        self.overflowed
    }

    /// Start-of-frame housekeeping: grow the atlas if a glyph did not fit last
    /// frame. Growing invalidates all cached UVs, so the slot cache is dropped
    /// (metrics are kept and cheap to reuse).
    pub fn begin_frame(&mut self) {
        if self.atlas.growth_pending() && self.atlas.grow() {
            self.slots.clear();
        }
        self.overflowed = false;
    }

    /// Build the atlas upload for the renderer if its pixels changed. The caller
    /// (`Ui`) turns this into `PaintList::add_texture` / `update_texture`.
    pub fn take_atlas_update(&mut self, id: TextureId) -> Option<TextureUpload> {
        self.atlas.dirty()?;
        self.atlas.take_dirty();
        let (w, h) = self.atlas.size();
        Some(TextureUpload::new(id, w, h, self.atlas.pixels().to_vec()))
    }

    // ------------------------------------------------------------ metrics

    fn physical_px(&self, style: &TextStyle) -> u32 {
        ((style.size_px * self.scale).round().max(1.0) as u32).min(u16::MAX as u32)
    }

    fn face_for(&self, style: &TextStyle) -> u8 {
        style.weight.face_index()
    }

    /// The requested face, or the first face that loaded.
    ///
    /// Falling back to another weight (rather than rendering nothing) keeps text
    /// usable if a single embedded face is ever missing; `None` only when no face
    /// loaded at all, in which case callers degrade to glyph-less layout.
    fn font(&self, face: u8) -> Option<&Font> {
        self.faces
            .get(face as usize)
            .and_then(Option::as_ref)
            .or_else(|| self.faces.iter().find_map(Option::as_ref))
    }

    fn glyph_index(&self, face: u8, ch: char) -> u16 {
        self.font(face)
            .map(|font| font.lookup_glyph_index(ch))
            .unwrap_or(0)
    }

    fn glyph_metrics(&mut self, face: u8, glyph: u16, px: u32) -> Option<GlyphMetrics> {
        let key = GlyphKey {
            face,
            glyph,
            px: px as u16,
        };
        if let Some(m) = self.metrics.get(&key) {
            return Some(*m);
        }
        let fpx = px as f32;
        let m = self.font(face)?.metrics_indexed(glyph, fpx);
        let g = GlyphMetrics {
            advance: m.advance_width,
            xmin: m.xmin as f32,
            ymin: m.ymin as f32,
            w: m.width as f32,
            h: m.height as f32,
        };
        self.metrics.insert(key, g);
        Some(g)
    }

    /// Advance width of one character in logical px (unknown glyphs advance like
    /// a space so text does not collapse).
    fn char_advance(&mut self, face: u8, ch: char, px: u32) -> f32 {
        let fallback = px as f32 * 0.25;
        let glyph = self.glyph_index(face, ch);
        let advance = if glyph == 0 {
            let space = self.glyph_index(face, ' ');
            if space == 0 {
                fallback
            } else {
                self.glyph_metrics(face, space, px)
                    .map(|m| m.advance)
                    .unwrap_or(fallback)
            }
        } else {
            self.glyph_metrics(face, glyph, px)
                .map(|m| m.advance)
                .unwrap_or(fallback)
        };
        advance / self.scale
    }

    /// Logical line metrics of the style's face.
    pub fn line_metrics(&self, style: &TextStyle) -> LineMetrics {
        let px = self.physical_px(style) as f32;
        let face = self.face_for(style);
        match self
            .font(face)
            .and_then(|font| font.horizontal_line_metrics(px))
        {
            Some(lm) => LineMetrics {
                ascent: lm.ascent / self.scale,
                descent: lm.descent / self.scale,
                line_height: lm.new_line_size / self.scale,
            },
            None => LineMetrics {
                ascent: px / self.scale * 0.9,
                descent: -px / self.scale * 0.25,
                line_height: px / self.scale * 1.2,
            },
        }
    }

    /// The logical line pitch this style lays out with.
    pub fn line_height(&self, style: &TextStyle) -> f32 {
        if style.line_height > 0.0 {
            style.line_height
        } else {
            self.line_metrics(style).line_height
        }
    }

    /// Width of `text` on a single line, without wrapping (newlines are treated
    /// as zero-width). No glyphs are rasterised.
    pub fn line_width(&mut self, text: &str, style: &TextStyle) -> f32 {
        let face = self.face_for(style);
        let px = self.physical_px(style);
        let mut w = 0.0;
        let mut n = 0usize;
        for ch in text.chars() {
            if ch == '\n' || ch == '\r' {
                continue;
            }
            w += self.char_advance(face, ch, px) + style.letter_spacing;
            n += 1;
        }
        if n > 0 {
            w -= style.letter_spacing;
        }
        w.max(0.0)
    }

    // -------------------------------------------------------------- layout

    fn ensure_slot(
        &mut self,
        face: u8,
        glyph: u16,
        px: u32,
        m: GlyphMetrics,
    ) -> Option<(atlas::AtlasSlot, [f32; 4])> {
        let key = GlyphKey {
            face,
            glyph,
            px: px as u16,
        };
        if let Some(slot) = self.slots.get(&key) {
            return Some((*slot, self.atlas.uv(*slot)));
        }
        if m.w <= 0.0 || m.h <= 0.0 {
            return None;
        }
        let (rm, coverage) = self.font(face)?.rasterize_indexed(glyph, px as f32);
        if rm.width == 0 || rm.height == 0 || coverage.is_empty() {
            return None;
        }
        let slot = match self.atlas.allocate(rm.width as u32, rm.height as u32) {
            Some(slot) => slot,
            None => {
                self.overflowed = true;
                return None;
            }
        };
        self.atlas.upload(slot, &coverage);
        self.slots.insert(key, slot);
        self.rasterized += 1;
        Some((slot, self.atlas.uv(slot)))
    }

    /// Shape `text` into `out`. `max_width` of `Some(w)` wraps, `None` only
    /// breaks on explicit newlines.
    pub fn shape_into(
        &mut self,
        text: &str,
        style: &TextStyle,
        max_width: Option<f32>,
        out: &mut TextShape,
    ) {
        out.clear();
        let face = self.face_for(style);
        let px = self.physical_px(style);
        let lm = self.line_metrics(style);
        let line_height = if style.line_height > 0.0 {
            style.line_height
        } else {
            lm.line_height
        };
        // CSS half-leading inside the explicit line box (see REF_* ratios).
        let baseline = if style.line_height > 0.0 {
            let ascent = REF_ASCENT_RATIO * style.size_px;
            let descent = REF_DESCENT_RATIO * style.size_px;
            (line_height - (ascent + descent)) * 0.5 + ascent
        } else {
            lm.ascent
        };

        let mut ranges: Vec<Range<usize>> = Vec::with_capacity(4);
        match max_width {
            Some(w) if w.is_finite() && w > 0.0 => {
                self.wrap_ranges(text, style, w, &mut ranges);
            }
            _ => split_newlines(text, &mut ranges),
        }

        let mut max_w: f32 = 0.0;
        for (line_idx, range) in ranges.iter().enumerate() {
            let line = &text[range.clone()];
            let y0 = line_idx as f32 * line_height;
            let mut pen = 0.0f32;
            for ch in line.chars() {
                if ch == '\n' || ch == '\r' {
                    continue;
                }
                let glyph = self.glyph_index(face, ch);
                if glyph == 0 {
                    pen += self.char_advance(face, ch, px) + style.letter_spacing;
                    continue;
                }
                let Some(m) = self.glyph_metrics(face, glyph, px) else {
                    // No font face loaded at all: keep advances consistent
                    // without emitting glyphs.
                    pen += self.char_advance(face, ch, px) + style.letter_spacing;
                    continue;
                };
                if m.w > 0.0 && m.h > 0.0 {
                    if let Some((_slot, uv)) = self.ensure_slot(face, glyph, px, m) {
                        let gx = pen + m.xmin / self.scale;
                        let gy = y0 + baseline - (m.ymin + m.h) / self.scale;
                        out.glyphs.push(ShapedGlyph {
                            x: gx,
                            y: gy,
                            w: m.w / self.scale,
                            h: m.h / self.scale,
                            uv,
                        });
                    }
                }
                pen += m.advance / self.scale + style.letter_spacing;
            }
            if pen > 0.0 {
                pen -= style.letter_spacing;
            }
            max_w = max_w.max(pen);
        }

        out.lines = ranges.len().max(1) as u32;
        out.size = Size::new(max_w, line_height * out.lines as f32);
        out.baseline = baseline;
    }

    /// Shape a block, wrapping at `max_width`.
    pub fn layout_wrapped(&mut self, text: &str, style: &TextStyle, max_width: f32) -> TextShape {
        let mut shape = TextShape::new();
        self.shape_into(text, style, Some(max_width), &mut shape);
        shape
    }

    /// Shape without wrapping (explicit newlines are honoured).
    pub fn layout(&mut self, text: &str, style: &TextStyle) -> TextShape {
        let mut shape = TextShape::new();
        self.shape_into(text, style, None, &mut shape);
        shape
    }

    // ------------------------------------------------------------ measuring

    /// Size of a text block (no wrapping; newlines honoured).
    pub fn measure(&mut self, text: &str, style: &TextStyle) -> Size {
        let mut ranges = Vec::with_capacity(4);
        split_newlines(text, &mut ranges);
        self.measure_ranges(text, style, &ranges)
    }

    /// Size of a wrapped text block.
    pub fn measure_wrapped(&mut self, text: &str, style: &TextStyle, max_width: f32) -> Size {
        if !max_width.is_finite() || max_width <= 0.0 {
            return self.measure(text, style);
        }
        let mut ranges = Vec::with_capacity(4);
        self.wrap_ranges(text, style, max_width, &mut ranges);
        self.measure_ranges(text, style, &ranges)
    }

    fn measure_ranges(&mut self, text: &str, style: &TextStyle, ranges: &[Range<usize>]) -> Size {
        let mut max_w: f32 = 0.0;
        for r in ranges {
            max_w = max_w.max(self.line_width(&text[r.clone()], style));
        }
        let lines = ranges.len().max(1) as f32;
        Size::new(max_w, self.line_height(style) * lines)
    }

    /// Greedy word wrap. Returns byte ranges into `text`, one per rendered line,
    /// with trailing whitespace trimmed from each line and explicit newlines
    /// treated as hard breaks. Very long words are broken between characters.
    pub fn wrap_ranges(
        &mut self,
        text: &str,
        style: &TextStyle,
        max_width: f32,
        out: &mut Vec<Range<usize>>,
    ) {
        out.clear();
        let face = self.face_for(style);
        let px = self.physical_px(style);
        let spacing = style.letter_spacing;

        // Split into paragraphs at hard newlines; an empty paragraph is a blank
        // line, which keeps the block height right for trailing newlines.
        let mut paragraphs: Vec<Range<usize>> = Vec::with_capacity(2);
        let mut para_start = 0usize;
        for (i, ch) in text.char_indices() {
            if ch == '\n' {
                let mut end = i;
                if end > para_start && text.as_bytes()[end - 1] == b'\r' {
                    end -= 1;
                }
                paragraphs.push(para_start..end);
                para_start = i + ch.len_utf8();
            }
        }
        paragraphs.push(para_start..text.len());

        for para in paragraphs {
            let p = &text[para.clone()];
            if p.is_empty() {
                out.push(para.start..para.start);
                continue;
            }
            let mut line_start = para.start;
            let mut line_w = 0.0f32;
            let mut line_has_word = false;
            let mut i = para.start;
            while i < para.end {
                // Consume whitespace between words, remembering where the gap
                // starts: only that gap is charged to the line, not the whole
                // line prefix (which `line_w` already contains).
                let ws_start = i;
                while i < para.end {
                    let Some(c) = text[i..].chars().next() else {
                        break;
                    };
                    if c.is_whitespace() {
                        i += c.len_utf8();
                    } else {
                        break;
                    }
                }
                if i >= para.end {
                    break;
                }
                let word_start = i;
                while i < para.end {
                    let Some(c) = text[i..].chars().next() else {
                        break;
                    };
                    if c.is_whitespace() {
                        break;
                    }
                    i += c.len_utf8();
                }
                let word_end = i;
                let word_w = self.advance_str(text, word_start, word_end, style);
                let sep_w = if line_has_word {
                    self.advance_str(text, ws_start.min(word_start), word_start, style)
                } else {
                    0.0
                };
                if line_has_word && line_w + sep_w + word_w > max_width {
                    // The word does not fit: flush the current line (trailing
                    // whitespace trimmed) and start this word on a fresh one.
                    let end = trim_end(text, line_start, ws_start);
                    out.push(line_start..end);
                    line_start = word_start;
                    line_w = 0.0;
                    line_has_word = false;
                }
                if !line_has_word {
                    // Drop leading/interior whitespace from the line's range.
                    line_start = word_start;
                    line_w = 0.0;
                }
                // Hard-break a single word wider than a whole line.
                if word_w > max_width {
                    let mut best_start = word_start;
                    let mut best_w = 0.0f32;
                    let mut iter = word_start;
                    while iter < word_end {
                        let Some(c) = text[iter..].chars().next() else {
                            break;
                        };
                        let cw = self.char_advance(face, c, px) + spacing;
                        if best_w > 0.0 && best_w + cw > max_width {
                            out.push(best_start..iter);
                            best_start = iter;
                            best_w = cw;
                        } else {
                            best_w += cw;
                        }
                        iter += c.len_utf8();
                    }
                    line_start = best_start;
                    line_w = best_w;
                } else {
                    line_w += sep_w + word_w;
                }
                line_has_word = true;
            }
            if line_has_word || line_start <= para.end {
                let end = trim_end(text, line_start, para.end);
                out.push(line_start..end);
            }
        }
    }

    fn advance_str(&mut self, text: &str, start: usize, end: usize, style: &TextStyle) -> f32 {
        let s = &text[start..end];
        self.line_width(s, style).max(0.0)
    }

    /// Truncate `text` to at most `max_width` logical px, appending `…` when it
    /// does not fit. Returns the input untouched (no allocation) when it fits.
    pub fn ellipsize<'a>(
        &mut self,
        text: &'a str,
        style: &TextStyle,
        max_width: f32,
    ) -> Cow<'a, str> {
        if !max_width.is_finite() || max_width <= 0.0 {
            return Cow::Owned(String::new());
        }
        let newline = text.find('\n');
        if newline.is_none() && self.line_width(text, style) <= max_width {
            return Cow::Borrowed(text);
        }
        let first_end = newline.unwrap_or(text.len());
        let first = &text[..first_end];
        let ell_w = self.line_width("\u{2026}", style);
        if ell_w > max_width {
            return Cow::Owned(String::new());
        }
        let target = max_width - ell_w;
        // Binary search the longest prefix (on a char boundary) that fits.
        let mut lo = 0usize;
        let mut hi = first.len();
        while lo < hi {
            let mid = lo + (hi - lo).div_ceil(2);
            let mid = floor_char_boundary(first, mid);
            if mid <= lo {
                break;
            }
            if self.line_width(&first[..mid], style) <= target {
                lo = mid;
            } else {
                hi = mid.saturating_sub(1);
            }
        }
        let cut = trim_end(first, 0, lo);
        let mut s = String::with_capacity(cut + 3);
        s.push_str(&first[..cut]);
        s.push('\u{2026}');
        Cow::Owned(s)
    }

    /// Truncate a `String` in place. Convenience for state that owns text.
    pub fn ellipsize_owned(&mut self, text: &mut String, style: &TextStyle, max_width: f32) {
        let shaped: Cow<'_, str> = self.ellipsize(text.as_str(), style, max_width);
        if let Cow::Owned(s) = shaped {
            *text = s;
        }
    }

    /// Word-wrap into a new `String` with `\n` inserted.
    pub fn wrap_text(&mut self, text: &str, style: &TextStyle, max_width: f32) -> String {
        let mut ranges = Vec::new();
        self.wrap_ranges(text, style, max_width, &mut ranges);
        let mut s = String::with_capacity(text.len() + ranges.len());
        for (i, r) in ranges.iter().enumerate() {
            if i > 0 {
                s.push('\n');
            }
            s.push_str(&text[r.clone()]);
        }
        s
    }

    // --------------------------------------------------------------- icons

    fn icon_face(&self, outlined: bool) -> u8 {
        if outlined {
            FACE_ICONS_OUTLINED
        } else {
            FACE_ICONS_FILLED
        }
    }

    /// Shape one icon glyph for a box of `box_px` logical pixels.
    ///
    /// Material Icons are drawn on a 24px em with ~20px of ink, exactly like the
    /// inline SVGs in the old web UI: rendering the font at `box_px` reproduces
    /// the SVG's `width: 16px` sizing. The returned quad is the ink's bounding
    /// box in logical px; callers centre it inside the icon box.
    pub fn icon_glyph(
        &mut self,
        codepoint: char,
        box_px: f32,
        outlined: bool,
    ) -> Option<ShapedGlyph> {
        let face = self.icon_face(outlined);
        let px = ((box_px * self.scale).round().max(1.0) as u32).min(u16::MAX as u32);
        let glyph = self.glyph_index(face, codepoint);
        if glyph == 0 {
            return None;
        }
        let m = self.glyph_metrics(face, glyph, px)?;
        let (_, uv) = self.ensure_slot(face, glyph, px, m)?;
        Some(ShapedGlyph {
            x: 0.0,
            y: 0.0,
            w: m.w / self.scale,
            h: m.h / self.scale,
            uv,
        })
    }

    /// Logical advance width of an icon at `box_px` (the em advance).
    pub fn icon_advance(&mut self, codepoint: char, box_px: f32, outlined: bool) -> f32 {
        let face = self.icon_face(outlined);
        let px = ((box_px * self.scale).round().max(1.0) as u32).min(u16::MAX as u32);
        let glyph = self.glyph_index(face, codepoint);
        if glyph == 0 {
            return 0.0;
        }
        self.glyph_metrics(face, glyph, px)
            .map(|m| m.advance / self.scale)
            .unwrap_or(0.0)
    }

    /// Whether the font actually has this codepoint.
    pub fn has_glyph(&self, ch: char, outlined: bool) -> bool {
        self.glyph_index(self.icon_face(outlined), ch) != 0
    }

    /// Rasterise icons up-front so the first frame does not hitch on atlas work.
    pub fn prewarm_icons(&mut self, codepoints: &[char], box_sizes: &[f32]) {
        for &size in box_sizes {
            for &cp in codepoints {
                let _ = self.icon_glyph(cp, size, false);
                let _ = self.icon_glyph(cp, size, true);
            }
        }
    }

    /// Rasterise printable ASCII for one style (tests and the loading screen).
    pub fn prewarm_ascii(&mut self, style: &TextStyle) {
        let mut shape = TextShape::new();
        self.shape_into(
            "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789 .,:;!?/\\|-_()[]{}<>+=*&%$#@'\"`~^",
            style,
            None,
            &mut shape,
        );
    }
}

impl Default for Fonts {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

/// Split `text` at explicit newlines into per-line ranges (empty lines kept).
fn split_newlines(text: &str, out: &mut Vec<Range<usize>>) {
    out.clear();
    let mut start = 0usize;
    for (i, ch) in text.char_indices() {
        if ch == '\n' {
            let mut end = i;
            if end > start && text.as_bytes()[end - 1] == b'\r' {
                end -= 1;
            }
            out.push(start..end);
            start = i + 1;
        }
    }
    out.push(start..text.len());
}

/// Largest char boundary `<= i` in `s`.
fn floor_char_boundary(s: &str, mut i: usize) -> usize {
    if i >= s.len() {
        return s.len();
    }
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Move `end` back over ASCII whitespace (never below `start`).
fn trim_end(text: &str, start: usize, mut end: usize) -> usize {
    while end > start && matches!(text.as_bytes()[end - 1], b' ' | b'\t' | b'\n' | b'\r') {
        end -= 1;
    }
    end.max(start)
}
