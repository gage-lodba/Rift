//! Hand-written immediate-mode GUI.
//!
//! Everything under `crates/rift-desktop/src/ui/` is owned by the `imgui` teammate. The public
//! shape `app.rs` and `main.rs` depend on:
//!
//! ```ignore
//! let mut ui = Ui::new();
//! // per frame:
//! ui.begin_frame(input, viewport);   // viewport = logical client area
//! // ... screens draw here, laying out from scratch every frame ...
//! let paint: PaintList = ui.end_frame();
//! let cursor = ui.cursor_shape();    // for Window::set_cursor
//! ```
//!
//! Widgets paint and hit-test the same rectangle in the same call (no retained
//! tree). Cross-frame state that widgets need — active widget, keyboard focus,
//! scroll offsets, text cursors, hover animations, open popup, toasts — lives in
//! the [`Ui`] context keyed by stable hashes ([`Id`]). Painting goes into a flat
//! primitive list with a clip stack; popups and tooltips paint into a deferred
//! buffer that is appended after all normal content, so they stay on top no
//! matter where in the frame they were declared.
//!
//! The full widget/API reference for screen authors is
//! `docs/imgui-api.md`.

pub mod clipboard;
#[cfg(test)]
mod fuzz;
#[cfg(test)]
mod gallery;
pub mod icons;
pub mod id;
pub mod input;
pub mod layout;
pub mod paint;
mod popup;
mod scroll;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_polish;
mod text_input;
mod widgets;

pub use clipboard::{Clipboard, MemoryClipboard, NativeClipboard};
pub use icons::{Icon, IconFont};
pub use id::Id;
pub use input::{InputState, Key, KeyInput, Modifiers};
pub use layout::{Align, Column, Padding, Row, Size, VAlign, Vec2};
pub use paint::{
    Color, PaintList, Prim, Rect, TextureId, TextureUpload, ATLAS_TEXTURE_ID,
    FIRST_IMAGE_TEXTURE_ID, NO_TEXTURE, SCROLLBAR_THUMB, SCROLLBAR_THUMB_HOVER, SCROLLBAR_WIDTH,
};
pub use popup::{PopupAlign, ToastKind, MENU_ITEM_HEIGHT, MENU_MIN_CONTENT_WIDTH};
pub use scroll::ScrollResponse;
pub use text_input::{TextInputResponse, TextInputStyle};
pub use widgets::{
    format_duration, format_position, BadgeKind, ButtonKind, ButtonStyle, CheckboxResponse,
    IconButtonStyle, SliderResponse, SliderStyle, DANGER_BG, DANGER_TEXT, SLIDER_TRACK, SUCCESS,
    SUCCESS_BG, WARNING, WARNING_BG,
};

pub use crate::theme::Theme;

use std::borrow::Cow;
use std::collections::HashMap;

use crate::text::{Fonts, TextShape, TextStyle};

// ---------------------------------------------------------------------------
// editor / OS integration hooks
// ---------------------------------------------------------------------------
//
// The clipboard trait and its native/in-memory backends live in
// `ui::clipboard`; `Ui` creates the native backend lazily on first use.

/// Mouse cursor shape requested by the hovered widget this frame.
///
/// Clickable widgets request [`CursorShape::Pointer`] automatically (see
/// [`Ui::interact`]); a widget that wants a different shape calls
/// [`Ui::set_cursor`] after interacting and wins for the frame. Drag affordances
/// only appear while dragging (`Grabbing`), never on plain hover.
///
/// `main.rs` maps this onto `winit::window::CursorIcon` after `end_frame`:
///
/// ```ignore
/// CursorShape::Default   => CursorIcon::Default,
/// CursorShape::Pointer   => CursorIcon::Pointer,
/// CursorShape::Text      => CursorIcon::Text,
/// CursorShape::Grab      => CursorIcon::Grab,
/// CursorShape::Grabbing  => CursorIcon::Grabbing,
/// CursorShape::ResizeH   => CursorIcon::EwResize,
/// CursorShape::ResizeV   => CursorIcon::NsResize,
/// CursorShape::NotAllowed=> CursorIcon::NotAllowed,
/// CursorShape::Crosshair => CursorIcon::Crosshair,
/// ```
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CursorShape {
    #[default]
    Default,
    Pointer,
    Text,
    Grab,
    Grabbing,
    ResizeH,
    ResizeV,
    NotAllowed,
    Crosshair,
}

/// What a rectangle reacts to. Hover is always tracked; `click`/`drag` decide
/// whether presses latch onto it, `focus` whether Tab can reach it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Sense {
    pub click: bool,
    pub drag: bool,
    pub focus: bool,
}

impl Sense {
    pub const NONE: Sense = Sense {
        click: false,
        drag: false,
        focus: false,
    };
    /// Clickable, focusable (buttons, tabs, links, rows).
    pub const CLICK: Sense = Sense {
        click: true,
        drag: false,
        focus: true,
    };
    /// Hover-only (tooltip anchors, decorative hit areas).
    pub const HOVER: Sense = Sense {
        click: false,
        drag: false,
        focus: false,
    };
    /// Draggable, clickable, focusable (sliders, scrollbar thumbs, resizers).
    pub const DRAG: Sense = Sense {
        click: true,
        drag: true,
        focus: true,
    };
    /// Draggable but not click-activatable.
    pub const DRAG_ONLY: Sense = Sense {
        click: false,
        drag: true,
        focus: false,
    };
    /// Focusable but not click-activatable (text inputs handle clicks
    /// themselves because they need click-to-caret behaviour).
    pub const FOCUS: Sense = Sense {
        click: false,
        drag: false,
        focus: true,
    };

    pub const fn new(click: bool, drag: bool, focus: bool) -> Self {
        Self { click, drag, focus }
    }
}

/// The result of one widget's interaction pass.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Response {
    pub id: Id,
    pub rect: Rect,
    pub hovered: bool,
    /// Left button went down on this widget this frame.
    pub pressed: bool,
    /// Left button released on this widget this frame.
    pub clicked: bool,
    pub secondary_clicked: bool,
    pub double_clicked: bool,
    /// Left button is currently held on this widget.
    pub active: bool,
    pub dragged: bool,
    /// Mouse movement since the drag started.
    pub drag_delta: Vec2,
    pub drag_started: bool,
    pub focused: bool,
    /// Focus was reached with the keyboard, so draw the focus ring.
    pub focus_visible: bool,
    /// Animated hover amount 0..=1 (0.15s ease), for color transitions.
    pub hover_t: f32,
}

impl Response {
    pub fn is_hovered(&self) -> bool {
        self.hovered
    }

    /// Hovered or currently pressed — useful for "reveal on hover" rows.
    pub fn hovered_or_active(&self) -> bool {
        self.hovered || self.active
    }
}

// ---------------------------------------------------------------------------
// paint bookkeeping
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq)]
struct PrimWithClip {
    clip: Option<Rect>,
    prim: Prim,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum PaintTarget {
    #[default]
    Base,
    Deferred,
}

#[derive(Clone, Copy, Debug, Default)]
struct AnimState {
    hover: f32,
    seen: bool,
}

/// Reusable text-shaping buffer: keeps the glyph allocation alive between
/// frames instead of reallocating it for every label.
#[derive(Default)]
struct FrameScratch {
    shape: TextShape,
}

// ---------------------------------------------------------------------------
// Ui
// ---------------------------------------------------------------------------

/// The immediate-mode UI context. One instance lives for the whole app.
pub struct Ui {
    theme: Theme,
    fonts: Fonts,
    input: InputState,
    viewport: Rect,
    time: f64,
    frame_started: bool,

    // ---- painting ----
    prims: Vec<PrimWithClip>,
    deferred: Vec<PrimWithClip>,
    paint_target: PaintTarget,
    clip_stack: Vec<Option<Rect>>,
    current_clip: Option<Rect>,

    // ---- texture lifecycle (PaintList created/updated/dropped) ----
    created_textures: Vec<TextureUpload>,
    updated_textures: Vec<TextureUpload>,
    dropped_textures: Vec<TextureId>,
    atlas_created: bool,

    // ---- interaction ----
    active: Option<Id>,
    active_seen: bool,
    focus: Option<Id>,
    focus_visible: bool,
    focus_taken_this_frame: bool,
    /// Focusable ids in paint order this frame; previous frame's copy drives Tab.
    focus_order: Vec<Id>,
    prev_focus_order: Vec<Id>,
    last_click: Option<(Id, f64)>,
    drag_start: Vec2,
    drag_last: Vec2,
    keyboard_used: bool,
    /// Widgets an open popup covered last frame: they must not hover/click.
    blockers: Vec<Rect>,
    next_blockers: Vec<Rect>,
    popup_depth: u32,
    anims: HashMap<Id, AnimState>,
    cursor: CursorShape,
    /// Explicitly injected clipboard (takes precedence over the native one).
    clipboard: Option<Box<dyn Clipboard>>,
    /// Lazily-created OS clipboard (`arboard`); `None` until first used.
    native_clipboard: Option<NativeClipboard>,
    /// Whether the native clipboard may be created on demand (tests disable it).
    native_clipboard_enabled: bool,
    scroll_wheel_used: bool,

    // ---- retained widget memory ----
    scroll: HashMap<Id, scroll::ScrollState>,
    text_edits: HashMap<Id, text_input::TextEditState>,
    tooltips: HashMap<Id, popup::TooltipState>,
    pending_tooltip: Option<popup::PendingTooltip>,
    open_popup: Option<Id>,
    /// Focusable rows of the currently-open popup, in draw order (keyboard nav).
    popup_items: Vec<(Id, Rect)>,
    /// Widget to return keyboard focus to when the popup closes (its trigger).
    popup_return_focus: Option<Id>,
    /// Focus the first popup row on the frame after the popup opens.
    popup_needs_initial_focus: bool,
    /// Set by [`Ui::popup`] when the open popup was actually drawn this frame;
    /// an open popup that nobody draws (e.g. after a view switch) is closed at
    /// the end of the frame so it can never leak across screens.
    popup_drawn: bool,
    toasts: Vec<popup::Toast>,

    // ---- frame scratch (allocation reuse) ----
    scratch: FrameScratch,
    text_input_active: bool,
    wants_keyboard: bool,
}

impl Default for Ui {
    fn default() -> Self {
        Self::new()
    }
}

impl Ui {
    /// Create the context. Loads the embedded fonts (cheap; no GPU involved).
    pub fn new() -> Self {
        Self::new_with_theme(Theme::default())
    }

    pub fn new_with_theme(theme: Theme) -> Self {
        Self::new_with_fonts_and_theme(theme, Fonts::new())
    }

    /// Build the context around font faces parsed by the caller.
    ///
    /// Parsing the six embedded faces is the only expensive part of creating a
    /// [`Ui`], it is CPU-only, and it shares nothing with the renderer — so the
    /// windowed startup path parses them on a worker thread while the main
    /// thread is in Vulkan construction, then hands the result here (see
    /// `main.rs`). Calls [`Fonts::prewarm_icons`] like the other constructors.
    pub fn new_with_fonts(fonts: Fonts) -> Self {
        Self::new_with_fonts_and_theme(Theme::default(), fonts)
    }

    fn new_with_fonts_and_theme(theme: Theme, mut fonts: Fonts) -> Self {
        // Icons are tiny and used on the first frame; warming them avoids a
        // visible first-frame hitch while the atlas is being filled.
        let prewarm_started = std::time::Instant::now();
        fonts.prewarm_icons(&Icon::all_codepoints(), &[14.0, 16.0, 20.0, 24.0]);
        tracing::debug!(
            "startup: icon prewarm {:.1} ms",
            prewarm_started.elapsed().as_secs_f64() * 1000.0
        );
        Self {
            theme,
            fonts,
            input: InputState::default(),
            viewport: Rect::ZERO,
            time: 0.0,
            frame_started: false,
            prims: Vec::with_capacity(512),
            deferred: Vec::with_capacity(64),
            paint_target: PaintTarget::Base,
            clip_stack: Vec::with_capacity(8),
            current_clip: None,
            created_textures: Vec::new(),
            updated_textures: Vec::new(),
            dropped_textures: Vec::new(),
            atlas_created: false,
            active: None,
            active_seen: false,
            focus: None,
            focus_visible: false,
            focus_taken_this_frame: false,
            focus_order: Vec::with_capacity(64),
            prev_focus_order: Vec::with_capacity(64),
            last_click: None,
            drag_start: Vec2::ZERO,
            drag_last: Vec2::ZERO,
            keyboard_used: false,
            blockers: Vec::new(),
            next_blockers: Vec::new(),
            popup_depth: 0,
            anims: HashMap::new(),
            cursor: CursorShape::Default,
            clipboard: None,
            native_clipboard: None,
            native_clipboard_enabled: true,
            scroll_wheel_used: false,
            scroll: HashMap::new(),
            text_edits: HashMap::new(),
            tooltips: HashMap::new(),
            pending_tooltip: None,
            open_popup: None,
            popup_items: Vec::with_capacity(16),
            popup_return_focus: None,
            popup_needs_initial_focus: false,
            popup_drawn: false,
            toasts: Vec::new(),
            scratch: FrameScratch::default(),
            text_input_active: false,
            wants_keyboard: false,
        }
    }

    // ---------------------------------------------------------------- frame

    /// Start a frame. `viewport` is the client area in logical px (the same
    /// number `Renderer::logical_size` reports).
    pub fn begin_frame(&mut self, input: InputState, viewport: Rect) {
        debug_assert!(!self.frame_started, "begin_frame called twice");
        self.viewport = viewport;
        self.time = input.time;
        self.keyboard_used = !input.keys.is_empty() || !input.text.is_empty();
        self.fonts.set_scale(input.scale_factor.max(0.1));
        self.fonts.begin_frame();
        self.input = input;

        // Reset per-frame state, keeping capacities.
        //
        // NOTE: `created_textures` / `updated_textures` / `dropped_textures` are
        // deliberately NOT cleared here. `app.rs` uploads textures between
        // frames (e.g. cover art decoded from the network arrives via
        // `ControllerEvent::Cover` before `begin_frame`), and those uploads must
        // survive into the next `end_frame()`'s PaintList. They are drained
        // exactly once, by `end_frame`.
        self.prims.clear();
        self.deferred.clear();
        self.paint_target = PaintTarget::Base;
        self.clip_stack.clear();
        self.current_clip = None;
        self.active_seen = false;
        if self.input.focus_lost {
            // The window lost focus between frames: the release edge for any
            // held press/drag will never be delivered, so drop the latch.
            // Otherwise `active.is_none()` would swallow the first click after
            // refocusing. `focus_lost` is a one-frame edge (cleared by
            // `InputState::end_frame`), so this cannot fire twice.
            self.active = None;
            self.drag_start = Vec2::ZERO;
            self.drag_last = Vec2::ZERO;
            self.last_click = None;
        }
        self.focus_taken_this_frame = false;
        self.cursor = CursorShape::Default;
        self.scroll_wheel_used = false;
        self.text_input_active = false;
        self.wants_keyboard = false;
        self.next_blockers.clear();
        self.pending_tooltip = None;
        self.popup_drawn = false;

        // Tab moves keyboard focus through last frame's focusable order.
        if self.input.key_clicked(Key::Tab) {
            self.move_focus(self.input.modifiers.shift);
        }

        // Drop memory for widgets that have not been seen for a while.
        if self.anims.len() > 2048 {
            self.anims.retain(|_, a| a.seen);
        }
        for a in self.anims.values_mut() {
            a.seen = false;
        }

        self.frame_started = true;
    }

    /// Finish the frame and produce the renderer's [`PaintList`].
    ///
    /// Deferred content (popups, tooltips, toasts) is appended here, so callers
    /// see a single painter-ordered layer list. After this call,
    /// [`Ui::cursor_shape`] reflects the frame's hovered-widget cursor.
    ///
    /// Calling this without a matching [`Ui::begin_frame`] is tolerated (tests
    /// and shutdown paths do it): it still drains the texture queue so uploads
    /// made between frames are never lost, and returns an otherwise empty list.
    pub fn end_frame(&mut self) -> PaintList {
        if !self.frame_started {
            let mut paint = PaintList::new(self.theme.bg);
            paint.created.append(&mut self.created_textures);
            paint.updated.append(&mut self.updated_textures);
            paint.dropped.append(&mut self.dropped_textures);
            return paint;
        }
        // End-of-frame overlays paint into the deferred buffer.
        popup::draw_pending_tooltip(self);
        popup::draw_toasts(self);

        // An open popup that no screen drew this frame (view switch, data gone)
        // must not linger: close it so it cannot reappear on another screen.
        if self.open_popup.is_some() && !self.popup_drawn {
            popup::close_popup_if_not_drawn(self);
        }

        // ---- bookkeeping ----
        if !self.input.mouse_left_down {
            self.active = None;
        }
        if self.input.mouse_left_pressed && !self.focus_taken_this_frame {
            self.focus = None;
            self.focus_visible = false;
        }
        if let Some(focus) = self.focus {
            if !self.focus_order.contains(&focus) {
                self.focus = None;
            }
        }
        self.active_seen = false;

        // Blockers recorded this frame apply from the next frame on.
        std::mem::swap(&mut self.blockers, &mut self.next_blockers);
        self.next_blockers.clear();

        // Tab order for next frame.
        std::mem::swap(&mut self.focus_order, &mut self.prev_focus_order);
        self.focus_order.clear();

        // ---- flatten paint ----
        let mut paint = PaintList::new(self.theme.bg);
        push_grouped_layers(&mut paint, &self.prims);
        push_grouped_layers(&mut paint, &self.deferred);
        self.prims.clear();
        self.deferred.clear();

        // ---- textures ----
        if let Some(upload) = self.fonts.take_atlas_update(ATLAS_TEXTURE_ID) {
            if self.atlas_created {
                paint.updated.push(upload);
            } else {
                paint.created.push(upload);
                self.atlas_created = true;
            }
        }
        paint.created.append(&mut self.created_textures);
        paint.updated.append(&mut self.updated_textures);
        paint.dropped.append(&mut self.dropped_textures);

        self.frame_started = false;
        self.wants_keyboard = self.focus.is_some() || self.text_input_active;
        paint
    }

    // --------------------------------------------------------------- access

    pub fn theme(&self) -> &Theme {
        &self.theme
    }

    pub fn theme_mut(&mut self) -> &mut Theme {
        &mut self.theme
    }

    /// The viewport for the current frame, in logical px.
    pub fn viewport(&self) -> Rect {
        self.viewport
    }

    pub fn input(&self) -> &InputState {
        &self.input
    }

    /// Monotonic seconds, as of the beginning of the frame.
    pub fn time(&self) -> f64 {
        self.input.time
    }

    /// Seconds since the previous frame (clamped; safe for animation math).
    pub fn delta_time(&self) -> f32 {
        self.input.delta_time.clamp(0.0, 0.1)
    }

    pub fn fonts(&self) -> &Fonts {
        &self.fonts
    }

    pub fn fonts_mut(&mut self) -> &mut Fonts {
        &mut self.fonts
    }

    /// True when the UI is consuming keyboard input (a text field or a focused
    /// widget): screens should skip global keyboard shortcuts this frame.
    pub fn wants_keyboard(&self) -> bool {
        self.wants_keyboard
    }

    /// Whether a text input wants raw text events this frame.
    pub fn text_input_active(&self) -> bool {
        self.text_input_active
    }

    /// Cursor requested so far this frame. [`Ui::interact`] raises it to
    /// [`CursorShape::Pointer`] for any hovered clickable widget; widgets that
    /// want something else override it afterwards. Reset to
    /// [`CursorShape::Default`] at the start of every frame.
    pub fn cursor_shape(&self) -> CursorShape {
        self.cursor
    }

    /// Explicitly request a cursor for the rest of this frame (last writer
    /// wins). Widgets call this after [`Ui::interact`] to override the automatic
    /// pointer: text fields -> [`CursorShape::Text`], resizers ->
    /// [`CursorShape::ResizeH`]/`ResizeV`, a held drag ->
    /// [`CursorShape::Grabbing`], the titlebar drag region ->
    /// [`CursorShape::Default`].
    pub fn set_cursor(&mut self, cursor: CursorShape) {
        self.cursor = cursor;
    }

    /// The explicitly-injected clipboard, if any (native backend excluded).
    pub fn clipboard(&mut self) -> Option<&mut (dyn Clipboard + 'static)> {
        self.clipboard.as_deref_mut()
    }

    /// Inject a custom clipboard backend; it takes precedence over the native
    /// OS clipboard. [`MemoryClipboard`] is a deterministic choice for tests.
    pub fn set_clipboard(&mut self, clipboard: Box<dyn Clipboard>) {
        self.clipboard = Some(clipboard);
    }

    /// Remove an injected clipboard (the native backend is used again, unless
    /// disabled).
    pub fn clear_clipboard(&mut self) {
        self.clipboard = None;
    }

    /// Enable/disable the native OS clipboard (enabled by default). Tests and
    /// headless environments should disable it so copy/paste are deterministic
    /// no-ops; disabling also drops an already-created backend.
    pub fn set_native_clipboard_enabled(&mut self, enabled: bool) {
        self.native_clipboard_enabled = enabled;
        if !enabled {
            self.native_clipboard = None;
        }
    }

    /// Whether a usable OS clipboard is available (creates the backend on first
    /// call). Returns `false` when disabled or when the platform has no
    /// clipboard service (headless CI).
    pub fn native_clipboard_available(&mut self) -> bool {
        self.native_clipboard_backend()
            .map(|c| c.is_available())
            .unwrap_or(false)
    }

    fn native_clipboard_backend(&mut self) -> Option<&mut NativeClipboard> {
        if !self.native_clipboard_enabled {
            return None;
        }
        let backend = self
            .native_clipboard
            .get_or_insert_with(NativeClipboard::new);
        if backend.is_available() {
            Some(backend)
        } else {
            None
        }
    }

    /// Read the clipboard: an injected backend wins, then the native OS
    /// clipboard. `None` when neither is available.
    pub fn clipboard_text(&mut self) -> Option<String> {
        if let Some(clipboard) = self.clipboard.as_mut() {
            return clipboard.get_text();
        }
        self.native_clipboard_backend().and_then(|c| c.get_text())
    }

    /// Write the clipboard: an injected backend wins, then the native OS
    /// clipboard. Silent no-op when neither is available.
    pub fn set_clipboard_text(&mut self, text: &str) {
        if let Some(clipboard) = self.clipboard.as_mut() {
            clipboard.set_text(text);
            return;
        }
        if let Some(clipboard) = self.native_clipboard_backend() {
            clipboard.set_text(text);
        }
    }

    /// Keyboard activation of the focused widget (Enter/Space).
    pub fn keyboard_activated(&self) -> bool {
        self.input.key_clicked(Key::Enter) || self.input.key_clicked(Key::Space)
    }

    // -------------------------------------------------------------- textures

    /// Register a texture created by the app (cover art, …). It is uploaded by
    /// the renderer before this frame is drawn.
    pub fn upload_texture(&mut self, id: TextureId, width: u32, height: u32, rgba: Vec<u8>) {
        self.created_textures
            .push(TextureUpload::new(id, width, height, rgba));
    }

    /// Replace an existing texture's pixels.
    pub fn update_texture(&mut self, id: TextureId, width: u32, height: u32, rgba: Vec<u8>) {
        self.updated_textures
            .push(TextureUpload::new(id, width, height, rgba));
    }

    /// Release a texture the app no longer needs.
    pub fn drop_texture(&mut self, id: TextureId) {
        self.dropped_textures.push(id);
    }

    // ---------------------------------------------------------------- focus

    fn move_focus(&mut self, backwards: bool) {
        if self.prev_focus_order.is_empty() {
            return;
        }
        let len = self.prev_focus_order.len();
        let pos = self
            .focus
            .and_then(|f| self.prev_focus_order.iter().position(|&x| x == f));
        let next = match (pos, backwards) {
            (Some(p), false) => (p + 1) % len,
            (Some(p), true) => (p + len - 1) % len,
            (None, false) => 0,
            (None, true) => len - 1,
        };
        self.focus = Some(self.prev_focus_order[next]);
        self.focus_visible = true;
        self.keyboard_used = true;
    }

    pub fn focused_id(&self) -> Option<Id> {
        self.focus
    }

    pub fn request_focus(&mut self, id: Id) {
        self.focus = Some(id);
        self.focus_visible = true;
    }

    pub fn clear_focus(&mut self) {
        self.focus = None;
        self.focus_visible = false;
    }

    pub fn is_focused(&self, id: Id) -> bool {
        self.focus == Some(id)
    }

    /// Whether the last input was keyboard (for `:focus-visible`-style rings).
    pub fn keyboard_mode(&self) -> bool {
        self.keyboard_used
    }

    /// Draw the 2px focus ring used by `:focus-visible` in the web UI (offset
    /// outside the rect, rounded).
    pub fn focus_ring(&mut self, rect: Rect, radius: f32) {
        let ring = rect.expand(2.0);
        self.stroke(ring, radius + 2.0, 2.0, self.theme.focus);
    }

    /// Focus ring drawn *inside* the rect (the web UI's rows/cards use
    /// `outline-offset: -2px` so the ring cannot bleed into neighbours).
    pub fn focus_ring_inset(&mut self, rect: Rect, radius: f32) {
        let ring = rect.inset(1.0);
        self.stroke(ring, (radius - 1.0).max(0.0), 2.0, self.theme.focus);
    }

    // ------------------------------------------------------------ hit-test

    fn point(&self) -> Vec2 {
        Vec2::from(self.input.mouse_pos)
    }

    fn clip_contains(&self, pos: Vec2) -> bool {
        match self.current_clip {
            Some(c) => c.contains(pos.x, pos.y),
            None => true,
        }
    }

    fn in_blocker(&self, pos: Vec2) -> bool {
        self.popup_depth == 0 && self.blockers.iter().any(|r| r.contains(pos.x, pos.y))
    }

    /// Whether the pointer is over `rect`, respecting the active clip and popup
    /// blocking. Does not consume anything.
    pub fn hovered(&self, rect: Rect) -> bool {
        let pos = self.point();
        self.input.mouse_in_window
            && rect.contains(pos.x, pos.y)
            && self.clip_contains(pos)
            && !self.in_blocker(pos)
    }

    /// Rectangle of the active clip stack, if any.
    pub fn clip_rect(&self) -> Option<Rect> {
        self.current_clip
    }

    // ---------------------------------------------------------- interaction

    /// Core hit-test + press/release/drag state machine. Called by every widget.
    pub fn interact(&mut self, rect: Rect, id: Id, sense: Sense) -> Response {
        if sense.focus && !self.focus_order.contains(&id) {
            self.focus_order.push(id);
        }
        let pos = self.point();
        let hovered = rect.contains(pos.x, pos.y)
            && self.input.mouse_in_window
            && self.clip_contains(pos)
            && !self.in_blocker(pos);

        let mut r = Response {
            id,
            rect,
            hovered,
            ..Response::default()
        };

        // Click-to-focus (mouse focus never shows the keyboard ring).
        if hovered && self.input.mouse_left_pressed && sense.focus && self.active.is_none() {
            self.focus = Some(id);
            self.focus_visible = false;
            self.focus_taken_this_frame = true;
        }

        // Press latches the widget as active.
        if hovered
            && self.input.mouse_left_pressed
            && self.active.is_none()
            && (sense.click || sense.drag)
        {
            self.active = Some(id);
            self.drag_start = pos;
            self.drag_last = pos;
            r.pressed = true;
        }

        if self.active == Some(id) {
            r.active = true;
            self.active_seen = true;
            if sense.drag && self.input.mouse_left_down {
                let d = pos - self.drag_start;
                r.dragged = d.length() > 2.0;
                r.drag_delta = d;
                r.drag_started =
                    d.length() > 2.0 && (self.drag_last - self.drag_start).length() <= 2.0;
                self.drag_last = pos;
            }
        }

        // Release: a click only if released over the widget that captured it.
        if self.input.mouse_left_released && self.active == Some(id) {
            r.clicked = hovered && sense.click;
            self.active = None;
            if r.clicked {
                let now = self.time;
                r.double_clicked = matches!(self.last_click, Some((last_id, t)) if last_id == id && now - t < 0.35);
                self.last_click = Some((id, now));
            }
        }

        // Secondary click: instantaneous, no state.
        if hovered && self.input.mouse_right_pressed && (sense.click || sense.drag) {
            r.secondary_clicked = true;
        }

        // Keyboard activation: focused button responds to Enter/Space as a click.
        if sense.click && self.focus == Some(id) && self.keyboard_activated() {
            r.clicked = true;
        }

        // `active` reports the state *after* the release above, so a click's
        // release frame reports clicked = true, active = false.
        r.active = self.active == Some(id);
        r.focused = self.focus == Some(id);
        r.focus_visible = r.focused && self.focus_visible;
        r.hover_t = self.animate_hover(id, if hovered { 1.0 } else { 0.0 });

        // Clickable widgets get the normal hand automatically. This is written
        // unconditionally so it participates in last-writer-wins ordering:
        // widgets that need a different cursor call `set_cursor` *after* their
        // `interact` (text field -> Text, resizer -> ResizeH, held drag ->
        // Grabbing) and still win. `Sense::HOVER`/`Sense::NONE` regions (hover
        // anchors, disabled widgets) never get the hand.
        if hovered && sense.click {
            self.cursor = CursorShape::Pointer;
        }
        r
    }

    fn animate_hover(&mut self, id: Id, target: f32) -> f32 {
        let dt = self.delta_time();
        let entry = self.anims.entry(id).or_insert(AnimState {
            hover: target,
            seen: true,
        });
        entry.seen = true;
        if (entry.hover - target).abs() > f32::EPSILON {
            // 0.15s ease-out transition, same feel as the CSS rules.
            let step = (dt / 0.15).clamp(0.0, 1.0);
            entry.hover += (target - entry.hover) * step;
            if (entry.hover - target).abs() < 0.01 {
                entry.hover = target;
            }
        }
        entry.hover
    }

    // ------------------------------------------------------------- painting

    fn push_prim(&mut self, prim: Prim) {
        if !self.frame_started {
            return;
        }
        let entry = PrimWithClip {
            clip: self.current_clip,
            prim,
        };
        match self.paint_target {
            PaintTarget::Base => self.prims.push(entry),
            PaintTarget::Deferred => self.deferred.push(entry),
        }
    }

    /// Run `f` with painting redirected to the deferred (always-on-top) buffer.
    pub fn deferred<R>(&mut self, f: impl FnOnce(&mut Ui) -> R) -> R {
        let previous = self.paint_target;
        self.paint_target = PaintTarget::Deferred;
        let out = f(self);
        self.paint_target = previous;
        out
    }

    pub fn is_deferred(&self) -> bool {
        self.paint_target == PaintTarget::Deferred
    }

    /// Push a clip rectangle onto the clip stack. Nested clips intersect.
    pub fn push_clip(&mut self, rect: Rect) {
        self.clip_stack.push(self.current_clip);
        self.current_clip = Some(match self.current_clip {
            Some(current) => current.intersect(rect),
            None => rect.clamped(),
        });
    }

    pub fn pop_clip(&mut self) {
        self.current_clip = self.clip_stack.pop().unwrap_or(None);
    }

    /// Run `f` with a temporary clip rectangle.
    pub fn with_clip<R>(&mut self, rect: Rect, f: impl FnOnce(&mut Ui) -> R) -> R {
        self.push_clip(rect);
        let out = f(self);
        self.pop_clip();
        out
    }

    /// Solid (optionally unrounded) rectangle.
    pub fn fill(&mut self, rect: Rect, color: Color) {
        if rect.is_empty() || color.a == 0 {
            return;
        }
        self.push_prim(Prim::rect(rect, color));
    }

    /// Alias for [`Ui::fill`] that reads better for panels.
    pub fn rect(&mut self, rect: Rect, color: Color) {
        self.fill(rect, color);
    }

    pub fn rounded_rect(&mut self, rect: Rect, color: Color, radius: f32) {
        if rect.is_empty() || color.a == 0 {
            return;
        }
        self.push_prim(Prim::rounded(rect, color, radius.max(0.0)));
    }

    /// Rounded rect with a border ring inside its edge (the web UI's
    /// `background + border` combo, e.g. `.searchbox`, menus, cards).
    pub fn bordered_rect(
        &mut self,
        rect: Rect,
        fill: Color,
        radius: f32,
        border: f32,
        border_color: Color,
    ) {
        if rect.is_empty() {
            return;
        }
        self.push_prim(Prim::bordered(
            rect,
            fill,
            radius.max(0.0),
            border,
            border_color,
        ));
    }

    /// Border ring only (transparent fill).
    pub fn stroke(&mut self, rect: Rect, radius: f32, width: f32, color: Color) {
        if rect.is_empty() || width <= 0.0 || color.a == 0 {
            return;
        }
        self.bordered_rect(rect, Color::TRANSPARENT, radius, width, color);
    }

    /// Axis-aligned line (horizontal or vertical; other angles degrade to their
    /// bounding box — the UI never needs diagonals).
    pub fn line(&mut self, from: Vec2, to: Vec2, width: f32, color: Color) {
        let x0 = from.x.min(to.x);
        let y0 = from.y.min(to.y);
        let x1 = from.x.max(to.x);
        let y1 = from.y.max(to.y);
        let rect = if (y1 - y0).abs() <= (x1 - x0).abs() {
            Rect::new(x0, (y0 + y1) * 0.5 - width * 0.5, x1 - x0, width)
        } else {
            Rect::new((x0 + x1) * 0.5 - width * 0.5, y0, width, y1 - y0)
        };
        self.fill(rect, color);
    }

    pub fn hline(&mut self, x: f32, y: f32, w: f32, thickness: f32, color: Color) {
        self.fill(Rect::new(x, y, w, thickness), color);
    }

    pub fn vline(&mut self, x: f32, y: f32, h: f32, thickness: f32, color: Color) {
        self.fill(Rect::new(x, y, thickness, h), color);
    }

    /// Filled circle (a square rounded rect with radius = half the side).
    pub fn circle(&mut self, center: Vec2, radius: f32, color: Color) {
        self.rounded_rect(
            Rect::new(
                center.x - radius,
                center.y - radius,
                radius * 2.0,
                radius * 2.0,
            ),
            color,
            radius,
        );
    }

    /// A soft drop shadow. The paint contract has no blur primitive, so this
    /// approximates the CSS `box-shadow: 0 10px 30px rgba(0,0,0,.55)` used by
    /// menus/banners with four stacked translucent rounded rects offset down.
    pub fn shadow(&mut self, rect: Rect, radius: f32, strength: f32) {
        let strength = strength.clamp(0.0, 1.0);
        // (down offset, alpha, expand radius)
        let layers = [
            (2.0f32, 0.10f32, 0.0f32),
            (5.0, 0.12, 1.0),
            (8.0, 0.14, 2.0),
            (10.0, 0.17, 3.0),
        ];
        for (dy, alpha, expand) in layers {
            let color = Color::rgba(0, 0, 0, (alpha * strength * 255.0).round() as u8);
            self.rounded_rect(
                Rect::new(
                    rect.x - expand,
                    rect.y + dy - expand,
                    rect.w + expand * 2.0,
                    rect.h + expand * 2.0,
                ),
                color,
                radius + expand,
            );
        }
    }

    /// Textured rectangle (cover art, app textures). `radius` rounds the corners.
    pub fn image(&mut self, rect: Rect, texture: TextureId, tint: Color, radius: f32) {
        if rect.is_empty() || texture == NO_TEXTURE {
            return;
        }
        let uv = Rect::new(0.0, 0.0, 1.0, 1.0);
        self.push_prim(Prim::Image {
            rect,
            texture,
            uv,
            tint,
            radius: radius.max(0.0),
        });
    }

    /// Textured rectangle from a sub-rectangle of the texture (`uv` normalized).
    pub fn image_uv(&mut self, rect: Rect, texture: TextureId, uv: Rect, tint: Color, radius: f32) {
        if rect.is_empty() || texture == NO_TEXTURE {
            return;
        }
        self.push_prim(Prim::Image {
            rect,
            texture,
            uv,
            tint,
            radius: radius.max(0.0),
        });
    }

    // ----------------------------------------------------------------- text

    /// Shape a string without drawing it (for custom layout).
    pub fn layout_text(
        &mut self,
        text: &str,
        style: &TextStyle,
        max_width: Option<f32>,
    ) -> TextShape {
        let mut shape = TextShape::new();
        self.fonts.shape_into(text, style, max_width, &mut shape);
        shape
    }

    pub fn measure_text(&mut self, text: &str, style: &TextStyle) -> Size {
        self.fonts.measure(text, style)
    }

    pub fn measure_text_wrapped(&mut self, text: &str, style: &TextStyle, max_width: f32) -> Size {
        self.fonts.measure_wrapped(text, style, max_width)
    }

    pub fn text_width(&mut self, text: &str, style: &TextStyle) -> f32 {
        self.fonts.line_width(text, style)
    }

    pub fn line_height(&self, style: &TextStyle) -> f32 {
        self.fonts.line_height(style)
    }

    /// Draw a pre-shaped run of glyphs with its top-left at `origin`.
    pub fn glyph_run(&mut self, origin: Vec2, shape: &TextShape, color: Color) {
        for g in &shape.glyphs {
            self.image_uv(
                Rect::new(origin.x + g.x, origin.y + g.y, g.w, g.h),
                ATLAS_TEXTURE_ID,
                Rect::new(g.uv[0], g.uv[1], g.uv[2] - g.uv[0], g.uv[3] - g.uv[1]),
                color,
                0.0,
            );
        }
    }

    fn draw_ellipsized(&mut self, rect: Rect, text: &str, style: &TextStyle, align: Align) {
        if rect.is_empty() {
            return;
        }
        let has_newline = text.find('\n').is_some();
        let mut shape = std::mem::take(&mut self.scratch.shape);
        // Shape once first: most labels fit their rect, and the old ordering
        // paid a separate measuring pass (`ellipsize` -> `line_width`) plus this
        // shaping pass for every one of them. Only text that actually needs
        // truncating goes through the measuring fallback. Text with a hard
        // newline keeps the old path because `ellipsize` deliberately truncates
        // at the first line.
        if !has_newline {
            self.fonts.shape_into(text, style, None, &mut shape);
        }
        if has_newline || shape.size.w > rect.w {
            let displayed: Cow<'_, str> = self.fonts.ellipsize(text, style, rect.w);
            self.fonts
                .shape_into(displayed.as_ref(), style, None, &mut shape);
        }
        let origin = text_origin(rect, shape.size, align, VAlign::Center);
        self.glyph_run(origin, &shape, style.color);
        self.scratch.shape = shape;
    }

    fn draw_text_block(
        &mut self,
        rect: Rect,
        text: &str,
        style: &TextStyle,
        max_width: Option<f32>,
        align: Align,
        valign: VAlign,
    ) {
        if rect.is_empty() {
            return;
        }
        let mut shape = std::mem::take(&mut self.scratch.shape);
        self.fonts.shape_into(text, style, max_width, &mut shape);
        let origin = text_origin(rect, shape.size, align, valign);
        self.glyph_run(origin, &shape, style.color);
        self.scratch.shape = shape;
    }

    /// Single-line label: left-aligned, vertically centred, ellipsized to fit
    /// `rect.w`. This is the default text call for rows, buttons, chips.
    pub fn text(&mut self, rect: Rect, text: &str, style: &TextStyle) {
        self.draw_ellipsized(rect, text, style, Align::Left);
    }

    pub fn text_centered(&mut self, rect: Rect, text: &str, style: &TextStyle) {
        self.draw_ellipsized(rect, text, style, Align::Center);
    }

    pub fn text_right(&mut self, rect: Rect, text: &str, style: &TextStyle) {
        self.draw_ellipsized(rect, text, style, Align::Right);
    }

    /// Single-line label with explicit alignment, ellipsized to `rect.w`.
    pub fn text_aligned(&mut self, rect: Rect, text: &str, style: &TextStyle, align: Align) {
        self.draw_ellipsized(rect, text, style, align);
    }

    /// Word-wrapped block, top-left aligned, wrapped to `rect.w`.
    pub fn text_wrapped(&mut self, rect: Rect, text: &str, style: &TextStyle) {
        self.draw_text_block(rect, text, style, Some(rect.w), Align::Left, VAlign::Top);
    }

    /// Word-wrapped block with explicit alignment.
    pub fn text_wrapped_aligned(
        &mut self,
        rect: Rect,
        text: &str,
        style: &TextStyle,
        align: Align,
        valign: VAlign,
    ) {
        self.draw_text_block(rect, text, style, Some(rect.w), align, valign);
    }

    /// Unprocessed text block (no ellipsis, no wrapping) starting at the
    /// rectangle's top-left. Newlines are honoured.
    pub fn text_raw(&mut self, rect: Rect, text: &str, style: &TextStyle) {
        self.draw_text_block(rect, text, style, None, Align::Left, VAlign::Top);
    }

    // ---------------------------------------------------------------- icons

    /// Draw an icon centred in `rect`, rendered for an `icon_size`-pixel box
    /// (`Material Icons` sizing: 16.0 matches the old inline SVGs' 16px width).
    pub fn icon(&mut self, rect: Rect, icon: Icon, color: Color, icon_size: f32) {
        self.icon_font(rect, icon, color, icon_size, IconFont::Filled);
    }

    /// Draw an icon from a specific icon font variant.
    pub fn icon_font(
        &mut self,
        rect: Rect,
        icon: Icon,
        color: Color,
        icon_size: f32,
        font: IconFont,
    ) {
        let size = if icon_size > 0.0 {
            icon_size
        } else {
            rect.w.min(rect.h)
        };
        let outlined = font == IconFont::Outlined;
        if let Some(g) = self.fonts.icon_glyph(icon.codepoint(), size, outlined) {
            let ink = Rect::new(
                rect.x + (rect.w - g.w) * 0.5,
                rect.y + (rect.h - g.h) * 0.5,
                g.w,
                g.h,
            );
            self.image_uv(
                ink,
                ATLAS_TEXTURE_ID,
                Rect::new(g.uv[0], g.uv[1], g.uv[2] - g.uv[0], g.uv[3] - g.uv[1]),
                color,
                0.0,
            );
        }
    }

    /// Draw an icon from the outlined Material Icons font.
    pub fn icon_outlined(&mut self, rect: Rect, icon: Icon, color: Color, icon_size: f32) {
        self.icon_font(rect, icon, color, icon_size, IconFont::Outlined);
    }

    /// Inline icon + label laid out left-to-right and centred vertically, as
    /// used by `.play-all`/`.head-btn`. Returns the used width.
    pub fn icon_label(
        &mut self,
        rect: Rect,
        icon: Icon,
        icon_size: f32,
        label: &str,
        style: &TextStyle,
        gap: f32,
    ) -> f32 {
        let text_w = self.fonts.line_width(label, style);
        let total = icon_size + gap + text_w;
        let mut x = rect.x + (rect.w - total) * 0.5;
        let icon_rect = Rect::new(x, rect.y + (rect.h - icon_size) * 0.5, icon_size, icon_size);
        self.icon(icon_rect, icon, style.color, icon_size);
        x += icon_size + gap;
        self.draw_text_block(
            Rect::new(x, rect.y, text_w + 1.0, rect.h),
            label,
            style,
            None,
            Align::Left,
            VAlign::Center,
        );
        total
    }
}

/// Top-left origin that places a `size`-sized text block inside `rect` according
/// to the requested alignments. Shared by the wrapped and single-line draw
/// paths so they place text identically.
fn text_origin(rect: Rect, size: Size, align: Align, valign: VAlign) -> Vec2 {
    let mut origin = Vec2::new(rect.x, rect.y);
    match align {
        Align::Left => {}
        Align::Center => origin.x += (rect.w - size.w) * 0.5,
        Align::Right => origin.x += rect.w - size.w,
    }
    match valign {
        VAlign::Top => {}
        VAlign::Center => origin.y += (rect.h - size.h) * 0.5,
        VAlign::Bottom => origin.y += rect.h - size.h,
    }
    origin
}

/// Append a group of clipped primitives to the paint list, starting a new layer
/// every time the clip changes. Painter order is preserved exactly because the
/// input order is the paint order.
fn push_grouped_layers(paint: &mut PaintList, prims: &[PrimWithClip]) {
    let mut current: Option<Option<Rect>> = None;
    for p in prims {
        if current != Some(p.clip) {
            paint.layers.push(paint::Layer::new(p.clip));
            current = Some(p.clip);
        }
        if let Some(layer) = paint.layers.last_mut() {
            layer.prims.push(p.prim);
        }
    }
}
