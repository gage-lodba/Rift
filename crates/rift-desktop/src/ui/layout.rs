//! Layout helpers: geometry types and cursor-based row/column splits.
//!
//! Screens lay out against explicit logical-pixel rectangles; these helpers keep
//! that arithmetic readable and exact (the goal is a pixel match with the old
//! CSS). All values are logical px, origin top-left, `+y` down.

use crate::ui::paint::Rect;

pub use crate::text::Size;

/// A 2D vector in logical pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Vec2 {
    pub x: f32,
    pub y: f32,
}

impl Vec2 {
    pub const ZERO: Vec2 = Vec2 { x: 0.0, y: 0.0 };

    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }

    pub fn length(&self) -> f32 {
        (self.x * self.x + self.y * self.y).sqrt()
    }

    pub fn distance(&self, other: Vec2) -> f32 {
        (*self - other).length()
    }
}

impl From<(f32, f32)> for Vec2 {
    fn from((x, y): (f32, f32)) -> Self {
        Self::new(x, y)
    }
}

impl std::ops::Add for Vec2 {
    type Output = Vec2;
    fn add(self, rhs: Vec2) -> Vec2 {
        Vec2::new(self.x + rhs.x, self.y + rhs.y)
    }
}

impl std::ops::Sub for Vec2 {
    type Output = Vec2;
    fn sub(self, rhs: Vec2) -> Vec2 {
        Vec2::new(self.x - rhs.x, self.y - rhs.y)
    }
}

impl std::ops::Mul<f32> for Vec2 {
    type Output = Vec2;
    fn mul(self, rhs: f32) -> Vec2 {
        Vec2::new(self.x * rhs, self.y * rhs)
    }
}

/// Per-edge padding/margin.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Padding {
    pub left: f32,
    pub right: f32,
    pub top: f32,
    pub bottom: f32,
}

impl Padding {
    pub const ZERO: Padding = Padding {
        left: 0.0,
        right: 0.0,
        top: 0.0,
        bottom: 0.0,
    };

    pub const fn all(v: f32) -> Self {
        Self {
            left: v,
            right: v,
            top: v,
            bottom: v,
        }
    }

    pub const fn symmetric(horizontal: f32, vertical: f32) -> Self {
        Self {
            left: horizontal,
            right: horizontal,
            top: vertical,
            bottom: vertical,
        }
    }

    pub const fn xy(x: f32, y: f32) -> Self {
        Self::symmetric(x, y)
    }

    pub const fn vertical(v: f32) -> Self {
        Self {
            left: 0.0,
            right: 0.0,
            top: v,
            bottom: v,
        }
    }

    pub const fn horizontal(h: f32) -> Self {
        Self {
            left: h,
            right: h,
            top: 0.0,
            bottom: 0.0,
        }
    }

    pub fn total_width(&self) -> f32 {
        self.left + self.right
    }

    pub fn total_height(&self) -> f32 {
        self.top + self.bottom
    }
}

/// Horizontal alignment inside a box.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Align {
    #[default]
    Left,
    Center,
    Right,
}

/// Vertical alignment inside a box.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum VAlign {
    #[default]
    Top,
    Center,
    Bottom,
}

/// Extension methods on [`Rect`] used by the screens.
///
/// The base helpers (`split_top`, `split_left`, `inset`, `intersect`, …) are in
/// the frozen paint contract; these are the extra ergonomics the layout code
/// needs. Kept out of `gfx/paint.rs` on purpose.
impl Rect {
    pub fn left(&self) -> f32 {
        self.x
    }

    pub fn top(&self) -> f32 {
        self.y
    }

    pub fn pos(&self) -> Vec2 {
        Vec2::new(self.x, self.y)
    }

    pub fn size(&self) -> Size {
        Size::new(self.w, self.h)
    }

    pub fn center_v(&self) -> Vec2 {
        Vec2::new(self.x + self.w * 0.5, self.y + self.h * 0.5)
    }

    /// Split `amount` off the bottom, returning `(rest, bottom)`.
    pub fn split_bottom(&self, amount: f32) -> (Rect, Rect) {
        let amount = amount.clamp(0.0, self.h);
        (
            Rect::new(self.x, self.y, self.w, self.h - amount),
            Rect::new(self.x, self.y + self.h - amount, self.w, amount),
        )
    }

    /// Split `amount` off the top; alias of the contract helper for symmetry.
    pub fn split_vertical(&self, top: f32, bottom: f32) -> Rect {
        Rect::new(
            self.x,
            self.y + top,
            self.w,
            (self.h - top - bottom).max(0.0),
        )
    }

    /// Inset each edge by its own amount.
    pub fn inset_pad(&self, p: Padding) -> Rect {
        Rect::new(
            self.x + p.left,
            self.y + p.top,
            (self.w - p.left - p.right).max(0.0),
            (self.h - p.top - p.bottom).max(0.0),
        )
    }

    /// Grow every edge by `amount` (negative shrinks).
    pub fn expand(&self, amount: f32) -> Rect {
        Rect::new(
            self.x - amount,
            self.y - amount,
            (self.w + amount * 2.0).max(0.0),
            (self.h + amount * 2.0).max(0.0),
        )
    }

    pub fn offset(&self, dx: f32, dy: f32) -> Rect {
        Rect::new(self.x + dx, self.y + dy, self.w, self.h)
    }

    pub fn with_x(&self, x: f32) -> Rect {
        Rect::new(x, self.y, self.w, self.h)
    }

    pub fn with_y(&self, y: f32) -> Rect {
        Rect::new(self.x, y, self.w, self.h)
    }

    pub fn with_w(&self, w: f32) -> Rect {
        Rect::new(self.x, self.y, w, self.h)
    }

    pub fn with_h(&self, h: f32) -> Rect {
        Rect::new(self.x, self.y, self.w, h)
    }

    /// `N` equal-width columns separated by `gap` (remainder goes to the last).
    pub fn split_cols<const N: usize>(&self, gap: f32) -> [Rect; N] {
        let total_gap = gap * (N.saturating_sub(1)) as f32;
        let col_w = ((self.w - total_gap).max(0.0)) / N as f32;
        let mut out = [Rect::ZERO; N];
        for (i, slot) in out.iter_mut().enumerate() {
            let x = self.x + (col_w + gap) * i as f32;
            let w = if i + 1 == N {
                self.x + self.w - x
            } else {
                col_w
            };
            *slot = Rect::new(x, self.y, w.max(0.0), self.h);
        }
        out
    }

    /// `N` equal-height rows separated by `gap`.
    pub fn split_rows<const N: usize>(&self, gap: f32) -> [Rect; N] {
        let total_gap = gap * (N.saturating_sub(1)) as f32;
        let row_h = ((self.h - total_gap).max(0.0)) / N as f32;
        let mut out = [Rect::ZERO; N];
        for (i, slot) in out.iter_mut().enumerate() {
            let y = self.y + (row_h + gap) * i as f32;
            let h = if i + 1 == N {
                self.y + self.h - y
            } else {
                row_h
            };
            *slot = Rect::new(self.x, y, self.w, h.max(0.0));
        }
        out
    }

    /// A `size`-sized rectangle centred/aligned inside this one.
    pub fn align(&self, size: Size, h: Align, v: VAlign) -> Rect {
        let x = match h {
            Align::Left => self.x,
            Align::Center => self.x + (self.w - size.w) * 0.5,
            Align::Right => self.x + self.w - size.w,
        };
        let y = match v {
            VAlign::Top => self.y,
            VAlign::Center => self.y + (self.h - size.h) * 0.5,
            VAlign::Bottom => self.y + self.h - size.h,
        };
        Rect::new(x, y, size.w, size.h)
    }

    /// Shrink to the (possibly negative-clamped) overlap and clamp to zero size.
    pub fn clamped(&self) -> Rect {
        Rect::new(self.x, self.y, self.w.max(0.0), self.h.max(0.0))
    }
}

/// A cursor that hands out rectangles left-to-right inside a row.
///
/// ```ignore
/// let mut row = Row::new(rect, 8.0);
/// let icon = row.fixed(16.0);
/// let title = row.remaining();       // takes the rest
/// ```
#[derive(Clone, Debug)]
pub struct Row {
    rect: Rect,
    x: f32,
    gap: f32,
    spaced: bool,
}

impl Row {
    pub fn new(rect: Rect, gap: f32) -> Self {
        Self {
            rect,
            x: rect.x,
            gap,
            spaced: false,
        }
    }

    /// Total width available (for percentage math in callers).
    pub fn total_width(&self) -> f32 {
        self.rect.w
    }

    /// Width not yet allocated.
    pub fn remaining_width(&self) -> f32 {
        (self.rect.x + self.rect.w - self.x).max(0.0)
    }

    pub fn is_empty(&self) -> bool {
        self.remaining_width() <= 0.0
    }

    /// Insert the gap before the next cell.
    fn separate(&mut self) {
        if self.spaced {
            self.x += self.gap;
        }
    }

    fn advance(&mut self, w: f32) {
        self.x += w;
        self.spaced = true;
    }

    /// Take a fixed-width cell.
    pub fn fixed(&mut self, w: f32) -> Rect {
        self.separate();
        let w = w.min(self.remaining_width());
        let out = Rect::new(self.x, self.rect.y, w, self.rect.h);
        self.advance(w);
        out
    }

    /// Take a percentage of the row's total width.
    pub fn percent(&mut self, pct: f32) -> Rect {
        self.fixed(self.rect.w * pct)
    }

    /// Take everything that is left (no trailing gap).
    pub fn remaining(&mut self) -> Rect {
        self.separate();
        let w = self.remaining_width();
        let out = Rect::new(self.x, self.rect.y, w, self.rect.h);
        self.x = self.rect.x + self.rect.w;
        self.spaced = true;
        out
    }

    /// Skip `w` pixels of empty space.
    pub fn skip(&mut self, w: f32) {
        self.separate();
        self.advance(w.min(self.remaining_width()));
    }

    /// The full row rectangle, for painting a background behind it.
    pub fn rect(&self) -> Rect {
        self.rect
    }
}

/// A cursor that hands out rectangles top-to-bottom inside a column.
#[derive(Clone, Debug)]
pub struct Column {
    rect: Rect,
    y: f32,
    gap: f32,
    spaced: bool,
}

impl Column {
    pub fn new(rect: Rect, gap: f32) -> Self {
        Self {
            rect,
            y: rect.y,
            gap,
            spaced: false,
        }
    }

    pub fn total_height(&self) -> f32 {
        self.rect.h
    }

    pub fn remaining_height(&self) -> f32 {
        (self.rect.y + self.rect.h - self.y).max(0.0)
    }

    pub fn is_empty(&self) -> bool {
        self.remaining_height() <= 0.0
    }

    /// Insert the gap before the next cell.
    fn separate(&mut self) {
        if self.spaced {
            self.y += self.gap;
        }
    }

    fn advance(&mut self, h: f32) {
        self.y += h;
        self.spaced = true;
    }

    pub fn fixed(&mut self, h: f32) -> Rect {
        self.separate();
        let h = h.min(self.remaining_height());
        let out = Rect::new(self.rect.x, self.y, self.rect.w, h);
        self.advance(h);
        out
    }

    pub fn percent(&mut self, pct: f32) -> Rect {
        self.fixed(self.rect.h * pct)
    }

    pub fn remaining(&mut self) -> Rect {
        self.separate();
        let h = self.remaining_height();
        let out = Rect::new(self.rect.x, self.y, self.rect.w, h);
        self.y = self.rect.y + self.rect.h;
        self.spaced = true;
        out
    }

    pub fn skip(&mut self, h: f32) {
        self.separate();
        self.advance(h.min(self.remaining_height()));
    }

    pub fn rect(&self) -> Rect {
        self.rect
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inset_pad_uses_each_edge() {
        let r = Rect::new(10.0, 20.0, 100.0, 50.0);
        let padded = r.inset_pad(Padding {
            left: 5.0,
            right: 10.0,
            top: 2.0,
            bottom: 4.0,
        });
        assert_eq!(padded, Rect::new(15.0, 22.0, 85.0, 44.0));
    }

    #[test]
    fn splitting_columns_accounts_for_gaps() {
        let r = Rect::new(0.0, 0.0, 100.0, 40.0);
        let cols = r.split_cols::<3>(5.0);
        assert_eq!(cols.len(), 3);
        assert_eq!(cols[0], Rect::new(0.0, 0.0, 30.0, 40.0));
        assert_eq!(cols[1], Rect::new(35.0, 0.0, 30.0, 40.0));
        assert_eq!(cols[2].right(), 100.0);
        assert!((cols[2].w - 30.0).abs() < 1e-4);
    }

    #[test]
    fn row_cursor_respects_gaps_and_remaining() {
        let r = Rect::new(10.0, 0.0, 100.0, 20.0);
        let mut row = Row::new(r, 8.0);
        let a = row.fixed(20.0);
        assert_eq!(a, Rect::new(10.0, 0.0, 20.0, 20.0));
        let b = row.fixed(20.0);
        assert_eq!(b.x, 38.0, "gap is inserted between cells");
        let rest = row.remaining();
        assert_eq!(rest.right(), r.right());
        assert_eq!(rest.x, 66.0);
    }

    #[test]
    fn column_cursor_clamps_to_bounds() {
        let r = Rect::new(0.0, 10.0, 30.0, 50.0);
        let mut col = Column::new(r, 4.0);
        let a = col.fixed(30.0);
        assert_eq!(a.h, 30.0);
        let b = col.fixed(100.0);
        assert_eq!(b.y, 44.0);
        assert_eq!(b.h, 16.0);
        assert!(col.is_empty());
    }

    #[test]
    fn align_centers_size() {
        let r = Rect::new(0.0, 0.0, 100.0, 50.0);
        let out = r.align(Size::new(20.0, 10.0), Align::Center, VAlign::Center);
        assert_eq!(out, Rect::new(40.0, 20.0, 20.0, 10.0));
        let out = r.align(Size::new(20.0, 10.0), Align::Right, VAlign::Bottom);
        assert_eq!(out, Rect::new(80.0, 40.0, 20.0, 10.0));
    }

    #[test]
    fn vec2_math() {
        let a = Vec2::new(3.0, 4.0);
        assert!((a.length() - 5.0).abs() < 1e-6);
        assert_eq!(a + Vec2::new(1.0, 1.0), Vec2::new(4.0, 5.0));
        assert_eq!(Vec2::from((2.0, 3.0)), Vec2::new(2.0, 3.0));
    }
}
