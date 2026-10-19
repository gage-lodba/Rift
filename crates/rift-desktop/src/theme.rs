//! Design tokens for the Rift UI.
//!
//! These mirror the legacy stylesheet one-for-one, so the Vulkan-rendered UI
//! can be held to the same visual spec. Values in logical pixels; colors are
//! sRGB straight-alpha.
//!
//! The defaults below come from the root custom properties and the most
//! load-bearing rules in `styles.css`; screens may add tokens here as they
//! refine the pixel match. Keep every token named after its purpose, not its
//! value, so the theme stays swappable.

use crate::gfx::paint::Color;

/// A full set of tokens. `Theme::default()` is the Rift dark theme.
#[derive(Clone, Debug, PartialEq)]
pub struct Theme {
    // ---------------------------------------------------------- surfaces
    pub bg: Color,
    pub bg_raised: Color,
    pub bg_hover: Color,
    pub border: Color,
    pub titlebar_bg: Color,
    pub sidebar_bg: Color,
    pub player_bg: Color,
    pub queue_bg: Color,

    // -------------------------------------------------------------- text
    pub text: Color,
    pub text_dim: Color,

    // ------------------------------------------------------------ accent
    pub accent: Color,
    pub accent_dim: Color,
    pub danger: Color,
    pub focus: Color,

    // ---------------------------------------------------------- extras
    /// Scrollbar thumb (`::-webkit-scrollbar-thumb { background: #2a2a30 }`).
    pub scrollbar_thumb: Color,
    /// Fallback cover tile gradient (135deg).
    pub cover_gradient_start: Color,
    pub cover_gradient_end: Color,
    /// Error toast card (the `.toast.error` variant in styles.css).
    pub toast_bg: Color,
    pub toast_border: Color,
    pub toast_text: Color,
    /// Update banner card.
    pub banner_bg: Color,
    /// Seek bar: unplayed track (`--seek-track`).
    pub seek_track: Color,
    /// Seek bar: played portion / active volume (accent in the reference).
    pub seek_active: Color,

    // ---------------------------------------------------------- typography
    /// Base body size (`html, body { font-size: 14px }`).
    pub font_size: f32,
    pub font_size_small: f32,
    pub font_size_tiny: f32,
    pub font_size_large: f32,
    pub font_size_title: f32,
    pub font_size_page_title: f32,

    // ------------------------------------------------------------ metrics
    pub titlebar_height: f32,
    pub sidebar_width: f32,
    pub sidebar_width_min: f32,
    pub sidebar_width_max: f32,
    pub player_height: f32,
    pub queue_height: f32,
    pub queue_collapsed_height: f32,
    pub card_min_width: f32,
    pub card_gap: f32,
    pub track_row_height: f32,
    pub queue_row_height: f32,
    pub main_padding_h: f32,
    pub main_padding_v: f32,
    /// Always-reserved scrollbar gutter on the right of the main scroll area.
    pub main_gutter: f32,
    /// Sidebar nav row: height and bottom margin.
    pub sidebar_item_height: f32,
    pub sidebar_item_margin: f32,
    /// Titlebar search box (width is `min(search_width_max, 42vw)`).
    pub search_width_max: f32,
    pub search_height: f32,
    /// Seek/volume track height and thumb diameter.
    pub seek_height: f32,
    pub seek_thumb: f32,
    /// Library card: inner padding, and name/subtitle margins.
    pub card_padding: f32,
    pub card_name_margin: f32,
    pub card_sub_margin: f32,

    // ------------------------------------------------------------- radii
    pub radius_sm: f32,
    /// Small covers and track-row covers (4px in the CSS).
    pub radius_sm2: f32,
    pub radius_md: f32,
    pub radius_lg: f32,
    pub radius_xl: f32,
    /// Fully rounded pills (`border-radius: 999px`).
    pub radius_pill: f32,

    // ----------------------------------------------------------- spacing
    pub space_xs: f32,
    pub space_sm: f32,
    pub space_md: f32,
    pub space_lg: f32,

    // ------------------------------------------------- window constraints
    pub min_window_width: f32,
    pub min_window_height: f32,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            bg: Color::hex(0x0e0e10),
            bg_raised: Color::hex(0x151518),
            bg_hover: Color::hex(0x1d1d21),
            border: Color::hex(0x232328),
            titlebar_bg: Color::hex(0x111113),
            sidebar_bg: Color::hex(0x111113),
            player_bg: Color::hex(0x111113),
            queue_bg: Color::hex(0x121215),

            text: Color::hex(0xececef),
            text_dim: Color::hex(0x8b8b93),

            accent: Color::hex(0x8b3df0),
            accent_dim: Color::hex(0x6d28d9),
            danger: Color::hex(0xe5484d),
            focus: Color::hex(0xc4a0ff),

            scrollbar_thumb: Color::hex(0x2a2a30),
            cover_gradient_start: Color::hex(0x1c1c22),
            cover_gradient_end: Color::hex(0x141417),
            toast_bg: Color::hex(0x2a1216),
            toast_border: Color::hex(0xb81e30),
            toast_text: Color::hex(0xffb3bb),
            banner_bg: Color::hex(0x1c1c20),
            seek_track: Color::hex(0xefefef),
            seek_active: Color::hex(0x8b3df0),

            font_size: 14.0,
            font_size_small: 13.0,
            font_size_tiny: 11.0,
            font_size_large: 15.0,
            font_size_title: 17.0,
            font_size_page_title: 30.0,

            titlebar_height: 46.0,
            sidebar_width: 216.0,
            sidebar_width_min: 160.0,
            sidebar_width_max: 420.0,
            player_height: 84.0,
            queue_height: 235.0,
            queue_collapsed_height: 45.0,
            card_min_width: 160.0,
            card_gap: 18.0,
            track_row_height: 54.0,
            queue_row_height: 44.0,
            main_padding_h: 24.0,
            main_padding_v: 20.0,
            main_gutter: 10.0,
            sidebar_item_height: 38.0,
            sidebar_item_margin: 3.0,
            search_width_max: 480.0,
            search_height: 31.0,
            seek_height: 4.0,
            seek_thumb: 16.0,
            card_padding: 10.0,
            card_name_margin: 9.0,
            card_sub_margin: 2.0,

            radius_sm: 5.0,
            radius_sm2: 4.0,
            radius_md: 6.0,
            radius_lg: 8.0,
            radius_xl: 10.0,
            radius_pill: 999.0,

            space_xs: 4.0,
            space_sm: 8.0,
            space_md: 12.0,
            space_lg: 18.0,

            min_window_width: 940.0,
            min_window_height: 620.0,
        }
    }
}
