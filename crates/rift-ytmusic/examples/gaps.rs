//! Verify the gap fixes: tracks_playlist_id + the video/episode type filter.
//! cargo run -p rift-ytmusic --example gaps

use rift_ytmusic::Client;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = Client::new()?;

    let artist = client.artist("UCRr1xG_2WIDs18a6cIiCxeA").await?;
    println!("tracks_playlist_id = {:?}", artist.tracks_playlist_id);

    if let Some(pl) = &artist.tracks_playlist_id {
        let songs = client.playlist(pl, 300).await?;
        println!("full catalog: {} tracks", songs.tracks.len());
        for t in songs.tracks.iter().take(5) {
            let album = t.album.as_deref().unwrap_or("-");
            println!("  - {} \"{}\" [{}]", t.id, t.title, album);
        }
    }

    let tracks = client.search_tracks("daft punk").await?;
    println!(
        "search_tracks: {} (video/episode types dropped)",
        tracks.len()
    );
    for t in tracks.iter().take(8) {
        println!("  - \"{}\" by {}", t.title, t.artist);
    }

    Ok(())
}
