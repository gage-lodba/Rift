//! Bottom player bar: seek, now-playing, transport controls, and volume.

use rift_types::{events, PlaybackState, Progress, RepeatMode, Track};
use serde_json::json;
use wasm_bindgen_futures::spawn_local;
use web_sys::HtmlInputElement;
use yew::prelude::*;

use super::icons::{artist_links, cover, fmt_pos, icon};
use crate::api;

/// Identity of the track a set of progress values describes. `None` (nothing
/// loaded) is its own identity, so stopping clears the bar too.
///
/// This is the dependency that makes the re-seed effect below run on a track
/// change (see the comment at the `use_effect_with`), which is what H4 was
/// about. `pub(crate)` so its contract — distinct tracks get distinct keys —
/// can be tested without a renderer; the effect is keyed on its output, so a
/// key that stopped distinguishing tracks would silently reintroduce H4.
pub(crate) fn track_key(track: &Option<Track>) -> Option<String> {
    track.as_ref().map(|t| t.id.clone())
}

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
    // Which track the currently-rendered `live` values belong to. `track_changed`
    // is true during the one render between the new track arriving and the
    // re-seed effect below running — the only window in which `live` still
    // describes the previous track.
    let rendered_track = use_state(|| track_key(&props.track));
    let key = track_key(&props.track);
    let track_changed = *rendered_track != key;
    {
        // Re-seed when the prop seed changes (bootstrap resolving, or the
        // preview override arriving) *and* when the track changes.
        //
        // The track id is part of the dependency on purpose. `props.progress`
        // never moves after bootstrap — `App` deliberately does not subscribe to
        // PROGRESS — so without it this effect never re-runs on a track change,
        // and the bar keeps showing the *previous* track's elapsed time and
        // slider `max` for the whole fetch window. The backend only emits its
        // corrective `PROGRESS {0.0, duration}` once the stream resolves
        // (`playback.rs`), i.e. after a network fetch.
        let live = live.clone();
        let rendered_track = rendered_track.clone();
        let key = key.clone();
        use_effect_with((props.progress, key.clone()), move |(seed, _)| {
            rendered_track.set(key);
            live.set(*seed);
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
    // While `live` still belongs to the track that just ended, fall back to the
    // new track's own metadata: its known length for the slider's range, and a
    // zeroed position. Otherwise the seek bar briefly spans the *previous*
    // track's length and its thumb sits at that track's elapsed time.
    let track_duration = props.track.as_ref().and_then(|t| t.duration).unwrap_or(0) as f64;
    let (pos, dur) = if track_changed {
        (0.0, track_duration)
    } else {
        (live.position, live.duration)
    };
    let duration = dur.max(1.0);
    // Show the dragged position while scrubbing, otherwise the live position.
    let seek_pos = scrubbing.unwrap_or(pos);

    html! {
        <footer class="player">
            <div class="seek-row">
                <span class="time">{ fmt_pos(seek_pos) }</span>
                <input class="seek" type="range" min="0" step="1"
                       max={format!("{}", duration as u64)}
                       value={format!("{}", seek_pos as u64)}
                       aria-label="Seek"
                       aria-valuetext={format!("{} of {}", fmt_pos(seek_pos), fmt_pos(dur))}
                       oninput={seek_input} onchange={seek_commit} />
                <span class="time">{ fmt_pos(dur) }</span>
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
                           aria-label="Volume"
                           aria-valuetext={format!("{}%", (props.volume * 100.0) as u32)}
                           oninput={set_volume} onchange={save_volume} />
                </div>
            </div>
        </footer>
    }
}
