//! GPU smoke tests for the renderer (`cargo test -p rift-desktop`).
//!
//! These render hand-built [`PaintList`]s through the real Vulkan pipeline
//! (headless — no window) and assert pixel values, so pipeline, scissor, UV,
//! sRGB and rounded-rect/AA bugs are caught before the UI exists.
//!
//! Machines without a Vulkan driver skip the tests with a message instead of
//! failing: the renderer is exercised for real on the development machine.

use std::path::PathBuf;

use crate::gfx::{
    paint::{Color, PaintList, Prim, Rect},
    window_size_changed, Renderer,
};

/// Where the smoke-test PNGs are written for manual inspection.
fn artifact_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("gfx-smoke")
        .join(name)
}

fn pixel(rgba: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
    let offset = ((y * width + x) * 4) as usize;
    [
        rgba[offset],
        rgba[offset + 1],
        rgba[offset + 2],
        rgba[offset + 3],
    ]
}

fn assert_near(actual: [u8; 4], expected: [u8; 4], tolerance: i32, label: &str) {
    let close = actual
        .iter()
        .zip(expected.iter())
        .all(|(a, e)| (*a as i32 - *e as i32).abs() <= tolerance);
    assert!(
        close,
        "{label}: got {actual:?}, expected ~{expected:?} (tolerance {tolerance})"
    );
}

/// 2x2 texture: red, green / blue, white — each texel is one quadrant.
fn checker_texture(id: u64) -> crate::gfx::paint::TextureUpload {
    crate::gfx::paint::TextureUpload::new(
        id,
        2,
        2,
        vec![
            255, 0, 0, 255, // (0,0) red
            0, 255, 0, 255, // (1,0) green
            0, 0, 255, 255, // (0,1) blue
            255, 255, 255, 255, // (1,1) white
        ],
    )
}

fn build_scene() -> PaintList {
    const CLEAR: Color = Color::hex(0x14_14_18);
    let mut paint = PaintList::new(CLEAR);

    // Invisible on purpose: a texture created this frame, used below.
    paint.add_texture(checker_texture(1));

    // 1. Rounded solid rect.
    paint.push(Prim::rounded(
        Rect::new(20.0, 20.0, 100.0, 80.0),
        Color::hex(0xC8_3C_3C),
        12.0,
    ));

    // 2. Bordered rect: black fill, 4px white border, inset ring.
    paint.push(Prim::bordered(
        Rect::new(140.0, 20.0, 80.0, 80.0),
        Color::BLACK,
        0.0,
        4.0,
        Color::WHITE,
    ));

    // 3. Textured quad (2x2 texture stretched).
    paint.push(Prim::image(
        Rect::new(240.0, 20.0, 60.0, 60.0),
        1,
        Rect::new(0.0, 0.0, 1.0, 1.0),
        Color::WHITE,
    ));

    // 4. Clipped layer: a 200px-wide blue rect clipped to x in [0, 100].
    paint.push_layer(Some(Rect::new(0.0, 120.0, 100.0, 60.0)));
    paint.push(Prim::rect(
        Rect::new(0.0, 100.0, 200.0, 100.0),
        Color::hex(0x28_50_C8),
    ));

    // 5. Empty clip layer: must draw nothing at all.
    paint.push_layer(Some(Rect::new(500.0, 500.0, 10.0, 10.0)));
    paint.push(Prim::rect(
        Rect::new(0.0, 0.0, 320.0, 200.0),
        Color::hex(0xFF_00_FF),
    ));

    paint
}

/// Whether a headless Vulkan device can be created at all.
///
/// Only consulted *after* a renderer/screenshot failure: a failure with no
/// usable device is an environment skip, but a failure while the device exists
/// is a renderer regression (shader, pipeline, render pass, allocation) and
/// must fail the suite instead of silently skipping the only GPU coverage.
fn vulkan_device_available() -> bool {
    crate::gfx::gpu::GfxContext::headless().is_ok()
}

fn headless_or_skip(width: u32, height: u32) -> Option<Renderer> {
    init_logging();
    match Renderer::new_headless(width, height, 1.0) {
        Ok(renderer) => Some(renderer),
        Err(error) => {
            if vulkan_device_available() {
                panic!(
                    "a Vulkan device is available, so this is a renderer regression rather than \
                     a missing driver: {error:#}"
                );
            }
            eprintln!("skipping GPU smoke test: no usable Vulkan device\n{error:#}");
            None
        }
    }
}

/// When `RIFT_VK_VALIDATION=1` is set (e.g. in CI or a manual run), install a
/// tracing subscriber so the validation layer's messages are visible in the
/// test output. Without the env var this does nothing.
fn init_logging() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        if std::env::var("RIFT_VK_VALIDATION").is_err() {
            return;
        }
        let _ = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::new(
                "warn,vulkan=warn,rift_desktop=info",
            ))
            .try_init();
    });
}

#[test]
fn offscreen_scene_renders_expected_pixels() {
    let Some(mut renderer) = headless_or_skip(320, 200) else {
        return;
    };
    let paint = build_scene();
    let rgba = renderer
        .render_offscreen(&paint)
        .expect("headless render should succeed");
    super::write_png(&artifact_path("scene.png"), 320, 200, &rgba)
        .expect("writing the smoke-test PNG");

    let clear = [0x14u8, 0x14, 0x18, 0xFF];
    assert_near(pixel(&rgba, 320, 5, 5), clear, 3, "background/clear colour");

    // Rounded rect: inside is the fill, the extreme corner is clipped by radius.
    assert_near(
        pixel(&rgba, 320, 70, 60),
        [0xC8, 0x3C, 0x3C, 0xFF],
        2,
        "rounded rect fill",
    );
    assert_near(
        pixel(&rgba, 320, 21, 21),
        clear,
        6,
        "rounded rect corner is antialiased away",
    );

    // Border ring: on the edge it is white; the middle is the black fill.
    assert_near(
        pixel(&rgba, 320, 142, 60),
        [0xFF, 0xFF, 0xFF, 0xFF],
        3,
        "border ring (left edge)",
    );
    assert_near(
        pixel(&rgba, 320, 180, 60),
        [0x00, 0x00, 0x00, 0xFF],
        3,
        "border fill (centre)",
    );

    // Texture: 60px quad = 30px per texel, so quadrant centres are texel centres.
    // Linear filtering happens in linear light and the result is sRGB-encoded,
    // so a sub-pixel offset from the texel centre shows up as a large byte delta
    // on the dark channels (0.016 linear -> ~33 sRGB); 40 is still far below the
    // 220+ delta between the four texel colours.
    assert_near(
        pixel(&rgba, 320, 255, 35),
        [0xFF, 0x00, 0x00, 0xFF],
        40,
        "texture top-left texel",
    );
    assert_near(
        pixel(&rgba, 320, 285, 65),
        [0xFF, 0xFF, 0xFF, 0xFF],
        40,
        "texture bottom-right texel",
    );

    // Clip layer: inside the clip it is blue, outside it the blue rect is cut.
    assert_near(
        pixel(&rgba, 320, 50, 150),
        [0x28, 0x50, 0xC8, 0xFF],
        2,
        "clipped layer inside",
    );
    assert_near(
        pixel(&rgba, 320, 150, 150),
        clear,
        3,
        "clipped layer outside",
    );
    assert_near(
        pixel(&rgba, 320, 50, 110),
        clear,
        3,
        "clipped layer above clip",
    );

    // Empty layer must not paint anything (no magenta anywhere).
    assert_near(
        pixel(&rgba, 320, 300, 180),
        clear,
        3,
        "empty clip layer draws nothing",
    );

    renderer.shutdown();
}

#[test]
fn updated_texture_replaces_pixels() {
    let Some(mut renderer) = headless_or_skip(64, 64) else {
        return;
    };

    let mut paint = PaintList::new(Color::BLACK);
    paint.add_texture(crate::gfx::paint::TextureUpload::new(
        7,
        1,
        1,
        vec![255, 0, 0, 255],
    ));
    paint.push(Prim::image(
        Rect::new(0.0, 0.0, 64.0, 64.0),
        7,
        Rect::new(0.0, 0.0, 1.0, 1.0),
        Color::WHITE,
    ));
    let first = renderer.render_offscreen(&paint).expect("first render");
    assert_near(
        pixel(&first, 64, 32, 32),
        [255, 0, 0, 255],
        2,
        "red texture",
    );

    paint.update_texture(crate::gfx::paint::TextureUpload::new(
        7,
        1,
        1,
        vec![0, 0, 255, 255],
    ));
    let second = renderer.render_offscreen(&paint).expect("updated render");
    assert_near(
        pixel(&second, 64, 32, 32),
        [0, 0, 255, 255],
        2,
        "updated texture is blue",
    );

    renderer.shutdown();
}

/// A region update must patch only its rectangle of an existing image. This is
/// the atlas path: newly rasterised glyphs arrive as a small dirty rect, and the
/// renderer must neither lose the untouched pixels nor require a full re-upload.
#[test]
fn region_texture_update_patches_pixels_in_place() {
    let Some(mut renderer) = headless_or_skip(64, 64) else {
        return;
    };

    // 4x4 texture: left half red, right half green.
    let mut rgba = Vec::with_capacity(4 * 4 * 4);
    for _y in 0..4 {
        for x in 0..4 {
            if x < 2 {
                rgba.extend_from_slice(&[255, 0, 0, 255]);
            } else {
                rgba.extend_from_slice(&[0, 255, 0, 255]);
            }
        }
    }
    let mut paint = PaintList::new(Color::BLACK);
    paint.add_texture(crate::gfx::paint::TextureUpload::new(7, 4, 4, rgba));
    paint.push(Prim::image(
        Rect::new(0.0, 0.0, 64.0, 64.0),
        7,
        Rect::new(0.0, 0.0, 1.0, 1.0),
        Color::WHITE,
    ));
    let first = renderer.render_offscreen(&paint).expect("first render");
    assert_near(pixel(&first, 64, 8, 32), [255, 0, 0, 255], 3, "left red");
    assert_near(
        pixel(&first, 64, 56, 32),
        [0, 255, 0, 255],
        3,
        "right green",
    );
    paint.created.clear();

    // Patch the right half only.
    paint.update_texture_region(crate::gfx::paint::TextureRegion::new(
        7,
        2,
        0,
        2,
        4,
        [0u8, 0, 255, 255].repeat(2 * 4),
    ));
    let second = renderer.render_offscreen(&paint).expect("region render");
    assert_near(
        pixel(&second, 64, 8, 32),
        [255, 0, 0, 255],
        3,
        "region update must not clear the untouched left half",
    );
    assert_near(
        pixel(&second, 64, 56, 32),
        [0, 0, 255, 255],
        3,
        "region update is blue on the right half",
    );

    renderer.shutdown();
}

/// End-to-end proof that the incremental glyph path reaches the GPU: the first
/// frame creates the atlas, the second adds one glyph as a region update, and
/// the second render shows the extra ink. This would fail if the renderer
/// ignored region updates (frame 2 would render the first glyph only).
#[test]
fn incremental_glyph_region_update_reaches_the_gpu() {
    use crate::text::{TextStyle, Weight};
    use crate::ui::{InputState, TextureUpdate, Ui};

    let Some(mut renderer) = headless_or_skip(240, 80) else {
        return;
    };
    let viewport = Rect::new(0.0, 0.0, 240.0, 80.0);
    let text_rect = Rect::new(10.0, 10.0, 220.0, 34.0);
    let style = TextStyle::new(26.0, Weight::Regular, Color::WHITE);
    let input = |time: f64| InputState {
        viewport,
        time,
        delta_time: 0.016,
        mouse_pos: (-10.0, -10.0),
        ..InputState::default()
    };
    let bright_pixels = |rgba: &[u8]| {
        (0..80u32)
            .flat_map(|y| (0..240u32).map(move |x| (x, y)))
            .filter(|(x, y)| {
                let p = pixel(rgba, 240, *x, *y);
                p[0] > 140 && p[1] > 140 && p[2] > 140
            })
            .count()
    };

    let mut ui = Ui::new();
    ui.begin_frame(input(0.0), viewport);
    ui.text(text_rect, "A", &style);
    let paint = ui.end_frame();
    assert_eq!(
        paint.created.iter().filter(|t| t.id == 1).count(),
        1,
        "the first frame creates the glyph atlas"
    );
    let first = renderer.render_offscreen(&paint).expect("first text frame");
    let first_ink = bright_pixels(&first);

    ui.begin_frame(input(0.016), viewport);
    ui.text(text_rect, "AB", &style);
    let paint = ui.end_frame();
    let region = paint
        .updated
        .iter()
        .find_map(|update| match update {
            TextureUpdate::Region(region) if region.id == 1 => Some(region),
            _ => None,
        })
        .expect("a newly rasterised glyph must upload a region");
    assert!(
        region.rgba.len()
            < (crate::text::ATLAS_INITIAL_SIZE * crate::text::ATLAS_INITIAL_SIZE * 4) as usize,
        "the second frame must not re-upload the whole atlas"
    );
    let second = renderer
        .render_offscreen(&paint)
        .expect("second text frame");
    let second_ink = bright_pixels(&second);
    assert!(
        second_ink > first_ink + 10,
        "the incremental glyph must add ink on the GPU (first {first_ink}, second {second_ink})"
    );

    renderer.shutdown();
}

/// A whole update supersedes a region for the same id in one frame. This can
/// happen if a caller patches a texture and also replaces it this frame; the
/// documented rule is whole-wins, and the renderer must not apply the stale
/// region to the new image (which could be a different size).
#[test]
fn a_whole_update_supersedes_a_region_for_the_same_id() {
    let Some(mut renderer) = headless_or_skip(64, 64) else {
        return;
    };
    let mut paint = PaintList::new(Color::BLACK);
    paint.add_texture(crate::gfx::paint::TextureUpload::new(
        7,
        1,
        1,
        vec![255, 0, 0, 255],
    ));
    paint.push(Prim::image(
        Rect::new(0.0, 0.0, 64.0, 64.0),
        7,
        Rect::new(0.0, 0.0, 1.0, 1.0),
        Color::WHITE,
    ));
    // Both operations for id 7 in the same frame: the region must be ignored.
    paint.update_texture_region(crate::gfx::paint::TextureRegion::new(
        7,
        0,
        0,
        1,
        1,
        vec![0, 0, 255, 255],
    ));
    paint.update_texture(crate::gfx::paint::TextureUpload::new(
        7,
        1,
        1,
        vec![0, 255, 0, 255],
    ));
    let image = renderer.render_offscreen(&paint).expect("render");
    assert_near(
        pixel(&image, 64, 32, 32),
        [0, 255, 0, 255],
        3,
        "the whole update wins over the same-frame region",
    );
    renderer.shutdown();
}

/// Runtime-created textures spread over several frames must render on the frame
/// they appear, accumulate across frames, survive updates (including a size
/// change) and be replaceable by drop → create again — the exact pattern of
/// cover art arriving asynchronously from the backend while the window is live.
/// Also covers large texture ids (the UI allocates cover ids from 1024 upward).
#[test]
fn multi_frame_runtime_texture_lifecycle() {
    let Some(mut renderer) = headless_or_skip(96, 96) else {
        return;
    };
    let full = Rect::new(0.0, 0.0, 96.0, 96.0);
    let half = Rect::new(0.0, 0.0, 48.0, 96.0);
    let other_half = Rect::new(48.0, 0.0, 48.0, 96.0);
    let third = Rect::new(64.0, 0.0, 32.0, 96.0);
    let uv = Rect::new(0.0, 0.0, 1.0, 1.0);

    // Frame 1: no textures yet.
    let paint = PaintList::new(Color::BLACK);
    let first = renderer.render_offscreen(&paint).expect("frame 1");
    assert_near(
        pixel(&first, 96, 48, 48),
        [0, 0, 0, 255],
        3,
        "frame 1 clear",
    );

    // Frame 2: a texture created this frame and drawn this frame.
    let mut paint = PaintList::new(Color::BLACK);
    paint.add_texture(crate::gfx::paint::TextureUpload::new(
        1024,
        1,
        1,
        vec![255, 0, 0, 255],
    ));
    paint.push(Prim::image(full, 1024, uv, Color::WHITE));
    let frame2 = renderer.render_offscreen(&paint).expect("frame 2");
    paint.created.clear();
    paint.updated.clear();
    paint.dropped.clear();
    assert_near(
        pixel(&frame2, 96, 48, 48),
        [255, 0, 0, 255],
        3,
        "frame 2: newly created texture renders immediately",
    );

    // Frame 3: second texture created; both are referenced this frame.
    paint.add_texture(crate::gfx::paint::TextureUpload::new(
        2048,
        1,
        1,
        vec![0, 255, 0, 255],
    ));
    paint.layers.clear();
    paint.push(Prim::image(half, 1024, uv, Color::WHITE));
    paint.push(Prim::image(other_half, 2048, uv, Color::WHITE));
    let frame3 = renderer.render_offscreen(&paint).expect("frame 3");
    paint.created.clear();
    paint.updated.clear();
    paint.dropped.clear();
    assert_near(
        pixel(&frame3, 96, 20, 48),
        [255, 0, 0, 255],
        3,
        "frame 3: first texture still red on the left",
    );
    assert_near(
        pixel(&frame3, 96, 70, 48),
        [0, 255, 0, 255],
        3,
        "frame 3: second texture green on the right",
    );

    // Frame 4: third texture with a large (UI-allocated) id + replace the first
    // texture's pixels in the same frame.
    paint.add_texture(crate::gfx::paint::TextureUpload::new(
        999_999,
        1,
        1,
        vec![0, 0, 255, 255],
    ));
    paint.update_texture(crate::gfx::paint::TextureUpload::new(
        1024,
        1,
        1,
        vec![255, 255, 0, 255],
    ));
    paint.layers.clear();
    paint.push(Prim::image(half, 1024, uv, Color::WHITE));
    paint.push(Prim::image(third, 999_999, uv, Color::WHITE));
    let frame4 = renderer.render_offscreen(&paint).expect("frame 4");
    paint.created.clear();
    paint.updated.clear();
    paint.dropped.clear();
    assert_near(
        pixel(&frame4, 96, 20, 48),
        [255, 255, 0, 255],
        3,
        "frame 4: updated texture is yellow",
    );
    assert_near(
        pixel(&frame4, 96, 80, 48),
        [0, 0, 255, 255],
        3,
        "frame 4: large texture id renders blue",
    );

    // Frame 5: drop two textures, keep drawing one, and (regression guard) a
    // never-created id draws the white fallback instead of silently vanishing.
    paint.drop_texture(1024);
    paint.drop_texture(999_999);
    paint.layers.clear();
    paint.push(Prim::image(half, 2048, uv, Color::WHITE));
    paint.push(Prim::image(other_half, 424_242, uv, Color::WHITE));
    let frame5 = renderer.render_offscreen(&paint).expect("frame 5");
    paint.created.clear();
    paint.updated.clear();
    paint.dropped.clear();
    assert_near(
        pixel(&frame5, 96, 20, 48),
        [0, 255, 0, 255],
        3,
        "frame 5: surviving texture still renders after drops",
    );
    assert_near(
        pixel(&frame5, 96, 70, 48),
        [255, 255, 255, 255],
        3,
        "frame 5: unknown texture id uses the white fallback",
    );

    // Frame 6: texture updated with *different dimensions* (atlas growth path).
    paint.update_texture(crate::gfx::paint::TextureUpload::new(
        2048,
        4,
        4,
        [0u8, 128, 255, 255].repeat(16),
    ));
    paint.layers.clear();
    paint.push(Prim::image(full, 2048, uv, Color::WHITE));
    let frame6 = renderer.render_offscreen(&paint).expect("frame 6");
    paint.created.clear();
    paint.updated.clear();
    paint.dropped.clear();
    assert_near(
        pixel(&frame6, 96, 48, 48),
        [0, 128, 255, 255],
        4,
        "frame 6: resized texture (new dimensions) renders",
    );

    renderer.shutdown();
}

#[test]
fn png_screenshot_helper_writes_readable_png() {
    let path = artifact_path("screenshot-helper.png");
    let paint = build_scene();
    if let Err(error) = crate::gfx::screenshot(320, 200, 1.0, &paint, &path) {
        if vulkan_device_available() {
            panic!(
                "a Vulkan device is available, so a failing screenshot() is a renderer \
                 regression rather than a missing driver: {error:#}"
            );
        }
        eprintln!("skipping screenshot() test: {error:#}");
        return;
    }

    let decoded = image::open(&path).expect("the produced PNG must be readable");
    assert_eq!((decoded.width(), decoded.height()), (320, 200));
    let rgba = decoded.into_rgba8();
    assert_near(
        rgba.get_pixel(5, 5).0,
        [0x14, 0x14, 0x18, 0xFF],
        3,
        "PNG background pixel",
    );
}

#[test]
fn empty_paint_list_renders_clear_colour_only() {
    let Some(mut renderer) = headless_or_skip(96, 64) else {
        return;
    };
    let paint = PaintList::new(Color::hex(0x33_66_99));
    let rgba = renderer
        .render_offscreen(&paint)
        .expect("clear-only render");
    assert_near(
        pixel(&rgba, 96, 10, 10),
        [0x33, 0x66, 0x99, 0xFF],
        3,
        "clear colour from PaintList::clear",
    );
    renderer.shutdown();
}

/// Deterministic xorshift, so the stress test is reproducible.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 32) as u32
    }

    fn f32(&mut self) -> f32 {
        self.next() as f32 / u32::MAX as f32
    }

    fn color(&mut self) -> Color {
        let value = self.next();
        Color::rgba(
            (value & 0xFF) as u8,
            ((value >> 8) & 0xFF) as u8,
            ((value >> 16) & 0xFF) as u8,
            160 + (value >> 24) as u8 / 4,
        )
    }
}

/// A synthetic high-load frame: 10k primitives, 120 clipped layers (scroll
/// containers), 2000 alternating-texture primitives (draw-call switching) and
/// an atlas that grows 1024 -> 2048 with the same texture id.
///
/// This is exactly the regime the real UI can hit (big libraries/long queues on
/// a 4K window): it exercises vertex-buffer ring growth beyond the 16k-vertex
/// initial capacity, per-layer scissor changes, and texture replacement.
#[test]
fn stress_10k_prims_many_clips_and_atlas_growth() {
    let Some(mut renderer) = headless_or_skip(1600, 1000) else {
        return;
    };
    let mut rng = Rng(0x5EED_1234_5678_9ABC);

    let mut paint = PaintList::new(Color::hex(0x10_10_14));

    // The font atlas: 1024x1024 white-alpha-ish RGBA8, id 1 (contract reserved).
    let atlas_1024 = vec![200u8; 1024 * 1024 * 4];
    paint.add_texture(crate::gfx::paint::TextureUpload::new(
        1, 1024, 1024, atlas_1024,
    ));
    // A cover-art style texture, id 1024.
    let cover = (0..(256 * 256 * 4))
        .map(|i| if i % 4 == 3 { 255 } else { (i % 251) as u8 })
        .collect::<Vec<u8>>();
    paint.add_texture(crate::gfx::paint::TextureUpload::new(1024, 256, 256, cover));

    // Unclipped background: 6000 rounded/bordered rects.
    for i in 0..6000 {
        let x = (i % 100) as f32 * 16.0;
        let y = (i / 100) as f32 * 16.0;
        let radius = (rng.next() % 8) as f32;
        let border = (rng.next() % 3) as f32;
        paint.push(Prim::bordered(
            Rect::new(x, y, 4.0 + rng.f32() * 12.0, 4.0 + rng.f32() * 12.0),
            rng.color(),
            radius,
            border,
            rng.color(),
        ));
    }

    // 120 clipped scroll-container layers, 40 prims each (rects + images).
    for layer in 0..120 {
        let clip_y = layer as f32 * 8.0;
        paint.push_layer(Some(Rect::new(0.0, clip_y, 1600.0, 6.0)));
        for prim in 0..40 {
            let x = ((layer * 37 + prim * 53) % 1500) as f32;
            let y = clip_y + ((prim * 17) % 4) as f32;
            if prim % 2 == 0 {
                paint.push(Prim::rounded(Rect::new(x, y, 6.0, 5.0), rng.color(), 2.0));
            } else {
                let texture = if prim % 4 == 1 { 1 } else { 1024 };
                paint.push(Prim::image(
                    Rect::new(x, y, 8.0, 8.0),
                    texture,
                    Rect::new(0.0, 0.0, 0.5, 0.5),
                    Color::WHITE,
                ));
            }
        }
    }

    // 2000 prims alternating between two textures: maximal draw-call switching.
    paint.push_layer(Some(Rect::new(0.0, 940.0, 1600.0, 50.0)));
    for i in 0..2000 {
        let texture = if i % 2 == 0 { 1 } else { 1024 };
        paint.push(Prim::image(
            Rect::new(((i * 7) % 1590) as f32, 940.0, 6.0, 6.0),
            texture,
            Rect::new(0.0, 0.0, 1.0, 1.0),
            Color::WHITE,
        ));
    }

    let started = std::time::Instant::now();
    let first = renderer
        .render_offscreen(&paint)
        .expect("stress render should succeed");
    let first_elapsed = started.elapsed();
    super::write_png(&artifact_path("stress-10k.png"), 1600, 1000, &first)
        .expect("writing the stress PNG");

    // The renderer must have grown its vertex ring past the initial capacity.
    assert!(
        paint
            .layers
            .iter()
            .map(|layer| layer.prims.len())
            .sum::<usize>()
            > 10_000,
        "stress frame should really be ~10k primitives"
    );
    assert!(
        first_elapsed.as_millis() < 5_000,
        "stress frame took {first_elapsed:?}, suspiciously slow"
    );

    // Atlas growth: same id 1, now 2048x2048. The renderer must replace the
    // image/view/descriptor set rather than assume the old dimensions.
    let atlas_2048 = vec![180u8; 2048 * 2048 * 4];
    paint.update_texture(crate::gfx::paint::TextureUpload::new(
        1, 2048, 2048, atlas_2048,
    ));
    let started = std::time::Instant::now();
    let second = renderer
        .render_offscreen(&paint)
        .expect("stress render after atlas growth should succeed");
    let second_elapsed = started.elapsed();
    super::write_png(
        &artifact_path("stress-10k-atlas2048.png"),
        1600,
        1000,
        &second,
    )
    .expect("writing the grown-atlas stress PNG");

    assert!(
        second_elapsed.as_millis() < 5_000,
        "atlas-growth frame took {second_elapsed:?}, suspiciously slow"
    );
    // The frame still drew (not a clear-only image): some pixel differs from clear.
    let clear = pixel(&second, 1600, 1599, 999);
    let any_drawn = (0..1600u32).step_by(97).any(|x| {
        (0..1000u32)
            .step_by(89)
            .any(|y| pixel(&second, 1600, x, y) != clear)
    });
    assert!(any_drawn, "stress frame appears blank after atlas growth");

    eprintln!(
        "stress: 10k prims first frame {first_elapsed:?}, atlas-growth frame {second_elapsed:?}"
    );
    renderer.shutdown();
}

/// Headless rendering must advance the frame counter and drain retired rings;
/// before this it reused slot 0 forever and never called `drain_retired`, so a
/// long screenshot/test run could accumulate every grown vertex/uniform ring.
#[test]
fn headless_frames_advance_and_drain_retired_rings() {
    use super::RETIRE_FRAMES;
    let Some(mut renderer) = headless_or_skip(64, 64) else {
        return;
    };
    // Force a ring replacement so there is something to retire.
    let grown = renderer.vertex_capacity + 1024;
    renderer
        .grow_vertex_buffers(grown)
        .expect("grow the vertex ring");
    assert!(
        !renderer.retired_vertex_buffers.is_empty(),
        "the old ring is retired, not freed while a frame may use it"
    );

    let paint = PaintList::new(Color::BLACK);
    for _ in 0..=RETIRE_FRAMES {
        renderer.render_offscreen(&paint).expect("headless frame");
    }
    assert!(
        renderer.retired_vertex_buffers.is_empty(),
        "headless frames must drain retired vertex rings"
    );
    assert!(
        renderer.frame_index > RETIRE_FRAMES,
        "headless frames must advance the frame counter (got {})",
        renderer.frame_index
    );
    renderer.shutdown();
}

/// Vulkano retires a swapchain *before* `vkCreateSwapchainKHR` and keeps it
/// retired when creation fails (`vulkano-0.35.2/src/swapchain/mod.rs:452-493`).
/// This pins the recovery policy: the first rebuild may reuse the stored
/// swapchain, but after a failed `recreate` every following frame must create a
/// fresh swapchain with `Swapchain::new` — never call `recreate` on the retired
/// one again — until a replacement has actually been committed. Before the fix a
/// single transient failure retried `recreate` forever and froze the window.
///
/// Pure control-flow test (no Vulkan device needed): the closures stand in for
/// `Swapchain::recreate`/`create_swapchain_surface`.
#[test]
fn failed_swapchain_rebuild_recovers_with_a_fresh_swapchain() {
    use std::cell::Cell;

    use anyhow::anyhow;

    use super::{rebuild_window_swapchain, RebuiltSwapchain, SwapchainRetirement};

    let mut retirement = SwapchainRetirement::default();

    // First rebuild: `recreate` fails (e.g. a transient vkCreateSwapchainKHR
    // failure). The helper must still produce a result via a fresh swapchain.
    let recreate_called = Cell::new(false);
    let fresh_called = Cell::new(false);
    let rebuilt = rebuild_window_swapchain(
        &mut retirement,
        || {
            recreate_called.set(true);
            Err::<&str, _>(anyhow!("simulated vkCreateSwapchainKHR failure"))
        },
        || {
            fresh_called.set(true);
            Ok("fresh")
        },
    )
    .expect("a failed recreate must fall back to fresh creation");
    assert!(recreate_called.get());
    assert!(fresh_called.get());
    assert!(matches!(rebuilt, RebuiltSwapchain::Fresh("fresh")));

    // Next frame, still failing: `recreate` must not be called on the retired
    // swapchain, even though fresh creation fails again.
    let recreate_called = Cell::new(false);
    let result = rebuild_window_swapchain(
        &mut retirement,
        || {
            recreate_called.set(true);
            Ok("recreated")
        },
        || Err::<&str, _>(anyhow!("simulated surface failure")),
    );
    assert!(
        !recreate_called.get(),
        "a retired swapchain must never be passed to recreate() again"
    );
    assert!(result.is_err());

    // The failed fresh attempt must leave the next attempt on the fresh path.
    let recreate_called = Cell::new(false);
    let rebuilt = rebuild_window_swapchain(
        &mut retirement,
        || {
            recreate_called.set(true);
            Ok("recreated")
        },
        || Ok("fresh"),
    )
    .expect("the fresh retry succeeds");
    assert!(!recreate_called.get());
    assert!(matches!(rebuilt, RebuiltSwapchain::Fresh("fresh")));

    // Once the replacement has been committed to the target, its swapchain has
    // never been used for a recreation, so `recreate` is allowed again.
    retirement.commit();
    let rebuilt = rebuild_window_swapchain(&mut retirement, || Ok("recreated"), || Ok("fresh"))
        .expect("recreate succeeds on the committed swapchain");
    assert!(matches!(rebuilt, RebuiltSwapchain::Recreated("recreated")));
    retirement.commit();
}

/// resizing a headless renderer must reallocate the offscreen image, not just
/// update the bookkeeping size: before the fix the readback buffer used the new
/// dimensions while the render target was still the old one.
#[test]
fn headless_resize_reallocates_the_offscreen_target() {
    let Some(mut renderer) = headless_or_skip(64, 32) else {
        return;
    };
    let clear = Color::hex(0x33_66_99);
    let paint = PaintList::new(clear);
    // Warm up at the initial size, then resize to a different pixel count.
    renderer.render_offscreen(&paint).expect("initial render");
    renderer.resize((80, 60));
    assert_eq!(renderer.window_size(), (80, 60));
    let rgba = renderer
        .render_offscreen(&paint)
        .expect("render after resize");
    assert_eq!(rgba.len(), 80 * 60 * 4, "readback uses the new size");
    // A corner pixel only exists in the reallocated image; the old target
    // would have left it at the zero-initialised buffer value.
    assert_near(
        pixel(&rgba, 80, 79, 59),
        [0x33, 0x66, 0x99, 0xFF],
        3,
        "clear colour after resize",
    );
    renderer.shutdown();
}

/// A surface may clamp the swapchain extent below the window size (fixed
/// `currentExtent`, or a driver maximum). The renderer must compare the window
/// against the size it last *requested*, not against the clamped extent, or
/// every frame looks like a resize and the swapchain is recreated continuously.
#[test]
fn a_clamped_swapchain_extent_does_not_keep_requesting_a_resize() {
    let mut requested = [1160, 740];
    // A clamped extent (e.g. 1000x600) never enters this decision at all.
    for _ in 0..16 {
        assert!(
            !window_size_changed(&mut requested, [1160, 740]),
            "an unchanged window must not request a rebuild, however the surface clamped it"
        );
    }
    assert_eq!(requested, [1160, 740]);

    // A real window resize does, once; the next frame at the same size is quiet.
    assert!(window_size_changed(&mut requested, [1200, 800]));
    assert!(!window_size_changed(&mut requested, [1200, 800]));
    assert_eq!(requested, [1200, 800]);

    // Zero-sized window events are normalized to one pixel, never stored as 0.
    assert!(window_size_changed(&mut requested, [0, 0]));
    assert_eq!(requested, [1, 1], "a zero size must not become the extent");
}
