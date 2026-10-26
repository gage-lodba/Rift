//! Track-row interaction tests: nested controls (heart/kebab) and per-credit
//! artist links win over the row; the row background still plays the track.

use super::*;

/// Nested row controls must win over the row itself: clicking the heart or
/// kebab must not play the track (the first widget to see the press latches
/// the active state, and `track_row` interacts nested controls first).
#[test]
fn nested_row_controls_win_over_the_row() {
    use crate::ui::InputState;

    let track = rift_types::Track {
        id: "t1".into(),
        title: "Song Title 1".into(),
        artist: "Artist Name".into(),
        album: Some("Album Name".into()),
        duration: Some(187),
        cover: String::new(),
        artists: Vec::new(),
        album_id: None,
    };
    let row = Rect::new(0.0, 0.0, 766.0, TROW_HEIGHT);
    let opts = TrackRowOpts {
        playing: false,
        liked: false,
        ..TrackRowOpts::default()
    };

    let mut ui = Ui::new();
    let mut state = State::default();
    let viewport = Rect::new(0.0, 0.0, 800.0, 200.0);
    let id = Id::from_str("test/row");

    let frame = |ui: &mut Ui, state: &mut State, input: InputState| {
        ui.begin_frame(input, viewport);
        let resp = track_row(ui, state, id, row, &track, 0, opts);
        let _ = ui.end_frame();
        resp
    };

    // Discover the kebab rect (it is only reported while revealed, which the
    // pointer over the row is; a first hover frame gives us the geometry).
    let hover = InputState {
        viewport,
        mouse_pos: (400.0, 20.0),
        mouse_in_window: true,
        time: 0.0,
        ..Default::default()
    };
    let resp = frame(&mut ui, &mut state, hover);
    let kebab = resp.kebab_rect;
    let heart = Rect::new(kebab.x - 28.0, kebab.y, 28.0, 28.0);

    // Click the heart: press frame, release frame.
    let pos = (heart.center().0, heart.center().1);
    let press = InputState {
        viewport,
        mouse_pos: pos,
        mouse_in_window: true,
        mouse_left_down: true,
        mouse_left_pressed: true,
        time: 0.1,
        ..Default::default()
    };
    let _ = frame(&mut ui, &mut state, press);
    let release = InputState {
        viewport,
        mouse_pos: pos,
        mouse_in_window: true,
        mouse_left_released: true,
        time: 0.2,
        ..Default::default()
    };
    let resp = frame(&mut ui, &mut state, release);
    assert!(resp.like, "heart click must report a like");
    assert!(!resp.played, "heart click must not play the row");

    // Click the kebab.
    let pos = (kebab.center().0, kebab.center().1);
    let press = InputState {
        viewport,
        mouse_pos: pos,
        mouse_in_window: true,
        mouse_left_down: true,
        mouse_left_pressed: true,
        time: 0.3,
        ..Default::default()
    };
    let _ = frame(&mut ui, &mut state, press);
    let release = InputState {
        viewport,
        mouse_pos: pos,
        mouse_in_window: true,
        mouse_left_released: true,
        time: 0.4,
        ..Default::default()
    };
    let resp = frame(&mut ui, &mut state, release);
    assert!(resp.menu, "kebab click must report the menu intent");
    assert!(!resp.played, "kebab click must not play the row");

    // A click on the row background still plays it.
    let pos = (200.0, 30.0);
    let press = InputState {
        viewport,
        mouse_pos: pos,
        mouse_in_window: true,
        mouse_left_down: true,
        mouse_left_pressed: true,
        time: 0.5,
        ..Default::default()
    };
    let _ = frame(&mut ui, &mut state, press);
    let release = InputState {
        viewport,
        mouse_pos: pos,
        mouse_in_window: true,
        mouse_left_released: true,
        time: 0.6,
        ..Default::default()
    };
    let resp = frame(&mut ui, &mut state, release);
    assert!(resp.played, "row background click must play the track");
}

/// Artist credits are links (the old row's `artist_links`): clicking a name
/// reports that credit and must not play the row; each credit is separate.
#[test]
fn artist_credit_click_reports_the_credit_not_play() {
    use crate::ui::InputState;

    let track = rift_types::Track {
        id: "t1".into(),
        title: "Song".into(),
        artist: "Artist One, Artist Two".into(),
        album: Some("Album Name".into()),
        duration: Some(187),
        cover: String::new(),
        artists: vec![
            rift_types::ArtistRef {
                id: Some("artist-1".into()),
                name: "Artist One".into(),
            },
            rift_types::ArtistRef {
                id: Some("artist-2".into()),
                name: "Artist Two".into(),
            },
        ],
        album_id: None,
    };
    let row = Rect::new(0.0, 0.0, 766.0, TROW_HEIGHT);
    let opts = TrackRowOpts::default();
    let mut ui = Ui::new();
    let mut state = State::default();
    let viewport = Rect::new(0.0, 0.0, 800.0, 200.0);
    let id = Id::from_str("test/artist-row");
    let frame = |ui: &mut Ui, state: &mut State, input: InputState| {
        ui.begin_frame(input, viewport);
        let resp = track_row(ui, state, id, row, &track, 0, opts);
        let _ = ui.end_frame();
        resp
    };
    let style = style_lh(12.0, Weight::Regular, Color::WHITE, TROW_ARTIST_LINE);
    let first_w = ui.text_width("Artist One", &style);
    let sep_w = ui.text_width(", ", &style);
    // Artist line starts after the cover and title line, offset by the row
    // padding (same flex decomposition as `track_row`).
    let artist_x = TROW_PAD_X + TROW_COVER + TROW_GAP;
    let artist_y = (TROW_HEIGHT - (TROW_TITLE_LINE + TROW_ARTIST_LINE)) * 0.5 + TROW_TITLE_LINE;

    let click = |ui: &mut Ui, state: &mut State, pos: (f32, f32), time: f64| {
        let press = InputState {
            viewport,
            mouse_pos: pos,
            mouse_in_window: true,
            mouse_left_down: true,
            mouse_left_pressed: true,
            time,
            ..Default::default()
        };
        let _ = frame(ui, state, press);
        let release = InputState {
            viewport,
            mouse_pos: pos,
            mouse_in_window: true,
            mouse_left_released: true,
            time: time + 0.1,
            ..Default::default()
        };
        frame(ui, state, release)
    };

    let first = click(&mut ui, &mut state, (artist_x + 5.0, artist_y + 8.0), 0.1);
    assert_eq!(
        first.artist,
        Some(0),
        "clicking the first artist credit reports credit 0"
    );
    assert!(!first.played, "an artist link click must not play the row");

    let second = click(
        &mut ui,
        &mut state,
        (artist_x + first_w + sep_w + 5.0, artist_y + 8.0),
        1.0,
    );
    assert_eq!(
        second.artist,
        Some(1),
        "the second artist credit is its own link"
    );
    assert!(!second.played, "clicking a credit must not play the row");

    // A row-background click still plays the track.
    let played = click(&mut ui, &mut state, (400.0, 27.0), 2.0);
    assert!(played.played, "row background click still plays the track");
}

/// Render the artist links (and their hover underline) through the real GPU
/// for visual inspection: `cargo test -p rift-desktop --lib
/// artist_link_hover_visual_png -- --ignored --nocapture`.
#[test]
#[ignore = "renders a PNG with the real GPU; run with --ignored for visual inspection"]
fn artist_link_hover_visual_png() {
    use crate::ui::InputState;

    let track = rift_types::Track {
        id: "t1".into(),
        title: "Song Title".into(),
        artist: "Artist One, Artist Two".into(),
        album: Some("Album Name".into()),
        duration: Some(187),
        cover: String::new(),
        artists: vec![
            rift_types::ArtistRef {
                id: Some("artist-1".into()),
                name: "Artist One".into(),
            },
            rift_types::ArtistRef {
                id: Some("artist-2".into()),
                name: "Artist Two".into(),
            },
        ],
        album_id: None,
    };
    let row = Rect::new(20.0, 60.0, 500.0, TROW_HEIGHT);
    let viewport = Rect::new(0.0, 0.0, 540.0, 160.0);
    let mut ui = Ui::new();
    let mut state = State::default();
    let id = Id::from_str("visual/artist-row");
    let artist_x = row.x + TROW_PAD_X + TROW_COVER + TROW_GAP;
    let artist_y =
        row.y + (TROW_HEIGHT - (TROW_TITLE_LINE + TROW_ARTIST_LINE)) * 0.5 + TROW_TITLE_LINE + 8.0;
    let input = InputState {
        viewport,
        mouse_pos: (artist_x + 5.0, artist_y),
        mouse_in_window: true,
        time: 0.3,
        ..InputState::default()
    };
    ui.begin_frame(input, viewport);
    let _ = track_row(
        &mut ui,
        &mut state,
        id,
        row,
        &track,
        0,
        TrackRowOpts::default(),
    );
    let paint = ui.end_frame();

    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let path = root.join("artifacts/screens/artist-link-hover.png");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    crate::gfx::screenshot(540, 160, 1.0, &paint, &path).unwrap();
    assert!(path.is_file(), "wrote {}", path.display());
}

// ---------------------------------------------------------------------------
// determinate download progress bar
// ---------------------------------------------------------------------------

use crate::gfx::paint::{PaintList, Prim};
use crate::ui::InputState;

/// Row fixture used by the bar tests: an album keeps the meta column at the
/// real `flex_each` width.
fn progress_track() -> rift_types::Track {
    rift_types::Track {
        id: "t1".into(),
        title: "Song Title".into(),
        artist: "Artist Name".into(),
        album: Some("Album Name".into()),
        duration: Some(187),
        cover: String::new(),
        artists: Vec::new(),
        album_id: None,
    }
}

fn progress_opts(download: DownloadStatus, download_progress: Option<f32>) -> TrackRowOpts {
    TrackRowOpts {
        download,
        download_progress,
        ..TrackRowOpts::default()
    }
}

/// Every solid `Prim::Rect` in the frame as `(rect, color, radius)`.
fn rect_prims(paint: &PaintList) -> Vec<(Rect, Color, f32)> {
    paint
        .layers
        .iter()
        .flat_map(|layer| layer.prims.iter())
        .filter_map(|prim| match prim {
            Prim::Rect {
                rect,
                color,
                radius,
                ..
            } => Some((*rect, *color, *radius)),
            Prim::Image { .. } => None,
        })
        .collect()
}

/// The bar prims (3 px tall) of one row render.
fn bar_prims(paint: &PaintList) -> Vec<(Rect, Color, f32)> {
    rect_prims(paint)
        .into_iter()
        .filter(|(rect, ..)| (rect.h - DOWNLOAD_BAR_H).abs() < 0.01)
        .collect()
}

/// Expected determinate-bar geometry for `row`, from the same flex
/// decomposition `track_row` performs (album width measured through the same
/// text style).
fn expected_bar(ui: &mut Ui, row: Rect) -> Rect {
    let theme = ui.theme().clone();
    let album_style = style_lh(12.0, Weight::Regular, theme.text_dim, TROW_ARTIST_LINE);
    let album_w = ui.text_width("Album Name", &album_style).min(340.0);
    let inner_w = row.w - TROW_PAD_X * 2.0;
    let fixed = TROW_COVER + TROW_GAP + album_w + TROW_DL_W + TROW_DUR_W + TROW_ACTIONS_W;
    let flex_each = ((inner_w - fixed - TROW_GAP * 5.0).max(0.0)) * 0.5;
    let meta_y = row.y + (TROW_HEIGHT - (TROW_TITLE_LINE + TROW_ARTIST_LINE)) * 0.5;
    Rect::new(
        row.x + TROW_PAD_X + TROW_COVER + TROW_GAP,
        meta_y + TROW_TITLE_LINE + TROW_ARTIST_LINE + DOWNLOAD_BAR_GAP,
        flex_each,
        DOWNLOAD_BAR_H,
    )
}

/// Render one static (unhovered) row frame.
fn paint_row(
    ui: &mut Ui,
    state: &mut State,
    track: &rift_types::Track,
    row: Rect,
    opts: TrackRowOpts,
) {
    let viewport = Rect::new(0.0, 0.0, 800.0, 200.0);
    let input = InputState {
        viewport,
        time: 0.0,
        ..InputState::default()
    };
    let id = Id::from_str("test/progress-row");
    ui.begin_frame(input, viewport);
    let _ = track_row(ui, state, id, row, track, 0, opts);
}

/// No known fraction (a track queued behind the active download): the spinner
/// alone marks the row as downloading; no determinate bar is painted.
#[test]
fn downloading_without_a_fraction_keeps_the_spinner_and_paints_no_bar() {
    let track = progress_track();
    let row = Rect::new(0.0, 0.0, 766.0, TROW_HEIGHT);
    let mut ui = Ui::new();
    let mut state = State::default();

    paint_row(
        &mut ui,
        &mut state,
        &track,
        row,
        progress_opts(DownloadStatus::Downloading, None),
    );
    let paint = ui.end_frame();
    assert!(
        bar_prims(&paint).is_empty(),
        "no fraction must not paint a determinate bar"
    );
    // The 14 px spinner is 8 two-pixel dim rects; it is the only solid paint in
    // an unhovered row (covers/icons/glyphs are image prims).
    let rects = rect_prims(&paint);
    assert_eq!(rects.len(), 8, "spinner must still paint: {rects:?}");

    // A stale/misplaced fraction on a non-downloading row is ignored too.
    let mut ui = Ui::new();
    paint_row(
        &mut ui,
        &mut state,
        &track,
        row,
        progress_opts(DownloadStatus::Downloaded, Some(0.5)),
    );
    let paint = ui.end_frame();
    assert!(
        bar_prims(&paint).is_empty(),
        "only Downloading rows paint the bar"
    );
}

/// A known fraction paints the determinate bar at the measured geometry inside
/// the 54 px row, and the row still behaves like a row (the bar is paint-only).
#[test]
fn known_fraction_paints_the_determinate_bar_inside_the_row() {
    let track = progress_track();
    let row = Rect::new(0.0, 0.0, 766.0, TROW_HEIGHT);
    let mut ui = Ui::new();
    let mut state = State::default();
    let theme = ui.theme().clone();
    let expected = expected_bar(&mut ui, row);
    let opts = progress_opts(DownloadStatus::Downloading, Some(0.5));

    paint_row(&mut ui, &mut state, &track, row, opts);
    let paint = ui.end_frame();

    let bars = bar_prims(&paint);
    assert_eq!(
        bars.len(),
        2,
        "bar = bg_hover track + accent fill, got {bars:?}"
    );
    let (track_bar, track_color, track_radius) = bars[0];
    let (fill_bar, fill_color, fill_radius) = bars[1];

    // Track: full meta-column width at artist-bottom + 3.
    assert!((track_bar.x - expected.x).abs() < 0.01, "{track_bar:?}");
    assert!((track_bar.y - expected.y).abs() < 0.01, "{track_bar:?}");
    assert!((track_bar.w - expected.w).abs() < 0.01, "{track_bar:?}");
    assert_eq!(track_bar.h, DOWNLOAD_BAR_H);
    assert!((track_radius - DOWNLOAD_BAR_H * 0.5).abs() < 0.01);
    assert_eq!(track_color, theme.bg_hover);
    assert!(
        expected.y > row.y + TROW_TITLE_LINE + TROW_ARTIST_LINE
            && (expected.y + DOWNLOAD_BAR_H) < row.bottom(),
        "bar sits in the gap between the artist line and the row bottom"
    );

    // Fill: half the track width for 50 %, same x/y, accent.
    assert!((fill_bar.x - track_bar.x).abs() < 0.01, "{fill_bar:?}");
    assert!((fill_bar.y - track_bar.y).abs() < 0.01, "{fill_bar:?}");
    assert!(
        (fill_bar.w - track_bar.w * 0.5).abs() < 0.01,
        "fill = fraction * track: {fill_bar:?} vs {track_bar:?}"
    );
    assert_eq!(fill_bar.h, DOWNLOAD_BAR_H);
    assert!((fill_radius - DOWNLOAD_BAR_H * 0.5).abs() < 0.01);
    assert_eq!(fill_color, theme.accent);
    assert!(
        fill_bar.right() <= track_bar.right() && track_bar.right() < row.right(),
        "bar stays inside the meta column and the row"
    );

    // Spinner and bar are complementary: the in-flight spinner is still there.
    let spinner = rect_prims(&paint)
        .into_iter()
        .filter(|(rect, ..)| (rect.w - 2.0).abs() < 0.01 && (rect.h - 2.0).abs() < 0.01)
        .count();
    assert_eq!(spinner, 8, "downloading rows keep the 14 px spinner");

    // The bar is paint-only: clicking it is still a row click.
    let id = Id::from_str("test/progress-row");
    let viewport = Rect::new(0.0, 0.0, 800.0, 200.0);
    let (cx, cy) = (expected.x + expected.w * 0.5, expected.y + expected.h * 0.5);
    let press = InputState {
        viewport,
        mouse_pos: (cx, cy),
        mouse_in_window: true,
        mouse_left_down: true,
        mouse_left_pressed: true,
        time: 0.1,
        ..InputState::default()
    };
    ui.begin_frame(press, viewport);
    let _ = track_row(&mut ui, &mut state, id, row, &track, 0, opts);
    let _ = ui.end_frame();
    let release = InputState {
        viewport,
        mouse_pos: (cx, cy),
        mouse_in_window: true,
        mouse_left_released: true,
        time: 0.2,
        ..InputState::default()
    };
    ui.begin_frame(release, viewport);
    let resp = track_row(&mut ui, &mut state, id, row, &track, 0, opts);
    let _ = ui.end_frame();
    assert!(
        resp.played,
        "the bar is paint-only, so clicking it still plays the row"
    );
}
