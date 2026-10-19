//! Dump raw search responses for artists and albums (parser debugging).
//! cargo run -p rift-ytmusic --example search_all -- "daft punk"

use rift_ytmusic::{Client, SearchFilter};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let query = std::env::args().skip(1).collect::<Vec<_>>().join(" ");
    let query = if query.is_empty() {
        "daft punk".to_owned()
    } else {
        query
    };

    let client = Client::new()?;
    for (filter, name) in [
        (SearchFilter::Artists, "artists"),
        (SearchFilter::Albums, "albums"),
    ] {
        let raw = client.search_raw(filter, &query).await?;
        std::fs::write(
            format!("target/search_{name}.json"),
            serde_json::to_string_pretty(&raw)?,
        )?;
        println!("dumped {name} -> target/search_{name}.json");
    }
    Ok(())
}
