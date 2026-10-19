//! The Settings view painter: the four `.settings-row` cards, their controls and
//! the h2 title.

use crate::screens::cards;
use crate::state::{Action, State};
use crate::text::Weight;
use crate::ui::{Id, Padding, Rect, SliderStyle, TextInputStyle, Ui};

use super::layout::{
    btn_width, is_missing, row_heights, BLOCK_GAP, BTN_H, DESC_GAP, LABEL_H, LINE_13, ROW_GAP,
    ROW_MAX_W, ROW_PAD_X, ROW_PAD_Y, ROW_RADIUS, ROW_SPACING, SWITCH_H, SWITCH_W, TITLE_BLOCK_H,
};
use super::state::{downloading, path_draft, set_downloading, store_path_draft, PATH_DRAFT};
use super::updates::draw_update_status;
use super::ytdlp::draw_ytdlp_status;

// ---------------------------------------------------------------------------
// drawing
// ---------------------------------------------------------------------------

/// Draw the Settings view at `area`.
pub fn view(ui: &mut Ui, state: &mut State, actions: &mut Vec<Action>, area: Rect) {
    let theme = ui.theme().clone();
    let width = area.w.min(ROW_MAX_W);

    // ---- h2 "Settings" ----
    ui.text(
        Rect::new(area.x, area.y, area.w, 22.0),
        "Settings",
        &cards::style_lh(17.0, Weight::Bold, theme.text, 22.0),
    );
    let mut y = area.y + TITLE_BLOCK_H;
    let heights = row_heights(ui, state, area.w);

    let label_style = cards::style_lh(15.0, Weight::SemiBold, theme.text, LABEL_H);
    let desc_style = cards::style_lh(13.0, Weight::Regular, theme.text_dim, LINE_13);
    let meta_style = cards::style_lh(13.0, Weight::Regular, theme.text_dim, LINE_13);

    // =====================================================================
    // 1. Discord Rich Presence
    // =====================================================================
    let rect = Rect::new(area.x, y, width, heights[0]);
    draw_row_frame(ui, rect, &theme);
    let text_w = row_text_w(rect.w, SWITCH_W);
    let inner_x = rect.x + ROW_PAD_X;
    let inner_y = rect.y + ROW_PAD_Y;
    ui.text(
        Rect::new(inner_x, inner_y, text_w, LABEL_H),
        "Discord Rich Presence",
        &label_style,
    );
    let desc_y = inner_y + LABEL_H + DESC_GAP;
    ui.text_wrapped(
        Rect::new(inner_x, desc_y, text_w, LINE_13 * 3.0),
        "Show the track you're listening to on your Discord profile.",
        &desc_style,
    );
    {
        let mut on = state.discord_rpc;
        let switch = Rect::new(
            rect.right() - ROW_PAD_X - SWITCH_W,
            rect.y + (rect.h - SWITCH_H) * 0.5,
            SWITCH_W,
            SWITCH_H,
        );
        if ui.switch(switch, &mut on).changed {
            actions.push(Action::SetDiscordRpc(on));
        }
    }
    y += heights[0] + ROW_SPACING;

    // =====================================================================
    // 2. Crossfade
    // =====================================================================
    let rect = Rect::new(area.x, y, width, heights[1]);
    draw_row_frame(ui, rect, &theme);
    let text_w = row_text_w(rect.w, SWITCH_W);
    let inner_x = rect.x + ROW_PAD_X;
    let inner_y = rect.y + ROW_PAD_Y;
    ui.text(
        Rect::new(inner_x, inner_y, text_w, LABEL_H),
        "Crossfade",
        &label_style,
    );
    let desc_y = inner_y + LABEL_H + DESC_GAP;
    let desc_h = ui
        .measure_text_wrapped(
            "Overlap the end of each track with the start of the next for a smooth transition.",
            &desc_style,
            text_w,
        )
        .h;
    ui.text_wrapped(
        Rect::new(inner_x, desc_y, text_w, desc_h),
        "Overlap the end of each track with the start of the next for a smooth transition.",
        &desc_style,
    );
    let mut crossfade_on = state.crossfade > 0.0;
    if crossfade_on {
        let control_y = desc_y + desc_h + BLOCK_GAP;
        let mut value = state.crossfade.clamp(1.0, 12.0);
        let slider_style = SliderStyle::from_theme(&theme).with_step(1.0);
        let resp = ui.slider_styled(
            Id::from_str("settings/crossfade"),
            Rect::new(inner_x, control_y, 160.0, 4.0),
            &mut value,
            1.0,
            12.0,
            &slider_style,
        );
        if resp.changed {
            actions.push(Action::SetCrossfade(value.round()));
        }
        ui.text(
            Rect::new(
                inner_x + 160.0 + 10.0,
                control_y + 4.0 - LINE_13 * 0.5,
                32.0,
                LINE_13,
            ),
            &format!("{}s", state.crossfade.round() as i32),
            &meta_style,
        );
    }
    {
        let switch = Rect::new(
            rect.right() - ROW_PAD_X - SWITCH_W,
            rect.y + (rect.h - SWITCH_H) * 0.5,
            SWITCH_W,
            SWITCH_H,
        );
        if ui.switch(switch, &mut crossfade_on).changed {
            actions.push(if crossfade_on {
                Action::SetCrossfade(6.0)
            } else {
                Action::SetCrossfade(0.0)
            });
        }
    }
    y += heights[1] + ROW_SPACING;

    // =====================================================================
    // 3. Streaming engine (yt-dlp)
    // =====================================================================
    let rect = Rect::new(area.x, y, width, heights[2]);
    draw_row_frame(ui, rect, &theme);
    let check_w = btn_width(ui, "Check again");
    let text_w = row_text_w(rect.w, check_w);
    let inner_x = rect.x + ROW_PAD_X;
    let inner_y = rect.y + ROW_PAD_Y;
    ui.text(
        Rect::new(inner_x, inner_y, text_w, LABEL_H),
        "Streaming engine (yt-dlp)",
        &label_style,
    );
    let mut cy = inner_y + LABEL_H + DESC_GAP;
    let desc_h = ui
        .measure_text_wrapped(
            "Rift uses yt-dlp to fetch audio. It's auto-detected from your PATH and common install locations; set a custom path below if it lives elsewhere.",
            &desc_style,
            text_w,
        )
        .h;
    ui.text_wrapped(
        Rect::new(inner_x, cy, text_w, desc_h),
        "Rift uses yt-dlp to fetch audio. It's auto-detected from your PATH and common install locations; set a custom path below if it lives elsewhere.",
        &desc_style,
    );
    cy += desc_h + BLOCK_GAP;
    cy = draw_ytdlp_status(ui, state, inner_x, cy, text_w, &theme, &desc_style);
    if is_missing(state.yt_dlp_status.as_ref()) {
        cy += BLOCK_GAP;
        let label = if downloading() {
            "Downloading…"
        } else {
            "Download yt-dlp"
        };
        let w = btn_width(ui, label);
        let resp = cards::primary_button(
            ui,
            Rect::new(inner_x, cy, w, BTN_H),
            Id::from_str("settings/ytdlp-download"),
            label,
            !downloading(),
        );
        ui.text(
            Rect::new(
                inner_x + w + 10.0,
                cy + (BTN_H - 16.0) * 0.5,
                (text_w - w - 10.0).max(0.0),
                16.0,
            ),
            "Fetches the latest build into Rift's data folder.",
            &cards::style_lh(12.0, Weight::Regular, theme.text_dim, 16.0),
        );
        if resp.clicked {
            set_downloading(true);
            actions.push(Action::DownloadYtDlp);
        }
        // Once a probe comes back, the command has resolved.
        if state.yt_dlp_status.is_some() {
            set_downloading(false);
        }
        cy += BTN_H;
    }
    // custom path row
    cy += BLOCK_GAP;
    {
        let mut draft = path_draft(state);
        let input_w = text_w.min(360.0);
        let mut style = TextInputStyle::default_theme(&theme);
        style.bg = theme.bg;
        style.padding = Padding::symmetric(10.0, 7.0);
        style.text = cards::style_lh(13.0, Weight::Regular, theme.text, LINE_13);
        let resp = ui.text_input(
            Rect::new(inner_x, cy, input_w, BTN_H),
            Id::from_str("settings/ytdlp-path"),
            &mut draft,
            "Custom yt-dlp path (leave blank to auto-detect)",
            &style,
        );
        store_path_draft(draft.clone());
        let save_x = inner_x + input_w + 8.0;
        let save_w = btn_width(ui, "Save");
        let save_resp = cards::secondary_button(
            ui,
            Rect::new(save_x, cy, save_w, BTN_H),
            Id::from_str("settings/ytdlp-save"),
            "Save",
        );
        if resp.submitted || save_resp.clicked {
            let trimmed = draft.trim().to_string();
            actions.push(Action::SetYtDlpPath(if trimmed.is_empty() {
                None
            } else {
                Some(trimmed)
            }));
        }
    }
    // right column: "Check again"
    {
        let w = btn_width(ui, "Check again");
        let resp = cards::secondary_button(
            ui,
            Rect::new(
                rect.right() - ROW_PAD_X - w,
                rect.y + (rect.h - BTN_H) * 0.5,
                w,
                BTN_H,
            ),
            Id::from_str("settings/ytdlp-check"),
            "Check again",
        );
        if resp.clicked {
            PATH_DRAFT.lock().unwrap_or_else(|e| e.into_inner()).take();
            actions.push(Action::CheckYtDlp);
        }
    }
    y += heights[2] + ROW_SPACING;

    // =====================================================================
    // 4. Updates
    // =====================================================================
    let rect = Rect::new(area.x, y, width, heights[3]);
    draw_row_frame(ui, rect, &theme);
    let text_w = row_text_w(rect.w, SWITCH_W);
    let inner_x = rect.x + ROW_PAD_X;
    let inner_y = rect.y + ROW_PAD_Y;
    ui.text(
        Rect::new(inner_x, inner_y, text_w, LABEL_H),
        "Updates",
        &label_style,
    );
    let mut cy = inner_y + LABEL_H + DESC_GAP;
    let desc_h = ui
        .measure_text_wrapped(
            "Check GitHub for a newer version of Rift on launch and notify you.",
            &desc_style,
            text_w,
        )
        .h;
    ui.text_wrapped(
        Rect::new(inner_x, cy, text_w, desc_h),
        "Check GitHub for a newer version of Rift on launch and notify you.",
        &desc_style,
    );
    cy += desc_h + BLOCK_GAP;
    cy = draw_update_status(ui, state, inner_x, cy, text_w, &meta_style);
    cy += BLOCK_GAP;
    {
        let w = btn_width(ui, "Check now");
        let resp = cards::secondary_button(
            ui,
            Rect::new(inner_x, cy, w, BTN_H),
            Id::from_str("settings/check-update"),
            "Check now",
        );
        if resp.clicked {
            actions.push(Action::CheckUpdate);
        }
        let mut x = inner_x + w + 10.0;
        if state
            .update_status
            .as_ref()
            .is_some_and(|u| u.update_available)
        {
            let url = state.update_status.as_ref().and_then(|u| u.url.clone());
            let label = "Download update";
            let w = btn_width(ui, label);
            let resp = cards::primary_button(
                ui,
                Rect::new(x, cy, w, BTN_H),
                Id::from_str("settings/download-update"),
                label,
                true,
            );
            if resp.clicked {
                if let Some(url) = url {
                    actions.push(Action::OpenUrl(url));
                }
            }
            x += w;
        }
        let _ = x;
    }
    {
        let mut on = state.update_notifications;
        let switch = Rect::new(
            rect.right() - ROW_PAD_X - SWITCH_W,
            rect.y + (rect.h - SWITCH_H) * 0.5,
            SWITCH_W,
            SWITCH_H,
        );
        if ui.switch(switch, &mut on).changed {
            actions.push(Action::SetUpdateNotifications(on));
        }
    }
}

fn draw_row_frame(ui: &mut Ui, rect: Rect, theme: &crate::theme::Theme) {
    ui.bordered_rect(rect, theme.bg_raised, ROW_RADIUS, 1.0, theme.border);
}

pub(super) fn row_text_w(row_w: f32, control_w: f32) -> f32 {
    (row_w - ROW_PAD_X * 2.0 - ROW_GAP - control_w).max(0.0)
}
