//! Settings interaction/layout tests: switches, crossfade slider, yt-dlp and
//! update buttons, row-height arithmetic and the yt-dlp path wrap rules.

use super::*;

use crate::state::{Action, State};
use crate::ui::Rect;
use rift_types::YtDlpStatus;

use crate::screens::cards;
use crate::text::Weight;
use crate::ui::Ui;

use super::layout::{
    height, row_heights, BADGE_H, BLOCK_GAP, BTN_H, DESC_GAP, LABEL_H, LINE_13, ROW_MAX_W,
    ROW_PAD_X, ROW_PAD_Y, ROW_SPACING, SWITCH_W, TITLE_BLOCK_H,
};
use super::state::set_downloading;
use super::view::{row_text_w, view};
use super::ytdlp::{ytdlp_found_layout, ytdlp_missing_layout, ytdlp_status_height, STATUS_GAP};

use crate::screens::menu::harness::Sim;

/// Layout of the four settings rows for a state (same arithmetic as `view`).
fn settings_layout(ui: &mut Ui, state: &State, area: Rect) -> (f32, Vec<f32>, f32) {
    let width = area.w.min(ROW_MAX_W);
    let heights = row_heights(ui, state, area.w);
    let mut ys = Vec::new();
    let mut y = area.y + TITLE_BLOCK_H;
    for h in &heights {
        ys.push(y);
        y += h + ROW_SPACING;
    }
    (width, ys, y)
}

#[test]
fn discord_switch_emits_set_discord_rpc() {
    let mut state = State::default();
    let area = Rect::new(359.0, 66.0, 767.0, 640.0);
    let mut sim = Sim::new();
    let (width, ys, _) = settings_layout(&mut sim.ui, &state, area);
    let row = Rect::new(area.x, ys[0], width, 0.0);
    let switch = (
        area.x + width - ROW_PAD_X - SWITCH_W * 0.5,
        ys[0] + (row_heights(&mut sim.ui, &state, area.w)[0]) * 0.5,
    );
    let mut draw = |ui: &mut Ui, actions: &mut Vec<Action>| {
        view(ui, &mut state, actions, area);
    };
    let actions = sim.click(switch, &mut draw);
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, Action::SetDiscordRpc(false))),
        "Discord switch (on -> off): {actions:?} (row y {})",
        row.y
    );
}

#[test]
fn crossfade_slider_emits_set_crossfade() {
    let mut state = State {
        crossfade: 6.0,
        ..State::default()
    };
    let area = Rect::new(359.0, 66.0, 767.0, 640.0);
    let mut sim = Sim::new();
    let theme = sim.ui.theme().clone();
    let widths = area.w.min(ROW_MAX_W);
    let heights = row_heights(&mut sim.ui, &state, area.w);
    let row2_y = area.y + TITLE_BLOCK_H + heights[0] + ROW_SPACING;
    let text_w = row_text_w(widths, SWITCH_W);
    let desc_h = sim
        .ui
        .measure_text_wrapped(
            "Overlap the end of each track with the start of the next for a smooth transition.",
            &cards::style_lh(13.0, Weight::Regular, theme.text_dim, LINE_13),
            text_w,
        )
        .h;
    let slider_y = row2_y + ROW_PAD_Y + LABEL_H + DESC_GAP + desc_h + BLOCK_GAP + 2.0;
    let slider_x = area.x + ROW_PAD_X;
    let mut draw = |ui: &mut Ui, actions: &mut Vec<Action>| {
        view(ui, &mut state, actions, area);
    };
    // Click mid-slider: 1 + 11 * 0.5 = 6.5 -> step 1 rounds to 7 s.
    let actions = sim.click((slider_x + 80.0, slider_y), &mut draw);
    let set = actions
        .iter()
        .find_map(|a| match a {
            Action::SetCrossfade(v) => Some(*v),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no SetCrossfade: {actions:?}"));
    assert!((6.0..=8.0).contains(&set), "crossfade value {set}");
}

#[test]
fn check_again_and_check_now_emit_their_actions() {
    let mut state = State::default();
    let area = Rect::new(359.0, 66.0, 767.0, 640.0);
    let mut sim = Sim::new();
    let widths = area.w.min(ROW_MAX_W);
    let heights = row_heights(&mut sim.ui, &state, area.w);
    let row3_y = area.y + TITLE_BLOCK_H + heights[0] + ROW_SPACING + heights[1] + ROW_SPACING;
    let check_w = {
        let style = cards::style(13.0, Weight::SemiBold, crate::ui::Color::WHITE);
        sim.ui.text_width("Check again", &style) + 28.0
    };
    let check_again = (
        area.x + widths - ROW_PAD_X - check_w * 0.5,
        row3_y + heights[2] * 0.5,
    );
    let mut draw = |ui: &mut Ui, actions: &mut Vec<Action>| {
        view(ui, &mut state, actions, area);
    };
    let actions = sim.click(check_again, &mut draw);
    assert!(
        actions.iter().any(|a| matches!(a, Action::CheckYtDlp)),
        "Check again: {actions:?}"
    );

    // Updates row: "Check now" sits under the description + status line.
    let theme = sim.ui.theme().clone();
    let desc_style = cards::style_lh(13.0, Weight::Regular, theme.text_dim, LINE_13);
    let text_w = row_text_w(widths, SWITCH_W);
    let desc_h = sim
        .ui
        .measure_text_wrapped(
            "Check GitHub for a newer version of Rift on launch and notify you.",
            &desc_style,
            text_w,
        )
        .h;
    let row4_y = row3_y + heights[2] + ROW_SPACING;
    let check_now_y = row4_y
        + ROW_PAD_Y
        + LABEL_H
        + DESC_GAP
        + desc_h
        + BLOCK_GAP
        + BADGE_H
        + BLOCK_GAP
        + BTN_H * 0.5;
    let check_now_w = {
        let style = cards::style(13.0, Weight::SemiBold, crate::ui::Color::WHITE);
        sim.ui.text_width("Check now", &style) + 28.0
    };
    let check_now = (area.x + ROW_PAD_X + check_now_w * 0.5, check_now_y);
    let actions = sim.click(check_now, &mut draw);
    assert!(
        actions.iter().any(|a| matches!(a, Action::CheckUpdate)),
        "Check now: {actions:?}"
    );
}

#[test]
fn update_available_offers_a_download_link() {
    let mut state = State {
        update_status: Some(rift_types::UpdateStatus {
            current: "0.1.0".into(),
            latest: Some("0.2.0".into()),
            update_available: true,
            url: Some("https://example.invalid/release".into()),
        }),
        ..State::default()
    };
    let area = Rect::new(359.0, 66.0, 767.0, 640.0);
    let mut sim = Sim::new();
    let theme = sim.ui.theme().clone();
    let widths = area.w.min(ROW_MAX_W);
    let heights = row_heights(&mut sim.ui, &state, area.w);
    let text_w = row_text_w(widths, SWITCH_W);
    let desc_style = cards::style_lh(13.0, Weight::Regular, theme.text_dim, LINE_13);
    let desc_h = sim
        .ui
        .measure_text_wrapped(
            "Check GitHub for a newer version of Rift on launch and notify you.",
            &desc_style,
            text_w,
        )
        .h;
    let row4_y = area.y
        + TITLE_BLOCK_H
        + heights[0]
        + ROW_SPACING
        + heights[1]
        + ROW_SPACING
        + heights[2]
        + ROW_SPACING;
    let actions_y = row4_y
        + ROW_PAD_Y
        + LABEL_H
        + DESC_GAP
        + desc_h
        + BLOCK_GAP
        + BADGE_H
        + BLOCK_GAP
        + BTN_H * 0.5;
    let check_w = {
        let style = cards::style(13.0, Weight::SemiBold, crate::ui::Color::WHITE);
        sim.ui.text_width("Check now", &style) + 28.0
    };
    let dl_w = {
        let style = cards::style(13.0, Weight::SemiBold, crate::ui::Color::WHITE);
        sim.ui.text_width("Download update", &style) + 28.0
    };
    let download = (area.x + ROW_PAD_X + check_w + 10.0 + dl_w * 0.5, actions_y);
    let mut draw = |ui: &mut Ui, actions: &mut Vec<Action>| {
        view(ui, &mut state, actions, area);
    };
    let actions = sim.click(download, &mut draw);
    assert!(
        actions.iter().any(|a| matches!(
            a,
            Action::OpenUrl(url) if url == "https://example.invalid/release"
        )),
        "Download update: {actions:?}"
    );
}

#[test]
fn missing_ytdlp_offers_download() {
    set_downloading(false);
    let mut state = State {
        yt_dlp_status: Some(rift_types::YtDlpStatus {
            found: false,
            path: None,
            version: None,
        }),
        ..State::default()
    };
    let area = Rect::new(359.0, 66.0, 767.0, 640.0);
    let mut sim = Sim::new();
    let theme = sim.ui.theme().clone();
    let widths = area.w.min(ROW_MAX_W);
    let heights = row_heights(&mut sim.ui, &state, area.w);
    let row3_y = area.y + TITLE_BLOCK_H + heights[0] + ROW_SPACING + heights[1] + ROW_SPACING;
    let check_w = {
        let style = cards::style(13.0, Weight::SemiBold, crate::ui::Color::WHITE);
        sim.ui.text_width("Check again", &style) + 28.0
    };
    let text_w = row_text_w(widths, check_w);
    let desc_style = cards::style_lh(13.0, Weight::Regular, theme.text_dim, LINE_13);
    let desc_h = sim
            .ui
            .measure_text_wrapped(
                "Rift uses yt-dlp to fetch audio. It's auto-detected from your PATH and common install locations; set a custom path below if it lives elsewhere.",
                &desc_style,
                text_w,
            )
            .h;
    let status_h = ytdlp_status_height(
        state.yt_dlp_status.as_ref(),
        &mut sim.ui,
        text_w,
        &desc_style,
    );
    let btn_y = row3_y + ROW_PAD_Y + LABEL_H + DESC_GAP + desc_h + BLOCK_GAP + status_h + BLOCK_GAP;
    let dl_w = {
        let style = cards::style(13.0, Weight::SemiBold, crate::ui::Color::WHITE);
        sim.ui.text_width("Download yt-dlp", &style) + 28.0
    };
    let download = (area.x + ROW_PAD_X + dl_w * 0.5, btn_y + BTN_H * 0.5);
    let mut draw = |ui: &mut Ui, actions: &mut Vec<Action>| {
        view(ui, &mut state, actions, area);
    };
    let actions = sim.click(download, &mut draw);
    assert!(
        actions.iter().any(|a| matches!(a, Action::DownloadYtDlp)),
        "Download yt-dlp: {actions:?}"
    );
    set_downloading(false);
}
#[test]
fn row_heights_are_padding_plus_content() {
    let mut ui = Ui::new();
    let state = State::default();
    let rows = row_heights(&mut ui, &state, 766.0);
    for h in rows {
        assert!(h >= ROW_PAD_Y * 2.0 + LABEL_H, "row too short: {h}");
    }
    let total = height(&mut ui, &state, 766.0);
    let sum: f32 = rows.iter().sum::<f32>() + ROW_SPACING * 3.0 + TITLE_BLOCK_H;
    assert_eq!(total, sum);
}

#[test]
fn crossfade_row_grows_when_enabled() {
    let mut ui = Ui::new();
    let mut state = State {
        crossfade: 0.0,
        ..State::default()
    };
    let off = row_heights(&mut ui, &state, 766.0)[1];
    state.crossfade = 6.0;
    let on = row_heights(&mut ui, &state, 766.0)[1];
    assert_eq!(on - off, BLOCK_GAP + 20.0);
}

#[test]
fn control_sizes_match_the_css() {
    assert_eq!(BTN_H, 33.0);
    assert_eq!(SWITCH_W, 44.0);
    assert_eq!(SWITCH_H, 24.0);
    assert_eq!(ROW_MAX_W, 640.0);
}

fn desc_style_of(ui: &Ui) -> crate::text::TextStyle {
    let theme = ui.theme().clone();
    cards::style_lh(13.0, Weight::Regular, theme.text_dim, LINE_13)
}

fn long_path_status() -> YtDlpStatus {
    YtDlpStatus {
        found: true,
        path: Some(r"C:\Users\test\AppData\Roaming\dev.jerimiah.rift\bin\yt-dlp.exe".into()),
        version: Some("2026.10.19".into()),
    }
}

/// The resolved path used to be squeezed into the leftover space after the
/// badge/version and wrapped inside a clipped 2-line box. The old flex layout
/// moved the whole chip below them instead; pin that.
#[test]
fn long_ytdlp_path_moves_to_its_own_line() {
    let mut ui = Ui::new();
    let desc = desc_style_of(&ui);
    // Typical status-line width inside the 640 px settings row.
    let width = 500.0;
    let (slot, block_h) = ytdlp_found_layout(&mut ui, &long_path_status(), width, &desc);
    let slot = slot.expect("found status with a path has a chip slot");
    assert_eq!(
        slot.y,
        BADGE_H + STATUS_GAP,
        "the chip drops below badge/version when it doesn't fit beside them"
    );
    assert_eq!(slot.x, 0.0);
    assert!(!slot.wrapped);
    assert!(slot.w <= width);
    assert_eq!(block_h, BADGE_H + STATUS_GAP + slot.h);

    // A short path keeps the compact single-line layout.
    let short = YtDlpStatus {
        found: true,
        path: Some(r"C:\yt-dlp.exe".into()),
        version: Some("2026.10.19".into()),
    };
    let (slot, block_h) = ytdlp_found_layout(&mut ui, &short, width, &desc);
    assert_eq!(slot.expect("short path slot").y, 2.0);
    assert_eq!(block_h, BADGE_H);
}

/// A path wider than the row wraps inside its own chip (`word-break:
/// break-all`) instead of being clipped.
#[test]
fn overwide_ytdlp_path_wraps_inside_the_chip() {
    let mut ui = Ui::new();
    let desc = desc_style_of(&ui);
    let (slot, block_h) = ytdlp_found_layout(&mut ui, &long_path_status(), 180.0, &desc);
    let slot = slot.expect("slot");
    assert!(slot.wrapped, "an over-wide path wraps inside the chip");
    assert_eq!(slot.w, 180.0);
    assert!(slot.h > 16.0, "wrapped chip is taller than one line");
    assert_eq!(block_h, BADGE_H + STATUS_GAP + slot.h);
}

/// The not-found detail (which includes the resolved path when a file exists
/// but fails to run) follows the same wrap rule.
#[test]
fn long_not_found_detail_wraps_below_the_badge() {
    let mut ui = Ui::new();
    let desc = desc_style_of(&ui);
    let status = YtDlpStatus {
        found: false,
        path: Some(r"C:\Users\test\AppData\Roaming\dev.jerimiah.rift\bin\yt-dlp.exe".into()),
        version: None,
    };
    let (slot, block_h) = ytdlp_missing_layout(&mut ui, &status, 300.0, &desc);
    assert_eq!(slot.y, BADGE_H + STATUS_GAP);
    assert!(slot.wrapped);
    assert!(block_h > BADGE_H + STATUS_GAP);
}

/// The row-height math and the painter must agree even when the path makes
/// the status block taller (otherwise the next row overlaps it).
#[test]
fn ytdlp_row_height_accounts_for_a_wrapped_path() {
    let mut ui = Ui::new();
    let desc = desc_style_of(&ui);
    let short = YtDlpStatus {
        found: true,
        path: Some(r"C:\yt-dlp.exe".into()),
        version: Some("2026.10.19".into()),
    };
    let short_h = ytdlp_status_height(Some(&short), &mut ui, 500.0, &desc);
    let long_h = ytdlp_status_height(Some(&long_path_status()), &mut ui, 500.0, &desc);
    assert!(long_h > short_h);
    assert_eq!(
        long_h,
        ytdlp_found_layout(&mut ui, &long_path_status(), 500.0, &desc).1
    );
}

/// Render the resolved yt-dlp status (badge, version, path chip) through the
/// real GPU for visual inspection:
/// `cargo test -p rift-desktop --lib ytdlp_status_visual_png -- --ignored`.
#[test]
#[ignore = "renders a PNG with the real GPU; run with --ignored for visual inspection"]
fn ytdlp_status_visual_png() {
    use crate::app::App;
    use crate::controller::Controller;
    use crate::state::View;
    use crate::ui::InputState;

    let mut app = App::new(true, Controller::new_preview());
    app.state.view = View::Settings;
    app.state.yt_dlp_status = Some(YtDlpStatus {
        found: true,
        path: Some(r"C:\Users\test\AppData\Roaming\dev.jerimiah.rift\bin\yt-dlp.exe".into()),
        version: Some("2026.10.19".into()),
    });
    app.tick();
    let input = InputState {
        viewport: Rect::new(0.0, 0.0, 1160.0, 740.0),
        scale_factor: 1.0,
        time: 1.0,
        mouse_pos: (-100.0, -100.0),
        ..InputState::default()
    };
    let paint = app.frame(input);
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let path = root.join("artifacts/screens/settings-ytdlp-status.png");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    crate::gfx::screenshot(1160, 740, 1.0, &paint, &path).unwrap();
    assert!(path.is_file(), "wrote {}", path.display());
}
