//! Left navigation sidebar: library links, the reorderable playlist list, and
//! import/settings actions.

use wasm_bindgen::closure::Closure;
use wasm_bindgen::JsCast;
use web_sys::{DragEvent, HtmlInputElement};
use yew::prelude::*;

use super::icons::{eq_bars, icon};
use super::menu::{Menu, MenuAction};
use super::reorder::use_reorder;
use super::View;

#[derive(Properties, PartialEq)]
pub struct SidebarProps {
    pub view: View,
    /// (id, name, track count)
    pub playlists: Vec<(String, String, usize)>,
    pub on_nav: Callback<View>,
    pub on_new_playlist: Callback<String>,
    /// Import a playlist from a pasted YouTube Music link or ID.
    pub on_import_playlist: Callback<String>,
    /// Import a playlist from a Rift JSON file (opens a native picker).
    pub on_import_file: Callback<()>,
    pub on_rename_playlist: Callback<String>,
    pub on_delete_playlist: Callback<String>,
    /// Export a playlist (by id) to a JSON file.
    pub on_export_playlist: Callback<String>,
    /// Reorder: move the playlist at the first index to the second.
    pub on_move_playlist: Callback<(usize, usize)>,
    /// Source tag of the playing queue ("liked", "playlist:<id>").
    pub playing_source: Option<String>,
    pub is_playing: bool,
}

/// Sidebar width bounds and default (px) for the drag-resize handle.
const SIDEBAR_MIN_W: f64 = 160.0;
const SIDEBAR_MAX_W: f64 = 420.0;
const SIDEBAR_DEFAULT_W: f64 = 216.0;
const SIDEBAR_W_KEY: &str = "sidebar-width";

fn local_storage() -> Option<web_sys::Storage> {
    web_sys::window().and_then(|w| w.local_storage().ok().flatten())
}

#[function_component(Sidebar)]
pub fn sidebar(props: &SidebarProps) -> Html {
    let creating = use_state(|| false);
    let input = use_node_ref();
    let importing = use_state(|| false);
    let import_ref = use_node_ref();
    // Drag-to-reorder for the playlist list (shared with the queue panel). The
    // 3px margin matches the rows' margin-bottom.
    let reorder = use_reorder(
        props
            .playlists
            .iter()
            .map(|(id, _, _)| id.clone())
            .collect(),
        3,
        props.on_move_playlist.clone(),
    );

    // Drag-resizable width, restored from the previous session.
    let width = use_state(|| {
        local_storage()
            .and_then(|s| s.get_item(SIDEBAR_W_KEY).ok().flatten())
            .and_then(|v| v.parse::<f64>().ok())
            .map(|w| w.clamp(SIDEBAR_MIN_W, SIDEBAR_MAX_W))
            .unwrap_or(SIDEBAR_DEFAULT_W)
    });
    // Live mirror of `width` for the document-level listeners below, which are
    // created once on mount and would otherwise read a stale state snapshot.
    let width_ref = use_mut_ref(|| *width);
    let resizing = use_mut_ref(|| false);
    {
        // Document-level listeners so the drag keeps tracking the pointer even
        // when it leaves the thin handle.
        let width = width.clone();
        let width_ref = width_ref.clone();
        let resizing = resizing.clone();
        use_effect_with((), move |_| {
            let document = web_sys::window().unwrap().document().unwrap();
            let mousemove = Closure::<dyn FnMut(MouseEvent)>::new({
                let width = width.clone();
                let width_ref = width_ref.clone();
                let resizing = resizing.clone();
                move |e: MouseEvent| {
                    if *resizing.borrow() {
                        e.prevent_default();
                        // The sidebar starts at the window's left edge, so the
                        // pointer's x position is the desired width.
                        let w = (e.client_x() as f64).clamp(SIDEBAR_MIN_W, SIDEBAR_MAX_W);
                        *width_ref.borrow_mut() = w;
                        width.set(w);
                    }
                }
            });
            let mouseup = Closure::<dyn FnMut(MouseEvent)>::new({
                move |_: MouseEvent| {
                    if *resizing.borrow() {
                        *resizing.borrow_mut() = false;
                        if let Some(s) = local_storage() {
                            let _ = s.set_item(SIDEBAR_W_KEY, &width_ref.borrow().to_string());
                        }
                    }
                }
            });
            document
                .add_event_listener_with_callback("mousemove", mousemove.as_ref().unchecked_ref())
                .ok();
            document
                .add_event_listener_with_callback("mouseup", mouseup.as_ref().unchecked_ref())
                .ok();
            // Live for the lifetime of the app, like the other global listeners.
            mousemove.forget();
            mouseup.forget();
            || {}
        });
    }
    let start_resize = {
        let resizing = resizing.clone();
        Callback::from(move |e: MouseEvent| {
            e.prevent_default();
            *resizing.borrow_mut() = true;
        })
    };

    let nav = |view: View| {
        let on_nav = props.on_nav.clone();
        Callback::from(move |_: MouseEvent| on_nav.emit(view.clone()))
    };

    let start_create = {
        let creating = creating.clone();
        Callback::from(move |_: MouseEvent| creating.set(true))
    };
    // Read the draft name and create the playlist if it isn't blank.
    let submit_create = {
        let creating = creating.clone();
        let input = input.clone();
        let on_new = props.on_new_playlist.clone();
        move || {
            if let Some(el) = input.cast::<HtmlInputElement>() {
                let name = el.value();
                if !name.trim().is_empty() {
                    on_new.emit(name.trim().to_string());
                }
            }
            creating.set(false);
        }
    };
    let onkeydown = {
        let submit = submit_create.clone();
        let creating = creating.clone();
        Callback::from(move |e: KeyboardEvent| match e.key().as_str() {
            "Enter" => submit(),
            "Escape" => creating.set(false),
            _ => {}
        })
    };
    let confirm_create = {
        let submit = submit_create.clone();
        Callback::from(move |_: MouseEvent| submit())
    };
    let cancel_create = {
        let creating = creating.clone();
        Callback::from(move |_: MouseEvent| creating.set(false))
    };

    let start_import = {
        let importing = importing.clone();
        Callback::from(move |_: MouseEvent| importing.set(true))
    };
    // Read the pasted link/ID and kick off the import if it isn't blank.
    let submit_import = {
        let importing = importing.clone();
        let import_ref = import_ref.clone();
        let on_import = props.on_import_playlist.clone();
        move || {
            if let Some(el) = import_ref.cast::<HtmlInputElement>() {
                let url = el.value();
                if !url.trim().is_empty() {
                    on_import.emit(url.trim().to_string());
                }
            }
            importing.set(false);
        }
    };
    let import_keydown = {
        let submit = submit_import.clone();
        let importing = importing.clone();
        Callback::from(move |e: KeyboardEvent| match e.key().as_str() {
            "Enter" => submit(),
            "Escape" => importing.set(false),
            _ => {}
        })
    };
    let confirm_import = {
        let submit = submit_import.clone();
        Callback::from(move |_: MouseEvent| submit())
    };
    let cancel_import = {
        let importing = importing.clone();
        Callback::from(move |_: MouseEvent| importing.set(false))
    };
    // Stop the input from blurring (which would cancel) before a button's click
    // lands, so the check/X buttons act on the still-focused input.
    let keep_focus = Callback::from(|e: MouseEvent| e.prevent_default());

    let item_class = |active: bool| classes!("side-item", active.then_some("active"));

    // Leading icon for a collection: the animated equalizer while it plays,
    // otherwise the given icon.
    let lead = |source: &str, icon_name: &str| {
        if props.playing_source.as_deref() == Some(source) {
            eq_bars(props.is_playing)
        } else {
            icon(icon_name)
        }
    };

    html! {
        <nav class="sidebar" style={format!("width:{}px", *width)}>
            <div class="side-resizer" onmousedown={start_resize}></div>
            <div class="side-scroll">
                <div class={item_class(props.view == View::Home)} onclick={nav(View::Home)}>
                    { icon("home") }<span>{ "Home" }</span>
                </div>
                <div class={item_class(props.view == View::Liked)} onclick={nav(View::Liked)}>
                    { lead("liked", "heart") }<span class="side-name">{ "Liked Songs" }</span>
                </div>

                <div class="side-head">{ "PLAYLISTS" }</div>
                <div class={classes!("side-pl-list", reorder.reordering.then_some("reordering"), reorder.hover_calm.then_some("hover-calm"))}
                     ref={reorder.list_ref.clone()} ondragenter={reorder.dragover.clone()}
                     ondragover={reorder.dragover.clone()} ondrop={reorder.drop.clone()}
                     onmousemove={reorder.calm_clear.clone()}>
                { for props.playlists.iter().enumerate().map(|(i, (id, name, count))| html! {
                    <SidebarPlaylist
                        id={id.clone()}
                        name={name.clone()}
                        count={*count}
                        active={props.view == View::Playlist(id.clone())}
                        playing={props.playing_source.as_deref() == Some(format!("playlist:{id}").as_str())}
                        is_playing={props.is_playing}
                        dragging={reorder.dragging(i)}
                        shift={reorder.shift(i)}
                        on_dragstart={reorder.dragstart(i)}
                        on_dragend={reorder.dragend()}
                        on_nav={props.on_nav.clone()}
                        on_rename={props.on_rename_playlist.clone()}
                        on_delete={props.on_delete_playlist.clone()}
                        on_export={props.on_export_playlist.clone()}
                    />
                }) }
                </div>

                if *creating {
                    <div class="side-new-row">
                        <input class="side-new-input" ref={input} type="text"
                               placeholder="Playlist name" autofocus=true {onkeydown}
                               onblur={ let c = creating.clone(); Callback::from(move |_| c.set(false)) } />
                        <button class="ibtn side-new-btn confirm" title="Create"
                                onmousedown={keep_focus.clone()} onclick={confirm_create}>
                            { icon("check") }
                        </button>
                        <button class="ibtn side-new-btn cancel" title="Cancel"
                                onmousedown={keep_focus.clone()} onclick={cancel_create}>
                            { icon("x") }
                        </button>
                    </div>
                } else {
                    <div class="side-item side-new" onclick={start_create}>
                        { icon("plus") }<span>{ "New playlist" }</span>
                    </div>
                }
            </div>

            // Import actions pinned to the bottom, above the Settings divider.
            if *importing {
                <div class="side-new-row">
                    <input class="side-new-input" ref={import_ref} type="text"
                           placeholder="YouTube Music link or ID" autofocus=true onkeydown={import_keydown}
                           onblur={ let i = importing.clone(); Callback::from(move |_| i.set(false)) } />
                    <button class="ibtn side-new-btn confirm" title="Import"
                            onmousedown={keep_focus.clone()} onclick={confirm_import}>
                        { icon("check") }
                    </button>
                    <button class="ibtn side-new-btn cancel" title="Cancel"
                            onmousedown={keep_focus} onclick={cancel_import}>
                        { icon("x") }
                    </button>
                </div>
            } else {
                <div class="side-item side-new" onclick={start_import}>
                    { icon("download") }<span>{ "Import playlist" }</span>
                </div>
            }

            <div class="side-item side-new" onclick={
                let cb = props.on_import_file.clone();
                Callback::from(move |_: MouseEvent| cb.emit(()))
            }>
                { icon("album") }<span>{ "Import from file" }</span>
            </div>

            <div class="side-divider" />
            <div class={classes!("side-item", "side-settings", (props.view == View::Settings).then_some("active"))}
                 onclick={nav(View::Settings)}>
                { icon("settings") }<span>{ "Settings" }</span>
            </div>
        </nav>
    }
}

#[derive(Properties, PartialEq)]
struct SidebarPlaylistProps {
    id: String,
    name: String,
    count: usize,
    active: bool,
    /// This playlist is the source of the playing queue.
    playing: bool,
    /// Playback is active (vs paused), for the equalizer animation.
    is_playing: bool,
    /// This row is being dragged to a new position.
    dragging: bool,
    /// Inline transform sliding this row during a reorder drag's live preview.
    shift: Option<String>,
    on_dragstart: Callback<DragEvent>,
    on_dragend: Callback<DragEvent>,
    on_nav: Callback<View>,
    on_rename: Callback<String>,
    on_delete: Callback<String>,
    on_export: Callback<String>,
}

#[function_component(SidebarPlaylist)]
fn sidebar_playlist(props: &SidebarPlaylistProps) -> Html {
    let menu_open = use_state(|| false);

    let nav = {
        let cb = props.on_nav.clone();
        let id = props.id.clone();
        Callback::from(move |_: MouseEvent| cb.emit(View::Playlist(id.clone())))
    };
    let context = {
        let menu_open = menu_open.clone();
        Callback::from(move |e: MouseEvent| {
            e.prevent_default();
            menu_open.set(true);
        })
    };
    let toggle = {
        let menu_open = menu_open.clone();
        Callback::from(move |e: MouseEvent| {
            e.stop_propagation();
            menu_open.set(!*menu_open);
        })
    };
    let close = {
        let menu_open = menu_open.clone();
        Callback::from(move |_| menu_open.set(false))
    };

    let lead = if props.playing {
        eq_bars(props.is_playing)
    } else {
        icon("music")
    };

    let actions = vec![
        MenuAction::Item {
            icon: "edit",
            label: "Rename".into(),
            danger: false,
            cb: {
                let cb = props.on_rename.clone();
                let id = props.id.clone();
                Callback::from(move |_| cb.emit(id.clone()))
            },
        },
        MenuAction::Item {
            icon: "download",
            label: "Export".into(),
            danger: false,
            cb: {
                let cb = props.on_export.clone();
                let id = props.id.clone();
                Callback::from(move |_| cb.emit(id.clone()))
            },
        },
        MenuAction::Item {
            icon: "trash",
            label: "Delete".into(),
            danger: true,
            cb: {
                let cb = props.on_delete.clone();
                let id = props.id.clone();
                Callback::from(move |_| cb.emit(id.clone()))
            },
        },
    ];

    html! {
        <div class={classes!("side-item", props.active.then_some("active"), (*menu_open).then_some("menu-open"), props.dragging.then_some("dragging"))}
             style={props.shift.clone()}
             draggable="true" onclick={nav} oncontextmenu={context}
             ondragstart={props.on_dragstart.clone()} ondragend={props.on_dragend.clone()}>
            { lead }
            <span class="side-name">{ &props.name }</span>
            <span class="side-count">{ props.count }</span>
            <div class="menu-anchor side-kebab">
                <button class="ibtn" title="More" onclick={toggle}>{ icon("kebab") }</button>
                <Menu open={*menu_open} on_close={close} actions={actions} align_right=true />
            </div>
        </div>
    }
}
