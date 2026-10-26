# Immediate-mode UI API (`crates/rift-desktop/src/ui`, `crates/rift-desktop/src/text`)

Status: **v1.3, implemented and compiling** — all signatures below exist on disk in
`crates/rift-desktop/src/ui/`. This is the reference for screen code (`crates/rift-desktop/src/screens/**`),
`crates/rift-desktop/src/app.rs` and `crates/rift-desktop/src/main.rs`. If you need a widget that is missing,
message `imgui` rather than hand-rolling it in screens.

Recent additions (v1.3): real clipboard support — `ui.text_input` Ctrl+A/C/X/V now uses the native OS clipboard via `arboard` by default (created lazily; injected `MemoryClipboard`/custom `Clipboard` backends still work, and headless tests disable it). v1.2: texture uploads queued **between** frames are preserved
(they are drained by `end_frame`, not cleared by `begin_frame`) so cover art
decoded outside the frame renders live; `end_frame` without `begin_frame` is
tolerated; an open popup that no screen draws is auto-closed; DPI scale path
documented and verified; fuzz/robustness + HiDPI test suites; widget gallery
renderer for visual QA. v1.1 added `scroll_area_dynamic` /
`scroll_area_stable_dynamic`, menu keyboard navigation + first-item focus + focus
return, `menu_item_submenu`, `focus_ring_inset`, disabled widget variants
(`button_enabled`, `icon_button_enabled`, `switch_enabled`, `slider_enabled`),
single-slot toasts with explicit durations/`toast_sticky`, slider geometry
matched to ui-spec §6.3, equalizer timing matched to the CSS animation.

The framework is owned by `imgui`: `crates/rift-desktop/src/ui/**`, `crates/rift-desktop/src/text/**`,
`crates/rift-desktop/assets/fonts/**`. Screens own `crates/rift-desktop/src/screens/**` only. Painting contract
(`crates/rift-desktop/src/gfx/paint.rs`) is Lead-owned and frozen.

```text
ui/mod.rs        frame lifecycle, ids, interaction, clip stack, deferred layers,
                 primitives, text/icon drawing
ui/widgets.rs    styled widgets (buttons, switch, slider, badge, cover, …)
ui/text_input.rs single-line text editing (caret, selection, clipboard)
ui/scroll.rs     scroll containers + scrollbar
ui/popup.rs      menus/popups, tooltips, toasts
ui/layout.rs     Rect helpers, Vec2/Size, Row/Column cursors, Padding, Align
ui/id.rs         Id hashing
ui/icons.rs      Icon enum + icon fonts
ui/input.rs      InputState / Key / Modifiers (winit-agnostic, frozen)
ui/paint.rs      re-export of the frozen paint contract + texture-id rules
text/            fontdue fonts, shaping, measuring, wrapping, ellipsis, atlas
```

No external GUI crate is used. The renderer needs to know nothing about fonts.

## Frame flow (owned by `app.rs`)

```rust
let mut ui = ui::Ui::new();                 // loads embedded fonts once
// every frame:
ui.begin_frame(input_state, viewport_rect); // viewport = logical client rect
//   screens draw here (screens::root(ui, state, actions))
let paint: PaintList = ui.end_frame();
window.set_cursor(ui.cursor_shape());       // map CursorShape -> winit CursorIcon
renderer.frame(&paint)?;
```

When `end_frame` is called, the UI has already:

* flattened all primitives into one painter-ordered `PaintList` (one `Layer` per
  contiguous clip run; scroll containers and popups handled automatically),
* uploaded/replaced/dropped textures (`created`/`updated`/`dropped`),
* drawn deferred overlays (open popup, tooltips, toasts) **on top** of all
  screen content,
* committed interaction state for the next frame.

`end_frame` returns a fresh `PaintList`; **screens never append to a PaintList
themselves** — call `ui.*` and the UI buffers everything internally. The only
texture entry points screens need are
`ui.upload_texture(id, w, h, rgba8)`, `ui.update_texture(...)`, `ui.drop_texture(id)`
(cover art; ids from `state` start at `FIRST_IMAGE_TEXTURE_ID = 1024`).

## Coordinates, DPI, colors

* Logical pixels, origin top-left, `+y` down; design for the 1160×740 reference
  and take the real viewport from `ui.viewport()`.
* Colors are sRGB straight-alpha `ui::Color` (`Color::hex(0xRRGGBB)`,
  `Color::rgba(r,g,b,a)`, `.with_alpha(a)`, `.fade(f)`).

### DPI / scale factor (HiDPI)

The UI is resolution-independent and needs **no API call from screens**:

* `main.rs` puts the window's DPI scale into `InputState::scale_factor` (and
  keeps `viewport` in logical px = physical/scale); `Ui::begin_frame` feeds it
  to `Fonts::set_scale`.
* Text and icons are rasterised at **physical** pixel size
  (`size_px × scale_factor`, rounded to whole physical px) into the glyph atlas;
  the emitted quads/positions are **logical** px (`bitmap_px / scale_factor`).
  The renderer multiplies geometry by the same scale, so each glyph pixel maps
  1:1 to a device pixel at 100/125/150/200% scaling — crisp text, same layout.
* A scale change invalidates the metrics/slot caches and rebuilds the atlas (one
  full `updated` texture upload with the same texture id 1); the atlas starts at
  1024² and can grow to 2048², which is enough for 4 Inter weights + both icon
  fonts at any single scale. There is exactly one atlas variant at a time (the
  current scale), so memory stays bounded after monitor changes.
* Layout/measure results are scale-independent within rounding: logical text
  widths agree across 0.75–3.0 scale factors (tested).
* Screenshot mode at a simulated scale: set `input.scale_factor = scale` and
  `input.viewport = logical size`, then call
  `gfx::screenshot(logical_w*scale, logical_h*scale, scale, &paint, path)`.
  (Not wired as a CLI flag yet; Lead owns `main.rs`.)
* Evidence: `crates/rift-desktop/target/ui-gallery-150.png` is a 1740×1110 render from a
  1160×740 logical frame at scale 1.5 (glyphs rasterised at 1.5×, quads in
  logical px); `ui-gallery-125.png` is the 1.25 case.

## InputState (`ui/input.rs`, frozen)

`main.rs` fills this from winit events; the UI only sees this struct. All
per-frame edges are true for exactly one frame.

```rust
pub struct InputState {
    pub viewport: Rect,          // logical client rect
    pub scale_factor: f32,       // physical px per logical px
    pub time: f64,               // monotonic seconds
    pub delta_time: f32,         // seconds since last frame
    pub mouse_pos: (f32, f32),   // logical px, may be outside the window
    pub mouse_in_window: bool,
    pub mouse_left_down: bool, pub mouse_right_down: bool, pub mouse_middle_down: bool,
    pub mouse_left_pressed: bool, pub mouse_right_pressed: bool, pub mouse_middle_pressed: bool,
    pub mouse_left_released: bool, /* … */,
    pub scroll: (f32, f32),      // winit convention: +y = wheel up; px not lines
    pub modifiers: Modifiers { shift, ctrl, alt, super_key },
    pub keys: Vec<KeyInput>,     // this frame's transitions, in order
    pub text: String,            // typed text / IME this frame
}
pub struct KeyInput { pub key: Key, pub pressed: bool, pub repeat: bool, pub modifiers: Modifiers }
pub enum Key { Space, Enter, Escape, Backspace, Delete, Tab, Left, Right, Up, Down,
              Home, End, PageUp, PageDown, Insert, Char(char) }
```

Query helpers: `input.key_clicked(Key)`, `key_pressed` (includes OS repeat),
`key_down`, `command()` (Ctrl/Cmd), `InputState::end_frame()` (main calls after
the UI consumed the frame).

**winit mapping notes for `main.rs`:**

* `CursorMoved` → `mouse_pos = position.to_logical(scale_factor)`;
  `mouse_in_window` from `CursorEntered`/`CursorLeft` and window focus.
* `MouseInput { state, button }` → `*_down`, and set the matching
  `*_pressed`/`*_released` edge on the *current* frame only (they are cleared by
  `InputState::end_frame()`).
* `MouseWheel { delta }` → `scroll`: `LineDelta(x,y)` → `(x*40.0, y*40.0)`
  (a notch scrolls ~40 logical px); `PixelDelta(p)` → `(p.x, p.y)`.
* `KeyboardInput { event, .. }` → push
  `KeyInput { key, pressed: state.is_pressed(), repeat: event.repeat, modifiers }`
  when `!event.repeat || state.is_pressed()`. Text-producing keys also set
  `Key::Char(c)` so text inputs can consume them.
* `ModifiersChanged(mods)` → `modifiers` (winit `ModifiersState`).
* `Ime::Commit(text)` / `ReceivedCharacter(ch)` → append to `text`.
* `Resized`/`ScaleFactorChanged` → update viewport and scale factor before the
  next `begin_frame`.

## Ids (`ui::Id`)

Stable across frames, deterministic within one process; state (hover, focus,
scroll offsets, text cursors, menus) is keyed by them.

```rust
Id::from_str("titlebar/search")     // stable, readable
parent.with(&playlist.name)         // per-data child id (any Hash key)
parent.child(index as u64)          // per-row child id
```

Convention: screens prefix ids with the screen (`"home/…"`, `"queue/…"`) and
derive row ids from the row's stable data (track id / playlist id / index).
Widgets called as `ui.button(rect, label, kind)` build their own id from the
label + quantised rect, so a list row's buttons are already distinct (rows sit at
different y). Use the `*_with_id` variants when the label itself can change
without the widget being a different control. The stateful widgets
(`play_button`, `switch`) already derive an id that does **not** depend on their
on/off state, so toggling them keeps keyboard focus.

## Layout

`ui::Rect { x, y, w, h }` — the frozen paint type, plus these helpers
(`ui/layout.rs`):

```rust
rect.right(); rect.bottom(); rect.left(); rect.top(); rect.pos(); rect.size();
rect.center(); rect.center_v(); rect.contains(x, y); rect.intersect(other);
rect.inset(px); rect.inset_xy(dx, dy); rect.inset_pad(Padding);
rect.split_top(h) -> (top, rest);      rect.split_bottom(h) -> (rest, bottom);
rect.split_left(w) -> (left, rest);    rect.split_right(w) -> (rest, right);
rect.split_cols::<N>(gap) -> [Rect; N]; rect.split_rows::<N>(gap) -> [Rect; N];
rect.expand(px); rect.offset(dx, dy); rect.with_x/with_y/with_w/with_h(..);
rect.align(Size, Align, VAlign) -> Rect; rect.clamped();
```

Cursors for stacked layouts:

```rust
let mut col = ui::Column::new(rect, gap);
let header = col.fixed(34.0);
let body = col.remaining();          // all that is left
let mid = col.percent(0.5);          // percentage of the whole rect
col.skip(8.0);

let mut row = ui::Row::new(rect, gap);
let icon = row.fixed(16.0);
let title = row.remaining();         // gap is inserted between cells
```

`Padding { left, right, top, bottom }` with `Padding::all/symmetric/xy/vertical/horizontal`.
`Align { Left, Center, Right }`, `VAlign { Top, Center, Bottom }`,
`Vec2 { x, y }` (+ - * operators), `Size { w, h }` (`ui::Size` is re-exported
from `text`).

## Text

Everything is shaped with the embedded **Inter** fonts (Regular 400, Medium 500,
SemiBold 600, Bold 700) and the embedded **Material Icons** faces.

Font loading is **panic-free and degrades gracefully**. The font faces are
optional: if an embedded face ever fails to parse (only possible with corrupt or
missing baked-in assets — the committed assets are valid, so normal builds and
tests are unaffected and render exactly as before), `Fonts::new` logs a
`tracing::warn!` once instead of aborting, and:

* requests for the failed weight fall back to the first face that did load, so
  text still renders (in another weight) rather than disappearing;
* line metrics fall back to a synthetic ascent/descent derived from the pixel
  size when no face can provide them;
* character advances fall back to a fraction of the pixel size, so measuring and
  wrapping stay finite even with no glyphs;
* glyph rasterisation simply yields no quads (text renders as empty space) if no
  face at all loaded.

That fallback path is only reachable on a broken asset payload — never in normal
builds, tests or the shipped binary — and it replaces what used to be a startup
panic.

```rust
use rift_desktop::text::{TextStyle, Weight};

let style = TextStyle::new(14.0, Weight::Regular, theme.text);
let bold  = style.with_weight(Weight::Bold);
let dim   = TextStyle::new(13.0, Weight::Regular, theme.text_dim);
let tiny  = TextStyle::new(10.0, Weight::Bold, theme.text_dim).with_letter_spacing(1.5);
let fixed_pitch = style.with_line_height(20.0); // 0.0 = font default (~1.21em)
// fields: size_px: f32, weight: Weight, color: Color, line_height: f32, letter_spacing: f32
```

Drawing (all use the style's color; glyphs are `Prim::Image` from the atlas):

```rust
ui.text(rect, text, &style);           // single line, left, v-centred, ELLIPSIZED to rect.w
ui.text_centered(rect, text, &style);  // single line, centred, ellipsized
ui.text_right(rect, text, &style);     // single line, right, ellipsized
ui.text_aligned(rect, text, &style, Align::Center); // explicit h-align, v-centred
ui.text_wrapped(rect, text, &style);   // word-wrapped to rect.w, top-aligned
ui.text_wrapped_aligned(rect, text, &style, Align::Left, VAlign::Top);
ui.text_raw(rect, text, &style);       // no wrap/ellipsis, top-left, honours '\n'
ui.label_value(rect, "Size", "12 px", &label_style, &value_style);
ui.section_title(rect, "Recently played"); // 17px bold
ui.eyebrow(rect, "PLAYLISTS");             // 10px bold, 1.5px tracking
ui.empty_state(rect, "No results");        // 13px dim, centred
```

Measuring / shaping (never rasterizes, cheap enough per frame):

```rust
ui.measure_text(text, &style) -> Size;       // single line block (height = line box)
ui.measure_text_wrapped(text, &style, w) -> Size;
ui.text_width(text, &style) -> f32;          // advance width of one line
ui.line_height(&style) -> f32;
ui.layout_text(text, &style, Some(max_w)) -> TextShape; // custom placement
ui.glyph_run(origin: Vec2, &shape, color);   // draw a pre-shaped run
```

`TextShape { glyphs: Vec<ShapedGlyph>, size: Size, lines: u32, baseline: f32 }`.
`ShapedGlyph` quads are already positioned relative to the block's top-left.

Ellipsizing a `String` owned by state:
`ui.fonts_mut().ellipsize(text, &style, max_w) -> Cow<str>`,
`wrap_text(...) -> String`, `ellipsize_owned(&mut String, ...)`.

## Icons

```rust
use rift_desktop::ui::Icon;
ui.icon(rect, Icon::Play, color, 16.0);      // ink centred in rect; 16.0 matches the
                                            // old SVGs' 16px viewBox sizing
ui.icon_outlined(rect, Icon::Heart, color, 16.0); // outlined Material Icons font
ui.icon_font(rect, icon, color, size, IconFont::Filled);
ui.icon_label(rect, Icon::Play, 16.0, "Play", &style, 7.0) -> f32; // icon+text centred
```

`Icon` covers all required names: Play, Pause, SkipNext, SkipPrev, Shuffle,
Repeat, RepeatOne, PlayCircle, VolumeUp/Down/Off/Mute, Heart, HeartFilled, Like,
Home, MusicNote, Album, LibraryMusic, Person, Queue, PlaylistAdd, PlaylistPlay,
List, Radio, Search, Settings, MoreVert, MoreHoriz, DragHandle, ChevronDown/
Right/Left, ArrowBack/Forward, ExpandMore/Less, Add, AddCircle, Delete, Close,
Check, CheckCircle, Cancel, Edit, Share, Save, ContentCopy, Download, Upload,
FileOpen, FolderOpen, OpenInNew, Refresh, Sync, Minimize, Maximize, Fullscreen,
FullscreenExit, CloudOff, Offline, Info, Error, Warning, Timer, WifiOff, Mic,
Headset, Lyrics, Speed, Equalizer, GraphicEq, AudioTrack, Image, Photo,
FilterList, Sort, AccountCircle, Logout, Login, Notifications, Done, DoneAll,
RadioButton*, CheckBox*, plus `Icon::ALL` / `Icon::all_codepoints()`.

Icon sizes from the CSS: `.ibtn svg` 16, sidebar 16, window buttons 14, card
kebab 15, transport 20, play button 20, `.play-all svg` 16, volume 17,
queue caret 14, titlebar logo mark 22.

**No rotation support** in the paint contract (glyphs are axis-aligned quads):
for the queue collapse caret use `Icon::ChevronRight` when collapsed and
`Icon::ExpandMore` (`ChevronDown`) when expanded — same visual as the CSS
`rotate(-90deg)`.

## Painting primitives

```rust
ui.fill(rect, color);                                 // solid
ui.rect(rect, color);                                 // alias
ui.rounded_rect(rect, color, radius);
ui.bordered_rect(rect, fill, radius, border_width, border_color); // bg + 1px border
ui.stroke(rect, radius, width, color);                // border ring only
ui.line(from: Vec2, to: Vec2, width, color);          // axis-aligned
ui.hline(x, y, w, thickness, color); ui.vline(x, y, h, thickness, color);
ui.circle(center: Vec2, radius, color);
ui.shadow(rect, radius, strength);                    // soft drop shadow (stacked AA rects)
ui.image(rect, texture_id, tint, radius);             // cover art / textures
ui.image_uv(rect, texture_id, uv: Rect /*0..1*/, tint, radius);
ui.panel(rect, radius);                               // bg_raised + 1px border
ui.elevated_card(rect, radius);                       // #1c1c20 + border + shadow
ui.divider(rect);                                     // 1px border-colored line
```

Clipping: `ui.push_clip(rect)` / `ui.pop_clip()` / `ui.with_clip(rect, |ui| …)`
(nested clips intersect; primitives carry their clip into a paint layer).
Deferred layer: `ui.deferred(|ui| …)` paints after all normal content — used for
overlays/tooltips; popups/toasts/tooltips already do this for you.

## Widgets

All interactive widgets return `Response`:

```rust
pub struct Response {
    pub id: Id, pub rect: Rect,
    pub hovered: bool, pub pressed: bool, pub clicked: bool,
    pub secondary_clicked: bool, pub double_clicked: bool,
    pub active: bool, pub dragged: bool, pub drag_delta: Vec2, pub drag_started: bool,
    pub focused: bool, pub focus_visible: bool, pub hover_t: f32, // 0..1 animated
}
```

Buttons (kind derives colors from the theme; `Response` semantics as above;
keyboard Enter/Space while focused also produces `clicked`):

```rust
use rift_desktop::ui::ButtonKind;
ui.button(rect, "Play", ButtonKind::Primary) -> Response;
ui.button(rect, "Download", ButtonKind::Secondary);
ui.button(rect, "Show all", ButtonKind::Ghost);
ui.button(rect, "Songs", ButtonKind::Tab { active: true });
ui.button(rect, "Play", ButtonKind::PlayAll);      // accent pill + play icon
ui.button(rect, "Download", ButtonKind::HeadButton); // raised pill, 1px border
ui.button(rect, "Delete", ButtonKind::Danger);
ui.button_with_id(id, rect, label, kind);          // stable id when label flips
ui.button_styled(rect, label, &ButtonStyle);       // custom style
ui.head_button(rect, Icon::Download, "Download", ButtonKind::HeadButton);
ui.play_button(rect, playing: bool) -> Response;   // white circle transport button

ui.icon_button(rect, Icon::MoreVert) -> Response;  // .ibtn: 28x28, hover bg
ui.icon_button_tinted(rect, Icon::HeartFilled, theme.accent);
ui.icon_button_styled(rect, icon, active, &IconButtonStyle); // custom size/tint/bg
```

Toggles, sliders, progress:

```rust
let r = ui.switch(rect /*44x24*/, &mut on);        // r.changed when toggled
let r = ui.checkbox(rect, &mut checked, "Label");  // 16px box + label
let r = ui.slider(rect /*h=4 track row*/, &mut value, 0.0, 1.0); // r.changed
let r = ui.slider_styled(id, rect, &mut v, min, max, &SliderStyle::from_theme(&theme).with_step(1.0));
ui.progress(rect, fraction, theme.accent);          // thin rounded progress bar
ui.spinner(rect);                                    // indeterminate loader
ui.equalizer(rect, playing);                         // 3 animated accent bars
```

Badges and images:

```rust
use rift_desktop::ui::BadgeKind;
ui.badge(rect, "Downloaded", BadgeKind::Success);   // Neutral/Success/Warning/Danger/Accent
ui.cover(rect, texture: Option<TextureId>, radius); // cover art or music-note fallback
ui.cover_round(rect, texture);                      // circular (artist avatar)
```

Disabled widgets (`ui-spec` §2.4 `.btn-primary:disabled`, `.head-btn:disabled`):
no hover/click/focus, dimmed automatically:

```rust
ui.button_enabled(rect, "Download", ButtonKind::HeadButton, enabled) -> Response;
ui.icon_button_enabled(rect, Icon::SkipNext, enabled) -> Response;
ui.switch_enabled(rect, &mut on, enabled) -> CheckboxResponse;
ui.slider_enabled(rect, &mut v, min, max, enabled) -> SliderResponse;
```

Slider geometry matches `ui-spec` §6.3: 4px `#efefef` track, accent played fill,
16×16 accent thumb that stays flush inside the track at 0% and 100%
(centre travels `rect.w − 2·radius`). `ui.play_button(rect, playing)` is the
white circular transport button; `ui.equalizer(rect, playing)` matches the CSS
`eq-bounce` animation; `ui.spinner(rect)` is the indeterminate loader.

## Text input

```rust
use rift_desktop::ui::TextInputStyle;

let style = TextInputStyle::default_theme(&theme); // raised bg, 1px border, 6px radius, 13px
// style.search(theme)  -> transparent, no border (wrap in `.searchbox` yourself)
// style.rename(theme)  -> 17px bold, accent border (`.rename-input`)

let r = ui.text_input(rect, Id::from_str("playlist/rename"), &mut draft, "Playlist name", &style);
// r.changed    -> text mutated this frame (mark dirty)
// r.submitted  -> Enter pressed (commit)
// r.cancelled  -> Escape pressed (cancel; field also blurred)
// r.lost_focus -> focus left the field this frame (commit-or-cancel policy is yours)
// r.response   -> normal Response
```

Behaviour: click to place the caret, drag to select, double-click selects the
word; Backspace/Delete/Left/Right/Home/End, Shift-selection, Ctrl+A select-all,
Ctrl+C/X/V, caret blink; long text scrolls horizontally. Text lives in `State`;
the widget only edits it.

Retained editing state is **panic-free**. Per-field state (caret, selection
anchor, horizontal scroll, blink phase) lives in the UI context
(`Ui::text_edits`), keyed by the widget's `Id`. Each `text_input` call takes the
entry out of the map (`remove(&id).unwrap_or_default()` — a missing entry simply
means "first frame for this field"), drives the entire widget from that owned
local state, and inserts it back before returning; that function has no early
returns, which is the invariant the module documents. There are no
`unwrap()`/`expect()` calls in the production text-field path, and the
`clippy::unwrap_used`/`expect_used`/`panic` lints are clean for it.

Clipboard: by default the UI uses the **real OS clipboard** (`arboard`), created
lazily on the first copy/paste and degrading to a no-op if the session has no
clipboard service — nothing to wire in `main.rs`. Two other backends are
available through the `Clipboard` trait:

```rust
ui.set_clipboard(Box::new(MemoryClipboard::new())); // deterministic/in-process
ui.clear_clipboard();                                // back to native
ui.set_native_clipboard_enabled(false);              // headless: no OS access
let available = ui.native_clipboard_available();
```

`MemoryClipboard` is cloneable so a test can read back what the widget copied.
`cargo test` uses disabled/memory backends only, so tests never touch the OS
clipboard.

## Scrolling

```rust
ui::scroll_area(id, rect, content_height, |ui, content| { … }) -> (R, ScrollResponse);
ui::scroll_area_stable(id, rect, content_height, |ui, content| { … }); // always reserves 10px
// View code that measures its own height while drawing: closure returns
// (result, content_height); the previous frame's height drives the offset math.
ui::scroll_area_dynamic(id, rect, |ui, content| { …; (out, content_h) }) -> (R, ScrollResponse);
ui::scroll_area_stable_dynamic(id, rect, |ui, content| { …; (out, content_h) });
```

* `content` is the layout rect already offset by the scroll position
  (`x = rect.x`, `y = rect.y - offset`, `w = rect.w - gutter`); lay rows out from
  `content.y` and pass the total content height up front (`rows * row_h`).
* The container clips internally, draws the 10px scrollbar, handles wheel
  scrolling (only when hovered; negative `scroll.y` = wheel down = offset grows)
  and scrollbar drag (grab cursor; drag jumps).
* `ScrollResponse { hovered, scrolled, offset, max_offset, content_rect, viewport }`.
* Offsets are retained per id. Switching views:
  `ui.reset_scroll(id)`; restore: `ui.set_scroll_offset(id, offset)` /
  `ui.scroll_offset(id)` / `ui.scroll_content_height(id)`.
* With `scroll_area_stable`, the gutter is always reserved (the old `.main` uses
  `scrollbar-gutter: stable` so card grids don't reflow).

## Popups / context menus

The UI owns the open-popup state; a menu is opened/toggled from the anchor
button and drawn while open:

```rust
let menu_id = Id::from_str("track-row/menu").with(&track.id);
let kebab = ui.icon_button(kebab_rect, Icon::MoreVert);
if kebab.clicked { ui.toggle_popup(menu_id); }

ui.popup(menu_id, kebab_rect, Size::new(180.0, 3.0 * MENU_ITEM_HEIGHT),
         PopupAlign::Right, |ui, content| {
    let mut col = Column::new(content, 0.0);
    if ui.menu_item(col.fixed(MENU_ITEM_HEIGHT), "Play next", Some(Icon::PlaylistAdd), None, false).clicked {
        ui.close_popup();
        actions.push(Action::PlayNext(track.id.clone()));
    }
    if ui.menu_item(col.fixed(MENU_ITEM_HEIGHT), "Delete", Some(Icon::Delete), None, true).clicked {
        ui.close_popup();
        actions.push(Action::RemoveTrack(track.id.clone()));
    }
});
// ui.popup returns Option<R>: None while closed, Some(closure result) while open.
```

* `ui.popup(id, anchor, content_size, align, f)` draws a raised 8px-rounded menu
  with a CSS-like shadow (`0 10px 30px rgba(0,0,0,.55)`), auto-places it below
  the anchor (+4px, flips above if needed, clamps to the viewport), registers it
  as an input blocker for next frame, and closes on click-outside, Escape or Tab.
* **Keyboard**: opening a menu focuses its first row; Up/Down/Home/End move the
  focus ring; Enter/Space activates the focused row; Escape closes and returns
  focus to the widget that had it when the menu opened (the trigger).
* `content_size` is the inside size: add `10px` padding yourself by using
  `content.w` from the closure (or pass `Size::new(170.0, n * MENU_ITEM_HEIGHT)`).
* `PopupAlign::{Left, Right}` (`.menu` vs `.menu-right`).
* Items: `ui.menu_item(rect, label, Option<Icon>, Option<&str> shortcut, danger)`,
  `ui.menu_item_with_id`, `ui.menu_item_indented` (.menu-sub-option),
  `ui.menu_item_submenu(rect, label, icon, expanded)` (draws the ▸/▾ caret for
  inline-expanded submenus), `ui.menu_separator(rect)`. Constants:
  `MENU_ITEM_HEIGHT = 33.0` (8px padding + 17px line box),
  `MENU_MIN_CONTENT_WIDTH = 180.0` (menu min-width 190 − 10 padding).
* State helpers: `ui.popup_open() -> Option<Id>`, `ui.is_popup_open(id)`,
  `ui.open_popup(id)`, `ui.close_popup()`, `ui.toggle_popup(id)`.

Tooltips and toasts:

```rust
ui.tooltip(id, anchor_rect, hovered: bool, "Tooltip text"); // 450ms delay, auto-placed
ui.tooltip_for(&response, "Tooltip text");                   // convenience wrapper
ui.toast_error(message);          // `.toast` palette (ui-spec §6.5), 5s, replaces old
ui.toast_info(message);           // same single-slot component (CSS only has one palette)
ui.toast_with_duration(message, ToastKind::Error, 6.0); // e.g. command errors
ui.toast_sticky("Importing playlist…");                 // stays until replaced
```

One toast is visible at a time; a newer toast replaces the previous one and
restarts its timer (ui-spec §6.5).

Overlays.rs note: the UI already paints toasts/tooltips/popups in the deferred
top layer. If the app wants a *custom* card (e.g. the update banner), draw it
inside `ui.deferred(|ui| …)` and it will land on top.

## Focus, keyboard, cursor

* Tab / Shift+Tab move keyboard focus in paint order (previous frame's order);
  clicking a focusable widget focuses it (without the visible ring), clicking
  empty space blurs.
* Focused buttons activate on Enter/Space (`Response.clicked`).
* `ui.wants_keyboard()` is true while a text field or focused widget owns the
  keyboard — **screens must skip global shortcuts when it is true**
  (Space play/pause, Ctrl+arrows etc.).
* `ui.keyboard_mode()`, `ui.request_focus(id)`, `ui.clear_focus()`,
  `ui.is_focused(id)`, `ui.focused_id()`, `ui.focus_ring(rect, radius)` (offset
  +2, for buttons) and `ui.focus_ring_inset(rect, radius)` (offset −2, for
  rows/cards/menu options per ui-spec §2.4).
* Cursor: after `end_frame`, `ui.cursor_shape() -> CursorShape`; widgets set it
  automatically (pointer over clickables, text I-beam, grab/grabbing on
  scrollbars/sliders). `main.rs` maps it to winit:

| `CursorShape` | winit `CursorIcon` |
|---|---|
| `Default` | `Default` |
| `Pointer` | `Pointer` |
| `Text` | `Text` |
| `Grab` | `Grab` |
| `Grabbing` | `Grabbing` |
| `ResizeH` | `EwResize` |
| `ResizeV` | `NsResize` |
| `NotAllowed` | `NotAllowed` |
| `Crosshair` | `Crosshair` |

## Theme and design tokens

`ui.theme() -> &Theme` (`crates/rift-desktop/src/theme.rs`, Lead-owned) is the single source for
colors/metrics: `bg #0e0e10`, `bg_raised #151518`, `bg_hover #1d1d21`,
`border #232328`, `text #ececef`, `text_dim #8b8b93`, `accent #8b3df0`,
`accent_dim #6d28d9`, `danger #e5484d`, `focus #c4a0ff`, plus font sizes,
`titlebar_height 46`, `sidebar_width 216`, `player_height 92`, radii, spacing,
card gap/min-width, track row height (=54 incl. padding), etc.
`ui.theme_mut()` exists for previews/settings; don't mutate it from screens.

Widget-internal colors not in `Theme` (scrollbar `#2a2a30`/`#3a3a42`,
success/warning/danger status tones, slider track `#d7d7dc`) are exported from
`ui` as constants (`ui::SUCCESS`, `ui::DANGER_TEXT`, `ui::SLIDER_TRACK`, …) if a
screen needs to match them.

## Screen conventions

* Root composition stays in `app.rs`: it calls `ui.begin_frame`, then
  `screens::root(ui, state, actions)`, then `ui.end_frame`.
* Screen modules expose immediate-mode functions that take `&mut Ui`, `&State`
  (or `&mut State` for local UI state), `&mut Vec<Action>` and the content
  `Rect` they own, and return nothing. They must not paint the window
  background or the chrome.
* Ids: prefix with the screen and derive from stable data (see §Ids). Never use
  pointer values or raw frame counters.
* Do not call backend/controller code; push `Action` values and let `app.rs`
  dispatch them after `end_frame`.
* Don't allocate per-frame `Vec`s for glyphs/layout; the UI reuses its buffers,
  and `measure_text`/`text_width` are cached (metrics only, no rasterization).
* Prefer the widget methods over hand-painted approximations — the visual match
  (radii, borders, hover/pressed/focus states, disabled dimming via
  `Color::fade`) lives in them.

## Q&A for screens

1. **Button signature**: `ui.button(rect, label, ButtonKind) -> Response`; the
   id is auto-derived (label + quantised rect). Use `ui.button_with_id` when the
   label/position changes per state. Custom look: `ui.button_styled(rect, label, &ButtonStyle)`
   or mutate a `ButtonStyle::for_kind(kind, &theme)`.
2. **Toggles**: `ui.switch(rect, &mut bool) -> CheckboxResponse { response, changed }`;
   checkbox: `ui.checkbox(rect, &mut bool, "label")`.
3. **Slider**: `ui.slider(rect, &mut f32, min, max)`; crossfade
   (1..12 step 1): `ui.slider_styled(id, rect, &mut v, 1.0, 12.0, &SliderStyle::from_theme(&theme).with_step(1.0))`.
4. **Text input**: bind `&mut String`; use the `TextInputResponse` fields
   (`changed/submitted/cancelled/lost_focus`) for dirty/commit/cancel policies;
   placeholder is a parameter.
5. **Scrolling**: `ui.scroll_area(id, main_rect, content_height, |ui, content| …)`
   (auto gutter) or `scroll_area_stable` (always 10px, matches `.main`). The
   10px gutter is handled inside: content rect width is already reduced and the
   scrollbar is drawn for you.
6. **Popup from a kebab**: toggle on `kebab.clicked` then call `ui.popup(...)` as
   in the example above; open state lives in `Ui` (`ui.popup_open()` etc.).
7. **Toasts**: `ui.toast_error/info(...)` is drawn by the UI's deferred layer
   automatically; drawing your own card in `ui.deferred(...)` is compatible and
   will always be on top. `state.toast` can drive the message, but the UI owns
   display + lifetime.
8. **Tooltip**: `ui.tooltip(id, anchor_rect, hovered, "text")` or
   `ui.tooltip_for(&response, "text")`.
9. **Wheel**: scroll containers consume it automatically; there is no manual
   wheel helper. If a screen needs manual offsets, use `ui.scroll_offset(id)` /
   `set_scroll_offset`; don't read raw `input.scroll` unless you're implementing a
   custom scrollable (then set `scroll` consumption is per-frame anyway).
10. **TextStyle**: `TextStyle::new(size_px, Weight, color)` with
    `with_weight/with_color/with_line_height/with_letter_spacing`; fields
    `{ size_px, weight, color, line_height, letter_spacing }`. `Weight`:
    `Regular/Medium/SemiBold/Bold`. Letter spacing is supported (queue header,
    page-kind, logo wordmark).
11. **Spinner**: `ui.spinner(rect)` (8 pulsing dots, no hand-rolled arcs).
12. **Theme metrics**: use `Theme::default()` values; `Theme` is Lead-owned and
    validated against `styles.css` by screens-chrome's `ui-spec.md`. Message
    Lead if a token value looks wrong.

## Testing

### Test inventory (all pure-logic unless marked GPU)

`cargo test -p rift-desktop` runs them with the rest of the crate. Modules:

| Module | What it covers |
|---|---|
| `text::tests` (29) | embedded font loading; measure vs layout agreement; measure never rasterises; glyph-cache hits; atlas allocation/`TextureUpdate`; line boxes + CSS half-leading baseline; wrap word/gap correctness (incl. regression: no double-counted line prefix, long-word hard break, leading whitespace); ellipsis fit/borrow; randomized Unicode wrap/measure/ellipsize fuzz (250 cases) asserting char boundaries, no lost glyphs, line widths fit; HiDPI bitmap-vs-logical-quad math + cache invalidation; scale-stable measuring |
| `ui::tests` (33) | paint primitives/layers, clip grouping + nesting, deferred-after-base ordering; interaction (press/release/drag/outside-click/double-click), Tab/Shift+Tab focus, click-focus, keyboard activation; text prims/ellipsis/atlas id; icons; scroll wheel/clamp/retain/stable gutter; popup open/close/Escape/blockers; toasts/tooltips; text input typing/backspace/enter/escape; theme styles; widget auto-id stability; texture lifecycle incl. **inter-frame uploads**, in-frame exactly-once, dropped ids, `end_frame` without `begin_frame` |
| `ui::tests_polish` (13) | disabled button/switch, dynamic scroll containers, menu focus + arrow keys + Escape focus-return + click activation, slider thumb-inside-track geometry + colors, single-slot toasts/sticky, focus-ring inset, submenu caret |
| `ui::fuzz` (17) | deterministic xorshift fuzz over 3×220 random frames (random clicks/drags/wheel/Tab/typing/Escape/resizes/scale changes): paint rects finite/non-negative, clips valid, texture payload sizes correct, focus resolves, scroll clamped, text stays valid UTF-8, app texture/atlas created exactly once; popup cannot leak to a screen that doesn't draw it; deferred content paints once per frame; text-input edge cases (bounds, UTF-8 arrows, shift selection, Ctrl+A/C/X/V with injected clipboard and no-clipboard no-ops, Enter submit, Escape cancel/blur, long-text editing) |
| `ui::gallery` (1 pure + 2 `#[ignore]` GPU, serialized by an internal mutex) |
| `ui::clipboard` (4) | in-memory round-trip/sharing; real OS clipboard round-trip when a service is available (skips on headless CI, restores the previous contents); full `Ui` Ctrl+V/paste + Ctrl+A/C/copy path against the real OS clipboard (Windows, serialized); unavailable-backend no-op path | gallery paint test; renders `crates/rift-desktop/target/ui-gallery{,-125,-150}.png` through the real renderer and a dedicated `ui-image-check.png` test that **asserts rendered pixels** of a 64×64 RGBA gradient + red tint through `ui.image` (texture upload path) |

GPU tests are `#[ignore = "requires a Vulkan device"]`; run them manually
(they take an internal mutex because the offscreen renderer owns process-wide GPU
state):

```powershell
cargo test -p rift-desktop --lib ui::gallery -- --ignored --nocapture
```

Run the crate tests for the current counts:

```powershell
cargo test -p rift-desktop --lib
cargo test -p rift-desktop --test ui_flow
```

GPU gallery tests are ignored by default; run them manually with
`cargo test -p rift-desktop --lib ui::gallery -- --ignored --nocapture`.
