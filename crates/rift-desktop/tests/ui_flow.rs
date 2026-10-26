//! Headless end-to-end interaction tests: synthetic input → immediate-mode UI →
//! `Action`s → controller commands.
//!
//! These tests drive the *real* app (not a mock screen) through the same
//! per-frame path `main.rs` uses: `App::tick()` then `App::frame(InputState)`,
//! with a recording [`BackendCommands`] double installed through
//! [`Controller::with_commands`]. No window, GPU, network or backend is created,
//! so they run on any machine with `cargo test -p rift-desktop`.
//!
//! # Coordinate assumptions (read this before editing a test)
//!
//! The window is the reference size, **1160×740 logical px, scale factor 1.0**,
//! with the preview state (`App::new(true, …)`): sidebar 335 px wide, Home view,
//! "Song Title 1" playing at 83/214 s, a six-track queue and three playlists.
//! `screens::layout` then gives the exact bands: titlebar `0,0,1160,46`, sidebar
//! `0,46,335,610`, content `335,46,825,610`, main `335,46,825,375`, queue
//! `335,421,825,235`, player `0,656,1160,84`. The helpers below recompute the
//! widget rectangles from the same constants/formulas the screens use
//! (`screens/chrome/`, `screens/cards/`, `settings.rs`, `queue.rs`, `menu.rs`), so a change
//! to a screen's geometry fails the interaction that depends on it rather than
//! silently clicking empty space. Only numbers that are private to a screen
//! module (titlebar/sidebar paddings, `TITLE_BLOCK_H`, `BLOCK_GAP`, …) are
//! mirrored here, each with the source it mirrors.
//!
//! Every click is two frames, exactly like the framework's state machine
//! requires: a press frame (hover + `mouse_left_pressed/down`) then a release
//! frame (hover + `mouse_left_released`) over the same widget. Actions are
//! dispatched after `ui.end_frame()`, so assertions are made after the release
//! frame returns.
//!
//! The tests share session-scoped screen statics: the queue collapse state
//! (`screens/queue.rs` behind `set_collapsed`/`collapsed`), the open context
//! menu (`screens/menu.rs` behind `close`/`is_open`) and the one-shot generated
//! texture guard in `screens/cards/textures.rs`. A process-wide lock serialises
//! the tests, and `Harness::new` resets the queue/menu state so test order never
//! matters.

use std::sync::{Arc, Mutex, MutexGuard};

use rift_desktop::app::{App, WindowCommand};
use rift_desktop::controller::{BackendCommands, Controller};
use rift_desktop::gfx::paint::{PaintList, Prim, Rect};
use rift_desktop::screens::{self, cards, menu, overlays, queue as queue_screen, settings};
use rift_desktop::state::View;
use rift_desktop::text::Weight;
use rift_desktop::theme::Theme;
use rift_desktop::ui::{InputState, Key, KeyInput, Modifiers, Ui, SCROLLBAR_WIDTH};
use rift_types::{Bootstrap, Playlist, Track, UpdateStatus};

// ---------------------------------------------------------------------------
// serialisation
// ---------------------------------------------------------------------------

static TEST_LOCK: Mutex<()> = Mutex::new(());

/// The screens keep a few session-scoped statics (queue collapse, the open
/// menu); run these tests one at a time so they cannot interfere.
fn test_lock() -> MutexGuard<'static, ()> {
    TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

// ---------------------------------------------------------------------------
// recording backend double
// ---------------------------------------------------------------------------

/// One command the app's controller forwarded to the backend boundary. Only the
/// commands these tests assert are represented; the rest of the trait surface is
/// implemented as inert.
#[derive(Clone, Debug, PartialEq)]
enum Call {
    TogglePlay,
    NextTrack,
    PrevTrack,
    Seek(f64),
    SetVolume(f32),
    SaveVolume(f32),
    ToggleLike(String),
    QueueClear,
    QueueJump(usize),
    SetDiscordRpc(bool),
    SetCrossfade(f32),
    RenamePlaylist(String, String),
    SearchSongs(String),
    OpenUrl(String),
}

type Calls = Arc<Mutex<Vec<Call>>>;

/// Recording implementation of the controller's command seam.
struct Recorder {
    calls: Calls,
}

impl Recorder {
    fn record(&self, call: Call) {
        self.calls
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(call);
    }
}

impl BackendCommands for Recorder {
    fn bootstrap(&self) -> Result<Bootstrap, String> {
        Ok(Bootstrap::default())
    }

    // ------------------------------------------------------------ playback
    fn play_tracks(&self, _: Vec<Track>, _: usize, _: Option<String>) {}
    fn play_track(&self, _: Track) {}
    fn toggle_play(&self) {
        self.record(Call::TogglePlay);
    }
    fn next_track(&self) {
        self.record(Call::NextTrack);
    }
    fn prev_track(&self) {
        self.record(Call::PrevTrack);
    }
    fn seek(&self, position: f64) {
        self.record(Call::Seek(position));
    }
    fn set_volume(&self, volume: f32) {
        self.record(Call::SetVolume(volume));
    }
    fn save_volume(&self, volume: f32) {
        self.record(Call::SaveVolume(volume));
    }
    fn set_discord_rpc(&self, enabled: bool) {
        self.record(Call::SetDiscordRpc(enabled));
    }
    fn set_crossfade(&self, secs: f32) {
        self.record(Call::SetCrossfade(secs));
    }
    fn toggle_shuffle(&self) {}
    fn cycle_repeat(&self) {}

    // --------------------------------------------------------------- queue
    fn queue_add(&self, _: Track) {}
    fn queue_play_next(&self, _: Vec<Track>) {}
    fn queue_move(&self, _: usize, _: usize) {}
    fn queue_remove(&self, _: usize) {}
    fn queue_jump(&self, index: usize) {
        self.record(Call::QueueJump(index));
    }
    fn queue_clear(&self) {
        self.record(Call::QueueClear);
    }

    // ------------------------------------------------------------- library
    fn toggle_like(&self, track: Track) {
        self.record(Call::ToggleLike(track.id));
    }
    fn create_playlist(&self, _: String) -> Result<(), String> {
        Ok(())
    }
    fn import_yt_playlist(&self, _: String) {}
    fn export_playlist(&self, _: String) {}
    fn import_playlist(&self) {}
    fn delete_playlist(&self, _: String) {}
    fn rename_playlist(&self, id: String, name: String) {
        self.record(Call::RenamePlaylist(id, name));
    }
    fn move_playlist(&self, _: String, _: usize) {}
    fn add_to_playlist(&self, _: String, _: Track) {}
    fn remove_from_playlist(&self, _: String, _: String) {}

    // ------------------------------------------------------------ downloads
    fn download_tracks(&self, _: Vec<Track>) {}
    fn remove_downloads(&self, _: Vec<String>) {}

    // -------------------------------------------------- updates / settings
    fn check_update(&self) {}
    fn set_update_notifications(&self, _: bool) {}
    fn open_url(&self, url: String) -> Result<(), String> {
        self.record(Call::OpenUrl(url));
        Ok(())
    }

    // ------------------------------------------------------ search/catalog
    fn search_songs(&self, _: u64, query: String) {
        self.record(Call::SearchSongs(query));
    }
    fn search_artists(&self, _: u64, _: String) {}
    fn search_albums(&self, _: u64, _: String) {}
    fn load_artist_page(&self, _: u64, _: String) {}
    fn load_artist_songs(&self, _: u64, _: String) {}
    fn load_album_page(&self, _: u64, _: String) {}

    // --------------------------------------------------------------- images
    fn fetch_cover(&self, _: String) {}

    // --------------------------------------------------------- connectivity
    fn check_online(&self) {}
}

// ---------------------------------------------------------------------------
// harness
// ---------------------------------------------------------------------------

const WIDTH: f32 = 1160.0;
const HEIGHT: f32 = 740.0;
const VIEWPORT: Rect = Rect::new(0.0, 0.0, WIDTH, HEIGHT);

struct Harness {
    app: App,
    calls: Calls,
    time: f64,
    mouse: (f32, f32),
}

impl Harness {
    fn new() -> Self {
        let calls: Calls = Arc::new(Mutex::new(Vec::new()));
        let recorder = Recorder {
            calls: Arc::clone(&calls),
        };
        let controller = Controller::with_commands(Box::new(recorder));
        let mut app = App::new(true, controller);
        // Session-scoped screen statics: start every test from the same state so
        // test order can never matter.
        queue_screen::set_collapsed(false);
        menu::close(&mut app.ui);
        Self {
            app,
            calls,
            time: 0.0,
            mouse: (0.0, 0.0),
        }
    }

    /// One frame with the given input modifiers. Also does the pre-frame
    /// `App::tick()` that `main.rs` runs every redraw.
    fn frame_with(&mut self, customise: impl FnOnce(&mut InputState)) -> PaintList {
        self.time += 1.0 / 60.0;
        let mut input = InputState {
            viewport: VIEWPORT,
            scale_factor: 1.0,
            time: self.time,
            delta_time: 1.0 / 60.0,
            mouse_pos: self.mouse,
            mouse_in_window: true,
            ..InputState::default()
        };
        customise(&mut input);
        self.app.tick();
        self.app.frame(input)
    }

    fn frame(&mut self) -> PaintList {
        self.frame_with(|_| {})
    }

    fn move_to(&mut self, x: f32, y: f32) -> PaintList {
        self.mouse = (x, y);
        self.frame()
    }

    /// One frame with a wheel delta at the current pointer position (winit
    /// convention: `+y` is wheel up, so a negative value scrolls down).
    fn wheel(&mut self, dy: f32) -> PaintList {
        self.frame_with(|input| input.scroll = (0.0, dy))
    }

    fn press(&mut self, x: f32, y: f32) -> PaintList {
        self.mouse = (x, y);
        self.frame_with(|input| {
            input.mouse_left_pressed = true;
            input.mouse_left_down = true;
        })
    }

    fn release(&mut self, x: f32, y: f32) -> PaintList {
        self.mouse = (x, y);
        self.frame_with(|input| input.mouse_left_released = true)
    }

    /// Press then release over the same point: one complete click.
    fn click(&mut self, x: f32, y: f32) {
        self.press(x, y);
        self.release(x, y);
    }

    /// One key-press frame (press edge; the release edge is not needed by the
    /// keyboard shortcut handlers).
    fn key(&mut self, key: Key, modifiers: Modifiers) {
        self.frame_with(|input| {
            input.modifiers = modifiers;
            input.keys.push(KeyInput {
                key,
                pressed: true,
                repeat: false,
                modifiers,
            });
        });
    }

    /// One IME/text frame.
    fn type_text(&mut self, text: &str) {
        self.frame_with(|input| input.text = text.to_string());
    }

    fn calls(&self) -> Vec<Call> {
        self.calls
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn take_calls(&self) -> Vec<Call> {
        std::mem::take(
            &mut *self
                .calls
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        )
    }

    fn contains(&self, predicate: impl Fn(&Call) -> bool) -> bool {
        self.calls().iter().any(predicate)
    }

    fn layout(&self) -> screens::Layout {
        screens::layout(&self.app.ui, &self.app.state)
    }
}

fn approx(a: f32, b: f32) -> bool {
    (a - b).abs() < 0.01
}

// ---------------------------------------------------------------------------
// coordinate helpers — each mirrors the formulas in the screen it drives
// ---------------------------------------------------------------------------

fn sidebar_home_center(l: &screens::Layout) -> (f32, f32) {
    // screens/chrome/sidebar.rs: SIDE_PAD = 10, SIDE_PAD_V = 12; the first row sits there.
    let x = l.sidebar.x + 10.0 + (l.sidebar.w - 20.0) * 0.5;
    (x, l.sidebar.y + 12.0 + 19.0)
}

fn sidebar_liked_center(l: &screens::Layout) -> (f32, f32) {
    // screens/chrome/sidebar.rs: 38 px row + 3 px gap below Home.
    let (x, home_y) = sidebar_home_center(l);
    (x, home_y + 38.0 + 3.0)
}

fn sidebar_settings_center(l: &screens::Layout) -> (f32, f32) {
    // screens/chrome/sidebar.rs: SETTINGS_FROM_BOTTOM = 12 (pad) + 3 (margin) + 38 (row).
    let x = l.sidebar.x + 10.0 + (l.sidebar.w - 20.0) * 0.5;
    (x, l.sidebar.bottom() - 53.0 + 19.0)
}

fn sidebar_playlist_center(l: &screens::Layout, index: usize) -> (f32, f32) {
    // screens/chrome/sidebar.rs: y = pad_v 12 | Home 38 + gap 3 | Liked 38 | +20, PLAYLISTS
    // label 13 + 6, then playlist rows of 38 + 3.
    let x = l.sidebar.x + 10.0 + (l.sidebar.w - 20.0) * 0.5;
    let y = l.sidebar.y
        + 12.0
        + 38.0
        + 3.0
        + 38.0
        + 20.0
        + 13.0
        + 6.0
        + index as f32 * (38.0 + 3.0)
        + 19.0;
    (x, y)
}

/// The sidebar's scrollable playlist region (the nav area above the pinned
/// import rows): `region = (x, 176 at the reference, w, import_y − 8 − top)`,
/// where `import_y = sidebar.bottom − 151` (53 settings + 9 divider + 49 + 40).
fn sidebar_playlist_region(l: &screens::Layout) -> Rect {
    let top = l.sidebar.y + 130.0;
    let import_y = l.sidebar.bottom() - 151.0;
    Rect::new(
        l.sidebar.x + 10.0,
        top,
        l.sidebar.w - 20.0,
        (import_y - 8.0 - top).max(0.0),
    )
}

/// Centre of playlist row `index` inside the scroll region, given a scroll
/// offset (the sidebar scroll list is virtualised/clipped).
fn scrolled_playlist_center(region: Rect, index: usize, offset: f32) -> (f32, f32) {
    (
        region.center().0,
        region.y + index as f32 * 41.0 - offset + 19.0,
    )
}

/// Titlebar search text field centre (screens/chrome/titlebar.rs: box 480×31 centred, button 79
/// wide reserved on the right).
fn titlebar_search_center(l: &screens::Layout) -> (f32, f32) {
    let box_w = 480.0f32.min(l.titlebar.w * 0.42);
    let box_x = l.titlebar.x + (l.titlebar.w - box_w) * 0.5;
    let box_y = l.titlebar.y + ((l.titlebar.h - 31.0) * 0.5).floor();
    let button_x = box_x + box_w - 1.0 - 79.0;
    let input_x = box_x + 1.0;
    let input_w = (button_x - box_x - 1.0).max(0.0);
    (input_x + input_w * 0.5, box_y + 1.0 + 29.0 * 0.5)
}

/// Centre of one of the three 46 px titlebar window buttons (`0` minimize,
/// `1` maximize, `2` close).
fn titlebar_window_button_center(l: &screens::Layout, index: usize) -> (f32, f32) {
    let controls_x = l.titlebar.right() - 46.0 * 3.0;
    (
        controls_x + 46.0 * index as f32 + 23.0,
        l.titlebar.y + l.titlebar.h * 0.5,
    )
}

/// Big play/pause button centre (`screens/chrome/player_bar.rs`: five 36 px controls, block centred).
fn player_play_center(l: &screens::Layout) -> (f32, f32) {
    let player = l.player;
    let ctrl_x = player.x + (player.w - 212.0) * 0.5;
    let play_x = ctrl_x + 2.0 * (36.0 + 8.0) + 18.0;
    (play_x, player.y + 50.0)
}

/// Seek-bar point for `fraction` (`screens/chrome/player_bar.rs`: slider x=58, width 1044, 8 px thumb
/// inset; y = 15 px into the player band).
fn player_seek_point(l: &screens::Layout, fraction: f32) -> (f32, f32) {
    let player = l.player;
    let slider_x = player.x + 58.0;
    let slider_w = player.w - 58.0 - 58.0; // 1044 at the reference width
    (
        slider_x + 8.0 + (slider_w - 16.0) * fraction,
        player.y + 15.0,
    )
}

/// Volume-slider point for `fraction` (right-aligned 110 px slider, 16 px pad).
fn player_volume_point(l: &screens::Layout, fraction: f32) -> (f32, f32) {
    let player = l.player;
    let slider_x = player.right() - 16.0 - 110.0;
    (slider_x + 8.0 + (110.0 - 16.0) * fraction, player.y + 50.0)
}

/// Player-bar like heart (`screens/chrome/player_bar.rs`: `meta_x` = cover.right + 10, then the text
/// block width, then the 28 px button; measured with the real font metrics).
fn player_like_center(l: &screens::Layout) -> (f32, f32) {
    let theme = Theme::default();
    let mut probe = Ui::new();
    let title_w = probe.text_width(
        "Song Title 1",
        &cards::style_lh(13.0, Weight::Medium, theme.text, 17.0),
    );
    let artist_w = probe.text_width(
        "Artist Name",
        &cards::style_lh(12.0, Weight::Regular, theme.text_dim, 16.0),
    );
    let meta_x = l.player.x + 16.0 + 44.0 + 10.0; // PLAYER_PAD_X + cover + gap
    let like_x = meta_x + title_w.max(artist_w) + 10.0;
    (like_x + 14.0, l.player.y + 50.0)
}

fn queue_header_center(l: &screens::Layout, queue: Rect) -> (f32, f32) {
    let _ = l;
    // queue.rs: header = area.y+1, 44 px tall; keep clear of the clear button.
    (queue.x + queue.w * 0.4, queue.y + 1.0 + 22.0)
}

fn queue_clear_center(queue: Rect) -> (f32, f32) {
    // queue.rs: 28 px `.ibtn` right-aligned 16 px, vertically centred.
    (queue.right() - 16.0 - 14.0, queue.y + 1.0 + 22.0)
}

fn queue_row_center(queue: Rect, index: usize) -> (f32, f32) {
    // queue.rs: list starts under the 44 px header, rows 44 px, 8 px pad x.
    let list_y = queue.y + 1.0 + 44.0;
    (queue.x + queue.w * 0.5, list_y + index as f32 * 44.0 + 22.0)
}

/// Settings row geometry: `row_heights` comes from the screen itself; only the
/// page's `TITLE_BLOCK_H` (22 h2 + 14 margin, private in settings.rs) and the
/// row x/width are mirrored here. `Settings` hides the queue, so `l.main` is the
/// full content column.
struct SettingsGeometry {
    x: f32,
    y: f32,
    width: f32,
    heights: [f32; 3],
}

fn settings_geometry(app: &App, l: &screens::Layout) -> SettingsGeometry {
    let theme = Theme::default();
    let mut probe = Ui::new();
    let area_x = l.main.x + theme.main_padding_h;
    let area_y = l.main.y + theme.main_padding_v;
    let width = (l.main.w - theme.main_padding_h * 2.0 - SCROLLBAR_WIDTH).max(0.0);
    let row_w = width.min(settings::ROW_MAX_W);
    let heights = settings::row_heights(&mut probe, &app.state, width);
    SettingsGeometry {
        x: area_x,
        y: area_y + 36.0, // TITLE_BLOCK_H in settings.rs
        width: row_w,
        heights,
    }
}

impl SettingsGeometry {
    fn row(&self, index: usize) -> Rect {
        let mut y = self.y;
        for height in self.heights.iter().take(index) {
            y += height + settings::ROW_SPACING;
        }
        Rect::new(self.x, y, self.width, self.heights[index])
    }
}

/// Home library card rect (`slot 0` = Liked Songs, `slot 1` = first playlist).
fn home_card_rect(app: &App, l: &screens::Layout, slot: usize) -> Rect {
    let theme = Theme::default();
    let content = screens::main_content_rect(&app.ui, l.main);
    let y = content.y + cards::H2_LINE + cards::H2_MARGIN_BOTTOM;
    let (cols, card_w) = cards::grid_metrics(content.w, theme.card_min_width, theme.card_gap);
    let cover = (card_w - cards::CARD_PADDING * 2.0).max(1.0);
    let card_h = cards::card_height(cover);
    let row = slot / cols.max(1);
    let col = slot % cols.max(1);
    Rect::new(
        content.x + col as f32 * (card_w + theme.card_gap),
        y + row as f32 * (card_h + theme.card_gap),
        card_w,
        card_h,
    )
}

/// The kebab overlay on a library card (home.rs: 24×24 at the inner right edge,
/// vertically centred on the card-name line).
fn home_card_kebab(app: &App, l: &screens::Layout, slot: usize) -> Rect {
    let card = home_card_rect(app, l, slot);
    let inner = card.inset(cards::CARD_PADDING);
    let cover = inner.w;
    let name_y = inner.y + cover + cards::CARD_NAME_MARGIN;
    Rect::new(
        inner.right() - 24.0,
        name_y + (cards::CARD_NAME_LINE - 24.0) * 0.5,
        24.0,
        24.0,
    )
}

/// Centre of one item in a two-item context menu. Mirrors `Ui::popup` placement
/// (right-aligned to the anchor, 4 px below, 5 px padding) and `menu.rs`'s
/// 32 px item rows; the menus driven here are short enough that the 180 px
/// minimum content width wins, so the measurement step is not repeated.
fn menu_item_center(anchor: Rect, item_count: usize, index: usize) -> (f32, f32) {
    let content_w = menu::MENU_MIN_CONTENT_WIDTH;
    let outer_w = content_w + 10.0;
    let outer_h = item_count as f32 * menu::MENU_ITEM_HEIGHT + 10.0;
    let mut x = anchor.right() - outer_w;
    x = x.clamp(8.0, (WIDTH - outer_w - 8.0).max(8.0));
    let mut y = anchor.bottom() + 4.0;
    if y + outer_h > HEIGHT - 8.0 {
        let above = anchor.y - outer_h - 4.0;
        y = if above >= 8.0 {
            above
        } else {
            (HEIGHT - 8.0 - outer_h).max(8.0)
        };
    }
    (
        x + 5.0 + content_w * 0.5,
        y + 5.0 + index as f32 * menu::MENU_ITEM_HEIGHT + menu::MENU_ITEM_HEIGHT * 0.5,
    )
}

fn text_width(
    text: &str,
    size: f32,
    weight: Weight,
    color: rift_desktop::gfx::paint::Color,
) -> f32 {
    let mut probe = Ui::new();
    probe.text_width(text, &cards::style(size, weight, color))
}

// ---------------------------------------------------------------------------
// 1. boot: the first frame paints the whole window
// ---------------------------------------------------------------------------

#[test]
fn boot_frame_paints_the_window_bands() {
    let _lock = test_lock();
    let mut h = Harness::new();
    let paint = h.frame();

    assert_eq!(h.app.state.view, View::Home);
    assert_eq!(h.app.state.sidebar_width, 335.0, "preview reference width");

    let prims: Vec<Prim> = paint
        .layers
        .iter()
        .flat_map(|layer| layer.prims.iter().copied())
        .collect();
    assert!(
        prims.len() > 100,
        "a full window frame should paint many primitives, got {}",
        prims.len()
    );

    let has_rect = |x: f32, y: f32, w: f32, hgt: f32| {
        prims.iter().any(|prim| match prim {
            Prim::Rect { rect, .. } => {
                (rect.x - x).abs() < 0.01
                    && (rect.y - y).abs() < 0.01
                    && (rect.w - w).abs() < 0.01
                    && (rect.h - hgt).abs() < 0.01
            }
            Prim::Image { .. } => false,
        })
    };
    assert!(has_rect(0.0, 0.0, 1160.0, 46.0), "titlebar band");
    assert!(has_rect(0.0, 46.0, 335.0, 610.0), "sidebar band");
    assert!(has_rect(335.0, 46.0, 825.0, 375.0), "main band");
    assert!(has_rect(335.0, 421.0, 825.0, 235.0), "queue band");
    assert!(has_rect(0.0, 656.0, 1160.0, 84.0), "player band");

    // The transport controls and the seek thumb are painted too.
    let play = player_play_center(&h.layout());
    assert!(
        prims.iter().any(|prim| {
            matches!(prim, Prim::Rect { rect, radius, .. }
                if (*rect).contains(play.0, play.1) && *radius >= 17.0)
        }),
        "36 px play circle at {play:?}"
    );
}

// ---------------------------------------------------------------------------
// 2. sidebar navigation
// ---------------------------------------------------------------------------

#[test]
fn sidebar_rows_navigate() {
    let _lock = test_lock();
    let mut h = Harness::new();
    h.frame();

    let l = h.layout();
    let (x, y) = sidebar_liked_center(&l);
    h.click(x, y);
    assert_eq!(h.app.state.view, View::Liked, "Liked Songs row click");

    h.frame(); // render the Liked view
    let l = h.layout();
    let (x, y) = sidebar_home_center(&l);
    h.click(x, y);
    assert_eq!(h.app.state.view, View::Home, "Home row click");
}

// ---------------------------------------------------------------------------
// 3. search: focus, typing, Enter, clearing
// ---------------------------------------------------------------------------

#[test]
fn titlebar_search_types_submits_and_clears() {
    let _lock = test_lock();
    let mut h = Harness::new();
    h.frame();

    let l = h.layout();
    let (x, y) = titlebar_search_center(&l);
    h.click(x, y);
    assert!(
        h.app.ui.wants_keyboard(),
        "clicking the search field must grab keyboard focus"
    );

    h.type_text("daft punk");
    assert_eq!(h.app.state.search.query, "daft punk");
    assert!(
        h.contains(|call| matches!(call, Call::SearchSongs(query) if query == "daft punk")),
        "typing reaches the controller's search command: {:?}",
        h.calls()
    );

    // Enter submits the current contents (the old UI trimmed it).
    h.key(Key::Enter, Modifiers::default());
    assert_eq!(h.app.state.view, View::Search);
    assert_eq!(h.app.state.search.shown_query, "daft punk");
    assert!(h.app.state.search.busy, "search in flight");
    let last_search = h
        .calls()
        .into_iter()
        .filter(|call| matches!(call, Call::SearchSongs(_)))
        .next_back();
    assert_eq!(
        last_search,
        Some(Call::SearchSongs("daft punk".into())),
        "Enter re-submits the query through the controller"
    );

    // Escape clears the box, cancels the in-flight generation and leaves Search
    // for Home (`SearchClear`, ui-spec §7.4).
    h.key(Key::Escape, Modifiers::default());
    assert_eq!(h.app.state.search.query, "");
    assert_eq!(h.app.state.search.shown_query, "");
    assert!(!h.app.state.search.busy, "clearing resolves the spinner");
    assert_eq!(
        h.app.state.view,
        View::Home,
        "clearing the search box leaves the Search view"
    );
    // The framework's `wants_keyboard` is latched for the whole frame that had a
    // text field active (so a global shortcut can't fire in the same frame as the
    // Escape that closed it); one neutral frame later the keyboard is released.
    h.frame();
    assert!(!h.app.ui.wants_keyboard(), "field released the keyboard");
}

// ---------------------------------------------------------------------------
// 4. player bar: transport, seek, volume, like
// ---------------------------------------------------------------------------

#[test]
fn player_bar_clicks_reach_the_backend_bridge() {
    let _lock = test_lock();
    let mut h = Harness::new();
    h.frame();
    h.take_calls();

    let l = h.layout();

    let (x, y) = player_play_center(&l);
    h.click(x, y);
    assert!(
        h.contains(|call| *call == Call::TogglePlay),
        "play/pause click: {:?}",
        h.calls()
    );

    let (x, y) = player_seek_point(&l, 0.5);
    h.click(x, y);
    assert!(
        h.contains(|call| matches!(call, Call::Seek(position) if approx(*position as f32, 107.0))),
        "seek to the middle of the 214 s track (~107 s): {:?}",
        h.calls()
    );

    let (x, y) = player_volume_point(&l, 0.75);
    h.click(x, y);
    assert!(
        h.contains(|call| matches!(call, Call::SetVolume(volume) if approx(*volume, 0.75))),
        "volume slider at 75%: {:?}",
        h.calls()
    );
    assert!(
        h.contains(|call| matches!(call, Call::SaveVolume(volume) if approx(*volume, 0.75))),
        "release persists the same volume: {:?}",
        h.calls()
    );

    let (x, y) = player_like_center(&l);
    h.click(x, y);
    assert!(
        h.contains(|call| matches!(call, Call::ToggleLike(id) if id == "preview-1")),
        "like heart toggles the playing track: {:?}",
        h.calls()
    );
}

// ---------------------------------------------------------------------------
// 5. keyboard shortcuts
// ---------------------------------------------------------------------------

#[test]
fn keyboard_shortcuts_work_without_text_focus() {
    let _lock = test_lock();
    let mut h = Harness::new();
    h.frame();

    h.key(Key::Space, Modifiers::default());
    assert!(
        h.contains(|call| *call == Call::TogglePlay),
        "Space toggles playback: {:?}",
        h.calls()
    );

    h.take_calls();
    h.key(
        Key::Right,
        Modifiers {
            ctrl: true,
            ..Modifiers::default()
        },
    );
    assert!(
        h.contains(|call| *call == Call::NextTrack),
        "Ctrl+Right skips: {:?}",
        h.calls()
    );

    h.take_calls();
    h.key(
        Key::Left,
        Modifiers {
            ctrl: true,
            ..Modifiers::default()
        },
    );
    assert!(
        h.contains(|call| *call == Call::PrevTrack),
        "Ctrl+Left goes back: {:?}",
        h.calls()
    );

    // Escape closes the playlist rename editor (app-level state, no focus).
    h.app.state.renaming = Some(("preview-pl-1".into(), "Playlist 1".into()));
    h.key(Key::Escape, Modifiers::default());
    assert!(
        h.app.state.renaming.is_none(),
        "Escape closed the rename editor"
    );
}

// ---------------------------------------------------------------------------
// 6. queue panel
// ---------------------------------------------------------------------------

#[test]
fn queue_panel_collapses_clears_and_jumps() {
    let _lock = test_lock();
    let mut h = Harness::new();
    h.frame();

    let l = h.layout();
    let queue = l.queue.expect("the preview queue is visible on Home");

    let (x, y) = queue_header_center(&l, queue);
    h.click(x, y);
    assert!(queue_screen::collapsed(), "UP NEXT header collapses");

    // The collapsed panel is only the header, docked above the player bar — the
    // layout moved it, so re-derive its rect before clicking it again.
    h.frame();
    let l = h.layout();
    let queue = l.queue.expect("the collapsed queue still shows its header");
    assert!(
        queue.h < 100.0,
        "collapsed panel is header-height: {queue:?}"
    );
    let (x, y) = queue_header_center(&l, queue);
    h.click(x, y);
    assert!(!queue_screen::collapsed(), "clicking again expands");

    h.frame();
    let l = h.layout();
    let queue = l.queue.expect("the expanded queue is visible again");
    h.take_calls();
    let (x, y) = queue_row_center(queue, 1);
    h.click(x, y);
    assert!(
        h.contains(|call| *call == Call::QueueJump(1)),
        "clicking the second row jumps the queue: {:?}",
        h.calls()
    );

    let (x, y) = queue_clear_center(queue);
    h.click(x, y);
    assert!(
        h.contains(|call| *call == Call::QueueClear),
        "trash button clears the queue: {:?}",
        h.calls()
    );
}

// ---------------------------------------------------------------------------
// 7. window controls and drag region
// ---------------------------------------------------------------------------

#[test]
fn window_controls_emit_window_commands() {
    let _lock = test_lock();
    let mut h = Harness::new();
    h.frame();
    let _ = h.app.take_window_commands();

    let l = h.layout();
    for (index, expected) in [
        (0, WindowCommand::Minimize),
        (1, WindowCommand::ToggleMaximize),
        (2, WindowCommand::Close),
    ] {
        let (x, y) = titlebar_window_button_center(&l, index);
        h.click(x, y);
        assert_eq!(
            h.app.take_window_commands(),
            vec![expected],
            "titlebar button {index}"
        );
    }

    // Dragging an empty part of the titlebar starts a window drag.
    let (x, y) = (l.titlebar.x + 200.0, l.titlebar.y + 23.0);
    h.press(x, y);
    h.mouse = (x + 15.0, y);
    h.frame_with(|input| input.mouse_left_down = true);
    assert_eq!(
        h.app.take_window_commands(),
        vec![WindowCommand::Drag],
        "dragging the titlebar asks the OS to move the window"
    );
    h.release(x + 15.0, y);
}

// ---------------------------------------------------------------------------
// 8. settings
// ---------------------------------------------------------------------------

#[test]
fn settings_toggles_and_sliders_map_to_commands() {
    let _lock = test_lock();
    let mut h = Harness::new();
    h.frame();

    // Navigate via the sidebar, like a user would.
    let l = h.layout();
    let (x, y) = sidebar_settings_center(&l);
    h.click(x, y);
    assert_eq!(h.app.state.view, View::Settings);
    h.frame(); // render the Settings page
    let l = h.layout();

    // --- Discord Rich Presence switch (on -> off).
    let geometry = settings_geometry(&h.app, &l);
    let row = geometry.row(0);
    let switch_x = row.right() - settings::ROW_PAD_X - settings::SWITCH_W * 0.5;
    h.click(switch_x, row.center().1);
    assert!(!h.app.state.discord_rpc, "switch flipped the UI state");
    assert!(
        h.contains(|call| *call == Call::SetDiscordRpc(false)),
        "switch reaches the backend: {:?}",
        h.calls()
    );

    // --- Crossfade switch (off -> 6 s; the old UI's default when enabled).
    let geometry = settings_geometry(&h.app, &l);
    let row = geometry.row(1);
    let switch_x = row.right() - settings::ROW_PAD_X - settings::SWITCH_W * 0.5;
    h.click(switch_x, row.center().1);
    assert_eq!(h.app.state.crossfade, 6.0);
    assert!(
        h.contains(|call| matches!(call, Call::SetCrossfade(secs) if approx(*secs, 6.0))),
        "crossfade switch reaches the backend: {:?}",
        h.calls()
    );

    // --- Crossfade slider (1..=12 s) now that the row has grown.
    h.frame();
    let l = h.layout();
    let geometry = settings_geometry(&h.app, &l);
    let row = geometry.row(1);
    let theme = Theme::default();
    let desc_style = cards::style_lh(13.0, Weight::Regular, theme.text_dim, 17.0);
    let text_w =
        (row.w - settings::ROW_PAD_X * 2.0 - settings::ROW_GAP - settings::SWITCH_W).max(0.0);
    let desc = "Overlap the end of each track with the start of the next for a smooth transition.";
    let desc_h = {
        let mut probe = Ui::new();
        probe.measure_text_wrapped(desc, &desc_style, text_w).h
    };
    // settings.rs: inner_y + LABEL_H + DESC_GAP + desc_h + BLOCK_GAP (10).
    let control_y =
        row.y + settings::ROW_PAD_Y + settings::LABEL_H + settings::DESC_GAP + desc_h + 10.0;
    let slider_x = geometry.x + settings::ROW_PAD_X;
    h.take_calls();
    h.click(slider_x + 80.0, control_y + 2.0);
    assert!(
        h.contains(|call| matches!(call, Call::SetCrossfade(secs) if *secs > 6.0)),
        "dragging the crossfade slider changes the value: {:?}",
        h.calls()
    );
}

// ---------------------------------------------------------------------------
// 9. context menu: open, Escape, item action
// ---------------------------------------------------------------------------

#[test]
fn context_menu_opens_closes_and_dispatches_an_action() {
    let _lock = test_lock();
    let mut h = Harness::new();
    h.frame();

    let l = h.layout();
    // Slot 1 is the first playlist card; its kebab only exists while hovered.
    let kebab = home_card_kebab(&h.app, &l, 1);
    let (kx, ky) = kebab.center();
    h.move_to(kx, ky);
    h.frame();
    let l = h.layout();
    let kebab = home_card_kebab(&h.app, &l, 1);
    h.click(kx, ky);
    assert!(menu::is_open(), "kebab click opens the context menu");

    // Escape closes it without dispatching anything.
    let calls_before = h.take_calls();
    h.key(Key::Escape, Modifiers::default());
    assert!(
        h.app.ui.popup_open().is_none(),
        "Escape closes the framework popup"
    );
    h.frame();
    assert!(!menu::is_open(), "menu state is cleared on the next frame");
    assert_eq!(h.calls(), calls_before, "closing emits no command");

    // Re-open and activate "Rename": the mapped Action reaches the app state.
    h.click(kx, ky);
    assert!(menu::is_open(), "menu re-opened");
    let (ix, iy) = menu_item_center(kebab, 2, 0);
    h.click(ix, iy);
    h.frame();
    assert_eq!(
        h.app.state.renaming,
        Some(("preview-pl-1".into(), "Playlist 1".into())),
        "Rename item dispatches BeginRenamePlaylist"
    );
    assert!(!menu::is_open(), "menu closes after activating an item");
}

// ---------------------------------------------------------------------------
// 10. text editing and playlist rename submit
// ---------------------------------------------------------------------------

#[test]
fn text_editing_handles_backspace_and_enter_submit() {
    let _lock = test_lock();
    let mut h = Harness::new();
    h.frame();

    // ---- search box: append-at-end editing, backspace, clear.
    let l = h.layout();
    let (x, y) = titlebar_search_center(&l);
    h.click(x, y);
    h.type_text("abc");
    assert_eq!(h.app.state.search.query, "abc");
    h.key(Key::Backspace, Modifiers::default());
    assert_eq!(
        h.app.state.search.query, "ab",
        "Backspace deletes the last char"
    );
    h.key(Key::Backspace, Modifiers::default());
    assert_eq!(h.app.state.search.query, "a");
    h.type_text("d");
    assert_eq!(
        h.app.state.search.query, "ad",
        "the caret stays at the end, so typing appends"
    );
    h.key(Key::Escape, Modifiers::default());
    assert_eq!(h.app.state.search.query, "", "Escape clears the field");

    // ---- playlist rename: open it from the header kebab, edit, Enter submits.
    h.frame();
    let l = h.layout();
    let (x, y) = sidebar_playlist_center(&l, 0);
    h.click(x, y);
    assert_eq!(
        h.app.state.view,
        View::Playlist("preview-pl-1".into()),
        "sidebar playlist row navigates"
    );

    h.frame(); // render the playlist page
    let l = h.layout();
    let theme = Theme::default();
    let main = l.main;
    let area = Rect::new(
        main.x + theme.main_padding_h,
        main.y + theme.main_padding_v,
        (main.w - theme.main_padding_h * 2.0 - SCROLLBAR_WIDTH).max(0.0),
        main.h,
    );
    let title_w = text_width("Playlist 1", 17.0, Weight::Bold, theme.text);
    // playlist.rs: kebab 28×28 at title + 10, vertically centred in the 28 px head.
    let kebab = Rect::new(
        (area.x + title_w + 10.0).min(area.right() - 28.0),
        area.y,
        28.0,
        28.0,
    );
    h.click(kebab.center().0, kebab.center().1);
    assert!(menu::is_open(), "playlist header kebab opens Rename/Delete");
    let (ix, iy) = menu_item_center(kebab, 2, 0);
    h.click(ix, iy);
    h.frame(); // render the rename editor
    assert!(
        h.app.state.renaming.is_some(),
        "Rename opened the inline editor"
    );

    // Click into the field (it is not auto-focused), extend the name, submit.
    let field = Rect::new(area.x, area.y, area.w.clamp(260.0, 520.0), 32.0);
    h.click(field.center().0, field.center().1);
    h.type_text(" 2");
    h.key(Key::Enter, Modifiers::default());

    assert_eq!(h.app.state.renaming, None, "Enter closes the editor");
    assert!(
        h.contains(|call| matches!(call, Call::RenamePlaylist(id, name)
                if id == "preview-pl-1" && name == "Playlist 1 2")),
        "rename reaches the backend with the typed name: {:?}",
        h.calls()
    );
}

// ---------------------------------------------------------------------------
// 11. overlays paint last but interact first
// ---------------------------------------------------------------------------

/// The update banner floats over the queue panel and the player bar. Because
/// immediate-mode input is first-come-first-served, the root draws the overlays
/// *first* (they paint into the deferred buffer, so they still land on top):
/// otherwise the queue row underneath would latch the press and the banner's
/// Download button would never receive its click.
#[test]
fn update_banner_buttons_win_over_the_queue_panel_underneath() {
    let _lock = test_lock();
    let mut h = Harness::new();
    let url = "https://example.invalid/release";
    h.app.state.update_banner = Some(UpdateStatus {
        current: "0.4.0".into(),
        latest: Some("0.5.0".into()),
        update_available: true,
        url: Some(url.into()),
    });
    h.frame();

    // Mirror `screens::overlays`' geometry exactly.
    let theme = Theme::default();
    let white = rift_desktop::gfx::paint::Color::WHITE;
    let text = "Rift v0.5.0 is available.";
    let text_w = text_width(text, 13.0, Weight::Regular, theme.text);
    let button_w = |label: &str| text_width(label, 13.0, Weight::SemiBold, white);
    let download_w = button_w("Download") + overlays::BANNER_BTN_PAD_X * 2.0;
    let silence_w = button_w("Don't ask again") + overlays::BANNER_BTN_PAD_X * 2.0;
    let room = overlays::banner_text_room(download_w, silence_w);
    let (w, hgt) = overlays::banner_size(text_w.min(room), download_w, silence_w);
    let banner = overlays::banner_origin(VIEWPORT, (w, hgt));
    // `update_banner` lays the buttons out after the full text room (not the
    // measured text width): x starts at `rect.x + PAD_L`, the text span takes
    // `room`, then the 4 px text margin + 12 px gap.
    let download = Rect::new(
        banner.x
            + overlays::BANNER_PAD_L
            + room
            + overlays::BANNER_TEXT_MARGIN
            + overlays::BANNER_GAP,
        banner.y + (hgt - overlays::BANNER_BTN_H) * 0.5,
        download_w,
        overlays::BANNER_BTN_H,
    );

    // Precondition: the button really is over the queue panel's row band.
    let queue = h.layout().queue.expect("preview queue visible");
    assert!(
        (download.center().1 - queue.y).abs() < queue.h,
        "banner button at {download:?} must overlap the queue panel {queue:?}"
    );
    let _ = h.frame();

    let _ = h.take_calls();
    h.click(download.center().0, download.center().1);
    assert!(
        h.contains(|call| matches!(call, Call::OpenUrl(opened) if opened == url)),
        "the banner Download button receives its click: {:?}",
        h.calls()
    );
    assert!(
        !h.contains(|call| matches!(call, Call::QueueJump(_))),
        "the queue row underneath must not also act: {:?}",
        h.calls()
    );
}

// ---------------------------------------------------------------------------
// 12. sidebar playlist rows: stable ids + scrolling
// ---------------------------------------------------------------------------

fn minimal_track(id: &str) -> Track {
    Track {
        id: id.into(),
        title: format!("Song {id}"),
        artist: "Artist Name".into(),
        album: Some("Album Name".into()),
        duration: Some(100),
        cover: String::new(),
        artists: Vec::new(),
        album_id: None,
    }
}

fn playlist(id: &str, name: &str) -> Playlist {
    Playlist {
        id: id.into(),
        name: name.into(),
        tracks: vec![minimal_track("t1")],
    }
}

/// Two playlists may share a name; each row keeps its own interaction id (derived
/// from the playlist id), so a click latches and navigates the row it was on.
#[test]
fn sidebar_playlist_rows_with_duplicate_names_are_distinct() {
    let _lock = test_lock();
    let mut h = Harness::new();
    h.app.state.library.playlists = vec![
        playlist("dup-a", "Same name"),
        playlist("dup-b", "Same name"),
    ];
    h.frame();

    let l = h.layout();
    let (x, y) = sidebar_playlist_center(&l, 0);
    h.click(x, y);
    assert_eq!(
        h.app.state.view,
        View::Playlist("dup-a".into()),
        "the first row navigates to its own playlist"
    );

    h.frame();
    let l = h.layout();
    let (x, y) = sidebar_playlist_center(&l, 1);
    h.click(x, y);
    assert_eq!(
        h.app.state.view,
        View::Playlist("dup-b".into()),
        "the second same-named row navigates to its own playlist"
    );
}

/// A library with more playlists than fit in the nav area scrolls (ui-spec §6.2:
/// the sidebar's playlist region is an `overflow-y: auto` container with the
/// standard 10 px scrollbar), so every playlist stays reachable by mouse.
#[test]
fn sidebar_playlist_list_scrolls_to_reach_hidden_rows() {
    let _lock = test_lock();
    let mut h = Harness::new();
    h.app.state.library.playlists = (0..12)
        .map(|i| playlist(&format!("p{i}"), &format!("Playlist {i}")))
        .collect();
    h.frame();

    let l = h.layout();
    let region = sidebar_playlist_region(&l);
    assert!(
        region.y + 11.0 * 41.0 > region.bottom(),
        "row 11 starts clipped before scrolling (region {region:?})"
    );

    h.move_to(region.center().0, region.center().1);
    h.wheel(-400.0);
    let scroll_id = rift_desktop::ui::Id::from_str("sidebar/playlists");
    let offset = h.app.ui.scroll_offset(scroll_id);
    assert!(
        offset > 0.0,
        "the wheel scrolls the sidebar's playlist list (offset {offset})"
    );

    let (x, y) = scrolled_playlist_center(region, 11, offset);
    assert!(
        region.contains(x, y),
        "row 11 is inside the viewport after scrolling ({x}, {y} in {region:?})"
    );
    h.click(x, y);
    assert_eq!(
        h.app.state.view,
        View::Playlist("p11".into()),
        "the last playlist is reachable after scrolling"
    );
}
