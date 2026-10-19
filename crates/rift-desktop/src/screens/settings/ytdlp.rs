//! The Streaming-engine (yt-dlp) row pieces: the status-line flex-wrap layout
//! (badge/version/path chip) and the painter. The layout is shared with the
//! row-height arithmetic so the scrollbar can never disagree with the pixels.

use rift_types::YtDlpStatus;

use crate::screens::cards;
use crate::state::State;
use crate::text::Weight;
use crate::ui::{BadgeKind, Rect, Ui};

use super::layout::{BADGE_H, LINE_13};

/// `.ytdlp-status { display: flex; flex-wrap: wrap; align-items: center; gap: 8px }`.
pub(crate) const STATUS_GAP: f32 = 8.0;

/// Where the `<code class="ytdlp-path">` chip sits inside the found-status
/// block. Mirrors the old flex-wrap layout: inline after the badge/version when
/// the whole chip fits, otherwise the chip moves to its own line below them, and
/// only wraps *inside* the chip (`word-break: break-all`) when it is wider than
/// the row. This keeps long Windows paths readable instead of clipping them
/// into the leftover space next to the badge.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct YtdlpPathSlot {
    /// Offset from the status block's left edge.
    pub(crate) x: f32,
    /// Offset from the status block's top.
    pub(crate) y: f32,
    pub(crate) w: f32,
    pub(crate) h: f32,
    /// Text wraps inside the chip (`word-break: break-all`).
    pub(crate) wrapped: bool,
}

/// `(chip slot, block height)` for a found yt-dlp status.
pub(crate) fn ytdlp_found_layout(
    ui: &mut Ui,
    status: &YtDlpStatus,
    width: f32,
    desc_style: &crate::text::TextStyle,
) -> (Option<YtdlpPathSlot>, f32) {
    let mut meta_right = badgew(ui, "✓ Found") + STATUS_GAP;
    if let Some(version) = status.version.as_deref().filter(|v| !v.is_empty()) {
        meta_right += ui.text_width(version, desc_style) + STATUS_GAP;
    }
    let Some(path) = status.path.as_deref().filter(|p| !p.is_empty()) else {
        return (None, BADGE_H);
    };
    let code_style = cards::style_lh(12.0, Weight::Regular, desc_style.color, 16.0);
    // Chip intrinsic width: text + 6 px horizontal padding on each side.
    let chip_w = ui.text_width(path, &code_style) + 12.0;
    let remaining = (width - meta_right).max(0.0);
    if chip_w <= remaining {
        (
            Some(YtdlpPathSlot {
                x: meta_right,
                y: 2.0,
                w: chip_w,
                h: 16.0,
                wrapped: false,
            }),
            BADGE_H,
        )
    } else if chip_w <= width {
        (
            Some(YtdlpPathSlot {
                x: 0.0,
                y: BADGE_H + STATUS_GAP,
                w: chip_w,
                h: 16.0,
                wrapped: false,
            }),
            BADGE_H + STATUS_GAP + 16.0,
        )
    } else {
        let wrapped_h = ui
            .measure_text_wrapped(path, &code_style, (width - 12.0).max(40.0))
            .h
            + 4.0;
        (
            Some(YtdlpPathSlot {
                x: 0.0,
                y: BADGE_H + STATUS_GAP,
                w: width,
                h: wrapped_h,
                wrapped: true,
            }),
            BADGE_H + STATUS_GAP + wrapped_h,
        )
    }
}

/// `(detail slot, block height)` for a not-found status (`status.path` is kept
/// when a file exists but does not run, making the detail longer).
pub(crate) fn ytdlp_missing_layout(
    ui: &mut Ui,
    status: &YtDlpStatus,
    width: f32,
    desc_style: &crate::text::TextStyle,
) -> (YtdlpPathSlot, f32) {
    let detail = match &status.path {
        Some(path) => format!("Found a file at {path} but it didn't run."),
        None => "Not found on your PATH or in common install locations.".to_string(),
    };
    let badge_right = badgew(ui, "✗ Not found") + STATUS_GAP;
    let remaining = (width - badge_right).max(0.0);
    if ui.text_width(&detail, desc_style) <= remaining {
        (
            YtdlpPathSlot {
                x: badge_right,
                y: (BADGE_H - LINE_13) * 0.5,
                w: remaining,
                h: LINE_13,
                wrapped: false,
            },
            BADGE_H,
        )
    } else {
        let wrapped_h = ui.measure_text_wrapped(&detail, desc_style, width).h;
        (
            YtdlpPathSlot {
                x: 0.0,
                y: BADGE_H + STATUS_GAP,
                w: width,
                h: wrapped_h,
                wrapped: true,
            },
            BADGE_H + STATUS_GAP + wrapped_h,
        )
    }
}

pub(crate) fn ytdlp_status_height(
    status: Option<&YtDlpStatus>,
    ui: &mut Ui,
    width: f32,
    desc_style: &crate::text::TextStyle,
) -> f32 {
    match status {
        None => LINE_13,
        Some(s) if s.found => ytdlp_found_layout(ui, s, width, desc_style).1,
        Some(s) => ytdlp_missing_layout(ui, s, width, desc_style).1,
    }
}

/// Draw the yt-dlp status line (badge + version/detail + path). Returns the y
/// below it.
pub(crate) fn draw_ytdlp_status(
    ui: &mut Ui,
    state: &State,
    x: f32,
    y: f32,
    width: f32,
    theme: &crate::theme::Theme,
    desc_style: &crate::text::TextStyle,
) -> f32 {
    match &state.yt_dlp_status {
        None => {
            // In preview mode there is no backend probe, so "Checking…" would be
            // a lie that never resolves; say so. A live launch probes on mount.
            let text = if state.preview {
                "Not checked yet (no backend in preview mode)"
            } else {
                "Checking…"
            };
            ui.text(Rect::new(x, y, width, LINE_13), text, desc_style);
            y + LINE_13
        }
        Some(status) if status.found => {
            let (slot, block_h) = ytdlp_found_layout(ui, status, width, desc_style);
            let label = "✓ Found";
            let w = badgew(ui, label);
            ui.badge(Rect::new(x, y, w, BADGE_H), label, BadgeKind::Success);
            if let Some(version) = status.version.as_deref().filter(|v| !v.is_empty()) {
                let vw = ui.text_width(version, desc_style);
                ui.text(
                    Rect::new(
                        x + w + STATUS_GAP,
                        y + (BADGE_H - LINE_13) * 0.5,
                        vw,
                        LINE_13,
                    ),
                    version,
                    desc_style,
                );
            }
            if let (Some(slot), Some(path)) =
                (slot, status.path.as_deref().filter(|path| !path.is_empty()))
            {
                let code_style = cards::style_lh(12.0, Weight::Regular, theme.text_dim, 16.0);
                let chip = Rect::new(x + slot.x, y + slot.y, slot.w, slot.h);
                ui.rounded_rect(chip, theme.bg, 6.0);
                ui.bordered_rect(chip, crate::ui::Color::TRANSPARENT, 6.0, 1.0, theme.border);
                if slot.wrapped {
                    ui.text_wrapped(
                        Rect::new(
                            chip.x + 6.0,
                            chip.y + 4.0,
                            (chip.w - 12.0).max(1.0),
                            (chip.h - 4.0).max(LINE_13),
                        ),
                        path,
                        &code_style,
                    );
                } else {
                    ui.text(
                        Rect::new(
                            chip.x + 6.0,
                            chip.y + (chip.h - LINE_13) * 0.5,
                            (chip.w - 12.0).max(1.0),
                            LINE_13,
                        ),
                        path,
                        &code_style,
                    );
                }
            }
            y + block_h
        }
        Some(status) => {
            let (slot, block_h) = ytdlp_missing_layout(ui, status, width, desc_style);
            let label = "✗ Not found";
            let w = badgew(ui, label);
            ui.badge(Rect::new(x, y, w, BADGE_H), label, BadgeKind::Danger);
            let detail = match &status.path {
                Some(path) => format!("Found a file at {path} but it didn't run."),
                None => "Not found on your PATH or in common install locations.".to_string(),
            };
            ui.text_wrapped(
                Rect::new(x + slot.x, y + slot.y, slot.w, slot.h),
                &detail,
                desc_style,
            );
            y + block_h
        }
    }
}

pub(crate) fn badgew(ui: &mut Ui, label: &str) -> f32 {
    ui.text_width(
        label,
        &cards::style(12.0, Weight::SemiBold, crate::ui::Color::WHITE),
    ) + 16.0
}
