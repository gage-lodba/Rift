//! The screens of the app. Each submodule owns one area of the window; the root
//! composes them in the same order the old DOM did:
//!
//! ```text
//! ┌──────────────────────────────────────────────────────────┐
//! │ titlebar: logo + search + window controls      46px      │
//! ├──────────────┬───────────────────────────────────────────┤
//! │ sidebar      │ main view                     ┌ queue ───┐ │
//! │ 216px        │                               │ panel    │ │
//! │              │                               └──────────┘ │
//! ├──────────────┴───────────────────────────────────────────┤
//! │ player bar: seek row + transport + volume                │
//! └──────────────────────────────────────────────────────────┘
//! ```
//!
//! All functions are immediate mode: `fn(ui, state, actions)` lays out and paints
//! this frame. They never block, never touch the backend, and never retain
//! widgets. Persistent UI state that has to survive frames (sidebar width, menu
//! identities, text-edit buffers) lives in `State`/`Ui`.
//!
//! Ownership: `mod.rs`, `home.rs`, `chrome/` (titlebar, sidebar, player bar,
//! queue panel wrapper, toast/banner wrappers) and `cards/` (shared widget
//! primitives) are `screens-chrome`'s; `pages.rs` (the routed views + queue panel
//! contents beyond the header) is `screens-pages`'s.

pub mod album;
pub mod artist;
pub mod cards;
pub mod chrome;
pub mod home;
pub mod liked;
pub mod menu;
pub mod overlays;
pub mod pages;
pub mod playlist;
pub mod queue;
pub mod search;
pub mod settings;
pub mod track_list;

use crate::gfx::paint::Rect;
use crate::state::{Action, State, View};
use crate::theme::Theme;
use crate::ui::{Id, Padding, Ui, SCROLLBAR_WIDTH};

/// The fixed window bands, in logical pixels, for the current state.
///
/// The layout mirrors the old CSS flex column exactly:
/// `titlebar (46) / body (sidebar + content) / player (84)`, where `content` is
/// split into the scrolling `main` area and the optional queue panel docked
/// above the player bar.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Layout {
    pub viewport: Rect,
    pub titlebar: Rect,
    pub body: Rect,
    pub sidebar: Rect,
    pub content: Rect,
    pub main: Rect,
    pub queue: Option<Rect>,
    pub player: Rect,
}

/// Whether the queue panel is visible at all: it is hidden on Settings (old
/// `if *view != View::Settings`) and when the queue is empty (`return html! {}`).
pub fn queue_visible(state: &State) -> bool {
    !matches!(state.view, View::Settings) && !state.queue.tracks.is_empty()
}

/// Height the queue panel occupies (0 when hidden). Collapsed = the header only.
pub fn queue_height(ui: &Ui, state: &State) -> f32 {
    if !queue_visible(state) {
        return 0.0;
    }
    let theme = ui.theme();
    // A very short window still needs room for the main view; the old flex layout
    // would let the panel push `main` down to its min-height (0), so clamp the
    // panel instead of producing negative rectangles.
    let body_h = ui.viewport().h.max(0.0) - theme.titlebar_height - theme.player_height;
    let wanted = if collapse_requested() {
        theme.queue_collapsed_height
    } else {
        theme.queue_height
    };
    wanted.min((body_h - 120.0).max(theme.queue_collapsed_height))
}

/// Queue-panel collapse toggle. The old UI kept this in the `QueuePanel`
/// component; the native port keeps it in `queue.rs` (`Ui`-owned popup-like
/// state), so the main area grows the instant the header is clicked.
fn collapse_requested() -> bool {
    queue::collapsed()
}

/// Compute the window layout for this frame. Every screen derives its rectangles
/// from here, so the bands and the gutter can never drift apart.
pub fn layout(ui: &Ui, state: &State) -> Layout {
    let theme = ui.theme();
    let viewport = ui.viewport();
    let titlebar = Rect::new(0.0, 0.0, viewport.w, theme.titlebar_height);
    let player = Rect::new(
        0.0,
        viewport.h - theme.player_height,
        viewport.w,
        theme.player_height,
    );
    let body = Rect::new(
        0.0,
        titlebar.h,
        viewport.w,
        (player.y - titlebar.h).max(0.0),
    );
    let sidebar_w = state
        .sidebar_width
        .clamp(theme.sidebar_width_min, theme.sidebar_width_max);
    let sidebar = Rect::new(0.0, body.y, sidebar_w, body.h);
    let content = Rect::new(
        sidebar.right(),
        body.y,
        (viewport.w - sidebar.w).max(0.0),
        body.h,
    );
    let qh = queue_height(ui, state);
    let main = Rect::new(content.x, content.y, content.w, (content.h - qh).max(0.0));
    let queue = if qh > 0.0 {
        Some(Rect::new(content.x, main.bottom(), content.w, qh))
    } else {
        None
    };
    Layout {
        viewport,
        titlebar,
        body,
        sidebar,
        content,
        main,
        queue,
        player,
    }
}

/// The content column of `main`: `main` minus its `20px 24px` padding and the
/// permanently reserved 10 px scrollbar gutter (`scrollbar-gutter: stable`).
pub fn main_content_rect(ui: &Ui, main: Rect) -> Rect {
    let theme = ui.theme();
    let pad = Padding {
        left: theme.main_padding_h,
        right: theme.main_padding_h + SCROLLBAR_WIDTH,
        top: theme.main_padding_v,
        bottom: theme.main_padding_v,
    };
    main.inset_pad(pad)
}

/// Draw the whole window for this frame. `ui.begin_frame` has already been called
/// and the caller will take the paint list when this returns.
pub fn root(ui: &mut Ui, state: &mut State, actions: &mut Vec<Action>) {
    let l = layout(ui, state);
    let theme: Theme = ui.theme().clone();

    // Paint order follows the DOM: titlebar, body (sidebar + main + queue),
    // player bar, then the overlays.
    ui.fill(l.viewport, theme.bg);

    // Window-anchored overlays (toast / update banner) interact *first* even
    // though they paint last: immediate-mode input is first-come-first-served,
    // so a press over a banner button would otherwise be latched by the queue
    // row or player control painted underneath it. They draw into the UI's
    // deferred buffer, so the paint order is still "above every band".
    chrome::toast(ui, state, l.viewport);
    chrome::update_banner(ui, state, actions, l.viewport);

    // ---- main view ----
    ui.fill(l.main, theme.bg);
    // Context menus (screens-pages' `menu.rs`) consume the click that dismissed
    // them before the content below is drawn, and paint deferred above all bands.
    // The layer is called again at the end of the frame when the content opened
    // a menu *this* frame (`Ui::end_frame` drops a popup that was never drawn).
    let menu_was_open = menu::is_open();
    let _ = menu::layer(ui, actions);
    let view = state.view.clone();
    let main_theme = theme.clone();
    match view {
        View::Home => {
            // Home scrolls like the old `.main`: a stable 10 px gutter (so the
            // card grid never reflows when the queue panel opens) plus wheel and
            // scrollbar-drag handling from the framework's scroll container.
            let _ =
                ui.scroll_area_stable_dynamic(Id::from_str("main/home"), l.main, |ui, content| {
                    let inner = Rect::new(
                        content.x + main_theme.main_padding_h,
                        content.y + main_theme.main_padding_v,
                        (content.w - main_theme.main_padding_h * 2.0).max(0.0),
                        (content.h - main_theme.main_padding_v * 2.0).max(0.0),
                    );
                    let body = home::home(ui, state, actions, inner);
                    ((), main_theme.main_padding_v * 2.0 + body)
                });
        }
        _ => {
            let mut handled = false;
            ui.with_clip(l.main, |ui| {
                handled = pages::page(ui, state, actions);
            });
            if !handled {
                // A page that is not implemented yet falls back to the home view
                // so the window never renders an empty main area.
                let content = main_content_rect(ui, l.main);
                ui.with_clip(l.main, |ui| {
                    let _ = home::home(ui, state, actions, content);
                });
            }
        }
    }

    // ---- queue panel ----
    if let Some(queue) = l.queue {
        chrome::queue_panel(ui, state, actions, queue);
    }

    // ---- chrome ----
    chrome::titlebar(ui, state, actions, l.titlebar);
    chrome::sidebar(ui, state, actions, l.sidebar);
    chrome::player_bar(ui, state, actions, l.player);

    // A menu opened by this frame's content/chrome must be drawn now: the UI
    // auto-dismisses popups that were opened but never drawn before `end_frame`.
    if !menu_was_open && menu::is_open() {
        let _ = menu::layer(ui, actions);
    }

    // Run last: an open menu's inert backdrop keeps the arrow even when content
    // drawn after the menu layer raised the hand (the menu layer runs early in
    // the frame to consume the outside click).
    menu::pin_backdrop_cursor(ui);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gfx::paint::Rect;

    #[test]
    fn queue_is_hidden_on_settings_and_when_empty() {
        let mut state = State::default();
        assert!(!queue_visible(&state));
        state.queue.tracks.push(rift_types::Track {
            id: "a".into(),
            title: "A".into(),
            artist: String::new(),
            album: None,
            duration: None,
            cover: String::new(),
            artists: Vec::new(),
            album_id: None,
        });
        assert!(queue_visible(&state));
        state.view = View::Settings;
        assert!(!queue_visible(&state));
    }

    #[test]
    fn main_content_reserves_the_scrollbar_gutter() {
        let ui = Ui::new();
        let theme = ui.theme().clone();
        let main = Rect::new(335.0, 46.0, 825.0, 375.0);
        let content = main_content_rect(&ui, main);
        assert_eq!(content.x, 335.0 + theme.main_padding_h);
        assert_eq!(
            content.right(),
            335.0 + 825.0 - theme.main_padding_h - SCROLLBAR_WIDTH
        );
        assert_eq!(content.w, 767.0); // x=359..1125 inclusive
    }

    #[test]
    fn reference_layout_bands() {
        // 1160x740, sidebar 335 (preview screenshot): the exact band edges from
        // Previews/image.png.
        let mut ui = Ui::new();
        let state = State {
            sidebar_width: 335.0,
            view: View::Home,
            queue: rift_types::QueueSnapshot {
                tracks: (0..6)
                    .map(|i| rift_types::Track {
                        id: format!("p{i}"),
                        title: format!("Song Title {}", i + 1),
                        artist: "Artist Name".into(),
                        album: Some("Album Name".into()),
                        duration: Some(187),
                        cover: String::new(),
                        artists: Vec::new(),
                        album_id: None,
                    })
                    .collect(),
                ..Default::default()
            },
            ..State::default()
        };
        // begin_frame is what sets the viewport.
        let input = crate::ui::InputState {
            viewport: Rect::new(0.0, 0.0, 1160.0, 740.0),
            ..Default::default()
        };
        ui.begin_frame(input, Rect::new(0.0, 0.0, 1160.0, 740.0));
        let l = layout(&ui, &state);
        let _ = ui.end_frame();
        assert_eq!(l.titlebar, Rect::new(0.0, 0.0, 1160.0, 46.0));
        assert_eq!(l.player, Rect::new(0.0, 656.0, 1160.0, 84.0));
        assert_eq!(l.body, Rect::new(0.0, 46.0, 1160.0, 610.0));
        assert_eq!(l.sidebar, Rect::new(0.0, 46.0, 335.0, 610.0));
        assert_eq!(l.content, Rect::new(335.0, 46.0, 825.0, 610.0));
        assert_eq!(l.main, Rect::new(335.0, 46.0, 825.0, 375.0));
        assert_eq!(l.queue, Some(Rect::new(335.0, 421.0, 825.0, 235.0)));
    }

    /// Run one real frame of the Home preview and assert the paint list contains
    /// the reference-screenshot geometry (ui-spec.md §13). This is the no-GPU
    /// guard for the pixel pass: if a band, card or player element moves, this
    /// fails with the expected rectangle.
    #[test]
    fn home_preview_paints_the_reference_geometry() {
        use crate::gfx::paint::Prim;

        let mut state = crate::preview::preview_state();
        state.view = View::Home;
        state.sidebar_width = 335.0;

        let mut ui = Ui::new();
        let viewport = Rect::new(0.0, 0.0, 1160.0, 740.0);
        // The reference screenshot has the pointer over the first recent row.
        let input = crate::ui::InputState {
            viewport,
            mouse_pos: (700.0, 407.0),
            mouse_in_window: true,
            time: 1.0,
            ..Default::default()
        };
        ui.begin_frame(input, viewport);
        let mut actions = Vec::new();
        root(&mut ui, &mut state, &mut actions);
        let paint = ui.end_frame();

        let mut rects: Vec<(Rect, crate::gfx::paint::Color, f32)> = Vec::new();
        let mut images: Vec<(Rect, crate::gfx::paint::TextureId)> = Vec::new();
        for layer in &paint.layers {
            for prim in &layer.prims {
                match prim {
                    Prim::Rect {
                        rect,
                        color,
                        radius,
                        ..
                    } => rects.push((*rect, *color, *radius)),
                    Prim::Image { rect, texture, .. } => images.push((*rect, *texture)),
                }
            }
        }
        let has_rect = |x: f32, y: f32, w: f32, h: f32| {
            rects.iter().any(|(r, _, _)| {
                (r.x - x).abs() <= 1.01
                    && (r.y - y).abs() <= 1.01
                    && (r.w - w).abs() <= 1.01
                    && (r.h - h).abs() <= 1.01
            })
        };
        let has_image = |x: f32, y: f32, w: f32, h: f32| {
            images.iter().any(|(r, _)| {
                (r.x - x).abs() <= 1.01
                    && (r.y - y).abs() <= 1.01
                    && (r.w - w).abs() <= 1.01
                    && (r.h - h).abs() <= 1.01
            })
        };

        // Bands.
        assert!(has_rect(0.0, 0.0, 1160.0, 46.0), "titlebar band");
        assert!(has_rect(0.0, 46.0, 335.0, 610.0), "sidebar band");
        assert!(has_rect(335.0, 46.0, 825.0, 375.0), "main band");
        assert!(has_rect(335.0, 421.0, 825.0, 235.0), "queue band");
        assert!(has_rect(0.0, 656.0, 1160.0, 84.0), "player band");
        // Titlebar: search box bg + accent button + 22px logo image.
        assert!(has_rect(340.0, 7.5, 480.0, 31.0), "search box fill");
        assert!(has_rect(740.0, 8.5, 79.0, 29.0), "search button");
        // The logo texture is a 32 px canvas (22 px tile + 5 px glow margin).
        assert!(has_image(9.0, 7.0, 32.0, 32.0), "logo mark");
        // Home: 4 cards' 158px covers at y=112 (369/565/761/957).
        for x in [369.0, 565.0, 761.0, 957.0] {
            assert!(has_image(x, 112.0, 158.0, 158.0), "card cover at x={x}");
        }
        // Player: 44px np cover, 36px white play circle at 562..598.
        assert!(has_image(16.0, 684.0, 44.0, 44.0), "np cover");
        assert!(has_rect(562.0, 688.0, 36.0, 36.0), "play button circle");
        // Queue: 34px covers at x=351 starting y=471.
        assert!(has_image(351.0, 471.0, 34.0, 34.0), "queue cover 1");

        // The generated app textures are the source for the placeholder tiles
        // (their one-time upload lives in a process-wide static, so assert on the
        // drawn texture id rather than on `PaintList::created`).
        assert!(
            images
                .iter()
                .any(|(r, tex)| *tex == cards::COVER_FALLBACK_TEX
                    && (r.x - 369.0).abs() <= 1.01
                    && (r.y - 112.0).abs() <= 1.01),
            "card cover uses the generated fallback texture"
        );
        assert!(
            images.iter().any(|(r, tex)| *tex == cards::LOGO_TEX
                && (r.x - 9.0).abs() <= 1.01
                && (r.y - 7.0).abs() <= 1.01),
            "titlebar logo uses the generated logo texture"
        );

        // Recently played row 1 is hovered (pointer 700,407): raised bg 359..1125.
        assert!(
            has_rect(359.0, 386.0, 767.0, 54.0),
            "hovered recent row background"
        );
        // Accent seek thumb: 16px circle whose centre is 58+8+0.3878*1028.
        let thumb_x = 58.0 + 8.0 + (83.0 / 214.0) * (1044.0 - 16.0);
        assert!(
            rects.iter().any(|(r, c, radius)| {
                (r.center().0 - thumb_x).abs() < 0.6
                    && (r.center().1 - 670.5).abs() < 0.6
                    && *radius >= 7.9
                    && *c == ui.theme().accent
            }),
            "accent seek thumb at {thumb_x}"
        );
    }
}
