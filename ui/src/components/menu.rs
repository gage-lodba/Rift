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
/// submenu, and Escape closes the whole menu and restores focus to its anchor.
/// Pointer behaviour is unchanged.
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
    // Escape closes from anywhere in the popup, including a submenu; focus
    // returns to whatever opened it.
    let onkeydown = {
        let close = props.on_close.clone();
        let anchor = props.anchor.clone();
        let items = items.clone();
        let expanded = expanded.clone();
        Callback::from(move |e: KeyboardEvent| match e.key().as_str() {
            "Escape" => {
                e.prevent_default();
                e.stop_propagation();
                close.emit(());
                if let Some(node) = anchor
                    .as_ref()
                    .and_then(NodeRef::cast::<web_sys::HtmlElement>)
                {
                    let _ = node.focus();
                }
            }
            "ArrowDown" | "ArrowUp" | "Home" | "End" => {
                e.prevent_default();
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
                    expanded.set(Some(index));
                }
            }
            "ArrowLeft" if expanded.is_some() => {
                e.prevent_default();
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

    html! {
        <>
            <div class="menu-backdrop"
                 onclick={Callback::from(move |e: MouseEvent| { e.stop_propagation(); backdrop_close.emit(()); })}
                 oncontextmenu={let c = close.clone(); Callback::from(move |e: MouseEvent| { e.prevent_default(); e.stop_propagation(); c.emit(()); })}>
            </div>
            <div class={classes!("menu", props.align_right.then_some("menu-right"))}
                 role="menu" tabindex="-1" ref={items} {onkeydown}
                 onclick={|e: MouseEvent| e.stop_propagation()}>
                { for props.actions.iter().enumerate().map(|(i, a)| {
                    match a {
                        MenuAction::Separator => html! { <div class="menu-sep" role="separator"></div> },
                        MenuAction::Item { icon: ic, label, danger, cb } => {
                            let activate = {
                                let cb = cb.clone();
                                let close = close.clone();
                                Callback::from(move |_: ()| {
                                    cb.emit(());
                                    close.emit(());
                                })
                            };
                            let onclick = {
                                let activate = activate.clone();
                                Callback::from(move |_: MouseEvent| activate.emit(()))
                            };
                            // A `div` with a role is focusable and Enter/Space
                            // activate it, without the default button chrome.
                            let onkeydown = Callback::from(move |e: KeyboardEvent| {
                                if e.key() == "Enter" || e.key() == " " {
                                    e.prevent_default();
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
                                        expanded.set(if is_open { None } else { Some(i) });
                                    }
                                })
                            };
                            html! {
                                <>
                                    <div class="menu-item" role="menuitem" tabindex="0"
                                         aria-haspopup="true" aria-expanded={is_open.to_string()}
                                         onclick={toggle} {onkeydown}>
                                        { icon(ic) }<span>{ label }</span>
                                        <span class="menu-caret">{ if is_open { "▾" } else { "▸" } }</span>
                                    </div>
                                    if is_open {
                                        <div class="menu-sub-list" role="group">
                                            { for options.iter().map(|(id, name)| {
                                                let activate = {
                                                    let cb = cb.clone();
                                                    let close = close.clone();
                                                    let id = id.clone();
                                                    Callback::from(move |_: ()| {
                                                        cb.emit(id.clone());
                                                        close.emit(());
                                                    })
                                                };
                                                let onclick = {
                                                    let activate = activate.clone();
                                                    Callback::from(move |_: MouseEvent| activate.emit(()))
                                                };
                                                let onkeydown = Callback::from(move |e: KeyboardEvent| {
                                                    if e.key() == "Enter" || e.key() == " " {
                                                        e.prevent_default();
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
        node.is_same_node(Some(&active)).then_some(i as usize)
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
