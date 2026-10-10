//! Tolerant parsing of innertube music responses into `rift_types`.
//!
//! Innertube responses are large, nested, and churn frequently, so parsing is
//! defensive: it walks the JSON looking for the renderers it knows and skips
//! everything else, never failing the whole request because one node changed.

use rift_types::{AlbumPage, AlbumSummary, ArtistPage, ArtistRef, ArtistSummary, Track};
use serde_json::Value;
use tracing::warn;

/// Recursively collect every object that contains `key`.
///
/// **Fallback only.** `serde_json` is built without `preserve_order` (the
/// lockfile has no `indexmap`), so `Value::Object` is a `BTreeMap` and
/// `map.values()` iterates keys *alphabetically* — not in document order. A
/// `.first()` on the result therefore depends on key spelling rather than on the
/// response's layout. Every known location is read directly (see [`at`]); this
/// stays for shapes a future capture might add.
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

/// The renderer at a known path, if the response still has that shape.
fn at<'a>(v: &'a Value, pointer: &str) -> Option<&'a Value> {
    v.pointer(pointer)
}

/// The page header of `renderer`, from whichever known location carries it.
///
/// Album and playlist pages place their `musicResponsiveHeaderRenderer` at one
/// of a few paths depending on the surface. Both are checked explicitly, in
/// document order, so the choice is a decision rather than a key-ordering side
/// effect. `find_all` remains the last resort for a shape not seen yet.
fn header_of<'a>(resp: &'a Value, renderer: &str) -> Option<&'a Value> {
    for pointer in [
        // Browse pages served to the desktop web client.
        "/header",
        "/contents/twoColumnBrowseResultsRenderer/tabs/0/tabRenderer/content/sectionListRenderer/contents/0",
        "/contents/singleColumnBrowseResultsRenderer/tabs/0/tabRenderer/content/sectionListRenderer/contents/0",
        // A bare section list, as a continuation page or a mobile shape.
        "/contents/sectionListRenderer/contents/0",
    ] {
        if let Some(node) = at(resp, pointer).and_then(|n| n.get(renderer)) {
            return Some(node);
        }
    }
    let mut nodes = Vec::new();
    find_all(resp, renderer, &mut nodes);
    nodes.first().and_then(|n| n.get(renderer))
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
///
/// Rounds rather than truncates: "1.7K" is 1700, not 1699, and truncation is
/// visible in the subscriber counts the artist page displays.
pub fn parse_count(s: &str) -> Option<u64> {
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
    let scaled = num * mult as f64;
    // `as` saturates for out-of-range floats, but map the non-finite cases
    // explicitly so a malformed "infB" can't produce a meaningless count.
    if !scaled.is_finite() {
        return None;
    }
    Some(scaled.round().clamp(0.0, u64::MAX as f64) as u64)
}

/// How many components a duration string may have: "m:ss" or "h:mm:ss".
const MAX_DURATION_PARTS: usize = 3;

/// Parse a "m:ss" or "h:mm:ss" duration into seconds.
///
/// Every component must parse. Silently dropping one turns "1:2x:03" into
/// `[1, 03]` and reads it as 63 seconds — a wrong-but-plausible length that the
/// UI then shows. The arithmetic is checked too: this input is server-controlled
/// and unchecked `u32` math panics in debug and wraps to a bogus duration in
/// release (the workspace sets no `overflow-checks`).
pub fn parse_duration(s: &str) -> Option<u32> {
    let mut parts = Vec::with_capacity(MAX_DURATION_PARTS);
    for part in s.split(':') {
        if parts.len() == MAX_DURATION_PARTS {
            return None;
        }
        parts.push(part.trim().parse::<u32>().ok()?);
    }
    // Minutes and seconds are positional, so a lone "45" is not a duration.
    let (h, m, sec) = match parts.as_slice() {
        [m, s] => (0, *m, *s),
        [h, m, s] => (*h, *m, *s),
        _ => return None,
    };
    h.checked_mul(3600)?
        .checked_add(m.checked_mul(60)?)?
        .checked_add(sec)
}

/// Whether a subtitle run is the row's type badge ("Song", "Album", "Artist"…)
/// rather than part of its byline.
pub fn is_type_badge(text: &str) -> bool {
    matches!(
        text.trim(),
        "Song" | "Video" | "Album" | "Single" | "EP" | "Artist" | "Playlist" | "Episode"
    )
}

/// Whether a run carries no byline text of its own — a separator, or a
/// duration run ("3:22").
///
/// Deliberately narrow. Only runs that are *structurally* identifiable are
/// dropped: a bare four-digit year can sit where a byline does (as `search_albums`
/// rows carry it), but so can a band named "1975", and this fallback runs
/// precisely because there is no link to disambiguate with. Wrong-but-plausible
/// is worse than a stray year here, so anything ambiguous is kept.
pub fn is_subtitle_noise(text: &str) -> bool {
    let t = text.trim();
    if t.is_empty() || t.chars().all(|c| !c.is_alphanumeric()) {
        return true;
    }
    // Handles the " • 3:22" shape as well as a bare duration run.
    parse_duration(t.trim_start_matches(|c: char| !c.is_ascii_digit())).is_some()
}

// ---------------------------------------------------------------------------
// Tracks
// ---------------------------------------------------------------------------

/// Every item of the page's main track shelf, or of all shelves as a fallback.
///
/// `/header` and `/contents` are read in that order explicitly. The fallback
/// keeps working if the shelf moves, and is the only path a shape we have not
/// seen gets.
fn shelf_items(resp: &Value) -> Vec<&Value> {
    for pointer in [
        "/contents/twoColumnBrowseResultsRenderer/secondaryContents/sectionListRenderer/contents",
        "/contents/twoColumnBrowseResultsRenderer/tabs/0/tabRenderer/content/sectionListRenderer/contents",
        "/contents/singleColumnBrowseResultsRenderer/tabs/0/tabRenderer/content/sectionListRenderer/contents",
        "/contents/sectionListRenderer/contents",
    ] {
        let Some(sections) = at(resp, pointer).and_then(Value::as_array) else {
            continue;
        };
        let items = items_in_shelves(sections);
        if !items.is_empty() {
            return items;
        }
    }
    // Unknown shape: fall back to every renderer in the document.
    let mut nodes = Vec::new();
    find_all(resp, "musicResponsiveListItemRenderer", &mut nodes);
    nodes
        .into_iter()
        .filter_map(|n| n.get("musicResponsiveListItemRenderer"))
        .collect()
}

/// The row renderers inside a set of sections, in document order.
fn items_in_shelves(sections: &[Value]) -> Vec<&Value> {
    let mut out = Vec::new();
    for section in sections {
        let Some(shelf) = section
            .get("musicShelfRenderer")
            .or_else(|| section.get("musicPlaylistShelfRenderer"))
        else {
            continue;
        };
        if let Some(items) = shelf.get("contents").and_then(Value::as_array) {
            out.extend(
                items
                    .iter()
                    .filter_map(|n| n.get("musicResponsiveListItemRenderer")),
            );
        }
    }
    out
}

/// Extract audio tracks from a music search response.
pub fn tracks(resp: &Value) -> Vec<Track> {
    let parsed: Vec<Track> = shelf_items(resp)
        .iter()
        .filter_map(|n| track_from_mrlir(n))
        .collect();
    if parsed.is_empty() {
        // A renderer rename looks identical to "no results" from here, so the
        // two are worth distinguishing in a log: one is a YouTube change, the
        // other is a genuine empty search. Counting *rows anywhere in the
        // document* (not just in the shelves we understand) is what makes the
        // difference visible even when the shelf itself moved.
        //
        // Rows this parser dropped on purpose — UGC-typed, or badged
        // "Video"/"Episode" — are not evidence of anything, so they do not
        // count: a response made entirely of videos is an *explained* empty
        // result, and warning about it would train the reader to ignore the
        // warning on exactly the searches that return mostly videos.
        let mut rows = Vec::new();
        find_all(resp, "musicResponsiveListItemRenderer", &mut rows);
        let explained = rows
            .iter()
            .filter(|n| {
                n.get("musicResponsiveListItemRenderer")
                    .is_some_and(is_non_audio_row)
            })
            .count();
        warn_on_silent_degradation("track", rows.len() - explained);
    }
    parsed
}

/// Warn when the response carried rows that nothing could be extracted from.
///
/// Innertube renames renderers without notice, and a parser that quietly
/// returns an empty list turns that into "no results" — indistinguishable from
/// a real empty search for whoever is looking at the UI. Logging the
/// discrepancy is the difference between a five-minute diagnosis and a
/// debugging session. `rows_seen` is therefore the count of rows the parser did
/// *not* drop on purpose; see [`tracks`].
fn warn_on_silent_degradation(what: &str, rows_seen: usize) {
    if rows_seen > 0 {
        warn!(
            "{rows_seen} {what} row(s) in the response but none could be parsed — the \
             layout may have changed"
        );
    }
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
///
/// This is the *typed* signal, read from the row's own
/// `watchEndpointMusicConfig.musicVideoType`, and it is authoritative wherever
/// it travels: it is title-independent, so a UGC upload called "Blinding
/// Lights" is dropped exactly like one called "Blinding Lights (Official
/// Video)". Every path that carries the field — search, album, artist and
/// playlist rows parsed here — goes through [`is_non_audio_row`].
///
/// It cannot be the whole rule. `Track` carries no type, so a track that comes
/// back from Rift's own JSON export, or from the on-disk library, has no
/// `musicVideoType` to consult; the title-marker fallback in
/// `src-tauri/src/commands/convert.rs` exists for exactly those paths and is
/// deliberately *weaker*, not equivalent. The split is asserted on both sides:
/// this module pins "the type alone drops a clean-titled UGC row", and
/// `convert.rs` pins "the title fallback cannot, and keeps it".
fn is_ugc(node: &Value) -> bool {
    let mut found = Vec::new();
    find_all(node, "musicVideoType", &mut found);
    found
        .iter()
        .any(|n| n.get("musicVideoType").and_then(Value::as_str) == Some("MUSIC_VIDEO_TYPE_UGC"))
}

/// Whether a row's own metadata says it is not an audio track.
///
/// Two independent pieces of evidence, strongest first:
///
/// 1. [`is_ugc`] — the typed `musicVideoType`, which is title-independent.
/// 2. [`is_non_audio_type`] — the leading subtitle badge ("Video"/"Episode"),
///    which is prose the layout can rename.
///
/// The title-marker rule ("(Official Video)") is *not* applied here. It lives in
/// `convert.rs` because it is the only rule the signal-less paths can use, and
/// applying it here as well would mean the innertube path and the JSON-import
/// path agree by luck rather than by construction. Doing the drop here would
/// also hide from the caller a row it may legitimately want to see.
fn is_non_audio_row(node: &Value) -> bool {
    is_non_audio_type(node) || is_ugc(node)
}

/// The byline to show for a row with no linked artist.
///
/// The row's title, or `None` when the title column is absent or not a string.
///
/// `runs_all` folds a missing or `null` title to `""`, which would otherwise
/// produce a track the UI renders as a blank, unclickable row. A row this
/// parser cannot read is skipped instead.
fn title_of(node: &Value) -> Option<String> {
    let runs = column_text(node, 0)?.get("runs")?.as_array()?;
    let title = runs
        .iter()
        .filter_map(|r| r.get("text").and_then(Value::as_str))
        .collect::<String>();
    (!title.is_empty()).then_some(title)
}

/// The byline to show for a row with no linked artist.
///
/// The subtitle column mixes the row's type badge, the byline, and the duration
/// into one run list — an unlinked search row is `["Song", " • ", "3:22"]`, and
/// the old "join every run" fallback rendered that as the artist, i.e.
/// `"Song • 3:22"`. Drop the leading badge and the separator/duration runs, and
/// join whatever byline text is left.
///
/// Linked runs are kept verbatim: this only runs when no `UC…` run exists, but
/// the row may still link a *different* byline entry (an album, say), and a
/// linked run is real text by construction.
pub fn unlinked_artist(node: &Value) -> String {
    let Some(runs) = subtitle_runs(node) else {
        return String::new();
    };
    let mut out: Vec<&str> = Vec::new();
    for (i, run) in runs.iter().enumerate() {
        let Some(text) = run.get("text").and_then(Value::as_str) else {
            continue;
        };
        if i == 0 && is_type_badge(text) {
            continue;
        }
        if run
            .pointer("/navigationEndpoint/browseEndpoint/browseId")
            .and_then(Value::as_str)
            .is_none()
            && is_subtitle_noise(text)
        {
            continue;
        }
        out.push(text);
    }
    out.join("").trim().to_owned()
}

fn track_from_mrlir(node: &Value) -> Option<Track> {
    let id = video_id(node)?;
    if is_non_audio_row(node) {
        return None;
    }
    // A row whose title is absent or the wrong JSON type is a row shape this
    // parser does not understand, so it is skipped: an id with no title is not
    // something the UI can render or the user can act on.
    let title = title_of(node)?;
    let artist = linked_text(node, 1, "UC");
    let artist = if artist.is_empty() {
        unlinked_artist(node)
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
    shelf_items(resp)
        .iter()
        .filter_map(|n| artist_from_mrlir(n))
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
///
/// Skips runs without a string `text` rather than returning early: this scans
/// backwards, and a `?` here would abandon the whole scan on the first
/// structural run — silently dropping a count that *is* present a run earlier.
fn audience(node: &Value) -> Option<u64> {
    let runs = subtitle_runs(node)?;
    for run in runs.iter().rev() {
        let Some(text) = run.get("text").and_then(Value::as_str) else {
            continue;
        };
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
    shelf_items(resp)
        .iter()
        .filter_map(|n| album_from_mrlir(n))
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
///
/// Skips runs without a string `text` rather than returning early — see
/// [`audience`]; this scans backwards too, so a `?` would drop a year that is
/// present.
fn trailing_year(runs: &[Value]) -> Option<u16> {
    for run in runs.iter().rev() {
        let Some(text) = run.get("text").and_then(Value::as_str) else {
            continue;
        };
        let text = text.trim();
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
    let header = header_of(resp, "musicResponsiveHeaderRenderer")?;
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
    let header = header_of(resp, "musicImmersiveHeaderRenderer")?;

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
    // The channel's own subscriber count, from the header's subscribe button.
    // Read by path: the old `find_all(resp, "subscriberCountText").first()`
    // walked `contents` before `header` alphabetically, so which count it found
    // depended on key spelling — a subscriber count for a *different* channel in
    // the page body would win over the page's own.
    let subscribers = at(
        header,
        "/subscriptionButton/subscribeButtonRenderer/subscriberCountText",
    )
    .or_else(|| at(header, "/subscriberCountText"))
    .and_then(|n| n.pointer("/runs/0/text"))
    .and_then(Value::as_str)
    // "7.17M subscribers" / "7.17M monthly audience" — the count is the first
    // token either way.
    .and_then(|text| text.split_whitespace().next())
    .and_then(parse_count);

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
///
/// The link must come from the *top-songs shelf's own* `bottomEndpoint`. Taking
/// the first `VL…` id under any `bottomEndpoint` in the document (the old
/// `find_all(..).first()`, which is alphabetical order) would silently load a
/// different list once the layout shifts — the fixture already carries a second
/// `VL…` id on the Videos carousel, so "Show all songs" would load that instead.
pub fn tracks_playlist_id(resp: &Value) -> Option<String> {
    // The shelf is the first section of the artist page's tab content; its
    // `bottomEndpoint` is the "Show all" link for that shelf.
    let shelf = at(
        resp,
        "/contents/singleColumnBrowseResultsRenderer/tabs/0/tabRenderer/content/sectionListRenderer/contents/0/musicShelfRenderer",
    )
    .or_else(|| {
        at(
            resp,
            "/contents/twoColumnBrowseResultsRenderer/tabs/0/tabRenderer/content/sectionListRenderer/contents/0/musicShelfRenderer",
        )
    });
    if let Some(id) = shelf
        .and_then(|s| at(s, "/bottomEndpoint/browseEndpoint/browseId"))
        .and_then(Value::as_str)
        .and_then(|id| id.strip_prefix("VL"))
    {
        return Some(id.to_owned());
    }

    // Fallback for a shape we have not seen: the first shelf in the *document*
    // that carries one, which at least keeps the page working. Any shelf type
    // counts here — the point is only that the known path did not match.
    let mut nodes = Vec::new();
    find_all(resp, "bottomEndpoint", &mut nodes);
    nodes.iter().find_map(|n| {
        n.pointer("/bottomEndpoint/browseEndpoint/browseId")
            .and_then(Value::as_str)
            .and_then(|id| id.strip_prefix("VL"))
            .map(str::to_owned)
    })
}

/// The album/single/EP label a carousel shelf's header implies.
///
/// Matched with `starts_with`, not equality: the shelf title is
/// server-controlled prose ("Albums", "Singles and EPs", "Singles"), and an
/// exact match on one spelling silently degrades every card to "Album" the
/// moment YouTube adjusts a word. The fixture carries no Singles shelf at all,
/// so nothing would have caught that.
pub fn album_type_from_shelf(title: &str) -> &'static str {
    let t = title.trim();
    if t.starts_with("Single") {
        "Single"
    } else if t.starts_with("EP") {
        "EP"
    } else {
        "Album"
    }
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
        let album_type = album_type_from_shelf(&title);
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
    let name = header_of(resp, "musicResponsiveHeaderRenderer")
        .and_then(|h| h.pointer("/title"))
        .map(runs_all);

    // The page's own continuation, read by path. The old
    // `find_all(resp, "nextContinuationData").first()` took whichever node
    // sorted first alphabetically, which is not the same as the page's.
    let continuation = at(
        resp,
        "/contents/twoColumnBrowseResultsRenderer/secondaryContents/sectionListRenderer/continuations/0/nextContinuationData/continuation",
    )
    .or_else(|| {
        at(
            resp,
            "/contents/singleColumnBrowseResultsRenderer/tabs/0/tabRenderer/content/sectionListRenderer/continuations/0/nextContinuationData/continuation",
        )
    })
    .or_else(|| {
        at(
            resp,
            "/contents/twoColumnBrowseResultsRenderer/tabs/0/tabRenderer/content/sectionListRenderer/continuations/0/nextContinuationData/continuation",
        )
    })
    .and_then(Value::as_str)
    .map(str::to_owned)
    .or_else(|| {
        // Unknown shape: the first continuation anywhere, in document order.
        let mut conts = Vec::new();
        find_all(resp, "nextContinuationData", &mut conts);
        conts
            .iter()
            .find_map(|c| at(c, "/nextContinuationData/continuation"))
            .and_then(Value::as_str)
            .map(str::to_owned)
    });

    (name, tracks(resp), continuation)
}

// ---------------------------------------------------------------------------
// Track details (next)
// ---------------------------------------------------------------------------

/// Parse a `/next` response into per-artist credits, album name, and album id.
///
/// The track's byline mixes clickable artist runs (`UC…`) with a clickable
/// album run (`MPREb_…`) and separators; each is classified by its browse id.
///
/// The byline must belong to the renderer for the *requested* `video_id`. Taking
/// the first `longBylineText` in the document (the old `find_all(..).first()`,
/// which is alphabetical order, not document order) means a substituted or
/// unavailable video quietly yields some other song's artists and album — the
/// credits are then written onto the requested track's row and persisted. The
/// queue panel for a single video carries one `playlistPanelVideoRenderer` whose
/// `videoId` is the answer, so it is matched explicitly.
pub fn track_credits(
    resp: &Value,
    video_id: &str,
) -> Option<(Vec<ArtistRef>, Option<String>, Option<String>)> {
    let byline = byline_for_video(resp, video_id)?;
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

/// The `longBylineText` of the queue entry for `video_id`.
///
/// Returns `None` rather than falling back to another entry's byline: a wrong
/// byline is worse than no credits, because the caller persists what it gets.
fn byline_for_video<'a>(resp: &'a Value, video_id: &str) -> Option<&'a Value> {
    // The known path for a single-video queue. `find_all` result order is
    // alphabetical, so this is read directly.
    let entries = at(
        resp,
        "/contents/singleColumnMusicWatchNextResultsRenderer/tabbedRenderer/watchNextTabbedResultsRenderer/tabs/0/tabRenderer/content/musicQueueRenderer/content/playlistPanelRenderer/contents",
    )
    .or_else(|| {
        at(
            resp,
            "/contents/singleColumnMusicWatchNextResultsRenderer/tabbedRenderer/watchNextTabbedResultsRenderer/tabs/0/tabRenderer/content/playlistPanelRenderer/contents",
        )
    })
    .and_then(Value::as_array)
    .map(Vec::as_slice)
    .unwrap_or_default()
    .iter()
    .filter_map(|n| n.get("playlistPanelVideoRenderer"))
    .collect::<Vec<&Value>>();

    if let Some(found) = renderer_matching(&entries, video_id) {
        return Some(found);
    }

    // Fallback for a shape we have not seen: scan every queue entry, still
    // requiring the video id to match.
    let mut renderers = Vec::new();
    find_all(resp, "playlistPanelVideoRenderer", &mut renderers);
    let all: Vec<&Value> = renderers
        .into_iter()
        .filter_map(|n| n.get("playlistPanelVideoRenderer"))
        .collect();
    renderer_matching(&all, video_id)
}

/// `longBylineText` of the first renderer whose `videoId` is `video_id`.
fn renderer_matching<'a>(renderers: &[&'a Value], video_id: &str) -> Option<&'a Value> {
    renderers
        .iter()
        .find(|r| r.get("videoId").and_then(Value::as_str) == Some(video_id))
        .and_then(|r| r.get("longBylineText"))
}

#[cfg(test)]
mod tests {
    use super::{
        album_type_from_shelf, audience, is_non_audio_row, is_subtitle_noise, is_type_badge,
        parse_count, parse_duration, track_credits, tracks, trailing_year, unlinked_artist,
    };
    use serde_json::{json, Value};

    /// One search-shaped row: a title column, a subtitle column carrying the
    /// type badge, and a playable video id typed with `musicVideoType` — the
    /// three things a row needs to be a candidate track at all.
    fn search_row(video_id: &str, title: &str, badge: &str, music_video_type: &str) -> Value {
        json!({
            "musicResponsiveListItemRenderer": {
                "flexColumns": [
                    { "musicResponsiveListItemFlexColumnRenderer": { "text": { "runs": [
                        { "text": title }
                    ] } } },
                    { "musicResponsiveListItemFlexColumnRenderer": { "text": { "runs": [
                        { "text": badge }, { "text": " • " }, { "text": "Some Artist" }
                    ] } } }
                ],
                "overlay": {
                    "musicItemThumbnailOverlayRenderer": { "content": { "musicPlayButtonRenderer": {
                        "playNavigationEndpoint": { "watchEndpoint": {
                            "videoId": video_id,
                            "watchEndpointMusicSupportedConfigs": { "watchEndpointMusicConfig": {
                                "musicVideoType": music_video_type
                            } }
                        } }
                    } } }
                }
            }
        })
    }

    /// A response whose single shelf holds `rows`, read by a path `shelf_items`
    /// checks explicitly (so the assertion cannot depend on key ordering).
    fn one_shelf(rows: Vec<Value>) -> Value {
        json!({
            "contents": { "sectionListRenderer": { "contents": [
                { "musicShelfRenderer": { "contents": rows } }
            ] } }
        })
    }

    /// L10's exact regression: a UGC-typed row with a **clean** title must be
    /// dropped on the paths that carry the typed signal.
    ///
    /// Both inputs here look identical to the title fallback in
    /// `src-tauri/src/commands/convert.rs` — badge "Song", no "Official Video"
    /// marker, non-empty title — so only `musicVideoType` distinguishes them.
    /// Before this was pinned, the row was kept: the app-level filter can see a
    /// title and nothing else, and nothing carried the type far enough to stop
    /// it. Reverting `is_non_audio_row` to the badge check alone brings the UGC
    /// row back and fails this test.
    #[test]
    fn a_ugc_typed_row_with_a_clean_title_is_dropped() {
        const UGC_ID: &str = "ugc-clean-title";
        const UGC_TITLE: &str = "Blinding Lights (Cover)";

        let official = search_row(
            "atv-official",
            "Blinding Lights",
            "Song",
            "MUSIC_VIDEO_TYPE_ATV",
        );
        let ugc = search_row(UGC_ID, UGC_TITLE, "Song", "MUSIC_VIDEO_TYPE_UGC");
        let resp = one_shelf(vec![official, ugc]);

        // The shape is parseable — the ATV row comes through — so the UGC row's
        // absence below cannot be blamed on a row this parser cannot read.
        let parsed = tracks(&resp);
        assert_eq!(
            parsed.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(),
            ["atv-official"],
            "the official audio row is kept and the clean-titled UGC row is not"
        );
        assert!(
            !parsed.iter().any(|t| t.title == UGC_TITLE),
            "a UGC upload must not reach the caller on the strength of its title"
        );

        // And the reason is the type alone, not the badge or the title.
        let ugc_row = search_row(UGC_ID, UGC_TITLE, "Song", "MUSIC_VIDEO_TYPE_UGC");
        let ugc_row = ugc_row.get("musicResponsiveListItemRenderer").unwrap();
        assert!(
            is_non_audio_row(ugc_row),
            "the typed signal alone must classify the row as non-audio"
        );
    }

    #[test]
    fn parses_durations() {
        assert_eq!(parse_duration("3:45"), Some(225));
        assert_eq!(parse_duration("1:02:03"), Some(3723));
        assert_eq!(parse_duration(""), None);
    }

    /// Every component must parse. Dropping a bad one turns `"1:2x:03"` into
    /// `[1, 03]` and reads it as 63 seconds — a plausible-looking wrong answer.
    #[test]
    fn a_duration_with_a_bad_component_is_rejected_not_salvaged() {
        assert_eq!(parse_duration("1:2x:03"), None);
        assert_eq!(parse_duration("3:4x"), None);
        assert_eq!(parse_duration("x:45"), None);
        // Separators that are not durations, and shapes that don't fit.
        assert_eq!(
            parse_duration(" • 3:22"),
            None,
            "leading separator is not a duration"
        );
        assert_eq!(
            parse_duration("45"),
            None,
            "a bare number is not a duration"
        );
        assert_eq!(parse_duration("1:2:3:4"), None, "too many components");
        assert_eq!(parse_duration("1:-2"), None, "negative component");
    }

    /// The arithmetic is unchecked-`u32` in the original, which panics in debug
    /// and wraps to a bogus length in release. This input is server-controlled.
    #[test]
    fn a_duration_that_would_overflow_is_rejected() {
        assert_eq!(parse_duration("4294967295:59"), None);
        assert_eq!(parse_duration("4294967295:59:59"), None);
        // The largest value that does fit still works.
        assert_eq!(parse_duration("1193046:28:15"), Some(4_294_967_295));
    }

    #[test]
    fn parses_counts() {
        assert_eq!(parse_count("78.8M"), Some(78_800_000));
        assert_eq!(parse_count("1.2B"), Some(1_200_000_000));
        assert_eq!(parse_count("500K"), Some(500_000));
        assert_eq!(parse_count("42"), Some(42));
        assert_eq!(parse_count(""), None);
        // Rounds rather than truncating: 1.7K is 1700, not 1699.
        assert_eq!(parse_count("1.7K"), Some(1_700));
        assert_eq!(parse_count("22.5"), Some(23), "a bare float rounds too");
        // Non-finite input cannot produce a count.
        assert_eq!(parse_count("infB"), None);
        assert_eq!(parse_count("NaNB"), None);
    }

    /// The shelf header is server-controlled prose; an exact match silently
    /// degrades every card to "Album" the moment YouTube adjusts a word.
    #[test]
    fn album_type_matches_the_shelf_title_by_prefix() {
        assert_eq!(album_type_from_shelf("Singles"), "Single");
        assert_eq!(album_type_from_shelf("Singles and EPs"), "Single");
        assert_eq!(album_type_from_shelf("EPs"), "EP");
        assert_eq!(album_type_from_shelf("Albums"), "Album");
        assert_eq!(album_type_from_shelf("Featured on"), "Album");
        assert_eq!(album_type_from_shelf(""), "Album");
    }

    /// A row's type badge is only a badge in the leading position.
    #[test]
    fn type_badges_are_recognized() {
        for badge in [
            "Song", "Video", "Album", "Single", "EP", "Artist", "Playlist",
        ] {
            assert!(is_type_badge(badge), "{badge} is a badge");
        }
        assert!(!is_type_badge("The Weeknd"));
        assert!(!is_type_badge("Album Leaf"), "only an exact badge counts");
    }

    /// The noise rule must stay narrow: a band named "1975" or "2 Unlimited" is
    /// a real byline, and this fallback runs precisely because there is no link
    /// to disambiguate with.
    #[test]
    fn subtitle_noise_is_limited_to_separators_and_durations() {
        assert!(is_subtitle_noise(" • "));
        assert!(is_subtitle_noise("3:22"));
        assert!(is_subtitle_noise(" • 3:22"));
        assert!(is_subtitle_noise(""));
        // Ambiguous runs are kept, not dropped.
        assert!(!is_subtitle_noise("1975"));
        assert!(!is_subtitle_noise("2 Unlimited"));
        assert!(!is_subtitle_noise("Various Artists"));
        assert!(!is_subtitle_noise("The Weeknd"));
    }

    /// The regression guard for M11: the artist must never be the raw subtitle.
    #[test]
    fn unlinked_artist_drops_the_badge_and_duration() {
        let row = json!({
            "flexColumns": [
                { "musicResponsiveListItemFlexColumnRenderer": { "text": { "runs": [{ "text": "Blinding Lights" }] } } },
                { "musicResponsiveListItemFlexColumnRenderer": { "text": { "runs": [
                    { "text": "Song" }, { "text": " • " }, { "text": "3:22" }
                ] } } }
            ]
        });
        assert_eq!(unlinked_artist(&row), "");

        // A real unlinked byline survives, including one that looks numeric.
        let named = json!({
            "flexColumns": [
                { "musicResponsiveListItemFlexColumnRenderer": { "text": { "runs": [{ "text": "T" }] } } },
                { "musicResponsiveListItemFlexColumnRenderer": { "text": { "runs": [
                    { "text": "Album" }, { "text": " • " }, { "text": "Various Artists" },
                    { "text": " • " }, { "text": "2022" }
                ] } } }
            ]
        });
        assert_eq!(unlinked_artist(&named), "Various Artists2022");

        let numeric = json!({
            "flexColumns": [
                { "musicResponsiveListItemFlexColumnRenderer": { "text": { "runs": [{ "text": "T" }] } } },
                { "musicResponsiveListItemFlexColumnRenderer": { "text": { "runs": [
                    { "text": "Song" }, { "text": " • " }, { "text": "2 Unlimited" }
                ] } } }
            ]
        });
        assert_eq!(unlinked_artist(&numeric), "2 Unlimited");

        // No subtitle column at all is not a panic.
        assert_eq!(unlinked_artist(&json!({})), "");
    }

    /// M12's sharpest edge: a substituted or unavailable video must not donate
    /// another song's artists and album, because the caller persists what it
    /// gets onto the requested track.
    #[test]
    fn track_credits_only_reads_the_byline_of_the_requested_video() {
        let resp = json!({
            "contents": {
                "singleColumnMusicWatchNextResultsRenderer": {
                    "tabbedRenderer": {
                        "watchNextTabbedResultsRenderer": {
                            "tabs": [{ "tabRenderer": { "content": { "musicQueueRenderer": {
                                "content": { "playlistPanelRenderer": { "contents": [
                                    {
                                        "playlistPanelVideoRenderer": {
                                            "videoId": "wanted",
                                            "longBylineText": { "runs": [
                                                { "text": "Right Artist",
                                                  "navigationEndpoint": { "browseEndpoint": { "browseId": "UCRight" } } }
                                            ] }
                                        }
                                    },
                                    {
                                        "playlistPanelVideoRenderer": {
                                            "videoId": "someone-else",
                                            "longBylineText": { "runs": [
                                                { "text": "Wrong Artist",
                                                  "navigationEndpoint": { "browseEndpoint": { "browseId": "UCWrong" } } }
                                            ] }
                                        }
                                    }
                                ] } }
                            } } } }]
                        }
                    }
                }
            }
        });

        let (artists, _, _) = track_credits(&resp, "wanted").expect("credits for the wanted video");
        assert_eq!(
            artists.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(),
            ["Right Artist"],
            "the wanted video's own byline wins, not the first in the document"
        );

        // The same response parsed for a video it does not contain yields
        // nothing at all — never another entry's credits.
        assert!(
            track_credits(&resp, "absent").is_none(),
            "an absent video must not borrow another entry's byline"
        );
    }

    /// M12 again, from the other direction: `serde_json` orders object keys
    /// alphabetically, so a byline reachable only through a key that sorts
    /// after `contents` must still be found — and one that sorts before it must
    /// not win by accident.
    #[test]
    fn track_credits_is_not_decided_by_key_spelling() {
        let resp = json!({
            "aaa_first_alphabetically": {
                "playlistPanelVideoRenderer": {
                    "videoId": "other",
                    "longBylineText": { "runs": [
                        { "text": "Decoy", "navigationEndpoint": { "browseEndpoint": { "browseId": "UCDecoy" } } }
                    ] }
                }
            },
            "contents": {
                "singleColumnMusicWatchNextResultsRenderer": {
                    "tabbedRenderer": {
                        "watchNextTabbedResultsRenderer": {
                            "tabs": [{ "tabRenderer": { "content": { "musicQueueRenderer": {
                                "content": { "playlistPanelRenderer": { "contents": [
                                    { "playlistPanelVideoRenderer": {
                                        "videoId": "target",
                                        "longBylineText": { "runs": [
                                            { "text": "Real", "navigationEndpoint": { "browseEndpoint": { "browseId": "UCReal" } } }
                                        ] }
                                    } }
                                ] } }
                            } } } }]
                        }
                    }
                }
            }
        });

        let (artists, _, _) = track_credits(&resp, "target").expect("credits");
        assert_eq!(
            artists.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(),
            ["Real"],
            "a decoy under an alphabetically-earlier key must not win"
        );
    }

    /// L9's regression guard. The "Show all songs" id must come from the
    /// top-songs shelf, not from whichever `VL…` id the document happens to
    /// yield first — `serde_json` orders keys alphabetically, so a decoy under
    /// an earlier-sorting key used to win and "Show all songs" silently loaded
    /// the wrong list.
    #[test]
    fn tracks_playlist_id_comes_from_the_top_songs_shelf() {
        const TOP_SONGS: &str = "VLOLAK5uy_top_songs";
        const TOP_SONGS_BARE: &str = "OLAK5uy_top_songs";
        const DECOY: &str = "VLPLSdecoy_carousel";
        const DECOY_BARE: &str = "PLSdecoy_carousel";

        let resp = json!({
            // Sorts before "contents", so a key-order walk would take it.
            "aaaVideos": {
                "musicCarouselShelfRenderer": {
                    "bottomEndpoint": { "browseEndpoint": { "browseId": DECOY } }
                }
            },
            "contents": {
                "singleColumnBrowseResultsRenderer": {
                    "tabs": [{ "tabRenderer": { "content": { "sectionListRenderer": {
                        "contents": [
                            {
                                "musicShelfRenderer": {
                                    "bottomEndpoint": { "browseEndpoint": { "browseId": TOP_SONGS } }
                                }
                            },
                            {
                                "musicCarouselShelfRenderer": {
                                    "bottomEndpoint": { "browseEndpoint": { "browseId": DECOY } }
                                }
                            }
                        ]
                    } } } }]
                }
            }
        });

        assert_eq!(
            super::tracks_playlist_id(&resp).as_deref(),
            Some(TOP_SONGS_BARE),
            "the top-songs shelf's own link wins, and the VL prefix is stripped"
        );

        // A response with no top-songs shelf at all falls back to the first
        // shelf that has one, rather than failing the page outright.
        let no_shelf = json!({
            "contents": { "singleColumnBrowseResultsRenderer": { "tabs": [{ "tabRenderer": {
                "content": { "sectionListRenderer": { "contents": [{
                    "musicCarouselShelfRenderer": {
                        "bottomEndpoint": { "browseEndpoint": { "browseId": DECOY } }
                    }
                }] } }
            } }] } }
        });
        assert_eq!(
            super::tracks_playlist_id(&no_shelf).as_deref(),
            Some(DECOY_BARE)
        );
    }

    /// L17: malformed input must yield empty results, never a panic. The parser
    /// is deliberately tolerant, so this pins "tolerant" to mean "degrades to
    /// nothing" rather than "panics on a shape it did not expect".
    #[test]
    fn malformed_input_parses_to_nothing_without_panicking() {
        let missing_flex_columns = json!({
            "musicResponsiveListItemRenderer": {
                "playlistItemData": { "videoId": "abc" }
            }
        });
        let null_title = json!({
            "musicResponsiveListItemRenderer": {
                "playlistItemData": { "videoId": "abc" },
                "flexColumns": [
                    { "musicResponsiveListItemFlexColumnRenderer": { "text": { "runs": [{ "text": null }] } } },
                    { "musicResponsiveListItemFlexColumnRenderer": { "text": { "runs": [] } } }
                ]
            }
        });
        let wrong_types = json!({
            "musicResponsiveListItemRenderer": {
                "playlistItemData": { "videoId": 42 },
                "flexColumns": "not an array",
                "thumbnail": { "musicThumbnailRenderer": { "thumbnail": { "thumbnails": "nope" } } }
            }
        });
        let empty_arrays = json!({
            "musicResponsiveListItemRenderer": {
                "playlistItemData": { "videoId": "abc" },
                "flexColumns": [],
                "thumbnail": { "musicThumbnailRenderer": { "thumbnail": { "thumbnails": [] } } }
            }
        });
        let no_results = json!({
            "contents": {
                "tabbedSearchResultsRenderer": { "tabs": [{ "tabRenderer": { "content": {
                    "sectionListRenderer": { "contents": [{
                        "itemSectionRenderer": { "contents": [{
                            "messageRenderer": { "text": { "runs": [{ "text": "No results found" }] } }
                        }] }
                    }] }
                } } }] }
            }
        });
        // The consent/interstitial body YouTube serves to an un-consented client.
        let consent_body = json!({
            "consentBumpRenderer": {
                "title": { "runs": [{ "text": "Before you continue" }] },
                "actionButton": { "buttonRenderer": { "text": { "runs": [{ "text": "I agree" }] } } }
            }
        });
        let error_body = json!({
            "error": { "code": 400, "message": "API server error", "status": "INVALID_ARGUMENT" }
        });
        let not_an_object = json!([1, "two", null, { "three": 3 }]);
        let deeply_nested = {
            // Pathological nesting, to make sure the recursive walk terminates.
            let mut v = json!({ "musicResponsiveListItemRenderer": {} });
            for _ in 0..64 {
                v = json!({ "child": v });
            }
            v
        };

        let cases: Vec<(&str, Value)> = vec![
            ("missing flexColumns", missing_flex_columns),
            ("null title", null_title),
            ("wrong JSON types", wrong_types),
            ("empty arrays", empty_arrays),
            ("no results", no_results),
            ("consent body", consent_body),
            ("error body", error_body),
            ("not an object", not_an_object),
            ("deeply nested", deeply_nested),
        ];

        for (name, body) in cases {
            assert!(tracks(&body).is_empty(), "tracks: {name} produced rows");
            assert!(
                super::artists(&body).is_empty(),
                "artists: {name} produced rows"
            );
            assert!(
                super::albums(&body).is_empty(),
                "albums: {name} produced rows"
            );
            assert!(
                super::album_page(&body, "MPREb_x").is_none(),
                "album_page: {name} produced a page"
            );
            assert!(
                super::artist_page(&body, "UCx").is_none(),
                "artist_page: {name} produced a page"
            );
            let (name_opt, pl, cont) = super::playlist_page(&body);
            assert!(name_opt.is_none(), "playlist name: {name}");
            assert!(pl.is_empty(), "playlist tracks: {name}");
            assert!(cont.is_none(), "playlist continuation: {name}");
            assert!(
                track_credits(&body, "abc").is_none(),
                "credits: {name} produced a byline"
            );
        }
    }

    /// M14: the backward scan must skip a run that carries no string `text`
    /// rather than abandon the whole search.
    ///
    /// The subscriber count sits *before* a structural run in document order, so
    /// the loop meets the textless run first. With `?` in place of `continue`,
    /// the count that is present one run earlier is silently lost.
    #[test]
    fn audience_skips_textless_runs_instead_of_abandoning_the_scan() {
        let row = json!({
            "flexColumns": [
                { "musicResponsiveListItemFlexColumnRenderer": { "text": { "runs": [{ "text": "The Weeknd" }] } } },
                { "musicResponsiveListItemFlexColumnRenderer": { "text": { "runs": [
                    { "text": "Artist" },
                    { "text": " • " },
                    { "text": "78.8M monthly audience" },
                    { "navigationEndpoint": { "browseEndpoint": { "browseId": "UCdecoy" } } }
                ] } } }
            ]
        });
        assert_eq!(
            audience(&row),
            Some(78_800_000),
            "a textless run after the count must not hide the count"
        );
    }

    /// M14 for the year: the same backward scan, the same requirement. A
    /// textless run — including one whose `text` is the wrong JSON type — is
    /// skipped, not fatal.
    #[test]
    fn trailing_year_skips_textless_runs_instead_of_abandoning_the_scan() {
        let runs = vec![
            json!({ "text": "Album" }),
            json!({ "text": " • " }),
            json!({ "text": "2013" }),
            json!({ "navigationEndpoint": { "browseEndpoint": { "browseId": "MPREb_x" } } }),
        ];
        assert_eq!(trailing_year(&runs), Some(2013));

        let null_then_year = vec![json!({ "text": "2013" }), json!({ "text": null })];
        assert_eq!(trailing_year(&null_then_year), Some(2013));

        assert_eq!(trailing_year(&[json!({ "text": "not a year" })]), None);
        assert_eq!(trailing_year(&[]), None);
    }

    /// 3.8: a response that carries rows but yields no tracks must say so.
    ///
    /// "The renderer was renamed" and "the search found nothing" are identical
    /// from the caller's side, and the warning is the only thing that tells them
    /// apart. Asserting on it needs a capturing subscriber: `tracing` ships no
    /// test subscriber and this crate must not gain a `tracing-subscriber`
    /// dependency for one test, so the five required methods are implemented
    /// here. The dispatcher is thread-local, so parallel tests cannot see each
    /// other's events.
    #[test]
    fn a_response_with_rows_that_yield_nothing_warns_once() {
        use std::sync::{Arc, Mutex};

        /// Collects the `message` field of every event recorded through it.
        #[derive(Clone, Default)]
        struct CapturedWarnings(Arc<Mutex<Vec<String>>>);

        struct MessageVisitor(String);

        impl tracing::field::Visit for MessageVisitor {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                if field.name() == "message" {
                    self.0 = format!("{value:?}");
                }
            }
        }

        impl tracing::Subscriber for CapturedWarnings {
            fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
                true
            }
            fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
                tracing::span::Id::from_u64(1)
            }
            fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
            fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
            fn enter(&self, _: &tracing::span::Id) {}
            fn exit(&self, _: &tracing::span::Id) {}
            fn event(&self, event: &tracing::Event<'_>) {
                let mut visitor = MessageVisitor(String::new());
                event.record(&mut visitor);
                self.0.lock().unwrap().push(visitor.0);
            }
        }

        // A row renderer the parser cannot read: no title, no id. The row was
        // there, so this is a layout change, not an empty result.
        let unreadable_row = json!({
            "musicResponsiveListItemRenderer": { "playlistItemData": { "videoId": "abc" } }
        });
        // A response with no rows at all: a genuine "no results", which must not
        // cry wolf.
        let no_results = json!({
            "contents": { "sectionListRenderer": { "contents": [
                { "itemSectionRenderer": { "contents": [
                    { "messageRenderer": { "text": { "runs": [{ "text": "No results found" }] } } }
                ] } }
            ] } }
        });
        // A response whose only row this parser dropped on purpose (a UGC
        // upload). Empty, but explained — a video-heavy search must not look
        // like a layout change.
        let only_dropped_on_purpose = one_shelf(vec![search_row(
            "ugc-only",
            "Some Cover",
            "Song",
            "MUSIC_VIDEO_TYPE_UGC",
        )]);

        let warnings = CapturedWarnings::default();
        let captured = warnings.clone();
        tracing::subscriber::with_default(warnings, || {
            assert!(tracks(&unreadable_row).is_empty());
            assert!(tracks(&no_results).is_empty());
            assert!(tracks(&only_dropped_on_purpose).is_empty());
        });

        let logged = captured.0.lock().unwrap().clone();
        assert_eq!(
            logged.len(),
            1,
            "exactly one warning: the unreadable row. The empty search and the \
             all-UGC page are explained results. Got {logged:?}"
        );
        assert!(
            logged[0].contains("row(s) in the response but none could be parsed"),
            "unexpected warning text: {logged:?}"
        );
    }
}
