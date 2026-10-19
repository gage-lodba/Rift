//! Sidebar flow interactions (task-15): import URL box, import from file,
//! new-playlist inline input, playlist kebab menu (Rename/Export/Delete),
//! plus the now-playing equalizer replacing the row glyph.

use super::editors::inline_editor_buttons;
use super::*;
// ----------------------------------------------------------------
// sidebar flow interactions (task-15): import URL box, import from file,
// new-playlist inline input, playlist kebab menu (Rename/Export/Delete).
// Scripted `InputState` frames against the real `sidebar()` renderer.
// ----------------------------------------------------------------

use crate::screens::chrome::test_support::{input_at, preview_state, VIEWPORT};
use crate::screens::menu::harness::Sim;
use crate::ui::{InputState, Key, KeyInput, Modifiers};
use std::sync::Mutex;

/// The sidebar/menu statics are process-wide, so these tests (and the
/// screens-pages menu tests) must not run concurrently.
static FLOW_LOCK: Mutex<()> = Mutex::new(());

const SIDEBAR_RECT: Rect = Rect::new(0.0, 46.0, 335.0, 610.0);

/// Serialize the sidebar/menu statics and start every test from a clean drag
/// state (the drag index lives in a thread-local, like the queue panel's).
fn lock_for_flows() -> std::sync::MutexGuard<'static, ()> {
    let guard = FLOW_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    super::reset_reorder_animation_for_tests();
    guard
}

fn base_state() -> State {
    let mut state = preview_state();
    state.new_playlist = None;
    state.import_playlist = None;
    state
}

/// Run one sidebar frame the way `screens::root` does (menu layer first for
/// outside-click consumption, again after the content when a menu opened).
fn frame(ui: &mut Ui, state: &mut State, actions: &mut Vec<Action>, input: InputState) {
    let menu_was_open = menu::is_open();
    ui.begin_frame(input, VIEWPORT);
    let _ = menu::layer(ui, actions);
    sidebar(ui, state, actions, SIDEBAR_RECT);
    if !menu_was_open && menu::is_open() {
        let _ = menu::layer(ui, actions);
    }
    let _ = ui.end_frame();
}

fn press_release(
    ui: &mut Ui,
    state: &mut State,
    actions: &mut Vec<Action>,
    pos: (f32, f32),
    mut time: f64,
) {
    let mut press = input_at(pos, time);
    press.mouse_left_down = true;
    press.mouse_left_pressed = true;
    frame(ui, state, actions, press);
    time += 0.1;
    let mut release = input_at(pos, time);
    release.mouse_left_released = true;
    frame(ui, state, actions, release);
}

fn key_input(key: Key, time: f64) -> InputState {
    InputState {
        viewport: VIEWPORT,
        mouse_pos: (0.0, 0.0),
        time,
        keys: vec![KeyInput {
            key,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::default(),
        }],
        ..Default::default()
    }
}

/// Open the sidebar's import-playlist paste box and return the y of its row.
fn open_import_box(ui: &mut Ui, state: &mut State, actions: &mut Vec<Action>, time: f64) -> f32 {
    let (import_y, _, _, _) = sidebar_pinned_rows(SIDEBAR_RECT);
    press_release(ui, state, actions, (100.0, import_y + 19.0), time);
    assert!(
        state.import_playlist.is_some(),
        "clicking Import playlist opens the inline box"
    );
    // Render frame: the box appears and takes focus from the opening row.
    frame(
        ui,
        state,
        actions,
        input_at((100.0, import_y + 19.0), time + 0.3),
    );
    assert!(
        ui.is_focused(Id::from_str("sidebar/import-input")),
        "the paste box autofocuses when it appears"
    );
    import_y
}

#[test]
fn sidebar_import_playlist_url_flow() {
    let _guard = lock_for_flows();
    let mut ui = Ui::new();
    let mut state = base_state();
    let mut actions = Vec::new();
    let _ = open_import_box(&mut ui, &mut state, &mut actions, 1.0);

    let url = "https://music.youtube.com/playlist?list=PLabc123";
    let mut typed = input_at((100.0, 0.0), 1.4);
    typed.text = url.to_string();
    frame(&mut ui, &mut state, &mut actions, typed);

    let mut enter = key_input(Key::Enter, 1.5);
    enter.mouse_pos = (100.0, 0.0);
    frame(&mut ui, &mut state, &mut actions, enter);

    let submitted: Vec<&String> = actions
        .iter()
        .filter_map(|a| match a {
            Action::ImportYtPlaylistUrl(url) => Some(url),
            _ => None,
        })
        .collect();
    assert_eq!(
        submitted,
        vec![&url.to_string()],
        "Enter submits exactly one ImportYtPlaylistUrl with the trimmed URL"
    );
    assert!(
        state.import_playlist.is_none(),
        "the box closes after submit"
    );
}

#[test]
fn sidebar_import_playlist_escape_cancels() {
    let _guard = lock_for_flows();
    let mut ui = Ui::new();
    let mut state = base_state();
    let mut actions = Vec::new();
    let _ = open_import_box(&mut ui, &mut state, &mut actions, 2.0);

    let mut typed = input_at((100.0, 0.0), 2.3);
    typed.text = "PLabc".to_string();
    frame(&mut ui, &mut state, &mut actions, typed);
    frame(
        &mut ui,
        &mut state,
        &mut actions,
        key_input(Key::Escape, 2.4),
    );

    assert!(!actions
        .iter()
        .any(|a| matches!(a, Action::ImportYtPlaylistUrl(_))));
    assert!(state.import_playlist.is_none(), "Escape closes the box");
}

#[test]
fn sidebar_import_from_file_emits_the_native_picker_action() {
    let _guard = lock_for_flows();
    let mut ui = Ui::new();
    let mut state = base_state();
    let mut actions = Vec::new();
    let (_, file_y, _, _) = sidebar_pinned_rows(SIDEBAR_RECT);
    press_release(
        &mut ui,
        &mut state,
        &mut actions,
        (100.0, file_y + 19.0),
        3.0,
    );
    assert_eq!(
        actions
            .iter()
            .filter(|a| matches!(a, Action::ImportPlaylistFile))
            .count(),
        1
    );
}

#[test]
fn sidebar_new_playlist_inline_input_creates() {
    let _guard = lock_for_flows();
    let mut ui = Ui::new();
    let mut state = base_state();
    let mut actions = Vec::new();
    // The row is the 4th playlist-region row: Home → Liked → label → 3
    // playlists → "New playlist".
    let new_row_y = SIDEBAR_RECT.y
        + SIDE_PAD_V
        + (SIDE_ITEM_H + SIDE_ITEM_GAP)
        + SIDE_ITEM_H
        + 20.0
        + 13.0
        + 6.0
        + 3.0 * (SIDE_ITEM_H + SIDE_ITEM_GAP);
    press_release(
        &mut ui,
        &mut state,
        &mut actions,
        (100.0, new_row_y + 19.0),
        4.0,
    );
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, Action::BeginNewPlaylist)),
        "clicking New playlist opens the inline input"
    );
    // app.rs applies BeginNewPlaylist after the frame:
    state.new_playlist = Some(String::new());
    let mut time = 4.3;
    // render, autofocus; then type the name.
    frame(
        &mut ui,
        &mut state,
        &mut actions,
        input_at((100.0, new_row_y + 19.0), time),
    );
    assert!(
        ui.is_focused(Id::from_str("sidebar/new-playlist-input")),
        "new-playlist input autofocuses"
    );
    time += 0.1;
    let mut typed = input_at((100.0, new_row_y + 19.0), time);
    typed.text = "My Mix".to_string();
    frame(&mut ui, &mut state, &mut actions, typed);
    // app.rs applies PlaylistDraftChanged after the frame:
    for action in actions.drain(..) {
        if let Action::PlaylistDraftChanged { id: None, name } = action {
            state.new_playlist = Some(name);
        }
    }
    time += 0.1;
    let mut enter = key_input(Key::Enter, time);
    enter.mouse_pos = (100.0, new_row_y + 19.0);
    frame(&mut ui, &mut state, &mut actions, enter);
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, Action::CreatePlaylist(name) if name == "My Mix")),
        "Enter creates the playlist with the typed name"
    );
    assert!(actions
        .iter()
        .any(|a| matches!(a, Action::ClosePlaylistEditor)));
}

/// The new-playlist editor's confirm/cancel buttons must be separate hit
/// targets (they used to overlap: confirm at right-34, cancel at right-28)
/// and clicking either must work. The editor also must stay open while a
/// button holds focus, otherwise the press frame closes it before the
/// release frame delivers the click.
#[test]
fn sidebar_new_playlist_inline_buttons_are_separate() {
    let _guard = lock_for_flows();
    let row_right = SIDEBAR_RECT.right() - SIDE_PAD;
    let center_y = playlist_row_y(3) + 17.0;

    // Cancel (X, 22 px at right:6 → centre right-17). Before the fix this
    // point was inside the confirm rect too, so it created instead.
    let mut ui = Ui::new();
    let mut state = base_state();
    state.new_playlist = Some("My Mix".into());
    let mut actions = Vec::new();
    frame(
        &mut ui,
        &mut state,
        &mut actions,
        input_at((-100.0, -100.0), 1.0),
    );
    press_release(
        &mut ui,
        &mut state,
        &mut actions,
        (row_right - 17.0, center_y),
        2.0,
    );
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, Action::ClosePlaylistEditor)),
        "clicking the X closes the editor: {actions:?}"
    );
    assert!(
        !actions
            .iter()
            .any(|a| matches!(a, Action::CreatePlaylist(_))),
        "the X must cancel, not create: {actions:?}"
    );

    // Confirm (check, 22 px at right:34 → centre right-45).
    let mut ui = Ui::new();
    let mut state = base_state();
    state.new_playlist = Some("My Mix".into());
    let mut actions = Vec::new();
    frame(
        &mut ui,
        &mut state,
        &mut actions,
        input_at((-100.0, -100.0), 1.0),
    );
    press_release(
        &mut ui,
        &mut state,
        &mut actions,
        (row_right - 45.0, center_y),
        2.0,
    );
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, Action::CreatePlaylist(name) if name == "My Mix")),
        "clicking the check creates the playlist: {actions:?}"
    );
    assert!(actions
        .iter()
        .any(|a| matches!(a, Action::ClosePlaylistEditor)));
}

/// Same two-hit-target contract for the import editor.
#[test]
fn sidebar_import_inline_buttons_are_separate() {
    let _guard = lock_for_flows();
    let (import_y, ..) = sidebar_pinned_rows(SIDEBAR_RECT);
    let row_right = SIDEBAR_RECT.right() - SIDE_PAD;
    let center_y = import_y + 17.0;
    let url = "https://music.youtube.com/playlist?list=PLabc123";

    // Cancel clears the UI-local draft.
    let mut ui = Ui::new();
    let mut state = base_state();
    state.import_playlist = Some(url.into());
    let mut actions = Vec::new();
    frame(
        &mut ui,
        &mut state,
        &mut actions,
        input_at((-100.0, -100.0), 1.0),
    );
    press_release(
        &mut ui,
        &mut state,
        &mut actions,
        (row_right - 17.0, center_y),
        2.0,
    );
    assert!(
        state.import_playlist.is_none(),
        "the X cancels the import editor"
    );
    assert!(
        !actions
            .iter()
            .any(|a| matches!(a, Action::ImportYtPlaylistUrl(_))),
        "the X must not import: {actions:?}"
    );

    // Confirm imports the draft.
    let mut ui = Ui::new();
    let mut state = base_state();
    state.import_playlist = Some(url.into());
    let mut actions = Vec::new();
    frame(
        &mut ui,
        &mut state,
        &mut actions,
        input_at((-100.0, -100.0), 1.0),
    );
    press_release(
        &mut ui,
        &mut state,
        &mut actions,
        (row_right - 45.0, center_y),
        2.0,
    );
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, Action::ImportYtPlaylistUrl(u) if u == url)),
        "clicking the check imports the pasted link: {actions:?}"
    );
    assert!(state.import_playlist.is_none());
}

#[test]
fn sidebar_playlist_kebab_menu_exports() {
    let _guard = lock_for_flows();
    let mut ui = Ui::new();
    let mut state = base_state();
    let mut actions = Vec::new();
    // First playlist row: Home, Liked, label, then the list starts at y=176
    // for the reference sidebar; its kebab is at the right end.
    let row_y = SIDEBAR_RECT.y
        + SIDE_PAD_V
        + (SIDE_ITEM_H + SIDE_ITEM_GAP)
        + SIDE_ITEM_H
        + 20.0
        + 13.0
        + 6.0;
    let kebab_pos = (SIDEBAR_RECT.right() - 10.0 - 11.0, row_y + 19.0);
    // Hover first so the row swaps its count for the kebab.
    frame(&mut ui, &mut state, &mut actions, input_at(kebab_pos, 5.0));
    press_release(&mut ui, &mut state, &mut actions, kebab_pos, 5.1);
    assert!(menu::is_open(), "the playlist kebab opens the context menu");
    assert!(
        !actions
            .iter()
            .any(|a| matches!(a, Action::Navigate(View::Playlist(_)))),
        "clicking the kebab must not navigate"
    );
    assert!(
        !actions
            .iter()
            .any(|a| matches!(a, Action::MovePlaylist { .. })),
        "clicking the kebab must not start a reorder drag: {actions:?}"
    );
    // Click "Export" (2nd of 3 items) in the open popup. Geometry mirrors
    // `Ui::popup`: content width 180 → outer 190, below the anchor +4, 5px
    // frame padding, 32px rows.
    let kebab_rect = Rect::new(
        kebab_pos.0 - 11.0,
        row_y + (SIDE_ITEM_H - 22.0) * 0.5,
        22.0,
        22.0,
    );
    let outer_w = 190.0f32;
    let content_h = 3.0 * 32.0;
    let menu_x = (kebab_rect.right() - outer_w).clamp(8.0, VIEWPORT.right() - outer_w - 8.0);
    let menu_y = kebab_rect.bottom() + 4.0;
    let _ = content_h;
    let export_pos = (menu_x + 90.0, menu_y + 5.0 + 32.0 + 16.0);
    press_release(&mut ui, &mut state, &mut actions, export_pos, 5.3);
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, Action::ExportPlaylist(id) if id == "preview-pl-1")),
        "the Export menu item emits ExportPlaylist for the row's playlist"
    );
}

/// The inline editors' check and X are separate hit targets (they used to
/// be painted on top of each other, making the X unclickable).
#[test]
fn inline_editor_buttons_do_not_overlap() {
    let row = Rect::new(SIDE_PAD, 200.0, SIDEBAR_RECT.w - SIDE_PAD * 2.0, 34.0);
    let (confirm, cancel) = inline_editor_buttons(row);
    // Old CSS: `.side-new-btn { width: 22px }`, confirm `right: 34px`,
    // cancel `right: 6px`.
    assert_eq!(confirm.right(), row.right() - 34.0);
    assert_eq!(cancel.right(), row.right() - 6.0);
    assert_eq!(confirm.w, 22.0);
    assert_eq!(cancel.w, 22.0);
    assert_eq!(
        cancel.left() - confirm.right(),
        6.0,
        "the two buttons are separated by the CSS 6 px gap"
    );
    assert!(
        confirm.right() <= cancel.left(),
        "confirm {confirm:?} and cancel {cancel:?} must not overlap"
    );
}

// ----------------------------------------------------------------
// Now-playing lead icon: the equalizer must *replace* the row glyph.
// Regression for the reported bug: playing a playlist drew the music-note
// and the equalizer on top of each other in the same 16x16 lead slot.
// ----------------------------------------------------------------

use crate::gfx::paint::{PaintList, Prim};

fn sidebar_paint(ui: &mut Ui, state: &mut State, actions: &mut Vec<Action>) -> PaintList {
    let menu_was_open = menu::is_open();
    ui.begin_frame(input_at((-100.0, -100.0), 1.0), VIEWPORT);
    let _ = menu::layer(ui, actions);
    sidebar(ui, state, actions, SIDEBAR_RECT);
    if !menu_was_open && menu::is_open() {
        let _ = menu::layer(ui, actions);
    }
    ui.end_frame()
}

/// Run one sidebar frame from an explicit input snapshot and return its paint.
fn sidebar_frame_paint(
    input: InputState,
    ui: &mut Ui,
    state: &mut State,
    actions: &mut Vec<Action>,
) -> PaintList {
    let menu_was_open = menu::is_open();
    ui.begin_frame(input, VIEWPORT);
    let _ = menu::layer(ui, actions);
    sidebar(ui, state, actions, SIDEBAR_RECT);
    if !menu_was_open && menu::is_open() {
        let _ = menu::layer(ui, actions);
    }
    ui.end_frame()
}

/// Advance the scripted sidebar with a held drag until the slot springs have
/// settled; returns the final paint so row positions can be asserted.
fn settle_held_drag(
    ui: &mut Ui,
    state: &mut State,
    actions: &mut Vec<Action>,
    from_row: usize,
    over_row: usize,
    t0: f64,
) -> PaintList {
    let mut press = input_at(playlist_center(from_row), t0);
    press.mouse_left_down = true;
    press.mouse_left_pressed = true;
    frame(ui, state, actions, press);
    let mut t = t0;
    // ~0.6 s of 60 Hz held frames: enough for the ~0.3 s settle plus margin.
    // (No interstitial idle frame here: it would drop the press before the
    // first movement frame could start the drag.)
    let mut paint = None;
    for _ in 0..40 {
        t += 1.0 / 60.0;
        let mut hold = input_at(playlist_center(over_row), t);
        hold.mouse_left_down = true;
        hold.delta_time = 1.0 / 60.0;
        paint = Some(sidebar_frame_paint(hold, ui, state, actions));
    }
    paint.expect("held frames painted")
}

/// `(image_prims, rect_prims)` intersecting `area`, across all paint layers.
fn prims_in(paint: &PaintList, area: Rect) -> (usize, usize) {
    let mut images = 0usize;
    let mut rects = 0usize;
    for layer in &paint.layers {
        for prim in &layer.prims {
            match prim {
                Prim::Rect { rect, .. } => {
                    if !rect.intersect(area).is_empty() {
                        rects += 1;
                    }
                }
                Prim::Image { rect, .. } => {
                    if !rect.intersect(area).is_empty() {
                        images += 1;
                    }
                }
            }
        }
    }
    (images, rects)
}

/// The 16x16 lead-icon slot of a sidebar row at `row_y`.
fn sidebar_icon_rect(row_y: f32) -> Rect {
    Rect::new(
        SIDEBAR_RECT.x + SIDE_PAD + 10.0,
        row_y + (SIDE_ITEM_H - 16.0) * 0.5,
        16.0,
        16.0,
    )
}

fn liked_row_y() -> f32 {
    SIDEBAR_RECT.y + SIDE_PAD_V + SIDE_ITEM_H + SIDE_ITEM_GAP
}

fn playlist_row_y(index: usize) -> f32 {
    let y = SIDEBAR_RECT.y
            + SIDE_PAD_V
            + (SIDE_ITEM_H + SIDE_ITEM_GAP) // Home
            + SIDE_ITEM_H                   // Liked Songs
            + 20.0                          // .side-divider / section spacing
            + 13.0                          // PLAYLISTS label line box
            + 6.0; // label bottom margin
    y + index as f32 * (SIDE_ITEM_H + SIDE_ITEM_GAP)
}

#[test]
fn playing_playlist_replaces_music_note_with_equalizer() {
    let mut ui = Ui::new();
    let mut state = base_state();
    let mut actions = Vec::new();
    state.queue.source = Some("playlist:preview-pl-1".into());
    state.playback = rift_types::PlaybackState::Playing;
    state.view = View::Playlist("preview-pl-1".into());

    let paint = sidebar_paint(&mut ui, &mut state, &mut actions);
    let (images, rects) = prims_in(&paint, sidebar_icon_rect(playlist_row_y(0)));
    assert_eq!(
        images, 0,
        "the playing playlist's lead slot must show only the equalizer; \
             the music-note glyph is still painted underneath it"
    );
    assert!(
        rects >= 3,
        "the equalizer draws 3 bars in the lead slot (got {rects} rect prims)"
    );

    let (images, _) = prims_in(&paint, sidebar_icon_rect(playlist_row_y(1)));
    assert!(
        images >= 1,
        "a non-playing playlist still shows its music-note glyph"
    );
}

#[test]
fn playing_liked_songs_replaces_heart_with_equalizer() {
    let mut ui = Ui::new();
    let mut state = base_state();
    let mut actions = Vec::new();
    state.queue.source = Some("liked".into());
    state.playback = rift_types::PlaybackState::Playing;
    state.view = View::Liked;

    let paint = sidebar_paint(&mut ui, &mut state, &mut actions);
    let (images, rects) = prims_in(&paint, sidebar_icon_rect(liked_row_y()));
    assert_eq!(
        images, 0,
        "the playing Liked Songs row must replace the heart with the equalizer"
    );
    assert!(rects >= 3, "equalizer bars are drawn (got {rects})");
}

/// Render the fixed state through the real GPU so the result can be
/// inspected as a PNG (manual/acceptance use; not part of the normal suite).
#[test]
#[ignore = "renders a PNG with the real GPU; run with --ignored for visual inspection"]
fn playing_playlist_visual_png() {
    use crate::app::App;
    use crate::controller::Controller;
    use crate::ui::InputState;

    let mut app = App::new(true, Controller::new_preview());
    app.set_preview_view("playlist");
    app.state.queue.source = Some("playlist:preview-pl-1".into());
    app.state.playback = rift_types::PlaybackState::Playing;
    app.tick();
    let input = InputState {
        viewport: VIEWPORT,
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
    let path = root.join("artifacts/screens/fix-playing-playlist.png");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    crate::gfx::screenshot(1160, 740, 1.0, &paint, &path).unwrap();
    assert!(path.is_file(), "wrote {}", path.display());
}

/// Render the fixed new-playlist inline editor through the real GPU so the
/// check and X can be inspected side by side.
#[test]
#[ignore = "renders a PNG with the real GPU; run with --ignored for visual inspection"]
fn inline_editor_visual_png() {
    use crate::app::App;
    use crate::controller::Controller;

    let mut app = App::new(true, Controller::new_preview());
    app.state.new_playlist = Some("My Mix".into());
    app.tick();
    let input = InputState {
        viewport: VIEWPORT,
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
    let path = root.join("artifacts/screens/inline-editor-fixed.png");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    crate::gfx::screenshot(1160, 740, 1.0, &paint, &path).unwrap();
    assert!(path.is_file(), "wrote {}", path.display());
}

// ----------------------------------------------------------------
// playlist drag-to-reorder (the old `use_reorder` sidebar hook)
// ----------------------------------------------------------------

/// Centre of playlist row `index` (inside the scroll clip).
fn playlist_center(index: usize) -> (f32, f32) {
    let y = playlist_row_y(index) + SIDE_ITEM_H * 0.5;
    (SIDEBAR_RECT.x + SIDE_PAD + 120.0, y)
}

/// Drag playlist row 0 held over row 2 and release: exactly one
/// `MovePlaylist { from: 0, to: 2 }` (backend removes then inserts, so `to`
/// is the final index) and no navigation.
#[test]
fn sidebar_playlist_drag_reorders() {
    let _guard = lock_for_flows();
    let mut state = base_state();
    let rect = SIDEBAR_RECT;
    let mut sim = Sim::new();
    let mut draw = |ui: &mut Ui, actions: &mut Vec<Action>| {
        sidebar(ui, &mut state, actions, rect);
    };
    let from = playlist_center(0);
    let over = playlist_center(2);
    sim.press(from, &mut draw);
    let held = sim.move_to(over, &mut draw); // held: starts the drag + live preview
    assert!(
        !held
            .iter()
            .any(|a| matches!(a, Action::MovePlaylist { .. } | Action::Navigate(_))),
        "holding a drag only previews the new order; the commit waits for release: {held:?}"
    );
    let actions = sim.release(over, &mut draw);
    let moves: Vec<&Action> = actions
        .iter()
        .filter(|a| matches!(a, Action::MovePlaylist { .. }))
        .collect();
    assert_eq!(moves.len(), 1, "exactly one MovePlaylist: {actions:?}");
    assert!(
        matches!(moves[0], Action::MovePlaylist { from: 0, to: 2 }),
        "drag 0 -> row 2 reorders to final index 2: {actions:?}"
    );
    assert!(
        !actions.iter().any(|a| matches!(a, Action::Navigate(_))),
        "a drag release must not navigate: {actions:?}"
    );
    assert!(!menu::is_open());
}

/// A plain click (press + release, no movement) still navigates.
#[test]
fn sidebar_playlist_plain_click_still_navigates() {
    let _guard = lock_for_flows();
    let mut state = base_state();
    let rect = SIDEBAR_RECT;
    let mut sim = Sim::new();
    let mut draw = |ui: &mut Ui, actions: &mut Vec<Action>| {
        sidebar(ui, &mut state, actions, rect);
    };
    let actions = sim.click(playlist_center(1), &mut draw);
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, Action::Navigate(View::Playlist(id)) if id == "preview-pl-2")),
        "plain click navigates to the playlist: {actions:?}"
    );
    assert!(
        !actions
            .iter()
            .any(|a| matches!(a, Action::MovePlaylist { .. })),
        "a plain click must not reorder: {actions:?}"
    );
}

/// Alt+Down on the focused playlist row moves it one slot down; Alt+Up at the
/// top of the list is a no-op (old UI keyboard reorder parity).
#[test]
fn sidebar_playlist_alt_arrows_reorder() {
    let _guard = lock_for_flows();
    let mut ui = Ui::new();
    let mut state = base_state();
    let mut actions = Vec::new();
    // Click the first row to focus it (the row's click is fine; ignore the
    // deferred navigation action).
    let center = playlist_center(0);
    press_release(&mut ui, &mut state, &mut actions, center, 1.0);
    actions.clear();

    // Alt+Down on the focused row.
    let mut input = input_at(center, 2.0);
    input.modifiers = Modifiers {
        alt: true,
        ..Modifiers::default()
    };
    input.keys = vec![KeyInput {
        key: Key::Down,
        pressed: true,
        repeat: false,
        modifiers: input.modifiers,
    }];
    frame(&mut ui, &mut state, &mut actions, input);
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, Action::MovePlaylist { from: 0, to: 1 })),
        "Alt+Down moves the focused playlist down: {actions:?}"
    );

    // Alt+Up on the first row is a no-op.
    actions.clear();
    let mut input = input_at(center, 3.0);
    input.modifiers = Modifiers {
        alt: true,
        ..Modifiers::default()
    };
    input.keys = vec![KeyInput {
        key: Key::Up,
        pressed: true,
        repeat: false,
        modifiers: input.modifiers,
    }];
    frame(&mut ui, &mut state, &mut actions, input);
    assert!(
        !actions
            .iter()
            .any(|a| matches!(a, Action::MovePlaylist { .. })),
        "Alt+Up at index 0 is a no-op: {actions:?}"
    );
}

// ----------------------------------------------------------------
// live drag-reorder preview (old `use_reorder::drag_shift_style`)
// ----------------------------------------------------------------

use super::drag_visual_index;

#[test]
fn drag_visual_index_previews_the_post_drop_order() {
    // Dragging row 0 over slot 2: it sits at 2 and the rows it passed slide up.
    assert_eq!(
        (0..3)
            .map(|i| drag_visual_index(i, 0, 2))
            .collect::<Vec<_>>(),
        vec![2, 0, 1]
    );
    // Dragging row 2 over slot 0: it sits at 0 and the rows it passed slide down.
    assert_eq!(
        (0..3)
            .map(|i| drag_visual_index(i, 2, 0))
            .collect::<Vec<_>>(),
        vec![1, 2, 0]
    );
    // Moving down across two rows keeps untouched rows where they are.
    assert_eq!(
        (0..5)
            .map(|i| drag_visual_index(i, 1, 3))
            .collect::<Vec<_>>(),
        vec![0, 3, 1, 2, 4]
    );
}

#[test]
fn drag_visual_index_identity_and_edges() {
    // Dropped in place (or not dragging): every row keeps its slot.
    for i in 0..4 {
        assert_eq!(drag_visual_index(i, 1, 1), i);
        assert_eq!(drag_visual_index(i, 0, 0), i);
        assert_eq!(drag_visual_index(i, 3, 3), i);
    }
    // Single-element list.
    assert_eq!(drag_visual_index(0, 0, 0), 0);
    // Dragging to an edge does not move rows on the far side.
    assert_eq!(drag_visual_index(3, 0, 1), 3);
    assert_eq!(drag_visual_index(0, 1, 0), 1);
}

/// Render a held drag (row 0 over slot 2) with the real GPU so the live
/// preview can be inspected:
/// `cargo test -p rift-desktop --lib sidebar_drag_preview_visual_png -- --ignored`.
#[test]
#[ignore = "renders a PNG with the real GPU; run with --ignored for visual inspection"]
fn sidebar_drag_preview_visual_png() {
    use crate::app::App;
    use crate::controller::Controller;
    use crate::ui::InputState;

    let mut app = App::new(true, Controller::new_preview());
    app.tick();
    let viewport = Rect::new(0.0, 0.0, 1160.0, 740.0);

    // Frame 1: press on playlist row 0 (sidebar starts at y=46; the list starts
    // at y=176 in the reference layout, rows are 41 px apart).
    let row0 = (130.0, 176.0 + SIDE_ITEM_H * 0.5);
    let row2 = (
        130.0,
        176.0 + 2.0 * (SIDE_ITEM_H + SIDE_ITEM_GAP) + SIDE_ITEM_H * 0.5,
    );
    let press = InputState {
        viewport,
        scale_factor: 1.0,
        time: 1.0,
        mouse_pos: row0,
        mouse_in_window: true,
        mouse_left_pressed: true,
        mouse_left_down: true,
        ..InputState::default()
    };
    let _ = app.frame(press);

    // Frame 2: still held, pointer over row 2 -> rows slide into post-drop order.
    let held = InputState {
        viewport,
        scale_factor: 1.0,
        time: 1.5,
        mouse_pos: row2,
        mouse_in_window: true,
        mouse_left_down: true,
        ..InputState::default()
    };
    let _ = app.frame(held);

    // Hold ~0.5 s of 60 Hz frames so the slot springs settle into the
    // post-drop order (row 0 glides down to slot 2, rows 1-2 slide up).
    let mut paint = None;
    for step in 0..30 {
        let held_preview = InputState {
            viewport,
            scale_factor: 1.0,
            time: 2.0 + step as f64 / 60.0,
            delta_time: 1.0 / 60.0,
            mouse_pos: row2,
            mouse_in_window: true,
            mouse_left_down: true,
            ..InputState::default()
        };
        paint = Some(app.frame(held_preview));
    }
    let paint = paint.expect("held frames painted");
    assert!(!paint.layers.is_empty());
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let path = root.join("artifacts/screens/sidebar-drag-held.png");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    crate::gfx::screenshot(1160, 740, 1.0, &paint, &path).unwrap();
    assert!(path.is_file(), "wrote {}", path.display());
}

/// Y centres of the now-playing equalizer bars, which are 3 px-wide rects.
/// Used to observe *where* a playlist row is painted without needing text.
fn eq_bar_ys(paint: &PaintList) -> Vec<f32> {
    let mut ys = Vec::new();
    for layer in &paint.layers {
        for prim in &layer.prims {
            if let Prim::Rect { rect, .. } = prim {
                if (rect.w - 3.0).abs() < 0.01 && rect.h >= 4.0 {
                    ys.push(rect.y + rect.h * 0.5);
                }
            }
        }
    }
    ys
}

/// While the drag is held the rows glide to their post-drop slots: the playing
/// playlist (row 1) animates up into slot 0, so after the springs settle its
/// equalizer bars paint there. This is the headless assertion for the live,
/// animated preview; the ignored `sidebar_drag_preview_visual_png` renders the
/// same state as an image.
#[test]
fn held_drag_paints_rows_in_the_post_drop_order() {
    let _guard = lock_for_flows();
    let mut state = base_state();
    state.queue.source = Some("playlist:preview-pl-2".into());
    state.playback = rift_types::PlaybackState::Playing;
    let mut actions = Vec::new();
    let mut ui = Ui::new();

    // Idle: Playlist 2 is row 1 (slot y=217), so its bars sit in that band.
    let idle = sidebar_paint(&mut ui, &mut state, &mut actions);
    let idle_ys = eq_bar_ys(&idle);
    assert!(!idle_ys.is_empty(), "playing playlist draws equalizer bars");
    assert!(
        idle_ys.iter().all(|y| (225.0..=255.0).contains(y)),
        "idle: the equalizer paints in slot 1: {idle_ys:?}"
    );

    let paint = settle_held_drag(&mut ui, &mut state, &mut actions, 0, 2, 2.0);
    let held_ys = eq_bar_ys(&paint);
    assert!(!held_ys.is_empty(), "held drag still paints the equalizer");
    assert!(
        held_ys.iter().all(|y| (185.0..=215.0).contains(y)),
        "after settling, row 1 has glided up into slot 0 while row 0 is \
         dragged to slot 2: {held_ys:?}"
    );
}

/// The committed-order hold keeps the released rows at the previewed order
/// until the backend event reorders the library — no snap-back in between.
#[test]
fn post_drop_committed_order_prevents_snap_back() {
    let _guard = lock_for_flows();
    let mut state = base_state();
    state.queue.source = Some("playlist:preview-pl-2".into());
    state.playback = rift_types::PlaybackState::Playing;
    let mut actions = Vec::new();
    let mut ui = Ui::new();

    // Drag row 0 over slot 2, settle, then release there.
    let paint = settle_held_drag(&mut ui, &mut state, &mut actions, 0, 2, 2.0);
    let held_ys = eq_bar_ys(&paint);
    assert!(
        held_ys.iter().all(|y| (185.0..=215.0).contains(y)),
        "preview settled before the drop: {held_ys:?}"
    );

    actions.clear();
    let mut release = input_at(playlist_center(2), 3.5);
    release.mouse_left_released = true;
    release.delta_time = 1.0 / 60.0;
    frame(&mut ui, &mut state, &mut actions, release);
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, Action::MovePlaylist { from: 0, to: 2 })),
        "release emits exactly one MovePlaylist: {actions:?}"
    );
    assert_eq!(
        actions
            .iter()
            .filter(|a| matches!(a, Action::MovePlaylist { .. }))
            .count(),
        1,
        "exactly one MovePlaylist on release: {actions:?}"
    );

    // The backend event has not landed yet: the library order is unchanged, but
    // the held order must keep the rows where they were previewed.
    for step in 0..20 {
        let idle = input_at((-100.0, -100.0), 3.6 + step as f64 / 60.0);
        let _ = sidebar_frame_paint(idle, &mut ui, &mut state, &mut actions);
    }
    let idle = input_at((-100.0, -100.0), 4.0);
    let after = sidebar_frame_paint(idle, &mut ui, &mut state, &mut actions);
    let after_ys = eq_bar_ys(&after);
    assert!(
        after_ys.iter().all(|y| (185.0..=215.0).contains(y)),
        "no snap-back before the backend order arrives: {after_ys:?}"
    );
    assert!(
        super::COMMITTED_ORDER.with(|hold| hold.borrow().is_some()),
        "the committed-order hold is active while the event is in flight"
    );

    // Apply the backend order (what the MovePlaylist event eventually does).
    let mut ids = state.library.playlists.clone();
    let moved = ids.remove(0);
    ids.insert(2, moved);
    state.library.playlists = ids;
    let idle = input_at((-100.0, -100.0), 4.2);
    let _ = sidebar_frame_paint(idle, &mut ui, &mut state, &mut actions);
    assert!(
        super::COMMITTED_ORDER.with(|hold| hold.borrow().is_none()),
        "the hold clears once the actual order matches the expected order"
    );
    let idle = input_at((-100.0, -100.0), 4.4);
    let settled = sidebar_frame_paint(idle, &mut ui, &mut state, &mut actions);
    let settled_ys = eq_bar_ys(&settled);
    assert!(
        settled_ys.iter().all(|y| (185.0..=215.0).contains(y)),
        "rows stay put when the real order matches the preview: {settled_ys:?}"
    );
}

// ----------------------------------------------------------------
// spring animation helpers
// ----------------------------------------------------------------

use super::{moved_order, spring_slot};

/// Tuning: ~8 % overshoot (ζ≈0.62) and a settle well under half a second.
#[test]
fn spring_slot_overshoots_slightly_and_settles_quickly() {
    let dt = 1.0 / 60.0;
    let mut x = 0.0f32;
    let mut v = 0.0f32;
    let mut peak = 0.0f32;
    let mut settled_at = None;
    for step in 1..=120 {
        let (nx, nv) = spring_slot(x, v, 1.0, dt);
        x = nx;
        v = nv;
        peak = peak.max(x);
        if settled_at.is_none() && (x - 1.0).abs() < 0.02 && v.abs() < 0.05 {
            settled_at = Some(step as f32 * dt);
        }
    }
    assert!(peak > 1.0, "the row overshoots the target (peak {peak})");
    assert!(peak < 1.15, "overshoot stays subtle (~8 %): peak {peak}");
    let settled_at = settled_at.expect("spring settles");
    assert!(
        settled_at < 0.5,
        "settles quickly (2 % band at {settled_at:.3} s)"
    );
    assert!(
        (x - 1.0).abs() < 0.01 && v.abs() < 0.01,
        "converges to the target (x {x}, v {v})"
    );
}

#[test]
fn spring_slot_is_stable_under_large_dt() {
    // A janky/huge frame time is clamped to 1/30 s, so the integration stays
    // bounded and converges instead of exploding.
    let (x, _) = spring_slot(0.0, 0.0, 1.0, 1.0);
    assert!(
        x.is_finite() && x > 0.0 && x < 1.0,
        "one clamped step stays bounded: {x}"
    );
    let mut x = 0.0;
    let mut v = 0.0;
    for _ in 0..240 {
        let (nx, nv) = spring_slot(x, v, 1.0, 5.0);
        x = nx;
        v = nv;
    }
    assert!((x - 1.0).abs() < 0.01, "converges under clamped dt: {x}");
}

#[test]
fn moved_order_matches_the_backend_remove_then_insert() {
    let ids: Vec<String> = ["A", "B", "C", "D"].iter().map(|s| s.to_string()).collect();
    assert_eq!(moved_order(&ids, 0, 2), ["B", "C", "A", "D"]);
    assert_eq!(moved_order(&ids, 3, 1), ["A", "D", "B", "C"]);
    assert_eq!(moved_order(&ids, 1, 1), ids);
    assert_eq!(moved_order(&ids, 0, 3), ["B", "C", "D", "A"]);
}

// ----------------------------------------------------------------
// cursor shapes (user feedback: plain pointer on hover, grab only while
// dragging a playlist; framework auto-pointer handles Sense::CLICK widgets)
// ----------------------------------------------------------------

use crate::ui::CursorShape;

/// Hovering a playlist row, its kebab, a nav row or the resizer must show the
/// normal hand (or the resize arrow) — never the grab hand. The grab hand was
/// removed from the sidebar's hover path; `Ui::interact`/`icon_button` supply
/// `CursorShape::Pointer` automatically for clickable widgets.
#[test]
fn sidebar_hover_cursors_are_pointer_and_resize() {
    let _guard = lock_for_flows();
    let mut ui = Ui::new();
    let mut state = base_state();
    let mut actions = Vec::new();

    // Playlist row body (no nested button under the pointer).
    frame(
        &mut ui,
        &mut state,
        &mut actions,
        input_at(playlist_center(0), 1.0),
    );
    assert_eq!(
        ui.cursor_shape(),
        CursorShape::Pointer,
        "playlist row hover shows the normal hand, not Grab"
    );

    // The row's kebab (nested button) — still the normal hand.
    let kebab = (
        SIDEBAR_RECT.right() - 10.0 - 11.0,
        playlist_row_y(0) + SIDE_ITEM_H * 0.5,
    );
    frame(&mut ui, &mut state, &mut actions, input_at(kebab, 1.2));
    assert_eq!(
        ui.cursor_shape(),
        CursorShape::Pointer,
        "playlist kebab hover shows the normal hand, not Grab"
    );

    // Nav row hover (Home: first row).
    let home = (
        SIDEBAR_RECT.x + SIDE_PAD + 120.0,
        SIDEBAR_RECT.y + SIDE_PAD_V + SIDE_ITEM_H * 0.5,
    );
    frame(&mut ui, &mut state, &mut actions, input_at(home, 1.4));
    assert_eq!(
        ui.cursor_shape(),
        CursorShape::Pointer,
        "nav row hover shows the normal hand"
    );

    // The drag-resize handle on the sidebar's right edge keeps its arrow.
    let resizer = (SIDEBAR_RECT.right() - 3.0, SIDEBAR_RECT.y + 200.0);
    frame(&mut ui, &mut state, &mut actions, input_at(resizer, 1.6));
    assert_eq!(
        ui.cursor_shape(),
        CursorShape::ResizeH,
        "the resize handle keeps ResizeH"
    );
}

/// Only an actual held playlist drag shows `Grabbing`; releasing it returns to
/// the normal hand (the drag cursor is set after the row loop, so it wins over
/// the per-row auto-pointer).
#[test]
fn sidebar_playlist_drag_cursor_is_grabbing_only_while_held() {
    let _guard = lock_for_flows();
    let mut ui = Ui::new();
    let mut state = base_state();
    let mut actions = Vec::new();

    // Press row 0: before the drag starts it is still the normal hand.
    let mut press = input_at(playlist_center(0), 1.0);
    press.mouse_left_down = true;
    press.mouse_left_pressed = true;
    frame(&mut ui, &mut state, &mut actions, press);
    assert_eq!(
        ui.cursor_shape(),
        CursorShape::Pointer,
        "pressing a playlist row is not a drag yet"
    );

    // Hold over slot 2: the drag is active -> Grabbing, and stays Grabbing on
    // subsequent held frames.
    let mut t = 1.0;
    for _ in 0..5 {
        t += 1.0 / 60.0;
        let mut held = input_at(playlist_center(2), t);
        held.mouse_left_down = true;
        held.delta_time = 1.0 / 60.0;
        frame(&mut ui, &mut state, &mut actions, held);
        assert_eq!(
            ui.cursor_shape(),
            CursorShape::Grabbing,
            "a held playlist drag shows the grabbing hand"
        );
    }

    // Release: the drag is over -> no longer Grabbing (the pointer is still over
    // a playlist row, so the normal hand returns).
    let mut release = input_at(playlist_center(2), t + 1.0 / 60.0);
    release.mouse_left_released = true;
    release.delta_time = 1.0 / 60.0;
    frame(&mut ui, &mut state, &mut actions, release);
    assert_ne!(
        ui.cursor_shape(),
        CursorShape::Grabbing,
        "after release the drag cursor is gone"
    );
    assert_eq!(
        ui.cursor_shape(),
        CursorShape::Pointer,
        "after release the normal hand is back"
    );
}

// ----------------------------------------------------------------
// review follow-ups (task-50): focus loss, in-place clicks, settling gating,
// spring dt edge cases, Alt during the committed hold
// ----------------------------------------------------------------

/// A window focus loss mid-drag cancels the retained drag without committing,
/// and the stale click on the refocus frame does not navigate.
#[test]
fn sidebar_focus_loss_cancels_a_held_drag() {
    let _guard = lock_for_flows();
    let mut ui = Ui::new();
    let mut state = base_state();
    let mut actions = Vec::new();

    let mut press = input_at(playlist_center(0), 1.0);
    press.mouse_left_down = true;
    press.mouse_left_pressed = true;
    frame(&mut ui, &mut state, &mut actions, press);
    let mut hold = input_at(playlist_center(2), 1.2);
    hold.mouse_left_down = true;
    hold.delta_time = 1.0 / 60.0;
    frame(&mut ui, &mut state, &mut actions, hold);
    assert!(
        super::PLAYLIST_DRAG_FROM.with(|from| from.get().is_some()),
        "the drag is retained before focus is lost"
    );

    actions.clear();
    let mut lost = input_at(playlist_center(2), 1.4);
    lost.focus_lost = true;
    frame(&mut ui, &mut state, &mut actions, lost);
    assert!(
        super::PLAYLIST_DRAG_FROM.with(|from| from.get().is_none()),
        "focus loss clears the retained drag"
    );
    assert!(
        super::COMMITTED_ORDER.with(|hold| hold.borrow().is_none()),
        "a cancelled drag does not commit an order"
    );
    assert!(
        !actions
            .iter()
            .any(|a| matches!(a, Action::MovePlaylist { .. } | Action::Navigate(_))),
        "the focus-loss frame emits neither a move nor a navigation: {actions:?}"
    );

    // The stale release edge on the refocus frame must not commit or navigate
    // (Ui::begin_frame has dropped the active latch).
    actions.clear();
    let mut release = input_at(playlist_center(2), 1.6);
    release.mouse_left_released = true;
    frame(&mut ui, &mut state, &mut actions, release);
    assert!(
        !actions
            .iter()
            .any(|a| matches!(a, Action::MovePlaylist { .. } | Action::Navigate(_))),
        "the refocus release frame does not commit or navigate: {actions:?}"
    );
}

/// Press, drift a few pixels (enough to latch the >2 px drag threshold) and
/// release inside the origin slot: this is a click, so it navigates. Only a
/// committed move (or a drag that started this frame) swallows the click.
#[test]
fn sidebar_drift_click_releases_in_place_and_navigates() {
    let _guard = lock_for_flows();
    let mut state = base_state();
    let mut sim = Sim::new();
    let mut draw = |ui: &mut Ui, actions: &mut Vec<Action>| {
        sidebar(ui, &mut state, actions, SIDEBAR_RECT);
    };
    let origin = playlist_center(0);
    let drifted = (origin.0 + 3.0, origin.1);
    let mut actions = sim.press(origin, &mut draw);
    actions.extend(sim.move_to(drifted, &mut draw)); // > 2 px: drag_started
    actions.extend(sim.release(drifted, &mut draw));
    assert!(
        !actions
            .iter()
            .any(|a| matches!(a, Action::MovePlaylist { .. })),
        "releasing in the origin slot does not move the playlist: {actions:?}"
    );
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, Action::Navigate(View::Playlist(id)) if id == "preview-pl-1")),
        "a drift-release in place still navigates: {actions:?}"
    );
}

/// While the post-drop springs are still settling the rows paint normally but
/// are not interactive: a click during the glide is swallowed, and the same
/// click navigates once the rows have arrived at their slots.
#[test]
fn sidebar_settling_rows_do_not_navigate_until_settled() {
    let _guard = lock_for_flows();
    let mut ui = Ui::new();
    let mut state = base_state();
    let mut actions = Vec::new();

    // Start a drag and drop it after only two animated frames: the rows are
    // still gliding toward their post-drop slots when the button is released.
    let mut press = input_at(playlist_center(0), 1.0);
    press.mouse_left_down = true;
    press.mouse_left_pressed = true;
    frame(&mut ui, &mut state, &mut actions, press);
    for step in 0..2 {
        let mut hold = input_at(playlist_center(2), 1.1 + step as f64 * 1.0 / 60.0);
        hold.mouse_left_down = true;
        hold.delta_time = 1.0 / 60.0;
        frame(&mut ui, &mut state, &mut actions, hold);
    }
    actions.clear();
    let mut release = input_at(playlist_center(2), 1.2);
    release.mouse_left_released = true;
    release.delta_time = 1.0 / 60.0;
    frame(&mut ui, &mut state, &mut actions, release);
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, Action::MovePlaylist { from: 0, to: 2 })),
        "the drop commits exactly once: {actions:?}"
    );
    assert!(
        super::COMMITTED_ORDER.with(|hold| hold.borrow().is_some()),
        "the committed order is held while the rows settle"
    );

    // Click the top slot immediately: rows are mid-glide -> swallowed.
    actions.clear();
    let mut press = input_at(playlist_center(0), 1.3);
    press.mouse_left_down = true;
    press.mouse_left_pressed = true;
    frame(&mut ui, &mut state, &mut actions, press);
    let mut release = input_at(playlist_center(0), 1.4);
    release.mouse_left_released = true;
    frame(&mut ui, &mut state, &mut actions, release);
    assert!(
        !actions.iter().any(|a| matches!(a, Action::Navigate(_))),
        "a click during the settle glide must not navigate: {actions:?}"
    );

    // Let the springs settle (~0.6 s at 60 Hz), then the same click navigates
    // to the row that visually sits in slot 0 after the move: Playlist 2.
    for step in 0..40 {
        let mut idle = input_at((-100.0, -100.0), 1.5 + step as f64 / 60.0);
        idle.delta_time = 1.0 / 60.0;
        frame(&mut ui, &mut state, &mut actions, idle);
    }
    actions.clear();
    let mut press = input_at(playlist_center(0), 2.3);
    press.mouse_left_down = true;
    press.mouse_left_pressed = true;
    frame(&mut ui, &mut state, &mut actions, press);
    let mut release = input_at(playlist_center(0), 2.4);
    release.mouse_left_released = true;
    frame(&mut ui, &mut state, &mut actions, release);
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, Action::Navigate(View::Playlist(id)) if id == "preview-pl-2")),
        "after settling, the click navigates to the visually-top playlist: {actions:?}"
    );
}

/// A paused/first frame (`dt == 0`) or a bad clock must not advance the spring.
#[test]
fn spring_slot_zero_or_non_finite_dt_is_a_noop() {
    for dt in [0.0f32, -0.0, -0.5, f32::NAN, f32::INFINITY] {
        assert_eq!(
            spring_slot(0.25, 1.5, 1.0, dt),
            (0.25, 1.5),
            "dt {dt} must leave the spring untouched"
        );
    }
    // A real dt still advances.
    let (x, v) = spring_slot(0.25, 0.0, 1.0, 1.0 / 60.0);
    assert!(x > 0.25 && v > 0.0);
}

/// Alt+Up/Down is ignored while a committed order is in flight (the row indices
/// would refer to the stale library order).
#[test]
fn sidebar_alt_arrows_ignored_while_committed_hold_active() {
    let _guard = lock_for_flows();
    let mut ui = Ui::new();
    let mut state = base_state();
    let mut actions = Vec::new();

    // Create a committed hold via a settled drop (0 -> slot 2).
    let _ = settle_held_drag(&mut ui, &mut state, &mut actions, 0, 2, 1.0);
    actions.clear();
    let mut release = input_at(playlist_center(2), 2.0);
    release.mouse_left_released = true;
    release.delta_time = 1.0 / 60.0;
    frame(&mut ui, &mut state, &mut actions, release);
    assert!(
        super::COMMITTED_ORDER.with(|hold| hold.borrow().is_some()),
        "the hold is active after the drop"
    );

    // Focus the row that now sits in slot 0 (Playlist 2) with a click.
    actions.clear();
    let mut press = input_at(playlist_center(0), 2.2);
    press.mouse_left_down = true;
    press.mouse_left_pressed = true;
    frame(&mut ui, &mut state, &mut actions, press);
    let mut release = input_at(playlist_center(0), 2.3);
    release.mouse_left_released = true;
    frame(&mut ui, &mut state, &mut actions, release);
    actions.clear();

    // Alt+Down while the hold is active: no move.
    let mut alt = input_at(playlist_center(0), 2.4);
    alt.modifiers = Modifiers {
        alt: true,
        ..Modifiers::default()
    };
    alt.keys = vec![KeyInput {
        key: Key::Down,
        pressed: true,
        repeat: false,
        modifiers: alt.modifiers,
    }];
    frame(&mut ui, &mut state, &mut actions, alt);
    assert!(
        !actions
            .iter()
            .any(|a| matches!(a, Action::MovePlaylist { .. })),
        "Alt+Down is ignored during the committed hold: {actions:?}"
    );
    assert!(
        super::COMMITTED_ORDER.with(|hold| hold.borrow().is_some()),
        "the hold is still active (the alt frame issued no move)"
    );
}
