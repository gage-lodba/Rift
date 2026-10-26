//! The range slider used by seek / volume / crossfade.

use std::cell::Cell;

use crate::theme::Theme;
use crate::ui::{Id, Rect, Response, Sense, Ui};

thread_local! {
    /// The slider a press started on, retained across frames so a drag that is
    /// released *outside* the widget still commits (see [`slider`]).
    ///
    /// The framework's [`Response`] only reports `clicked` when the release
    /// lands on the widget; a seek/volume drag that ends over the time label or
    /// outside the bar would otherwise be dropped. Per UI thread, like the other
    /// retained screen drags (sidebar/queue reorder).
    static DRAG_SOURCE: Cell<Option<Id>> = const { Cell::new(None) };
    /// Frame timestamp of the press that set [`DRAG_SOURCE`], so a slider drawn
    /// later in the same frame can distinguish "another slider claimed this
    /// press" from "this press is unclaimed and the latch is stale".
    static DRAG_PRESS_FRAME: Cell<Option<f64>> = const { Cell::new(None) };
}

/// Drop the retained drag latch (source + claiming press frame).
fn clear_drag_source() {
    DRAG_SOURCE.with(|source| source.set(None));
    DRAG_PRESS_FRAME.with(|frame| frame.set(None));
}

// ---------------------------------------------------------------------------
// sliders
// ---------------------------------------------------------------------------

/// What a [`slider`] interaction produced this frame.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SliderOutcome {
    pub response: Response,
    /// The fraction (0..=1) under the pointer while the slider is being dragged.
    pub dragged_fraction: Option<f32>,
    /// The thumb was released after a drag within the slider (commit).
    pub committed: bool,
    /// On the commit frame: the fraction under the pointer (0..=1), so callers
    /// can fire exactly one seek/save for the release position.
    pub committed_fraction: Option<f32>,
}

/// A range slider (seek / volume / crossfade): 4 px track with a 16 px round
/// thumb, played portion in the accent color. This is a local widget (the
/// framework's slider helper is not part of the frozen base API yet); it follows
/// the reference pixels from `ui-spec.md` §6.3.
///
/// The press is retained (`DRAG_SOURCE`) so the commit fires even when the
/// pointer has left the 4 px track by the release frame — dragging the seek bar
/// to either end, or releasing over the time label, is a normal gesture.
pub fn slider(ui: &mut Ui, rect: Rect, id: Id, fraction: f32, theme: &Theme) -> SliderOutcome {
    const THUMB_R: f32 = 8.0;
    let resp = ui.interact(rect, id, Sense::DRAG);
    if resp.hovered {
        ui.set_cursor(crate::ui::CursorShape::Pointer);
    }
    let mut fraction = fraction.clamp(0.0, 1.0);
    let mut dragged_fraction = None;
    if resp.active {
        let pointer_x = ui.input().mouse_pos.0;
        let travel = (rect.w - THUMB_R * 2.0).max(1.0);
        let f = ((pointer_x - (rect.x + THUMB_R)) / travel).clamp(0.0, 1.0);
        fraction = f;
        dragged_fraction = Some(f);
    }

    // Track: Chromium's range track is ~3.5 px tall and inset 2 px from the
    // input's box (`margin: 2px` in the UA stylesheet) — the reference's
    // blended edge rows (669/672 on the seek bar) fall out of exactly this.
    const TRACK_INSET: f32 = 2.0;
    let track_rect = Rect::new(
        rect.x + TRACK_INSET,
        rect.y,
        (rect.w - TRACK_INSET * 2.0).max(2.0),
        rect.h,
    );
    let track_y = track_rect.y + (track_rect.h - 3.5) * 0.5;
    let track = Rect::new(track_rect.x, track_y, track_rect.w, 3.5);
    ui.rounded_rect(track, theme.seek_track, 1.75);
    let filled_w = THUMB_R + (track_rect.w - THUMB_R * 2.0) * fraction;
    if filled_w > 0.5 {
        ui.rounded_rect(
            Rect::new(track_rect.x, track_y, filled_w, 3.5),
            theme.seek_active,
            1.75,
        );
    }
    // Thumb: 16 px circle travelling inside the inset track.
    let cx = track_rect.x + THUMB_R + (track_rect.w - THUMB_R * 2.0) * fraction;
    let cy = rect.y + rect.h * 0.5;
    ui.circle(crate::ui::Vec2::new(cx, cy), THUMB_R, theme.seek_active);

    if resp.focus_visible {
        ui.focus_ring(rect, 2.0);
    }

    // Release tracking. The latch is owned by the slider the press started on:
    // only that slider clears it on the release frame, and any other slider
    // merely being drawn later in the same frame must not clear it. That
    // ordering is the bug this prevents: the seek bar is drawn before the
    // volume slider (and crossfade before the player bar), so "a press I did
    // not take" used to drop the seek drag on its first frame and only the last
    // drawn slider retained a drag.
    //
    // Clearing happens for exactly three reasons:
    //   * the window lost focus (the OS drops the release edge),
    //   * a new press no slider claimed this frame (stale latch from an older
    //     gesture),
    //   * the button is up on a non-release frame (the owning widget was not
    //     drawn on the release frame, so the latch is stale).
    // A press claimed by another slider this frame is identified by the frame
    // timestamp recorded in [`DRAG_PRESS_FRAME`].
    let released = ui.input().mouse_left_released;
    // A press this slider did not take is only allowed to clear the latch when
    // no slider drew earlier in this frame claimed it; otherwise it is the
    // seek-vs-volume ordering bug. The button being up on a non-release frame
    // also means the latch is stale (its owner was not drawn on the release
    // frame).
    let claimed_this_press = ui.input().mouse_left_pressed
        && DRAG_PRESS_FRAME.with(|frame| frame.get() == Some(ui.time()));
    let stale = (ui.input().mouse_left_pressed && !claimed_this_press)
        || (!ui.input().mouse_left_down && !released);
    if ui.input().focus_lost || stale {
        clear_drag_source();
    }
    if resp.pressed {
        DRAG_SOURCE.with(|source| source.set(Some(id)));
        DRAG_PRESS_FRAME.with(|frame| frame.set(Some(ui.time())));
    }
    let released_source = released && DRAG_SOURCE.with(|source| source.get() == Some(id));
    if released_source {
        clear_drag_source();
    }
    let committed = resp.clicked || released_source || (resp.hovered && released);
    let committed_fraction = if committed {
        let pointer_x = ui.input().mouse_pos.0;
        let travel = (track_rect.w - THUMB_R * 2.0).max(1.0);
        Some(((pointer_x - (track_rect.x + THUMB_R)) / travel).clamp(0.0, 1.0))
    } else {
        None
    };
    SliderOutcome {
        response: resp,
        dragged_fraction,
        committed,
        committed_fraction,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::InputState;

    const VIEWPORT: Rect = Rect::new(0.0, 0.0, 800.0, 200.0);
    const SLIDER: Rect = Rect::new(100.0, 50.0, 200.0, 16.0);

    fn frame(ui: &mut Ui, input: InputState) -> SliderOutcome {
        let theme = ui.theme().clone();
        ui.begin_frame(input, VIEWPORT);
        let out = slider(ui, SLIDER, Id::from_str("test/slider"), 0.25, &theme);
        let _ = ui.end_frame();
        out
    }

    fn input(pos: (f32, f32), time: f64) -> InputState {
        InputState {
            viewport: VIEWPORT,
            mouse_pos: pos,
            mouse_in_window: true,
            time,
            ..InputState::default()
        }
    }

    /// Press in the middle of the track, drag past the right edge and release
    /// there: the release is outside the widget rectangle, but the drag must
    /// still commit the final fraction (the time label covers that area).
    #[test]
    fn release_outside_the_track_still_commits() {
        DRAG_SOURCE.with(|source| source.set(None));
        let mut ui = Ui::new();
        let center = (SLIDER.center().0, SLIDER.center().1);
        let press = InputState {
            mouse_left_down: true,
            mouse_left_pressed: true,
            ..input(center, 0.0)
        };
        let press_out = frame(&mut ui, press);
        assert!(
            press_out.dragged_fraction.is_some(),
            "press starts the drag"
        );

        let outside = (SLIDER.right() + 40.0, SLIDER.center().1);
        let held = InputState {
            mouse_left_down: true,
            ..input(outside, 0.1)
        };
        let held_out = frame(&mut ui, held);
        assert_eq!(
            held_out.dragged_fraction,
            Some(1.0),
            "dragging past the end clamps to 100 %"
        );

        let release = InputState {
            mouse_left_released: true,
            ..input(outside, 0.2)
        };
        let release_out = frame(&mut ui, release);
        assert!(
            release_out.committed,
            "releasing outside the track still commits the drag"
        );
        assert_eq!(release_out.committed_fraction, Some(1.0));
        assert!(
            DRAG_SOURCE.with(|source| source.get().is_none()),
            "the retained drag is cleared on release"
        );
    }

    /// A plain click (no drag) commits the fraction under the pointer too.
    #[test]
    fn click_commits_the_pointer_fraction() {
        DRAG_SOURCE.with(|source| source.set(None));
        let mut ui = Ui::new();
        let at = (SLIDER.x + 8.0 + (SLIDER.w - 16.0) * 0.5, SLIDER.center().1);
        let press = InputState {
            mouse_left_down: true,
            mouse_left_pressed: true,
            ..input(at, 0.0)
        };
        let _ = frame(&mut ui, press);
        let release = InputState {
            mouse_left_released: true,
            ..input(at, 0.1)
        };
        let out = frame(&mut ui, release);
        assert!(out.committed);
        let fraction = out.committed_fraction.expect("commit fraction");
        assert!((fraction - 0.5).abs() < 0.02, "fraction {fraction}");
    }

    /// An unrelated press clears the retained source: a stale drag from a
    /// previous gesture must not commit.
    #[test]
    fn press_elsewhere_clears_a_stale_drag() {
        DRAG_SOURCE.with(|source| source.set(Some(Id::from_str("test/slider"))));
        let mut ui = Ui::new();
        let elsewhere = (10.0, 10.0);
        let press = InputState {
            mouse_left_down: true,
            mouse_left_pressed: true,
            ..input(elsewhere, 0.0)
        };
        let out = frame(&mut ui, press);
        assert!(!out.committed);
        let release = InputState {
            mouse_left_released: true,
            ..input(elsewhere, 0.1)
        };
        let out = frame(&mut ui, release);
        assert!(
            !out.committed,
            "a press elsewhere must not commit the slider"
        );
    }

    /// A latch whose release frame was never seen (the owning widget stopped
    /// being drawn, so no slider matched it) is discarded once the button is up,
    /// instead of lingering until some later click commits it.
    #[test]
    fn a_latch_without_a_release_frame_is_discarded() {
        DRAG_SOURCE.with(|source| source.set(Some(Id::from_str("test/slider"))));
        DRAG_PRESS_FRAME.with(|frame| frame.set(Some(0.0)));
        let mut ui = Ui::new();
        let out = frame(&mut ui, input((10.0, 10.0), 1.0));
        assert!(!out.committed);
        assert!(
            DRAG_SOURCE.with(|source| source.get().is_none()),
            "an abandoned latch is discarded once the button is up"
        );
    }

    const SECOND_SLIDER: Rect = Rect::new(100.0, 90.0, 200.0, 16.0);

    /// Draw the given sliders in order for one frame and return their outcomes.
    fn frame_sliders(
        ui: &mut Ui,
        input: InputState,
        sliders: &[(Rect, &'static str)],
    ) -> Vec<SliderOutcome> {
        let theme = ui.theme().clone();
        ui.begin_frame(input, VIEWPORT);
        let outcomes = sliders
            .iter()
            .map(|(rect, id)| slider(ui, *rect, Id::from_str(id), 0.25, &theme))
            .collect();
        let _ = ui.end_frame();
        outcomes
    }

    /// Regression: the seek bar is drawn before the volume slider. The volume
    /// slider used to clear the retained latch just because it saw a press it
    /// did not own, so a seek drag released over the time label never committed.
    /// Only the last-drawn slider retained its drag.
    #[test]
    fn a_later_slider_does_not_clear_the_retained_drag() {
        clear_drag_source();
        let mut ui = Ui::new();
        let seek = SLIDER;
        let volume = SECOND_SLIDER;
        let sliders = [(seek, "test/seek"), (volume, "test/volume")];
        let center = (seek.center().0, seek.center().1);

        let out = frame_sliders(
            &mut ui,
            InputState {
                mouse_left_down: true,
                mouse_left_pressed: true,
                ..input(center, 0.0)
            },
            &sliders,
        );
        assert!(out[0].dragged_fraction.is_some());
        assert_eq!(
            DRAG_SOURCE.with(|source| source.get()),
            Some(Id::from_str("test/seek")),
            "the volume slider drawn after the seek bar must not drop its drag"
        );

        let outside = (seek.right() + 40.0, seek.center().1);
        let _ = frame_sliders(
            &mut ui,
            InputState {
                mouse_left_down: true,
                ..input(outside, 0.1)
            },
            &sliders,
        );
        let out = frame_sliders(
            &mut ui,
            InputState {
                mouse_left_released: true,
                ..input(outside, 0.2)
            },
            &sliders,
        );
        assert!(
            out[0].committed,
            "releasing outside the seek bar still commits its drag"
        );
        assert_eq!(out[0].committed_fraction, Some(1.0));
        assert!(!out[1].committed, "the volume slider must not commit");
        assert!(DRAG_SOURCE.with(|source| source.get().is_none()));
    }

    /// The other draw order must keep working: a pressed slider drawn last still
    /// owns the latch (the earlier ambient slider may clear a stale latch but
    /// must not interfere with this press).
    #[test]
    fn a_pressed_slider_drawn_last_still_retains_its_drag() {
        clear_drag_source();
        let mut ui = Ui::new();
        let seek = SLIDER;
        let volume = SECOND_SLIDER;
        let sliders = [(seek, "test/seek"), (volume, "test/volume")];
        let center = (volume.center().0, volume.center().1);

        let out = frame_sliders(
            &mut ui,
            InputState {
                mouse_left_down: true,
                mouse_left_pressed: true,
                ..input(center, 0.0)
            },
            &sliders,
        );
        assert!(out[1].dragged_fraction.is_some());
        assert_eq!(
            DRAG_SOURCE.with(|source| source.get()),
            Some(Id::from_str("test/volume"))
        );

        let outside = (volume.right() + 40.0, volume.center().1);
        let _ = frame_sliders(
            &mut ui,
            InputState {
                mouse_left_down: true,
                ..input(outside, 0.1)
            },
            &sliders,
        );
        let out = frame_sliders(
            &mut ui,
            InputState {
                mouse_left_released: true,
                ..input(outside, 0.2)
            },
            &sliders,
        );
        assert!(out[1].committed);
        assert_eq!(out[1].committed_fraction, Some(1.0));
        assert!(!out[0].committed, "the seek slider must not commit");
    }
}
