//! Live prove-it-still-works smoke test for the shell-framework-free pipeline:
//! YouTube Music search -> native stream fetch -> rodio decode.
//!
//! This intentionally does **not** construct [`rift::RiftBackend`]: it exercises
//! the parts that used to be reachable only through legacy IPC commands (the
//! ytmusic client + native fetch pipeline) with no window, no audio device and
//! no data directory, so it also proves the library is framework-agnostic.
//!
//! Run with:
//!
//! ```text
//! cargo run -p rift --example smoke [-- search terms]
//! ```
//!
//! Audio is resolved and downloaded by the native path (`rift_ytmusic::player`
//! + `rift::stream`); there is no external binary in the pipeline.

use std::io::Cursor;

use rodio::{Decoder, Source};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter("rift=debug,smoke=info")
        .init();

    // 1. Search YouTube Music through the innertube client.
    let query = {
        let args = std::env::args().skip(1).collect::<Vec<_>>().join(" ");
        if args.trim().is_empty() {
            "daft punk get lucky".to_string()
        } else {
            args
        }
    };
    let yt = rift_ytmusic::Client::new()?;
    let tracks = yt.search_tracks(&query).await?;
    let track = tracks
        .first()
        .ok_or_else(|| anyhow::anyhow!("search returned nothing for {query:?}"))?;
    println!(
        "search: \"{}\" by {} ({})",
        track.title, track.artist, track.id
    );

    // 2. Resolve the best audio stream natively and download it.
    let (data, duration) = rift::fetch::fetch_bytes(&track.id).await?;
    println!(
        "fetch: {} bytes ({} KiB), reported duration {}s",
        data.len(),
        data.len() / 1024,
        duration
    );

    // 3. Decode the bytes exactly as the audio thread does (in memory, seekable
    //    via the explicit byte length).
    let len = data.len() as u64;
    let decoder = Decoder::builder()
        .with_data(Cursor::new(data))
        .with_byte_len(len)
        .with_seekable(true)
        .build()?;
    println!(
        "decode: ok, decoder duration {:?}",
        decoder.total_duration()
    );

    println!("SMOKE PASS: search -> fetch -> decode");
    Ok(())
}
