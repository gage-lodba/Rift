//! Settings view: Discord RPC, crossfade, yt-dlp detection, and updates.

use rift_types::{UpdateStatus, YtDlpStatus};
use serde_json::json;
use wasm_bindgen_futures::spawn_local;
use web_sys::HtmlInputElement;
use yew::prelude::*;

use crate::api;

#[derive(Properties, PartialEq)]
pub struct SettingsProps {
    pub discord_rpc: bool,
    /// Called with the new value when the Discord toggle is flipped.
    pub on_discord_rpc: Callback<bool>,
    /// Crossfade overlap in seconds; 0 means disabled.
    pub crossfade: f32,
    /// Called with the new crossfade duration in seconds (0 disables it).
    pub on_crossfade: Callback<f32>,
    /// Configured custom yt-dlp path (empty/None = auto-detect).
    #[prop_or_default]
    pub yt_dlp_path: Option<String>,
    /// Whether launch-time update notifications are enabled.
    pub update_notifications: bool,
    /// Called with the new value when the update-notification toggle is flipped.
    pub on_update_notifications: Callback<bool>,
}

/// Default overlap applied when crossfade is toggled on from off.
const DEFAULT_CROSSFADE: f32 = 6.0;
/// Largest overlap the slider offers (mirrors the backend's `MAX_CROSSFADE`).
const MAX_CROSSFADE: f32 = 12.0;

/// `None` while a probe is in flight; `Some` once it resolves.
type YtDlpProbe = Option<YtDlpStatus>;

#[function_component(SettingsView)]
pub fn settings_view(props: &SettingsProps) -> Html {
    let on_discord = {
        let current = props.discord_rpc;
        let cb = props.on_discord_rpc.clone();
        Callback::from(move |_: MouseEvent| cb.emit(!current))
    };

    let crossfade_on = props.crossfade > 0.0;
    // The toggle flips crossfade between off and a sensible default.
    let on_crossfade_toggle = {
        let cb = props.on_crossfade.clone();
        Callback::from(move |_: MouseEvent| {
            cb.emit(if crossfade_on { 0.0 } else { DEFAULT_CROSSFADE })
        })
    };
    let on_crossfade_slider = {
        let cb = props.on_crossfade.clone();
        Callback::from(move |e: InputEvent| {
            let el: HtmlInputElement = e.target_unchecked_into();
            if let Ok(v) = el.value().parse::<f32>() {
                cb.emit(v);
            }
        })
    };

    // yt-dlp detection: probe once on mount, and again when the user clicks
    // "Check again". `None` renders as a "Checking…" state.
    let ytdlp = use_state(|| None as YtDlpProbe);
    let check = {
        let ytdlp = ytdlp.clone();
        Callback::from(move |_: ()| {
            let ytdlp = ytdlp.clone();
            ytdlp.set(None);
            spawn_local(async move {
                let status = api::invoke::<YtDlpStatus>("check_ytdlp", &json!({}))
                    .await
                    .unwrap_or_default();
                ytdlp.set(Some(status));
            });
        })
    };
    // Probe once when the view mounts.
    {
        let check = check.clone();
        use_effect_with((), move |_| {
            check.emit(());
            || ()
        });
    }
    let on_check = check.reform(|_: MouseEvent| ());

    // Custom yt-dlp location: draft mirrors the input; saving persists it and
    // re-probes. A blank value clears the override (back to auto-detect).
    let path_draft = use_state(|| props.yt_dlp_path.clone().unwrap_or_default());
    let on_path_input = {
        let path_draft = path_draft.clone();
        Callback::from(move |e: InputEvent| {
            let el: HtmlInputElement = e.target_unchecked_into();
            path_draft.set(el.value());
        })
    };
    let save_path = {
        let ytdlp = ytdlp.clone();
        let path_draft = path_draft.clone();
        Callback::from(move |_: MouseEvent| {
            let ytdlp = ytdlp.clone();
            let value = (*path_draft).clone();
            ytdlp.set(None);
            spawn_local(async move {
                let status =
                    api::invoke::<YtDlpStatus>("set_yt_dlp_path", &json!({ "path": value }))
                        .await
                        .unwrap_or_default();
                ytdlp.set(Some(status));
            });
        })
    };

    // Download yt-dlp into the app data dir when it isn't installed.
    let downloading = use_state(|| false);
    let download = {
        let ytdlp = ytdlp.clone();
        let downloading = downloading.clone();
        let path_draft = path_draft.clone();
        Callback::from(move |_: MouseEvent| {
            let ytdlp = ytdlp.clone();
            let downloading = downloading.clone();
            let path_draft = path_draft.clone();
            downloading.set(true);
            spawn_local(async move {
                let status = api::invoke::<YtDlpStatus>("download_ytdlp", &json!({}))
                    .await
                    .unwrap_or_default();
                if let Some(p) = &status.path {
                    path_draft.set(p.clone());
                }
                ytdlp.set(Some(status));
                downloading.set(false);
            });
        })
    };
    // Offer the download only once a probe has confirmed yt-dlp is missing.
    let missing = matches!(&*ytdlp, Some(s) if !s.found);

    // Update check: probe GitHub on mount and on demand. `None` = checking.
    let update = use_state(|| None as Option<UpdateStatus>);
    let check_update = {
        let update = update.clone();
        Callback::from(move |_: ()| {
            let update = update.clone();
            update.set(None);
            spawn_local(async move {
                let status = api::invoke::<UpdateStatus>("check_update", &json!({}))
                    .await
                    .unwrap_or_default();
                update.set(Some(status));
            });
        })
    };
    {
        let check_update = check_update.clone();
        use_effect_with((), move |_| {
            check_update.emit(());
            || ()
        });
    }
    let on_check_update = check_update.reform(|_: MouseEvent| ());
    let on_update_toggle = {
        let current = props.update_notifications;
        let cb = props.on_update_notifications.clone();
        Callback::from(move |_: MouseEvent| cb.emit(!current))
    };
    // Open the release page in the default browser.
    let open_release = {
        let update = update.clone();
        Callback::from(move |_: MouseEvent| {
            if let Some(url) = (*update).as_ref().and_then(|u| u.url.clone()) {
                api::fire("open_url", json!({ "url": url }));
            }
        })
    };

    html! {
        <>
            <h2>{ "Settings" }</h2>
            <div class="settings-row">
                <div class="settings-text">
                    <div class="settings-label">{ "Discord Rich Presence" }</div>
                    <div class="settings-desc">
                        { "Show the track you're listening to on your Discord profile." }
                    </div>
                </div>
                <button
                    class={classes!("switch", props.discord_rpc.then_some("on"))}
                    role="switch"
                    aria-checked={props.discord_rpc.to_string()}
                    onclick={on_discord}>
                    <span class="switch-knob" />
                </button>
            </div>
            <div class="settings-row">
                <div class="settings-text">
                    <div class="settings-label">{ "Crossfade" }</div>
                    <div class="settings-desc">
                        { "Overlap the end of each track with the start of the next for a smooth transition." }
                    </div>
                    if crossfade_on {
                        <div class="crossfade-control">
                            <input
                                class="crossfade-slider"
                                type="range"
                                min="1"
                                max={MAX_CROSSFADE.to_string()}
                                step="1"
                                value={props.crossfade.to_string()}
                                oninput={on_crossfade_slider} />
                            <span class="crossfade-value">
                                { format!("{}s", props.crossfade.round() as i32) }
                            </span>
                        </div>
                    }
                </div>
                <button
                    class={classes!("switch", crossfade_on.then_some("on"))}
                    role="switch"
                    aria-checked={crossfade_on.to_string()}
                    onclick={on_crossfade_toggle}>
                    <span class="switch-knob" />
                </button>
            </div>
            <div class="settings-row">
                <div class="settings-text">
                    <div class="settings-label">{ "Streaming engine (yt-dlp)" }</div>
                    <div class="settings-desc">
                        { "Rift uses yt-dlp to fetch audio. It's auto-detected from your PATH and common install locations; set a custom path below if it lives elsewhere." }
                    </div>
                    { ytdlp_status_line(&ytdlp) }
                    if missing {
                        <div class="ytdlp-actions">
                            <button class="btn-primary" onclick={download} disabled={*downloading}>
                                { if *downloading { "Downloading…" } else { "Download yt-dlp" } }
                            </button>
                            <span class="ytdlp-hint">{ "Fetches the latest build into Rift's data folder." }</span>
                        </div>
                    }
                    <div class="ytdlp-custom">
                        <input class="ytdlp-input" type="text"
                               placeholder="Custom yt-dlp path (leave blank to auto-detect)"
                               value={(*path_draft).clone()}
                               oninput={on_path_input} />
                        <button class="btn-secondary" onclick={save_path}>{ "Save" }</button>
                    </div>
                </div>
                <button class="btn-secondary" onclick={on_check}>{ "Check again" }</button>
            </div>
            <div class="settings-row">
                <div class="settings-text">
                    <div class="settings-label">{ "Updates" }</div>
                    <div class="settings-desc">
                        { "Check GitHub for a newer version of Rift on launch and notify you." }
                    </div>
                    { update_status_line(&update) }
                    <div class="ytdlp-actions">
                        <button class="btn-secondary" onclick={on_check_update}>{ "Check now" }</button>
                        if matches!(&*update, Some(u) if u.update_available) {
                            <button class="btn-primary" onclick={open_release}>
                                { "Download update" }
                            </button>
                        }
                    </div>
                </div>
                <button
                    class={classes!("switch", props.update_notifications.then_some("on"))}
                    role="switch"
                    aria-checked={props.update_notifications.to_string()}
                    onclick={on_update_toggle}>
                    <span class="switch-knob" />
                </button>
            </div>
        </>
    }
}

fn update_status_line(probe: &Option<UpdateStatus>) -> Html {
    match probe {
        None => html! { <div class="ytdlp-status checking">{ "Checking…" }</div> },
        Some(u) if u.update_available => html! {
            <div class="ytdlp-status missing">
                <span class="ytdlp-badge">{ "Update available" }</span>
                <span class="ytdlp-meta">
                    { format!("v{} → v{}", u.current, u.latest.clone().unwrap_or_default()) }
                </span>
            </div>
        },
        Some(u) if u.latest.is_some() => html! {
            <div class="ytdlp-status found">
                <span class="ytdlp-badge">{ "✓ Up to date" }</span>
                <span class="ytdlp-meta">{ format!("v{}", u.current) }</span>
            </div>
        },
        // Latest unknown (check failed / offline): just show the running version.
        Some(u) => html! {
            <div class="ytdlp-status">
                <span class="ytdlp-meta">{ format!("Rift v{} — could not reach GitHub", u.current) }</span>
            </div>
        },
    }
}

fn ytdlp_status_line(probe: &YtDlpProbe) -> Html {
    match probe {
        None => html! {
            <div class="ytdlp-status checking">{ "Checking…" }</div>
        },
        Some(status) if status.found => {
            let version = status.version.clone().unwrap_or_default();
            let path = status.path.clone().unwrap_or_default();
            html! {
                <div class="ytdlp-status found">
                    <span class="ytdlp-badge">{ "✓ Found" }</span>
                    { if version.is_empty() { html!{} } else { html!{ <span class="ytdlp-meta">{ version }</span> } } }
                    { if path.is_empty() { html!{} } else { html!{ <code class="ytdlp-path">{ path }</code> } } }
                </div>
            }
        }
        Some(status) => {
            // Not found, but we may have resolved a path that failed to run.
            let detail = match &status.path {
                Some(p) => format!("Found a file at {p} but it didn't run."),
                None => "Not found on your PATH or in common install locations.".to_string(),
            };
            html! {
                <div class="ytdlp-status missing">
                    <span class="ytdlp-badge">{ "✗ Not found" }</span>
                    <span class="ytdlp-meta">{ detail }</span>
                </div>
            }
        }
    }
}
