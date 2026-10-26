//! Live smoke test for the native resolver: resolve video ids and range-download
//! each returned stream.
//!
//! Networked and manual — never run in CI (see `docs/native-streaming.md` §6).
//! It answers the two questions a resolver change needs answered against the
//! real site:
//!
//! 1. does the innertube client chain still resolve a directly downloadable
//!    itag-140 stream (and which client served it), and
//! 2. does a *ranged* GET of the returned URL come back `206` with the reported
//!    byte count (an un-ranged GET is throttled; see §2.3).
//!
//! With no arguments it runs the wider 20-id coverage sample from §3.1: ATV and
//! OMV uploads plus one UGC video, from 19 s to ~9 min, 0.3–9 MB. Passing ids
//! runs just those instead.
//!
//! ```text
//! cargo run -p rift-ytmusic --example resolve_stream -- K0HSD_i2DvA Jb6gcoR266U
//! ```
//!
//! Resolved URLs are signed and short-lived, so the output names only the host
//! and the format; it never prints the URL itself.

use std::time::{Duration, Instant};

use reqwest::header;

/// Exit code used for a failed or un-206'd probe, so a script can distinguish it
/// from a build/argument error.
const PROBE_FAILED: i32 = 1;

/// The wider live-coverage sample: the five Phase-4 ids, the nine-track
/// `artifacts/native-stream/sample-coverage.json` sample, one 19-second UGC video
/// (`jNQXAC9IVRw`), and other OMV hits across ~3–9 minutes. Kept in the example
/// (not a test) because it needs the network.
const DEFAULT_IDS: [&str; 20] = [
    "K0HSD_i2DvA",
    "Jb6gcoR266U",
    "dQw4w9WgXcQ",
    "kJQP7kiw5Fk",
    "fJ9rUzIMcZQ",
    "OPf0YbXqDm0",
    "hTWKbfoikeg",
    "YQHsXMglC9A",
    "CevxZvSJLk8",
    "jNQXAC9IVRw",
    "9bZkp7q19f0",
    "JGwWNGJdvx8",
    "60ItHLz5WEA",
    "2Vv-BfVoq4g",
    "RgKAFK5djSk",
    "kXYiU_JCYtU",
    "eVTXPUF4Oz4",
    "hT_nvWreIhg",
    "rYEDA3JcQqw",
    "8SbUC-UaAxE",
];

#[tokio::main]
async fn main() {
    let ids: Vec<String> = std::env::args().skip(1).collect();
    let ids: Vec<String> = if ids.is_empty() {
        DEFAULT_IDS.iter().map(|id| (*id).to_owned()).collect()
    } else {
        ids
    };

    let http = match reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        // A full ranged GET of a multi-megabyte track must fit; 120 s is the
        // same order as the app's own audio fetch ceiling.
        .timeout(Duration::from_secs(120))
        .build()
    {
        Ok(http) => http,
        Err(error) => {
            eprintln!("could not build the probe HTTP client: {error}");
            std::process::exit(2);
        }
    };

    let mut failed = 0_usize;
    for id in &ids {
        let started = Instant::now();
        let stream = match rift_ytmusic::player::resolve(id).await {
            Ok(stream) => stream,
            Err(error) => {
                println!("{id}: RESOLVE FAILED: {error}");
                failed += 1;
                continue;
            }
        };
        let host = reqwest::Url::parse(&stream.url)
            .ok()
            .and_then(|url| url.host_str().map(str::to_owned))
            .unwrap_or_else(|| "<unparseable>".to_owned());
        println!(
            "{id}: client {} itag {} {} host {host} content_length {:?} resolved in {:?}",
            stream.client,
            stream.itag,
            stream.mime_type,
            stream.content_length,
            started.elapsed()
        );

        // The exact ranged request the native downloader is specified to make:
        // `bytes=0-(contentLength-1)` fetches the whole file at full speed.
        let range = match stream.content_length {
            Some(length) if length > 0 => format!("bytes=0-{}", length - 1),
            _ => "bytes=0-".to_owned(),
        };
        let response = match http
            .get(&stream.url)
            .header(header::RANGE, range)
            .send()
            .await
        {
            Ok(response) => response,
            Err(error) => {
                println!("{id}: RANGED GET FAILED: {error}");
                failed += 1;
                continue;
            }
        };
        let status = response.status();
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("<none>")
            .to_owned();
        let bytes = match response.bytes().await {
            Ok(bytes) => bytes,
            Err(error) => {
                println!("{id}: RANGED GET BODY FAILED: {error}");
                failed += 1;
                continue;
            }
        };
        let length_ok = stream
            .content_length
            .is_none_or(|length| length == bytes.len() as u64);
        println!(
            "  ranged GET: {status} {content_type} {} bytes in {:?} (length matches: {length_ok})",
            bytes.len(),
            started.elapsed()
        );
        let mut ok = status == reqwest::StatusCode::PARTIAL_CONTENT
            && content_type.starts_with("audio/")
            && length_ok;
        if status != reqwest::StatusCode::PARTIAL_CONTENT {
            println!("  FAILED: expected 206 Partial Content — range requests are load-bearing");
        } else if !content_type.starts_with("audio/") {
            println!("  FAILED: expected an audio/* content type, got {content_type}");
        } else if !length_ok {
            println!("  FAILED: body length differs from the reported contentLength");
        } else if let Some(length) = stream.content_length.filter(|length| *length > 1_048_576) {
            // The second half of the range contract: a mid-offset range must
            // also be served (some clients resolve fine at byte 0 but gate
            // later offsets with a 403 — the `ANDROID_VR` failure mode in
            // docs §3.1). A 64 KiB probe is enough to prove the offset path.
            let mid = length / 2;
            let end = (mid + 65_535).min(length - 1);
            let expected = (end - mid + 1) as usize;
            let mid_range = format!("bytes={mid}-{end}");
            match http
                .get(&stream.url)
                .header(header::RANGE, &mid_range)
                .send()
                .await
            {
                Ok(response) if response.status() == reqwest::StatusCode::PARTIAL_CONTENT => {
                    match response.bytes().await {
                        Ok(bytes) if bytes.len() == expected => {
                            println!("  mid-offset range {mid_range}: 206, {} bytes", bytes.len());
                        }
                        Ok(bytes) => {
                            println!(
                                "  FAILED: mid-offset range {mid_range} returned {} bytes, expected {expected}",
                                bytes.len()
                            );
                            ok = false;
                        }
                        Err(error) => {
                            println!("  FAILED: mid-offset range body: {error}");
                            ok = false;
                        }
                    }
                }
                Ok(response) => {
                    println!(
                        "  FAILED: mid-offset range {mid_range} returned {}",
                        response.status()
                    );
                    ok = false;
                }
                Err(error) => {
                    println!("  FAILED: mid-offset range request: {error}");
                    ok = false;
                }
            }
        }
        if !ok {
            failed += 1;
        }
    }

    println!(
        "RESULT: {}/{} ids resolved and ranged-GET-validated",
        ids.len() - failed,
        ids.len()
    );
    if failed > 0 {
        std::process::exit(PROBE_FAILED);
    }
}
