//! Settings interaction/layout tests: switches, the crossfade slider, the update
//! buttons and the row-height arithmetic.

use super::*;

use crate::state::{Action, State};
use crate::ui::Rect;

use crate::screens::cards;
use crate::text::Weight;
use crate::ui::Ui;

use super::layout::{
    height, row_heights, BADGE_H, BLOCK_GAP, BTN_H, DESC_GAP, LABEL_H, LINE_13, ROW_MAX_W,
    ROW_PAD_X, ROW_PAD_Y, ROW_SPACING, SWITCH_W, TITLE_BLOCK_H,
};
use super::view::{row_text_w, view};

use crate::screens::menu::harness::Sim;

/// Layout of the three settings rows for a state (same arithmetic as `view`).
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
fn check_now_emits_check_update() {
    let mut state = State::default();
    let area = Rect::new(359.0, 66.0, 767.0, 640.0);
    let mut sim = Sim::new();
    let theme = sim.ui.theme().clone();
    let widths = area.w.min(ROW_MAX_W);
    let heights = row_heights(&mut sim.ui, &state, area.w);
    let updates_y = area.y + TITLE_BLOCK_H + heights[0] + ROW_SPACING + heights[1] + ROW_SPACING;
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
    let check_now_y = updates_y
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
    let mut draw = |ui: &mut Ui, actions: &mut Vec<Action>| {
        view(ui, &mut state, actions, area);
    };
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
    let updates_y = area.y + TITLE_BLOCK_H + heights[0] + ROW_SPACING + heights[1] + ROW_SPACING;
    let actions_y = updates_y
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
fn row_heights_are_padding_plus_content() {
    let mut ui = Ui::new();
    let state = State::default();
    let rows = row_heights(&mut ui, &state, 766.0);
    // Three rows: Discord, Crossfade, Updates (there is no streaming-engine row).
    assert_eq!(rows.len(), 3);
    for h in rows {
        assert!(h >= ROW_PAD_Y * 2.0 + LABEL_H, "row too short: {h}");
    }
    let total = height(&mut ui, &state, 766.0);
    let sum: f32 = rows.iter().sum::<f32>() + ROW_SPACING * 2.0 + TITLE_BLOCK_H;
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
