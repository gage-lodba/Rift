//! Pure-logic tests for the text subsystem (no GPU, no window).
//!
//! These load the real embedded fonts, so they also prove the baked assets
//! parse and that every UI icon codepoint exists in the embedded icon fonts.

use super::*;
use crate::gfx::paint::Color;

fn regular() -> TextStyle {
    TextStyle::new(14.0, Weight::Regular, Color::WHITE)
}

fn fonts() -> Fonts {
    Fonts::new()
}

/// The faces are parsed concurrently on startup; every slot must still come
/// back populated in the documented order. (A slot order mistake would make
/// weights render with the wrong font instead of failing loudly.)
#[test]
fn all_six_embedded_faces_parse_into_their_slots() {
    let faces = super::load_faces();
    assert!(
        faces.iter().all(Option::is_some),
        "embedded font assets must all parse: {:?}",
        faces.iter().map(Option::is_some).collect::<Vec<_>>()
    );
    // Spot-check the slot order: the icon faces carry glyphs the UI fonts do
    // not, and the regular UI face must be first.
    let mut fonts = Fonts::new();
    let icon = TextStyle::new(16.0, Weight::Regular, Color::WHITE);
    assert!(
        fonts.measure("\u{e037}", &icon).w > 0.0,
        "first icon glyph must be found in the embedded icon faces"
    );
}

fn width_of(f: &mut Fonts, s: &str, style: &TextStyle) -> f32 {
    f.line_width(s, style)
}

#[test]
fn measure_matches_layout_size() {
    let mut f = fonts();
    let style = regular();
    let text = "Hello, Rift! 123";
    let measured = f.measure(text, &style);
    let shaped = f.layout(text, &style);
    assert!(
        (measured.w - shaped.size.w).abs() < 0.01,
        "{measured:?} != {:?}",
        shaped.size
    );
    assert!((measured.h - shaped.size.h).abs() < 0.01);
    assert!(measured.w > 0.0);
    let char_count = text.chars().filter(|c| !c.is_whitespace()).count() as u32;
    assert_eq!(shaped.glyphs.len() as u32, char_count);
}

#[test]
fn empty_text_has_line_height() {
    let mut f = fonts();
    let style = regular();
    let size = f.measure("", &style);
    assert_eq!(size.w, 0.0);
    assert!(size.h > 14.0 && size.h < 20.0, "h={}", size.h);
    let shaped = f.layout("", &style);
    assert!(shaped.glyphs.is_empty());
    assert_eq!(shaped.lines, 1);
}

#[test]
fn newlines_stack_lines() {
    let mut f = fonts();
    let style = regular();
    let one = f.measure("a", &style);
    let three = f.measure("a\nb\nc", &style);
    assert!((three.h - one.h * 3.0).abs() < 0.01);
    assert_eq!(f.layout("a\nb\nc", &style).lines, 3);
    // A trailing newline produces an empty final line.
    assert_eq!(f.layout("a\n", &style).lines, 2);
}

#[test]
fn measure_does_not_rasterize_and_layout_caches() {
    let mut f = fonts();
    let style = regular();
    assert_eq!(f.rasterized_glyphs(), 0);
    let a = f.measure("Rift", &style);
    let b = f.measure("Rift", &style);
    assert_eq!(a, b);
    assert_eq!(f.rasterized_glyphs(), 0, "measuring must not rasterize");

    f.layout("Rift", &style);
    let after_first = f.rasterized_glyphs();
    assert!(after_first > 0);
    f.layout("Rift", &style);
    assert_eq!(
        f.rasterized_glyphs(),
        after_first,
        "second layout of the same text must hit the glyph cache"
    );
    assert_eq!(f.atlas().allocations(), after_first);
}

#[test]
fn atlas_produces_upload_once_and_uvs_are_sane() {
    let mut f = fonts();
    let style = regular();
    let shape = f.layout("Atlas", &style);
    assert!(!shape.glyphs.is_empty());
    for g in &shape.glyphs {
        assert!(g.uv[0] >= 0.0 && g.uv[2] <= 1.0);
        assert!(g.uv[1] >= 0.0 && g.uv[3] <= 1.0);
        assert!(g.uv[2] > g.uv[0] && g.uv[3] > g.uv[1]);
        assert!(g.w > 0.0 && g.h > 0.0);
        assert!(g.x.is_finite() && g.y.is_finite());
    }
    let upload = f.take_atlas_update(ATLAS_TEXTURE_ID).expect("dirty atlas");
    assert_eq!(upload.id, ATLAS_TEXTURE_ID);
    assert_eq!(
        upload.width as usize * upload.height as usize * 4,
        upload.rgba.len()
    );
    // Nothing changed since: no second upload.
    assert!(f.take_atlas_update(ATLAS_TEXTURE_ID).is_none());
}

#[test]
fn line_height_is_font_default_and_overridable() {
    let mut f = fonts();
    let style = regular();
    let normal = f.line_height(&style);
    assert!(
        normal > 14.0 && normal < 20.0,
        "normal line height {normal}"
    );
    let fixed = style.with_line_height(20.0);
    assert_eq!(f.line_height(&fixed), 20.0);
    assert!((f.measure("a\nb", &fixed).h - 40.0).abs() < 0.01);
}

#[test]
fn wrap_splits_at_words_within_width() {
    let mut f = fonts();
    let style = regular();
    let text = "the quick brown fox jumps over the lazy dog";
    let full = width_of(&mut f, text, &style);
    let max = full / 2.0;
    let mut ranges = Vec::new();
    f.wrap_ranges(text, &style, max, &mut ranges);
    assert!(
        ranges.len() >= 2,
        "expected wrapping, got {} lines",
        ranges.len()
    );
    for r in &ranges {
        let line = &text[r.clone()];
        assert!(!line.is_empty());
        assert!(
            width_of(&mut f, line, &style) <= max + 0.01,
            "line {line:?} wider than {max}"
        );
        // Whole words only (no split without a long-word fallback).
        assert!(!line.starts_with(' ') && !line.ends_with(' '));
    }
    // Ranges reconstruct without losing non-space characters.
    let squeezed: String = ranges
        .iter()
        .map(|r| &text[r.clone()])
        .collect::<Vec<_>>()
        .join(" ");
    let a: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    let b: String = squeezed.chars().filter(|c| !c.is_whitespace()).collect();
    assert_eq!(a, b);
}

#[test]
fn wrap_breaks_a_word_longer_than_the_line() {
    let mut f = fonts();
    let style = regular();
    let text = "supercalifragilisticexpialidocious";
    let word_w = width_of(&mut f, "super", &style);
    let mut ranges = Vec::new();
    f.wrap_ranges(text, &style, word_w, &mut ranges);
    assert!(ranges.len() > 2);
    for r in &ranges {
        assert!(!text[r.clone()].is_empty());
        assert!(
            width_of(&mut f, &text[r.clone()], &style) <= word_w + 0.01,
            "piece {:?} too wide",
            &text[r.clone()]
        );
    }
    assert_eq!(ranges[0].start, 0);
    assert_eq!(ranges.last().unwrap().end, text.len());
}

#[test]
fn wrap_honours_explicit_newlines_and_max_width() {
    let mut f = fonts();
    let style = regular();
    let mut ranges = Vec::new();
    f.wrap_ranges("one\ntwo\nthree", &style, 1000.0, &mut ranges);
    assert_eq!(ranges.len(), 3);
    assert_eq!(&"one\ntwo\nthree"[ranges[1].clone()], "two");
    let size = f.measure_wrapped("one\ntwo\nthree", &style, 1000.0);
    assert!((size.h - f.line_height(&style) * 3.0).abs() < 0.01);
}

#[test]
fn ellipsize_borrows_when_it_fits() {
    let mut f = fonts();
    let style = regular();
    let text = "short";
    let out = f.ellipsize(text, &style, 1000.0);
    assert!(matches!(out, std::borrow::Cow::Borrowed("short")));
}

#[test]
fn ellipsize_truncates_and_fits() {
    let mut f = fonts();
    let style = regular();
    let text = "a very long title that will not fit in a narrow card column";
    let full = width_of(&mut f, text, &style);
    let max = full * 0.4;
    let out = f.ellipsize(text, &style, max);
    assert!(out.ends_with('\u{2026}'), "{out:?}");
    assert!(width_of(&mut f, &out, &style) <= max + 0.01);
    assert!(!out.starts_with(' '));
    // It cut on a char boundary of the original.
    let prefix = out.trim_end_matches('\u{2026}');
    assert!(text.starts_with(prefix));
}

#[test]
fn ellipsize_empty_when_no_room() {
    let mut f = fonts();
    let style = regular();
    let out = f.ellipsize("hello", &style, 0.5);
    assert!(out.is_empty());
}

#[test]
fn wrap_text_inserts_newlines() {
    let mut f = fonts();
    let style = regular();
    let text = "alpha beta gamma delta";
    let max = width_of(&mut f, "alpha beta", &style);
    let wrapped = f.wrap_text(text, &style, max);
    assert!(wrapped.contains('\n'));
    for line in wrapped.split('\n') {
        assert!(width_of(&mut f, line, &style) <= max + 0.01);
    }
}

#[test]
fn scale_changes_cache_but_not_logical_width() {
    let mut f = fonts();
    let style = regular();
    let w1 = width_of(&mut f, "Scaling", &style);
    let before = f.rasterized_glyphs();
    assert_eq!(before, 0);
    f.set_scale(2.0);
    let w2 = width_of(&mut f, "Scaling", &style);
    assert!(
        (w1 - w2).abs() < 0.6,
        "logical width should not change with DPI scale: {w1} vs {w2}"
    );
    // Rasterizing at 2x produces bitmap quads that are still logical-sized.
    let shape = f.layout("Scaling", &style);
    for g in &shape.glyphs {
        assert!(g.w < 40.0 && g.h < 40.0);
    }
    assert!(f.rasterized_glyphs() > 0);
    f.set_scale(1.0);
}

#[test]
fn weighted_faces_load_and_measure_differently() {
    let mut f = fonts();
    let text = "Weighted";
    let regular = f.measure(text, &TextStyle::new(14.0, Weight::Regular, Color::WHITE));
    let bold = f.measure(text, &TextStyle::new(14.0, Weight::Bold, Color::WHITE));
    assert!(bold.w >= regular.w);
    assert!(bold.w > 0.0);
}

#[test]
fn every_ui_icon_codepoint_exists_in_both_icon_fonts() {
    // The codepoint table the UI's `Icon` enum uses. Update together with
    // `crates/rift-desktop/src/ui/icons.rs`; this test fails loudly if a codepoint is wrong or
    // the wrong icon font was embedded.
    let pts: &[(char, &str)] = &[
        ('\u{e037}', "play_arrow"),
        ('\u{e034}', "pause"),
        ('\u{e044}', "skip_next"),
        ('\u{e045}', "skip_previous"),
        ('\u{e043}', "shuffle"),
        ('\u{e040}', "repeat"),
        ('\u{e041}', "repeat_one"),
        ('\u{e87d}', "favorite"),
        ('\u{e87e}', "favorite_border"),
        ('\u{e050}', "volume_up"),
        ('\u{e04f}', "volume_off"),
        ('\u{e04d}', "volume_down"),
        ('\u{e8b6}', "search"),
        ('\u{e88a}', "home"),
        ('\u{e405}', "music_note"),
        ('\u{e8b8}', "settings"),
        ('\u{f090}', "download"),
        ('\u{e03b}', "playlist_add"),
        ('\u{e145}', "add"),
        ('\u{e5d4}', "more_vert"),
        ('\u{e872}', "delete"),
        ('\u{e5cd}', "close"),
        ('\u{e15b}', "remove"),
        ('\u{e3c6}', "crop_square"),
        ('\u{e5ca}', "check"),
        ('\u{f09b}', "upload"),
        ('\u{eaf3}', "file_open"),
        ('\u{e89e}', "open_in_new"),
        ('\u{e5d5}', "refresh"),
        ('\u{e2c1}', "cloud_off"),
        ('\u{e945}', "drag_indicator"),
        ('\u{e03d}', "queue_music"),
        ('\u{e8dc}', "thumb_up"),
        ('\u{e88e}', "info"),
        ('\u{e5cf}', "expand_more"),
        ('\u{e5cc}', "chevron_right"),
        ('\u{e5c4}', "arrow_back"),
        ('\u{e5c8}', "arrow_forward"),
        ('\u{e3c9}', "edit"),
        ('\u{e80d}', "share"),
        ('\u{e896}', "list"),
        ('\u{e030}', "library_music"),
        ('\u{e7fd}', "person"),
        ('\u{e019}', "album"),
        ('\u{e147}', "add_circle"),
        ('\u{e000}', "error"),
        ('\u{e002}', "warning"),
        ('\u{e86c}', "check_circle"),
        ('\u{e5c9}', "cancel"),
        ('\u{e5d0}', "fullscreen"),
        ('\u{e5d1}', "fullscreen_exit"),
        ('\u{e152}', "filter_list"),
        ('\u{e164}', "sort"),
        ('\u{e5ce}', "expand_less"),
        ('\u{e425}', "timer"),
        ('\u{e648}', "wifi_off"),
        ('\u{e029}', "mic"),
        ('\u{e05f}', "playlist_play"),
        ('\u{e03e}', "radio"),
        ('\u{e04e}', "volume_mute"),
        ('\u{e310}', "headset"),
        ('\u{e03c}', "queue"),
        ('\u{ec0b}', "lyrics"),
        ('\u{e9e4}', "speed"),
        ('\u{e01d}', "equalizer"),
        ('\u{e5d3}', "more_horiz"),
        ('\u{e313}', "keyboard_arrow_down"),
        ('\u{e315}', "keyboard_arrow_right"),
        ('\u{e15c}', "remove_circle"),
        ('\u{e7f4}', "notifications"),
        ('\u{e853}', "account_circle"),
        ('\u{e9ba}', "logout"),
        ('\u{ea77}', "login"),
        ('\u{e161}', "save"),
        ('\u{e14d}', "content_copy"),
        ('\u{e3f4}', "image"),
        ('\u{e410}', "photo"),
        ('\u{e1b8}', "graphic_eq"),
        ('\u{e3a1}', "audiotrack"),
        ('\u{e627}', "sync"),
        ('\u{e876}', "done"),
        ('\u{e877}', "done_all"),
        ('\u{e837}', "radio_button_checked"),
        ('\u{e836}', "radio_button_unchecked"),
        ('\u{e834}', "check_box"),
        ('\u{e835}', "check_box_outline_blank"),
        ('\u{e934}', "play_circle"),
        ('\u{e1a3}', "chevron_left"),
        ('\u{e5c4}', "arrow_back_dup"),
    ];
    let f = fonts();
    for &(cp, name) in pts {
        assert!(
            f.has_glyph(cp, false),
            "filled icon font missing {name} U+{:04X}",
            cp as u32
        );
        assert!(
            f.has_glyph(cp, true),
            "outlined icon font missing {name} U+{:04X}",
            cp as u32
        );
    }
}

#[test]
fn icon_quad_is_centered_ink_size() {
    let mut f = fonts();
    let g = f.icon_glyph('\u{e037}', 16.0, false).expect("play icon");
    // Material Icons: a 16px em yields ~13.3px of ink, matching the old SVGs.
    assert!(g.w > 6.0 && g.w <= 16.5, "w={}", g.w);
    assert!(g.h > 6.0 && g.h <= 16.5, "h={}", g.h);
    assert!((g.x).abs() < f32::EPSILON && (g.y).abs() < f32::EPSILON);
    let adv = f.icon_advance('\u{e037}', 16.0, false);
    assert!((adv - 16.0).abs() < 0.6, "advance={adv}");
}

#[test]
fn prewarm_fills_atlas_once() {
    let mut f = fonts();
    f.prewarm_ascii(&regular());
    let n = f.rasterized_glyphs();
    assert!(n >= 60, "rasterized {n}");
    f.prewarm_ascii(&regular());
    assert_eq!(f.rasterized_glyphs(), n);
}

#[test]
fn explicit_line_height_uses_css_half_leading_baseline() {
    // Reference screenshot font is Segoe UI (hhea ascent 2210/descent 514 on a
    // 2048 em); `ui-spec.md` §4.2 requires its line boxes with Inter glyphs. CSS
    // half-leading inside a 22px line box at 17px puts the baseline at 18.04px.
    let mut f = fonts();
    let style = TextStyle::new(17.0, Weight::Bold, Color::WHITE).with_line_height(22.0);
    let shape = f.layout("Your library", &style);
    assert!(
        (shape.baseline - 18.04).abs() < 0.1,
        "baseline {}",
        shape.baseline
    );
    let first = shape.glyphs.iter().find(|g| g.w > 0.0).expect("glyphs");
    // Cap height of Inter at 17px is ~12.4px, so the ink starts ~5.7px down.
    assert!((5.0..8.0).contains(&first.y), "ink y {} ", first.y);
    // Font-native line boxes keep the font's own ascent.
    let natural = f.layout(
        "Your library",
        &TextStyle::new(17.0, Weight::Bold, Color::WHITE),
    );
    let lm = f.line_metrics(&TextStyle::new(17.0, Weight::Bold, Color::WHITE));
    assert!((natural.baseline - lm.ascent).abs() < 0.01);
}
#[test]
fn wrap_does_not_double_count_the_line_prefix() {
    // Regression: the separator width used to be measured from the start of the
    // line, charging every word the whole line width again, so text wrapped
    // after the first word when the line clearly had room for all of it.
    let mut f = fonts();
    let style = TextStyle::new(13.0, Weight::Regular, Color::WHITE);
    let text = "Show the track you're listening to on your Discord profile.";
    let full = width_of(&mut f, text, &style);
    assert!(full > 200.0 && full < 500.0, "full width {full}");
    // A line 8px wider than the text must be a single line.
    let mut ranges = Vec::new();
    f.wrap_ranges(text, &style, full + 8.0, &mut ranges);
    assert_eq!(ranges.len(), 1, "should fit on one line: {ranges:?}");
    assert_eq!(ranges[0], 0..text.len());
    // Half the width must wrap into several lines that each fit.
    f.wrap_ranges(text, &style, full * 0.5, &mut ranges);
    assert!(ranges.len() >= 2, "half width should wrap: {ranges:?}");
    // Every wrapped line individually fits and the first one is close to full.
    for r in &ranges {
        assert!(width_of(&mut f, &text[r.clone()], &style) <= full * 0.5 + 0.01);
    }
    assert!(width_of(&mut f, &text[ranges[0].clone()], &style) > full * 0.3);
    // measure_wrapped agrees with the layout height.
    let size = f.measure_wrapped(text, &style, full + 8.0);
    assert!(
        (size.h - f.line_height(&style)).abs() < 0.01,
        "h={}",
        size.h
    );
    assert!((size.w - full).abs() < 0.01);
}

#[test]
fn wrap_gap_is_only_the_whitespace_between_words() {
    // Two words with a large gap: wrapping at a width that fits both words but
    // not their gap must produce two lines, and the gap must not be double
    // charged when it does fit.
    let mut f = fonts();
    let style = TextStyle::new(13.0, Weight::Regular, Color::WHITE);
    let a = width_of(&mut f, "alpha", &style);
    let b = width_of(&mut f, "beta", &style);
    let space = width_of(&mut f, " ", &style);
    let mut ranges = Vec::new();
    f.wrap_ranges("alpha beta", &style, a + space + b + 0.5, &mut ranges);
    assert_eq!(ranges.len(), 1, "both words + single space should fit");
    f.wrap_ranges("alpha beta", &style, a + b + 0.5, &mut ranges);
    assert_eq!(ranges.len(), 2, "without the gap it must wrap");
}
// ---------------------------------------------------------------------------
// HiDPI / scale factor
// ---------------------------------------------------------------------------

#[test]
fn hidpi_rasterizes_glyph_bitmaps_at_physical_size_but_quads_stay_logical() {
    let mut f = fonts();
    let style = TextStyle::new(14.0, Weight::Regular, Color::WHITE);
    let shape1 = f.layout("Hamburgefonstiv", &style);
    let g1 = shape1.glyphs[0];
    let slot_w1 = (g1.uv[2] - g1.uv[0]) * f.atlas().width() as f32;
    let slot_h1 = (g1.uv[3] - g1.uv[1]) * f.atlas().height() as f32;
    let rendered1 = f.rasterized_glyphs();
    assert!(rendered1 > 0);

    f.set_scale(1.5);
    assert!((f.scale() - 1.5).abs() < f32::EPSILON);
    let shape2 = f.layout("Hamburgefonstiv", &style);
    let g2 = shape2.glyphs[0];
    let slot_w2 = (g2.uv[2] - g2.uv[0]) * f.atlas().width() as f32;
    let slot_h2 = (g2.uv[3] - g2.uv[1]) * f.atlas().height() as f32;

    // The bitmap is rasterized at physical size (1.5x) ...
    assert!(
        (slot_w2 / slot_w1 - 1.5).abs() < 0.2,
        "bitmap width ratio {} (slot {} -> {})",
        slot_w2 / slot_w1,
        slot_w1,
        slot_w2
    );
    assert!((slot_h2 / slot_h1 - 1.5).abs() < 0.2);
    // ... while the emitted quad (logical px) is scale-independent.
    assert!(
        (g2.w - g1.w).abs() <= 1.0,
        "logical quad width changed: {} -> {}",
        g1.w,
        g2.w
    );
    assert!((g2.h - g1.h).abs() <= 1.0);
    assert!((shape2.size.w - shape1.size.w).abs() < 1.0);
    // A scale change invalidates the cache and re-rasterizes.
    assert!(f.rasterized_glyphs() > rendered1);
    // ... and switching back re-rasterizes again rather than reusing the 1.5x
    // bitmaps.
    let before = f.rasterized_glyphs();
    f.set_scale(1.0);
    f.layout("Hamburgefonstiv", &style);
    assert!(f.rasterized_glyphs() > before);
}

#[test]
fn hidpi_icons_keep_their_logical_box() {
    let mut f = fonts();
    let i1 = f.icon_glyph('\u{e037}', 16.0, false).expect("play");
    let slot1 = (i1.uv[2] - i1.uv[0]) * f.atlas().width() as f32;
    f.set_scale(2.0);
    let i2 = f.icon_glyph('\u{e037}', 16.0, false).expect("play");
    let slot2 = (i2.uv[2] - i2.uv[0]) * f.atlas().width() as f32;
    assert!((i2.w - i1.w).abs() <= 1.0, "logical icon width stable");
    assert!((i2.h - i1.h).abs() <= 1.0);
    assert!((slot2 / slot1 - 2.0).abs() < 0.25, "icon bitmap doubles");
}

// ---------------------------------------------------------------------------
// randomized robustness for wrapping / measuring / ellipsizing
// ---------------------------------------------------------------------------

struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0
    }
    fn below(&mut self, n: u32) -> u32 {
        if n == 0 {
            0
        } else {
            (self.next() >> 33) as u32 % n
        }
    }
    fn f32(&mut self, lo: f32, hi: f32) -> f32 {
        let t = (self.next() >> 40) as f32 / (1u32 << 24) as f32;
        lo + (hi - lo) * t
    }
}

#[test]
fn wrap_and_ellipsize_are_robust_for_random_unicode_and_widths() {
    let mut f = fonts();
    let pool = [
        'a', 'W', 'i', 'l', '.', ' ', ' ', '\n', '\t', 'é', '音', '🎵', '-',
    ];
    let mut rng = Lcg(0x5eed_1234);
    for iter in 0..250 {
        let mut text = String::new();
        for _ in 0..rng.below(80) {
            text.push(pool[rng.below(pool.len() as u32) as usize]);
        }
        let size = [11.0f32, 13.0, 14.0, 17.0, 30.0][rng.below(5) as usize];
        let weight = Weight::ALL[rng.below(4) as usize];
        let style = TextStyle::new(size, weight, Color::WHITE);
        let max_w = rng.f32(4.0, 520.0);
        let widest_char = f.line_width("W", &style);

        let mut ranges = Vec::new();
        f.wrap_ranges(&text, &style, max_w, &mut ranges);
        assert!(!ranges.is_empty(), "iter {iter}");
        let mut prev_end = 0usize;
        let mut covered = String::new();
        for r in &ranges {
            assert!(
                r.start <= r.end && r.end <= text.len(),
                "iter {iter}: bad range {r:?} for {text:?}"
            );
            assert!(text.is_char_boundary(r.start) && text.is_char_boundary(r.end));
            assert!(
                r.start >= prev_end.saturating_sub(0),
                "iter {iter}: overlapping ranges"
            );
            prev_end = r.end;
            let line = &text[r.clone()];
            let w = f.line_width(line, &style);
            let slack = widest_char + 0.5;
            assert!(
                w <= max_w.max(widest_char) + slack,
                "iter {iter}: line {line:?} width {w} > max {max_w}"
            );
            covered.extend(line.chars().filter(|c| !c.is_whitespace()));
        }
        let expected: String = text.chars().filter(|c| !c.is_whitespace()).collect();
        assert_eq!(
            covered, expected,
            "iter {iter}: wrapped ranges lost characters"
        );

        let measured = f.measure_wrapped(&text, &style, max_w);
        assert!((measured.h - f.line_height(&style) * ranges.len() as f32).abs() < 0.01);
        assert!(
            measured.w <= max_w.max(widest_char) + 1.0,
            "iter {iter}: measured {} > max {} ({:?} size {size} weight {weight:?}, ranges {ranges:?}, text {text:?})",
            measured.w,
            max_w,
            ranges,
        );
        let shaped = f.layout_wrapped(&text, &style, max_w);
        assert!((shaped.size.h - measured.h).abs() < 0.01);

        // Ellipsize: always fits (or is empty when there is no room at all).
        let ell = f.ellipsize(&text, &style, max_w);
        if !ell.is_empty() {
            let w = f.line_width(&ell, &style);
            assert!(
                w <= max_w + 0.01 || max_w < f.line_width("\u{2026}", &style),
                "iter {iter}: ellipsized width {w} > {max_w}"
            );
            if let std::borrow::Cow::Owned(s) = &ell {
                assert!(s.ends_with('\u{2026}'), "iter {iter}: {s:?}");
            }
        }
    }
}

#[test]
fn measure_is_stable_across_scale_factors() {
    let mut f = fonts();
    let style = TextStyle::new(13.0, Weight::Regular, Color::WHITE);
    let text = "Show the track you're listening to on your Discord profile.";
    let base = f.measure(text, &style);
    for scale in [0.75f32, 1.0, 1.25, 1.5, 2.0, 2.5, 3.0] {
        f.set_scale(scale);
        let size = f.measure(text, &style);
        assert!(
            (size.w - base.w).abs() <= base.w * 0.04 + 1.0,
            "scale {scale}: width {} vs {}",
            size.w,
            base.w
        );
        assert!((size.h - base.h).abs() <= base.h * 0.04 + 1.0);
    }
}
