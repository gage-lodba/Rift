//! Collapsible "Up next" queue panel with rows reorderable by drag or by
//! Alt+Up/Down.

use rift_types::QueueSnapshot;
use yew::prelude::*;

use super::icons::{cover, icon};
use super::key_on_nested_control;
use super::reorder::use_reorder;

#[derive(Properties, PartialEq)]
pub struct QueuePanelProps {
    pub queue: QueueSnapshot,
    pub on_jump: Callback<usize>,
    pub on_remove: Callback<usize>,
    pub on_clear: Callback<()>,
    /// Reorder: move the track at the first index to the second.
    pub on_move: Callback<(usize, usize)>,
}

#[function_component(QueuePanel)]
pub fn queue_panel(props: &QueuePanelProps) -> Html {
    let collapsed = use_state(|| false);
    // Every hook must run on every render, so this callback is built *before*
    // the empty-queue early return below — a hook called after it would be
    // skipped on the first (empty) render and then demanded on the next one,
    // which trips Yew's hook-order assertion.
    let flip = {
        let collapsed = collapsed.clone();
        Callback::from(move |_: ()| collapsed.set(!*collapsed))
    };
    let toggle = {
        let flip = flip.clone();
        Callback::from(move |_: MouseEvent| flip.emit(()))
    };
    let toggle_key = Callback::from(move |e: KeyboardEvent| {
        // "Clear queue" is a button inside this header; Enter on it is its own
        // key, not a collapse. See `key_on_nested_control`.
        if key_on_nested_control(&e) {
            return;
        }
        if e.key() == "Enter" || e.key() == " " {
            e.prevent_default();
            flip.emit(());
        }
    });
    // Drag-to-reorder for the queue (shared with the sidebar playlists). The
    // `(id, label)` pairs are the order by id plus the title a keyboard move
    // names in its announcement.
    let reorder = use_reorder(
        props
            .queue
            .tracks
            .iter()
            .map(|t| (t.id.clone(), t.title.clone()))
            .collect(),
        0,
        props.on_move.clone(),
    );
    if props.queue.tracks.is_empty() {
        return html! {};
    }
    let clear = {
        let cb = props.on_clear.clone();
        Callback::from(move |e: MouseEvent| {
            e.stop_propagation();
            cb.emit(());
        })
    };

    html! {
        <section class="queue-panel">
            // The header toggles collapse on click; give it the same keyboard
            // path rather than leaving the panel mouse-only.
            <div class="queue-head" role="button" tabindex="0" aria-expanded={(!*collapsed).to_string()}
                 onclick={toggle} onkeydown={toggle_key}
                 title={if *collapsed { "Expand queue" } else { "Collapse queue" }}>
                <span class="queue-title">
                    <span class={classes!("queue-caret", (*collapsed).then_some("collapsed"))}>
                        { icon("chevron-down") }
                    </span>
                    { format!("UP NEXT ({})", props.queue.tracks.len()) }
                </span>
                <button class="ibtn" title="Clear queue" onclick={clear}>{ icon("trash") }</button>
            </div>
            if !*collapsed {
                <div class={classes!("queue-list", reorder.reordering.then_some("reordering"), reorder.hover_calm.then_some("hover-calm"))}
                     ref={reorder.list_ref.clone()} ondragenter={reorder.dragover.clone()}
                     ondragover={reorder.dragover.clone()} ondrop={reorder.drop.clone()}
                     onmousemove={reorder.calm_clear.clone()}
                     // Keyboard reorder lives on the list, not the row: the row
                     // already has a keydown handler, and Alt+Up/Down is the
                     // list's business either way.
                     onkeydown={reorder.keyboard.clone()}>
                    { for props.queue.tracks.iter().enumerate().map(|(i, t)| {
                        let jump = { let cb = props.on_jump.clone(); Callback::from(move |_: ()| cb.emit(i)) };
                        let onclick = {
                            let jump = jump.clone();
                            Callback::from(move |_: MouseEvent| jump.emit(()))
                        };
                        let onkeydown = Callback::from(move |e: KeyboardEvent| {
                            // The row's own remove button keeps its keys: Enter
                            // there removes the row, it does not jump to it.
                            if key_on_nested_control(&e) {
                                return;
                            }
                            if e.key() == "Enter" || e.key() == " " {
                                e.prevent_default();
                                jump.emit(());
                            }
                        });
                        let remove = {
                            let cb = props.on_remove.clone();
                            Callback::from(move |e: MouseEvent| { e.stop_propagation(); cb.emit(i); })
                        };
                        let current = props.queue.current == Some(i);
                        html! {
                            // role/tabindex rather than a real <button>: the row
                            // carries its own remove button and is a drag
                            // handle, and nesting those in a button is invalid.
                            <div class={classes!("qrow", current.then_some("current"), reorder.dragging(i).then_some("dragging"))}
                                 style={reorder.shift(i)}
                                 role="button" tabindex="0" aria-current={current.then_some("true")}
                                 // `data-index` is how the list's Alt+Up/Down
                                 // handler finds the focused row (see
                                 // `Reorder::keyboard`); the ARIA pair is what
                                 // tells a screen reader the shortcut exists.
                                 data-index={i.to_string()}
                                 aria-keyshortcuts="Alt+ArrowUp Alt+ArrowDown"
                                 title="Alt+Up / Alt+Down to reorder"
                                 draggable="true" onclick={onclick} onkeydown={onkeydown}
                                 ondragstart={reorder.dragstart(i)} ondragend={reorder.dragend()}>
                                { cover(&t.cover, "qrow-cover") }
                                <div class="trow-meta">
                                    <div class="trow-title">{ &t.title }</div>
                                    <div class="trow-artist">{ &t.artist }</div>
                                </div>
                                <button class="ibtn" title="Remove from queue" onclick={remove}>{ icon("x") }</button>
                            </div>
                        }
                    }) }
                </div>
            }
            // A keyboard reorder moves rows with no visual cue a screen reader
            // can follow, so the move is announced here. Visually hidden, polite
            // (it queues behind whatever is being read), and mounted empty from
            // the start — a region that appears already carrying its first
            // message is the one case screen readers routinely miss.
            <div class="sr-only" role="status" aria-live="polite">
                { reorder.announcement.clone().unwrap_or_default() }
            </div>
        </section>
    }
}
