//! Manual live probe of the **native** playback fetch path.
//!
//! This is **not** a CI test. It requires outbound network access and exercises
//! `rift::fetch::fetch_bytes` exactly as playback does: native resolution
//! (`rift_ytmusic::player`, JS-less `VISIONOS` client) → ranged download
//! (`rift::stream`) → decode with the playback decoder. It exists for the
//! native-only world: there is no oracle backend anymore, so this checks that
//! every sampled id resolves, downloads a plausible amount of bytes and decodes
//! to a real duration.
//!
//! Run it from the repository root:
//!
//! ```text
//! cargo run -p rift --example native_probe
//! cargo run -p rift --example native_probe -- K0HSD_i2DvA Jb6gcoR266U
//! ```
//!
//! Exit status: 0 when every requested id resolved + downloaded + decoded; 1
//! otherwise (the table says which id failed and why). The probe always runs one
//! known-invalid id and requires the native error to carry YouTube's
//! "unavailable" reason, pinning the error mapping the UI/toasts rely on.

use std::io::Cursor;
use std::time::Instant;

use anyhow::Context;
use rodio::{Decoder, Source};

/// Ids used by the other native-streaming probes: a mix of ATV (topic-channel
/// audio) and official music-video uploads with different durations/sizes.
const DEFAULT_IDS: [&str; 5] = [
    "K0HSD_i2DvA",
    "Jb6gcoR266U",
    "dQw4w9WgXcQ",
    "fJ9rUzIMcZQ",
    "hTWKbfoikeg",
];

/// A syntactically valid but nonexistent id: YouTube answers
/// `playabilityStatus` `ERROR` / "This video is unavailable", which the resolver
/// maps to `Error::Unavailable` with that reason.
const INVALID_ID: &str = "xXxXxXxXxXx";

/// One id's attempt through the playback fetch path.
struct Row {
    id: String,
    outcome: Result<Outcome, String>,
}

struct Outcome {
    bytes: usize,
    elapsed_ms: u128,
    /// Length the decoder reports, in seconds. `fetch_bytes` itself reports 0.0
    /// (the stream length is unknown until decoded).
    decoded_secs: f64,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("rift=info")),
        )
        .init();

    let mut args: Vec<String> = std::env::args().skip(1).collect();
    args.retain(|arg| !arg.starts_with('-') && arg != "--");
    let ids: Vec<String> = if args.is_empty() {
        DEFAULT_IDS.iter().map(|id| (*id).to_owned()).collect()
    } else {
        args
    };

    println!("native_probe: {} id(s)\n", ids.len());

    let rows: Vec<Row> = {
        let mut rows = Vec::with_capacity(ids.len());
        for id in &ids {
            rows.push(probe(id).await);
        }
        rows
    };

    // Failure-path check: the unavailable id must fail and the error must name
    // YouTube's reason (the UI shows that text).
    let invalid = probe(INVALID_ID).await;
    match &invalid.outcome {
        Ok(outcome) => anyhow::bail!(
            "the invalid id {INVALID_ID} unexpectedly succeeded ({} bytes)",
            outcome.bytes
        ),
        Err(error) => {
            println!("expected-failure check: {} → {error}\n", invalid.id);
            if !error.to_lowercase().contains("unavailable") {
                anyhow::bail!("the invalid-id error should carry the unavailable reason: {error}");
            }
        }
    }

    print_table(&rows);
    let failures = rows
        .iter()
        .filter(|row| row.outcome.is_err())
        .inspect(|row| {
            if let Err(error) = &row.outcome {
                println!("FAIL {}: {error}", row.id);
            }
        })
        .count();
    if failures == 0 {
        println!("\nPASS: every requested id resolved, downloaded and decoded natively.");
        Ok(())
    } else {
        anyhow::bail!("{failures} id(s) failed; see the table above");
    }
}

async fn probe(id: &str) -> Row {
    let started = Instant::now();
    let outcome = match rift::fetch::fetch_bytes(id).await {
        Ok((bytes, _)) => {
            let elapsed_ms = started.elapsed().as_millis();
            match decoded_length(&bytes) {
                Ok(decoded_secs) => Ok(Outcome {
                    bytes: bytes.len(),
                    elapsed_ms,
                    decoded_secs,
                }),
                Err(error) => Err(format!("downloaded but did not decode: {error:#}")),
            }
        }
        Err(error) => Err(format!("{error:#}")),
    };
    Row {
        id: id.to_owned(),
        outcome,
    }
}

/// Length in seconds as the real decoder (the one playback uses) sees it.
fn decoded_length(bytes: &[u8]) -> anyhow::Result<f64> {
    let len = bytes.len() as u64;
    let decoder = Decoder::builder()
        .with_data(Cursor::new(bytes.to_vec()))
        .with_byte_len(len)
        .with_seekable(true)
        .build()
        .context("decoder construction failed")?;
    let secs = decoder
        .total_duration()
        .context("the decoded stream reports no total duration")?;
    Ok(secs.as_secs_f64())
}

fn print_table(rows: &[Row]) {
    println!(
        "{:<14} {:>9} {:>9} {:>7}  status",
        "id", "bytes", "ms", "decoded"
    );
    for row in rows {
        match &row.outcome {
            Ok(outcome) => println!(
                "{:<14} {:>9} {:>9} {:>6.2}s  ok",
                row.id, outcome.bytes, outcome.elapsed_ms, outcome.decoded_secs
            ),
            Err(error) => println!("{:<14} {:>9} {:>9} {:>7}  {}", row.id, "-", "-", "-", error),
        }
    }
}
