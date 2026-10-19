//! Probe: fetch a playlist and a track's credits.
//! cargo run -p rift-ytmusic --example dump_more

use rift_ytmusic::Client;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = Client::new()?;

    let playlist = client
        .playlist("PL2_OBreMn7FrsiSW0VDZjdq0xqUKkZYHT", 300)
        .await?;
    println!("\"{}\" — {} tracks", playlist.name, playlist.tracks.len());
    for t in playlist.tracks.iter().take(3) {
        println!("  - {} \"{}\" by {}", t.id, t.title, t.artist);
    }

    let (artists, album, album_id) = client.track_credits("khnokW3Mw24").await?;
    let artists: Vec<String> = artists
        .iter()
        .map(|a| format!("{} ({:?})", a.name, a.id))
        .collect();
    println!(
        "credits: artists=[{}], album={:?} ({:?})",
        artists.join(", "),
        album,
        album_id
    );

    Ok(())
}
