//! A visual QA gallery for the widget set (test-only; GPU-required tests are
//! `#[ignore]` so CI without a Vulkan device still runs the pure paint test).
//!
//! ```text
//! cargo test -p rift-desktop --lib ui::gallery -- --ignored --nocapture
//! ```
//!
//! renders `crates/rift-desktop/target/ui-gallery.png` (scale 1.0), `ui-gallery-125.png`,
//! `ui-gallery-150.png` (simulated `HiDPI`: glyphs rasterised at physical size)
//! and `ui-image-check.png` (the `ui.image` / texture-upload path with a known
//! 64x64 RGBA gradient), all through the real renderer.

use super::*;
use crate::text::{TextStyle, Weight};

/// The offscreen renderer owns process-wide GPU state, so the ignored GPU tests
/// must not run concurrently (they do by default: one test thread per test).
static GPU_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Texture id used by the gallery's gradient asset.
const GRADIENT_TEX: TextureId = FIRST_IMAGE_TEXTURE_ID;

/// Build a 64x64 straight-alpha gradient like the CSS cover fallback
/// `linear-gradient(135deg, #1c1c22, #141417)` (top-left lighter, bottom-right
/// darker).
fn gradient_rgba() -> Vec<u8> {
    let mut rgba = Vec::with_capacity(64 * 64 * 4);
    for y in 0..64u32 {
        for x in 0..64u32 {
            let t = (x + y) as f32 / 126.0;
            let mix = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round() as u8;
            rgba.extend_from_slice(&[mix(0x1c, 0x14), mix(0x1c, 0x14), mix(0x22, 0x17), 255]);
        }
    }
    rgba
}

/// Build the gallery frame. Split out from the tests so it is also exercised as
/// a pure paint test without a GPU.
fn build_gallery(ui: &mut Ui, upload_assets: bool) {
    let theme = ui.theme().clone();
    let viewport = ui.viewport();
    ui.fill(viewport, theme.bg);
    if upload_assets {
        ui.upload_texture(GRADIENT_TEX, 64, 64, gradient_rgba());
    }
    let pad = 18.0;
    let body = TextStyle::new(13.0, Weight::Regular, theme.text);
    let dim = TextStyle::new(12.0, Weight::Regular, theme.text_dim);

    // Row/column cursors exercise the layout helpers.
    let mut col = Column::new(viewport.inset(pad), 14.0);
    ui.eyebrow(col.fixed(14.0), "IMGUI WIDGET GALLERY");

    // ---- buttons ----
    let row = col.fixed(40.0);
    let mut r = Row::new(row, 10.0);
    ui.button(r.fixed(110.0), "Primary", ButtonKind::Primary);
    ui.button(r.fixed(120.0), "Secondary", ButtonKind::Secondary);
    ui.button(r.fixed(90.0), "Ghost", ButtonKind::Ghost);
    ui.button(r.fixed(80.0), "Tab", ButtonKind::Tab { active: true });
    ui.button(r.fixed(120.0), "Play", ButtonKind::PlayAll);
    ui.button(r.fixed(140.0), "Download", ButtonKind::HeadButton);
    ui.button(r.fixed(100.0), "Danger", ButtonKind::Danger);
    ui.button_enabled(r.fixed(110.0), "Disabled", ButtonKind::Primary, false);

    // ---- icon buttons + transport ----
    let row = col.fixed(40.0);
    let mut r = Row::new(row, 6.0);
    for icon in [
        Icon::Play,
        Icon::Pause,
        Icon::SkipNext,
        Icon::SkipPrev,
        Icon::Shuffle,
        Icon::Repeat,
        Icon::RepeatOne,
        Icon::Heart,
        Icon::HeartFilled,
        Icon::VolumeUp,
        Icon::VolumeOff,
        Icon::Search,
        Icon::Home,
        Icon::MusicNote,
        Icon::Settings,
        Icon::Download,
        Icon::MoreVert,
        Icon::Delete,
        Icon::Close,
        Icon::Info,
        Icon::CloudOff,
        Icon::DragHandle,
        Icon::Check,
        Icon::Upload,
    ] {
        let cell = r.fixed(28.0);
        ui.icon_button(cell, icon);
    }
    ui.play_button(r.fixed(36.0), true);

    // ---- typography ----
    let row = col.fixed(56.0);
    ui.text(
        Rect::new(row.x, row.y, row.w * 0.5, 20.0),
        "Inter 400 13px body text",
        &body,
    );
    ui.text(
        Rect::new(row.x, row.y + 18.0, row.w * 0.5, 20.0),
        "Inter 600 semibold · dim",
        &TextStyle::new(13.0, Weight::SemiBold, theme.text_dim),
    );
    ui.text(
        Rect::new(row.x + row.w * 0.5, row.y, row.w * 0.5, 22.0),
        "Inter 700 17px heading",
        &TextStyle::new(17.0, Weight::Bold, theme.text),
    );
    ui.text(
        Rect::new(row.x + row.w * 0.5, row.y + 24.0, 300.0, 20.0),
        "Ellipsized: a very long label that cannot fit",
        &dim,
    );

    // ---- inputs + sliders ----
    let row = col.fixed(110.0);
    let mut r = Row::new(row, 16.0);
    let style = TextInputStyle::default_theme(&theme);
    let mut text = String::from("editable text");
    ui.text_input(
        r.fixed(220.0).with_h(33.0),
        Id::from_str("gallery/input"),
        &mut text,
        "Placeholder",
        &style,
    );
    // A focused field with a selection.
    let focused_id = Id::from_str("gallery/input-focused");
    ui.request_focus(focused_id);
    let mut text2 = String::from("selected text");
    ui.text_input(
        r.fixed(220.0).with_h(33.0),
        focused_id,
        &mut text2,
        "Placeholder",
        &style,
    );
    let mut v = 0.35f32;
    ui.slider(r.fixed(200.0).with_h(4.0), &mut v, 0.0, 1.0);
    let mut v2 = 0.75f32;
    ui.slider(r.fixed(120.0).with_h(4.0), &mut v2, 0.0, 1.0);

    // ---- bools, badges, progress ----
    let row = col.fixed(34.0);
    let mut r = Row::new(row, 16.0);
    let mut on = true;
    ui.switch(r.fixed(44.0).with_h(24.0), &mut on);
    let mut off = false;
    ui.switch(r.fixed(44.0).with_h(24.0), &mut off);
    let mut checked = true;
    ui.checkbox(r.fixed(120.0), &mut checked, "Checkbox");
    ui.badge(r.fixed(90.0), "Downloaded", BadgeKind::Success);
    ui.badge(r.fixed(70.0), "Missing", BadgeKind::Danger);
    ui.badge(r.fixed(70.0), "6 songs", BadgeKind::Neutral);
    ui.progress(r.fixed(160.0).with_h(6.0), 0.42, theme.accent);
    ui.spinner(r.fixed(20.0).with_h(20.0));
    ui.equalizer(r.fixed(16.0).with_h(14.0), true);

    // ---- images (gradient fallback texture through ui.image) ----
    let row = col.fixed(70.0);
    let mut r = Row::new(row, 10.0);
    let cover_rect = r.fixed(64.0);
    ui.rounded_rect(cover_rect, theme.bg_raised, 6.0);
    ui.image(cover_rect, GRADIENT_TEX, Color::WHITE, 6.0);
    // Tinted variant (multiply): 50% red tint should darken green/blue.
    let tinted = r.fixed(64.0);
    ui.image(tinted, GRADIENT_TEX, Color::rgba(255, 80, 80, 255), 6.0);
    ui.cover(r.fixed(64.0), None, 6.0);
    ui.cover_round(r.fixed(56.0), None);
    ui.icon_outlined(
        r.fixed(24.0).with_h(24.0),
        Icon::Heart,
        theme.text_dim,
        20.0,
    );
    ui.icon(
        r.fixed(24.0).with_h(24.0),
        Icon::HeartFilled,
        theme.accent,
        20.0,
    );
    ui.elevated_card(r.fixed(120.0), 10.0);
    ui.panel(r.fixed(90.0), 8.0);
    ui.divider(r.fixed(80.0));

    // ---- wrapped description (settings-style line boxes) ----
    let row = col.fixed(40.0);
    let desc = TextStyle::new(13.0, Weight::Regular, theme.text_dim).with_line_height(17.0);
    ui.text_wrapped(
        Rect::new(row.x, row.y, row.w * 0.5, row.h),
        "Rift uses yt-dlp to fetch audio. It's auto-detected from your PATH and common install locations.",
        &desc,
    );

    // ---- scroll container with clipped rows ----
    let row = col.fixed(120.0);
    let (_, _scroll) =
        ui.scroll_area_stable(Id::from_str("gallery/scroll"), row, 400.0, |ui, content| {
            for i in 0..14 {
                let y = content.y + i as f32 * 28.0;
                let rr = Rect::new(content.x, y, content.w, 26.0);
                ui.rounded_rect(
                    rr,
                    if i % 2 == 0 {
                        theme.bg_raised
                    } else {
                        theme.bg
                    },
                    5.0,
                );
                ui.text(
                    rr.inset_pad(Padding::symmetric(8.0, 0.0)),
                    &format!("Scroll row {i}"),
                    &body,
                );
            }
        });
}

fn viewport() -> Rect {
    Rect::new(0.0, 0.0, 1160.0, 740.0)
}

fn input(time: f64, scale: f32) -> InputState {
    InputState {
        viewport: viewport(),
        scale_factor: scale,
        time,
        delta_time: 1.0 / 60.0,
        mouse_in_window: true,
        mouse_pos: (620.0, 250.0),
        ..InputState::default()
    }
}

/// Build one gallery frame (with the open-popup/tooltip/toast overlays on the
/// second frame) and return its paint list.
fn build_frame(ui: &mut Ui, frame_index: u32, scale: f32) -> PaintList {
    let mut at = input(0.16, scale);
    if frame_index == 0 {
        ui.begin_frame(at.clone(), viewport());
        build_gallery(ui, true);
        return ui.end_frame();
    }
    // Frame 2: overlays on top of the gallery.
    let menu_id = Id::from_str("gallery/menu");
    ui.open_popup(menu_id);
    ui.toast_error("A toast notification");
    // Frame index 2 keeps the overlays open and lets the tooltip clear its
    // hover delay (it needs two frames of hovering).
    at.time = if frame_index >= 2 { 1.15 } else { 0.62 };
    at.delta_time = 0.46;
    at.mouse_pos = (960.0, 50.0);
    ui.begin_frame(at, viewport());
    build_gallery(ui, false);
    ui.tooltip(
        Id::from_str("gallery/tooltip"),
        Rect::new(950.0, 40.0, 28.0, 28.0),
        true,
        "Tooltip text",
    );
    ui.popup(
        menu_id,
        Rect::new(950.0, 40.0, 28.0, 28.0),
        Size::new(MENU_MIN_CONTENT_WIDTH, 3.0 * MENU_ITEM_HEIGHT),
        PopupAlign::Right,
        |ui, content| {
            let mut col = Column::new(content, 0.0);
            ui.menu_item(
                col.fixed(MENU_ITEM_HEIGHT),
                "Play next",
                Some(Icon::PlaylistAdd),
                None,
                false,
            );
            ui.menu_item_submenu(
                col.fixed(MENU_ITEM_HEIGHT),
                "Add to playlist",
                Some(Icon::PlaylistAdd),
                false,
            );
            ui.menu_separator(col.fixed(9.0));
            ui.menu_item(
                col.fixed(MENU_ITEM_HEIGHT),
                "Delete",
                Some(Icon::Delete),
                Some("Del"),
                true,
            );
        },
    );
    ui.end_frame()
}

/// Pure paint test: the gallery must produce prims in multiple clipped layers.
#[test]
fn gallery_builds_a_non_empty_paint_list() {
    let mut ui = Ui::new();
    let first = build_frame(&mut ui, 0, 1.0);
    // The gradient texture is uploaded during the first frame.
    let created = first
        .created
        .iter()
        .find(|t| t.id == GRADIENT_TEX)
        .expect("gradient texture upload");
    assert_eq!((created.width, created.height), (64, 64));
    assert_eq!(created.rgba.len(), 64 * 64 * 4);
    let paint = build_frame(&mut ui, 1, 1.0);
    assert!(!paint.is_empty());
    assert!(
        paint.layers.len() >= 2,
        "scroll container must add a clipped layer"
    );
    assert!(paint.layers.iter().any(|l| l.clip.is_some()));
    let prims: usize = paint.layers.iter().map(|l| l.prims.len()).sum();
    assert!(prims > 100, "gallery should paint many prims, got {prims}");
    assert!(
        paint.created.is_empty(),
        "nothing new is uploaded on frame 2"
    );
}

/// Render the gallery at 1.0, 1.25 and 1.5 scale (GPU required; ignored by
/// default). At 1.25/1.5 the text is rasterised at the physical size and the
/// renderer scales geometry, so the PNG crops stay crisp.
#[test]
#[ignore = "requires a Vulkan device; run manually for visual QA"]
fn render_widget_gallery_png() {
    let _gpu = GPU_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    std::fs::create_dir_all("target").ok();
    for (scale, name) in [
        (1.0f32, "target/ui-gallery.png"),
        (1.25, "target/ui-gallery-125.png"),
        (1.5, "target/ui-gallery-150.png"),
    ] {
        let mut ui = Ui::new();
        let first = build_frame(&mut ui, 0, scale);
        let _second = build_frame(&mut ui, 1, scale);
        let mut paint = build_frame(&mut ui, 2, scale);
        // Earlier frames are not rendered directly; hand their texture uploads to
        // the renderer with the final frame (it consumes `created` before drawing).
        paint.created.extend(first.created);
        let scale_px = |v: f32| (v * scale).round() as u32;
        crate::gfx::screenshot(
            scale_px(1160.0),
            scale_px(740.0),
            scale,
            &paint,
            std::path::Path::new(name),
        )
        .expect("offscreen screenshot");
        eprintln!("wrote {name} (scale {scale})");
    }
}

/// Render only the gradient/tint image path and verify the uploaded RGBA pixels
/// round-trip through `ui.image` (GPU required; ignored by default).
#[test]
#[ignore = "requires a Vulkan device; run manually for visual QA"]
fn render_image_texture_png() {
    let _gpu = GPU_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    std::fs::create_dir_all("target").ok();
    let vp = Rect::new(0.0, 0.0, 200.0, 120.0);
    let mut ui = Ui::new();
    let mut at = InputState {
        viewport: vp,
        scale_factor: 1.0,
        time: 0.0,
        mouse_in_window: true,
        ..InputState::default()
    };
    at.delta_time = 1.0 / 60.0;
    ui.begin_frame(at, vp);
    ui.fill(vp, Color::hex(0x0e0e10));
    ui.upload_texture(GRADIENT_TEX, 64, 64, gradient_rgba());
    ui.image(
        Rect::new(20.0, 20.0, 64.0, 64.0),
        GRADIENT_TEX,
        Color::WHITE,
        0.0,
    );
    ui.image(
        Rect::new(110.0, 20.0, 64.0, 64.0),
        GRADIENT_TEX,
        Color::rgba(255, 80, 80, 255),
        0.0,
    );
    let paint = ui.end_frame();

    let path = std::path::Path::new("target/ui-image-check.png");
    crate::gfx::screenshot(200, 120, 1.0, &paint, path).expect("offscreen screenshot");

    // Verify the texture path: the white-tinted copy must show the gradient
    // (lighter top-left #1c1c22, darker bottom-right #141417) and the red-tinted
    // copy must multiply green/blue down.
    let img = image::open(path).expect("open rendered png").to_rgb8();
    let px = |x: u32, y: u32| {
        let p = img.get_pixel(x, y);
        [p[0] as i32, p[1] as i32, p[2] as i32]
    };
    let tl = px(22, 22);
    let br = px(82, 82);
    assert!(
        tl[0] >= 25 && tl[2] >= 30,
        "top-left should be the light end of the gradient, got {tl:?}"
    );
    assert!(
        br[0] <= tl[0] && br[2] <= tl[2],
        "gradient must get darker towards the bottom-right: {tl:?} -> {br:?}"
    );
    let red = px(112, 22);
    assert!(
        red[1] < tl[1] / 2 && red[2] < tl[2] / 2,
        "red tint must multiply green/blue: {red:?} vs {tl:?}"
    );
    eprintln!("wrote {} + verified gradient/tint pixels", path.display());
}
