//! Binary entry point: command-line handling, the winit event loop, and the
//! translation of OS input into the winit-agnostic [`InputState`] the
//! immediate-mode UI consumes.
//!
//! Modes:
//! * windowed (default): a real Vulkan window, presented through a swapchain.
//! * `--screenshot <path>` (development builds and the explicit `preview`
//!   feature only): starts no window, builds one UI frame with preview data and
//!   writes it to a PNG through the offscreen renderer. `--view
//!   home|search|liked|settings|artist|artist-songs|album|playlist` selects the
//!   screen to capture. The shipped release build neither accepts `--preview`
//!   nor contains the placeholder data.
//!
//! Window configuration mirrors the old window config: 1160x740, minimum
//! 940x620, decorations disabled (the app draws its own titlebar), resizable.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(any(debug_assertions, feature = "preview"))]
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Context;
use rift_desktop::app::{App, WindowCommand};
use rift_desktop::controller::Controller;
use rift_desktop::gfx;
use rift_desktop::state::ResizeEdge;
use rift_desktop::ui::input::{InputState, Key, KeyInput, Modifiers};
use rift_desktop::ui::{CursorShape, Rect};
use rift_desktop::{single_instance, window_state};
use tracing::{error, info, warn};
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalPosition, LogicalSize, PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key as WinitKey, ModifiersState, NamedKey};
use winit::window::{CursorIcon, ResizeDirection, Window, WindowId};

/// Default window / reference-screenshot size (matches the old window config).
#[cfg(any(debug_assertions, feature = "preview"))]
const DEFAULT_WIDTH: u32 = 1160;
#[cfg(any(debug_assertions, feature = "preview"))]
const DEFAULT_HEIGHT: u32 = 740;
/// Invisible border around the client area that starts a native window resize.
const RESIZE_BORDER: f32 = 5.0;

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                tracing_subscriber::EnvFilter::new("rift=debug,rift_desktop=debug")
            }),
        )
        .init();

    let args = Args::parse(std::env::args().skip(1))?;
    info!("rift-desktop starting: {args:?}");

    // A second instance exits immediately: the old build used a shell-level
    // single-instance plugin to focus the running window; the native app just
    // refuses to start a rival audio thread/GPU context (documented in
    // docs/native-platform.md).
    let _single_instance = match single_instance::acquire() {
        Some(guard) => guard,
        None => {
            info!("another Rift instance is already running; exiting");
            eprintln!("Rift is already running.");
            return Ok(());
        }
    };

    #[cfg(any(debug_assertions, feature = "preview"))]
    if let Some(path) = args.screenshot.clone() {
        return run_screenshot(&args, &path);
    }
    run_windowed(args)
}

// ---------------------------------------------------------------------------
// Command line
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Default)]
struct Args {
    /// Dev-only placeholder data (the old `RIFT_PREVIEW=1`). `false` in the
    /// shipped release build, where the dev flags are rejected before this is
    /// ever set.
    preview: bool,
    /// One-shot offscreen capture target (development builds / `preview` feature).
    #[cfg(any(debug_assertions, feature = "preview"))]
    screenshot: Option<PathBuf>,
    /// Capture size for `--screenshot` (defaults to 1160x740).
    #[cfg(any(debug_assertions, feature = "preview"))]
    size: (u32, u32),
    /// Which preview screen to capture.
    #[cfg(any(debug_assertions, feature = "preview"))]
    view: Option<String>,
    /// Pointer position for the deterministic screenshot (logical px), used to
    /// reproduce hover states in the reference screenshot.
    #[cfg(any(debug_assertions, feature = "preview"))]
    pointer: Option<(f32, f32)>,
    /// Device pixel ratio for `--screenshot` (default 1.0; 1.25/1.5 exercise the
    /// `HiDPI` text-atlas path).
    #[cfg(any(debug_assertions, feature = "preview"))]
    scale: f32,
}

/// Whether the old `RIFT_PREVIEW` environment switch is set to an enabling
/// value (`1`/anything non-empty and not `0`).
fn preview_env_requested() -> bool {
    std::env::var("RIFT_PREVIEW")
        .map(|value| !value.is_empty() && value != "0")
        .unwrap_or(false)
}

impl Args {
    fn parse(args: impl Iterator<Item = String>) -> anyhow::Result<Self> {
        #[cfg(any(debug_assertions, feature = "preview"))]
        let mut parsed = Args {
            preview: preview_env_requested(),
            size: (DEFAULT_WIDTH, DEFAULT_HEIGHT),
            scale: 1.0,
            ..Args::default()
        };
        #[cfg(not(any(debug_assertions, feature = "preview")))]
        #[allow(unused_mut)] // no dev flags mutate it in a shipped release build
        let mut parsed = {
            if preview_env_requested() {
                anyhow::bail!(
                    "RIFT_PREVIEW is a development-only option and is not available in release builds"
                );
            }
            Args::default()
        };
        let mut args = args.peekable();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--preview" | "--demo" => {
                    #[cfg(any(debug_assertions, feature = "preview"))]
                    {
                        parsed.preview = true;
                    }
                    #[cfg(not(any(debug_assertions, feature = "preview")))]
                    {
                        anyhow::bail!(
                            "--preview is a development-only option and is not available in release builds"
                        );
                    }
                }
                "--live" => {
                    #[cfg(any(debug_assertions, feature = "preview"))]
                    {
                        parsed.preview = false;
                    }
                    #[cfg(not(any(debug_assertions, feature = "preview")))]
                    {
                        anyhow::bail!(
                            "--live is a development-only option and is not available in release builds"
                        );
                    }
                }
                "--screenshot" => {
                    #[cfg(any(debug_assertions, feature = "preview"))]
                    {
                        let path = args
                            .next()
                            .context("--screenshot needs an output path argument")?;
                        parsed.screenshot = Some(PathBuf::from(path));
                    }
                    #[cfg(not(any(debug_assertions, feature = "preview")))]
                    {
                        anyhow::bail!(
                            "--screenshot is a development-only option and is not available in release builds"
                        );
                    }
                }
                "--view" => {
                    #[cfg(any(debug_assertions, feature = "preview"))]
                    {
                        parsed.view = Some(args.next().context("--view needs a screen name")?);
                    }
                    #[cfg(not(any(debug_assertions, feature = "preview")))]
                    {
                        anyhow::bail!(
                            "--view is a development-only option and is not available in release builds"
                        );
                    }
                }
                "--pointer" => {
                    #[cfg(any(debug_assertions, feature = "preview"))]
                    {
                        let value = args.next().context("--pointer needs an x,y argument")?;
                        let (x, y) = value
                            .split_once(',')
                            .context("--pointer must look like 700,407")?;
                        parsed.pointer = Some((
                            x.trim().parse().context("invalid --pointer x")?,
                            y.trim().parse().context("invalid --pointer y")?,
                        ));
                    }
                    #[cfg(not(any(debug_assertions, feature = "preview")))]
                    {
                        anyhow::bail!(
                            "--pointer is a development-only option and is not available in release builds"
                        );
                    }
                }
                "--scale" => {
                    #[cfg(any(debug_assertions, feature = "preview"))]
                    {
                        let value = args.next().context("--scale needs a factor like 1.5")?;
                        parsed.scale = value.trim().parse().context("invalid --scale factor")?;
                        if !(0.5..=4.0).contains(&parsed.scale) {
                            anyhow::bail!("--scale must be between 0.5 and 4.0");
                        }
                    }
                    #[cfg(not(any(debug_assertions, feature = "preview")))]
                    {
                        anyhow::bail!(
                            "--scale is a development-only option and is not available in release builds"
                        );
                    }
                }
                "--size" => {
                    #[cfg(any(debug_assertions, feature = "preview"))]
                    {
                        let value = args.next().context("--size needs a WxH argument")?;
                        let (w, h) = value
                            .split_once('x')
                            .or_else(|| value.split_once('X'))
                            .context("--size must look like 1160x740")?;
                        parsed.size = (
                            w.parse().context("invalid --size width")?,
                            h.parse().context("invalid --size height")?,
                        );
                    }
                    #[cfg(not(any(debug_assertions, feature = "preview")))]
                    {
                        anyhow::bail!(
                            "--size is a development-only option and is not available in release builds"
                        );
                    }
                }
                "-h" | "--help" => {
                    #[cfg(any(debug_assertions, feature = "preview"))]
                    println!(
                        "rift-music [--preview] [--screenshot <path>] [--view <name>] [--size WxH]\n\
                         \n\
                         Vanilla launch opens the Rift window. --screenshot renders one frame\n\
                         offscreen (preview data unless --live) to a PNG and exits."
                    );
                    #[cfg(not(any(debug_assertions, feature = "preview")))]
                    println!("rift-music\n\nVanilla launch opens the Rift window.");
                    std::process::exit(0);
                }
                other => anyhow::bail!("unknown argument {other:?} (try --help)"),
            }
        }
        Ok(parsed)
    }
}

// ---------------------------------------------------------------------------
// Offscreen screenshot mode (development builds / explicit `preview` feature)
// ---------------------------------------------------------------------------

#[cfg(any(debug_assertions, feature = "preview"))]
fn run_screenshot(args: &Args, path: &std::path::Path) -> anyhow::Result<()> {
    let (logical_w, logical_h) = args.size;
    let scale = args.scale;
    let physical_w = (logical_w as f32 * scale).round() as u32;
    let physical_h = (logical_h as f32 * scale).round() as u32;
    let mut app = App::new(args.preview, Controller::new_preview());
    if let Some(view) = &args.view {
        app.set_preview_view(view);
    }
    // Let the first frame settle the UI memory (fonts, hover state, …).
    app.tick();
    // The reference screenshot has the pointer hovering the first
    // "Recently played" row; use that position by default so hover-revealed
    // controls are reproduced deterministically.
    let (pointer_x, pointer_y) = args.pointer.unwrap_or((700.0, 407.0));
    let input = InputState {
        viewport: Rect::new(0.0, 0.0, logical_w as f32, logical_h as f32),
        scale_factor: scale,
        time: 1.0,
        delta_time: 1.0 / 60.0,
        mouse_pos: (pointer_x, pointer_y),
        mouse_in_window: true,
        ..InputState::default()
    };
    let paint = app.frame(input);
    gfx::screenshot(physical_w, physical_h, scale, &paint, path)?;
    info!(
        "wrote {physical_w}x{physical_h} screenshot (scale {scale}) to {}",
        path.display()
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Windowed mode
// ---------------------------------------------------------------------------

fn run_windowed(args: Args) -> anyhow::Result<()> {
    let event_loop = EventLoop::new().context("could not create the winit event loop")?;
    event_loop.set_control_flow(ControlFlow::WaitUntil(
        Instant::now() + Duration::from_millis(16),
    ));
    let mut app = NativeApp::new(args);
    event_loop.run_app(&mut app).context("event loop failed")?;
    Ok(())
}

struct NativeApp {
    args: Args,
    window: Option<Arc<Window>>,
    renderer: Option<gfx::Renderer>,
    app: Option<App>,
    input: InputState,
    started: Instant,
    last_frame: Instant,
    last_cursor: Option<CursorIcon>,
    /// Mouse position in physical px, for edge hit-testing.
    mouse_physical: PhysicalPosition<f64>,
    /// Last persisted window geometry (native replacement for the old shell's
    /// window-state plugin).
    window_state: window_state::WindowState,
    /// Whether `window_state` came from disk and should be restored on create.
    has_saved_window_state: bool,
}

impl NativeApp {
    fn new(args: Args) -> Self {
        let saved = window_state::WindowState::load_default();
        let has_saved_window_state = saved.is_some();
        let window_state = saved.unwrap_or_default();
        Self {
            args,
            window: None,
            renderer: None,
            app: None,
            input: InputState::default(),
            started: Instant::now(),
            last_frame: Instant::now(),
            last_cursor: None,
            mouse_physical: PhysicalPosition::new(-1.0, -1.0),
            window_state,
            has_saved_window_state,
        }
    }

    fn create_window(&mut self, event_loop: &ActiveEventLoop) -> anyhow::Result<()> {
        let icon = window_icon();
        // Restore the last session's size if there is one, otherwise the old
        // Legacy default (1160x740, centred by the OS, min 940x620).
        let (width, height) = if self.window_state.width >= 1 && self.window_state.height >= 1 {
            self.window_state.size()
        } else {
            window_state::DEFAULT_SIZE
        };
        let mut attributes = Window::default_attributes()
            .with_title("Rift")
            .with_inner_size(LogicalSize::new(width as f64, height as f64))
            .with_min_inner_size(LogicalSize::new(
                window_state::MIN_WIDTH as f64,
                window_state::MIN_HEIGHT as f64,
            ))
            .with_decorations(false)
            .with_resizable(true)
            .with_visible(true);
        if let Some(icon) = icon {
            attributes = attributes.with_window_icon(Some(icon));
        }
        let window = Arc::new(
            event_loop
                .create_window(attributes)
                .context("could not create the Rift window")?,
        );
        if self.has_saved_window_state {
            // Position and maximized flag are applied after creation; the size is
            // already in the attributes above.
            window_state::restore(&window, &self.window_state);
        } else if let Some(monitor) = window.primary_monitor() {
            // The old window had `"center": true`; winit has no centering
            // attribute, so do it once from the monitor's logical size.
            let scale = monitor.scale_factor();
            let screen = monitor.size().to_logical::<f64>(scale);
            let x = ((screen.width - width as f64) * 0.5).max(0.0);
            let y = ((screen.height - height as f64) * 0.5).max(0.0);
            window.set_outer_position(LogicalPosition::new(x, y));
        }
        let renderer =
            gfx::Renderer::new(window.clone()).context("could not create the Vulkan renderer")?;

        // The backend is constructed after the window so Windows media
        // integration can attach to its HWND. If it fails, the app still opens in
        // a degraded preview-ish mode with the error visible as a toast.
        //
        // Preview mode (`--preview` / RIFT_PREVIEW) never constructs the backend:
        // it must render only the deterministic placeholder data, exactly like the
        // old dev-only mode the reference screenshots came from.
        let hwnd = native_window_handle(&window);
        let (controller, backend_error) = if self.args.preview {
            (Controller::new_preview(), None)
        } else {
            match Controller::new(hwnd) {
                Ok(controller) => (controller, None),
                Err(error) => {
                    warn!("backend unavailable, continuing without it: {error:#}");
                    (Controller::new_preview(), Some(format!("{error:#}")))
                }
            }
        };
        let mut app = App::new(self.args.preview, controller);
        if let Some(error) = backend_error {
            app.toast_error(error);
        }

        self.window = Some(window);
        self.renderer = Some(renderer);
        self.app = Some(app);
        self.started = Instant::now();
        self.last_frame = Instant::now();
        info!("window created");
        Ok(())
    }

    fn redraw(&mut self, event_loop: &ActiveEventLoop) {
        let (Some(window), Some(renderer), Some(app)) = (
            self.window.as_ref(),
            self.renderer.as_mut(),
            self.app.as_mut(),
        ) else {
            return;
        };
        let now = self.started.elapsed().as_secs_f64();
        self.input.time = now;
        self.input.delta_time = self.last_frame.elapsed().as_secs_f32().clamp(0.0, 0.1);
        self.last_frame = Instant::now();
        let scale = window.scale_factor() as f32;
        let size = window.inner_size();
        self.input.scale_factor = scale;
        self.input.viewport = Rect::new(
            0.0,
            0.0,
            size.width as f32 / scale,
            size.height as f32 / scale,
        );

        app.tick();
        let paint = app.frame(self.input.clone());
        if let Err(error) = renderer.frame(&paint) {
            error!("render failed: {error:#}");
        }
        for command in app.take_window_commands() {
            run_window_command(window, event_loop, command);
        }
        let cursor = cursor_for(app.cursor_shape, self.edge_at(window));
        if self.last_cursor != Some(cursor) {
            window.set_cursor(cursor);
            self.last_cursor = Some(cursor);
        }
        self.input.end_frame();
        window.request_redraw();
    }

    /// Which window edge the pointer is over (for the undecorated resize border).
    fn edge_at(&self, window: &Window) -> Option<ResizeEdge> {
        let size = window.inner_size();
        let scale = window.scale_factor();
        let x = self.mouse_physical.x / scale;
        let y = self.mouse_physical.y / scale;
        let width = size.width as f64 / scale;
        let height = size.height as f64 / scale;
        let border = RESIZE_BORDER as f64;
        let left = x <= border;
        let right = x >= width - border;
        let top = y <= border;
        let bottom = y >= height - border;
        match (left, right, top, bottom) {
            (true, _, true, _) => Some(ResizeEdge::TopLeft),
            (_, true, true, _) => Some(ResizeEdge::TopRight),
            (true, _, _, true) => Some(ResizeEdge::BottomLeft),
            (_, true, _, true) => Some(ResizeEdge::BottomRight),
            (true, _, _, _) => Some(ResizeEdge::Left),
            (_, true, _, _) => Some(ResizeEdge::Right),
            (_, _, true, _) => Some(ResizeEdge::Top),
            (_, _, _, true) => Some(ResizeEdge::Bottom),
            _ => None,
        }
    }
}

impl ApplicationHandler for NativeApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_none() {
            if let Err(error) = self.create_window(event_loop) {
                error!("startup failed: {error:#}");
                event_loop.exit();
            }
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        let Some(window) = self.window.clone() else {
            return;
        };
        if window.id() != window_id {
            return;
        }
        match event {
            WindowEvent::CloseRequested => {
                window_state::save_on_exit(&self.window_state);
                event_loop.exit();
            }
            WindowEvent::Resized(size) => {
                self.input.viewport = logical_viewport(&window, size);
                if let Some(renderer) = self.renderer.as_mut() {
                    renderer.resize((size.width, size.height));
                }
                window_state::track(&window, &mut self.window_state);
            }
            WindowEvent::Moved(_) => {
                window_state::track(&window, &mut self.window_state);
            }
            WindowEvent::ScaleFactorChanged { .. } => {
                self.input.viewport = logical_viewport(&window, window.inner_size());
                window_state::track(&window, &mut self.window_state);
            }
            WindowEvent::RedrawRequested => self.redraw(event_loop),
            WindowEvent::CursorMoved { position, .. } => {
                self.mouse_physical = position;
                let scale = window.scale_factor();
                self.input.mouse_pos = ((position.x / scale) as f32, (position.y / scale) as f32);
                self.input.mouse_in_window = true;
                window.request_redraw();
            }
            WindowEvent::CursorEntered { .. } => {
                self.input.mouse_in_window = true;
            }
            WindowEvent::CursorLeft { .. } => {
                self.input.mouse_in_window = false;
            }
            WindowEvent::MouseInput { state, button, .. } => {
                let pressed = state == ElementState::Pressed;
                // The undecorated resize border wins over the UI underneath it.
                if pressed {
                    if let Some(edge) = self.edge_at(&window) {
                        window.request_redraw();
                        if run_resize(&window, edge) {
                            self.input.mouse_left_pressed = false;
                            return;
                        }
                    }
                }
                match button {
                    MouseButton::Left => {
                        if pressed {
                            self.input.mouse_left_pressed = true;
                        } else {
                            self.input.mouse_left_released = true;
                        }
                        self.input.mouse_left_down = pressed;
                    }
                    MouseButton::Right => {
                        if pressed {
                            self.input.mouse_right_pressed = true;
                        } else {
                            self.input.mouse_right_released = true;
                        }
                        self.input.mouse_right_down = pressed;
                    }
                    MouseButton::Middle => {
                        if pressed {
                            self.input.mouse_middle_pressed = true;
                        } else {
                            self.input.mouse_middle_released = true;
                        }
                        self.input.mouse_middle_down = pressed;
                    }
                    _ => {}
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let (dx, dy) = match delta {
                    MouseScrollDelta::LineDelta(x, y) => (x * 40.0, y * 40.0),
                    MouseScrollDelta::PixelDelta(pos) => (pos.x as f32, pos.y as f32),
                };
                self.input.scroll.0 += dx;
                self.input.scroll.1 += dy;
            }
            WindowEvent::ModifiersChanged(modifiers) => {
                self.input.modifiers = modifiers_from(modifiers.state());
            }
            WindowEvent::KeyboardInput { event, .. } => {
                let pressed = event.state == ElementState::Pressed;
                let modifiers = self.input.modifiers;
                let key = map_key(&event.logical_key);
                if let Some(key) = key {
                    self.input.keys.push(KeyInput {
                        key,
                        pressed,
                        repeat: event.repeat,
                        modifiers,
                    });
                }
                if pressed && !event.repeat && !modifiers.ctrl && !modifiers.alt {
                    if let Some(text) = &event.text {
                        for ch in text.chars() {
                            if !ch.is_control() {
                                self.input.text.push(ch);
                            }
                        }
                    }
                }
            }
            WindowEvent::Focused(focused) => {
                if !focused {
                    apply_focus_lost(&mut self.input);
                }
            }
            WindowEvent::Occluded(false) => window.request_redraw(),
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        // ~60 fps while visible; the UI is cheap and animations/toasts need a
        // steady clock. When occluded, winit stops delivering RedrawRequested.
        event_loop.set_control_flow(ControlFlow::WaitUntil(
            Instant::now() + Duration::from_millis(16),
        ));
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        // Release GPU resources before the window (and its surface) goes away,
        // persist the window geometry, then stop backend threads cleanly.
        window_state::save_on_exit(&self.window_state);
        if let Some(renderer) = self.renderer.as_mut() {
            renderer.shutdown();
        }
        if let Some(app) = self.app.as_mut() {
            app.controller.shutdown();
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn logical_viewport(window: &Window, size: PhysicalSize<u32>) -> Rect {
    let scale = window.scale_factor() as f32;
    Rect::new(
        0.0,
        0.0,
        size.width as f32 / scale,
        size.height as f32 / scale,
    )
}

fn run_window_command(window: &Window, event_loop: &ActiveEventLoop, command: WindowCommand) {
    match command {
        WindowCommand::Drag => {
            if let Err(error) = window.drag_window() {
                warn!("window drag failed: {error}");
            }
        }
        WindowCommand::Minimize => window.set_minimized(true),
        WindowCommand::ToggleMaximize => window.set_maximized(!window.is_maximized()),
        WindowCommand::Close => event_loop.exit(),
        WindowCommand::BeginResize(edge) => {
            run_resize(window, edge);
        }
    }
}

fn run_resize(window: &Window, edge: ResizeEdge) -> bool {
    let direction = match edge {
        ResizeEdge::Left => ResizeDirection::West,
        ResizeEdge::Right => ResizeDirection::East,
        ResizeEdge::Top => ResizeDirection::North,
        ResizeEdge::Bottom => ResizeDirection::South,
        ResizeEdge::TopLeft => ResizeDirection::NorthWest,
        ResizeEdge::TopRight => ResizeDirection::NorthEast,
        ResizeEdge::BottomLeft => ResizeDirection::SouthWest,
        ResizeEdge::BottomRight => ResizeDirection::SouthEast,
    };
    match window.drag_resize_window(direction) {
        Ok(()) => true,
        Err(error) => {
            warn!("window resize failed: {error}");
            false
        }
    }
}

fn cursor_for(cursor: CursorShape, edge: Option<ResizeEdge>) -> CursorIcon {
    if let Some(edge) = edge {
        return match edge {
            ResizeEdge::Left | ResizeEdge::Right => CursorIcon::EwResize,
            ResizeEdge::Top | ResizeEdge::Bottom => CursorIcon::NsResize,
            ResizeEdge::TopLeft | ResizeEdge::BottomRight => CursorIcon::NwseResize,
            ResizeEdge::TopRight | ResizeEdge::BottomLeft => CursorIcon::NeswResize,
        };
    }
    match cursor {
        CursorShape::Default => CursorIcon::Default,
        CursorShape::Pointer => CursorIcon::Pointer,
        CursorShape::Text => CursorIcon::Text,
        CursorShape::Grab => CursorIcon::Grab,
        CursorShape::Grabbing => CursorIcon::Grabbing,
        CursorShape::ResizeH => CursorIcon::EwResize,
        CursorShape::ResizeV => CursorIcon::NsResize,
        CursorShape::NotAllowed => CursorIcon::NotAllowed,
        CursorShape::Crosshair => CursorIcon::Crosshair,
    }
}

fn modifiers_from(state: ModifiersState) -> Modifiers {
    Modifiers {
        shift: state.shift_key(),
        ctrl: state.control_key(),
        alt: state.alt_key(),
        super_key: state.super_key(),
    }
}

fn map_key(key: &WinitKey) -> Option<Key> {
    use winit::keyboard::Key as K;
    Some(match key {
        K::Named(NamedKey::Space) => Key::Space,
        K::Named(NamedKey::Enter) => Key::Enter,
        K::Named(NamedKey::Escape) => Key::Escape,
        K::Named(NamedKey::Backspace) => Key::Backspace,
        K::Named(NamedKey::Delete) => Key::Delete,
        K::Named(NamedKey::Tab) => Key::Tab,
        K::Named(NamedKey::ArrowLeft) => Key::Left,
        K::Named(NamedKey::ArrowRight) => Key::Right,
        K::Named(NamedKey::ArrowUp) => Key::Up,
        K::Named(NamedKey::ArrowDown) => Key::Down,
        K::Named(NamedKey::Home) => Key::Home,
        K::Named(NamedKey::End) => Key::End,
        K::Named(NamedKey::PageUp) => Key::PageUp,
        K::Named(NamedKey::PageDown) => Key::PageDown,
        K::Named(NamedKey::Insert) => Key::Insert,
        K::Character(text) => Key::Char(text.chars().next()?.to_ascii_lowercase()),
        _ => return None,
    })
}

/// Drop in-flight input when the window loses focus.
///
/// The OS releases held keys/buttons while we are not focused, so their release
/// edges are never delivered. Clear the held state **without synthesizing a
/// release edge** and raise the one-frame [`InputState::focus_lost`] flag:
/// `Ui::begin_frame` uses it to drop stale press/drag latches, so the first
/// click after refocusing is a fresh press instead of a swallowed one.
fn apply_focus_lost(input: &mut InputState) {
    input.keys.clear();
    input.mouse_left_down = false;
    input.mouse_right_down = false;
    input.mouse_middle_down = false;
    input.mouse_left_pressed = false;
    input.mouse_right_pressed = false;
    input.mouse_middle_pressed = false;
    input.mouse_left_released = false;
    input.mouse_right_released = false;
    input.mouse_middle_released = false;
    input.focus_lost = true;
}

/// Load the window icon from the app assets, embedded at build
/// time so the binary is self-contained.
fn window_icon() -> Option<winit::window::Icon> {
    static ICON_PNG: &[u8] = include_bytes!("../assets/icons/128x128.png");
    let image = image::load_from_memory(ICON_PNG).ok()?.into_rgba8();
    let (width, height) = image.dimensions();
    winit::window::Icon::from_rgba(image.into_raw(), width, height).ok()
}

/// Native window handle for OS media integration (SMTC on Windows).
fn native_window_handle(window: &Window) -> Option<isize> {
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    match window.window_handle().ok()?.as_raw() {
        RawWindowHandle::Win32(handle) => Some(handle.hwnd.get()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Focus loss releases held buttons/keys eagerly but must not fake a release
    /// edge (the UI clears its own latch from `focus_lost` instead).
    #[test]
    fn apply_focus_lost_releases_state_without_a_synthetic_release_edge() {
        let mut input = InputState {
            mouse_left_down: true,
            mouse_left_pressed: true,
            mouse_right_down: true,
            keys: vec![KeyInput {
                key: Key::Space,
                pressed: true,
                repeat: false,
                modifiers: Modifiers::default(),
            }],
            ..InputState::default()
        };

        apply_focus_lost(&mut input);

        assert!(input.focus_lost, "the one-frame flag is raised");
        assert!(!input.mouse_left_down && !input.mouse_right_down);
        assert!(!input.mouse_left_pressed && !input.mouse_right_pressed);
        assert!(
            !input.mouse_left_released && !input.mouse_right_released,
            "no synthetic release edge; the UI clears its latch instead"
        );
        assert!(input.keys.is_empty(), "held keys are released");
    }
}
