//! Dev tool: compact a recorded innertube response into a test fixture.
//!
//! The raw dumps in `target/*.json` are multi-megabyte pages whose bulk is
//! tracking params, logging directives, and UI we never read. This keeps only
//! the renderer subtrees the parsers walk, and inside them only the keys the
//! parsers actually read. It then re-parses both the original and the compacted
//! document and refuses to write unless the two agree exactly — so a pruned
//! fixture can never silently change what a test asserts.
//!
//! ```text
//! cargo run -p rift-ytmusic --example prune_fixture -- target/search_response.json tracks crates/ytmusic/tests/fixtures/search_tracks.json
//! ```
//!
//! Kinds: `tracks`, `artists`, `albums`, `album`, `artist`, `playlist`, `next`.

use std::fs;
use std::path::Path;

use rift_ytmusic::raw;
use serde_json::{json, Map, Value};

/// Renderer keys located with a recursive search — the tree is only walked
/// along paths that can reach one of these.
///
/// The sets are per response *shape*, because a key that roots one surface can
/// be a plain payload key on another: `musicVideoType` is what the list parsers
/// filter UGC by, but on a `/next` response it is a leaf inside the queue item,
/// and rooting the walk there would cut it off above the byline.
const LIST_ROOTS: &[&str] = &[
    "musicResponsiveListItemRenderer",
    "musicVideoType",
    "musicResponsiveHeaderRenderer",
    "musicImmersiveHeaderRenderer",
    "subscriberCountText",
    "bottomEndpoint",
    "musicCarouselShelfRenderer",
    "musicTwoRowItemRenderer",
    "nextContinuationData",
];

/// `/next` (watch-next) responses: the byline is the only thing read.
const NEXT_ROOTS: &[&str] = &["longBylineText"];

/// Playlist pages: the item list, the header name, and the continuation node.
const PLAYLIST_ROOTS: &[&str] = &[
    "musicResponsiveListItemRenderer",
    "musicVideoType",
    "musicResponsiveHeaderRenderer",
];

/// Keys the parsers read, plus the container keys the walk must pass through to
/// reach a renderer. Anything else inside an unmatched node is dropped.
///
/// The whitelist is a union across response shapes — it has to be, since a key
/// that is a container on one surface is a payload leaf on another. `main`
/// reports which entries actually contributed, so a key that stops earning its
/// place shows up as unused.
const KEEP_KEYS: &[&str] = &[
    // --- structure the walk passes through to reach a renderer ---
    "tabbedSearchResultsRenderer",
    "tabs",
    "tabRenderer",
    "content",
    "sectionListRenderer",
    "contents",
    "itemSectionRenderer",
    "musicCardShelfRenderer",
    "musicShelfRenderer",
    "musicPlaylistShelfRenderer",
    "gridRenderer",
    "items",
    "twoColumnBrowseResultsRenderer",
    "secondaryContents",
    "singleColumnBrowseResultsRenderer",
    "singleColumnMusicWatchNextResultsRenderer",
    "tabbedRenderer",
    "watchNextTabbedResultsRenderer",
    "musicQueueRenderer",
    "playlistPanelRenderer",
    "playlistPanelVideoRenderer",
    "continuations",
    "tabs",
    // --- renderers and the containers they hang off ---
    "header",
    "musicResponsiveListItemRenderer",
    "musicItemThumbnailOverlayRenderer",
    "musicPlayButtonRenderer",
    "playNavigationEndpoint",
    "watchEndpoint",
    "playlistItemData",
    "musicResponsiveListItemFlexColumnRenderer",
    "musicResponsiveListItemFixedColumnRenderer",
    "musicThumbnailRenderer",
    "musicTwoRowItemRenderer",
    "thumbnailRenderer",
    // --- item identity / links ---
    "videoId",
    "browseId",
    "navigationEndpoint",
    "browseEndpoint",
    "overlay",
    "fixedColumns",
    "flexColumns",
    // --- text ---
    "text",
    "runs",
    // --- header / shelf titles ---
    "title",
    "musicCarouselShelfBasicHeaderRenderer",
    // --- thumbnails ---
    "thumbnail",
    "thumbnails",
    "url",
    "width",
    // --- album/artist page headers ---
    "straplineTextOne",
    "subtitle",
    "description",
    // --- artist page ---
    "subscriberCountText",
    // --- "Show all songs" ---
    "bottomEndpoint",
    // --- album/single classification and UGC filtering ---
    "musicVideoType",
    // --- playlist pagination ---
    "nextContinuationData",
    "continuation",
    // --- track credits ---
    "longBylineText",
];

/// What a fixture is parsed as, so the pruner can verify the round-trip.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Tracks,
    Artists,
    Albums,
    Album,
    Artist,
    Playlist,
    Next,
}

impl Kind {
    fn parse(self, v: &Value, browse_id: &str, video_id: &str) -> String {
        match self {
            Kind::Tracks => format!("{:?}", raw::tracks(v)),
            Kind::Artists => format!("{:?}", raw::artists(v)),
            Kind::Albums => format!("{:?}", raw::albums(v)),
            Kind::Album => raw::album_page(v, browse_id)
                .map(|p| format!("{p:?}"))
                .unwrap_or_else(|| "<none>".to_owned()),
            Kind::Artist => raw::artist_page(v, browse_id)
                .map(|p| format!("{p:?}"))
                .unwrap_or_else(|| "<none>".to_owned()),
            Kind::Playlist => {
                let page = raw::playlist_page(v);
                format!(
                    "name={:?} cont={} {:?}",
                    page.name,
                    page.continuation.is_some(),
                    page.tracks
                )
            }
            Kind::Next => format!("{:?}", raw::track_credits(v, video_id)),
        }
    }
}

/// Pruning policy: which subtrees the parsers locate by search, and which keys
/// the walk may pass through to reach them.
struct Opts {
    semantic: &'static [&'static str],
    keep: &'static [&'static str],
    /// Whether `nextContinuationData` should be preserved with its surrounding
    /// page structure. A playlist fixture exists to be paginated, so its whole
    /// continuation page is kept; elsewhere that key is only a payload field the
    /// display parsers ignore, and the walk prunes it like any other node.
    nested_continuations: bool,
}

/// Prune an object to the keys the parsers read.
///
/// Two-phase: a node matched by `opts.semantic` is kept completely verbatim
/// (the parsers locate those renderers by search and then read their payload
/// field-by-field, so pruning inside one would silently change what the fixture
/// proves), and every other node is rebuilt from the [`KEEP_KEYS`] skeleton that
/// leads down to the next match.
fn prune(v: &Value, opts: &Opts) -> Option<Value> {
    match v {
        Value::Object(map) => {
            // A continuation page is a real response the code under test
            // follows, so it is kept intact rather than pruned to a match.
            if opts.nested_continuations && map.contains_key("nextContinuationData") {
                return Some(v.clone());
            }
            if map.keys().any(|k| opts.semantic.contains(&k.as_str())) {
                if std::env::var_os("PRUNE_TRACE").is_some() {
                    eprintln!("MATCH {:?}", map.keys().collect::<Vec<_>>());
                }
                return Some(v.clone());
            }
            if std::env::var_os("PRUNE_TRACE").is_some() {
                eprintln!("nomatch {:?}", map.keys().collect::<Vec<_>>());
            }
            let mut out = Map::new();
            for (k, child) in map {
                if !opts.keep.contains(&k.as_str()) {
                    continue;
                }
                if let Some(kept) = prune(child, opts) {
                    out.insert(k.clone(), kept);
                }
            }
            if out.is_empty() {
                None
            } else {
                Some(Value::Object(out))
            }
        }
        Value::Array(items) => {
            let kept: Vec<Value> = items.iter().filter_map(|c| prune(c, opts)).collect();
            if kept.is_empty() {
                None
            } else {
                Some(Value::Array(kept))
            }
        }
        _ => None,
    }
}

/// Assemble the compacted document, keeping each parser's root path intact.
///
/// `browse_id` is what the parser is called with in production (it comes from
/// the caller, not the response), and `album_page` stamps it onto every track,
/// so the fixture must be parsed with the same id the real page was fetched by.
fn compact(kind: Kind, original: &Value, browse_id: &str, opts: &Opts) -> Value {
    let mut out = Map::new();

    if kind == Kind::Album {
        // `album_page` reads the track list from the `/browse` response root.
        if let Some(secondary) =
            original.pointer("/contents/twoColumnBrowseResultsRenderer/secondaryContents")
        {
            if let Some(kept) = prune(secondary, opts) {
                out.insert(
                    "contents".to_owned(),
                    json!({ "twoColumnBrowseResultsRenderer": { "secondaryContents": kept } }),
                );
            }
        }
        // The header is located by recursive search, so any shape works.
        if let Some(header) = first_of(original, "musicResponsiveHeaderRenderer") {
            out.insert(
                "header".to_owned(),
                json!({ "musicResponsiveHeaderRenderer": header }),
            );
        }
    } else {
        if let Some(contents) = original.get("contents") {
            if std::env::var_os("PRUNE_TRACE").is_some() {
                eprintln!("compact: contents is {}", type_name(contents));
            }
            if let Some(kept) = prune(contents, opts) {
                out.insert("contents".to_owned(), kept);
            }
        }
        if kind == Kind::Artist {
            if let Some(header) = original.get("header") {
                if let Some(kept) = prune(header, opts) {
                    out.insert("header".to_owned(), kept);
                }
            }
        }
    }

    if kind == Kind::Album && !browse_id.is_empty() {
        // Informational: album pages are backed by a playlist (`OLAK5uy_…`) or
        // carry the album id (`MPREb_…`) in their canonical URL, but neither is
        // guaranteed, so this only reports what the dump looks like.
        if let Some(url) = original
            .pointer("/microformat/microformatDataRenderer/urlCanonical")
            .and_then(Value::as_str)
        {
            assert!(
                url.contains("MPREb_") || url.contains("OLAK5uy_") || url.contains("list="),
                "unexpected album dump shape: {url}"
            );
        }
    }

    Value::Object(out)
}

/// The first `key` renderer found anywhere in the tree, pruned.
fn first_of(v: &Value, key: &str) -> Option<Value> {
    match v {
        Value::Object(map) => {
            if let Some(child) = map.get(key) {
                return Some(child.clone());
            }
            map.values().find_map(|c| first_of(c, key))
        }
        Value::Array(items) => items.iter().find_map(|c| first_of(c, key)),
        _ => None,
    }
}

/// Every key used anywhere in a document — for reporting whitelist entries that
/// no longer contribute.
fn used_keys(v: &Value, out: &mut std::collections::HashSet<String>) {
    match v {
        Value::Object(map) => {
            for (k, child) in map {
                out.insert(k.clone());
                used_keys(child, out);
            }
        }
        Value::Array(items) => items.iter().for_each(|c| used_keys(c, out)),
        _ => {}
    }
}

/// Debug: the first few key paths leading to `key`.
fn find_key_paths(
    v: &Value,
    key: &str,
    path: &mut Vec<String>,
    out: &mut Vec<Vec<String>>,
    limit: usize,
) {
    if out.len() >= limit {
        return;
    }
    match v {
        Value::Object(map) => {
            for (k, child) in map {
                if k == key {
                    out.push(path.clone());
                }
                if out.len() >= limit {
                    return;
                }
                path.push(k.clone());
                find_key_paths(child, key, path, out, limit);
                path.pop();
            }
        }
        Value::Array(items) => {
            for (i, child) in items.iter().enumerate() {
                path.push(format!("[{i}]"));
                find_key_paths(child, key, path, out, limit);
                path.pop();
            }
        }
        _ => {}
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (input, kind, out, browse_id, video_id) = match args.as_slice() {
        [input, kind, out] => (input, kind, out, "", ""),
        [input, kind, out, browse_id] => (input, kind, out, browse_id.as_str(), ""),
        [input, kind, out, browse_id, video_id] => {
            (input, kind, out, browse_id.as_str(), video_id.as_str())
        }
        _ => {
            eprintln!(
                "usage: prune_fixture <input.json> <kind> <output.json> [browseId] [videoId]"
            );
            eprintln!("kinds: tracks artists albums album artist playlist next");
            eprintln!("browseId is required for `album` and `artist` (it comes from the");
            eprintln!("caller in production, and the parser stamps it onto the result)");
            eprintln!("videoId is required for `next` (track_credits matches the byline to the");
            eprintln!("requested video, so the fixture must be parsed with the same id)");
            std::process::exit(2);
        }
    };
    let kind = match kind.as_str() {
        "tracks" => Kind::Tracks,
        "artists" => Kind::Artists,
        "albums" => Kind::Albums,
        "album" => Kind::Album,
        "artist" => Kind::Artist,
        "playlist" => Kind::Playlist,
        "next" => Kind::Next,
        other => return Err(format!("unknown kind `{other}`").into()),
    };

    let opts = Opts {
        semantic: if kind == Kind::Next {
            NEXT_ROOTS
        } else if kind == Kind::Playlist {
            // A playlist fixture is followed page by page, so its response
            // headers and continuation nodes are the point, not decoration.
            PLAYLIST_ROOTS
        } else {
            LIST_ROOTS
        },
        keep: KEEP_KEYS,
        nested_continuations: kind == Kind::Playlist,
    };

    let raw_text = fs::read_to_string(input)?;
    let original: Value = serde_json::from_str(&raw_text)?;
    let before = kind.parse(&original, browse_id, video_id);

    let compacted = compact(kind, &original, browse_id, &opts);
    if std::env::var_os("PRUNE_TRACE").is_some() {
        eprintln!(
            "compacted root keys: {:?}",
            compacted.as_object().map(|m| m.keys().collect::<Vec<_>>())
        );
    }
    if std::env::var_os("PRUNE_DEBUG").is_some() {
        eprintln!("-- key paths to each semantic renderer --");
        for key in opts.semantic {
            let mut found = Vec::new();
            find_key_paths(&original, key, &mut Vec::new(), &mut found, 2);
            for p in &found {
                eprintln!("   {key}: {}", p.join("."));
            }
        }
    }
    let after = kind.parse(&compacted, browse_id, video_id);

    // NOTE: this proves the compacted document parses the *same as itself* — it
    // is an equivalence check between the original and the compacted copy, not
    // a correctness check on the parser. Both sides run the same parser, so a
    // parser that returns the wrong thing returns the same wrong thing twice
    // and this passes. Its job is to guarantee pruning did not lose any input
    // the parsers read; correctness is what `tests/fixtures.rs` asserts, against
    // values captured from the real responses.
    if before != after {
        // Leave the compacted document behind for `PRUNE_KEEP=1` debugging.
        if std::env::var_os("PRUNE_KEEP").is_some() {
            if let Some(parent) = Path::new(out).parent() {
                let _ = fs::create_dir_all(parent);
            }
            let _ = fs::write(out, serde_json::to_vec(&compacted)?);
        }
        eprintln!("REFUSING TO WRITE: pruning changed the parse result.");
        let (b, a) = (before.as_bytes(), after.as_bytes());
        let at = b.iter().zip(a).position(|(x, y)| x != y).unwrap_or(0);
        let from = at.saturating_sub(200);
        eprintln!("first difference at byte {at}");
        eprintln!("before: …{}", truncate(&before[from..], 600));
        eprintln!(
            "after:  …{}",
            truncate(&after[from.min(after.len())..], 600)
        );
        std::process::exit(1);
    }

    let bytes = serde_json::to_vec(&compacted)?;
    if let Some(parent) = Path::new(out).parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(out, &bytes)?;
    println!(
        "{input} ({} KiB) -> {out} ({} KiB) — parse verified identical",
        raw_text.len() / 1024,
        bytes.len() / 1024,
    );

    if std::env::var_os("PRUNE_REPORT").is_some() {
        let mut used = std::collections::HashSet::new();
        used_keys(&compacted, &mut used);
        let unused: Vec<&&str> = KEEP_KEYS.iter().filter(|k| !used.contains(**k)).collect();
        println!("  whitelist entries not present in this fixture: {unused:?}");
    }
    Ok(())
}

fn truncate(s: &str, n: usize) -> String {
    if s.len() <= n {
        return s.to_owned();
    }
    let mut end = n;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

/// Debug: a JSON value's shape, for tracing the walk.
fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}
