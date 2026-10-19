//! Track-list interaction tests: kebab/heart/artist-link nested controls, the
//! head Play/Download pills, the collection header state machines, and the
//! context-menu entry order.

use super::*;
use rift_types::{DownloadState, Library, PlaybackState, Playlist};

use crate::screens::{cards, menu};
use crate::state::{Action, State};
use crate::text::Weight;
use crate::ui::{Color, Id, Rect};

use super::header::blocked_by_other_playlist;

fn track(id: &str, title: &str, dur: Option<u32>) -> Track {
    Track {
        id: id.into(),
        title: title.into(),
        artist: "Artist Name".into(),
        album: Some("Album Name".into()),
        duration: dur,
        cover: String::new(),
        artists: Vec::new(),
        album_id: None,
    }
}

// ------------------------------------------------------------------
// scripted interaction tests (task-12)
// ------------------------------------------------------------------

use crate::screens::menu::harness::{menu_row_center, Sim};
use crate::ui::Ui;
use rift_types::ArtistRef;

/// A track with every menu affordance: structured artist id + album id.
fn rich_track() -> Track {
    Track {
        id: "rt".into(),
        title: "Rich Song".into(),
        artist: "Artist Name".into(),
        album: Some("Album Name".into()),
        duration: Some(200),
        cover: String::new(),
        artists: vec![ArtistRef {
            id: Some("artist-1".into()),
            name: "Artist Name".into(),
        }],
        album_id: Some("album-1".into()),
    }
}

fn rich_state() -> State {
    let mut state = State::default();
    state.library.playlists = vec![Playlist {
        id: "pl-1".into(),
        name: "Mix".into(),
        tracks: Vec::new(),
    }];
    state
}

/// Click the kebab of row 0 through the real list renderer and assert the
/// track menu opens for that track.
#[test]
fn kebab_click_opens_the_track_menu() {
    let _guard = menu::lock_for_tests();
    let mut state = rich_state();
    let tracks = vec![rich_track()];
    let rect = Rect::new(100.0, 100.0, 766.0, 54.0);
    let opts = TrackListOpts {
        id: "kebab-test",
        ..TrackListOpts::default()
    };
    let mut sim = Sim::new();
    let mut draw = |ui: &mut Ui, actions: &mut Vec<Action>| {
        crate::screens::track_list::track_list(ui, &mut state, actions, rect, &tracks, &opts);
        menu::layer(ui, actions);
    };
    sim.step(&mut draw); // establish hover / row ids
    let kebab = (rect.right() - 24.0, rect.y + 27.0);
    let actions = sim.click(kebab, &mut draw);
    assert!(menu::is_open(), "kebab click must open the menu");
    assert!(
        !actions
            .iter()
            .any(|a| matches!(a, Action::PlayTracks { .. })),
        "clicking the kebab must not also play the row: {actions:?}"
    );
    assert_eq!(
        menu::open_id(),
        Some(Id::from_str("track-menu").with(&tracks[0].id))
    );
}

/// The heart button is a nested control of the row: clicking it emits
/// `ToggleLike` and must NOT play the track (pins the nested-control ordering
/// that `cards::track_row` fixes).
#[test]
fn heart_click_emits_toggle_like_without_playing() {
    let _guard = menu::lock_for_tests();
    let mut state = rich_state();
    let tracks = vec![rich_track()];
    let rect = Rect::new(100.0, 100.0, 766.0, 54.0);
    let opts = TrackListOpts {
        id: "heart-test",
        ..TrackListOpts::default()
    };
    let mut sim = Sim::new();
    let mut draw = |ui: &mut Ui, actions: &mut Vec<Action>| {
        crate::screens::track_list::track_list(ui, &mut state, actions, rect, &tracks, &opts);
        menu::layer(ui, actions);
    };
    sim.step(&mut draw); // hover (reveals the kebab; heart is always visible)
                         // Like button: row.right() - 66 .. -38 (the 56 px actions tail).
    let heart = (rect.right() - 52.0, rect.y + 27.0);
    let actions = sim.click(heart, &mut draw);
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, Action::ToggleLike(t) if t.id == "rt")),
        "heart must toggle the like: {actions:?}"
    );
    assert!(
        !actions
            .iter()
            .any(|a| matches!(a, Action::PlayTracks { .. })),
        "clicking the heart must not also play the row: {actions:?}"
    );
}

/// Every leaf entry of the real track menu dispatches exactly one expected
/// Action; the submenu expands and its option dispatches `AddToPlaylist`.
#[test]
fn each_track_menu_entry_emits_its_action() {
    let _guard = menu::lock_for_tests();
    let state = rich_state();
    let track = rich_track();
    let entries = track_menu_entries(&state, &track, Some("pl-1"));

    let anchor = Rect::new(300.0, 300.0, 28.0, 28.0);
    let leaves: Vec<(usize, Action)> = entries
        .iter()
        .enumerate()
        .filter_map(|(idx, entry)| match entry {
            menu::Entry::Item {
                item,
                action: Some(action),
            } if item.enabled => Some((idx, action.clone())),
            _ => None,
        })
        .collect();
    assert!(leaves.len() >= 5, "expected several actionable entries");

    for (idx, expected) in &leaves {
        let list = vec![entries[*idx].clone()];
        let mut sim = Sim::new();
        menu::open(
            &mut sim.ui,
            Id::from_str("menu-under-test"),
            anchor,
            list.clone(),
        );
        let mut draw = |ui: &mut Ui, actions: &mut Vec<Action>| {
            menu::layer(ui, actions);
        };
        let at = menu_row_center(&mut sim.ui, &list, anchor, None, 0);
        let actions = sim.click(at, &mut draw);
        assert_eq!(actions.len(), 1, "entry {idx} must emit exactly one action");
        assert_same_action(&actions[0], expected);
        assert!(menu::open_id().is_none(), "menu closes after the action");
    }

    // The submenu: click "Add to playlist", it expands; click the option.
    let sub_idx = entries
        .iter()
        .position(|e| matches!(e, menu::Entry::Sub { .. }))
        .expect("submenu entry");
    let list = vec![entries[sub_idx].clone()];
    let mut sim = Sim::new();
    menu::open(
        &mut sim.ui,
        Id::from_str("submenu-under-test"),
        anchor,
        list.clone(),
    );
    let mut draw = |ui: &mut Ui, actions: &mut Vec<Action>| {
        menu::layer(ui, actions);
    };
    let parent = menu_row_center(&mut sim.ui, &list, anchor, None, 0);
    let actions = sim.click(parent, &mut draw);
    assert!(actions.is_empty(), "expanding a submenu emits nothing");
    assert!(menu::is_open(), "menu stays open while the submenu expands");
    let option = menu_row_center(&mut sim.ui, &list, anchor, Some(0), 1);
    let actions = sim.click(option, &mut draw);
    assert_eq!(actions.len(), 1);
    assert!(matches!(
        &actions[0],
        Action::AddToPlaylist { playlist_id, track } if playlist_id == "pl-1" && track.id == "rt"
    ));
}

/// Variant + payload comparison for `Action` (it deliberately has no
/// `PartialEq`).
fn assert_same_action(actual: &Action, expected: &Action) {
    use Action::*;
    match (actual, expected) {
        (QueuePlayNext(a), QueuePlayNext(b)) | (QueueAdd(a), QueueAdd(b)) => assert_eq!(a, b),
        (OpenArtist(a), OpenArtist(b)) => assert_eq!(a, b),
        (OpenAlbum(a), OpenAlbum(b)) => assert_eq!(a, b),
        (DownloadTracks(a), DownloadTracks(b)) | (RemoveDownloads(a), RemoveDownloads(b)) => {
            assert_eq!(a, b);
        }
        (
            RemoveFromPlaylist {
                playlist_id: a,
                track_id: b,
            },
            RemoveFromPlaylist {
                playlist_id: c,
                track_id: d,
            },
        ) => {
            assert_eq!(a, c);
            assert_eq!(b, d);
        }
        _ => panic!("unexpected action pair: {actual:?} vs {expected:?}"),
    }
}

/// Head Play pill: starts the collection when it is not current, toggles when
/// it is.
#[test]
fn head_play_pill_emits_play_tracks_or_toggle() {
    let mut state = rich_state();
    let tracks = vec![rich_track()];
    let rect = Rect::new(100.0, 100.0, 766.0, 400.0);
    let source = Some("liked");
    let mut sim = Sim::new();
    let play_center = play_pill_center(&mut sim.ui, rect, "Play");
    let mut draw = |ui: &mut Ui, actions: &mut Vec<Action>| {
        head_actions(
            ui,
            &state,
            actions,
            HeadActions {
                x: rect.x,
                y: rect.y,
                tracks: &tracks,
                source,
                id_key: "head-test",
            },
        );
    };
    let actions = sim.click(play_center, &mut draw);
    assert!(
        actions.iter().any(|a| matches!(
            a,
            Action::PlayTracks { start, source: s, .. }
                if *start == 0 && s.as_deref() == Some("liked")
        )),
        "Play pill must start the collection: {actions:?}"
    );

    // Current collection & playing -> the pill says Pause and toggles.
    let mut current = rich_state();
    current.queue.source = Some("liked".into());
    current.playback = rift_types::PlaybackState::Playing;
    let mut sim = Sim::new();
    let pause_center = play_pill_center(&mut sim.ui, rect, "Pause");
    let mut draw = |ui: &mut Ui, actions: &mut Vec<Action>| {
        head_actions(
            ui,
            &current,
            actions,
            HeadActions {
                x: rect.x,
                y: rect.y,
                tracks: &tracks,
                source,
                id_key: "head-test",
            },
        );
    };
    let actions = sim.click(pause_center, &mut draw);
    assert!(
        actions.iter().any(|a| matches!(a, Action::TogglePlay)),
        "current collection's pill must toggle: {actions:?}"
    );
    let _ = &mut state;
}

fn play_pill_center(ui: &mut Ui, rect: Rect, label: &str) -> (f32, f32) {
    let style = cards::style(14.0, Weight::Bold, Color::WHITE);
    let w = (44.0 + 16.0 + 7.0 + ui.text_width(label, &style)).max(84.0);
    (
        rect.x + w * 0.5,
        rect.y + HEAD_ACTIONS_MARGIN_TOP + HEAD_BTN_H * 0.5,
    )
}

/// Head Download pill: Download when missing, `RemoveDownloads` when complete.
#[test]
fn head_download_pill_emits_download_and_remove() {
    let mut state = rich_state();
    let tracks = vec![rich_track()];
    let rect = Rect::new(100.0, 100.0, 766.0, 400.0);
    let source = Some("liked");

    // Download (nothing downloaded yet).
    let mut sim = Sim::new();
    let center = download_pill_center(&mut sim.ui, rect, &state, &tracks);
    let mut draw = |ui: &mut Ui, actions: &mut Vec<Action>| {
        head_actions(
            ui,
            &state,
            actions,
            HeadActions {
                x: rect.x,
                y: rect.y,
                tracks: &tracks,
                source,
                id_key: "dl-test",
            },
        );
    };
    let actions = sim.click(center, &mut draw);
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, Action::DownloadTracks(ids) if ids.len() == 1 && ids[0] == "rt")),
        "Download pill: {actions:?}"
    );

    // All downloaded -> RemoveDownloads.
    let mut complete = rich_state();
    complete.downloads.downloaded.insert("rt".into());
    let mut sim = Sim::new();
    let center = download_pill_center(&mut sim.ui, rect, &complete, &tracks);
    let mut draw = |ui: &mut Ui, actions: &mut Vec<Action>| {
        head_actions(
            ui,
            &complete,
            actions,
            HeadActions {
                x: rect.x,
                y: rect.y,
                tracks: &tracks,
                source,
                id_key: "dl-test",
            },
        );
    };
    let actions = sim.click(center, &mut draw);
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, Action::RemoveDownloads(ids) if ids.len() == 1 && ids[0] == "rt")),
        "Downloaded pill: {actions:?}"
    );
    let _ = &mut state;
}

fn download_pill_center(ui: &mut Ui, rect: Rect, state: &State, tracks: &[Track]) -> (f32, f32) {
    let theme = ui.theme().clone();
    let ids: Vec<String> = tracks.iter().map(|t| t.id.clone()).collect();
    let done = ids
        .iter()
        .filter(|id| state.downloads.downloaded.contains(*id))
        .count();
    let st = download_head_state(ids.len(), done, 0, false);
    let play_w = {
        let style = cards::style(14.0, Weight::Bold, Color::WHITE);
        (44.0 + 16.0 + 7.0 + ui.text_width("Play", &style)).max(84.0)
    };
    let dl_w = (36.0
        + 16.0
        + 7.0
        + ui.text_width(
            &st.label(),
            &cards::style(14.0, Weight::SemiBold, theme.text),
        ))
    .max(96.0);
    (
        rect.x + play_w + HEAD_ACTIONS_GAP + dl_w * 0.5,
        rect.y + HEAD_ACTIONS_MARGIN_TOP + HEAD_BTN_H * 0.5,
    )
}

#[test]
fn collection_meta_counts_songs_and_total_duration() {
    let tracks = vec![
        track("1", "A", Some(200)),
        track("2", "B", Some(150)),
        track("3", "C", None),
    ];
    assert_eq!(collection_meta_text(&tracks), "3 songs • 5 min");
    let one = vec![track("1", "A", Some(42))];
    assert_eq!(collection_meta_text(&one), "1 song • 42 sec");
    assert_eq!(collection_meta_text(&[]), "0 songs • 0 sec");
}

#[test]
fn head_actions_total_height() {
    // 18 top margin + 35 button + 16 bottom margin.
    assert_eq!(HEAD_ACTIONS_H, 69.0);
}

#[test]
fn download_head_button_state_machine() {
    assert_eq!(
        download_head_state(10, 0, 0, false),
        DownloadHeadState::Download
    );
    assert!(download_head_state(3, 3, 0, false).enabled());
    assert!(!download_head_state(3, 3, 0, true).enabled());
    // Downloading wins over "all done" and is disabled.
    assert_eq!(
        download_head_state(3, 3, 1, true),
        DownloadHeadState::Downloading { done: 3, total: 3 }
    );
    assert_eq!(
        download_head_state(10, 0, 2, false).label(),
        "Downloading 1/10"
    );
    // The counter is 1-indexed and clamped to total.
    assert_eq!(
        download_head_state(4, 3, 1, false).label(),
        "Downloading 4/4"
    );
}

#[test]
fn current_collection_needs_a_source_and_a_loaded_track() {
    let mut state = State::default();
    assert!(!is_current_collection(&state, Some("liked")));
    state.queue.source = Some("liked".into());
    state.playback = PlaybackState::Playing;
    assert!(is_current_collection(&state, Some("liked")));
    // A different collection is not current.
    assert!(!is_current_collection(&state, Some("playlist:x")));
    // No source never toggles.
    assert!(!is_current_collection(&state, None));
    // Stopped playback is not current even with a matching source.
    state.playback = PlaybackState::Stopped;
    assert!(!is_current_collection(&state, Some("liked")));
}

#[test]
fn track_menu_matches_the_old_order() {
    let mut state = State {
        library: Library {
            playlists: vec![Playlist {
                id: "pl-1".into(),
                name: "Mix".into(),
                tracks: Vec::new(),
            }],
            ..Library::default()
        },
        ..State::default()
    };
    let t = track("t1", "Song", Some(100));
    let entries = track_menu_entries(&state, &t, Some("pl-1"));
    let labels: Vec<String> = entries
        .iter()
        .filter_map(|e| match e {
            menu::Entry::Item { item, .. } => Some(item.label.clone()),
            menu::Entry::Sub { label, .. } => Some(label.clone()),
            menu::Entry::Separator => None,
        })
        .collect();
    assert_eq!(
        labels,
        vec![
            "Play next",
            "Add to queue",
            "Add to playlist",
            "Download",
            "Remove from playlist",
        ]
    );
    assert!(entries.iter().any(|e| matches!(e, menu::Entry::Separator)));

    // A downloaded track offers "Remove download", not "Download".
    state.downloads = DownloadState::default();
    state.downloads.downloaded.insert("t1".into());
    let entries = track_menu_entries(&state, &t, None);
    assert!(entries.iter().any(|e| matches!(
        e,
        menu::Entry::Item { item, .. } if item.label == "Remove download"
    )));
}

#[test]
fn blocked_by_other_playlist_ignores_the_collection_itself() {
    let mut state = State {
        library: Library {
            playlists: vec![Playlist {
                id: "pl-1".into(),
                name: "One".into(),
                tracks: vec![track("t1", "A", None)],
            }],
            ..Library::default()
        },
        ..State::default()
    };
    state.downloads.downloaded.insert("t1".into());
    let tracks = vec![track("t1", "A", None)];
    // Viewing the same playlist: its own pinning does not block removal.
    assert!(!blocked_by_other_playlist(
        &state,
        &tracks,
        Some("playlist:pl-1")
    ));
    // Viewing a different collection: the pinned copy blocks removal.
    assert!(blocked_by_other_playlist(&state, &tracks, Some("liked")));
    assert!(blocked_by_other_playlist(&state, &tracks, None));
}

/// Artist credit names are links (the old row's `artist_links`): clicking
/// one navigates to that artist's profile and must not play the row.
/// Regression for the reported "can't click artist names" bug.
#[test]
fn artist_credit_click_navigates_to_the_profile() {
    let _guard = menu::lock_for_tests();
    let mut state = rich_state();
    let tracks = vec![rich_track()];
    let rect = Rect::new(100.0, 100.0, 766.0, 54.0);
    let opts = TrackListOpts {
        id: "artist-link-test",
        ..TrackListOpts::default()
    };
    let mut sim = Sim::new();
    let mut draw = |ui: &mut Ui, actions: &mut Vec<Action>| {
        crate::screens::track_list::track_list(ui, &mut state, actions, rect, &tracks, &opts);
        menu::layer(ui, actions);
    };
    sim.step(&mut draw); // hover / establish ids

    // Artist line: after the 40 px cover + 12 px gap, second text line.
    let meta_x = rect.x + cards::TROW_PAD_X + cards::TROW_COVER + cards::TROW_GAP;
    let artist_y = rect.y
        + (cards::TROW_HEIGHT - (cards::TROW_TITLE_LINE + cards::TROW_ARTIST_LINE)) * 0.5
        + cards::TROW_TITLE_LINE;
    let actions = sim.click((meta_x + 5.0, artist_y + 8.0), &mut draw);

    assert!(
        actions
            .iter()
            .any(|a| matches!(a, Action::OpenArtist(id) if id == "artist-1")),
        "clicking the artist name must navigate to the profile: {actions:?}"
    );
    assert!(
        !actions
            .iter()
            .any(|a| matches!(a, Action::PlayTracks { .. })),
        "clicking the artist name must not also play the row: {actions:?}"
    );
}

#[test]
fn list_height_is_54_per_row() {
    assert_eq!(list_height(0), 0.0);
    assert_eq!(list_height(5), 270.0);
}
