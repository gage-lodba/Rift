//! Headless scripted-input harness shared by the screen modules' interaction
//! tests (task-12). It drives a real [`Ui`] with synthetic `InputState`
//! sequences — no window, no GPU — exactly like `main.rs` would.

use super::*;
use crate::screens::cards;
use crate::state::Action;
use crate::text::Weight;
use crate::ui::{InputState, Key, KeyInput, Modifiers, Rect, Ui};

/// Headless scripted-input harness shared by the screen modules' interaction
/// tests (task-12). It drives a real [`Ui`] with synthetic `InputState`
/// sequences — no window, no GPU — exactly like `main.rs` would.
pub(crate) const VIEWPORT: Rect = Rect {
    x: 0.0,
    y: 0.0,
    w: 1160.0,
    h: 740.0,
};

/// A scripted UI session: one `step` == one frame.
pub(crate) struct Sim {
    pub ui: Ui,
    pub time: f64,
    pub pointer: (f32, f32),
    pressed: bool,
    released: bool,
    down: bool,
    text: String,
    keys: Vec<KeyInput>,
}

impl Sim {
    pub(crate) fn new() -> Self {
        Self {
            ui: Ui::new(),
            time: 1.0,
            pointer: (0.0, 0.0),
            pressed: false,
            released: false,
            down: false,
            text: String::new(),
            keys: Vec::new(),
        }
    }

    /// Run one frame. `draw` is the screen under test; it may append actions
    /// and (for popups) layer them via `menu::layer`.
    pub(crate) fn step(&mut self, draw: &mut impl FnMut(&mut Ui, &mut Vec<Action>)) -> Vec<Action> {
        let input = InputState {
            viewport: VIEWPORT,
            mouse_pos: self.pointer,
            mouse_in_window: true,
            mouse_left_pressed: self.pressed,
            mouse_left_down: self.down,
            mouse_left_released: self.released,
            time: self.time,
            // Fast-forward hover animations: at a real 60 Hz the hover
            // reveal would need ~8 frames, which makes click scripts long.
            delta_time: 0.2,
            text: std::mem::take(&mut self.text),
            keys: std::mem::take(&mut self.keys),
            ..Default::default()
        };
        self.ui.begin_frame(input, VIEWPORT);
        let mut actions = Vec::new();
        draw(&mut self.ui, &mut actions);
        let _ = self.ui.end_frame();
        self.time += 1.0;
        self.pressed = false;
        self.released = false;
        actions
    }

    pub(crate) fn move_to(
        &mut self,
        at: (f32, f32),
        draw: &mut impl FnMut(&mut Ui, &mut Vec<Action>),
    ) -> Vec<Action> {
        self.pointer = at;
        self.step(draw)
    }

    pub(crate) fn press(
        &mut self,
        at: (f32, f32),
        draw: &mut impl FnMut(&mut Ui, &mut Vec<Action>),
    ) -> Vec<Action> {
        self.pointer = at;
        self.pressed = true;
        self.down = true;
        self.step(draw)
    }

    pub(crate) fn release(
        &mut self,
        at: (f32, f32),
        draw: &mut impl FnMut(&mut Ui, &mut Vec<Action>),
    ) -> Vec<Action> {
        self.pointer = at;
        self.released = true;
        self.down = false;
        self.step(draw)
    }

    /// Hover-once, then press + release on the same point; returns the
    /// actions emitted by all three frames.
    pub(crate) fn click(
        &mut self,
        at: (f32, f32),
        draw: &mut impl FnMut(&mut Ui, &mut Vec<Action>),
    ) -> Vec<Action> {
        let mut out = self.move_to(at, draw);
        out.extend(self.press(at, draw));
        out.extend(self.release(at, draw));
        out
    }

    /// Feed a key press while the pointer is at `at`.
    pub(crate) fn key(
        &mut self,
        key: Key,
        at: (f32, f32),
        draw: &mut impl FnMut(&mut Ui, &mut Vec<Action>),
    ) -> Vec<Action> {
        self.pointer = at;
        self.keys.push(KeyInput {
            key,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::default(),
        });
        self.step(draw)
    }

    /// Feed typed text (as `main.rs` does for IME/character input).
    pub(crate) fn type_text(
        &mut self,
        text: &str,
        draw: &mut impl FnMut(&mut Ui, &mut Vec<Action>),
    ) -> Vec<Action> {
        self.text.push_str(text);
        self.step(draw)
    }
}

/// The screen-space center of row `row` of the currently open menu, using
/// the same placement/measurement rules as [`super::layer`].
///
/// `expanded` is the index of the expanded submenu entry (matching the
/// retained menu state), and rows are counted top-to-bottom including the
/// expanded submenu's option rows.
pub(crate) fn menu_row_center(
    ui: &mut Ui,
    entries: &[Entry],
    anchor: Rect,
    expanded: Option<usize>,
    row: usize,
) -> (f32, f32) {
    let theme = ui.theme().clone();
    let label_style = cards::style(13.0, Weight::Regular, theme.text);
    let shortcut_style = cards::style(11.0, Weight::Regular, theme.text_dim);
    let mut width: f32 = MENU_MIN_CONTENT_WIDTH;
    let mut height: f32 = 0.0;
    let mut rects: Vec<Rect> = Vec::new();

    // content width/height (same arithmetic as `layer`)
    for entry in entries {
        match entry {
            Entry::Item { item, .. } => {
                let icon_w = if item.icon.is_some() { 26.0 } else { 0.0 };
                let shortcut_w = item
                    .shortcut
                    .as_ref()
                    .map(|s| ui.text_width(s, &shortcut_style))
                    .unwrap_or(0.0);
                width = width.max(
                    10.0 + icon_w + ui.text_width(&item.label, &label_style) + shortcut_w + 10.0,
                );
                height += MENU_ITEM_HEIGHT;
            }
            Entry::Separator => height += MENU_SEPARATOR_HEIGHT,
            Entry::Sub {
                icon,
                label,
                options,
                ..
            } => {
                let icon_w = if icon.is_some() { 26.0 } else { 0.0 };
                width = width.max(10.0 + icon_w + ui.text_width(label, &label_style) + 17.0 + 10.0);
                height += MENU_ITEM_HEIGHT;
                for (option_label, _) in options {
                    width = width.max(
                        10.0 + SUBMENU_INDENT + ui.text_width(option_label, &label_style) + 10.0,
                    );
                }
            }
        }
    }
    if let Some(expanded) = expanded {
        if let Some(Entry::Sub { options, .. }) = entries.get(expanded) {
            height += options.len().min(SUBMENU_MAX_VISIBLE) as f32 * MENU_ITEM_HEIGHT;
        }
    }

    // popup placement (same rules as `Ui::popup`)
    let outer_w = (width + 10.0).max(80.0);
    let outer_h = height + 10.0;
    let mut x = anchor.right() - outer_w;
    let mut y = anchor.bottom() + 4.0;
    if y + outer_h > VIEWPORT.bottom() - 8.0 {
        let above = anchor.y - outer_h - 4.0;
        y = if above >= 8.0 {
            above
        } else {
            (VIEWPORT.bottom() - 8.0 - outer_h).max(8.0)
        };
    }
    x = x.clamp(8.0, (VIEWPORT.right() - outer_w - 8.0).max(8.0));
    let content = Rect::new(x + 5.0, y + 5.0, outer_w - 10.0, outer_h - 10.0);

    // row rects
    let mut cursor = content.y;
    for (idx, entry) in entries.iter().enumerate() {
        match entry {
            Entry::Item { .. } => {
                rects.push(Rect::new(content.x, cursor, content.w, MENU_ITEM_HEIGHT));
                cursor += MENU_ITEM_HEIGHT;
            }
            Entry::Separator => {
                rects.push(Rect::new(
                    content.x,
                    cursor,
                    content.w,
                    MENU_SEPARATOR_HEIGHT,
                ));
                cursor += MENU_SEPARATOR_HEIGHT;
            }
            Entry::Sub { options, .. } => {
                rects.push(Rect::new(content.x, cursor, content.w, MENU_ITEM_HEIGHT));
                cursor += MENU_ITEM_HEIGHT;
                if expanded == Some(idx) {
                    for _ in options.iter().take(SUBMENU_MAX_VISIBLE) {
                        rects.push(Rect::new(content.x, cursor, content.w, MENU_ITEM_HEIGHT));
                        cursor += MENU_ITEM_HEIGHT;
                    }
                }
            }
        }
    }
    let rect = *rects.get(row).expect("menu row index out of range");
    let (cx, cy) = rect.center();
    (cx, cy)
}
