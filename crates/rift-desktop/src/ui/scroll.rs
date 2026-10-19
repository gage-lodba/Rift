//! Scroll containers with wheel, scrollbar drag and clipped paint.
//!
//! A scroll container is one call: it clips its content, offsets it by the
//! retained scroll position, draws the WebKit-style 10px scrollbar in a right
//! gutter, and returns the content's layout rectangle to the closure. Wheel
//! scrolling only applies when the pointer is inside; one wheel event is
//! consumed by a single container per frame.
//!
//! Two flavours:
//!
//! * [`Ui::scroll_area`] / [`Ui::scroll_area_stable`] take the total content
//!   height up front (the caller already knows `rows * row_height`).
//! * [`Ui::scroll_area_dynamic`] / [`Ui::scroll_area_stable_dynamic`] take a
//!   closure that draws the content and returns its height, using the previous
//!   frame's height for the offset math (view code that computes its height
//!   while drawing, e.g. `home::view`).

use super::paint::{SCROLLBAR_THUMB, SCROLLBAR_THUMB_HOVER, SCROLLBAR_WIDTH};
use super::{CursorShape, Id, Rect, Sense, Ui};

/// Per-container retained state.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct ScrollState {
    pub offset: f32,
    /// Content height measured last frame, for the dynamic-height variants.
    pub content_height: f32,
}

/// Result of one scroll container.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ScrollResponse {
    /// Pointer is over the container (content or scrollbar).
    pub hovered: bool,
    /// This frame's scroll position was changed (wheel or scrollbar drag).
    pub scrolled: bool,
    /// Current offset from the top of the content, in logical px.
    pub offset: f32,
    /// Largest possible offset (`content_height - viewport.h`, >= 0).
    pub max_offset: f32,
    /// The content layout rectangle, already offset by the scroll position. Its
    /// height is `content_height` (not the viewport height), so rows can be laid
    /// out from its top.
    pub content_rect: Rect,
    /// The visible viewport rectangle (same as the `rect` passed in).
    pub viewport: Rect,
}

impl Ui {
    /// A scrollable region. `content_height` is the total height of the content
    /// the closure draws; the scrollbar gutter is only reserved once the content
    /// actually overflows.
    pub fn scroll_area<R>(
        &mut self,
        id: Id,
        rect: Rect,
        content_height: f32,
        f: impl FnOnce(&mut Ui, Rect) -> R,
    ) -> (R, ScrollResponse) {
        self.scroll_area_impl(id, rect, content_height, false, |ui, content| {
            (f(ui, content), content_height)
        })
    }

    /// Like [`Ui::scroll_area`] but always reserves the 10px gutter
    /// (`scrollbar-gutter: stable`, which is what `.main` does so card grids do
    /// not reflow when the queue panel opens).
    pub fn scroll_area_stable<R>(
        &mut self,
        id: Id,
        rect: Rect,
        content_height: f32,
        f: impl FnOnce(&mut Ui, Rect) -> R,
    ) -> (R, ScrollResponse) {
        self.scroll_area_impl(id, rect, content_height, true, |ui, content| {
            (f(ui, content), content_height)
        })
    }

    /// Scroll container for view code that discovers its content height while
    /// drawing: the closure returns `(result, content_height)`. Uses the
    /// previous frame's height for the scroll offset (correct from frame 2
    /// onwards), so the first frame always starts at the top.
    pub fn scroll_area_dynamic<R>(
        &mut self,
        id: Id,
        rect: Rect,
        f: impl FnOnce(&mut Ui, Rect) -> (R, f32),
    ) -> (R, ScrollResponse) {
        let prev = self
            .scroll
            .get(&id)
            .map(|s| s.content_height)
            .unwrap_or(0.0);
        self.scroll_area_impl(id, rect, prev, false, f)
    }

    /// Like [`Ui::scroll_area_dynamic`] but always reserves the 10px gutter.
    pub fn scroll_area_stable_dynamic<R>(
        &mut self,
        id: Id,
        rect: Rect,
        f: impl FnOnce(&mut Ui, Rect) -> (R, f32),
    ) -> (R, ScrollResponse) {
        let prev = self
            .scroll
            .get(&id)
            .map(|s| s.content_height)
            .unwrap_or(0.0);
        self.scroll_area_impl(id, rect, prev, true, f)
    }

    fn scroll_area_impl<R>(
        &mut self,
        id: Id,
        rect: Rect,
        content_height_hint: f32,
        stable_gutter: bool,
        f: impl FnOnce(&mut Ui, Rect) -> (R, f32),
    ) -> (R, ScrollResponse) {
        let mut content_height = content_height_hint.max(0.0);
        let mut max_offset = (content_height - rect.h).max(0.0);
        let mut has_bar = max_offset > 0.0;
        let mut gutter = if has_bar || stable_gutter {
            SCROLLBAR_WIDTH
        } else {
            0.0
        };
        let mut content_w = (rect.w - gutter).max(0.0);
        let mut offset = self
            .scroll
            .get(&id)
            .map(|s| s.offset.clamp(0.0, max_offset))
            .unwrap_or(0.0);

        // ---- wheel (keyed off the previous/known content height) ----
        let hovered_viewport = self.hovered(rect);
        let mut scrolled = false;
        if hovered_viewport && !self.scroll_wheel_used {
            let dy = self.input().scroll.1;
            if dy != 0.0 {
                let new = (offset - dy).clamp(0.0, max_offset);
                if (new - offset).abs() > f32::EPSILON {
                    offset = new;
                    scrolled = true;
                    self.scroll_wheel_used = true;
                }
            }
        }

        // ---- content ----
        let content_rect = Rect::new(rect.x, rect.y - offset, content_w, content_height);
        let clip = Rect::new(rect.x, rect.y, content_w, rect.h);
        let (out, measured_height) = self.with_clip(clip, |ui| f(ui, content_rect));
        debug_assert!(measured_height.is_finite());

        // ---- scrollbar geometry (after measuring so a dynamic height is exact) ----
        if measured_height > 0.0 {
            content_height = measured_height;
            max_offset = (content_height - rect.h).max(0.0);
            has_bar = max_offset > 0.0;
            gutter = if has_bar || stable_gutter {
                SCROLLBAR_WIDTH
            } else {
                0.0
            };
            content_w = (rect.w - gutter).max(0.0);
            offset = offset.clamp(0.0, max_offset);
        }
        let thumb_h = if has_bar {
            (rect.h * rect.h / content_height.max(1.0))
                .max(24.0)
                .min(rect.h)
        } else {
            0.0
        };
        let travel = (rect.h - thumb_h).max(0.0);
        let bar = Rect::new(
            rect.right() - SCROLLBAR_WIDTH,
            rect.y,
            SCROLLBAR_WIDTH,
            rect.h,
        );
        if has_bar {
            // Thumb drag takes the press first; the track then handles clicks
            // outside the thumb (its interact sees `active` already set).
            let thumb = Rect::new(
                bar.x,
                rect.y
                    + if max_offset > 0.0 {
                        travel * (offset / max_offset)
                    } else {
                        0.0
                    },
                SCROLLBAR_WIDTH,
                thumb_h,
            );
            let thumb_resp = self.interact(thumb.expand(2.0), id.with("thumb"), Sense::DRAG);
            let track_resp = self.interact(bar, id.with("track"), Sense::CLICK);
            if thumb_resp.active && self.input().mouse_left_down {
                let my = self.input().mouse_pos.1;
                let t = ((my - rect.y - thumb_h * 0.5) / travel.max(1.0)).clamp(0.0, 1.0);
                let new = t * max_offset;
                if (new - offset).abs() > f32::EPSILON {
                    offset = new;
                    scrolled = true;
                }
            } else if track_resp.clicked {
                let my = self.input().mouse_pos.1;
                let t = ((my - rect.y - thumb_h * 0.5) / travel.max(1.0)).clamp(0.0, 1.0);
                offset = t * max_offset;
                scrolled = true;
            }
            if thumb_resp.hovered || thumb_resp.active || track_resp.hovered {
                self.set_cursor(if thumb_resp.active {
                    CursorShape::Grabbing
                } else {
                    CursorShape::Default
                });
            }
            let thumb_color = if thumb_resp.hovered || thumb_resp.active {
                SCROLLBAR_THUMB_HOVER
            } else {
                SCROLLBAR_THUMB
            };
            let y = rect.y
                + if max_offset > 0.0 {
                    travel * (offset / max_offset).clamp(0.0, 1.0)
                } else {
                    0.0
                };
            self.rounded_rect(
                Rect::new(bar.x, y, SCROLLBAR_WIDTH, thumb_h),
                thumb_color,
                5.0,
            );
        }

        // ---- commit ----
        self.scroll.insert(
            id,
            ScrollState {
                offset,
                content_height,
            },
        );
        (
            out,
            ScrollResponse {
                hovered: hovered_viewport,
                scrolled,
                offset,
                max_offset,
                content_rect: Rect::new(rect.x, rect.y - offset, content_w, content_height),
                viewport: rect,
            },
        )
    }

    /// Current scroll offset of a container (for "restore scroll" behaviour).
    pub fn scroll_offset(&self, id: Id) -> f32 {
        self.scroll.get(&id).map(|s| s.offset).unwrap_or(0.0)
    }

    /// Last measured content height of a container.
    pub fn scroll_content_height(&self, id: Id) -> f32 {
        self.scroll
            .get(&id)
            .map(|s| s.content_height)
            .unwrap_or(0.0)
    }

    /// Set a container's scroll offset; clamped on the next draw.
    pub fn set_scroll_offset(&mut self, id: Id, offset: f32) {
        let state = self.scroll.entry(id).or_default();
        state.offset = offset.max(0.0);
    }

    /// Jump a container back to the top (e.g. when switching views).
    pub fn reset_scroll(&mut self, id: Id) {
        if let Some(state) = self.scroll.get_mut(&id) {
            state.offset = 0.0;
        }
    }

    /// Whether this container has been used before (has retained state).
    pub fn has_scroll_state(&self, id: Id) -> bool {
        self.scroll.contains_key(&id)
    }
}
