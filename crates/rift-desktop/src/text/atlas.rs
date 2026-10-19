//! A packed single-texture glyph atlas.
//!
//! Every rasterized glyph (UI text and icons alike) is blitted into one RGBA8
//! image. Glyph coverage is stored in the alpha channel with white RGB, which
//! makes the atlas usable as a plain tinted [`Prim::Image`](crate::gfx::paint::Prim)
//! on the renderer side: `tint * texel` yields the colored, antialiased glyph.
//!
//! Packing is shelf-based: glyphs of a given pixel size are allocated
//! left-to-right on horizontal shelves, which is close to optimal for text
//! (glyph heights within one size are near-identical) and costs no per-glyph
//! bookkeeping beyond the slot itself.
//!
//! Growth is deliberately *between frames*: when the atlas fills up it records
//! `growth_pending`, [`Fonts`](super::Fonts) rebuilds it at the start of the next
//! frame, and glyphs that do not fit in the meantime are simply skipped for one
//! frame. That keeps the UVs already handed to the paint list valid for the
//! whole frame.

/// Width/height (logical pixels at scale 1) the atlas is allocated at.
pub const INITIAL_SIZE: u32 = 1024;
/// The atlas never grows past this; at that point the UI has far more distinct
/// glyphs than any Rift screen uses.
pub const MAX_SIZE: u32 = 2048;
/// Padding between glyphs so linear filtering cannot bleed between neighbours.
const PAD: u32 = 1;

/// A half-open integer pixel rectangle in the atlas.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IRect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

impl IRect {
    pub const fn new(x: u32, y: u32, w: u32, h: u32) -> Self {
        Self { x, y, w, h }
    }

    pub const fn is_empty(&self) -> bool {
        self.w == 0 || self.h == 0
    }

    /// Union of two rectangles; empty rectangles are ignored.
    pub fn union(&self, other: IRect) -> IRect {
        if self.is_empty() {
            return other;
        }
        if other.is_empty() {
            return *self;
        }
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        let r = (self.x + self.w).max(other.x + other.w);
        let b = (self.y + self.h).max(other.y + other.h);
        IRect::new(x, y, r - x, b - y)
    }
}

/// Where a glyph's bitmap lives in the atlas, in atlas pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AtlasSlot {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

/// The CPU-side atlas image.
pub struct GlyphAtlas {
    width: u32,
    height: u32,
    /// RGBA8, row-major, `width * height * 4` bytes.
    data: Vec<u8>,
    /// Current shelf's vertical extent and the next free x within it.
    shelf_y: u32,
    shelf_height: u32,
    shelf_x: u32,
    /// Region modified since the renderer last uploaded the atlas.
    dirty: Option<IRect>,
    /// Bumped every time pixels move in a way that invalidates all cached UVs
    /// (i.e. on growth). The renderer re-uploads when it changes.
    generation: u64,
    /// A glyph did not fit; the owner should grow at the next frame boundary.
    growth_pending: bool,
    /// Statistics used by tests and the debug overlay.
    allocations: u64,
    rebuilds: u32,
    skipped: u64,
}

impl GlyphAtlas {
    pub fn new() -> Self {
        Self {
            width: INITIAL_SIZE,
            height: INITIAL_SIZE,
            data: vec![0; (INITIAL_SIZE * INITIAL_SIZE * 4) as usize],
            shelf_y: 0,
            shelf_height: 0,
            shelf_x: 0,
            dirty: Some(IRect::new(0, 0, INITIAL_SIZE, INITIAL_SIZE)),
            generation: 1,
            growth_pending: false,
            allocations: 0,
            rebuilds: 0,
            skipped: 0,
        }
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// The whole atlas image, RGBA8, straight alpha.
    pub fn pixels(&self) -> &[u8] {
        &self.data
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn growth_pending(&self) -> bool {
        self.growth_pending
    }

    pub fn allocations(&self) -> u64 {
        self.allocations
    }

    pub fn rebuild_count(&self) -> u32 {
        self.rebuilds
    }

    pub fn skipped(&self) -> u64 {
        self.skipped
    }

    /// Take the dirty region for an upload. Returns `None` when nothing changed
    /// since the last call.
    pub fn take_dirty(&mut self) -> Option<IRect> {
        self.dirty.take()
    }

    /// The dirty region without clearing it.
    pub fn dirty(&self) -> Option<IRect> {
        self.dirty
    }

    /// Reserved for the frame-boundary growth path in [`Fonts`](super::Fonts).
    pub fn request_growth(&mut self) {
        self.growth_pending = true;
    }

    /// Rebuild the atlas one size step larger. All cached UVs become invalid;
    /// the caller must drop its glyph cache. Returns `false` when the atlas is
    /// already at [`MAX_SIZE`].
    pub fn grow(&mut self) -> bool {
        if self.width >= MAX_SIZE {
            self.growth_pending = false;
            return false;
        }
        let next = (self.width * 2).min(MAX_SIZE);
        self.width = next;
        self.height = next;
        self.data = vec![0; (next * next * 4) as usize];
        self.shelf_y = 0;
        self.shelf_height = 0;
        self.shelf_x = 0;
        self.dirty = Some(IRect::new(0, 0, next, next));
        self.generation += 1;
        self.growth_pending = false;
        self.rebuilds += 1;
        true
    }

    /// Normalized UV rectangle `[u0, v0, u1, v1]` for a slot.
    pub fn uv(&self, slot: AtlasSlot) -> [f32; 4] {
        let w = self.width as f32;
        let h = self.height as f32;
        [
            slot.x as f32 / w,
            slot.y as f32 / h,
            (slot.x + slot.w) as f32 / w,
            (slot.y + slot.h) as f32 / h,
        ]
    }

    /// Allocate `w * h` pixels (plus padding) and return the slot. When the
    /// atlas is full the shelf packer reports failure; the owner either grows at
    /// the next frame boundary or skips the glyph.
    pub fn allocate(&mut self, w: u32, h: u32) -> Option<AtlasSlot> {
        if w == 0 || h == 0 {
            return None;
        }
        let pw = w + PAD;
        let ph = h + PAD;
        if pw > self.width {
            self.growth_pending = true;
            return None;
        }
        if self.shelf_x + pw > self.width {
            // Start a new shelf.
            self.shelf_y += self.shelf_height;
            self.shelf_x = 0;
            self.shelf_height = 0;
        }
        if self.shelf_y + ph > self.height || self.shelf_height.max(ph) + self.shelf_y > self.height
        {
            self.growth_pending = true;
            return None;
        }
        let slot = AtlasSlot {
            x: self.shelf_x,
            y: self.shelf_y,
            w,
            h,
        };
        self.shelf_x += pw;
        self.shelf_height = self.shelf_height.max(ph);
        self.allocations += 1;
        Some(slot)
    }

    /// Blit a single-channel coverage bitmap into an allocated slot.
    pub fn upload(&mut self, slot: AtlasSlot, coverage: &[u8]) {
        debug_assert_eq!(coverage.len(), (slot.w * slot.h) as usize);
        for row in 0..slot.h as usize {
            let src = &coverage[row * slot.w as usize..(row + 1) * slot.w as usize];
            let dst_start = ((slot.y as usize + row) * self.width as usize + slot.x as usize) * 4;
            let dst = &mut self.data[dst_start..dst_start + slot.w as usize * 4];
            for (px, cov) in dst.chunks_exact_mut(4).zip(src.iter().copied()) {
                px[0] = 255;
                px[1] = 255;
                px[2] = 255;
                px[3] = cov;
            }
        }
        let r = IRect::new(slot.x, slot.y, slot.w, slot.h);
        self.dirty = Some(match self.dirty {
            Some(d) => d.union(r),
            None => r,
        });
    }

    /// Blit a whole RGBA8 white-coverage image (used by tests/tools).
    pub fn clear(&mut self) {
        self.data.fill(0);
        self.shelf_y = 0;
        self.shelf_height = 0;
        self.shelf_x = 0;
        self.dirty = Some(IRect::new(0, 0, self.width, self.height));
    }
}

impl Default for GlyphAtlas {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocate_does_not_overlap() {
        let mut a = GlyphAtlas::new();
        let s1 = a.allocate(10, 12).unwrap();
        a.upload(s1, &[255; 120]);
        let s2 = a.allocate(10, 12).unwrap();
        let r1 = IRect::new(s1.x, s1.y, s1.w, s1.h);
        let r2 = IRect::new(s2.x, s2.y, s2.w, s2.h);
        assert!(r1.x + r1.w <= r2.x || r1.y + r1.h <= r2.y || r2.x + r2.w <= r1.x);
        assert_eq!(a.allocations(), 2);
    }

    #[test]
    fn shelves_wrap_and_request_growth() {
        let mut a = GlyphAtlas::new();
        // Fill the first shelf, then many more, until the atlas reports full.
        let mut count = 0u32;
        while let Some(slot) = a.allocate(64, 64) {
            a.upload(slot, &vec![255u8; 64 * 64]);
            count += 1;
            assert!(count < 100_000);
        }
        assert!(a.growth_pending());
        assert!(count > 8);
        let gen = a.generation();
        assert!(a.grow());
        assert_eq!(a.size(), (INITIAL_SIZE * 2, INITIAL_SIZE * 2));
        assert!(a.generation() > gen);
        assert!(!a.growth_pending());
        // New UVs are relative to the new size.
        let slot = a.allocate(8, 8).unwrap();
        let uv = a.uv(slot);
        assert!(uv[2] <= 1.0 && uv[3] <= 1.0);
    }

    #[test]
    fn dirty_region_is_a_union() {
        let mut a = GlyphAtlas::new();
        assert!(a.take_dirty().is_some());
        let s1 = a.allocate(4, 4).unwrap();
        a.upload(s1, &[1; 16]);
        let s2 = a.allocate(4, 4).unwrap();
        a.upload(s2, &[1; 16]);
        let d = a.take_dirty().unwrap();
        assert!(d.w >= 8);
        assert!(a.take_dirty().is_none());
    }

    #[test]
    fn uv_is_normalized() {
        let mut a = GlyphAtlas::new();
        let s = a.allocate(8, 8).unwrap();
        let uv = a.uv(s);
        assert!((uv[2] - uv[0] - 8.0 / a.width() as f32).abs() < 1e-4);
        assert!((uv[3] - uv[1] - 8.0 / a.height() as f32).abs() < 1e-4);
    }
}
