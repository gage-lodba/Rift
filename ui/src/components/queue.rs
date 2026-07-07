//! Collapsible "Up next" queue panel with drag-to-reorder rows.

use rift_types::QueueSnapshot;
use yew::prelude::*;

use super::icons::{cover, icon};
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
    // Drag-to-reorder for the queue (shared with the sidebar playlists).
    let reorder = use_reorder(
        props.queue.tracks.iter().map(|t| t.id.clone()).collect(),
        0,
        props.on_move.clone(),
    );
    if props.queue.tracks.is_empty() {
        return html! {};
    }
    let toggle = {
        let collapsed = collapsed.clone();
        Callback::from(move |_: MouseEvent| collapsed.set(!*collapsed))
    };
    let clear = {
        let cb = props.on_clear.clone();
        Callback::from(move |e: MouseEvent| {
            e.stop_propagation();
            cb.emit(());
        })
    };

    html! {
        <section class="queue-panel">
            <div class="queue-head" onclick={toggle}
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
                     onmousemove={reorder.calm_clear.clone()}>
                    { for props.queue.tracks.iter().enumerate().map(|(i, t)| {
                        let jump = { let cb = props.on_jump.clone(); Callback::from(move |_| cb.emit(i)) };
                        let remove = {
                            let cb = props.on_remove.clone();
                            Callback::from(move |e: MouseEvent| { e.stop_propagation(); cb.emit(i); })
                        };
                        let current = props.queue.current == Some(i);
                        html! {
                            <div class={classes!("qrow", current.then_some("current"), reorder.dragging(i).then_some("dragging"))}
                                 style={reorder.shift(i)}
                                 draggable="true" onclick={jump}
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
        </section>
    }
}
