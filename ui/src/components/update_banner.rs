//! Dismissible banner shown when the launch check finds a newer release.

use rift_types::UpdateStatus;
use serde_json::json;
use yew::prelude::*;

use super::icons::icon;
use crate::api;

#[derive(Properties, PartialEq)]
pub struct UpdateBannerProps {
    pub status: UpdateStatus,
    /// Close the banner for this session.
    pub on_dismiss: Callback<()>,
    /// "Don't ask again": silence future launch checks and close the banner.
    pub on_silence: Callback<()>,
}

#[function_component(UpdateBanner)]
pub fn update_banner(props: &UpdateBannerProps) -> Html {
    let dismiss = props.on_dismiss.reform(|_: MouseEvent| ());
    let silence = props.on_silence.reform(|_: MouseEvent| ());
    let download = {
        let url = props.status.url.clone();
        Callback::from(move |_: MouseEvent| {
            if let Some(url) = url.clone() {
                api::fire("open_url", json!({ "url": url }));
            }
        })
    };
    html! {
        <div class="update-banner">
            <span class="update-banner-text">
                { format!("Rift v{} is available.", props.status.latest.clone().unwrap_or_default()) }
            </span>
            <button class="btn-primary" onclick={download}>{ "Download" }</button>
            <button class="btn-secondary" onclick={silence}>{ "Don't ask again" }</button>
            <button class="ibtn update-banner-close" title="Dismiss" onclick={dismiss}>
                { icon("x") }
            </button>
        </div>
    }
}
