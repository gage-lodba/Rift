//! Probe: fetch an album page and an artist page.
//! cargo run -p rift-ytmusic --example browse_dump

use rift_ytmusic::Client;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = Client::new()?;

    let album = client.album("MPREb_K8qWMWVqXGi").await?;
    let year = album.year.map(|y| format!(" ({y})")).unwrap_or_default();
    println!(
        "\"{}\" by {}{} [{}] — {} tracks",
        album.name,
        album.artist,
        year,
        album.album_type,
        album.tracks.len()
    );

    let artist = client.artist("UCRr1xG_2WIDs18a6cIiCxeA").await?;
    let subs = artist
        .subscribers
        .map(|s| format!(" ({s} subs)"))
        .unwrap_or_default();
    let desc = artist
        .description
        .as_deref()
        .map(|d| {
            let mut c = d.chars();
            let head: String = c.by_ref().take(60).collect();
            if c.next().is_some() {
                format!("{head}…")
            } else {
                head
            }
        })
        .unwrap_or_default();
    println!(
        "\"{}\"{} — {} top songs, {} albums",
        artist.name,
        subs,
        artist.tracks.len(),
        artist.albums.len()
    );
    println!("  description: {desc}");
    for t in artist.tracks.iter().take(3) {
        println!("  - {} \"{}\"", t.id, t.title);
    }
    for a in artist.albums.iter().take(3) {
        let y = a.year.map(|y| format!(" ({y})")).unwrap_or_default();
        println!("  [{}] \"{}\"{}", a.album_type, a.name, y);
    }

    Ok(())
}
