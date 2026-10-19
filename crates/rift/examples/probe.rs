//! Pipeline smoke test: search -> resolve stream -> download -> decode.
//! Run with: cargo run --example probe [search terms]

use rodio::{Decoder, Source};
use std::io::Cursor;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter("rift=debug,probe=debug")
        .init();
    let query = std::env::args().skip(1).collect::<Vec<_>>().join(" ");
    let query = if query.is_empty() {
        "daft punk get lucky".into()
    } else {
        query
    };

    let yt = rift_ytmusic::Client::new()?;
    let tracks = yt.search_tracks(&query).await?;
    let track = tracks
        .first()
        .ok_or_else(|| anyhow::anyhow!("no results"))?;
    println!("\"{}\" by {} ({})", track.title, track.artist, track.id);

    let (data, duration) = rift::fetch::fetch_bytes(&track.id).await?;
    println!("download ok: {} bytes, {duration} s", data.len());

    let decoder = Decoder::new(Cursor::new(data))?;
    println!("decode ok: {:?}", decoder.total_duration());
    Ok(())
}
