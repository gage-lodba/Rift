//! Pipeline smoke test: search -> resolve stream -> download -> decode.
//! Run with: cargo run --example probe [search terms]

use rodio::{Decoder, Source};
use rustypipe::client::RustyPipe;
use std::io::Cursor;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter("rift=debug,probe=debug,rustypipe=info")
        .init();
    let query = std::env::args().skip(1).collect::<Vec<_>>().join(" ");
    let query = if query.is_empty() {
        "daft punk get lucky".into()
    } else {
        query
    };

    let rp = RustyPipe::builder()
        .storage_dir(std::env::temp_dir())
        .build()?;

    let results = rp.query().music_search_tracks(&query).await?;
    let track = results
        .items
        .items
        .first()
        .ok_or_else(|| anyhow::anyhow!("no results"))?;
    println!(
        "search ok: \"{}\" by {} ({})",
        track.name,
        track
            .artists
            .first()
            .map(|a| a.name.as_str())
            .unwrap_or("?"),
        track.id
    );

    let http = reqwest::Client::new();
    let (data, duration) = rift::fetch::fetch_bytes(&rp, &http, &track.id).await?;
    println!("download ok: {} bytes, {duration} s", data.len());

    let decoder = Decoder::new(Cursor::new(data))?;
    println!("decode ok: {:?}", decoder.total_duration());
    Ok(())
}
