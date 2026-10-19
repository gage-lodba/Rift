//! Live prove-it-still-works smoke test for the shell-framework-free pipeline:
//! YouTube Music search -> yt-dlp stream fetch -> rodio decode.
//!
//! This intentionally does **not** construct [`rift::RiftBackend`]: it exercises
//! the parts that used to be reachable only through legacy IPC commands (the
//! ytmusic client + fetch pipeline) with no window, no audio device and no data
//! directory, so it also proves the library is framework-agnostic.
//!
//! Run with:
//!
//! ```text
//! cargo run -p rift --example smoke [-- search terms]
//! ```
//!
//! yt-dlp resolution order: `RIFT_YTDLP` if set, then the app's own installed
//! copy (`<app data dir>/bin/yt-dlp[.exe]`, which the Settings one-click
//! installer writes), then the normal auto-detection (PATH / common install
//! dirs).

use std::io::Cursor;
use std::path::PathBuf;

use rodio::{Decoder, Source};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter("rift=debug,smoke=info")
        .init();

    // 1. Prefer the yt-dlp this machine already has installed for the app.
    let installed = rift::native_data_dir()
        .ok()
        .map(|dir| {
            dir.join("bin").join(if cfg!(windows) {
                "yt-dlp.exe"
            } else {
                "yt-dlp"
            })
        })
        .filter(|path| path.is_file());
    let override_path = std::env::var_os("RIFT_YTDLP")
        .map(PathBuf::from)
        .or(installed);
    if let Some(path) = &override_path {
        println!("yt-dlp override: {}", path.display());
        rift::fetch::set_ytdlp_override(Some(path.clone()));
    }
    let status = rift::fetch::detect_ytdlp().await;
    println!(
        "yt-dlp detected: found={} path={:?} version={:?}",
        status.found, status.path, status.version
    );
    if !status.found {
        anyhow::bail!("no working yt-dlp — install it or set RIFT_YTDLP");
    }

    // 2. Search YouTube Music through the innertube client.
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

    // 3. Resolve the best audio stream through yt-dlp and download it.
    let (data, duration) = rift::fetch::fetch_bytes(&track.id).await?;
    println!(
        "fetch: {} bytes ({} KiB), reported duration {}s",
        data.len(),
        data.len() / 1024,
        duration
    );

    // 4. Decode the bytes exactly as the audio thread does (in memory, seekable
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
