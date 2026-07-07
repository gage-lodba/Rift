//! Shared drag-to-reorder state for vertical lists (queue panel + sidebar
//! playlists). See [`Reorder`] for the wiring contract.

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
    Some(((y / slot_h).floor().max(0.0) as usize).min(len - 1))
}

/// Live reorder preview: inline transform for row `i` while row `from` is
/// dragged over row `over`. The dragged row follows the prospective drop slot
/// and the rows in between slide one slot the other way, so the list shows the
/// exact post-drop order. `margin` is the vertical gap between rows in px
/// (translateY percentages only cover the row's own height).
fn drag_shift_style(
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

/// Custom hook backing [`Reorder`]. `ids` is the list's current order by stable
/// id (used to detect when a committed reorder has landed); `margin` is the
/// vertical gap between rows in px, used both to locate the hovered slot and to
/// size the preview transforms. `on_move` is emitted with `(from, to)` on a
/// committed drop.
#[hook]
pub(crate) fn use_reorder(
    ids: Vec<String>,
    margin: i64,
    on_move: Callback<(usize, usize)>,
) -> Reorder {
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

    Reorder {
        list_ref,
        reordering: eff_src.is_some(),
        hover_calm: *hover_calm,
        dragover,
        drop,
        calm_clear,
        eff_src,
        eff_over,
        margin,
        drag_src,
        drag_over,
        committed,
        hover_calm_state: hover_calm,
        active,
    }
}
