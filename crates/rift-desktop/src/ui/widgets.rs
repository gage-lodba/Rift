//! Styled widgets matching the old web UI's `styles.css`.
//!
//! Every widget is one immediate-mode method on [`Ui`]: it paints and hit-tests
//! the same rectangle in the same pass and returns a
//! [`Response`](crate::ui::Response). Widget ids are derived from the label plus
//! the (quantised) rectangle, so a row's buttons keep stable identity frame to
//! frame; use the `_with_id` variants when a widget's label/position can change
//! without the widget itself being a different widget.
//!
//! Colors come from [`Theme`](crate::theme::Theme) where the web UI had a token;
//! the handful of page-local colors (slider track, badges) live in this module
//! as named constants so screens stay out of hex literals.

use super::paint::{SCROLLBAR_THUMB, SCROLLBAR_THUMB_HOVER, SCROLLBAR_WIDTH};
use super::{
    Align, Color, CursorShape, Icon, IconFont, Id, Padding, Rect, Response, Sense, Size, Ui, Vec2,
};
use crate::text::{TextStyle, Weight};

// ---------------------------------------------------------------------------
// shared helpers
// ---------------------------------------------------------------------------

pub(crate) fn mix(a: Color, b: Color, t: f32) -> Color {
    let t = t.clamp(0.0, 1.0);
    let ch =
        |x: u8, y: u8| ((x as f32 + (y as f32 - x as f32) * t).round()).clamp(0.0, 255.0) as u8;
    Color::rgba(ch(a.r, b.r), ch(a.g, b.g), ch(a.b, b.b), ch(a.a, b.a))
}

pub(crate) fn with_alpha(c: Color, a: u8) -> Color {
    Color::rgba(c.r, c.g, c.b, a)
}

/// Auto id for label-based widgets: stable for a label at a (sub-pixel
/// quantised) position. Rows at different y get distinct ids.
fn auto_id(scope: &str, label: &str, rect: Rect) -> Id {
    Id::hash_of((
        scope,
        label,
        quant(rect.x),
        quant(rect.y),
        quant(rect.w),
        quant(rect.h),
    ))
}

/// Auto id for icon widgets (the "label" is the icon codepoint).
fn auto_id_icon(icon: Icon, rect: Rect) -> Id {
    Id::hash_of((
        "icon-button",
        icon.codepoint() as u32,
        quant(rect.x),
        quant(rect.y),
        quant(rect.w),
        quant(rect.h),
    ))
}

fn quant(v: f32) -> i64 {
    (v * 2.0).round() as i64
}

/// Badge/pill text colors from `.ytdlp-badge` and friends.
pub const SUCCESS: Color = Color::hex(0x4ade80);
pub const SUCCESS_BG: Color = Color::rgba(74, 222, 128, 31);
pub const WARNING: Color = Color::hex(0xfbbf24);
pub const WARNING_BG: Color = Color::rgba(251, 191, 36, 31);
pub const DANGER_TEXT: Color = Color::hex(0xf87171);
pub const DANGER_BG: Color = Color::rgba(248, 113, 113, 31);

/// Slider track color: the browser's default `input[type=range]` track on the
/// dark page (ui-spec §6.3 names `#efefef` literally — not a theme token).
pub const SLIDER_TRACK: Color = Color::hex(0xefefef);

// ---------------------------------------------------------------------------
// buttons
// ---------------------------------------------------------------------------

/// Which visual treatment a button gets. The names follow the CSS classes in
/// `ui/styles.css`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ButtonKind {
    /// `.btn-primary` / `.play-all`: accent fill, white text, 8px radius.
    Primary,
    /// `.btn-secondary` / `.head-btn`: raised fill, 1px border, 8px radius.
    Secondary,
    /// `.link-btn` / `.link`: no chrome, dim text, brightens on hover.
    Ghost,
    /// `.tab`: pill, raised until active, then accent.
    Tab { active: bool },
    /// `.play-all`: accent pill with a leading icon, 999px radius.
    PlayAll,
    /// `.head-btn`: raised pill with a leading icon, 999px radius.
    HeadButton,
    /// Destructive action (delete, remove download).
    Danger,
    /// `.menu-item` row (usually drawn by [`Ui::menu_item`]).
    MenuItem { danger: bool },
}

/// A fully specified button appearance. [`ButtonStyle::for_kind`] derives the
/// standard ones from the theme; screens may tweak fields directly.
#[derive(Clone, Debug)]
pub struct ButtonStyle {
    pub bg: Color,
    pub hover_bg: Color,
    pub pressed_bg: Color,
    pub fg: Color,
    pub hover_fg: Color,
    pub border: Color,
    pub hover_border: Color,
    pub border_width: f32,
    pub radius: f32,
    pub padding: Padding,
    pub font_size: f32,
    pub font_weight: Weight,
    pub icon: Option<Icon>,
    pub icon_size: f32,
    pub gap: f32,
}

impl ButtonStyle {
    /// The standard style for a `ButtonKind`, from the theme.
    pub fn for_kind(kind: ButtonKind, theme: &crate::theme::Theme) -> Self {
        let base = ButtonStyle {
            bg: Color::TRANSPARENT,
            hover_bg: theme.bg_hover,
            pressed_bg: theme.bg_hover,
            fg: theme.text,
            hover_fg: theme.text,
            border: Color::TRANSPARENT,
            hover_border: Color::TRANSPARENT,
            border_width: 0.0,
            radius: theme.radius_lg,
            padding: Padding::symmetric(14.0, 8.0),
            font_size: theme.font_size_small,
            font_weight: Weight::SemiBold,
            icon: None,
            icon_size: 16.0,
            gap: 7.0,
        };
        match kind {
            ButtonKind::Primary => ButtonStyle {
                bg: theme.accent,
                hover_bg: theme.accent_dim,
                pressed_bg: theme.accent_dim,
                fg: Color::WHITE,
                hover_fg: Color::WHITE,
                ..base
            },
            ButtonKind::Secondary => ButtonStyle {
                bg: theme.bg_hover,
                hover_bg: theme.border,
                pressed_bg: theme.border,
                fg: theme.text,
                hover_fg: theme.text,
                border: theme.border,
                hover_border: theme.accent,
                border_width: 1.0,
                ..base
            },
            ButtonKind::Ghost => ButtonStyle {
                bg: Color::TRANSPARENT,
                hover_bg: Color::TRANSPARENT,
                pressed_bg: Color::TRANSPARENT,
                fg: theme.text_dim,
                hover_fg: theme.text,
                padding: Padding::symmetric(6.0, 4.0),
                font_weight: Weight::SemiBold,
                radius: theme.radius_md,
                ..base
            },
            ButtonKind::Tab { active } => ButtonStyle {
                bg: if active {
                    theme.accent
                } else {
                    theme.bg_raised
                },
                hover_bg: if active {
                    theme.accent
                } else {
                    theme.bg_raised
                },
                pressed_bg: theme.accent_dim,
                fg: if active { Color::WHITE } else { theme.text_dim },
                hover_fg: if active { Color::WHITE } else { theme.text },
                border: if active { theme.accent } else { theme.border },
                hover_border: if active { theme.accent } else { theme.border },
                border_width: 1.0,
                radius: 999.0,
                padding: Padding::symmetric(16.0, 6.0),
                font_size: theme.font_size_small,
                ..base
            },
            ButtonKind::PlayAll => ButtonStyle {
                bg: theme.accent,
                hover_bg: theme.accent_dim,
                pressed_bg: theme.accent_dim,
                fg: Color::WHITE,
                hover_fg: Color::WHITE,
                radius: 999.0,
                padding: Padding::symmetric(22.0, 8.0),
                font_size: theme.font_size,
                font_weight: Weight::Bold,
                icon: Some(Icon::Play),
                icon_size: 16.0,
                ..base
            },
            ButtonKind::HeadButton => ButtonStyle {
                bg: theme.bg_raised,
                hover_bg: theme.bg_hover,
                pressed_bg: theme.bg_hover,
                fg: theme.text,
                hover_fg: theme.text,
                border: theme.border,
                hover_border: theme.text_dim,
                border_width: 1.0,
                radius: 999.0,
                padding: Padding::symmetric(18.0, 8.0),
                font_size: theme.font_size,
                font_weight: Weight::SemiBold,
                ..base
            },
            ButtonKind::Danger => ButtonStyle {
                bg: Color::TRANSPARENT,
                hover_bg: with_alpha(theme.danger, 24),
                pressed_bg: with_alpha(theme.danger, 40),
                fg: theme.danger,
                hover_fg: theme.danger,
                radius: theme.radius_md,
                padding: Padding::symmetric(10.0, 8.0),
                ..base
            },
            ButtonKind::MenuItem { danger } => {
                let fg = if danger { theme.accent } else { theme.text };
                ButtonStyle {
                    bg: Color::TRANSPARENT,
                    hover_bg: theme.bg_hover,
                    pressed_bg: theme.bg_hover,
                    fg,
                    hover_fg: fg,
                    radius: theme.radius_md,
                    padding: Padding::symmetric(10.0, 8.0),
                    font_size: theme.font_size_small,
                    font_weight: Weight::Regular,
                    gap: 10.0,
                    ..base
                }
            }
        }
    }
}

impl Ui {
    /// Draw a text button. `kind` picks the standard style from the theme.
    ///
    /// `.clicked` is true on release inside, and on keyboard Enter/Space while
    /// focused; `.double_clicked` for double clicks.
    pub fn button(&mut self, rect: Rect, label: &str, kind: ButtonKind) -> Response {
        let id = auto_id("button", label, rect);
        self.button_with_id(id, rect, label, kind)
    }

    /// Text button with an explicit id (use when the label changes, e.g. a
    /// play/pause toggle whose text flips).
    pub fn button_with_id(
        &mut self,
        id: Id,
        rect: Rect,
        label: &str,
        kind: ButtonKind,
    ) -> Response {
        let style = ButtonStyle::for_kind(kind, self.theme());
        self.button_styled_with_id(id, rect, label, &style)
    }

    /// Text button with a fully custom style.
    pub fn button_styled(&mut self, rect: Rect, label: &str, style: &ButtonStyle) -> Response {
        let id = auto_id("button", label, rect);
        self.button_styled_with_id(id, rect, label, style)
    }

    pub fn button_styled_with_id(
        &mut self,
        id: Id,
        rect: Rect,
        label: &str,
        style: &ButtonStyle,
    ) -> Response {
        let response = self.interact(rect, id, Sense::CLICK);
        let t = if response.active {
            1.0
        } else {
            response.hover_t
        };
        self.paint_button(rect, label, style, t);
        if response.focus_visible {
            self.focus_ring(rect, style.radius);
        }
        response
    }

    /// Paint a button without interacting with it (used by the disabled path).
    fn paint_button(&mut self, rect: Rect, label: &str, style: &ButtonStyle, t: f32) {
        let bg = mix(style.bg, style.hover_bg, t);
        let fg = mix(style.fg, style.hover_fg, t);
        let border = mix(style.border, style.hover_border, t);
        self.bordered_rect(rect, bg, style.radius, style.border_width, border);
        let content = rect.inset_pad(style.padding);
        let text_style = TextStyle::new(style.font_size, style.font_weight, fg);
        match style.icon {
            Some(icon) => {
                self.icon_label(
                    content,
                    icon,
                    style.icon_size,
                    label,
                    &text_style,
                    style.gap,
                );
            }
            None => self.text_centered(content, label, &text_style),
        }
    }

    /// Button with a `disabled` state: no interaction (no hover/click/focus),
    /// dimmed colors (`.head-btn:disabled`, `.btn-primary:disabled`).
    pub fn button_enabled(
        &mut self,
        rect: Rect,
        label: &str,
        kind: ButtonKind,
        enabled: bool,
    ) -> Response {
        if enabled {
            return self.button(rect, label, kind);
        }
        let theme = self.theme().clone();
        let mut style = ButtonStyle::for_kind(kind, &theme);
        style.bg = mix(style.bg, theme.bg, 0.45);
        style.hover_bg = style.bg;
        style.border = mix(style.border, theme.bg, 0.45);
        style.hover_border = style.border;
        style.fg = theme.text_dim;
        style.hover_fg = style.fg;
        let id = auto_id("button-disabled", label, rect);
        let response = self.interact(rect, id, Sense::NONE);
        self.paint_button(rect, label, &style, 0.0);
        response
    }

    /// Icon button with a `disabled` state (e.g. "next" with an empty queue).
    pub fn icon_button_enabled(&mut self, rect: Rect, icon: Icon, enabled: bool) -> Response {
        let theme = self.theme().clone();
        let style = IconButtonStyle::from_theme(&theme);
        let id = auto_id_icon(icon, rect);
        if enabled {
            return self.icon_button_styled(rect, icon, false, &style);
        }
        let response = self.interact(rect, id, Sense::NONE);
        let color = mix(style.color, theme.bg, 0.5);
        self.icon_font(rect, icon, color, style.icon_size, style.icon_font);
        response
    }

    /// A button pre-configured with a leading 16px icon (`.play-all`-style).
    pub fn head_button(
        &mut self,
        rect: Rect,
        icon: Icon,
        label: &str,
        kind: ButtonKind,
    ) -> Response {
        let mut style = ButtonStyle::for_kind(kind, self.theme());
        style.icon = Some(icon);
        self.button_styled(rect, label, &style)
    }

    /// The old UI's `.play-btn`: a filled circle with a dark play/pause glyph.
    pub fn play_button(&mut self, rect: Rect, playing: bool) -> Response {
        let id = auto_id("play-button", if playing { "pause" } else { "play" }, rect);
        let response = self.interact(rect, id, Sense::CLICK);
        let theme = self.theme().clone();
        let bg = if response.active {
            theme.text_dim
        } else {
            theme.text
        };
        self.circle(rect.center_v(), rect.w.min(rect.h) * 0.5, bg);
        let icon = if playing { Icon::Pause } else { Icon::Play };
        let icon_rect = rect.inset(rect.w * 0.3);
        self.icon(icon_rect, icon, Color::hex(0x111111), icon_rect.w.max(12.0));
        if response.focus_visible {
            self.focus_ring(rect, rect.w * 0.5);
        }
        response
    }

    // ---------------------------------------------------------- icon buttons

    /// `.ibtn`: a square transparent icon button that gets a hover background.
    pub fn icon_button(&mut self, rect: Rect, icon: Icon) -> Response {
        let style = IconButtonStyle::from_theme(self.theme());
        self.icon_button_styled(rect, icon, false, &style)
    }

    /// Icon button whose icon is permanently tinted (e.g. an active heart).
    pub fn icon_button_tinted(&mut self, rect: Rect, icon: Icon, color: Color) -> Response {
        let mut style = IconButtonStyle::from_theme(self.theme());
        style.color = color;
        style.hover_color = color;
        self.icon_button_styled(rect, icon, true, &style)
    }

    pub fn icon_button_styled(
        &mut self,
        rect: Rect,
        icon: Icon,
        active: bool,
        style: &IconButtonStyle,
    ) -> Response {
        let id = auto_id_icon(icon, rect);
        let response = self.interact(rect, id, Sense::CLICK);
        let base = if active {
            style.active_color.unwrap_or(style.color)
        } else {
            style.color
        };
        let t = if response.active {
            1.0
        } else {
            response.hover_t
        };
        let fg = mix(base, style.hover_color, t);
        if (response.hovered || response.active) && style.hover_bg.a > 0 {
            self.rounded_rect(rect, style.hover_bg, style.radius);
        }
        self.icon_font(rect, icon, fg, style.icon_size, style.icon_font);
        if response.focus_visible {
            self.focus_ring(rect, style.radius);
        }
        response
    }
}

/// Appearance of an `.ibtn` icon button. `28x28` by default, 5px radius.
#[derive(Clone, Debug)]
pub struct IconButtonStyle {
    pub icon_size: f32,
    pub color: Color,
    pub hover_color: Color,
    pub active_color: Option<Color>,
    pub hover_bg: Color,
    pub radius: f32,
    pub icon_font: IconFont,
}

impl IconButtonStyle {
    pub fn from_theme(theme: &crate::theme::Theme) -> Self {
        Self {
            icon_size: 16.0,
            color: theme.text_dim,
            hover_color: theme.text,
            active_color: None,
            hover_bg: theme.bg_hover,
            radius: theme.radius_sm,
            icon_font: IconFont::Filled,
        }
    }

    pub fn size(mut self, icon_size: f32) -> Self {
        self.icon_size = icon_size;
        self
    }

    pub fn tint(mut self, color: Color) -> Self {
        self.color = color;
        self
    }

    /// Accent color when `active` (liked hearts, active transport buttons).
    pub fn active_tint(mut self, color: Color) -> Self {
        self.active_color = Some(color);
        self
    }

    pub fn no_background(mut self) -> Self {
        self.hover_bg = Color::TRANSPARENT;
        self
    }
}

/// A tiny stable salt for auto ids when the same icon appears twice at the same
/// rect in one frame (rare; distinct rects already disambiguate).
#[allow(dead_code)]
fn icon_id_salt(icon: Icon) -> u32 {
    icon.codepoint() as u32
}

// ---------------------------------------------------------------------------
// toggle / checkbox
// ---------------------------------------------------------------------------

/// Result of a boolean widget.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CheckboxResponse {
    pub response: Response,
    /// The bound value changed this frame.
    pub changed: bool,
}

impl Ui {
    /// The settings `.switch`: 44x24 pill with a 20px white knob, accent when
    /// on. Pass the whole switch rect; it is drawn inside.
    pub fn switch(&mut self, rect: Rect, on: &mut bool) -> CheckboxResponse {
        self.switch_enabled(rect, on, true)
    }

    /// Switch with a `disabled` state: dimmed and non-interactive.
    pub fn switch_enabled(&mut self, rect: Rect, on: &mut bool, enabled: bool) -> CheckboxResponse {
        let id = auto_id("switch", if *on { "on" } else { "off" }, rect);
        let response = self.interact(rect, id, if enabled { Sense::CLICK } else { Sense::NONE });
        let theme = self.theme().clone();
        let mut changed = false;
        if enabled && response.clicked {
            *on = !*on;
            changed = true;
        }
        let track = if *on { theme.accent } else { theme.bg_hover };
        let track = if enabled {
            track
        } else {
            mix(track, theme.bg, 0.45)
        };
        self.rounded_rect(rect, track, rect.h * 0.5);
        let knob = (rect.h - 4.0).max(4.0);
        let knob_x = if *on {
            rect.right() - 2.0 - knob
        } else {
            rect.x + 2.0
        };
        let knob_color = if enabled {
            Color::WHITE
        } else {
            theme.text_dim
        };
        self.circle(
            Vec2::new(knob_x + knob * 0.5, rect.y + rect.h * 0.5),
            knob * 0.5,
            knob_color,
        );
        if response.focus_visible {
            self.focus_ring(rect, rect.h * 0.5);
        }
        CheckboxResponse { response, changed }
    }

    /// A 16x16 checkbox with a label. `rect` is the whole row (box on the left,
    /// label after it).
    pub fn checkbox(&mut self, rect: Rect, checked: &mut bool, label: &str) -> CheckboxResponse {
        let id = auto_id("checkbox", label, rect);
        let response = self.interact(rect, id, Sense::CLICK);
        let theme = self.theme().clone();
        let mut changed = false;
        if response.clicked {
            *checked = !*checked;
            changed = true;
        }
        let box_size = rect.h.min(16.0);
        let box_rect = Rect::new(
            rect.x,
            rect.y + (rect.h - box_size) * 0.5,
            box_size,
            box_size,
        );
        if *checked {
            self.rounded_rect(box_rect, theme.accent, 4.0);
            let inset = box_rect.inset(3.0);
            self.icon(inset, Icon::Check, Color::WHITE, inset.w.max(8.0));
        } else {
            self.bordered_rect(box_rect, Color::TRANSPARENT, 4.0, 1.0, theme.border);
        }
        let label_rect = Rect::new(
            box_rect.right() + 8.0,
            rect.y,
            (rect.right() - box_rect.right() - 8.0).max(0.0),
            rect.h,
        );
        let style = TextStyle::new(theme.font_size, Weight::Regular, theme.text);
        self.text(label_rect, label, &style);
        if response.focus_visible {
            self.focus_ring(rect, theme.radius_md);
        }
        CheckboxResponse { response, changed }
    }
}

// ---------------------------------------------------------------------------
// slider
// ---------------------------------------------------------------------------

/// Appearance of a range slider (`input[type=range]` with `accent-color`).
#[derive(Clone, Debug)]
pub struct SliderStyle {
    pub track_height: f32,
    pub track_color: Color,
    pub fill_color: Color,
    pub fill_color_hover: Color,
    pub thumb_radius: f32,
    pub thumb_radius_hover: f32,
    pub thumb_color: Color,
    /// Optional granularity; `0.0` = continuous.
    pub step: f32,
}

impl SliderStyle {
    pub fn from_theme(theme: &crate::theme::Theme) -> Self {
        Self {
            track_height: 4.0,
            track_color: SLIDER_TRACK,
            fill_color: theme.accent,
            fill_color_hover: theme.accent,
            // ui-spec §6.3: 4px track #efefef, played fill accent, 16x16 thumb.
            thumb_radius: 8.0,
            thumb_radius_hover: 8.0,
            thumb_color: theme.accent,
            step: 0.0,
        }
    }

    /// The player bar's seek/volume slider styles (accent on a light track).
    pub fn seek(theme: &crate::theme::Theme) -> Self {
        Self::from_theme(theme)
    }

    pub fn with_step(mut self, step: f32) -> Self {
        self.step = step;
        self
    }

    pub fn with_track_color(mut self, color: Color) -> Self {
        self.track_color = color;
        self
    }
}

/// Result of a slider interaction.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SliderResponse {
    pub response: Response,
    /// The bound value changed this frame.
    pub changed: bool,
}

impl Ui {
    /// Horizontal range slider. `rect` is the interactive row (the visible track
    /// is `style.track_height` tall and vertically centred); `value` is clamped
    /// to `min..=max`.
    pub fn slider(&mut self, rect: Rect, value: &mut f32, min: f32, max: f32) -> SliderResponse {
        let id = auto_id("slider", "range", rect);
        let style = SliderStyle::from_theme(self.theme());
        self.slider_styled(id, rect, value, min, max, &style)
    }

    pub fn slider_styled(
        &mut self,
        id: Id,
        rect: Rect,
        value: &mut f32,
        min: f32,
        max: f32,
        style: &SliderStyle,
    ) -> SliderResponse {
        // Grab area: the row expanded vertically so thin sliders are easy to hit.
        let hit = rect.expand((style.thumb_radius - rect.h * 0.5).max(6.0));
        let response = self.interact(hit, id, Sense::DRAG);
        let mut changed = false;
        let range = (max - min).max(f32::EPSILON);
        let r = style.thumb_radius;
        // The thumb stays inside the track at both extremes, so its centre
        // travels over `rect.w - 2r` (ui-spec §6.3: center = x + 8 + f*(w-16)).
        let usable = (rect.w - r * 2.0).max(1.0);
        let mut v = value.clamp(min, max);
        if (response.active && self.input().mouse_left_down) || response.clicked {
            let mouse_x = self.input().mouse_pos.0;
            let t = ((mouse_x - rect.x - r) / usable).clamp(0.0, 1.0);
            v = min + t * range;
            if style.step > 0.0 {
                v = ((v - min) / style.step).round() * style.step + min;
            }
            v = v.clamp(min, max);
            if (v - *value).abs() > f32::EPSILON {
                *value = v;
                changed = true;
            }
        }
        let t = ((v - min) / range).clamp(0.0, 1.0);
        let active = response.active || response.hovered;
        let track_y = rect.y + (rect.h - style.track_height) * 0.5;
        let track = Rect::new(rect.x, track_y, rect.w, style.track_height);
        let cx = rect.x + r + usable * t;
        let cy = rect.y + rect.h * 0.5;
        self.rounded_rect(track, style.track_color, style.track_height * 0.5);
        let fill_w = (cx - track.x).max(0.0);
        if fill_w > 0.0 {
            let fill_color = if active {
                style.fill_color_hover
            } else {
                style.fill_color
            };
            self.rounded_rect(
                Rect::new(track.x, track.y, fill_w, style.track_height),
                fill_color,
                style.track_height * 0.5,
            );
        }
        let r_draw = if active { style.thumb_radius_hover } else { r };
        self.circle(Vec2::new(cx, cy), r_draw, style.thumb_color);
        if response.hovered || response.active {
            self.set_cursor(CursorShape::Pointer);
        }
        if response.focus_visible {
            self.focus_ring(rect, rect.h * 0.5);
        }
        SliderResponse { response, changed }
    }

    /// Slider that can be disabled: no interaction, dimmed visuals.
    pub fn slider_enabled(
        &mut self,
        rect: Rect,
        value: &mut f32,
        min: f32,
        max: f32,
        enabled: bool,
    ) -> SliderResponse {
        if enabled {
            return self.slider(rect, value, min, max);
        }
        let style = SliderStyle::from_theme(self.theme());
        let id = auto_id("slider", "range", rect);
        let response = self.interact(rect, id, Sense::HOVER);
        let theme = self.theme().clone();
        let t = ((*value - min) / (max - min).max(f32::EPSILON)).clamp(0.0, 1.0);
        let track_y = rect.y + (rect.h - style.track_height) * 0.5;
        let track = Rect::new(rect.x, track_y, rect.w, style.track_height);
        let r = style.thumb_radius;
        let usable = (rect.w - r * 2.0).max(1.0);
        self.rounded_rect(track, theme.border, style.track_height * 0.5);
        let cx = rect.x + r + usable * t;
        self.rounded_rect(
            Rect::new(
                track.x,
                track.y,
                (cx - track.x).max(0.0),
                style.track_height,
            ),
            theme.bg_hover,
            style.track_height * 0.5,
        );
        self.circle(Vec2::new(cx, rect.y + rect.h * 0.5), r, theme.text_dim);
        SliderResponse {
            response,
            changed: false,
        }
    }
}

// ---------------------------------------------------------------------------
// progress / spinner / equalizer / badges
// ---------------------------------------------------------------------------

/// Badge color scheme (`.ytdlp-badge` states plus generic pills).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BadgeKind {
    Neutral,
    Success,
    Warning,
    Danger,
    Accent,
}

impl Ui {
    /// Thin progress bar (download progress, loading state).
    pub fn progress(&mut self, rect: Rect, fraction: f32, color: Color) {
        let theme = self.theme().clone();
        let radius = rect.h * 0.5;
        self.rounded_rect(rect, theme.bg_hover, radius);
        let f = fraction.clamp(0.0, 1.0);
        if f > 0.0 {
            self.rounded_rect(Rect::new(rect.x, rect.y, rect.w * f, rect.h), color, radius);
        }
    }

    /// Indeterminate spinner: eight dots around a circle with a rotating bright
    /// head. `rect` is the square the spinner lives in.
    pub fn spinner(&mut self, rect: Rect) {
        let theme = self.theme().clone();
        let now = self.time() as f32;
        let center = rect.center_v();
        let radius = rect.w.min(rect.h) * 0.5;
        let dot = (radius * 0.28).max(1.5);
        for i in 0..8 {
            let phase = (i as f32 / 8.0 + now * 1.4).fract();
            let angle = i as f32 / 8.0 * std::f32::consts::TAU;
            let alpha = ((phase * 255.0) as u8).max(24);
            let pos = Vec2::new(
                center.x + angle.cos() * (radius - dot),
                center.y + angle.sin() * (radius - dot),
            );
            self.circle(pos, dot * 0.5, with_alpha(theme.text_dim, alpha));
        }
    }

    /// The `.eq` "now playing" equalizer bars: three 3px accent bars bouncing
    /// while playing, frozen at 5px when paused. Matches the CSS animation
    /// (`eq-bounce`, 0.9s ease-in-out, delays -0.5s/-0.2s/-0.7s).
    pub fn equalizer(&mut self, rect: Rect, playing: bool) {
        let theme = self.theme().clone();
        let now = self.time();
        let bar_w = 3.0;
        let gap = 2.0;
        // CSS nth-child delays: -0.5s, -0.2s, -0.7s.
        let delays = [0.5f64, 0.2, 0.7];
        for (i, delay) in delays.iter().enumerate() {
            let h = if playing {
                let phase = ((now + delay) / 0.9).fract() as f32;
                // ease-in-out bounce between 4 and 14 px (50% keyframe at 14).
                9.0 - 5.0 * (phase * std::f32::consts::TAU).cos()
            } else {
                5.0
            };
            let h = h.clamp(4.0, 14.0);
            let x = rect.x + i as f32 * (bar_w + gap);
            let y = rect.y + rect.h - h;
            self.rounded_rect(Rect::new(x, y, bar_w, h), theme.accent, 1.0);
        }
    }

    /// A rounded pill with 12px semibold text, e.g. "Downloaded" or "6 songs".
    pub fn badge(&mut self, rect: Rect, label: &str, kind: BadgeKind) {
        let theme = self.theme().clone();
        let (fg, bg) = match kind {
            BadgeKind::Neutral => (theme.text_dim, theme.bg_hover),
            BadgeKind::Success => (SUCCESS, SUCCESS_BG),
            BadgeKind::Warning => (WARNING, WARNING_BG),
            BadgeKind::Danger => (DANGER_TEXT, DANGER_BG),
            BadgeKind::Accent => (theme.accent, with_alpha(theme.accent, 36)),
        };
        self.rounded_rect(rect, bg, rect.h * 0.5);
        let style = TextStyle::new(12.0, Weight::SemiBold, fg);
        self.text_centered(rect, label, &style);
    }

    // ------------------------------------------------------ covers / panels

    /// Cover art with `.cover-fallback` behaviour: when there is no texture,
    /// draw the raised background plus a dim music note.
    pub fn cover(&mut self, rect: Rect, texture: Option<super::TextureId>, radius: f32) {
        let theme = self.theme().clone();
        match texture {
            Some(tex) if tex != super::NO_TEXTURE => {
                self.rounded_rect(rect, theme.bg_raised, radius);
                self.image(rect, tex, Color::WHITE, radius);
            }
            _ => {
                self.rounded_rect(rect, theme.bg_raised, radius);
                let ink = rect.w.min(rect.h) * 0.3;
                self.icon(rect, Icon::MusicNote, theme.text_dim, ink);
            }
        }
    }

    /// Circular cover (artist avatar).
    pub fn cover_round(&mut self, rect: Rect, texture: Option<super::TextureId>) {
        self.cover(rect, texture, rect.w.min(rect.h) * 0.5);
    }

    /// A `.card`-style raised panel: raised background, 1px border, 8px radius.
    pub fn panel(&mut self, rect: Rect, radius: f32) {
        let theme = self.theme().clone();
        self.bordered_rect(rect, theme.bg_raised, radius, 1.0, theme.border);
    }

    /// An elevated card (menu-ish): `#1c1c20`, border, 10px radius, soft shadow.
    pub fn elevated_card(&mut self, rect: Rect, radius: f32) {
        let theme = self.theme().clone();
        self.shadow(rect, radius, 1.0);
        self.bordered_rect(rect, Color::hex(0x1c1c20), radius, 1.0, theme.border);
    }

    /// A 1px divider line spanning `rect` horizontally (border color).
    pub fn divider(&mut self, rect: Rect) {
        let theme = self.theme().clone();
        let y = rect.y + (rect.h - 1.0) * 0.5;
        self.fill(Rect::new(rect.x, y, rect.w, 1.0), theme.border);
    }

    // -------------------------------------------------------- inline layout

    /// Draw a left label + right value row (settings rows, counts).
    pub fn label_value(
        &mut self,
        rect: Rect,
        label: &str,
        value: &str,
        label_style: &TextStyle,
        value_style: &TextStyle,
    ) {
        self.text_aligned(rect, label, label_style, Align::Left);
        self.text_aligned(rect, value, value_style, Align::Right);
    }

    /// A page section heading (`h2`): 17px bold.
    pub fn section_title(&mut self, rect: Rect, title: &str) {
        let theme = self.theme().clone();
        let style = TextStyle::new(17.0, Weight::Bold, theme.text);
        self.text(rect, title, &style);
    }

    /// Small uppercase eyebrow text (10px, 1.5px tracking) used by the sidebar's
    /// "PLAYLISTS" heading and the queue header.
    pub fn eyebrow(&mut self, rect: Rect, text: &str) {
        let theme = self.theme().clone();
        let style = TextStyle::new(10.0, Weight::Bold, theme.text_dim).with_letter_spacing(1.5);
        self.text(rect, text, &style);
    }

    /// Empty-state message (`.empty`): dim, centred.
    pub fn empty_state(&mut self, rect: Rect, text: &str) {
        let theme = self.theme().clone();
        let style = TextStyle::new(13.0, Weight::Regular, theme.text_dim);
        self.text_centered(rect, text, &style);
    }

    /// Size of a text run, for laying out inline chips (`ui.measure_text` alias
    /// with a shorter name for screen code).
    pub fn text_size(&mut self, text: &str, style: &TextStyle) -> Size {
        self.measure_text(text, style)
    }
}

/// Format seconds as `m:ss` or `h:mm:ss` (ported from the old `fmt_secs`).
pub fn format_duration(total_secs: u32) -> String {
    let (h, m, s) = (total_secs / 3600, (total_secs % 3600) / 60, total_secs % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// Format a position in seconds (f64) the way the player bar does.
pub fn format_position(secs: f64) -> String {
    format_duration(secs.max(0.0) as u32)
}

/// Unused re-exports kept for widget code that wants scrollbar colors directly.
pub(crate) const _SCROLLBAR_COLORS: (Color, Color, f32) =
    (SCROLLBAR_THUMB, SCROLLBAR_THUMB_HOVER, SCROLLBAR_WIDTH);
