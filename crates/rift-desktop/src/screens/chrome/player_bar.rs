//! The bottom player bar: seek row + now-playing / transport / volume row.

use rift_types::{PlaybackState, RepeatMode};

use crate::screens::cards;
use crate::state::{Action, State};
use crate::text::Weight;
use crate::theme::Theme;
use crate::ui::{Color, CursorShape, Icon, Id, Rect, Sense, Ui};

/// Player bar internals (ui-spec.md §6.3).
const SEEK_ROW_H: f32 = 21.0;
const TIME_W: f32 = 34.0;
const SEEK_GAP: f32 = 10.0;
const PLAYER_PAD_X: f32 = 16.0;
const NP_COVER: f32 = 44.0;
const NP_GAP: f32 = 10.0;
const CTRL_SIZE: f32 = 36.0;
const CTRL_GAP: f32 = 8.0;
const CTRLS_W: f32 = CTRL_SIZE * 5.0 + CTRL_GAP * 4.0; // 212
const VOL_SLIDER_W: f32 = 110.0;

// ---------------------------------------------------------------------------
// player bar
// ---------------------------------------------------------------------------

/// The bottom player bar: seek row + now-playing / transport / volume row.
pub fn player_bar(ui: &mut Ui, state: &mut State, actions: &mut Vec<Action>, rect: Rect) {
    let theme: Theme = ui.theme().clone();
    ui.fill(rect, theme.player_bg);
    ui.hline(rect.x, rect.y, rect.w, 1.0, theme.border);

    // ---- seek row (y=657..678 at the reference size) ----
    let seek_row = Rect::new(rect.x, rect.y + 1.0, rect.w, SEEK_ROW_H);
    seek(ui, state, actions, seek_row, &theme);

    // ---- player row (y=678..740 at the reference size) ----
    let row = Rect::new(
        rect.x,
        seek_row.bottom(),
        rect.w,
        (rect.bottom() - seek_row.bottom()).max(0.0),
    );
    let content = Rect::new(
        row.x + PLAYER_PAD_X,
        row.y,
        (row.w - PLAYER_PAD_X * 2.0).max(0.0),
        row.h,
    );
    let cover_y = row.y + 6.0;
    let row_center_y = cover_y + NP_COVER * 0.5;

    now_playing(ui, state, actions, content, cover_y, row_center_y, &theme);
    transport_controls(ui, state, actions, rect, cover_y, &theme);
    volume(ui, state, actions, content, row_center_y, &theme);
}

/// The seek row: current/duration labels and the scrubber. `position` is shown
/// from the pointer while dragging and from `state.progress` otherwise.
fn seek(ui: &mut Ui, state: &mut State, actions: &mut Vec<Action>, row: Rect, theme: &Theme) {
    let time_y = row.y + 6.0;
    let duration = state.progress.duration.max(1.0);
    let fraction = (state.progress.position / duration).clamp(0.0, 1.0) as f32;
    let time_style = cards::style_lh(11.0, Weight::Regular, theme.text_dim, 15.0);

    let left_time = Rect::new(row.x + 14.0, time_y, TIME_W, 15.0);
    let right_time = Rect::new(row.right() - 14.0 - TIME_W, time_y, TIME_W, 15.0);
    let slider_rect = Rect::new(
        left_time.right() + SEEK_GAP,
        time_y,
        (right_time.x - SEEK_GAP - (left_time.right() + SEEK_GAP)).max(10.0),
        16.0,
    );
    let seek = cards::slider(
        ui,
        slider_rect,
        Id::from_str("player/seek"),
        fraction,
        theme,
    );
    let shown_fraction = seek.dragged_fraction.unwrap_or(fraction);
    let shown_pos = if seek.dragged_fraction.is_some() {
        shown_fraction as f64 * duration
    } else {
        state.progress.position
    };
    ui.text_centered(left_time, &crate::state::fmt_time(shown_pos), &time_style);
    ui.text_centered(
        right_time,
        &crate::state::fmt_time(state.progress.duration),
        &time_style,
    );
    // Emit the release position: `dragged_fraction` only exists while the
    // slider is held, so the commit frame carries its own fraction.
    if let Some(f) = seek.committed_fraction {
        actions.push(Action::Seek(f as f64 * duration));
    }
}

/// The now-playing block: cover / title / artist link / like. Empty while no
/// track is loaded (`ui-spec.md` §6.3).
#[allow(clippy::too_many_arguments)]
fn now_playing(
    ui: &mut Ui,
    state: &mut State,
    actions: &mut Vec<Action>,
    content: Rect,
    cover_y: f32,
    row_center_y: f32,
    theme: &Theme,
) {
    // Take the track out for the frame so the cover request (`&mut State`) can
    // run while `track` is owned; put it back before returning. Cloning the
    // whole track every frame (it has three `String`s and an artist list) would
    // allocate on the hot path, and nothing else in the frame reads it.
    let Some(track) = state.track.take() else {
        return;
    };

    let title_style = cards::style_lh(13.0, Weight::Medium, theme.text, 17.0);
    let artist_style = cards::style_lh(12.0, Weight::Regular, theme.text_dim, 16.0);
    let title_w = ui.text_width(&track.title, &title_style);
    let artist_w = ui.text_width(&track.artist, &artist_style);
    let meta_w = title_w.max(artist_w);

    let cover = Rect::new(content.x, cover_y, NP_COVER, NP_COVER);
    let texture = state.request_cover(&track.cover);
    cards::cover_tile(ui, cover, texture, theme.radius_sm);

    let meta_x = cover.right() + NP_GAP;
    let meta_y = cover_y + (NP_COVER - 33.0) * 0.5;
    ui.text(
        Rect::new(meta_x, meta_y, meta_w, 17.0),
        &track.title,
        &title_style,
    );
    // The artist name links to the artist profile, exactly like the old player
    // bar's `artist_links` (the first linkable credit when a track has several
    // credits; the text itself is one hit target here).
    let artist_id = track.artists.iter().find_map(|artist| artist.id.as_deref());
    let artist_rect = Rect::new(meta_x, meta_y + 17.0, artist_w.max(1.0), 16.0);
    let artist_resp = if artist_id.is_some() {
        let resp = ui.interact(artist_rect, Id::from_str("player/artist"), Sense::CLICK);
        if resp.hovered {
            ui.set_cursor(CursorShape::Pointer);
        }
        Some(resp)
    } else {
        None
    };
    let artist_color = artist_resp.map_or(theme.text_dim, |resp| {
        cards::mix(theme.text_dim, theme.text, resp.hover_t)
    });
    ui.text(
        artist_rect,
        &track.artist,
        &artist_style.with_color(artist_color),
    );
    if let Some(resp) = artist_resp {
        if resp.hover_t > 0.5 {
            ui.hline(
                artist_rect.x,
                artist_rect.y + 15.0,
                artist_w,
                1.0,
                theme.text,
            );
        }
        if resp.focus_visible {
            ui.focus_ring(artist_rect, 4.0);
        }
        if resp.clicked {
            if let Some(artist_id) = artist_id {
                actions.push(Action::OpenArtist(artist_id.to_string()));
            }
        }
    }

    // Like button sits right after the text block (`text block` shrinks to its
    // content; it is not pushed toward the controls).
    let like = Rect::new(meta_x + meta_w + NP_GAP, row_center_y - 14.0, 28.0, 28.0);
    // Same identity rule as `State::liked_index` (id fast path, then `same_song`)
    // without building the whole index for one row.
    let liked = state
        .library
        .liked
        .iter()
        .any(|liked| liked.id == track.id || liked.same_song(&track));
    let (icon, color) = if liked {
        (Icon::HeartFilled, theme.accent)
    } else {
        (Icon::Heart, theme.text_dim)
    };
    if cards::icon_button(ui, like, Id::from_str("player/like"), icon, 16.0, color).clicked {
        actions.push(Action::ToggleLike(track.clone()));
    }

    state.track = Some(track);
}

/// The five 36 px transport buttons, block centred in the window.
fn transport_controls(
    ui: &mut Ui,
    state: &mut State,
    actions: &mut Vec<Action>,
    band: Rect,
    cover_y: f32,
    theme: &Theme,
) {
    let shuffle = state.queue.shuffle;
    let repeat = state.queue.repeat;
    let playing = matches!(
        state.playback,
        PlaybackState::Playing | PlaybackState::Loading
    );
    let loading = state.playback == PlaybackState::Loading;

    let play_icon = if playing { Icon::Pause } else { Icon::Play };
    let repeat_icon = match repeat {
        RepeatMode::Off => (Icon::Repeat, theme.text_dim),
        RepeatMode::All => (Icon::Repeat, theme.accent),
        RepeatMode::One => (Icon::RepeatOne, theme.accent),
    };
    let controls: [(Id, Icon, Color, Action); 5] = [
        (
            Id::from_str("player/shuffle"),
            Icon::Shuffle,
            if shuffle {
                theme.accent
            } else {
                theme.text_dim
            },
            Action::ToggleShuffle,
        ),
        (
            Id::from_str("player/prev"),
            Icon::SkipPrev,
            theme.text_dim,
            Action::PrevTrack,
        ),
        (
            Id::from_str("player/play"),
            play_icon,
            theme.text,
            Action::TogglePlay,
        ),
        (
            Id::from_str("player/next"),
            Icon::SkipNext,
            theme.text_dim,
            Action::NextTrack,
        ),
        (
            Id::from_str("player/repeat"),
            repeat_icon.0,
            repeat_icon.1,
            Action::CycleRepeat,
        ),
    ];
    let ctrl_x = controls_x(band.x, band.w);
    let ctrl_y = (cover_y + (NP_COVER - CTRL_SIZE) * 0.5).round();
    for (i, (id, icon, color, action)) in controls.into_iter().enumerate() {
        let rect = Rect::new(
            ctrl_x + i as f32 * (CTRL_SIZE + CTRL_GAP),
            ctrl_y,
            CTRL_SIZE,
            CTRL_SIZE,
        );
        if i == 2 {
            // Play/pause: the white 36 px circle (`play_button`, ui-spec §6.3).
            let resp = ui.interact(rect, id, Sense::CLICK);
            let pulse = if loading {
                0.4 + 0.6 * ((ui.time() as f32 * 2.0 * std::f32::consts::PI).sin() * 0.5 + 0.5)
            } else {
                1.0
            };
            ui.circle(
                crate::ui::Vec2::new(rect.center().0, rect.center().1),
                CTRL_SIZE * 0.5,
                theme.text.fade(pulse),
            );
            ui.icon(rect, icon, Color::hex(0x111111), 20.0);
            if resp.focus_visible {
                ui.focus_ring(rect, 999.0);
            }
            if resp.clicked {
                actions.push(action);
            }
        } else if cards::icon_button(ui, rect, id, icon, 20.0, color).clicked {
            actions.push(action);
        }
    }
}

/// Volume icon + 110 px slider, right-aligned 16 px from the content edge.
fn volume(
    ui: &mut Ui,
    state: &mut State,
    actions: &mut Vec<Action>,
    content: Rect,
    row_center_y: f32,
    theme: &Theme,
) {
    let slider_rect = Rect::new(
        content.right() - VOL_SLIDER_W,
        row_center_y - 8.0,
        VOL_SLIDER_W,
        16.0,
    );
    let icon_rect = Rect::new(slider_rect.x - 8.0 - 17.0, row_center_y - 8.5, 17.0, 17.0);
    ui.icon(icon_rect, Icon::VolumeUp, theme.text_dim, 17.0);
    let volume = cards::slider(
        ui,
        slider_rect,
        Id::from_str("player/volume"),
        state.volume,
        theme,
    );
    if let Some(f) = volume.dragged_fraction {
        actions.push(Action::SetVolume(f));
    }
    if volume.committed {
        actions.push(Action::SaveVolume);
    }
}

/// The x of the centred transport block, snapped to a whole logical pixel.
///
/// The block is centred in the window; at an odd window width that puts its
/// left edge (and with it the play/pause circle's centre) on a half pixel.
/// Circles and icons are rendered from fractional rects, so a half-pixel centre
/// makes the anti-aliased edge land one pixel short on one side — at 1251 px the
/// white circle is visibly flattened/clipped on its right edge. Snapping the
/// block keeps every button pixel-aligned at any window size.
fn controls_x(band_x: f32, band_w: f32) -> f32 {
    (band_x + (band_w - CTRLS_W) * 0.5).round()
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::screens::chrome::test_support::{input_at, preview_state, VIEWPORT};
    use crate::ui::InputState;

    /// The player bar's now-playing artist name links to the profile (the old
    /// player bar used `artist_links`); clicking it must emit `OpenArtist` and
    /// nothing else.
    #[test]
    fn player_bar_artist_name_links_to_the_profile() {
        let mut ui = Ui::new();
        let mut state = preview_state();
        state.track = Some(rift_types::Track {
            id: "t1".into(),
            title: "Song Title".into(),
            artist: "Artist Name".into(),
            album: Some("Album Name".into()),
            duration: Some(187),
            cover: String::new(),
            artists: vec![rift_types::ArtistRef {
                id: Some("artist-1".into()),
                name: "Artist Name".into(),
            }],
            album_id: None,
        });
        state.playback = rift_types::PlaybackState::Paused;
        let mut actions = Vec::new();
        let player = Rect::new(0.0, 656.0, 1160.0, 84.0);

        // Geometry from `player_bar`: row starts below the 21 px seek row, the
        // 44 px cover sits 6 px in, then title (17) and artist (16) lines.
        let meta_x = player.x + PLAYER_PAD_X + NP_COVER + NP_GAP;
        let row_y = player.y + 1.0 + SEEK_ROW_H;
        let artist_y = row_y + 6.0 + (NP_COVER - 33.0) * 0.5 + 17.0;
        let pos = (meta_x + 5.0, artist_y + 8.0);

        let draw =
            |ui: &mut Ui, state: &mut State, actions: &mut Vec<Action>, input: InputState| {
                ui.begin_frame(input, VIEWPORT);
                player_bar(ui, state, actions, player);
                let _ = ui.end_frame();
            };
        draw(&mut ui, &mut state, &mut actions, input_at(pos, 0.0));
        let press = InputState {
            mouse_left_down: true,
            mouse_left_pressed: true,
            ..input_at(pos, 0.1)
        };
        draw(&mut ui, &mut state, &mut actions, press);
        let release = InputState {
            mouse_left_released: true,
            ..input_at(pos, 0.2)
        };
        draw(&mut ui, &mut state, &mut actions, release);

        assert!(
            actions
                .iter()
                .any(|a| matches!(a, Action::OpenArtist(id) if id == "artist-1")),
            "clicking the now-playing artist must navigate to the profile: {actions:?}"
        );
        assert!(
            !actions.iter().any(|a| matches!(
                a,
                Action::TogglePlay | Action::NextTrack | Action::PrevTrack
            )),
            "the artist link press must not reach the transport controls: {actions:?}"
        );
        assert!(
            state.track.is_some(),
            "the now-playing track is restored after the frame"
        );
    }

    #[test]
    fn controls_block_is_centred() {
        assert_eq!(CTRLS_W, 212.0);
        assert_eq!((1160.0 - CTRLS_W) * 0.5, 474.0);
    }

    /// The transport block must land on whole pixels at odd window widths too:
    /// a half-pixel play/pause centre clips the white circle's right edge.
    #[test]
    fn the_controls_block_is_snapped_to_whole_pixels() {
        assert_eq!(controls_x(0.0, 1160.0), 474.0, "even width stays centred");
        assert_eq!(controls_x(0.0, 1251.0), 520.0, "odd width snaps up");
        assert!(
            (controls_x(0.0, 1251.0) - (1251.0 - CTRLS_W) * 0.5).abs() <= 0.5,
            "snapping must not visibly shift the block"
        );
        assert_eq!(
            controls_x(0.0, 940.0).fract(),
            0.0,
            "minimum width is whole"
        );
    }
}
