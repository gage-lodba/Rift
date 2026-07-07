//! Context / kebab popup menu and its trigger button.

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
}

/// A controlled popup menu: render it inside a `.menu-anchor` and drive `open`
/// from the parent (so both a kebab button and a right-click can open it).
#[function_component(Menu)]
pub fn menu(props: &MenuProps) -> Html {
    let expanded = use_state(|| None::<usize>);
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
                 onclick={|e: MouseEvent| e.stop_propagation()}>
                { for props.actions.iter().enumerate().map(|(i, a)| {
                    match a {
                        MenuAction::Separator => html! { <div class="menu-sep"></div> },
                        MenuAction::Item { icon: ic, label, danger, cb } => {
                            let cb = cb.clone();
                            let close = close.clone();
                            let onclick = Callback::from(move |_: MouseEvent| { cb.emit(()); close.emit(()); });
                            html! {
                                <div class={classes!("menu-item", danger.then_some("danger"))} onclick={onclick}>
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
                            html! {
                                <>
                                    <div class="menu-item" onclick={toggle}>
                                        { icon(ic) }<span>{ label }</span>
                                        <span class="menu-caret">{ if is_open { "▾" } else { "▸" } }</span>
                                    </div>
                                    if is_open {
                                        <div class="menu-sub-list">
                                            { for options.iter().map(|(id, name)| {
                                                let cb = cb.clone();
                                                let close = close.clone();
                                                let id = id.clone();
                                                let onclick = Callback::from(move |_: MouseEvent| {
                                                    cb.emit(id.clone());
                                                    close.emit(());
                                                });
                                                html! { <div class="menu-item menu-sub-option" onclick={onclick}>{ name }</div> }
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
            <button class="ibtn" title="More" onclick={toggle}>
                { icon(props.icon) }
            </button>
            <Menu open={*open} on_close={on_close} actions={props.actions.clone()} align_right={props.align_right} />
        </div>
    }
}
