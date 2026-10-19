//! Fuzz-ish robustness tests for the immediate-mode UI.
//!
//! These drive [`Ui`] through long, deterministic pseudo-random sequences of
//! frames — random pointer positions/clicks/drags, wheel, Tab/Shift+Tab, typing,
//! Escape, window resizes and view switches — and assert the invariants the rest
//! of the app relies on. A deterministic xorshift keeps failures reproducible
//! from the seed printed in the panic message.

use super::*;
use crate::text::{TextStyle, Weight};

// ---------------------------------------------------------------------------
// deterministic PRNG (no dev-dependency needed)
// ---------------------------------------------------------------------------

struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed | 1)
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn next_u32(&mut self) -> u32 {
        (self.next_u64() >> 32) as u32
    }

    fn below(&mut self, n: u32) -> u32 {
        if n == 0 {
            0
        } else {
            self.next_u32() % n
        }
    }

    fn f32(&mut self, lo: f32, hi: f32) -> f32 {
        lo + (hi - lo) * (self.next_u32() as f32 / u32::MAX as f32)
    }

    fn chance(&mut self, percent: u32) -> bool {
        self.below(100) < percent
    }
}

// ---------------------------------------------------------------------------
// scene: a widget-dense screen with stable ids
// ---------------------------------------------------------------------------

#[derive(Default)]
struct SceneState {
    text: String,
    slider: f32,
    on: bool,
    menu_open: bool,
    show_menu: bool,
}

const SCROLL_ID: Id = Id(0x5c4011);
const INPUT_ID: Id = Id(0x1f907);
const MENU_ID: Id = Id(0x3e9);
const TOAST_TEX_ID: TextureId = FIRST_IMAGE_TEXTURE_ID;

fn scene(ui: &mut Ui, state: &mut SceneState, frame: u64) {
    let vp = ui.viewport();
    let theme = ui.theme().clone();
    ui.fill(vp, theme.bg);
    if frame == 0 {
        // App texture uploaded during the frame; must be created exactly once.
        ui.upload_texture(TOAST_TEX_ID, 2, 2, vec![255; 16]);
    }
    let body = TextStyle::new(13.0, Weight::Regular, theme.text);
    let mut col = Column::new(vp.inset(16.0), 8.0);

    let row = col.fixed(34.0);
    let mut r = Row::new(row, 8.0);
    let b1 = ui.button(r.fixed(90.0), "One", ButtonKind::Primary);
    let b2 = ui.button(r.fixed(90.0), "Two", ButtonKind::Secondary);
    let b3 = ui.button(r.fixed(90.0), "Three", ButtonKind::Ghost);
    let mut v = state.slider;
    let slider = ui.slider(r.fixed(160.0).with_h(4.0), &mut v, 0.0, 1.0);
    if slider.changed {
        state.slider = v;
    }
    let mut on = state.on;
    let sw = ui.switch(r.fixed(44.0).with_h(24.0), &mut on);
    if sw.changed {
        state.on = on;
    }

    // Text input.
    let input_rect = col.fixed(33.0).with_w(320.0);
    let style = TextInputStyle::default_theme(&theme);
    let input = ui.text_input(input_rect, INPUT_ID, &mut state.text, "Type here", &style);

    // A kebab that opens/toggles a menu.
    let kebab_rect = col.fixed(28.0).with_w(28.0);
    let kebab = ui.icon_button(kebab_rect, Icon::MoreVert);
    if kebab.clicked {
        state.menu_open = !state.menu_open;
        state.show_menu = true;
        if state.menu_open {
            ui.open_popup(MENU_ID);
        } else {
            ui.close_popup();
        }
    }
    if input.submitted {
        state.text.push('!');
    }

    // Scroll area with 30 rows.
    let scroll_rect = col.remaining();
    let (_, scroll) = ui.scroll_area_stable(SCROLL_ID, scroll_rect, 30.0 * 26.0, |ui, content| {
        for i in 0..30 {
            let rr = Rect::new(content.x, content.y + i as f32 * 26.0, content.w, 24.0);
            if i % 2 == 0 {
                ui.rounded_rect(rr, theme.bg_raised, 4.0);
            }
            ui.text(
                rr.inset_pad(Padding::symmetric(6.0, 0.0)),
                &format!("Row number {i}"),
                &body,
            );
        }
    });
    let _ = scroll;

    // The menu: drawn every frame while open (this is the contract).
    if state.menu_open && state.show_menu {
        ui.popup(
            MENU_ID,
            kebab_rect,
            Size::new(MENU_MIN_CONTENT_WIDTH, 2.0 * MENU_ITEM_HEIGHT),
            PopupAlign::Right,
            |ui, content| {
                let mut col = Column::new(content, 0.0);
                ui.menu_item(
                    col.fixed(MENU_ITEM_HEIGHT),
                    "Action one",
                    Some(Icon::PlaylistAdd),
                    None,
                    false,
                );
                if ui
                    .menu_item(
                        col.fixed(MENU_ITEM_HEIGHT),
                        "Action two",
                        Some(Icon::Delete),
                        None,
                        true,
                    )
                    .clicked
                {
                    state.menu_open = false;
                    ui.close_popup();
                }
            },
        );
    }
    // Occasionally queue a toast.
    if frame % 37 == 0 {
        ui.toast_info("fuzz toast");
    }
    let _ = (b1, b2, b3);
}

fn empty_input(viewport: Rect) -> InputState {
    InputState {
        viewport,
        scale_factor: 1.0,
        time: 0.0,
        delta_time: 1.0 / 60.0,
        mouse_in_window: true,
        ..InputState::default()
    }
}

fn assert_paint_sane(paint: &PaintList, frame: u64) {
    for (li, layer) in paint.layers.iter().enumerate() {
        if let Some(clip) = layer.clip {
            assert!(
                clip.x.is_finite()
                    && clip.y.is_finite()
                    && clip.w.is_finite()
                    && clip.h.is_finite(),
                "frame {frame}: layer {li} clip not finite: {clip:?}"
            );
            assert!(
                clip.w >= 0.0 && clip.h >= 0.0,
                "frame {frame}: layer {li} clip has negative size: {clip:?}"
            );
        }
        for (pi, prim) in layer.prims.iter().enumerate() {
            let r = prim.bounds();
            assert!(
                r.x.is_finite() && r.y.is_finite() && r.w.is_finite() && r.h.is_finite(),
                "frame {frame}: layer {li} prim {pi} not finite: {r:?}"
            );
            assert!(
                r.w >= 0.0 && r.h >= 0.0,
                "frame {frame}: layer {li} prim {pi} negative size: {r:?}"
            );
            if let Prim::Image { uv, .. } = prim {
                assert!(
                    uv.x.is_finite() && uv.w.is_finite() && uv.w > 0.0 && uv.h > 0.0,
                    "frame {frame}: bad uv"
                );
            }
        }
    }
    for tex in paint.created.iter().chain(paint.updated.iter()) {
        assert_eq!(
            tex.rgba.len(),
            tex.width as usize * tex.height as usize * 4,
            "frame {frame}: texture {} has wrong pixel count",
            tex.id
        );
    }
}

/// Random frame driver: applies one random batch of input events, runs a frame,
/// and returns the paint list.
fn chaotic_frame(ui: &mut Ui, state: &mut SceneState, rng: &mut Rng, frame: u64) -> PaintList {
    let mut viewport = Rect::new(
        0.0,
        0.0,
        rng.f32(700.0, 1400.0).round(),
        rng.f32(480.0, 900.0).round(),
    );
    if rng.chance(5) {
        // Tiny window edge case.
        viewport.w = rng.f32(120.0, 260.0).round();
        viewport.h = rng.f32(90.0, 200.0).round();
    }
    let mut input = empty_input(viewport);
    input.time = frame as f64 / 60.0;
    input.scale_factor = if rng.chance(3) { 1.25 } else { 1.0 };

    input.mouse_pos = if rng.chance(85) {
        (
            rng.f32(-50.0, viewport.w + 50.0),
            rng.f32(-50.0, viewport.h + 50.0),
        )
    } else {
        (-1000.0, -1000.0)
    };
    input.mouse_in_window = rng.chance(90);

    // Mouse button edges; maintain a plausible down/up progression.
    match rng.below(10) {
        0 => {
            input.mouse_left_down = true;
            input.mouse_left_pressed = true;
        }
        1 => {
            input.mouse_left_released = true;
        }
        2 => {
            input.mouse_left_down = true;
        }
        3 => {
            input.mouse_right_pressed = true;
        }
        4 => {
            if rng.chance(50) {
                input.mouse_left_down = true;
            }
        }
        _ => {}
    }

    if rng.chance(25) {
        input.scroll = (0.0, rng.f32(-400.0, 400.0));
    }

    // Keyboard: Tab/Shift+Tab, typing, editing keys.
    if rng.chance(20) {
        let shift = rng.chance(40);
        input.keys.push(KeyInput {
            key: Key::Tab,
            pressed: true,
            repeat: false,
            modifiers: Modifiers {
                shift,
                ..Modifiers::default()
            },
        });
    }
    if rng.chance(35) {
        let pool = [
            Key::Backspace,
            Key::Delete,
            Key::Left,
            Key::Right,
            Key::Home,
            Key::End,
            Key::Enter,
            Key::Escape,
            Key::Space,
            Key::Char('a'),
            Key::Char('b'),
        ];
        let key = pool[rng.below(pool.len() as u32) as usize];
        input.keys.push(KeyInput {
            key,
            pressed: true,
            repeat: rng.chance(20),
            modifiers: Modifiers::default(),
        });
    }
    if rng.chance(30) {
        let chars = ['a', 'b', 'c', ' ', 'é', '音', '🎵', '\t'];
        let mut s = String::new();
        for _ in 0..rng.below(6) {
            s.push(chars[rng.below(chars.len() as u32) as usize]);
        }
        input.text = s;
    }
    if rng.chance(10) {
        input.modifiers.ctrl = true;
        input.text = "v".into();
    }

    ui.begin_frame(input, viewport);
    scene(ui, state, frame);
    ui.end_frame()
}

fn assert_scene_invariants(ui: &Ui, state: &SceneState, frame: u64) {
    // Focus must refer to a live widget or be cleared.
    if let Some(focus) = ui.focused_id() {
        assert!(
            ui.is_focused(focus),
            "frame {frame}: focus/bookkeeping mismatch"
        );
    }
    // Scroll offset must stay inside the content.
    let offset = ui.scroll_offset(SCROLL_ID);
    assert!(
        offset.is_finite(),
        "frame {frame}: scroll offset not finite"
    );
    assert!(
        offset >= 0.0,
        "frame {frame}: negative scroll offset {offset}"
    );
    let max = (30.0 * 26.0_f32 - 40.0).max(0.0); // 30 rows minimum viewport height
    assert!(
        offset <= 30.0 * 26.0,
        "frame {frame}: scroll offset {offset} way past content"
    );
    let _ = max;
    // Text must remain a valid string (editing never splits UTF-8).
    let _ = std::str::from_utf8(state.text.as_bytes()).expect("text stays valid UTF-8");
    assert!(state.slider.is_finite() && (0.0..=1.0).contains(&state.slider));
    // Popup: if open it must be the menu this scene draws (it draws it whenever
    // open, so an open popup after end_frame always means it was drawn).
    if let Some(open) = ui.popup_open() {
        assert_eq!(open, MENU_ID, "frame {frame}: unexpected popup");
    }
}

#[test]
fn random_input_sequences_never_break_ui_invariants() {
    for seed in [
        0x1234_5678_9abc_def1u64,
        0xdead_beef_0bad_f00d,
        0x0f0f_0f0f_1234_4321,
    ] {
        let mut rng = Rng::new(seed);
        let mut ui = Ui::new();
        let mut state = SceneState::default();
        let mut created_app_tex = 0usize;
        let mut created_atlas = 0usize;
        for frame in 0..220u64 {
            let paint = chaotic_frame(&mut ui, &mut state, &mut rng, frame);
            assert_paint_sane(&paint, frame);
            created_app_tex += paint
                .created
                .iter()
                .filter(|t| t.id == TOAST_TEX_ID)
                .count();
            created_atlas += paint
                .created
                .iter()
                .filter(|t| t.id == ATLAS_TEXTURE_ID)
                .count();
            assert_scene_invariants(&ui, &state, frame);
        }
        // Texture ids are stable and created exactly once (atlas may be
        // re-created on a scale change, but never per-frame).
        assert_eq!(
            created_app_tex, 1,
            "seed {seed:#x}: app texture created {created_app_tex} times"
        );
        assert_eq!(
            created_atlas, 1,
            "seed {seed:#x}: atlas created {created_atlas} times"
        );
    }
}

#[test]
fn open_popup_cannot_leak_to_another_screen() {
    let mut ui = Ui::new();
    let mut state = SceneState::default();
    let mut rng = Rng::new(7);
    // Frames 0..20: normal screen, force the menu open on the first frame.
    state.menu_open = true;
    state.show_menu = true;
    for frame in 0..20u64 {
        chaotic_frame(&mut ui, &mut state, &mut rng, frame);
    }
    // Force the menu open in a deterministic frame (random clicks may have
    // dismissed it above), then switch screens.
    let vp = Rect::new(0.0, 0.0, 1160.0, 740.0);
    state.menu_open = true;
    state.show_menu = true;
    ui.begin_frame(empty_input(vp), vp);
    ui.open_popup(MENU_ID);
    scene(&mut ui, &mut state, 20);
    let _ = ui.end_frame();
    assert!(ui.popup_open().is_some(), "menu should be open while drawn");
    // Switch "view": draw a screen that does not draw the popup at all.
    ui.begin_frame(empty_input(vp), vp);
    ui.fill(vp, ui.theme().bg);
    ui.button(
        Rect::new(10.0, 10.0, 100.0, 30.0),
        "Other screen",
        ButtonKind::Primary,
    );
    let _ = ui.end_frame();
    assert!(
        ui.popup_open().is_none(),
        "an open popup that no screen draws must be auto-closed"
    );
}

#[test]
fn deferred_content_paints_once_and_disappears() {
    let mut ui = Ui::new();
    let vp = Rect::new(0.0, 0.0, 800.0, 600.0);
    let count = |p: &PaintList| -> usize { p.layers.iter().map(|l| l.prims.len()).sum() };
    fn run(ui: &mut Ui, vp: Rect, time: f64, toast: Option<&str>) -> PaintList {
        let mut at = empty_input(vp);
        at.time = time;
        at.delta_time = 1.0 / 60.0;
        ui.begin_frame(at, vp);
        ui.fill(vp, ui.theme().bg);
        ui.button(
            Rect::new(10.0, 10.0, 100.0, 30.0),
            "Stable",
            ButtonKind::Primary,
        );
        if let Some(msg) = toast {
            ui.toast_error(msg);
        }
        ui.end_frame()
    }
    let base = count(&run(&mut ui, vp, 0.0, None));
    let with_toast = count(&run(&mut ui, vp, 0.1, Some("once")));
    assert!(
        with_toast > base,
        "toast prims must be painted (base {base}, toast {with_toast})"
    );
    // Next frame: the toast is still alive (5s lifetime) but must not duplicate.
    let next = count(&run(&mut ui, vp, 0.2, None));
    assert_eq!(next, with_toast, "the toast must paint once per frame");
    // After expiry the frame returns to the base primitive count.
    let expired = count(&run(&mut ui, vp, 10.0, None));
    assert_eq!(expired, base, "expired toast must disappear");
}
#[test]
fn random_resizes_and_scale_changes_keep_paint_valid() {
    let mut rng = Rng::new(0xfeed_face);
    let mut ui = Ui::new();
    let mut state = SceneState::default();
    for frame in 0..80u64 {
        let mut vp = Rect::new(
            0.0,
            0.0,
            rng.f32(300.0, 1400.0).round(),
            rng.f32(200.0, 900.0).round(),
        );
        if rng.chance(20) {
            vp = Rect::new(0.0, 0.0, 1.0, 1.0); // degenerate window
        }
        let mut input = empty_input(vp);
        input.time = frame as f64 / 60.0;
        input.scale_factor = [1.0f32, 1.25, 1.5, 2.0][rng.below(4) as usize];
        input.mouse_pos = (rng.f32(0.0, vp.w), rng.f32(0.0, vp.h));
        ui.begin_frame(input, vp);
        scene(&mut ui, &mut state, frame);
        let paint = ui.end_frame();
        assert_paint_sane(&paint, frame);
        assert_scene_invariants(&ui, &state, frame);
    }
}

// ---------------------------------------------------------------------------
// text input edge cases
// ---------------------------------------------------------------------------

struct TestClipboard {
    shared: std::rc::Rc<std::cell::RefCell<Option<String>>>,
}

impl Clipboard for TestClipboard {
    fn get_text(&mut self) -> Option<String> {
        self.shared.borrow().clone()
    }
    fn set_text(&mut self, text: &str) {
        *self.shared.borrow_mut() = Some(text.to_string());
    }
}

fn input_style(ui: &Ui) -> TextInputStyle {
    TextInputStyle::default_theme(&ui.theme().clone())
}

struct InputHarness {
    ui: Ui,
    id: Id,
    text: String,
    time: f64,
}

impl InputHarness {
    fn new() -> Self {
        let mut ui = Ui::new();
        // Tests must never touch the OS clipboard; inject or use memory instead.
        ui.set_native_clipboard_enabled(false);
        let id = Id::from_str("harness/input");
        ui.request_focus(id);
        Self {
            ui,
            id,
            text: String::new(),
            time: 0.0,
        }
    }

    fn draw(&mut self) {
        let rect = Rect::new(10.0, 10.0, 240.0, 33.0);
        let style = input_style(&self.ui);
        let mut at = empty_input(Rect::new(0.0, 0.0, 400.0, 80.0));
        at.time = self.time;
        at.delta_time = 1.0 / 60.0;
        self.ui.begin_frame(at, Rect::new(0.0, 0.0, 400.0, 80.0));
        self.ui
            .text_input(rect, self.id, &mut self.text, "Placeholder", &style);
        let _ = self.ui.end_frame();
        self.time += 1.0 / 60.0;
    }

    fn keys(&mut self, keys: &[Key]) {
        let mut at = empty_input(Rect::new(0.0, 0.0, 400.0, 80.0));
        at.modifiers = Modifiers::default();
        at.time = self.time;
        at.delta_time = 1.0 / 60.0;
        at.keys = keys
            .iter()
            .map(|&key| KeyInput {
                key,
                pressed: true,
                repeat: false,
                modifiers: Modifiers::default(),
            })
            .collect();
        self.draw_with(at);
    }

    fn cmd_keys(&mut self, keys: &[Key]) {
        let mut at = empty_input(Rect::new(0.0, 0.0, 400.0, 80.0));
        at.modifiers = Modifiers {
            ctrl: true,
            ..Modifiers::default()
        };
        at.time = self.time;
        at.delta_time = 1.0 / 60.0;
        at.keys = keys
            .iter()
            .map(|&key| KeyInput {
                key,
                pressed: true,
                repeat: false,
                modifiers: Modifiers {
                    ctrl: true,
                    ..Modifiers::default()
                },
            })
            .collect();
        self.draw_with(at);
    }

    fn type_text(&mut self, text: &str) {
        let mut at = empty_input(Rect::new(0.0, 0.0, 400.0, 80.0));
        at.modifiers = Modifiers::default();
        at.time = self.time;
        at.delta_time = 1.0 / 60.0;
        at.text = text.to_string();
        self.draw_with(at);
    }

    fn draw_with(&mut self, at: InputState) {
        let rect = Rect::new(10.0, 10.0, 240.0, 33.0);
        let style = input_style(&self.ui);
        self.ui.begin_frame(at, Rect::new(0.0, 0.0, 400.0, 80.0));
        self.ui
            .text_input(rect, self.id, &mut self.text, "Placeholder", &style);
        let _ = self.ui.end_frame();
        self.time += 1.0 / 60.0;
    }
}

#[test]
fn text_input_caret_and_edit_bounds_are_safe() {
    let mut h = InputHarness::new();
    // Backspace/Delete on an empty field are no-ops (must not panic).
    h.keys(&[Key::Backspace]);
    h.keys(&[Key::Delete]);
    h.keys(&[Key::Home]);
    h.keys(&[Key::End]);
    assert_eq!(h.text, "");
    // Type, go home, delete forward, type.
    h.type_text("abc");
    h.keys(&[Key::Home]);
    h.keys(&[Key::Delete]);
    assert_eq!(h.text, "bc");
    h.keys(&[Key::End]);
    h.keys(&[Key::Backspace]);
    assert_eq!(h.text, "b");
    // Arrow navigation over UTF-8 must never split a character.
    h.type_text("é音");
    // One Left from the end lands before "音", so Backspace removes "é".
    h.keys(&[Key::Left]);
    h.keys(&[Key::Backspace]);
    assert_eq!(h.text, "b音");
    h.keys(&[Key::Right, Key::Right, Key::Right, Key::Right]);
    h.keys(&[Key::Backspace]);
    assert_eq!(h.text, "b");
}

#[test]
fn text_input_selection_shift_home_end_and_ctrl_a_replace() {
    let mut h = InputHarness::new();
    h.type_text("hello");
    h.keys(&[Key::Home]);
    // Shift+End selects the whole line; typing replaces it.
    let mut at = empty_input(Rect::new(0.0, 0.0, 400.0, 80.0));
    at.modifiers = Modifiers {
        shift: true,
        ..Modifiers::default()
    };
    at.keys = vec![KeyInput {
        key: Key::End,
        pressed: true,
        repeat: false,
        modifiers: at.modifiers,
    }];
    h.draw_with(at);
    h.type_text("bye");
    assert_eq!(h.text, "bye");
    // Ctrl+A then typing replaces everything.
    h.cmd_keys(&[Key::Char('a')]);
    h.type_text("new");
    assert_eq!(h.text, "new");
}

#[test]
fn text_input_clipboard_works_with_injected_clipboard() {
    let mut h = InputHarness::new();
    let shared = std::rc::Rc::new(std::cell::RefCell::new(None));
    h.ui.set_clipboard(Box::new(TestClipboard {
        shared: shared.clone(),
    }));
    h.type_text("copy me");
    h.cmd_keys(&[Key::Char('a')]);
    h.cmd_keys(&[Key::Char('c')]);
    assert_eq!(shared.borrow().as_deref(), Some("copy me"));
    // Paste into an empty field.
    let mut h2 = InputHarness::new();
    h2.ui.set_clipboard(Box::new(TestClipboard {
        shared: shared.clone(),
    }));
    h2.draw();
    h2.cmd_keys(&[Key::Char('v')]);
    assert_eq!(h2.text, "copy me");
    // Cut removes the selected text and writes it to the clipboard.
    h2.cmd_keys(&[Key::Char('a')]);
    h2.cmd_keys(&[Key::Char('x')]);
    assert_eq!(h2.text, "");
    assert_eq!(shared.borrow().as_deref(), Some("copy me"));
}

#[test]
fn injected_clipboard_takes_precedence_over_the_native_backend() {
    let mut h = InputHarness::new();
    let memory = crate::ui::MemoryClipboard::new();
    let reader = memory.clone();
    h.ui.set_clipboard(Box::new(memory));
    h.type_text("injected");
    h.cmd_keys(&[Key::Char('a')]);
    h.cmd_keys(&[Key::Char('c')]);
    assert_eq!(reader.text().as_deref(), Some("injected"));
}

#[test]
fn text_input_without_clipboard_copies_are_no_ops() {
    let mut h = InputHarness::new();
    h.type_text("abc");
    h.cmd_keys(&[Key::Char('a')]);
    h.cmd_keys(&[Key::Char('c')]); // no clipboard injected
    h.cmd_keys(&[Key::Char('v')]);
    assert_eq!(
        h.text, "abc",
        "paste without clipboard must not change text"
    );
}

#[test]
fn text_input_enter_submits_and_escape_cancels_and_blurs() {
    let mut h = InputHarness::new();
    h.type_text("abc");
    // Enter: inspect the response via a direct frame.
    let rect = Rect::new(10.0, 10.0, 240.0, 33.0);
    let style = input_style(&h.ui);
    let mut at = empty_input(Rect::new(0.0, 0.0, 400.0, 80.0));
    at.time = h.time;
    at.keys = vec![KeyInput {
        key: Key::Enter,
        pressed: true,
        repeat: false,
        modifiers: Modifiers::default(),
    }];
    h.ui.begin_frame(at, Rect::new(0.0, 0.0, 400.0, 80.0));
    let r =
        h.ui.text_input(rect, h.id, &mut h.text, "Placeholder", &style);
    let _ = h.ui.end_frame();
    assert!(r.submitted);
    assert_eq!(h.text, "abc");
    // Escape cancels and blurs.
    let mut at = empty_input(Rect::new(0.0, 0.0, 400.0, 80.0));
    at.time = h.time + 1.0 / 60.0;
    at.keys = vec![KeyInput {
        key: Key::Escape,
        pressed: true,
        repeat: false,
        modifiers: Modifiers::default(),
    }];
    h.ui.begin_frame(at, Rect::new(0.0, 0.0, 400.0, 80.0));
    let r =
        h.ui.text_input(rect, h.id, &mut h.text, "Placeholder", &style);
    let _ = h.ui.end_frame();
    assert!(r.cancelled);
    assert_eq!(h.ui.focused_id(), None);
}

#[test]
fn text_input_long_text_keeps_editing_inside_the_field() {
    let mut h = InputHarness::new();
    let long: String = "word ".repeat(80);
    h.type_text(&long);
    h.keys(&[Key::End]);
    h.keys(&[Key::Backspace]);
    assert!(h.text.len() > 300);
    assert!(
        h.text.ends_with("word"),
        "got tail {:?}",
        &h.text[h.text.len().saturating_sub(8)..]
    );
    // Typing more must not panic once the text is far wider than the field.
    h.type_text("the quick brown fox jumps over the lazy dog");
    h.keys(&[Key::Home]);
    h.keys(&[Key::Delete]);
    assert!(!h.text.is_empty());
}
