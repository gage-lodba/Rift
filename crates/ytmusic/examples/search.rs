//! Probe: search tracks, artists, and albums. Run with:
//! cargo run -p rift-ytmusic --example search -- "daft punk"

use rift_ytmusic::Client;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let query = std::env::args().skip(1).collect::<Vec<_>>().join(" ");
    let query = if query.is_empty() {
        "daft punk".to_owned()
    } else {
        query
    };

    let client = Client::new()?;

    let tracks = client.search_tracks(&query).await?;
    println!("TRACKS ({})", tracks.len());
    for t in tracks.iter().take(5) {
        let album = t.album.as_deref().unwrap_or("-");
        println!("  - {} \"{}\" by {} [{}]", t.id, t.title, t.artist, album);
    }

    let artists = client.search_artists(&query).await?;
    println!("ARTISTS ({})", artists.len());
    for a in artists.iter().take(5) {
        let subs = a.subscribers.map(|s| format!(" ({s})")).unwrap_or_default();
        println!("  - {} \"{}\"{}", a.id, a.name, subs);
    }

    let albums = client.search_albums(&query).await?;
    println!("ALBUMS ({})", albums.len());
    for a in albums.iter().take(5) {
        let year = a.year.map(|y| format!(" ({y})")).unwrap_or_default();
        println!("  - {} \"{}\" by {}{}", a.id, a.name, a.artist, year);
    }

    Ok(())
}
