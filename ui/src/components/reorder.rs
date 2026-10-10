//! Shared drag- and keyboard-reorder state for vertical lists (queue panel +
//! sidebar playlists). See [`Reorder`] for the wiring contract.

use std::cell::RefCell;
use std::rc::Rc;

use wasm_bindgen::JsCast;
use web_sys::DragEvent;
use yew::prelude::*;

/// 1×1 transparent GIF used to blank out the browser's drag ghost.
const TRANSPARENT_GIF: &str =
    "data:image/gif;base64,R0lGODlhAQABAIAAAAAAAP///yH5BAEAAAAALAAAAAABAAEAAAIBRAA7";

thread_local! {
    /// The transparent ghost image, created once and reused. A freshly-made
    /// image may not be decoded when the browser captures the drag ghost, which
    /// lets the default (doubled) ghost flash through; caching means every drag
    /// after the first reuses an already-loaded image. (wasm is single-threaded,
    /// so a thread-local is effectively a global here.)
    static DRAG_GHOST: Option<web_sys::HtmlImageElement> = {
        match web_sys::HtmlImageElement::new() {
            Ok(img) => {
                img.set_src(TRANSPARENT_GIF);
                Some(img)
            }
            Err(_) => None,
        }
    };
}

/// Mark a reorder drag as a "move" so the cursor doesn't show a copy badge.
/// Some engines also refuse to start a drag without payload data.
fn mark_move_drag(e: &DragEvent) {
    if let Some(dt) = e.data_transfer() {
        dt.set_effect_allowed("move");
        let _ = dt.set_data("text/plain", "");
        // Suppress the browser's semi-transparent ghost snapshot. The list
        // itself previews the move (the solid row slides between slots), and
        // the ghost floating on top of that doubles the row and reads as
        // flicker.
        DRAG_GHOST.with(|img| {
            if let Some(img) = img {
                dt.set_drag_image(img, 0, 0);
            }
        });
    }
}

/// Accept a drop on this element, keeping the "move" cursor while hovering.
fn allow_move_drop(e: &DragEvent) {
    e.prevent_default();
    if let Some(dt) = e.data_transfer() {
        dt.set_drop_effect("move");
    }
}

/// Which slot a vertical offset inside the list's content falls in.
///
/// Split out of [`hover_slot`] so the arithmetic — the part where an off-by-one
/// is invisible in a diff and only shows up as the wrong row moving — can be
/// tested without a DOM. `y` is the pointer's offset from the top of the list's
/// *content*, i.e. already including `scroll_top`.
///
/// `slot_h` must be positive; the caller establishes that before calling.
pub(crate) fn slot_at_offset(y: f64, slot_h: f64, len: usize) -> usize {
    debug_assert!(len > 0, "slot_at_offset needs a non-empty list");
    // `max(0.0)` so "above the list is slot 0" is stated rather than inherited:
    // `as usize` on a negative float happens to saturate to 0, so dropping the
    // clamp would leave the behaviour identical and the intent implicit.
    let slot = (y / slot_h).max(0.0) as usize;
    // `min` rather than leaving the slot to the caller: a drop at or past the
    // last row's bottom edge computes `len`, and an index one past the end is
    // how a drag silently moves the wrong track.
    slot.min(len - 1)
}

/// Which slot a reorder drag is hovering, from the pointer's Y offset inside
/// the list element (uniform slot height, DOM order). Geometry-based because
/// the live-preview transforms slide rows around under the pointer — hit-tests
/// against the row elements themselves would oscillate, and the drop would
/// usually land on the dragged row (which follows the pointer's slot).
fn hover_slot(list: &NodeRef, e: &DragEvent, len: usize, margin: f64) -> Option<usize> {
    if len == 0 {
        return None;
    }
    let el = list.cast::<web_sys::HtmlElement>()?;
    let first = el
        .first_element_child()?
        .dyn_into::<web_sys::HtmlElement>()
        .ok()?;
    let slot_h = f64::from(first.offset_height()) + margin;
    if slot_h <= 0.0 {
        return None;
    }
    let y =
        f64::from(e.client_y()) - el.get_bounding_client_rect().top() + f64::from(el.scroll_top());
    Some(slot_at_offset(y, slot_h, len))
}

/// Live reorder preview: inline transform for row `i` while row `from` is
/// dragged over row `over`. The dragged row follows the prospective drop slot
/// and the rows in between slide one slot the other way, so the list shows the
/// exact post-drop order. `margin` is the vertical gap between rows in px
/// (translateY percentages only cover the row's own height).
pub(crate) fn drag_shift_style(
    from: Option<usize>,
    over: Option<usize>,
    i: usize,
    margin: i64,
) -> Option<String> {
    let (from, over) = (from?, over?);
    if from == over {
        return None;
    }
    // How many slots this row moves: the dragged row spans the whole distance;
    // rows between the two positions step one slot toward the vacated one.
    let slots: i64 = if i == from {
        over as i64 - from as i64
    } else if from < over && i > from && i <= over {
        -1
    } else if from > over && i >= over && i < from {
        1
    } else {
        return None;
    };
    Some(format!(
        "transform: translateY(calc({}% + {}px));",
        slots * 100,
        slots * margin
    ))
}

/// Where a keyboard reorder (Alt+Up/Down) moves the row at `from`, if anywhere.
///
/// Split out of the handler for the same reason [`slot_at_offset`] is: the
/// boundary is the whole correctness question. An index one past the end is how
/// a reorder silently moves the wrong row, and a move off the top has nothing to
/// swap with. `None` is "no move", which the caller must treat as "nothing
/// happened" rather than as a slot that merely equals `from`.
pub(crate) fn keyboard_move_target(key: &str, from: usize, len: usize) -> Option<usize> {
    match key {
        "ArrowUp" => from.checked_sub(1),
        "ArrowDown" => (from + 1 < len).then_some(from + 1),
        _ => None,
    }
}

/// The `data-index` of the row the user's focus is inside, if any.
///
/// Read from the DOM at key time rather than captured when the row was
/// rendered: an unkeyed list is patched in place, so a row's index changes under
/// it after a move and a stale capture would move whichever row took its place.
/// `closest` so the shortcut still works while one of the row's own buttons has
/// focus; the open menu is excluded explicitly, because it renders *inside* the
/// row's subtree and Alt+Up/Down there would move the row out from under the
/// menu the user is reading.
fn focused_row_index() -> Option<usize> {
    let active = web_sys::window()?.document()?.active_element()?;
    if active.closest(".menu").ok().flatten().is_some() {
        return None;
    }
    let row = if active.has_attribute("data-index") {
        active
    } else {
        active.closest("[data-index]").ok().flatten()?
    };
    row.get_attribute("data-index")?.parse().ok()
}

/// Put focus back on the row at `index` in this list, if it is still there.
fn focus_row(list: &NodeRef, index: usize) {
    let Some(root) = list.cast::<web_sys::HtmlElement>() else {
        return;
    };
    let Ok(Some(row)) = root.query_selector(&format!("[data-index=\"{index}\"]")) else {
        return;
    };
    if let Ok(row) = row.dyn_into::<web_sys::HtmlElement>() {
        let _ = row.focus();
    }
}

/// The live-region text for a keyboard reorder: the row's label and its new
/// position, one-based and counted against the whole list.
///
/// Split out like [`keyboard_move_target`] so the wording and the arithmetic are
/// decidable without a DOM: a screen reader hears this sentence, so an
/// off-by-one here is the difference between "position 2" and the row actually
/// being second.
pub(crate) fn move_announcement(label: &str, to: usize, len: usize) -> String {
    format!("{label} moved to position {} of {len}", to + 1)
}

/// Shared drag-to-reorder state for a vertical list. Both the queue panel and
/// the sidebar playlist list drive their reordering through this: the two were
/// line-for-line identical — the drag source/over slots, the committed-order
/// hold that stops a post-drop snap-back, the hover-calm mute, and the
/// container dragover/drop handlers — so the logic lives here once. Build it
/// with [`use_reorder`]; wire the container fields onto the list element and
/// the per-row methods onto each row.
pub(crate) struct Reorder {
    /// Attach to the list container element.
    pub(crate) list_ref: NodeRef,
    /// A drag is in progress (add the `reordering` class to the container).
    pub(crate) reordering: bool,
    /// Mute :hover until the next mousemove (add the `hover-calm` class).
    pub(crate) hover_calm: bool,
    /// Container `ondragenter`/`ondragover` handler.
    pub(crate) dragover: Callback<DragEvent>,
    /// Container `ondrop` handler.
    pub(crate) drop: Callback<DragEvent>,
    /// Container `onmousemove` handler (clears hover-calm).
    pub(crate) calm_clear: Callback<MouseEvent>,
    /// Container `onkeydown` handler: Alt+Up/Down reorders the focused row.
    pub(crate) keyboard: Callback<KeyboardEvent>,
    /// The last keyboard move as live-region text, e.g. "Chill moved to position
    /// 2 of 5"; `None` until one lands. Render it in a `role="status"` element
    /// (the lists use `.sr-only`), which is what makes a keyboard reorder
    /// audible rather than a silent reshuffle.
    pub(crate) announcement: Option<String>,
    // Effective source/target slots and the per-row state used to build each
    // row's handlers and live-preview transform.
    eff_src: Option<usize>,
    eff_over: Option<usize>,
    margin: i64,
    drag_src: UseStateHandle<Option<usize>>,
    drag_over: UseStateHandle<Option<usize>>,
    committed: Rc<RefCell<Option<Vec<String>>>>,
    hover_calm_state: UseStateHandle<bool>,
    /// Synchronous "our drag is live" flag. `drag_src` lags a render behind
    /// `set`, so the first dragenter after dragstart would still read `None` and
    /// skip `prevent_default`, flashing the no-drop cursor for a frame; this ref
    /// flips the instant the drag starts so the container accepts it right away.
    active: Rc<RefCell<bool>>,
}

impl Reorder {
    /// This row is the one being dragged.
    pub(crate) fn dragging(&self, i: usize) -> bool {
        self.eff_src == Some(i)
    }
    /// Inline transform sliding row `i` for the live reorder preview.
    pub(crate) fn shift(&self, i: usize) -> Option<String> {
        drag_shift_style(self.eff_src, self.eff_over, i, self.margin)
    }
    /// `ondragstart` for the row at index `i`.
    pub(crate) fn dragstart(&self, i: usize) -> Callback<DragEvent> {
        let drag_src = self.drag_src.clone();
        let committed = self.committed.clone();
        let hover_calm = self.hover_calm_state.clone();
        let active = self.active.clone();
        Callback::from(move |e: DragEvent| {
            mark_move_drag(&e);
            *committed.borrow_mut() = None;
            *active.borrow_mut() = true;
            hover_calm.set(true);
            drag_src.set(Some(i));
        })
    }
    /// `ondragend` for any row (clears the drag unless a drop is pending).
    pub(crate) fn dragend(&self) -> Callback<DragEvent> {
        let drag_src = self.drag_src.clone();
        let drag_over = self.drag_over.clone();
        let committed = self.committed.clone();
        let active = self.active.clone();
        Callback::from(move |_: DragEvent| {
            // The drag is over regardless of whether it committed.
            *active.borrow_mut() = false;
            // dragend fires right after a committed drop; keep the preview until
            // the new order lands.
            if committed.borrow().is_some() {
                return;
            }
            drag_src.set(None);
            drag_over.set(None);
        })
    }
}

/// Custom hook backing [`Reorder`]. `rows` is the list's current order as
/// `(stable id, label)`: the ids detect when a committed reorder has landed, and
/// the labels are what a keyboard move names in its announcement. `margin` is
/// the vertical gap between rows in px, used both to locate the hovered slot and
/// to size the preview transforms. `on_move` is emitted with `(from, to)` on a
/// committed drop and on a keyboard move.
#[hook]
pub(crate) fn use_reorder(
    rows: Vec<(String, String)>,
    margin: i64,
    on_move: Callback<(usize, usize)>,
) -> Reorder {
    let ids: Vec<String> = rows.iter().map(|(id, _)| id.clone()).collect();
    let drag_src = use_state(|| None::<usize>);
    let drag_over = use_state(|| None::<usize>);
    let list_ref = use_node_ref();
    // Order (by id) at the moment a reorder was dropped; held until the backend
    // echoes the new order so the preview transforms don't snap back for a few
    // frames during the round trip.
    let committed = use_mut_ref(|| None::<Vec<String>>);
    // Mute :hover from dragstart until the first post-drop mousemove: the
    // browser doesn't re-evaluate hover until the mouse moves, so it stays
    // stuck on the grab position after a drop.
    let hover_calm = use_state(|| false);
    // Synchronous "our drag is live" flag; see the field doc on [`Reorder`].
    let active = use_mut_ref(|| false);
    // Where focus has to go once a keyboard reorder lands, held with the order
    // the move was computed against. `None` = no keyboard move in flight.
    // Declared here with the other hooks rather than beside the handler that
    // fills it: a hook belongs at the top of the body, and the handlers below
    // return early on paths that must not skip it.
    let focus_after_move = use_mut_ref(|| None::<(Vec<String>, usize)>);
    // The last keyboard move, as text for a live region. Held in state rather
    // than recomputed from `focus_after_move`, because the intent is consumed by
    // the effect below: a region whose text vanished on the next render reads
    // unreliably (some screen readers have not got to it yet).
    let announcement = use_state(|| None::<String>);
    {
        // Hand focus to the slot the moved row landed in. An unkeyed list is
        // patched in place, so the focused element stays on the *old* index
        // holding a different row — pressing Alt+Down again would move that one,
        // swapping the same two rows forever. Runs once the order the move was
        // computed against has actually changed.
        //
        // The same moment is the only honest one for the announcement: it is
        // when the move is known to have landed rather than merely been asked
        // for, and the one-move-in-flight guard means there cannot be two
        // announcements racing.
        let focus_after_move = focus_after_move.clone();
        let list_ref = list_ref.clone();
        let announcement = announcement.clone();
        let rows = rows.clone();
        let ids = ids.clone();
        use_effect_with(ids.clone(), move |ids| {
            let landed = focus_after_move
                .borrow()
                .as_ref()
                .is_some_and(|(before, _)| before != ids);
            if landed {
                if let Some((_, to)) = focus_after_move.borrow_mut().take() {
                    focus_row(&list_ref, to);
                    if let Some((_, label)) = rows.get(to) {
                        announcement.set(Some(move_announcement(label, to, rows.len())));
                    }
                }
            }
            || ()
        });
    }

    // The committed reorder has landed: resolve the preview in the same render
    // that first shows the new order (visually identical to the preview, so the
    // handoff doesn't move a pixel).
    let landed = committed.borrow().as_ref().is_some_and(|old| *old != ids);
    if landed {
        *committed.borrow_mut() = None;
        *active.borrow_mut() = false;
        drag_src.set(None);
        drag_over.set(None);
    }
    let (eff_src, eff_over) = if landed {
        (None, None)
    } else {
        (*drag_src, *drag_over)
    };

    // Container-level dragover/drop working on pointer geometry (see
    // [`hover_slot`] for why row-level targets don't work here).
    let n = ids.len();
    let dragover = {
        let drag_over = drag_over.clone();
        let list_ref = list_ref.clone();
        let active = active.clone();
        Callback::from(move |e: DragEvent| {
            if !*active.borrow() {
                return; // not our drag (e.g. a file dragged over the window)
            }
            allow_move_drop(&e);
            if let Some(slot) = hover_slot(&list_ref, &e, n, margin as f64) {
                if *drag_over != Some(slot) {
                    drag_over.set(Some(slot));
                }
            }
        })
    };
    let drop = {
        let drag_src = drag_src.clone();
        let drag_over = drag_over.clone();
        let committed = committed.clone();
        let active = active.clone();
        let ids = ids.clone();
        // Cloned rather than moved: the keyboard handler below emits through the
        // same callback and has to own one too.
        let on_move = on_move.clone();
        Callback::from(move |e: DragEvent| {
            e.prevent_default();
            *active.borrow_mut() = false;
            if let (Some(from), Some(to)) = (*drag_src, *drag_over) {
                if from != to {
                    // Keep the preview up until the new order lands.
                    *committed.borrow_mut() = Some(ids.clone());
                    on_move.emit((from, to));
                    // Safety net: if the backend rejects the move (e.g. a stale
                    // index because the list changed mid-drag) no echo arrives
                    // and the preview would stay frozen. Force-clear after a
                    // beat — guarded by the committed order so it can't clobber
                    // a newer drag that reused the state in the meantime.
                    let drag_src = drag_src.clone();
                    let drag_over = drag_over.clone();
                    let committed = committed.clone();
                    let settled = ids.clone();
                    gloo_timers::callback::Timeout::new(700, move || {
                        if committed.borrow().as_deref() == Some(settled.as_slice()) {
                            *committed.borrow_mut() = None;
                            drag_src.set(None);
                            drag_over.set(None);
                        }
                    })
                    .forget();
                    return;
                }
            }
            drag_src.set(None);
            drag_over.set(None);
        })
    };
    let calm_clear = {
        let hover_calm = hover_calm.clone();
        Callback::from(move |_: MouseEvent| {
            if *hover_calm {
                hover_calm.set(false);
            }
        })
    };
    // Keyboard reorder: Alt+Up/Down moves the focused row one slot, through the
    // same `on_move` a drop emits — so the backend command, the committed-order
    // hold and the sidebar's id lookup are all the existing path, and the only
    // thing the keyboard adds is where the keystroke came from and where focus
    // has to end up.
    let keyboard = {
        let ids = ids.clone();
        let on_move = on_move.clone();
        let focus_after_move = focus_after_move.clone();
        Callback::from(move |e: KeyboardEvent| {
            if !e.alt_key() {
                return;
            }
            // One keyboard move at a time. The index comes from the DOM, which
            // only reflects the new order once the move has landed, so a second
            // keystroke before the echo would compute the same `(from, to)`
            // again — and two identical moves cancel out. This is also what
            // makes a held Alt+Down walk the row down the list instead of
            // fighting itself: each repeat waits for the previous echo.
            if focus_after_move.borrow().is_some() {
                return;
            }
            let Some(from) = focused_row_index() else {
                return;
            };
            let Some(to) = keyboard_move_target(&e.key(), from, ids.len()) else {
                return;
            };
            e.prevent_default();
            *focus_after_move.borrow_mut() = Some((ids.clone(), to));
            // The drop path's safety net, for the same reason: a rejected move
            // never echoes the new order, and a stale intent would otherwise
            // grab focus on some later, unrelated list change.
            let settled = ids.clone();
            let focus_after_move = focus_after_move.clone();
            gloo_timers::callback::Timeout::new(700, move || {
                let mut pending = focus_after_move.borrow_mut();
                if pending
                    .as_ref()
                    .is_some_and(|(before, _)| *before == settled)
                {
                    *pending = None;
                }
            })
            .forget();
            on_move.emit((from, to));
        })
    };

    Reorder {
        list_ref,
        reordering: eff_src.is_some(),
        hover_calm: *hover_calm,
        dragover,
        drop,
        calm_clear,
        keyboard,
        eff_src,
        eff_over,
        margin,
        drag_src,
        drag_over,
        committed,
        hover_calm_state: hover_calm,
        active,
        announcement: (*announcement).clone(),
    }
}
