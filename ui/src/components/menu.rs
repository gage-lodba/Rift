//! Context / kebab popup menu and its trigger button.

use wasm_bindgen::JsCast;
use yew::prelude::*;

use super::icons::icon;

/// One entry in a kebab / right-click menu.
#[derive(Clone, PartialEq)]
pub enum MenuAction {
    /// A plain action item.
    Item {
        icon: &'static str,
        label: String,
        danger: bool,
        cb: Callback<()>,
    },
    /// An item that expands into a list of options (e.g. "Add to playlist").
    /// `cb` receives the chosen option's id.
    Sub {
        icon: &'static str,
        label: String,
        options: Vec<(String, String)>,
        cb: Callback<String>,
    },
    Separator,
}

#[derive(Properties, PartialEq)]
pub struct MenuProps {
    pub open: bool,
    pub on_close: Callback<()>,
    pub actions: Vec<MenuAction>,
    /// Align the popup to the right edge of its anchor.
    #[prop_or_default]
    pub align_right: bool,
    /// The element that opened this menu, so focus can go back to it when the
    /// menu closes. Without restoring focus, dismissing a menu with Escape drops
    /// a keyboard user at the top of the document.
    #[prop_or_default]
    pub anchor: Option<NodeRef>,
}

/// A controlled popup menu: render it inside a `.menu-anchor` and drive `open`
/// from the parent (so both a kebab button and a right-click can open it).
///
/// Operable by keyboard: `role="menu"`/`menuitem`, focus moves to the first item
/// on open, Up/Down (and Home/End) walk the items, Left/Right and Enter expand a
/// submenu, Escape closes the whole menu and restores focus to its anchor, and
/// focus leaving the popup (Tab) closes it too. Pointer behaviour is unchanged.
///
/// **Every key the popup handles stops propagating.** A menu is rendered inside
/// the row that opened it, and those rows are `role="button"` containers that
/// activate on Enter/Space. Their guard (`key_on_nested_control`) recognizes
/// tag-based controls only, and a menu item is a `div` with a role — so without
/// the stops, Enter on "Play next" ran the action *and* started the track. The
/// rule is stated here rather than at each handler because the next item kind
/// added has to follow it.
#[function_component(Menu)]
pub fn menu(props: &MenuProps) -> Html {
    let expanded = use_state(|| None::<usize>);
    let items = use_node_ref();
    {
        // Collapse any expanded submenu whenever the menu closes.
        let expanded = expanded.clone();
        use_effect_with(props.open, move |open| {
            if !*open {
                expanded.set(None);
            }
            || ()
        });
    }
    {
        // On open, move focus into the menu so a keyboard user is inside it
        // rather than still on the trigger with the menu unreachable.
        let items = items.clone();
        use_effect_with(props.open, move |open| {
            if *open {
                if let Some(first) = menu_items(&items).into_iter().next() {
                    let _ = first.focus();
                }
            }
            || ()
        });
    }
    // Close the popup and hand focus back to whatever opened it. Escape and an
    // activated item both need this: the menu unmounts, so the element that had
    // focus goes with it and a keyboard user is otherwise dropped at the top of
    // the document.
    let close_and_restore = {
        let close = props.on_close.clone();
        let anchor = props.anchor.clone();
        Callback::from(move |_: ()| {
            close.emit(());
            if let Some(node) = anchor
                .as_ref()
                .and_then(NodeRef::cast::<web_sys::HtmlElement>)
            {
                let _ = node.focus();
            }
        })
    };
    // Escape closes from anywhere in the popup, including a submenu.
    let onkeydown = {
        let close_and_restore = close_and_restore.clone();
        let items = items.clone();
        let expanded = expanded.clone();
        // Every arm that handles a key stops it there (see the component doc):
        // nothing above the popup has any business with a key aimed inside it.
        Callback::from(move |e: KeyboardEvent| match e.key().as_str() {
            "Escape" => {
                e.prevent_default();
                e.stop_propagation();
                close_and_restore.emit(());
            }
            "ArrowDown" | "ArrowUp" | "Home" | "End" => {
                e.prevent_default();
                e.stop_propagation();
                let list = menu_items(&items);
                if list.is_empty() {
                    return;
                }
                let current = list
                    .iter()
                    .position(|el| el.is_same_node(Some(&active_element())));
                let next = match e.key().as_str() {
                    "Home" => 0,
                    "End" => list.len() - 1,
                    "ArrowDown" => current.map_or(0, |i| (i + 1) % list.len()),
                    _ => current.map_or(list.len() - 1, |i| (i + list.len() - 1) % list.len()),
                };
                let _ = list[next].focus();
            }
            "ArrowRight" => {
                // Expand the focused submenu, if it is one.
                if let Some(index) = focused_submenu(&items) {
                    e.prevent_default();
                    e.stop_propagation();
                    expanded.set(Some(index));
                }
            }
            "ArrowLeft" if expanded.is_some() => {
                e.prevent_default();
                e.stop_propagation();
                // Collapsing unmounts the options, and removing the focused
                // element drops focus to the body — which would also close this
                // menu, since focus leaving is what closes it now. Hand focus
                // back to the submenu's own header first. A no-op when the
                // header itself has focus, which is the other way in.
                focus_open_submenu_header(&items);
                expanded.set(None);
            }
            _ => {}
        })
    };
    if !props.open {
        return html! {};
    }
    let close = props.on_close.clone();
    let backdrop_close = close.clone();
    // Focus leaving the popup closes it. Escape alone only works while focus is
    // still *inside* the menu: tab past the last item and the menu used to stay
    // open with no keyboard way out, since its backdrop answers a pointer only.
    // This does not move focus — by the time `focusout` fires, Tab's own default
    // action has already put it where the user asked.
    let onfocusout = {
        let close = props.on_close.clone();
        let items = items.clone();
        Callback::from(move |e: FocusEvent| {
            let root = items.cast::<web_sys::Node>();
            let next = e
                .related_target()
                .and_then(|t| t.dyn_into::<web_sys::Node>().ok());
            // No related target means focus went nowhere (the window lost it, or
            // the focused element was removed) — that counts as leaving too.
            let still_inside = match (root.as_ref(), next.as_ref()) {
                (Some(root), Some(next)) => root.contains(Some(next)),
                _ => false,
            };
            if !still_inside {
                close.emit(());
            }
        })
    };

    html! {
        <>
            <div class="menu-backdrop"
                 onclick={Callback::from(move |e: MouseEvent| { e.stop_propagation(); backdrop_close.emit(()); })}
                 oncontextmenu={let c = close.clone(); Callback::from(move |e: MouseEvent| { e.prevent_default(); e.stop_propagation(); c.emit(()); })}>
            </div>
            <div class={classes!("menu", props.align_right.then_some("menu-right"))}
                 role="menu" tabindex="-1" ref={items} {onkeydown} {onfocusout}
                 onclick={|e: MouseEvent| e.stop_propagation()}>
                { for props.actions.iter().enumerate().map(|(i, a)| {
                    match a {
                        MenuAction::Separator => html! { <div class="menu-sep" role="separator"></div> },
                        MenuAction::Item { icon: ic, label, danger, cb } => {
                            let activate = {
                                let cb = cb.clone();
                                // Activating closes the menu, and the popup
                                // unmounting takes the activated item with it:
                                // without the focus restore, choosing an entry
                                // with Enter drops focus at the top of the
                                // document (the review's Escape finding, one
                                // step further).
                                let close_and_restore = close_and_restore.clone();
                                Callback::from(move |_: ()| {
                                    cb.emit(());
                                    close_and_restore.emit(());
                                })
                            };
                            let onclick = {
                                let activate = activate.clone();
                                Callback::from(move |_: MouseEvent| activate.emit(()))
                            };
                            // A `div` with a role is focusable and Enter/Space
                            // activate it, without the default button chrome.
                            //
                            // `stop_propagation` is not decoration: a menu is
                            // rendered *inside* the row that opened it, and rows
                            // activate on Enter/Space themselves. Their guard
                            // (`key_on_nested_control`) recognizes tag-based
                            // controls only, and a menu item is a `div` — so
                            // without this, Enter on "Play next" ran the action
                            // *and* started the track. The key belongs to the
                            // control it was aimed at.
                            let onkeydown = Callback::from(move |e: KeyboardEvent| {
                                if e.key() == "Enter" || e.key() == " " {
                                    e.prevent_default();
                                    e.stop_propagation();
                                    activate.emit(());
                                }
                            });
                            html! {
                                <div class={classes!("menu-item", danger.then_some("danger"))}
                                     role="menuitem" tabindex="0" {onclick} {onkeydown}>
                                    { icon(ic) }<span>{ label }</span>
                                </div>
                            }
                        }
                        MenuAction::Sub { icon: ic, label, options, cb } => {
                            let is_open = *expanded == Some(i);
                            let toggle = {
                                let expanded = expanded.clone();
                                Callback::from(move |_: MouseEvent| {
                                    expanded.set(if is_open { None } else { Some(i) });
                                })
                            };
                            let onkeydown = {
                                let expanded = expanded.clone();
                                Callback::from(move |e: KeyboardEvent| {
                                    if e.key() == "Enter" || e.key() == " " {
                                        e.prevent_default();
                                        // Same reason as a plain item below:
                                        // expanding is this control's action, and
                                        // the host row must not also activate.
                                        e.stop_propagation();
                                        expanded.set(if is_open { None } else { Some(i) });
                                    }
                                })
                            };
                            html! {
                                <>
                                    // `data-action-index` is how ArrowRight maps
                                    // the focused item back to the action it
                                    // expands; see `focused_submenu`.
                                    <div class="menu-item" role="menuitem" tabindex="0"
                                         aria-haspopup="true" aria-expanded={is_open.to_string()}
                                         data-action-index={i.to_string()}
                                         onclick={toggle} {onkeydown}>
                                        { icon(ic) }<span>{ label }</span>
                                        <span class="menu-caret">{ if is_open { "▾" } else { "▸" } }</span>
                                    </div>
                                    if is_open {
                                        <div class="menu-sub-list" role="group">
                                            { for options.iter().map(|(id, name)| {
                                                let activate = {
                                                    let cb = cb.clone();
                                                    // Same focus handoff as a
                                                    // top-level item: the popup
                                                    // (and this option with it)
                                                    // unmounts on close.
                                                    let close_and_restore = close_and_restore.clone();
                                                    let id = id.clone();
                                                    Callback::from(move |_: ()| {
                                                        cb.emit(id.clone());
                                                        close_and_restore.emit(());
                                                    })
                                                };
                                                let onclick = {
                                                    let activate = activate.clone();
                                                    Callback::from(move |_: MouseEvent| activate.emit(()))
                                                };
                                                let onkeydown = Callback::from(move |e: KeyboardEvent| {
                                                    if e.key() == "Enter" || e.key() == " " {
                                                        e.prevent_default();
                                                        // Same as a top-level
                                                        // item: the option owns
                                                        // the key, not the host
                                                        // row.
                                                        e.stop_propagation();
                                                        activate.emit(());
                                                    }
                                                });
                                                html! {
                                                    <div class="menu-item menu-sub-option" role="menuitem"
                                                         tabindex="0" {onclick} {onkeydown}>{ name }</div>
                                                }
                                            }) }
                                        </div>
                                    }
                                </>
                            }
                        }
                    }
                }) }
            </div>
        </>
    }
}

/// The menu's focusable items, in document order.
fn menu_items(container: &NodeRef) -> Vec<web_sys::HtmlElement> {
    let Some(root) = container.cast::<web_sys::HtmlElement>() else {
        return Vec::new();
    };
    let Ok(list) = root.query_selector_all("[role=\"menuitem\"]") else {
        return Vec::new();
    };
    (0..list.length())
        .filter_map(|i| list.item(i))
        .filter_map(|node| node.dyn_into::<web_sys::HtmlElement>().ok())
        .collect()
}

/// The action index of the submenu whose own item currently has focus.
///
/// The index is the item's position in `props.actions`, which is what
/// `expanded` stores — so ArrowRight focuses and expands the same entry.
fn focused_submenu(container: &NodeRef) -> Option<usize> {
    let active = active_element();
    let root = container.cast::<web_sys::HtmlElement>()?;
    let list = root.query_selector_all("[aria-haspopup=\"true\"]").ok()?;
    (0..list.length()).find_map(|i| {
        let node = list.item(i)?;
        if !node.is_same_node(Some(&active)) {
            return None;
        }
        // Read the action index off the element rather than using `i`: this
        // query only sees the submenus, so a submenu's position here is not its
        // position in `actions` whenever a plain item precedes it — which is
        // every menu in the app. Using `i` made ArrowRight expand a different
        // entry than the focused one, or (with a single submenu) nothing at all.
        node.dyn_into::<web_sys::Element>()
            .ok()?
            .get_attribute("data-action-index")?
            .parse()
            .ok()
    })
}

/// The document's currently focused element.
fn active_element() -> web_sys::Node {
    web_sys::window()
        .and_then(|w| w.document())
        .and_then(|d| d.active_element())
        .map(|el| el.unchecked_into::<web_sys::Node>())
        .unwrap_or(wasm_bindgen::JsValue::NULL.unchecked_into())
}

/// Move focus to the header of the submenu whose option has focus, if any.
///
/// Returns whether focus moved. A submenu collapse unmounts its options, and
/// removing the focused element sends focus to the body, so ArrowLeft from an
/// option has to hand focus to the header that owns it first. The header is the
/// list's previous element sibling: the two are rendered side by side inside the
/// menu.
fn focus_open_submenu_header(container: &NodeRef) -> bool {
    let Some(root) = container.cast::<web_sys::Node>() else {
        return false;
    };
    let Some(option) = active_element().dyn_into::<web_sys::Element>().ok() else {
        return false;
    };
    let Some(list) = option.closest(".menu-sub-list").ok().flatten() else {
        return false;
    };
    // Only this menu's submenu: `active_element` is document-wide.
    if !root.contains(Some(list.unchecked_ref::<web_sys::Node>())) {
        return false;
    }
    let Some(header) = list
        .previous_element_sibling()
        .and_then(|n| n.dyn_into::<web_sys::HtmlElement>().ok())
    else {
        return false;
    };
    header.focus().is_ok()
}

/// A kebab (⋮) button that owns its open state and shows a [`Menu`].
#[derive(Properties, PartialEq)]
pub struct MenuButtonProps {
    pub actions: Vec<MenuAction>,
    #[prop_or_default]
    pub align_right: bool,
    /// Trigger icon; all menus use the vertical kebab by default.
    #[prop_or("kebab")]
    pub icon: &'static str,
}

#[function_component(MenuButton)]
pub fn menu_button(props: &MenuButtonProps) -> Html {
    let open = use_state(|| false);
    // Where Escape returns focus: the kebab that opened the menu.
    let trigger = use_node_ref();
    let toggle = {
        let open = open.clone();
        Callback::from(move |e: MouseEvent| {
            e.stop_propagation();
            open.set(!*open);
        })
    };
    let on_close = {
        let open = open.clone();
        Callback::from(move |_| open.set(false))
    };
    html! {
        <div class="menu-anchor">
            <button class="ibtn" title="More" aria-haspopup="menu" aria-expanded={open.to_string()}
                    ref={trigger.clone()} onclick={toggle}>
                { icon(props.icon) }
            </button>
            <Menu open={*open} on_close={on_close} actions={props.actions.clone()}
                  align_right={props.align_right} anchor={Some(trigger)} />
        </div>
    }
}
