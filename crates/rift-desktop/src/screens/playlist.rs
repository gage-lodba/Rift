//! Playlist view (`View::Playlist(id)`).
//!
//! Spec: `ui-spec.md` §7.3.
//!
//! * not found → `.empty` "Playlist not found." (no header — the old app's match
//!   arm returns only the empty state);
//! * header `.view-head`: h2 with the name + a kebab (Rename / Delete);
//! * rename mode: the h2 is replaced by `.rename-input` (17/700, raised bg,
//!   1px accent border, radius 6, padding 4x10, min-width 260, autofocus);
//! * empty → `.empty` "Empty playlist. Use ♪+ on any track to add it.";
//! * else `.collection-meta`, the Play/Download action row (source
//!   `"playlist:<id>"`) and the track list, whose row menus gain
//!   "Remove from playlist".

use crate::screens::{menu, pages, track_list};
use crate::state::{Action, State};
use crate::text::Weight;
use crate::ui::{Icon, Id, Rect, TextInputStyle, Ui};

/// The header row height: the taller of the 22 px h2 / 28 px kebab.
pub const HEAD_H: f32 = 28.0;
/// `.rename-input` height: 22 line box + 4x2 padding + 1px border each side.
pub const RENAME_H: f32 = 32.0;
/// `.rename-input { min-width: 260px }`.
pub const RENAME_MIN_W: f32 = 260.0;

fn head_height(is_renaming: bool) -> f32 {
    (if is_renaming { RENAME_H } else { HEAD_H }) + pages::VIEW_HEAD_MARGIN_BOTTOM
}

/// Build the playlist header kebab entries (Rename / Delete).
pub fn header_menu_entries(id: &str, name: &str) -> Vec<menu::Entry> {
    vec![
        menu::Entry::item(
            menu::Item::new("Rename", Some(Icon::Edit)),
            Action::BeginRenamePlaylist {
                id: id.to_string(),
                name: name.to_string(),
            },
        ),
        menu::Entry::item(
            menu::Item::new("Delete", Some(Icon::Delete)).danger(),
            Action::DeletePlaylist(id.to_string()),
        ),
    ]
}

/// Content height (no main padding) for [`view`].
pub fn height(_ui: &mut Ui, state: &State, _width: f32, id: &str) -> f32 {
    let Some(p) = state.playlist(id) else {
        return pages::EMPTY_H;
    };
    let renaming = state.renaming.as_ref().is_some_and(|(rid, _)| rid == id);
    let head = head_height(renaming);
    if p.tracks.is_empty() {
        head + pages::EMPTY_H
    } else {
        head + pages::COLLECTION_HEAD_H + track_list::list_height(p.tracks.len())
    }
}

/// Draw the playlist view at `area`.
pub fn view(ui: &mut Ui, state: &mut State, actions: &mut Vec<Action>, area: Rect, id: &str) {
    let Some(index) = state
        .library
        .playlists
        .iter()
        .position(|playlist| playlist.id == id)
    else {
        pages::empty(ui, area.x, area.y, area.w, "Playlist not found.");
        return;
    };
    let name = state.library.playlists[index].name.clone();
    let is_renaming = state.renaming.as_ref().is_some_and(|(rid, _)| rid == id);

    let mut y = if is_renaming {
        // Take the draft out so the text input can edit it while `state` stays
        // available to the rest of the view; put it back before returning (the
        // dispatched action closes it when committed/cancelled).
        let mut editing = state.renaming.take();
        let mut commit = false;
        let mut cancel = false;
        if let Some((_, draft)) = editing.as_mut() {
            let theme = ui.theme().clone();
            let style = TextInputStyle::rename(&theme);
            let w = area.w.clamp(RENAME_MIN_W, 520.0);
            let rect = Rect::new(area.x, area.y, w, RENAME_H);
            let resp = ui.text_input(
                rect,
                Id::from_str("playlist/rename"),
                draft,
                "Playlist name",
                &style,
            );
            if resp.submitted {
                let name = draft.trim().to_string();
                if !name.is_empty() {
                    actions.push(Action::RenamePlaylist {
                        id: id.to_string(),
                        name,
                    });
                }
                commit = true;
            } else if resp.cancelled || resp.lost_focus {
                cancel = true;
            } else if resp.response.hovered && ui.input().mouse_left_pressed {
                ui.request_focus(Id::from_str("playlist/rename"));
            }
        }
        if commit || cancel {
            actions.push(Action::ClosePlaylistEditor);
            state.renaming = None;
        } else {
            state.renaming = editing;
        }
        area.y + head_height(true)
    } else {
        // h2 + kebab (28x28 `.ibtn`, gap 10), vertically centred in the row.
        let theme = ui.theme().clone();
        let h2_y = area.y + (HEAD_H - pages::H2_LINE) * 0.5;
        ui.text(
            Rect::new(area.x, h2_y, area.w - 40.0, pages::H2_LINE),
            &name,
            &crate::screens::cards::style_lh(17.0, Weight::Bold, theme.text, pages::H2_LINE),
        );
        let title_w = ui.text_width(
            &name,
            &crate::screens::cards::style(17.0, Weight::Bold, theme.text),
        );
        let kebab_rect = Rect::new(
            (area.x + title_w + 10.0).min(area.right() - 28.0),
            area.y + (HEAD_H - 28.0) * 0.5,
            28.0,
            28.0,
        );
        let kebab = menu::kebab(ui, kebab_rect, Id::from_str("playlist/header-kebab"));
        if kebab.clicked {
            let entries = header_menu_entries(id, &name);
            menu::open(
                ui,
                Id::from_str("playlist/header-menu").with(id),
                kebab_rect,
                entries,
            );
        }
        area.y + head_height(false)
    };

    // Move the track list out of `State` for the frame: the header and rows need
    // `&mut State` (cover requests), so borrowing it from `state.library` would
    // conflict, and cloning every track per frame would allocate on the hot path.
    // It goes back before returning (also on the empty early return below).
    let tracks = std::mem::take(&mut state.library.playlists[index].tracks);
    if tracks.is_empty() {
        state.library.playlists[index].tracks = tracks;
        pages::empty(
            ui,
            area.x,
            y,
            area.w,
            "Empty playlist. Use ♪+ on any track to add it.",
        );
        return;
    }
    let source = format!("playlist:{id}");
    let sources = Some(source.as_str());
    y = pages::collection_head(
        ui,
        state,
        actions,
        pages::CollectionHead {
            x: area.x,
            y,
            w: area.w,
            tracks: &tracks,
            source: sources,
            id_key: &source,
        },
    );
    let opts = track_list::TrackListOpts {
        source: sources,
        playlist_id: Some(id),
        id: "playlist-list",
        ..track_list::TrackListOpts::default()
    };
    track_list::track_list(
        ui,
        state,
        actions,
        Rect::new(area.x, y, area.w, track_list::list_height(tracks.len())),
        &tracks,
        &opts,
    );
    state.library.playlists[index].tracks = tracks;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::Ui;
    use rift_types::{Library, Playlist};

    fn state_with(tracks: usize) -> State {
        let t = |n: usize| rift_types::Track {
            id: format!("t{n}"),
            title: format!("Song {n}"),
            artist: "Artist".into(),
            album: None,
            duration: Some(100),
            cover: String::new(),
            artists: Vec::new(),
            album_id: None,
        };
        State {
            library: Library {
                playlists: vec![Playlist {
                    id: "pl".into(),
                    name: "Mix".into(),
                    tracks: (0..tracks).map(t).collect(),
                }],
                ..Library::default()
            },
            ..State::default()
        }
    }

    use crate::screens::menu::harness::{menu_row_center, Sim};

    fn playlist_area() -> Rect {
        Rect::new(359.0, 66.0, 767.0, 600.0)
    }

    fn playlist_state() -> State {
        let mut state = State::default();
        state.library.playlists = vec![rift_types::Playlist {
            id: "pl".into(),
            name: "Mix".into(),
            tracks: Vec::new(),
        }];
        state
    }

    #[test]
    fn header_kebab_opens_rename_and_delete_menu() {
        for (row, expected_delete) in [(0usize, false), (1usize, true)] {
            let _guard = menu::lock_for_tests();
            let mut state = playlist_state();
            let area = playlist_area();
            let mut sim = Sim::new();
            let title_style = crate::screens::cards::style(17.0, Weight::Bold, sim.ui.theme().text);
            let title_w = sim.ui.text_width("Mix", &title_style);
            let kebab_rect = Rect::new(area.x + title_w + 10.0, area.y, 28.0, 28.0);
            let kebab = (kebab_rect.x + 14.0, kebab_rect.y + 14.0);
            let mut draw = |ui: &mut Ui, actions: &mut Vec<Action>| {
                view(ui, &mut state, actions, area, "pl");
                menu::layer(ui, actions);
            };
            let _ = sim.click(kebab, &mut draw);
            assert!(menu::is_open(), "header kebab must open the menu");
            let entries = header_menu_entries("pl", "Mix");
            let item = menu_row_center(&mut sim.ui, &entries, kebab_rect, None, row);
            let actions = sim.click(item, &mut draw);
            if expected_delete {
                assert!(
                    actions
                        .iter()
                        .any(|a| matches!(a, Action::DeletePlaylist(id) if id == "pl")),
                    "Delete: {actions:?}"
                );
            } else {
                assert!(
                    actions.iter().any(|a| matches!(
                        a,
                        Action::BeginRenamePlaylist { id, name } if id == "pl" && name == "Mix"
                    )),
                    "Rename: {actions:?}"
                );
            }
        }
    }

    #[test]
    fn rename_input_enter_submits_and_escape_cancels() {
        // Enter -> RenamePlaylist + ClosePlaylistEditor.
        {
            let mut state = playlist_state();
            state.renaming = Some(("pl".into(), "Mix".into()));
            let area = playlist_area();
            let mut sim = Sim::new();
            let input_rect = Rect::new(
                area.x,
                area.y,
                520.0f32.min(area.w).max(RENAME_MIN_W),
                RENAME_H,
            );
            let input = (
                input_rect.x + input_rect.w * 0.5,
                input_rect.y + RENAME_H * 0.5,
            );
            let mut draw = |ui: &mut Ui, actions: &mut Vec<Action>| {
                view(ui, &mut state, actions, area, "pl");
            };
            let _ = sim.click(input, &mut draw); // focus the field
            let _ = sim.type_text("New ", &mut draw);
            let actions = sim.key(crate::ui::Key::Enter, input, &mut draw);
            assert!(
                actions
                    .iter()
                    .any(|a| matches!(a, Action::RenamePlaylist { id, .. } if id == "pl")),
                "Enter must submit: {actions:?}"
            );
            assert!(
                actions
                    .iter()
                    .any(|a| matches!(a, Action::ClosePlaylistEditor)),
                "Enter must close the editor: {actions:?}"
            );
        }
        // Escape -> ClosePlaylistEditor only.
        {
            let mut state = playlist_state();
            state.renaming = Some(("pl".into(), "Mix".into()));
            let area = playlist_area();
            let mut sim = Sim::new();
            let input_rect = Rect::new(
                area.x,
                area.y,
                520.0f32.min(area.w).max(RENAME_MIN_W),
                RENAME_H,
            );
            let input = (
                input_rect.x + input_rect.w * 0.5,
                input_rect.y + RENAME_H * 0.5,
            );
            let mut draw = |ui: &mut Ui, actions: &mut Vec<Action>| {
                view(ui, &mut state, actions, area, "pl");
            };
            let _ = sim.click(input, &mut draw);
            let actions = sim.key(crate::ui::Key::Escape, input, &mut draw);
            assert!(
                actions
                    .iter()
                    .any(|a| matches!(a, Action::ClosePlaylistEditor)),
                "Escape must close the editor: {actions:?}"
            );
            assert!(
                !actions
                    .iter()
                    .any(|a| matches!(a, Action::RenamePlaylist { .. })),
                "Escape must not rename: {actions:?}"
            );
        }
    }
    #[test]
    fn missing_playlist_is_just_the_empty_state() {
        let mut ui = Ui::new();
        let state = State::default();
        assert_eq!(height(&mut ui, &state, 766.0, "nope"), pages::EMPTY_H);
    }

    #[test]
    fn playlist_height_adds_rows() {
        let mut ui = Ui::new();
        let state = state_with(4);
        let h = height(&mut ui, &state, 766.0, "pl");
        // head (28+4) + meta 17 + actions 69 + 4 rows.
        assert_eq!(h, 32.0 + 17.0 + 69.0 + 4.0 * 54.0);
    }

    #[test]
    fn header_menu_is_rename_then_delete() {
        let entries = header_menu_entries("pl", "Mix");
        assert_eq!(entries.len(), 2);
        assert!(matches!(
            &entries[0],
            menu::Entry::Item { item, action } if item.label == "Rename"
                && matches!(action, Some(Action::BeginRenamePlaylist { .. }))
        ));
        assert!(matches!(
            &entries[1],
            menu::Entry::Item { item, action } if item.label == "Delete"
                && item.danger
                && matches!(action, Some(Action::DeletePlaylist(id)) if id == "pl")
        ));
    }
}
