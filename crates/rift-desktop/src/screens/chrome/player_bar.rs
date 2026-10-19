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
    let time_y = seek_row.y + 6.0;
    let duration = state.progress.duration.max(1.0);
    let fraction = (state.progress.position / duration).clamp(0.0, 1.0) as f32;
    let time_style = cards::style_lh(11.0, Weight::Regular, theme.text_dim, 15.0);

    let left_time = Rect::new(rect.x + 14.0, time_y, TIME_W, 15.0);
    let right_time = Rect::new(rect.right() - 14.0 - TIME_W, time_y, TIME_W, 15.0);
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
        &theme,
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

    // Now playing: cover, title/artist, like. When stopped this region is empty.
    let np_width = if let Some(track) = state.track.clone() {
        let title_style = cards::style_lh(13.0, Weight::Medium, theme.text, 17.0);
        let artist_style = cards::style_lh(12.0, Weight::Regular, theme.text_dim, 16.0);
        let title_w = ui.text_width(&track.title, &title_style);
        let artist_w = ui.text_width(&track.artist, &artist_style);
        let meta_w = title_w.max(artist_w);

        let cover = Rect::new(content.x, cover_y, NP_COVER, NP_COVER);
        let texture = state.request_cover(&track.cover);
        cards::cover_tile(ui, state, cover, texture, theme.radius_sm);

        let meta_x = cover.right() + NP_GAP;
        let meta_y = cover_y + (NP_COVER - 33.0) * 0.5;
        ui.text(
            Rect::new(meta_x, meta_y, meta_w, 17.0),
            &track.title,
            &title_style,
        );
        // The artist name links to the artist profile, exactly like the old
        // player bar's `artist_links` (the first linkable credit when a track
        // has several credits; the text itself is one hit target here).
        let artist_id = track.artists.iter().find_map(|artist| artist.id.clone());
        let artist_rect = Rect::new(meta_x, meta_y + 17.0, artist_w.max(1.0), 16.0);
        let artist_hit = if artist_id.is_some() {
            let mut resp = ui.interact(artist_rect, Id::from_str("player/artist"), Sense::CLICK);
            resp.clicked |= ui.keyboard_activated() && resp.focused;
            if resp.hovered {
                ui.set_cursor(CursorShape::Pointer);
            }
            Some((artist_rect, resp))
        } else {
            None
        };
        let artist_color = match &artist_hit {
            Some((_, resp)) => cards::mix(theme.text_dim, theme.text, resp.hover_t),
            None => theme.text_dim,
        };
        ui.text(
            artist_rect,
            &track.artist,
            &artist_style.with_color(artist_color),
        );
        if let Some((rect, resp)) = &artist_hit {
            if resp.hover_t > 0.5 {
                ui.hline(rect.x, rect.y + 15.0, artist_w, 1.0, theme.text);
            }
            if resp.focus_visible {
                ui.focus_ring(*rect, 4.0);
            }
            if resp.clicked {
                if let Some(artist_id) = artist_id {
                    actions.push(Action::OpenArtist(artist_id));
                }
            }
        }
        // Like button sits right after the text block (`text block` shrinks to
        // its content; it is not pushed toward the controls).
        let like = Rect::new(meta_x + meta_w + NP_GAP, row_center_y - 14.0, 28.0, 28.0);
        let liked = state
            .track
            .as_ref()
            .map(|t| state.liked_index().contains(t))
            .unwrap_or(false);
        let (icon, color) = if liked {
            (Icon::HeartFilled, theme.accent)
        } else {
            (Icon::Heart, theme.text_dim)
        };
        if cards::icon_button(ui, like, Id::from_str("player/like"), icon, 16.0, color).clicked {
            if let Some(t) = &state.track {
                actions.push(Action::ToggleLike(t.clone()));
            }
        }
        NP_COVER + NP_GAP + meta_w + NP_GAP + 28.0
    } else {
        0.0
    };
    let _ = np_width;

    // Controls: five 36×36 buttons, block centred in the window.
    let ctrl_x = rect.x + (rect.w - CTRLS_W) * 0.5;
    let ctrl_y = cover_y + (NP_COVER - CTRL_SIZE) * 0.5;
    let shuffle = state.queue.shuffle;
    let repeat = state.queue.repeat;
    let playing = matches!(
        state.playback,
        PlaybackState::Playing | PlaybackState::Loading
    );
    let mut bx = ctrl_x;
    // shuffle
    let color = if shuffle {
        theme.accent
    } else {
        theme.text_dim
    };
    if cards::icon_button(
        ui,
        Rect::new(bx, ctrl_y, CTRL_SIZE, CTRL_SIZE),
        Id::from_str("player/shuffle"),
        Icon::Shuffle,
        20.0,
        color,
    )
    .clicked
    {
        actions.push(Action::ToggleShuffle);
    }
    bx += CTRL_SIZE + CTRL_GAP;
    // previous
    if cards::icon_button(
        ui,
        Rect::new(bx, ctrl_y, CTRL_SIZE, CTRL_SIZE),
        Id::from_str("player/prev"),
        Icon::SkipPrev,
        20.0,
        theme.text_dim,
    )
    .clicked
    {
        actions.push(Action::PrevTrack);
    }
    bx += CTRL_SIZE + CTRL_GAP;
    // play/pause (36 px white circle)
    let play_rect = Rect::new(bx, ctrl_y, CTRL_SIZE, CTRL_SIZE);
    let play = ui.interact(play_rect, Id::from_str("player/play"), Sense::CLICK);
    let pulse = if state.playback == PlaybackState::Loading {
        0.6 + 0.4 * ((ui.time() as f32 * 2.0 * std::f32::consts::PI).sin() * 0.5 + 0.5)
    } else {
        1.0
    };
    ui.circle(
        crate::ui::Vec2::new(play_rect.center().0, play_rect.center().1),
        CTRL_SIZE * 0.5,
        theme.text.fade(pulse),
    );
    ui.icon(
        play_rect,
        if playing { Icon::Pause } else { Icon::Play },
        Color::hex(0x111111),
        20.0,
    );
    if play.clicked {
        actions.push(Action::TogglePlay);
    }
    if play.focus_visible {
        ui.focus_ring(play_rect, 999.0);
    }
    bx += CTRL_SIZE + CTRL_GAP;
    // next
    if cards::icon_button(
        ui,
        Rect::new(bx, ctrl_y, CTRL_SIZE, CTRL_SIZE),
        Id::from_str("player/next"),
        Icon::SkipNext,
        20.0,
        theme.text_dim,
    )
    .clicked
    {
        actions.push(Action::NextTrack);
    }
    bx += CTRL_SIZE + CTRL_GAP;
    // repeat
    let (repeat_icon, repeat_color) = match repeat {
        RepeatMode::Off => (Icon::Repeat, theme.text_dim),
        RepeatMode::All => (Icon::Repeat, theme.accent),
        RepeatMode::One => (Icon::RepeatOne, theme.accent),
    };
    if cards::icon_button(
        ui,
        Rect::new(bx, ctrl_y, CTRL_SIZE, CTRL_SIZE),
        Id::from_str("player/repeat"),
        repeat_icon,
        20.0,
        repeat_color,
    )
    .clicked
    {
        actions.push(Action::CycleRepeat);
    }

    // Volume: icon 17 + 8 gap + 110 slider, right-aligned 16 px from the edge.
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
        &theme,
    );
    if let Some(f) = volume.dragged_fraction {
        actions.push(Action::SetVolume(f));
    }
    if volume.committed {
        actions.push(Action::SaveVolume);
    }
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
    }

    #[test]
    fn controls_block_is_centred() {
        assert_eq!(CTRLS_W, 212.0);
        assert_eq!((1160.0 - CTRLS_W) * 0.5, 474.0);
    }
}
