//! Bottom player bar: seek, now-playing, transport controls, and volume.

use rift_types::{events, PlaybackState, Progress, RepeatMode, Track};
use serde_json::json;
use wasm_bindgen_futures::spawn_local;
use web_sys::HtmlInputElement;
use yew::prelude::*;

use super::icons::{artist_links, cover, fmt_pos, icon};
use crate::api;

#[derive(Properties, PartialEq)]
pub struct PlayerBarProps {
    pub track: Option<Track>,
    pub state: PlaybackState,
    /// Seed for the seek bar — the bootstrap-restored position (or the preview
    /// override). Live position/duration after mount come from the PROGRESS
    /// event this bar subscribes to itself, so `App` doesn't re-render on every
    /// ~4 Hz tick.
    pub progress: Progress,
    pub volume: f32,
    pub shuffle: bool,
    pub repeat: RepeatMode,
    pub liked: bool,
    pub on_open_artist: Callback<String>,
    pub on_volume: Callback<f32>,
}

#[function_component(PlayerBar)]
pub fn player_bar(props: &PlayerBarProps) -> Html {
    // While the user drags the seek bar this holds the in-progress position,
    // so live progress ticks from the backend don't fight the drag.
    let scrubbing = use_state(|| None::<f64>);
    // Live position/duration, seeded from the prop and updated by PROGRESS
    // ticks. Kept local so the 4 Hz ticks re-render only this bar, not the app.
    let live = use_state(|| props.progress);
    {
        // Re-seed when the prop seed changes: bootstrap resolving, or the
        // preview override arriving. A new track resets via a PROGRESS{0,dur}
        // the backend emits, so that flows through the subscription below (the
        // prop stays put, so this effect doesn't fight it).
        let live = live.clone();
        use_effect_with(props.progress, move |p| {
            live.set(*p);
            || ()
        });
    }
    {
        // Subscribe once (for the bar's lifetime) to live progress ticks.
        let live = live.clone();
        use_effect_with((), move |_| {
            api::listen_event::<Progress>(events::PROGRESS, Callback::from(move |p| live.set(p)));
            || ()
        });
    }

    let seek_input = {
        let scrubbing = scrubbing.clone();
        Callback::from(move |e: InputEvent| {
            let el: HtmlInputElement = e.target_unchecked_into();
            if let Ok(pos) = el.value().parse::<f64>() {
                scrubbing.set(Some(pos));
            }
        })
    };
    let seek_commit = {
        let scrubbing = scrubbing.clone();
        Callback::from(move |e: Event| {
            let el: HtmlInputElement = e.target_unchecked_into();
            if let Ok(pos) = el.value().parse::<f64>() {
                api::fire("seek", json!({ "position": pos }));
            }
            scrubbing.set(None);
        })
    };
    let set_volume = {
        let on_volume = props.on_volume.clone();
        Callback::from(move |e: InputEvent| {
            let el: HtmlInputElement = e.target_unchecked_into();
            if let Ok(v) = el.value().parse::<f32>() {
                on_volume.emit(v / 100.0);
            }
        })
    };
    // Persist only when the drag ends, so we don't write to disk on every tick.
    let save_volume = Callback::from(move |e: Event| {
        let el: HtmlInputElement = e.target_unchecked_into();
        if let Ok(v) = el.value().parse::<f32>() {
            api::fire("save_volume", json!({ "volume": v / 100.0 }));
        }
    });
    let toggle_like = {
        let track = props.track.clone();
        Callback::from(move |_| {
            if let Some(t) = &track {
                let t = t.clone();
                spawn_local(async move {
                    let _ = api::invoke::<serde_json::Value>("toggle_like", &json!({ "track": t }))
                        .await;
                });
            }
        })
    };

    let loading = props.state == PlaybackState::Loading;
    let playing = props.state == PlaybackState::Playing;
    let duration = live.duration.max(1.0);
    // Show the dragged position while scrubbing, otherwise the live position.
    let seek_pos = scrubbing.unwrap_or(live.position);

    html! {
        <footer class="player">
            <div class="seek-row">
                <span class="time">{ fmt_pos(seek_pos) }</span>
                <input class="seek" type="range" min="0" step="1"
                       max={format!("{}", duration as u64)}
                       value={format!("{}", seek_pos as u64)}
                       oninput={seek_input} onchange={seek_commit} />
                <span class="time">{ fmt_pos(live.duration) }</span>
            </div>
            <div class="player-row">
                <div class="now-playing">
                    if let Some(t) = &props.track {
                        { cover(&t.cover, "np-cover") }
                        <div class="trow-meta">
                            <div class="trow-title">{ &t.title }</div>
                            <div class="trow-artist">
                                { artist_links(&t.artists, &t.artist, &props.on_open_artist) }
                            </div>
                        </div>
                        <button class={classes!("ibtn", "np-like", props.liked.then_some("liked"))}
                                title="Like" onclick={toggle_like}>
                            { icon(if props.liked { "heart" } else { "heart-outline" }) }
                        </button>
                    }
                </div>
                <div class="controls">
                    <button class={classes!("ibtn", props.shuffle.then_some("accent"))} title="Shuffle"
                            onclick={|_| api::fire("toggle_shuffle", json!({}))}>{ icon("shuffle") }</button>
                    <button class="ibtn" title="Previous"
                            onclick={|_| api::fire("prev_track", json!({}))}>{ icon("prev") }</button>
                    <button class={classes!("play-btn", loading.then_some("loading"))} title="Play/Pause"
                            onclick={|_| api::fire("toggle_play", json!({}))}>
                        { icon(if playing || loading { "pause" } else { "play" }) }
                    </button>
                    <button class="ibtn" title="Next"
                            onclick={|_| api::fire("next_track", json!({}))}>{ icon("next") }</button>
                    <button class={classes!("ibtn", (props.repeat != RepeatMode::Off).then_some("accent"))}
                            title="Repeat" onclick={|_| api::fire("cycle_repeat", json!({}))}>
                        { icon(if props.repeat == RepeatMode::One { "repeat-one" } else { "repeat" }) }
                    </button>
                </div>
                <div class="volume">
                    { icon("volume") }
                    <input class="vol-slider" type="range" min="0" max="100"
                           value={format!("{}", (props.volume * 100.0) as u32)}
                           oninput={set_volume} onchange={save_volume} />
                </div>
            </div>
        </footer>
    }
}
