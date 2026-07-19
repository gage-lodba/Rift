//! Card grids for albums and artists (search tabs, artist pages) and the
//! shared lazy-results wrapper.

use rift_types::{AlbumSummary, ArtistSummary};
use yew::prelude::*;

use super::icons::cover;

fn format_subs(n: u64) -> String {
    // "1.0M" -> "1M", "1.2M" -> "1.2M"
    fn compact(x: f64, unit: &str) -> String {
        let s = format!("{x:.1}");
        format!("{}{unit}", s.strip_suffix(".0").unwrap_or(&s))
    }
    match n {
        1 => "1 subscriber".into(),
        n if n >= 1_000_000 => format!("{} subscribers", compact(n as f64 / 1e6, "M")),
        n if n >= 1_000 => format!("{} subscribers", compact(n as f64 / 1e3, "K")),
        n => format!("{n} subscribers"),
    }
}

fn album_subtitle(a: &AlbumSummary) -> String {
    match a.year {
        Some(y) => format!("{} • {y}", a.album_type),
        None => a.album_type.clone(),
    }
}

pub fn album_grid(albums: &[AlbumSummary], on_open: Callback<String>) -> Html {
    html! {
        <div class="card-grid">
            { for albums.iter().map(|a| {
                let cb = on_open.clone();
                let id = a.id.clone();
                html! {
                    <div class="card" onclick={Callback::from(move |_| cb.emit(id.clone()))}>
                        { cover(&a.cover, "card-cover") }
                        <div class="card-name">{ &a.name }</div>
                        <div class="card-sub">{ album_subtitle(a) }</div>
                    </div>
                }
            }) }
        </div>
    }
}

pub fn artist_grid(artists: &[ArtistSummary], on_open: Callback<String>) -> Html {
    html! {
        <div class="card-grid">
            { for artists.iter().map(|a| {
                let cb = on_open.clone();
                let id = a.id.clone();
                html! {
                    <div class="card card-artist" onclick={Callback::from(move |_| cb.emit(id.clone()))}>
                        { cover(&a.avatar, "card-cover round") }
                        <div class="card-name">{ &a.name }</div>
                        <div class="card-sub">{ a.subscribers.map(format_subs).unwrap_or_default() }</div>
                    </div>
                }
            }) }
        </div>
    }
}

/// Render a lazily-loaded search tab: a loading state while `None`, an
/// empty state for no results, otherwise the given grid.
pub fn results_view<T>(results: &Option<Vec<T>>, render: impl FnOnce(&[T]) -> Html) -> Html {
    match results {
        None => html! { <div class="empty">{ "Searching..." }</div> },
        Some(v) if v.is_empty() => html! { <div class="empty">{ "No results." }</div> },
        Some(v) => render(v),
    }
}
