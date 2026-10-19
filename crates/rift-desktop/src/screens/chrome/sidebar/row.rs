//! Sidebar row primitive (nav items and playlist rows), the playlist kebab
//! menu entries, and the pinned-bottom row geometry.

use crate::screens::{cards, menu};
use crate::state::Action;
use crate::text::Weight;
use crate::ui::{Icon, Id, Rect, Response, Sense, Ui};

// ---------------------------------------------------------------------------
// measured decomposition (ui-spec.md §6)
// ---------------------------------------------------------------------------

/// Sidebar row metrics: `.side-item` is 38 px tall with a 3 px bottom margin.
pub(super) const SIDE_ITEM_H: f32 = 38.0;
pub(super) const SIDE_ITEM_GAP: f32 = 3.0;
pub(super) const SIDE_PAD: f32 = 10.0;
pub(super) const SIDE_PAD_V: f32 = 12.0;
/// Distance from the sidebar's bottom edge to the Settings row's top
/// (12 px padding + Settings' own 3 px bottom margin + 38 px row).
pub(super) const SETTINGS_FROM_BOTTOM: f32 = SIDE_PAD_V + SIDE_ITEM_GAP + SIDE_ITEM_H; // 53
/// `side-divider` margin: 8 px either side of a 1 px line.
pub(super) const DIVIDER_MARGIN: f32 = 8.0;
/// Import-from-file row bottom → divider, measured in the reference (49 px so
/// the row's ink lands on the reference rows; Inter sits ~1 px higher than the
/// Segoe UI capture, and the reference is the target).
pub(super) const FILE_TO_DIVIDER: f32 = 49.0;
/// Import-playlist row bottom → import-from-file row top.
pub(super) const IMPORT_TO_FILE: f32 = 40.0;

/// Top edges of the sidebar's pinned bottom rows (Settings, divider,
/// import-from-file, import-playlist), measured bottom-up from the reference.
pub(super) fn sidebar_pinned_rows(rect: Rect) -> (f32, f32, f32, f32) {
    let settings_y = rect.bottom() - SETTINGS_FROM_BOTTOM;
    let divider_y = settings_y - DIVIDER_MARGIN - 1.0;
    let file_y = divider_y - FILE_TO_DIVIDER;
    let import_y = file_y - IMPORT_TO_FILE;
    (import_y, file_y, settings_y, divider_y)
}

/// Stable id of a sidebar row (matches [`side_row`]'s id derivation, so inline
/// editors can tell "focus is still on the row that opened me" from "focus
/// moved elsewhere, close").
pub(super) fn side_row_id(label: &str, active: bool) -> Id {
    Id::from_str("sidebar/row").with((label, active))
}

/// One sidebar row. Returns the interaction; the caller handles navigation
/// because the row type is shared by nav items and playlists.
///
/// `icon` is the lead glyph; `None` reserves the 16×16 lead-icon slot without
/// painting a glyph, which the now-playing rows use for the equalizer so the
/// two indicators never overlap (the equalizer *replaces* the row's icon).
///
/// `draggable` selects the interaction sense: playlist rows use [`Sense::DRAG`]
/// so the sidebar can reorder them (the old UI's shared `use_reorder` hook);
/// every other row keeps a plain click.
///
/// `interactive == false` paints the row exactly as usual but swaps both the row
/// and its kebab to [`Sense::HOVER`]: used while the reorder springs are still
/// settling, so a click cannot latch a visually-lower row mid-glide.
pub(super) struct SideRow {
    pub(super) response: Response,
    /// This row started a drag on this frame (only for `draggable` rows).
    pub(super) drag_started: bool,
    pub(super) kebab_clicked: bool,
    pub(super) secondary_clicked: bool,
    pub(super) kebab_rect: Rect,
    pub(super) icon_rect: Rect,
}

/// How a sidebar row reacts to the pointer.
///
/// `draggable` makes playlist rows reorderable ([`Sense::DRAG`]); `interactive`
/// is false while the reorder springs are settling, when the row keeps painting
/// normally but must not latch clicks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct RowBehavior {
    pub(super) draggable: bool,
    pub(super) interactive: bool,
}

impl RowBehavior {
    /// Nav / import / settings rows: clickable, never dragged.
    pub(super) const NAV: Self = Self {
        draggable: false,
        interactive: true,
    };
    /// Settling playlist rows: painted like a row but not clickable yet.
    pub(super) const SETTLING: Self = Self {
        draggable: true,
        interactive: false,
    };
    /// An interactive playlist row.
    pub(super) fn playlist() -> Self {
        Self {
            draggable: true,
            interactive: true,
        }
    }
}

pub(super) fn side_row(
    ui: &mut Ui,
    rect: Rect,
    icon: Option<Icon>,
    label: &str,
    active: bool,
    count: Option<usize>,
    behavior: RowBehavior,
) -> SideRow {
    let theme = ui.theme().clone();
    let id = side_row_id(label, active);
    // Measurements first: the kebab must be interacted *before* the row so its
    // press latches first (the first hovered widget to see the press wins, like
    // the old DOM's nested buttons stopping propagation).
    let count_w = count
        .map(|c| {
            ui.text_width(
                &c.to_string(),
                &cards::style(11.0, Weight::Regular, theme.text_dim),
            )
        })
        .unwrap_or(0.0);
    let kebab = Rect::new(
        rect.right() - 10.0 - 22.0,
        rect.y + (rect.h - 22.0) * 0.5,
        22.0,
        22.0,
    );
    let hovering = ui.hovered(rect);
    let kebab_resp = if count.is_some() && hovering {
        Some(ui.interact(
            kebab,
            id.with("kebab"),
            if behavior.interactive {
                Sense::CLICK
            } else {
                Sense::HOVER
            },
        ))
    } else {
        None
    };
    let row_sense = if !behavior.interactive {
        // Settling rows still paint their hover/kebab state, they just cannot
        // be clicked (or focused) until the reorder animation finishes.
        Sense::HOVER
    } else if behavior.draggable {
        Sense::DRAG
    } else {
        Sense::CLICK
    };
    let response = ui.interact(rect, id, row_sense);

    let t = response.hover_t;
    let bg = if active {
        theme.bg_hover
    } else {
        theme.bg_hover.with_alpha((t * 255.0).round() as u8)
    };
    if bg.a > 0 {
        ui.rounded_rect(rect, bg, theme.radius_md);
    }
    let text_color = if active || hovering {
        theme.text
    } else {
        theme.text_dim
    };
    // Lead icon: 16×16 at padding-left 10, vertically centred. `None` leaves the
    // slot empty so a caller-drawn indicator (the now-playing equalizer) is the
    // only thing shown there.
    let icon_rect = Rect::new(rect.x + 10.0, rect.y + (rect.h - 16.0) * 0.5, 16.0, 16.0);
    let icon_color = if active { theme.accent } else { text_color };
    if let Some(icon) = icon {
        ui.icon(icon_rect, icon, icon_color, 16.0);
    }
    // Label: 14 px/500, 10 px after the icon.
    let label_x = icon_rect.right() + 10.0;
    let label_w =
        (rect.right() - 10.0 - label_x - if count.is_some() { count_w + 8.0 } else { 0.0 })
            .max(0.0);
    ui.text(
        Rect::new(label_x, rect.y, label_w, rect.h),
        label,
        &cards::style_lh(14.0, Weight::Medium, text_color, 19.0),
    );

    // Count ↔ kebab swap on hover (the old CSS hides one to show the other).
    let mut kebab_clicked = false;
    match count {
        Some(n) if !hovering => {
            ui.text_right(
                Rect::new(rect.right() - 10.0 - count_w, rect.y, count_w, rect.h),
                &n.to_string(),
                &cards::style_lh(11.0, Weight::Regular, theme.text_dim, 15.0),
            );
        }
        Some(_) => {
            if let Some(kresp) = kebab_resp {
                let kt = kresp.hover_t;
                if kt > 0.0 {
                    ui.rounded_rect(
                        kebab,
                        theme.bg_hover.with_alpha((kt * 255.0).round() as u8),
                        theme.radius_sm,
                    );
                }
                ui.icon(
                    kebab,
                    Icon::MoreVert,
                    cards::mix(theme.text_dim, theme.text, kt),
                    15.0,
                );
                if kresp.focus_visible {
                    ui.focus_ring(kebab, theme.radius_sm);
                }
                kebab_clicked = kresp.clicked;
            }
        }
        None => {}
    }
    if response.focus_visible {
        ui.focus_ring(rect, theme.radius_md);
    }
    SideRow {
        drag_started: response.drag_started,
        response,
        kebab_clicked,
        secondary_clicked: kebab_resp.map(|r| r.secondary_clicked).unwrap_or(false)
            || response.secondary_clicked,
        kebab_rect: kebab,
        icon_rect,
    }
}

/// Entries of a sidebar playlist's kebab / right-click menu, in the old UI's
/// order: Rename · Export · Delete (danger).
pub(super) fn playlist_menu_entries(id: &str, name: &str) -> Vec<menu::Entry> {
    vec![
        menu::Entry::item(
            menu::Item::new("Rename", Some(Icon::Edit)),
            Action::BeginRenamePlaylist {
                id: id.to_string(),
                name: name.to_string(),
            },
        ),
        menu::Entry::item(
            menu::Item::new("Export", Some(Icon::Download)),
            Action::ExportPlaylist(id.to_string()),
        ),
        menu::Entry::item(
            menu::Item::new("Delete", Some(Icon::Delete)).danger(),
            Action::DeletePlaylist(id.to_string()),
        ),
    ]
}
