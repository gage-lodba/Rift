//! Liked Songs view (`View::Liked`).
//!
//! Spec: `ui-spec.md` §7.2.
//!
//! 1. `.view-head` with the h2 "Liked Songs";
//! 2. empty → `.empty` "No liked songs yet. Click the heart on any track.";
//! 3. else `.collection-meta` ("N songs • duration"), the Play/Download action
//!    row (queue source `"liked"`), then the shared track list.

use crate::screens::{pages, track_list};
use crate::state::{Action, State};
use crate::ui::{Rect, Ui};

/// Content height (no main padding) for [`view`].
pub fn height(_ui: &mut Ui, state: &State, _width: f32) -> f32 {
    let head = pages::H2_LINE + pages::VIEW_HEAD_MARGIN_BOTTOM;
    if state.library.liked.is_empty() {
        head + pages::EMPTY_H
    } else {
        head + pages::COLLECTION_HEAD_H + track_list::list_height(state.library.liked.len())
    }
}

/// Draw the Liked Songs view at `area`.
pub fn view(ui: &mut Ui, state: &mut State, actions: &mut Vec<Action>, area: Rect) {
    let mut y = pages::view_head(ui, area.x, area.y, area.w, "Liked Songs");
    if state.library.liked.is_empty() {
        pages::empty(
            ui,
            area.x,
            y,
            area.w,
            "No liked songs yet. Click the heart on any track.",
        );
        return;
    }
    let tracks = state.library.liked.clone();
    y = pages::collection_head(
        ui,
        state,
        actions,
        pages::CollectionHead {
            x: area.x,
            y,
            w: area.w,
            tracks: &tracks,
            source: Some("liked"),
            id_key: "liked",
        },
    );
    let opts = track_list::TrackListOpts {
        source: Some("liked"),
        id: "liked-list",
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::Ui;

    fn track() -> rift_types::Track {
        rift_types::Track {
            id: "t".into(),
            title: "Song".into(),
            artist: "Artist".into(),
            album: None,
            duration: Some(100),
            cover: String::new(),
            artists: Vec::new(),
            album_id: None,
        }
    }

    #[test]
    fn empty_liked_has_only_the_head_plus_empty_state() {
        let mut ui = Ui::new();
        let state = State::default();
        let h = height(&mut ui, &state, 766.0);
        assert_eq!(h, 22.0 + 4.0 + 113.0);
    }

    #[test]
    fn liked_height_adds_one_row_per_track() {
        let mut ui = Ui::new();
        let mut state = State::default();
        state.library.liked = vec![track(); 3];
        let h = height(&mut ui, &state, 766.0);
        assert_eq!(h, 26.0 + 17.0 + 69.0 + 3.0 * 54.0);
    }
}
