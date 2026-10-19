//! Dev tool: print what each fixture parses into, for writing test assertions.
//!
//! ```text
//! cargo run -p rift-ytmusic --example fixture_report
//! ```

use std::fs;
use std::path::PathBuf;

use rift_ytmusic::raw;
use serde_json::Value;

fn load(name: &str) -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    serde_json::from_str(&fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!("read {}: {e}", path.display());
    }))
    .expect("fixture is valid JSON")
}

fn main() {
    let tracks = raw::tracks(&load("search_tracks.json"));
    println!("== search_tracks: {} tracks ==", tracks.len());
    for t in &tracks {
        println!(
            "   {} | {} | {} | {:?} | {:?}",
            t.id, t.title, t.artist, t.duration, t.album_id
        );
    }

    let artists = raw::artists(&load("search_artists.json"));
    println!("== search_artists: {} artists ==", artists.len());
    for a in &artists {
        println!("   {} | {} | {:?}", a.id, a.name, a.subscribers);
    }

    let albums = raw::albums(&load("search_albums.json"));
    println!("== search_albums: {} albums ==", albums.len());
    for a in &albums {
        println!(
            "   {} | {} | {} | {:?} | {:?} | {}",
            a.id, a.name, a.artist, a.year, a.artist_id, a.album_type
        );
    }

    let album = raw::album_page(&load("album.json"), "MPREb_K8qWMWVqXGi").expect("album page");
    println!(
        "== album: {:?} by {:?} ({:?}) [{}] — {} tracks ==",
        album.name,
        album.artist,
        album.year,
        album.album_type,
        album.tracks.len()
    );
    for t in &album.tracks {
        println!(
            "   {} | {} | {} | {:?} | {:?}",
            t.id, t.title, t.artist, t.duration, t.album
        );
    }

    let artist =
        raw::artist_page(&load("artist.json"), "UCRr1xG_2WIDs18a6cIiCxeA").expect("artist");
    println!(
        "== artist: {:?} subs={:?} tracks={} albums={} showall={:?} ==",
        artist.name,
        artist.subscribers,
        artist.tracks.len(),
        artist.albums.len(),
        artist.tracks_playlist_id
    );
    for t in &artist.tracks {
        println!(
            "   T {} | {} | {} | {:?} | {:?}",
            t.id, t.title, t.artist, t.album, t.album_id
        );
    }
    for a in &artist.albums {
        println!(
            "   A {} | {} | {:?} | {}",
            a.id, a.name, a.year, a.album_type
        );
    }

    let page = raw::playlist_page(&load("playlist.json"));
    let (name, pl, cont) = (page.name, page.tracks, page.continuation);
    println!(
        "== playlist: {name:?} tracks={} items={} next_page={} ==",
        pl.len(),
        page.items_seen,
        cont.is_some()
    );
    for t in pl.iter().take(5) {
        println!(
            "   {} | {} | {} | {:?}",
            t.id, t.title, t.artist, t.duration
        );
    }

    let credits = raw::track_credits(&load("next.json"), "khnokW3Mw24");
    println!("== next credits: {credits:?} ==");
}
