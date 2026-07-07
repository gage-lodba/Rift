//! Top window titlebar: logo, search box, and window controls.

use serde_json::json;
use wasm_bindgen_futures::spawn_local;
use web_sys::HtmlInputElement;
use yew::prelude::*;

use super::icons::{icon, logo_mark};
use crate::api;

#[derive(Properties, PartialEq)]
pub struct TitlebarProps {
    pub on_search: Callback<String>,
}

#[function_component(Titlebar)]
pub fn titlebar(props: &TitlebarProps) -> Html {
    let input = use_node_ref();

    let submit = {
        let input = input.clone();
        let on_search = props.on_search.clone();
        Callback::from(move |()| {
            if let Some(el) = input.cast::<HtmlInputElement>() {
                let q = el.value();
                if !q.trim().is_empty() {
                    on_search.emit(q.trim().to_string());
                }
            }
        })
    };
    let onkeydown = {
        let submit = submit.clone();
        Callback::from(move |e: KeyboardEvent| {
            if e.key() == "Enter" {
                submit.emit(());
            }
        })
    };
    // Debounced live search: each keystroke schedules a search ~300ms out, and a
    // monotonically increasing token cancels earlier pending ones so only the
    // last pause in typing actually fires. Enter still searches immediately.
    let debounce = use_mut_ref(|| 0u32);
    let oninput = {
        let on_search = props.on_search.clone();
        let debounce = debounce.clone();
        Callback::from(move |e: InputEvent| {
            let el: HtmlInputElement = e.target_unchecked_into();
            let q = el.value().trim().to_string();
            let token = {
                let mut d = debounce.borrow_mut();
                *d += 1;
                *d
            };
            if q.is_empty() {
                return;
            }
            let on_search = on_search.clone();
            let debounce = debounce.clone();
            spawn_local(async move {
                gloo_timers::future::TimeoutFuture::new(300).await;
                if *debounce.borrow() == token {
                    on_search.emit(q);
                }
            });
        })
    };
    let onclick = {
        let submit = submit.clone();
        Callback::from(move |_| submit.emit(()))
    };

    html! {
        <header class="titlebar" data-tauri-drag-region="true">
            <div class="logo" data-tauri-drag-region="true">
                { logo_mark() }
                <span class="logo-text" data-tauri-drag-region="true">{ "RIFT" }</span>
            </div>
            <div class="searchbox">
                <input ref={input} type="text" placeholder="Search YouTube Music..." {onkeydown} {oninput} />
                <button class="search-btn" {onclick}>{ "Search" }</button>
            </div>
            <div class="tb-space" data-tauri-drag-region="true"></div>
            <div class="win-controls">
                <button class="win-btn" title="Minimize"
                    onclick={|_| api::fire("window_minimize", json!({}))}>{ icon("min") }</button>
                <button class="win-btn" title="Maximize"
                    onclick={|_| api::fire("window_toggle_maximize", json!({}))}>{ icon("max") }</button>
                <button class="win-btn win-close" title="Close"
                    onclick={|_| api::fire("window_close", json!({}))}>{ icon("x") }</button>
            </div>
        </header>
    }
}
