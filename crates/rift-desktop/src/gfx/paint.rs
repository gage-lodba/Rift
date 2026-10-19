//! The painting contract between the immediate-mode GUI and the Vulkan renderer.
//!
//! The UI layer never touches Vulkan. Each frame it appends primitives to a
//! [`PaintList`]; the renderer turns that list into one batched draw pass. This
//! module is the single source of truth for both sides — keep the types in it
//! stable.
//!
//! Coordinates
//! -----------
//! Everything is in **logical pixels**, with the origin at the top-left of the
//! window and `+y` pointing down (exactly the CSS/IMGUI convention). The
//! renderer multiplies by its scale factor when it writes vertices, so the UI
//! can lay out against `logical_size()` and look identical at any DPI.
//!
//! Ordering
//! --------
//! Layers are painted in order; primitives inside a layer are painted in order
//! (painter's algorithm). A layer may carry a clip rectangle (a scissor), which
//! is how scroll containers, the queue panel and popups clip their contents.

/// A rectangle in logical pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub const ZERO: Rect = Rect {
        x: 0.0,
        y: 0.0,
        w: 0.0,
        h: 0.0,
    };

    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    pub const fn right(&self) -> f32 {
        self.x + self.w
    }

    pub const fn bottom(&self) -> f32 {
        self.y + self.h
    }

    pub fn center(&self) -> (f32, f32) {
        (self.x + self.w * 0.5, self.y + self.h * 0.5)
    }

    pub fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && py >= self.y && px < self.right() && py < self.bottom()
    }

    pub fn inset(&self, amount: f32) -> Rect {
        Rect {
            x: self.x + amount,
            y: self.y + amount,
            w: (self.w - amount * 2.0).max(0.0),
            h: (self.h - amount * 2.0).max(0.0),
        }
    }

    pub fn inset_xy(&self, dx: f32, dy: f32) -> Rect {
        Rect {
            x: self.x + dx,
            y: self.y + dy,
            w: (self.w - dx * 2.0).max(0.0),
            h: (self.h - dy * 2.0).max(0.0),
        }
    }

    pub fn translate(&self, dx: f32, dy: f32) -> Rect {
        Rect {
            x: self.x + dx,
            y: self.y + dy,
            ..*self
        }
    }

    /// The overlap of two rectangles (`ZERO` if they do not intersect).
    pub fn intersect(&self, other: Rect) -> Rect {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let r = self.right().min(other.right());
        let b = self.bottom().min(other.bottom());
        Rect {
            x,
            y,
            w: (r - x).max(0.0),
            h: (b - y).max(0.0),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.w <= 0.0 || self.h <= 0.0
    }

    /// Split `amount` logical pixels off the top, returning `(top, rest)`.
    pub fn split_top(&self, amount: f32) -> (Rect, Rect) {
        let amount = amount.clamp(0.0, self.h);
        (
            Rect::new(self.x, self.y, self.w, amount),
            Rect::new(self.x, self.y + amount, self.w, self.h - amount),
        )
    }

    /// Split `amount` logical pixels off the left, returning `(left, rest)`.
    pub fn split_left(&self, amount: f32) -> (Rect, Rect) {
        let amount = amount.clamp(0.0, self.w);
        (
            Rect::new(self.x, self.y, amount, self.h),
            Rect::new(self.x + amount, self.y, self.w - amount, self.h),
        )
    }

    /// Split `amount` logical pixels off the right, returning `(rest, right)`.
    pub fn split_right(&self, amount: f32) -> (Rect, Rect) {
        let amount = amount.clamp(0.0, self.w);
        (
            Rect::new(self.x, self.y, self.w - amount, self.h),
            Rect::new(self.x + self.w - amount, self.y, amount, self.h),
        )
    }
}

/// Straight-alpha sRGB color. The renderer expects straight (non-premultiplied)
/// alpha and converts to the swapchain's color space at draw time.
///
/// `Default` is fully transparent black, which is what a cleared [`PaintList`]
/// should start from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    pub const TRANSPARENT: Color = Color::rgba(0, 0, 0, 0);
    pub const WHITE: Color = Color::rgb(0xFF, 0xFF, 0xFF);
    pub const BLACK: Color = Color::rgb(0, 0, 0);

    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }

    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    /// `#RRGGBB` (alpha 255).
    pub const fn hex(hex: u32) -> Self {
        Self {
            r: ((hex >> 16) & 0xFF) as u8,
            g: ((hex >> 8) & 0xFF) as u8,
            b: (hex & 0xFF) as u8,
            a: 255,
        }
    }

    /// `#RRGGBBAA`.
    pub const fn hexa(hex: u32) -> Self {
        Self {
            r: ((hex >> 24) & 0xFF) as u8,
            g: ((hex >> 16) & 0xFF) as u8,
            b: ((hex >> 8) & 0xFF) as u8,
            a: (hex & 0xFF) as u8,
        }
    }

    pub const fn with_alpha(self, a: u8) -> Self {
        Self { a, ..self }
    }

    /// Multiply the alpha by `factor` (0..=1); useful for disabled states.
    pub fn fade(self, factor: f32) -> Self {
        let a = (self.a as f32 * factor.clamp(0.0, 1.0)).round();
        Self { a: a as u8, ..self }
    }

    pub fn to_array(self) -> [u8; 4] {
        [self.r, self.g, self.b, self.a]
    }

    pub fn from_array(v: [u8; 4]) -> Self {
        Self {
            r: v[0],
            g: v[1],
            b: v[2],
            a: v[3],
        }
    }
}

/// A handle to a GPU texture. `NO_TEXTURE` means "no sampling: use the vertex
/// color" and is what solid quads use. The UI layer allocates ids monotonically
/// (atlas = reserved low ids, images above); the renderer owns the actual GPU
/// resources and keeps them in sync from [`PaintList::created`] /
/// [`PaintList::dropped`].
pub type TextureId = u64;

/// Reserved: solid color / no texture.
pub const NO_TEXTURE: TextureId = 0;

/// A single drawable primitive.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Prim {
    /// A (rounded) rectangle, optionally with a border ring drawn inside its
    /// outer edge. `radius` may exceed half the smaller side (it is clamped).
    Rect {
        rect: Rect,
        color: Color,
        radius: f32,
        /// Border width in logical px; `0.0` = no border.
        border: f32,
        border_color: Color,
    },
    /// A textured rectangle (cover art, font-atlas glyph, icon glyph). `uv` is
    /// normalized 0..=1 texture space. `radius` clips the texture with the same
    /// antialiased rounded-rect coverage as `Rect`. `tint` multiplies the
    /// sampled texel (white = unchanged; used for icon/text colors on the
    /// white glyph atlas).
    Image {
        rect: Rect,
        texture: TextureId,
        uv: Rect,
        tint: Color,
        radius: f32,
    },
}

impl Prim {
    pub fn rect(rect: Rect, color: Color) -> Self {
        Prim::Rect {
            rect,
            color,
            radius: 0.0,
            border: 0.0,
            border_color: Color::TRANSPARENT,
        }
    }

    pub fn rounded(rect: Rect, color: Color, radius: f32) -> Self {
        Prim::Rect {
            rect,
            color,
            radius,
            border: 0.0,
            border_color: Color::TRANSPARENT,
        }
    }

    pub fn bordered(
        rect: Rect,
        color: Color,
        radius: f32,
        border: f32,
        border_color: Color,
    ) -> Self {
        Prim::Rect {
            rect,
            color,
            radius,
            border,
            border_color,
        }
    }

    pub fn image(rect: Rect, texture: TextureId, uv: Rect, tint: Color) -> Self {
        Prim::Image {
            rect,
            texture,
            uv,
            tint,
            radius: 0.0,
        }
    }

    /// The primitive's bounding box (before clipping).
    pub fn bounds(&self) -> Rect {
        match *self {
            Prim::Rect { rect, .. } => rect,
            Prim::Image { rect, .. } => rect,
        }
    }
}

/// One clipped group of primitives. The clip is applied as a scissor to every
/// primitive in the layer; `None` = no clipping.
#[derive(Clone, Debug, Default)]
pub struct Layer {
    pub clip: Option<Rect>,
    pub prims: Vec<Prim>,
}

impl Layer {
    pub fn new(clip: Option<Rect>) -> Self {
        Self {
            clip,
            prims: Vec::new(),
        }
    }

    pub fn push(&mut self, prim: Prim) {
        self.prims.push(prim);
    }
}

/// RGBA8 pixel data for a texture the UI created this frame. `rgba` is
/// `width * height * 4` bytes, straight alpha. The renderer uploads it before
/// drawing the frame.
#[derive(Clone, Debug, PartialEq)]
pub struct TextureUpload {
    pub id: TextureId,
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl TextureUpload {
    pub fn new(id: TextureId, width: u32, height: u32, rgba: Vec<u8>) -> Self {
        debug_assert_eq!(rgba.len(), width as usize * height as usize * 4);
        Self {
            id,
            width,
            height,
            rgba,
        }
    }
}

/// Everything the renderer needs to draw one frame.
#[derive(Clone, Debug, Default)]
pub struct PaintList {
    /// Window/attachment clear color.
    pub clear: Color,
    pub layers: Vec<Layer>,
    /// Textures created by the UI since the last frame (atlas, cover art, ...).
    pub created: Vec<TextureUpload>,
    /// Replacements for textures whose pixels changed (rare; e.g. atlas growth).
    pub updated: Vec<TextureUpload>,
    /// Textures the UI no longer references; the renderer releases them after
    /// this frame.
    pub dropped: Vec<TextureId>,
}

impl PaintList {
    pub fn new(clear: Color) -> Self {
        Self {
            clear,
            ..Self::default()
        }
    }

    /// Start a new clipping layer. Layers are drawn in push order.
    pub fn push_layer(&mut self, clip: Option<Rect>) {
        self.layers.push(Layer::new(clip));
    }

    /// Push a primitive into the current (last) layer, creating an unclipped
    /// layer if the list is empty.
    pub fn push(&mut self, prim: Prim) {
        if self.layers.is_empty() {
            self.layers.push(Layer::new(None));
        }
        if let Some(layer) = self.layers.last_mut() {
            layer.push(prim);
        }
    }

    pub fn add_texture(&mut self, texture: TextureUpload) {
        self.created.push(texture);
    }

    pub fn update_texture(&mut self, texture: TextureUpload) {
        self.updated.push(texture);
    }

    pub fn drop_texture(&mut self, id: TextureId) {
        self.dropped.push(id);
    }

    /// Whether anything at all would be drawn.
    pub fn is_empty(&self) -> bool {
        self.layers.iter().all(|l| l.prims.is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rect_split_helpers_respect_bounds() {
        let r = Rect::new(10.0, 20.0, 200.0, 50.0);
        let (top, rest) = r.split_top(20.0);
        assert_eq!(top, Rect::new(10.0, 20.0, 200.0, 20.0));
        assert_eq!(rest, Rect::new(10.0, 40.0, 200.0, 30.0));
        let (left, rest) = r.split_left(30.0);
        assert_eq!(left.w, 30.0);
        assert_eq!(rest.x, 40.0);
        // Splitting more than the side clamps instead of going negative.
        let (top, rest) = r.split_top(500.0);
        assert_eq!(top.h, 50.0);
        assert!(rest.is_empty());
    }

    #[test]
    fn intersect_and_contains() {
        let a = Rect::new(0.0, 0.0, 100.0, 100.0);
        let b = Rect::new(50.0, 50.0, 100.0, 100.0);
        assert_eq!(a.intersect(b), Rect::new(50.0, 50.0, 50.0, 50.0));
        assert!(a.contains(0.0, 0.0));
        assert!(!a.contains(100.0, 50.0));
        assert!(a.intersect(Rect::new(200.0, 0.0, 10.0, 10.0)).is_empty());
    }

    #[test]
    fn prims_go_into_the_last_layer() {
        let mut list = PaintList::new(Color::BLACK);
        list.push(Prim::rect(Rect::new(0.0, 0.0, 1.0, 1.0), Color::WHITE));
        list.push_layer(Some(Rect::new(0.0, 0.0, 10.0, 10.0)));
        list.push(Prim::rounded(
            Rect::new(0.0, 0.0, 2.0, 2.0),
            Color::WHITE,
            1.0,
        ));
        assert_eq!(list.layers.len(), 2);
        assert_eq!(list.layers[0].prims.len(), 1);
        assert_eq!(list.layers[1].prims.len(), 1);
    }
}
