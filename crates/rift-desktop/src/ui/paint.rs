//! The paint contract re-exported for the UI layer.
//!
//! `crates/rift-desktop/src/gfx/paint.rs` (Lead-owned, frozen) is the single source of truth for
//! the primitive types the renderer consumes. Everything under `ui/` imports
//! them from here so there is exactly one path to the contract:
//!
//! ```ignore
//! use crate::ui::paint::{Color, PaintList, Prim, Rect, TextureId};
//! ```
//!
//! The UI layer also owns the *texture id space*: ids below
//! [`FIRST_IMAGE_TEXTURE_ID`] are reserved for framework textures (the glyph
//! atlas uses [`ATLAS_TEXTURE_ID`]), cover art and other app textures start at
//! [`FIRST_IMAGE_TEXTURE_ID`].

pub use crate::gfx::paint::{
    Color, Layer, PaintList, Prim, Rect, TextureId, TextureUpload, NO_TEXTURE,
};

/// Glyph atlas texture id (one texture holds every UI glyph and icon).
pub const ATLAS_TEXTURE_ID: TextureId = crate::text::ATLAS_TEXTURE_ID;

/// First texture id available to the application (cover art, avatars, …).
pub const FIRST_IMAGE_TEXTURE_ID: TextureId = 1024;

/// Total width of the scrollbar gutter in logical px (`::-webkit-scrollbar`).
pub const SCROLLBAR_WIDTH: f32 = 10.0;

/// The web UI's scrollbar thumb colors (`::-webkit-scrollbar-thumb`).
pub const SCROLLBAR_THUMB: Color = Color::hex(0x2a2a30);
pub const SCROLLBAR_THUMB_HOVER: Color = Color::hex(0x3a3a42);
