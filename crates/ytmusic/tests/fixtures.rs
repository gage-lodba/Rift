//! Parser tests against recorded innertube responses.
//!
//! Each fixture in `tests/fixtures/` is a real YouTube Music response captured
//! from the live site, compacted down to only the keys the parsers read. The
//! compaction is verified at generation time (`examples/prune_fixture.rs`
//! re-parses the original and the compacted document and refuses to write
//! unless they agree), so these assertions are assertions about production
//! parsing, not about a hand-written stub.
//!
//! Fixtures go stale the way real responses do: if YouTube changes a renderer,
//! one of these fails and the parser needs updating. That is the point.

use std::fs;
use std::path::PathBuf;

use rift_types::{AlbumSummary, ArtistSummary, Track};
use rift_ytmusic::raw;
use serde_json::Value;

const ALBUM_ID: &str = "MPREb_K8qWMWVqXGi";
const ARTIST_ID: &str = "UCRr1xG_2WIDs18a6cIiCxeA";

fn fixture(name: &str) -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    let text = fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("could not read {}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{} is not JSON: {e}", path.display()))
}

fn track_by_id<'a>(tracks: &'a [Track], id: &str) -> &'a Track {
    tracks
        .iter()
        .find(|t| t.id == id)
        .unwrap_or_else(|| panic!("no track {id} in {tracks:#?}"))
}

fn artist_by_id<'a>(artists: &'a [ArtistSummary], id: &str) -> &'a ArtistSummary {
    artists
        .iter()
        .find(|a| a.id == id)
        .unwrap_or_else(|| panic!("no artist {id} in {artists:#?}"))
}

fn album_by_id<'a>(albums: &'a [AlbumSummary], id: &str) -> &'a AlbumSummary {
    albums
        .iter()
        .find(|a| a.id == id)
        .unwrap_or_else(|| panic!("no album {id} in {albums:#?}"))
}

// ---------------------------------------------------------------------------
// Track search
// ---------------------------------------------------------------------------

#[test]
fn search_tracks_parses_ids_titles_durations_and_covers() {
    let tracks = raw::tracks(&fixture("search_tracks.json"));

    assert_eq!(
        tracks.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(),
        [
            "J7p4bzqLvCw",
            "a4O-abCXsfA",
            "l5GSZ2-TP6Y",
            "wjE2YXSo2FM",
            "wnTz65qqxI0",
            "RmYCOm4ehKs",
            "qPRNIHxLhmc",
            "QgMYdDKVkrw",
        ]
    );

    let blinding_lights = track_by_id(&tracks, "J7p4bzqLvCw");
    assert_eq!(blinding_lights.title, "Blinding Lights");
    // Duration comes from the subtitle column's trailing run.
    assert_eq!(blinding_lights.duration, Some(202));
    // Cover comes from the 120px thumbnail, not the 60px or the largest.
    assert!(
        blinding_lights.cover.contains("w120-h120"),
        "unexpected cover: {}",
        blinding_lights.cover
    );
    // Search rows carry no album, and credits are backfilled separately.
    assert_eq!(blinding_lights.album, None);
    assert_eq!(blinding_lights.album_id, None);
    assert!(blinding_lights.artists.is_empty());
}

/// A row whose artist is not linked must not render the whole subtitle column
/// as the artist.
///
/// This used to assert `"Song • 3:22"` — the type badge, a separator, and the
/// duration — which enshrined the defect the fallback was supposed to avoid.
/// The subtitle column mixes all three into one run list, so the byline has to
/// be picked out of it.
#[test]
fn search_tracks_falls_back_to_the_subtitle_for_unlinked_artists() {
    let tracks = raw::tracks(&fixture("search_tracks.json"));

    // No artist link anywhere in the row: the badge and duration are dropped and
    // nothing is left, which is honest — better blank than "Song • 3:22".
    let unlinked = track_by_id(&tracks, "J7p4bzqLvCw");
    assert_eq!(unlinked.artist, "");
    assert_eq!(unlinked.duration, Some(202), "the duration is still parsed");

    let linked = track_by_id(&tracks, "wjE2YXSo2FM");
    assert_eq!(linked.artist, "The Weeknd");
}

/// A row whose byline is a plain unlinked name must keep that name.
///
/// The row for "Around The World - A Daft Punk Tribute" carries no artist link
/// at all, and the name in its subtitle is the only one available. The fallback
/// has to keep it while still dropping the badge, separators and duration — the
/// two requirements pull in opposite directions, and the first version of this
/// fix satisfied one by breaking the other.
#[test]
fn search_albums_keeps_an_unlinked_byline() {
    let albums = raw::albums(&fixture("search_albums.json"));

    let tribute = album_by_id(&albums, "MPREb_bBLhlwZWbpp");
    assert_eq!(tribute.name, "Around The World - A Daft Punk Tribute");
    assert_eq!(
        tribute.artist, "",
        "this row carries no byline at all; the badge/year must not fill the gap"
    );
    assert_eq!(tribute.year, Some(2022), "the year is still parsed");

    // Every album row keeps a linked artist, and none of them swallowed the
    // type badge or the year.
    for a in &albums {
        assert!(!a.artist.contains("Album"), "badge in byline: {a:?}");
        assert!(!a.artist.contains("2013"), "year in byline: {a:?}");
    }
    assert_eq!(album_by_id(&albums, ALBUM_ID).artist, "Daft Punk");
}

/// The UGC filter is what keeps remixes and re-uploads out of results; if it
/// regresses, this fixture's UGC rows come back.
///
/// The ids are named rather than merely counted: a count alone cannot say
/// *which* row came back, so a regression would read as "one extra track" with
/// nothing to point at. The two `MUSIC_VIDEO_TYPE_UGC` rows below are the typed
/// signal's own; the `OMV` and podcast-episode rows are dropped by their badge.
#[test]
fn search_tracks_drops_ugc_and_video_rows() {
    let tracks = raw::tracks(&fixture("search_tracks.json"));

    // The recording holds both kinds of droppable row plus audio rows; only the
    // audio ones survive.
    assert_eq!(tracks.len(), 8);
    for t in &tracks {
        assert!(!t.id.is_empty());
        assert!(!t.title.is_empty(), "title lost for {}", t.id);
    }

    // Rows the fixture carries and the parser must not return. The first two
    // are typed `MUSIC_VIDEO_TYPE_UGC`, the third `MUSIC_VIDEO_TYPE_OMV`, and
    // the last three are podcast episodes.
    for dropped in [
        "Dp-MTReQl0A",
        "0OQq9jEeLvI",
        "Mx92lTYxrJQ",
        "LfCDDbRl9_Y",
        "KEz-w6-NU78",
        "4GmQengq5F8",
    ] {
        assert!(
            !tracks.iter().any(|t| t.id == dropped),
            "{dropped} survived the audio filter: {tracks:#?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Artist and album search
// ---------------------------------------------------------------------------

#[test]
fn search_artists_parses_ids_names_avatars_and_subscribers() {
    let artists = raw::artists(&fixture("search_artists.json"));
    assert_eq!(artists.len(), 11);

    let daft_punk = artist_by_id(&artists, ARTIST_ID);
    assert_eq!(daft_punk.name, "Daft Punk");
    assert_eq!(daft_punk.subscribers, Some(78_800_000));
    assert!(daft_punk.avatar.contains("googleusercontent.com"));

    // The compact "22" case, i.e. a plain integer with no K/M suffix.
    assert_eq!(
        artist_by_id(&artists, "UCZ2J4f4052BX-NcUh55AFgg").subscribers,
        Some(22)
    );
}

/// Only rows whose own link is `UC…` are artists; a track or album row that
/// happens to appear in the response must not be returned as an artist.
#[test]
fn search_artists_rejects_non_artist_rows() {
    for a in raw::artists(&fixture("search_artists.json")) {
        assert!(a.id.starts_with("UC"), "not an artist id: {}", a.id);
        assert!(!a.name.is_empty());
    }
}

#[test]
fn search_albums_parses_year_and_type() {
    let albums = raw::albums(&fixture("search_albums.json"));
    assert_eq!(albums.len(), 20);

    let ram = album_by_id(&albums, ALBUM_ID);
    assert_eq!(ram.name, "Random Access Memories");
    assert_eq!(ram.artist, "Daft Punk");
    assert_eq!(ram.artist_id.as_deref(), Some(ARTIST_ID));
    assert_eq!(ram.year, Some(2013));
    assert_eq!(ram.album_type, "Album");

    // Singles and EPs are labelled by the subtitle's leading word.
    assert_eq!(
        album_by_id(&albums, "MPREb_Wy0wUNYLgkL").album_type,
        "Single"
    );
    assert_eq!(album_by_id(&albums, "MPREb_ePmUY37w4pZ").album_type, "EP");
}

#[test]
fn search_albums_rejects_non_album_rows() {
    for a in raw::albums(&fixture("search_albums.json")) {
        assert!(a.id.starts_with("MPREb_"), "not an album id: {}", a.id);
    }
}

// ---------------------------------------------------------------------------
// Album page
// ---------------------------------------------------------------------------

#[test]
fn album_page_parses_header_and_full_track_list() {
    let page = raw::album_page(&fixture("album.json"), ALBUM_ID).expect("album page");

    assert_eq!(page.id, ALBUM_ID);
    assert_eq!(page.name, "Random Access Memories");
    assert_eq!(page.artist, "Daft Punk");
    assert_eq!(page.artist_id.as_deref(), Some(ARTIST_ID));
    assert_eq!(page.year, Some(2013));
    assert_eq!(page.album_type, "Album");
    assert!(page.cover.contains("w544-h544"));
    assert_eq!(page.tracks.len(), 13);

    let first = &page.tracks[0];
    assert_eq!(first.id, "IluRBvnYMoY");
    assert_eq!(first.title, "Give Life Back to Music");
    assert_eq!(first.duration, Some(276));
    // Album rows omit the album name; it is inherited from the page header.
    assert_eq!(first.album.as_deref(), Some("Random Access Memories"));
    assert_eq!(first.album_id.as_deref(), Some(ALBUM_ID));
    assert!(first.cover.contains("w544-h544"));

    let last = &page.tracks[12];
    assert_eq!(last.id, "ZbbTmR6Xaag");
    assert_eq!(last.title, "Contact");
}

/// Track order is the album's own order, and every row is attributed even when
/// the row itself carries no album link.
#[test]
fn album_page_attributes_every_track_to_the_album() {
    let page = raw::album_page(&fixture("album.json"), ALBUM_ID).expect("album page");

    assert_eq!(
        page.tracks[7].title,
        "Get Lucky (feat. Pharrell Williams and Nile Rodgers)"
    );
    assert_eq!(
        page.tracks[7].artist,
        "Daft Punk, Pharrell Williams, Nile Rodgers"
    );
    for t in &page.tracks {
        assert_eq!(t.album.as_deref(), Some("Random Access Memories"));
        assert_eq!(t.album_id.as_deref(), Some(ALBUM_ID));
        assert!(t.duration.is_some(), "duration lost for {}", t.id);
    }
}

// ---------------------------------------------------------------------------
// Artist page
// ---------------------------------------------------------------------------

#[test]
fn artist_page_parses_header_songs_and_albums() {
    let page = raw::artist_page(&fixture("artist.json"), ARTIST_ID).expect("artist page");

    assert_eq!(page.id, ARTIST_ID);
    assert_eq!(page.name, "Daft Punk");
    assert_eq!(page.subscribers, Some(7_170_000));
    assert!(page.image.contains("googleusercontent.com"));

    // The description is the first run of `description`. YouTube folds the
    // Wikipedia attribution into that same run rather than emitting it
    // separately, so the whole paragraph (attribution included) is what the UI
    // has to render.
    let description = page.description.as_deref().expect("description");
    assert!(description.starts_with("Daft Punk were a French electronic music duo formed in 1993"));
    assert!(description.contains("They are regarded as one of the most influential acts"));
    assert!(
        description.contains("From Wikipedia"),
        "description truncated early"
    );

    assert_eq!(page.tracks.len(), 5);
    assert_eq!(page.tracks[0].id, "khnokW3Mw24");
    assert_eq!(
        page.tracks[0].album.as_deref(),
        Some("Random Access Memories")
    );
    assert_eq!(page.tracks[0].album_id.as_deref(), Some(ALBUM_ID));

    assert_eq!(page.albums.len(), 20);
    let ram = album_by_id(&page.albums, ALBUM_ID);
    assert_eq!(ram.name, "Random Access Memories");
    assert_eq!(ram.year, Some(2013));
    // Artist-page cards carry no artist link (the page is the artist).
    assert!(ram.artist.is_empty());
    assert_eq!(ram.artist_id, None);
}

/// The "Show all songs" link is a `VL…` browse id that the UI pastes into a
/// playlist request, so the `VL` prefix must be stripped and nothing else kept.
#[test]
fn artist_page_extracts_the_show_all_songs_playlist_id() {
    let page = raw::artist_page(&fixture("artist.json"), ARTIST_ID).expect("artist page");

    let playlist_id = page
        .tracks_playlist_id
        .as_deref()
        .expect("show-all playlist id");
    assert!(!playlist_id.starts_with("VL"), "VL prefix not stripped");
    assert_eq!(playlist_id, "OLAK5uy_lNVBcjNtiCwq-n95uoxJ-Gd4vsfMkyZNs");
}

// ---------------------------------------------------------------------------
// Playlist
// ---------------------------------------------------------------------------

#[test]
fn playlist_page_parses_name_tracks_and_continuation() {
    let (name, tracks, continuation) = raw::playlist_page(&fixture("playlist.json"));

    assert_eq!(name.as_deref(), Some("Homelab"));
    assert_eq!(tracks.len(), 100);
    assert!(
        continuation.is_some(),
        "first page must offer a continuation for pagination"
    );

    assert_eq!(tracks[0].id, "cVWF3u-y-Zg");
    assert_eq!(tracks[0].title, "I put a computer in my computer");
    assert_eq!(tracks[0].artist, "Jeff Geerling");
    assert_eq!(tracks[0].duration, Some(720));

    // A 100-track page with no duplicates means continuation parsing did not
    // re-emit rows.
    let mut ids: Vec<&str> = tracks.iter().map(|t| t.id.as_str()).collect();
    ids.sort_unstable();
    let unique = ids.len();
    ids.dedup();
    assert_eq!(ids.len(), unique, "duplicate tracks in one page");
}

// ---------------------------------------------------------------------------
// Track credits
// ---------------------------------------------------------------------------

/// The video the `next.json` queue was opened for. `track_credits` matches the
/// byline to this id, so a fixture captured for a different video would not
/// yield credits — which is the point (see the parser's own tests).
const CREDITS_VIDEO_ID: &str = "khnokW3Mw24";

#[test]
fn track_credits_splits_artists_from_the_album() {
    let (artists, album, album_id) =
        raw::track_credits(&fixture("next.json"), CREDITS_VIDEO_ID).expect("track credits");

    let credits: Vec<(Option<&str>, &str)> = artists
        .iter()
        .map(|a| (a.id.as_deref(), a.name.as_str()))
        .collect();
    assert_eq!(
        credits,
        [
            (Some(ARTIST_ID), "Daft Punk"),
            (Some("UCWhpbdGLnR8XtXLr0yl2nYA"), "Julian Casablancas"),
        ]
    );

    assert_eq!(album.as_deref(), Some("Random Access Memories"));
    assert_eq!(album_id.as_deref(), Some(ALBUM_ID));
}

/// A video the queue does not contain must yield nothing rather than whatever
/// byline happens to be present — the caller persists what it gets, so a
/// substituted or unavailable video would otherwise be credited to another
/// song's artists on the requested track's row.
#[test]
fn track_credits_refuses_a_video_the_response_does_not_contain() {
    assert!(raw::track_credits(&fixture("next.json"), "not-in-this-queue").is_none());
}
