//! Buttons and pills: the generic styled button, the accent/`.head-btn`
//! families, `.ibtn` icon buttons and `.link` text buttons.

use crate::text::{TextStyle, Weight};
use crate::ui::layout::Align;
use crate::ui::{Color, Icon, Id, Rect, Response, Sense, Ui};

use super::style;

// ---------------------------------------------------------------------------
// buttons
// ---------------------------------------------------------------------------

/// Where a button's fill/text should be in its state machine.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ButtonColors {
    pub fill: Color,
    pub hover_fill: Color,
    pub text: Color,
    pub border: Color,
    pub hover_border: Color,
}

/// The visual style of a generic [`button`]: text, state colours, corner radius
/// and optional border width. Bundled so the widget call stays readable.
#[derive(Clone, Debug, PartialEq)]
pub struct ButtonStyle {
    pub text: TextStyle,
    pub colors: ButtonColors,
    pub radius: f32,
    pub border: f32,
}

/// A generic label button with hover/pressed feedback, painted by the caller's
/// style. Returns the interaction result.
///
/// Disabled buttons are painted at the `.btn-primary:disabled` opacity (0.6,
/// ui-spec §2.4) and do not react to the pointer; keyboard activation is already
/// part of [`Ui::interact`] (focused + Enter/Space), so nothing extra is needed
/// here.
pub fn button(
    ui: &mut Ui,
    rect: Rect,
    id: Id,
    label: &str,
    style: &ButtonStyle,
    enabled: bool,
) -> Response {
    let mut resp = ui.interact(rect, id, if enabled { Sense::CLICK } else { Sense::HOVER });
    if !enabled {
        resp.hovered = false;
        resp.clicked = false;
    }
    let t = if enabled { resp.hover_t } else { 0.0 };
    let mut fill = mix(style.colors.fill, style.colors.hover_fill, t);
    let mut border_color = mix(style.colors.border, style.colors.hover_border, t);
    let text_color = style.text.color;
    let (fill, border_color, text_color) = if enabled {
        (fill, border_color, text_color)
    } else {
        fill.a = (fill.a as f32 * 0.6).round() as u8;
        border_color.a = (border_color.a as f32 * 0.6).round() as u8;
        (
            fill,
            border_color,
            text_color.with_alpha((text_color.a as f32 * 0.6).round() as u8),
        )
    };
    if style.border > 0.0 {
        ui.bordered_rect(rect, fill, style.radius, style.border, border_color);
    } else if fill.a > 0 {
        ui.rounded_rect(rect, fill, style.radius);
    }
    let pressed_offset = if resp.active && enabled { 1.0 } else { 0.0 };
    ui.text_centered(
        rect.offset(0.0, pressed_offset),
        label,
        &style.text.with_color(text_color),
    );
    if resp.focus_visible {
        ui.focus_ring(rect, style.radius);
    }
    resp
}

/// The `.search-btn` / `.play-all` / `.btn-primary` accent pill.
#[allow(clippy::too_many_arguments)] // immediate-mode widget: explicit style/layout args are the API
pub fn accent_button(
    ui: &mut Ui,
    rect: Rect,
    id: Id,
    label: &str,
    icon: Option<Icon>,
    font_size: f32,
    weight: Weight,
    radius: f32,
) -> Response {
    let theme = ui.theme().clone();
    let colors = ButtonColors {
        fill: theme.accent,
        hover_fill: theme.accent_dim,
        text: Color::WHITE,
        border: Color::TRANSPARENT,
        hover_border: Color::TRANSPARENT,
    };
    let resp = ui.interact(rect, id, Sense::CLICK);
    let t = resp.hover_t;
    ui.rounded_rect(rect, mix(colors.fill, colors.hover_fill, t), radius);
    let st = style(font_size, weight, Color::WHITE);
    match icon {
        Some(icon) => {
            ui.icon_label(rect, icon, font_size, label, &st, 7.0);
        }
        None => {
            ui.text_centered(rect, label, &st);
        }
    }
    if resp.focus_visible {
        ui.focus_ring(rect, radius);
    }
    resp
}

/// `.head-btn` secondary pill (raised fill, 1 px border).
pub fn secondary_pill(
    ui: &mut Ui,
    rect: Rect,
    id: Id,
    label: &str,
    icon: Option<Icon>,
    accent: bool,
) -> Response {
    let theme = ui.theme().clone();
    let colors = ButtonColors {
        fill: theme.bg_raised,
        hover_fill: theme.bg_hover,
        text: if accent { theme.accent } else { theme.text },
        border: if accent {
            theme.accent_dim
        } else {
            theme.border
        },
        hover_border: if accent { theme.accent } else { theme.text_dim },
    };
    let resp = ui.interact(rect, id, Sense::CLICK);
    let t = resp.hover_t;
    ui.bordered_rect(
        rect,
        mix(colors.fill, colors.hover_fill, t),
        999.0,
        1.0,
        mix(colors.border, colors.hover_border, t),
    );
    let st = style(
        14.0,
        if accent {
            Weight::Bold
        } else {
            Weight::SemiBold
        },
        colors.text,
    );
    match icon {
        Some(icon) => {
            ui.icon_label(rect, icon, 16.0, label, &st, 7.0);
        }
        None => {
            ui.text_centered(rect, label, &st);
        }
    }
    if resp.focus_visible {
        ui.focus_ring(rect, 999.0);
    }
    resp
}

/// `.btn-primary` (small accent button, radius 8).
pub fn primary_button(ui: &mut Ui, rect: Rect, id: Id, label: &str, enabled: bool) -> Response {
    let theme = ui.theme().clone();
    let colors = ButtonColors {
        fill: theme.accent,
        hover_fill: theme.accent_dim,
        text: Color::WHITE,
        border: Color::TRANSPARENT,
        hover_border: Color::TRANSPARENT,
    };
    button(
        ui,
        rect,
        id,
        label,
        &ButtonStyle {
            text: style(13.0, Weight::SemiBold, Color::WHITE),
            colors,
            radius: theme.radius_lg,
            border: 0.0,
        },
        enabled,
    )
}

/// `.btn-secondary` (small raised button, radius 8, accent border on hover).
pub fn secondary_button(ui: &mut Ui, rect: Rect, id: Id, label: &str) -> Response {
    let theme = ui.theme().clone();
    let colors = ButtonColors {
        fill: theme.bg_hover,
        hover_fill: theme.border,
        text: theme.text,
        border: theme.border,
        hover_border: theme.accent,
    };
    button(
        ui,
        rect,
        id,
        label,
        &ButtonStyle {
            text: style(13.0, Weight::SemiBold, theme.text),
            colors,
            radius: theme.radius_lg,
            border: 1.0,
        },
        true,
    )
}

/// Paint an `.ibtn`-style icon button from an *already-interacted* response.
///
/// Nested controls must be interacted before the row/card that contains them
/// (the first hovered widget to see the press latches it), so screens that draw
/// a revealed kebab/row button interact first and paint through this helper.
pub fn paint_icon_button(
    ui: &mut Ui,
    rect: Rect,
    icon: Icon,
    icon_size: f32,
    color: Color,
    resp: &Response,
) {
    let theme = ui.theme().clone();
    let t = resp.hover_t;
    let bg = if t > 0.0 {
        theme
            .bg_hover
            .with_alpha((t * 255.0).round().clamp(0.0, 255.0) as u8)
    } else {
        Color::TRANSPARENT
    };
    if bg.a > 0 {
        ui.rounded_rect(rect, bg, theme.radius_sm);
    }
    let icon_color = mix(color, theme.text, t);
    ui.icon(rect, icon, icon_color, icon_size);
    if resp.focus_visible {
        ui.focus_ring(rect, theme.radius_sm);
    }
}

/// A 28×28 `.ibtn` icon button: [`paint_icon_button`] plus its interaction.
pub fn icon_button(
    ui: &mut Ui,
    rect: Rect,
    id: Id,
    icon: Icon,
    icon_size: f32,
    color: Color,
) -> Response {
    let resp = ui.interact(rect, id, Sense::CLICK);
    paint_icon_button(ui, rect, icon, icon_size, color, &resp);
    resp
}

/// A button drawn as visible content (no background), used by `.link` elements.
pub fn link_button(
    ui: &mut Ui,
    rect: Rect,
    id: Id,
    label: &str,
    text_style: &TextStyle,
    align: Align,
) -> Response {
    let theme = ui.theme().clone();
    let resp = ui.interact(rect, id, Sense::CLICK);
    let color = mix(text_style.color, theme.text, resp.hover_t);
    ui.text_aligned(rect, label, &text_style.with_color(color), align);
    if resp.hover_t > 0.5 {
        let w = ui.text_width(label, text_style);
        let x = match align {
            Align::Left => rect.x,
            Align::Center => rect.x + (rect.w - w) * 0.5,
            Align::Right => rect.x + rect.w - w,
        };
        let underline_w = w.min(rect.w);
        ui.hline(x, rect.y + rect.h - 2.0, underline_w, 1.0, color);
    }
    if resp.focus_visible {
        ui.focus_ring(rect, 4.0);
    }
    resp
}

/// Blend two straight-alpha colors (for hover interpolation).
pub fn mix(a: Color, b: Color, t: f32) -> Color {
    let t = t.clamp(0.0, 1.0);
    Color::rgba(
        (a.r as f32 + (b.r as f32 - a.r as f32) * t).round() as u8,
        (a.g as f32 + (b.g as f32 - a.g as f32) * t).round() as u8,
        (a.b as f32 + (b.b as f32 - a.b as f32) * t).round() as u8,
        (a.a as f32 + (b.a as f32 - a.a as f32) * t).round() as u8,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mix_interpolates_colors() {
        let a = Color::rgb(0, 0, 0);
        let b = Color::rgb(255, 255, 255);
        assert_eq!(mix(a, b, 0.0), a);
        assert_eq!(mix(a, b, 1.0), b);
        assert_eq!(mix(a, b, 0.5).r, 128);
    }
}
