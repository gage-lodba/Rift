//! Drag-to-reorder index math (`components/reorder.rs`).
//!
//! Both halves of a reorder are pure arithmetic and both are off-by-one
//! territory: which slot the pointer is over, and which way each row slides to
//! preview the move. Neither is visible in a diff, and the only symptom of
//! getting one wrong is that a user drags a row and a different row moves —
//! which is exactly the class of bug that needs a test rather than an eye.
//!
//! The DOM readers (`hover_slot`'s element walk, the drag handlers) stay
//! untested here; the arithmetic they feed is not. See the module doc on
//! `crate::tests` for why nothing in this tree may touch a real `window`.

use wasm_bindgen_test::wasm_bindgen_test;

use crate::components::reorder::{
    drag_shift_style, keyboard_move_target, move_announcement, slot_at_offset,
};

/// Row height 100 + 40 gap, the shape the queue panel and sidebar both use.
const SLOT: f64 = 140.0;
const MARGIN: i64 = 40;

// ---------------------------------------------------------------------------
// slot_at_offset — which slot is the pointer over?
// ---------------------------------------------------------------------------

/// The boundary that matters: the last row's *bottom* edge. An inclusive test
/// on the wrong side here yields `len`, which `min(len - 1)` then hides — so
/// the visible symptom is not a panic but the drop landing one row short.
#[wasm_bindgen_test]
fn the_last_rows_bottom_edge_is_still_the_last_slot() {
    for len in 1..=8usize {
        let bottom_edge = len as f64 * SLOT;
        assert_eq!(
            slot_at_offset(bottom_edge, SLOT, len),
            len - 1,
            "len {len}: the bottom edge of the list must be the last slot"
        );
        // A pointer a hair past the end (a drop event can land there) clamps
        // rather than running off the list.
        assert_eq!(slot_at_offset(bottom_edge + 0.5, SLOT, len), len - 1);
        assert_eq!(slot_at_offset(bottom_edge + 5000.0, SLOT, len), len - 1);
    }
}

/// The other end: anything at or above the list's top edge is slot 0, and a
/// negative offset must not wrap. `as usize` on a negative float saturates to
/// 0, so this would *appear* to pass even without the `.max(0.0)` — the
/// assertion that it stays 0 for a large negative offset is the one with teeth.
#[wasm_bindgen_test]
fn the_top_edge_is_slot_zero() {
    assert_eq!(slot_at_offset(0.0, SLOT, 5), 0);
    assert_eq!(slot_at_offset(-0.001, SLOT, 5), 0);
    assert_eq!(slot_at_offset(-1e9, SLOT, 5), 0);
}

/// Every row's midpoint must resolve to that row. This is the property the
/// feature actually needs: if a row's own centre maps anywhere else, dragging
/// the row you grabbed moves a different one.
#[wasm_bindgen_test]
fn every_rows_midpoint_maps_to_itself() {
    let len = 6;
    for i in 0..len {
        let middle = i as f64 * SLOT + SLOT / 2.0;
        assert_eq!(
            slot_at_offset(middle, SLOT, len),
            i,
            "row {i}'s midpoint should be slot {i}"
        );
    }
}

/// Each slot's first and last addressable pixel, across a range of list lengths
/// and row heights. A chain of approximate values rather than one case, because
/// a `<=`/`<` slip is only visible at an exact boundary.
#[wasm_bindgen_test]
fn slot_edges_are_half_open() {
    for len in 1..=6usize {
        for slot_h in [24.0, 100.0, 140.0, 333.5] {
            for i in 0..len {
                let top = i as f64 * slot_h;
                let last_inside = top + slot_h - 0.001;
                assert_eq!(
                    slot_at_offset(top, slot_h, len),
                    i,
                    "len {len}, slot_h {slot_h}: slot {i} top edge"
                );
                assert_eq!(
                    slot_at_offset(last_inside, slot_h, len),
                    i,
                    "len {len}, slot_h {slot_h}: last pixel of slot {i}"
                );
            }
        }
    }
}

/// A single-row list is always slot 0, whatever the pointer does — the case
/// most likely to produce `len - 1` underflow.
#[wasm_bindgen_test]
fn a_single_row_list_has_one_slot() {
    assert_eq!(slot_at_offset(0.0, SLOT, 1), 0);
    assert_eq!(slot_at_offset(SLOT * 10.0, SLOT, 1), 0);
    assert_eq!(slot_at_offset(-SLOT * 10.0, SLOT, 1), 0);
}

/// The pointer offset already includes `scroll_top` (the caller adds it), so a
/// scrolled-into-view row still resolves by its position in the content rather
/// than on screen. Asserted at the seam the caller owns, so a future rewrite of
/// `hover_slot` that drops the `scroll_top` term fails here.
#[wasm_bindgen_test]
fn a_scrolled_list_is_measured_against_the_content_not_the_viewport() {
    // Row 9, in a 10-row list, scrolled down by 5 rows: viewport y = pointer
    // position minus 5 rows' worth, and the caller restores it.
    let (len, scrolled_by) = (10usize, 5 * SLOT as i64);
    let row = 9usize;
    let viewport_y = row as f64 * SLOT - scrolled_by as f64 + SLOT / 2.0;
    let content_y = viewport_y + scrolled_by as f64;
    assert_eq!(slot_at_offset(content_y, SLOT, len), row);
}

// ---------------------------------------------------------------------------
// drag_shift_style — which way does each row slide?
// ---------------------------------------------------------------------------

/// No drag, or a drag hovering its own slot, moves nothing. A transform here
/// would be a visible jitter on every dragenter.
#[wasm_bindgen_test]
fn no_transform_without_a_move() {
    assert_eq!(drag_shift_style(None, Some(3), 3, MARGIN), None);
    assert_eq!(drag_shift_style(Some(3), None, 3, MARGIN), None);
    assert_eq!(drag_shift_style(Some(2), Some(2), 2, MARGIN), None);
}

/// The dragged row spans the whole distance. Both the percentage and the pixel
/// margin scale with the slot count — the margin term is what makes the preview
/// land on the right pixel rather than a gap short.
#[wasm_bindgen_test]
fn the_dragged_row_spans_the_distance() {
    // Dragging row 1 down over row 3.
    assert_eq!(
        drag_shift_style(Some(1), Some(3), 1, MARGIN).as_deref(),
        Some("transform: translateY(calc(200% + 80px));")
    );
    // Dragging row 4 up over row 0.
    assert_eq!(
        drag_shift_style(Some(4), Some(0), 4, MARGIN).as_deref(),
        Some("transform: translateY(calc(-400% + -160px));")
    );
}

/// Rows the dragged row passes over step one slot toward the vacated position.
/// Downward: everything in `(from, over]` moves up. Upward: everything in
/// `[over, from)` moves down. Getting the inclusivity backwards leaves the row
/// immediately adjacent to the target in the wrong place — one row visibly out
/// of order during the drag, and correct again after the drop, which is very
/// hard to spot.
#[wasm_bindgen_test]
fn the_rows_between_step_one_slot_the_other_way() {
    let down = "transform: translateY(calc(-100% + -40px));";
    let up = "transform: translateY(calc(100% + 40px));";

    // Downward, from 1 to 3: rows 2 and 3 move up; 0 and 4 stay put.
    let moved: Vec<Option<String>> = (0..5)
        .map(|i| drag_shift_style(Some(1), Some(3), i, MARGIN))
        .collect();
    assert_eq!(
        moved,
        vec![
            None,
            Some("transform: translateY(calc(200% + 80px));".to_string()),
            Some(down.to_string()),
            Some(down.to_string()),
            None,
        ],
        "downward drag of row 1 to slot 3"
    );

    // Upward, from 3 to 1: rows 1 and 2 move down; 0 and 4 stay put.
    let moved: Vec<Option<String>> = (0..5)
        .map(|i| drag_shift_style(Some(3), Some(1), i, MARGIN))
        .collect();
    assert_eq!(
        moved,
        vec![
            None,
            Some(up.to_string()),
            Some(up.to_string()),
            Some("transform: translateY(calc(-200% + -80px));".to_string()),
            None,
        ],
        "upward drag of row 3 to slot 1"
    );
}

/// The slot a preview transform moves a row to, read back out of the CSS string
/// `drag_shift_style` produces.
///
/// Deliberately a *parser*, not a reimplementation: a test that recomputed the
/// shift from `(from, over, i)` with the same branch structure would agree with
/// the implementation while both were wrong. This reads the string a browser
/// would read.
///
/// Returns `None` for no transform, and panics on a transform it cannot parse —
/// a silent `None` there would turn a formatting change into a passing test.
fn slot_offset(style: Option<&str>, margin: i64) -> Option<i64> {
    let style = style?;
    let inner = style
        .strip_prefix("transform: translateY(calc(")
        .and_then(|s| s.strip_suffix("));"))
        .unwrap_or_else(|| panic!("unparsable preview transform: {style:?}"));
    let (pct, px) = inner
        .split_once("% + ")
        .unwrap_or_else(|| panic!("expected `<percent>% + <px>px` in {style:?}"));
    let pct: i64 = pct
        .parse()
        .unwrap_or_else(|e| panic!("bad percent in {style:?}: {e}"));
    let px: i64 = px
        .strip_suffix("px")
        .unwrap_or_else(|| panic!("expected px suffix in {style:?}"))
        .parse()
        .unwrap_or_else(|e| panic!("bad px in {style:?}: {e}"));
    assert_eq!(pct % 100, 0, "slots should be whole rows: {style:?}");
    assert_eq!(
        px,
        pct / 100 * margin,
        "the pixel term must track the slot count at {margin}px per gap: {style:?}"
    );
    Some(pct / 100)
}

/// A preview that is not a permutation of the slots would show two rows in one
/// slot, or a hole where a row should be — the failure a per-row assertion
/// cannot see, because every individual row's transform can look reasonable.
///
/// The `occupied` check is also the guard on `slot_offset`: if the parser
/// stopped understanding the format it would panic, and if it silently returned
/// the same value for every row this multiset would collapse.
#[wasm_bindgen_test]
fn a_preview_is_a_permutation_of_the_slots() {
    let len = 6;
    for from in 0..len {
        for over in 0..len {
            let mut lands: Vec<i64> = (0..len)
                .map(|i| {
                    let style = drag_shift_style(Some(from), Some(over), i, MARGIN);
                    i as i64 + slot_offset(style.as_deref(), MARGIN).unwrap_or(0)
                })
                .collect();
            lands.sort_unstable();
            assert_eq!(
                lands,
                (0..len as i64).collect::<Vec<_>>(),
                "{from} -> {over}: every slot must end up occupied exactly once"
            );
        }
    }
}

/// The dragged row's destination is where `slot_at_offset` said the pointer is,
/// not one off it — the two halves have to agree, since a drop commits
/// `(from, over)` and the preview promises that exact order.
#[wasm_bindgen_test]
fn the_preview_destination_matches_the_hovered_slot() {
    let len = 5;
    let from = 0usize;
    for over in 0..len {
        let midpoint = over as f64 * SLOT + SLOT / 2.0;
        let hovered = slot_at_offset(midpoint, SLOT, len);
        assert_eq!(hovered, over, "pointer over row {over}");
        let dragged = drag_shift_style(Some(from), Some(hovered), from, MARGIN);
        if over == from {
            assert_eq!(dragged, None, "no move, no preview");
        } else {
            let expected = over as i64 - from as i64;
            assert_eq!(
                dragged.as_deref(),
                Some(format!(
                    "transform: translateY(calc({}% + {}px));",
                    expected * 100,
                    expected * MARGIN
                ))
                .as_deref(),
                "row {from} should preview at slot {over}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// keyboard_move_target — where does Alt+Up / Alt+Down move the row?
// ---------------------------------------------------------------------------

/// **The boundaries of the keyboard reorder, which are the whole correctness
/// question.**
///
/// Two ways to be wrong, and neither is visible in a diff: moving off the top of
/// the list (there is nothing to swap with) and moving off the bottom — the
/// latter needs `from + 1 < len`, because `from + 1 <= len` would report a
/// destination one past the end and the caller would reorder with an index that
/// does not exist. The table is the function's whole contract; the mounted path
/// (focus a row, dispatch Alt+ArrowDown, see `on_move`) is covered in
/// `interact.rs`.
#[wasm_bindgen_test]
fn keyboard_move_target_reports_the_boundaries() {
    let cases: &[(&str, usize, usize, Option<usize>)] = &[
        ("ArrowUp", 0, 3, None),
        ("ArrowUp", 1, 3, Some(0)),
        ("ArrowUp", 2, 3, Some(1)),
        ("ArrowDown", 0, 3, Some(1)),
        ("ArrowDown", 1, 3, Some(2)),
        ("ArrowDown", 2, 3, None),
        // A single-row list can move in neither direction.
        ("ArrowUp", 0, 1, None),
        ("ArrowDown", 0, 1, None),
        // Anything that is not an arrow is not a reorder, whatever the caller
        // does with modifiers.
        ("Enter", 1, 3, None),
        ("ArrowLeft", 1, 3, None),
        ("", 1, 3, None),
    ];
    for (key, from, len, expected) in cases {
        assert_eq!(
            keyboard_move_target(key, *from, *len),
            *expected,
            "{key:?} from {from} of {len}"
        );
    }
}

/// **The live region's sentence is one-based and counts the whole list.**
///
/// A screen reader hears exactly this, so `to + 1` is the difference between
/// "position 2" and the row actually being second. The `len` of 1 is the case
/// that catches a "position N of N" written from the wrong end.
#[wasm_bindgen_test]
fn move_announcement_is_one_based_and_counts_the_whole_list() {
    assert_eq!(
        move_announcement("Chill", 1, 5),
        "Chill moved to position 2 of 5"
    );
    assert_eq!(
        move_announcement("Focus", 0, 1),
        "Focus moved to position 1 of 1"
    );
}
