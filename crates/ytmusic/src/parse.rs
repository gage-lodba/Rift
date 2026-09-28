//! Tolerant parsing of innertube music responses into `rift_types`.
//!
//! Innertube responses are large, nested, and churn frequently, so parsing is
//! defensive: it walks the JSON looking for the renderers it knows and skips
//! everything else, never failing the whole request because one node changed.

use rift_types::{AlbumPage, AlbumSummary, ArtistPage, ArtistRef, ArtistSummary, Track};
use serde_json::Value;

/// Recursively collect every object that contains `key`.
fn find_all<'a>(v: &'a Value, key: &str, out: &mut Vec<&'a Value>) {
    match v {
        Value::Object(map) => {
            if map.contains_key(key) {
                out.push(v);
            }
            for child in map.values() {
                find_all(child, key, out);
            }
        }
        Value::Array(items) => {
            for child in items {
                find_all(child, key, out);
            }
        }
        _ => {}
    }
}

/// Concatenate the text of every run in a text node.
fn runs_all(v: &Value) -> String {
    v.get("runs")
        .and_then(Value::as_array)
        .map(|runs| {
            runs.iter()
                .filter_map(|r| r.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default()
}

/// Best thumbnail URL: smallest at least 512 wide, else the largest.
fn thumb(urls: &[Value]) -> String {
    let mut best: Option<&Value> = None;
    for u in urls {
        let w = u.get("width").and_then(Value::as_u64).unwrap_or(0);
        match best {
            None => best = Some(u),
            Some(b) => {
                let bw = b.get("width").and_then(Value::as_u64).unwrap_or(0);
                let better = if w >= 512 && bw >= 512 {
                    w < bw
                } else if w >= 512 {
                    true
                } else if bw >= 512 {
                    false
                } else {
                    w > bw
                };
                if better {
                    best = Some(u);
                }
            }
        }
    }
    best.and_then(|u| u.get("url"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

/// The `text` node of `flexColumns[i]`.
fn column_text(node: &Value, i: usize) -> Option<&Value> {
    node.pointer(&format!(
        "/flexColumns/{i}/musicResponsiveListItemFlexColumnRenderer/text"
    ))
}

/// The `browseId` the item's own `navigationEndpoint` points at (its page id).
fn link_id(node: &Value) -> Option<String> {
    node.pointer("/navigationEndpoint/browseEndpoint/browseId")
        .and_then(Value::as_str)
        .map(str::to_owned)
}

/// The item's cover/avatar thumbnail URL.
fn cover(node: &Value) -> String {
    node.pointer("/thumbnail/musicThumbnailRenderer/thumbnail/thumbnails")
        .and_then(Value::as_array)
        .map(|urls| thumb(urls))
        .unwrap_or_default()
}

/// Runs in `flexColumns[col]` that link to a browse page whose id has `prefix`
/// (e.g. `UC` for artists, `MPREb_` for albums).
fn linked_runs<'a>(node: &'a Value, col: usize, prefix: &str) -> Vec<&'a Value> {
    node.pointer(&format!(
        "/flexColumns/{col}/musicResponsiveListItemFlexColumnRenderer/text/runs"
    ))
    .and_then(Value::as_array)
    .map(|runs| {
        runs.iter()
            .filter(|r| {
                r.pointer("/navigationEndpoint/browseEndpoint/browseId")
                    .and_then(Value::as_str)
                    .is_some_and(|id| id.starts_with(prefix))
            })
            .collect()
    })
    .unwrap_or_default()
}

fn linked_text(node: &Value, col: usize, prefix: &str) -> String {
    linked_runs(node, col, prefix)
        .into_iter()
        .filter_map(|r| r.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join(", ")
}

fn linked_id(node: &Value, col: usize, prefix: &str) -> Option<String> {
    linked_runs(node, col, prefix)
        .into_iter()
        .next()
        .and_then(|r| r.pointer("/navigationEndpoint/browseEndpoint/browseId"))
        .and_then(Value::as_str)
        .map(str::to_owned)
}

fn subtitle_runs(node: &Value) -> Option<&Vec<Value>> {
    node.pointer("/flexColumns/1/musicResponsiveListItemFlexColumnRenderer/text/runs")
        .and_then(Value::as_array)
}

/// Find the first run in any flex column whose browse link has `prefix`, and
/// return its text (searches the first four columns).
fn linked_text_any(node: &Value, prefix: &str) -> String {
    for col in 0..4 {
        let text = linked_text(node, col, prefix);
        if !text.is_empty() {
            return text;
        }
    }
    String::new()
}

fn linked_id_any(node: &Value, prefix: &str) -> Option<String> {
    for col in 0..4 {
        if let Some(id) = linked_id(node, col, prefix) {
            return Some(id);
        }
    }
    None
}

/// Parse a compact count like "78.8M", "1.2B", "500K", "1.2K".
fn parse_count(s: &str) -> Option<u64> {
    let s = s.trim();
    let (num, mult): (f64, u64) = if let Some(n) = s.strip_suffix('B') {
        (n.parse().ok()?, 1_000_000_000)
    } else if let Some(n) = s.strip_suffix('M') {
        (n.parse().ok()?, 1_000_000)
    } else if let Some(n) = s.strip_suffix('K') {
        (n.parse().ok()?, 1_000)
    } else {
        (s.parse().ok()?, 1)
    };
    Some((num * mult as f64) as u64)
}

/// Parse a "m:ss" or "h:mm:ss" duration into seconds.
fn parse_duration(s: &str) -> Option<u32> {
    let parts: Vec<u32> = s.split(':').filter_map(|p| p.parse().ok()).collect();
    match parts.as_slice() {
        [m, s] => Some(m * 60 + s),
        [h, m, s] => Some(h * 3600 + m * 60 + s),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Tracks
// ---------------------------------------------------------------------------

/// Extract audio tracks from a music search response.
pub fn tracks(resp: &Value) -> Vec<Track> {
    let mut nodes = Vec::new();
    find_all(resp, "musicResponsiveListItemRenderer", &mut nodes);
    nodes
        .into_iter()
        .filter_map(|n| track_from_mrlir(n.get("musicResponsiveListItemRenderer")?))
        .collect()
}

/// Whether a row's type badge marks it as non-audio ("Video"/"Episode").
fn is_non_audio_type(node: &Value) -> bool {
    let first = subtitle_runs(node)
        .and_then(|r| r.first())
        .and_then(|r| r.get("text"))
        .and_then(Value::as_str)
        .unwrap_or("");
    matches!(first, "Video" | "Episode")
}

/// Whether a track is a user upload (`MUSIC_VIDEO_TYPE_UGC`) rather than an
/// official YouTube Music catalog entry. UGC items are remixes/re-uploads by
/// arbitrary channels, not the artist's own music.
fn is_ugc(node: &Value) -> bool {
    let mut found = Vec::new();
    find_all(node, "musicVideoType", &mut found);
    found
        .iter()
        .any(|n| n.get("musicVideoType").and_then(Value::as_str) == Some("MUSIC_VIDEO_TYPE_UGC"))
}

fn track_from_mrlir(node: &Value) -> Option<Track> {
    let id = video_id(node)?;
    if is_non_audio_type(node) || is_ugc(node) {
        return None;
    }
    let title = column_text(node, 0).map(runs_all).unwrap_or_default();
    let artist = linked_text(node, 1, "UC");
    let artist = if artist.is_empty() {
        column_text(node, 1).map(runs_all).unwrap_or_default()
    } else {
        artist
    };
    // The album link (MPREb_) lives in different columns depending on the
    // surface: the subtitle column in search, a later column in catalog rows.
    let album = {
        let linked = linked_text_any(node, "MPREb_");
        if linked.is_empty() {
            None
        } else {
            Some(linked)
        }
    };

    Some(Track {
        id,
        title,
        artist,
        album,
        duration: duration(node),
        cover: cover(node),
        album_id: linked_id_any(node, "MPREb_"),
        // Per-artist credits are backfilled later (track_credits).
        artists: Vec::new(),
    })
}

/// The track's video id, from whichever renderer path carries it.
fn video_id(node: &Value) -> Option<String> {
    let paths = [
        "/overlay/musicItemThumbnailOverlayRenderer/content/musicPlayButtonRenderer/playNavigationEndpoint/watchEndpoint/videoId",
        "/playlistItemData/videoId",
        "/flexColumns/0/musicResponsiveListItemFlexColumnRenderer/text/runs/0/navigationEndpoint/watchEndpoint/videoId",
    ];
    paths
        .iter()
        .find_map(|p| node.pointer(p).and_then(Value::as_str))
        .map(str::to_owned)
}

fn duration(node: &Value) -> Option<u32> {
    // Album/playlist rows carry the duration in a fixed column; search rows
    // carry it as the trailing run of the subtitle flex column.
    if let Some(text) = node
        .pointer("/fixedColumns/0/musicResponsiveListItemFixedColumnRenderer/text")
        .map(runs_all)
    {
        if let Some(d) = parse_duration(&text) {
            return Some(d);
        }
    }
    let runs = subtitle_runs(node)?;
    for run in runs.iter().rev() {
        if let Some(text) = run.get("text").and_then(Value::as_str) {
            if let Some(d) = parse_duration(text) {
                return Some(d);
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Artists
// ---------------------------------------------------------------------------

/// Extract artists from a music search response.
pub fn artists(resp: &Value) -> Vec<ArtistSummary> {
    let mut nodes = Vec::new();
    find_all(resp, "musicResponsiveListItemRenderer", &mut nodes);
    nodes
        .into_iter()
        .filter_map(|n| artist_from_mrlir(n.get("musicResponsiveListItemRenderer")?))
        .collect()
}

fn artist_from_mrlir(node: &Value) -> Option<ArtistSummary> {
    let id = link_id(node)?;
    if !id.starts_with("UC") {
        return None;
    }
    Some(ArtistSummary {
        id,
        name: column_text(node, 0).map(runs_all).unwrap_or_default(),
        avatar: cover(node),
        subscribers: audience(node),
    })
}

/// The artist's audience/subscriber count, from the subtitle's trailing count.
fn audience(node: &Value) -> Option<u64> {
    let runs = subtitle_runs(node)?;
    for run in runs.iter().rev() {
        let text = run.get("text").and_then(Value::as_str)?;
        if let Some(first) = text.split_whitespace().next() {
            if let Some(n) = parse_count(first) {
                return Some(n);
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Albums
// ---------------------------------------------------------------------------

/// Extract albums from a music search response.
pub fn albums(resp: &Value) -> Vec<AlbumSummary> {
    let mut nodes = Vec::new();
    find_all(resp, "musicResponsiveListItemRenderer", &mut nodes);
    nodes
        .into_iter()
        .filter_map(|n| album_from_mrlir(n.get("musicResponsiveListItemRenderer")?))
        .collect()
}

fn album_from_mrlir(node: &Value) -> Option<AlbumSummary> {
    let id = link_id(node)?;
    if !id.starts_with("MPREb_") {
        return None;
    }
    Some(AlbumSummary {
        id,
        name: column_text(node, 0).map(runs_all).unwrap_or_default(),
        cover: cover(node),
        artist: linked_text(node, 1, "UC"),
        artist_id: linked_id(node, 1, "UC"),
        year: year(node),
        album_type: album_type(node),
    })
}

/// The trailing 4-digit year in a set of text runs, when present.
fn trailing_year(runs: &[Value]) -> Option<u16> {
    for run in runs.iter().rev() {
        let text = run.get("text").and_then(Value::as_str)?.trim();
        if text.len() == 4 && text.chars().all(|c| c.is_ascii_digit()) {
            return text.parse().ok();
        }
    }
    None
}

fn year(node: &Value) -> Option<u16> {
    trailing_year(subtitle_runs(node)?)
}

fn album_type(node: &Value) -> String {
    let runs = subtitle_runs(node);
    let first = runs
        .and_then(|r| r.first())
        .and_then(|r| r.get("text"))
        .and_then(Value::as_str)
        .unwrap_or("");
    match first {
        "Album" | "Single" | "EP" => first.to_owned(),
        _ => String::new(),
    }
}

// ---------------------------------------------------------------------------
// Album page
// ---------------------------------------------------------------------------

/// Parse a full album page (header + track list) from a `/browse` response.
pub fn album_page(resp: &Value, browse_id: &str) -> Option<AlbumPage> {
    let mut headers = Vec::new();
    find_all(resp, "musicResponsiveHeaderRenderer", &mut headers);
    let header = headers.first()?.get("musicResponsiveHeaderRenderer")?;
    let name = header.pointer("/title").map(runs_all).unwrap_or_default();
    let artist = header
        .pointer("/straplineTextOne")
        .map(runs_all)
        .unwrap_or_default();
    let artist_id = header
        .pointer("/straplineTextOne/runs/0/navigationEndpoint/browseEndpoint/browseId")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let year = header
        .pointer("/subtitle/runs")
        .and_then(Value::as_array)
        .and_then(|runs| trailing_year(runs));
    let album_type = header
        .pointer("/subtitle/runs/0/text")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let cover = header
        .pointer("/thumbnail/musicThumbnailRenderer/thumbnail/thumbnails")
        .and_then(Value::as_array)
        .map(|u| thumb(u))
        .unwrap_or_default();

    let mut tracks = tracks(resp);
    // Album rows often omit the album name/id and cover; inherit them from the
    // page header.
    for t in &mut tracks {
        if t.album.is_none() {
            t.album = Some(name.clone());
        }
        if t.album_id.is_none() {
            t.album_id = Some(browse_id.to_owned());
        }
        if t.cover.is_empty() {
            t.cover = cover.clone();
        }
    }

    Some(AlbumPage {
        id: browse_id.to_owned(),
        name,
        cover,
        artist,
        artist_id,
        year,
        album_type,
        tracks,
    })
}

// ---------------------------------------------------------------------------
// Artist page
// ---------------------------------------------------------------------------

/// Parse a full artist page (header + top songs + albums) from a `/browse`.
pub fn artist_page(resp: &Value, browse_id: &str) -> Option<ArtistPage> {
    let mut headers = Vec::new();
    find_all(resp, "musicImmersiveHeaderRenderer", &mut headers);
    let header = headers.first()?.get("musicImmersiveHeaderRenderer")?;

    let name = header.pointer("/title").map(runs_all).unwrap_or_default();
    let image = header
        .pointer("/thumbnail/musicThumbnailRenderer/thumbnail/thumbnails")
        .and_then(Value::as_array)
        .map(|u| thumb(u))
        .unwrap_or_default();
    // The description's first run is the main paragraph; later runs are the
    // Wikipedia attribution link, which we drop.
    let description = header
        .pointer("/description/runs/0/text")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let subscribers = {
        let mut nodes = Vec::new();
        find_all(resp, "subscriberCountText", &mut nodes);
        nodes
            .first()
            .and_then(|n| n.get("subscriberCountText"))
            .and_then(|n| n.pointer("/runs/0/text"))
            .and_then(Value::as_str)
            .and_then(parse_count)
    };

    Some(ArtistPage {
        id: browse_id.to_owned(),
        name,
        image,
        description,
        subscribers,
        tracks: tracks(resp),
        albums: album_cards(resp),
        tracks_playlist_id: tracks_playlist_id(resp),
    })
}

/// The artist's full-catalog playlist id, from the "Show all" link on the
/// top-songs shelf (a `VL…` browse id, stored without the prefix).
fn tracks_playlist_id(resp: &Value) -> Option<String> {
    let mut nodes = Vec::new();
    find_all(resp, "bottomEndpoint", &mut nodes);
    for node in nodes {
        if let Some(id) = node
            .pointer("/bottomEndpoint/browseEndpoint/browseId")
            .and_then(Value::as_str)
        {
            if let Some(bare) = id.strip_prefix("VL") {
                return Some(bare.to_owned());
            }
        }
    }
    None
}

/// Albums from carousel shelves (`musicTwoRowItemRenderer` cards).
///
/// Artist-page album cards carry only a year in their subtitle; the
/// album/single distinction comes from the carousel shelf's header title.
fn album_cards(resp: &Value) -> Vec<AlbumSummary> {
    let mut shelves = Vec::new();
    find_all(resp, "musicCarouselShelfRenderer", &mut shelves);
    let mut out = Vec::new();
    for wrapper in shelves {
        let Some(shelf) = wrapper.get("musicCarouselShelfRenderer") else {
            continue;
        };
        let title = shelf
            .pointer("/header/musicCarouselShelfBasicHeaderRenderer/title")
            .map(runs_all)
            .unwrap_or_default();
        let album_type = match title.as_str() {
            "Singles" => "Single",
            _ => "Album",
        };
        let mut cards = Vec::new();
        find_all(shelf, "musicTwoRowItemRenderer", &mut cards);
        for card in cards {
            if let Some(node) = card.get("musicTwoRowItemRenderer") {
                if let Some(a) = album_from_card(node, album_type) {
                    out.push(a);
                }
            }
        }
    }
    out
}

fn album_from_card(node: &Value, album_type: &str) -> Option<AlbumSummary> {
    let id = node
        .pointer("/navigationEndpoint/browseEndpoint/browseId")
        .and_then(Value::as_str)?;
    if !id.starts_with("MPREb_") {
        return None;
    }
    Some(AlbumSummary {
        id: id.to_owned(),
        name: node.pointer("/title").map(runs_all).unwrap_or_default(),
        cover: node
            .pointer("/thumbnailRenderer/musicThumbnailRenderer/thumbnail/thumbnails")
            .and_then(Value::as_array)
            .map(|u| thumb(u))
            .unwrap_or_default(),
        // Not present on artist-page album cards (the artist is the page).
        artist: String::new(),
        artist_id: None,
        year: node
            .pointer("/subtitle/runs")
            .and_then(Value::as_array)
            .and_then(|runs| trailing_year(runs)),
        album_type: album_type.to_owned(),
    })
}

// ---------------------------------------------------------------------------
// Playlist
// ---------------------------------------------------------------------------

/// Parse one playlist page: `(name, tracks, continuation_token)`.
///
/// The name is `None` on continuation pages (which carry only tracks plus a
/// further continuation, no header).
pub fn playlist_page(resp: &Value) -> (Option<String>, Vec<Track>, Option<String>) {
    let name = {
        let mut headers = Vec::new();
        find_all(resp, "musicResponsiveHeaderRenderer", &mut headers);
        headers
            .first()
            .and_then(|h| h.get("musicResponsiveHeaderRenderer"))
            .and_then(|h| h.pointer("/title"))
            .map(runs_all)
    };

    let mut conts = Vec::new();
    find_all(resp, "nextContinuationData", &mut conts);
    let continuation = conts
        .first()
        .and_then(|c| c.get("nextContinuationData"))
        .and_then(|c| c.pointer("/continuation"))
        .and_then(Value::as_str)
        .map(str::to_owned);

    (name, tracks(resp), continuation)
}

// ---------------------------------------------------------------------------
// Track details (next)
// ---------------------------------------------------------------------------

/// Parse a `/next` response into per-artist credits, album name, and album id.
///
/// The track's byline mixes clickable artist runs (`UC…`) with a clickable
/// album run (`MPREb_…`) and separators; each is classified by its browse id.
pub fn track_credits(resp: &Value) -> Option<(Vec<ArtistRef>, Option<String>, Option<String>)> {
    let mut nodes = Vec::new();
    find_all(resp, "longBylineText", &mut nodes);
    let byline = nodes.first()?.get("longBylineText")?;
    let runs = byline.get("runs").and_then(Value::as_array)?;

    let mut artists = Vec::new();
    let mut album = None;
    let mut album_id = None;
    for run in runs {
        let Some(text) = run.get("text").and_then(Value::as_str) else {
            continue;
        };
        let Some(id) = run
            .pointer("/navigationEndpoint/browseEndpoint/browseId")
            .and_then(Value::as_str)
        else {
            continue;
        };
        if id.starts_with("UC") {
            artists.push(ArtistRef {
                id: Some(id.to_owned()),
                name: text.to_owned(),
            });
        } else if id.starts_with("MPREb_") {
            album = Some(text.to_owned());
            album_id = Some(id.to_owned());
        }
    }
    Some((artists, album, album_id))
}

#[cfg(test)]
mod tests {
    use super::{parse_count, parse_duration};

    #[test]
    fn parses_durations() {
        assert_eq!(parse_duration("3:45"), Some(225));
        assert_eq!(parse_duration("1:02:03"), Some(3723));
        assert_eq!(parse_duration(""), None);
    }

    #[test]
    fn parses_counts() {
        assert_eq!(parse_count("78.8M"), Some(78_800_000));
        assert_eq!(parse_count("1.2B"), Some(1_200_000_000));
        assert_eq!(parse_count("500K"), Some(500_000));
        assert_eq!(parse_count("42"), Some(42));
        assert_eq!(parse_count(""), None);
    }
}
