//! Small shared presentational helpers: inline icons, covers, and formatting.

use rift_types::ArtistRef;
use yew::prelude::*;

/// Inline solid (filled) SVG icons, Material-style.
pub fn icon(name: &str) -> Html {
    let d = match name {
        "home" => "M10 20v-6h4v6h5v-8h3L12 3 2 12h3v8z",
        "heart" => "M12 21.35l-1.45-1.32C5.4 15.36 2 12.28 2 8.5 2 5.42 4.42 3 7.5 3c1.74 0 3.41.81 4.5 2.09C13.09 3.81 14.76 3 16.5 3 19.58 3 22 5.42 22 8.5c0 3.78-3.4 6.86-8.55 11.54L12 21.35z",
        "heart-outline" => "M16.5 3c-1.74 0-3.41.81-4.5 2.09C10.91 3.81 9.24 3 7.5 3 4.42 3 2 5.42 2 8.5c0 3.78 3.4 6.86 8.55 11.54L12 21.35l1.45-1.32C18.6 15.36 22 12.28 22 8.5 22 5.42 19.58 3 16.5 3zm-4.4 15.55l-.1.1-.1-.1C7.14 14.24 4 11.39 4 8.5 4 6.5 5.5 5 7.5 5c1.54 0 3.04.99 3.57 2.36h1.87C13.46 5.99 14.96 5 16.5 5c2 0 3.5 1.5 3.5 3.5 0 2.89-3.14 5.74-7.9 10.05z",
        "music" => "M21 3v12.5c0 1.38-1.12 2.5-2.5 2.5S16 16.88 16 15.5s1.12-2.5 2.5-2.5c.35 0 .69.07 1 .18V8.4L10 9.7v7.8c0 1.38-1.12 2.5-2.5 2.5S5 18.88 5 17.5 6.12 15 7.5 15c.35 0 .69.07 1 .18V6.3L21 3z",
        "plus" => "M19 13h-6v6h-2v-6H5v-2h6V5h2v6h6v2z",
        "search" => "M15.5 14h-.79l-.28-.27C15.41 12.59 16 11.11 16 9.5 16 5.91 13.09 3 9.5 3S3 5.91 3 9.5 5.91 16 9.5 16c1.61 0 3.09-.59 4.23-1.57l.27.28v.79l5 4.99L20.49 19l-4.99-5zm-6 0C7.01 14 5 11.99 5 9.5S7.01 5 9.5 5 14 7.01 14 9.5 11.99 14 9.5 14z",
        "play" => "M8 5v14l11-7z",
        "pause" => "M6 19h4V5H6v14zm8-14v14h4V5h-4z",
        "prev" => "M6 6h2v12H6zm3.5 6l8.5 6V6z",
        "next" => "M6 18l8.5-6L6 6v12zm10-12v12h2V6h-2z",
        "shuffle" => "M10.59 9.17L5.41 4 4 5.41l5.17 5.17 1.42-1.41zM14.5 4l2.04 2.04L4 18.59 5.41 20 17.96 7.46 20 9.5V4h-5.5zm.33 9.41l-1.41 1.41 3.13 3.13L14.5 20H20v-5.5l-2.04 2.04-3.13-3.13z",
        "repeat" => "M7 7h10v3l4-4-4-4v3H5v6h2V7zm10 10H7v-3l-4 4 4 4v-3h12v-6h-2v4z",
        "repeat-one" => "M7 7h10v3l4-4-4-4v3H5v6h2V7zm10 10H7v-3l-4 4 4 4v-3h12v-6h-2v4zm-4-2V9h-1l-2 1v1h1.5v4H13z",
        "volume" => "M3 9v6h4l5 5V4L7 9H3zm13.5 3c0-1.77-1.02-3.29-2.5-4.03v8.05c1.48-.73 2.5-2.25 2.5-4.02zM14 3.23v2.06c2.89.86 5 3.54 5 6.71s-2.11 5.85-5 6.71v2.06c4.01-.91 7-4.49 7-8.77s-2.99-7.86-7-8.77z",
        "x" => "M19 6.41L17.59 5 12 10.59 6.41 5 5 6.41 10.59 12 5 17.59 6.41 19 12 13.41 17.59 19 19 17.59 13.41 12z",
        "min" => "M19 13H5v-2h14v2z",
        "max" => "M4 4h16v16H4V4zm2 2v12h12V6H6z",
        "trash" => "M6 19c0 1.1.9 2 2 2h8c1.1 0 2-.9 2-2V7H6v12zM19 4h-3.5l-1-1h-5l-1 1H5v2h14V4z",
        "edit" => "M3 17.25V21h3.75L17.81 9.94l-3.75-3.75L3 17.25zM20.71 7.04c.39-.39.39-1.02 0-1.41l-2.34-2.34a.9959.9959 0 0 0-1.41 0l-1.83 1.83 3.75 3.75 1.83-1.83z",
        "download" => "M19 9h-4V3H9v6H5l7 7 7-7zM5 18v2h14v-2H5z",
        "check" => "M9 16.17L4.83 12l-1.42 1.41L9 19 21 7l-1.41-1.41z",
        "check-circle" => "M12 2C6.48 2 2 6.48 2 12s4.48 10 10 10 10-4.48 10-10S17.52 2 12 2zm-2 15l-5-5 1.41-1.41L10 14.17l7.59-7.59L19 8l-9 9z",
        "alert" => "M1 21h22L12 2 1 21zm12-3h-2v-2h2v2zm0-4h-2v-4h2v4z",
        "kebab" => "M12 8c1.1 0 2-.9 2-2s-.9-2-2-2-2 .9-2 2 .9 2 2 2zm0 2c-1.1 0-2 .9-2 2s.9 2 2 2 2-.9 2-2-.9-2-2-2zm0 6c-1.1 0-2 .9-2 2s.9 2 2 2 2-.9 2-2-.9-2-2-2z",
        "chevron-down" => "M16.59 8.59L12 13.17 7.41 8.59 6 10l6 6 6-6z",
        "queue" => "M15 6H3v2h12V6zm0 4H3v2h12v-2zM3 16h8v-2H3v2zM17 6v8.18c-.31-.11-.65-.18-1-.18-1.66 0-3 1.34-3 3s1.34 3 3 3 3-1.34 3-3V8h3V6h-5z",
        "person" => "M12 12c2.21 0 4-1.79 4-4s-1.79-4-4-4-4 1.79-4 4 1.79 4 4 4zm0 2c-2.67 0-8 1.34-8 4v2h16v-2c0-2.66-5.33-4-8-4z",
        "album" => "M12 2C6.48 2 2 6.48 2 12s4.48 10 10 10 10-4.48 10-10S17.52 2 12 2zm0 14.5c-2.49 0-4.5-2.01-4.5-4.5S9.51 7.5 12 7.5s4.5 2.01 4.5 4.5-2.01 4.5-4.5 4.5zm0-5.5c-.55 0-1 .45-1 1s.45 1 1 1 1-.45 1-1-.45-1-1-1z",
        "settings" => "M19.14 12.94c.04-.3.06-.61.06-.94 0-.32-.02-.64-.07-.94l2.03-1.58a.49.49 0 00.12-.61l-1.92-3.32a.488.488 0 00-.59-.22l-2.39.96c-.5-.38-1.03-.7-1.62-.94l-.36-2.54a.484.484 0 00-.48-.41h-3.84c-.24 0-.43.17-.47.41l-.36 2.54c-.59.24-1.13.57-1.62.94l-2.39-.96c-.22-.08-.47 0-.59.22L2.74 8.87c-.12.21-.08.47.12.61l2.03 1.58c-.05.3-.09.63-.09.94s.02.64.07.94l-2.03 1.58a.49.49 0 00-.12.61l1.92 3.32c.12.22.37.29.59.22l2.39-.96c.5.38 1.03.7 1.62.94l.36 2.54c.05.24.24.41.48.41h3.84c.24 0 .44-.17.47-.41l.36-2.54c.59-.24 1.13-.56 1.62-.94l2.39.96c.22.08.47 0 .59-.22l1.92-3.32c.12-.22.07-.47-.12-.61l-2.01-1.58zM12 15.6c-1.98 0-3.6-1.62-3.6-3.6s1.62-3.6 3.6-3.6 3.6 1.62 3.6 3.6-1.62 3.6-3.6 3.6z",
        _ => "",
    };
    html! {
        <svg viewBox="0 0 24 24" fill="currentColor"><path d={d.to_string()} /></svg>
    }
}

pub fn cover(url: &str, class: &'static str) -> Html {
    if url.is_empty() {
        html! { <div class={classes!(class, "cover-fallback")}>{ icon("music") }</div> }
    } else {
        html! { <img class={class} src={url.to_string()} loading="lazy" /> }
    }
}

/// The Rift logo mark: a cosmic cracked tile (violet nebula split by a glowing
/// rift). Rendered from `logo.svg` via `<img>` rather than inline SVG, because
/// loading an SVG as an image renders its gradients/filters fully — `html!`
/// doesn't reliably render inline camelCase gradient elements.
pub fn logo_mark() -> Html {
    html! { <img class="logo-mark" src="logo.svg" alt="" /> }
}

/// Render a track's artist credits, each one a profile link where YouTube
/// provides a channel ID. Falls back to the plain joined string for tracks
/// saved before per-artist credits existed.
pub fn artist_links(artists: &[ArtistRef], fallback: &str, on_open: &Callback<String>) -> Html {
    if artists.is_empty() {
        return html! { { fallback.to_string() } };
    }
    html! {
        <>
            { for artists.iter().enumerate().map(|(i, a)| {
                let name = a.name.clone();
                let link = a.id.clone().map(|id| {
                    let cb = on_open.clone();
                    Callback::from(move |e: MouseEvent| {
                        e.stop_propagation();
                        cb.emit(id.clone());
                    })
                });
                html! {
                    <>
                        if i > 0 { { ", " } }
                        if let Some(link) = link {
                            <span class="link" onclick={link}>{ name }</span>
                        } else {
                            { name }
                        }
                    </>
                }
            }) }
        </>
    }
}

pub fn fmt_secs(total: u32) -> String {
    let (h, m, s) = (total / 3600, (total % 3600) / 60, total % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// Animated equalizer bars shown beside a playing collection. `playing` false
/// renders them frozen (paused).
pub(crate) fn eq_bars(playing: bool) -> Html {
    html! {
        <span class={classes!("eq", (!playing).then_some("paused"))}>
            <span></span><span></span><span></span>
        </span>
    }
}

pub(crate) fn fmt_pos(secs: f64) -> String {
    fmt_secs(secs.max(0.0) as u32)
}
