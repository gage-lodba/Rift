//! The 54 px track row primitive: cover / title+artist (per-credit links) /
//! centred album / download slot / duration / heart + kebab, plus the small
//! download spinner and the determinate download progress bar.

use crate::state::{DownloadStatus, State};
use crate::text::{TextStyle, Weight};
use crate::theme::Theme;
use crate::ui::{Color, Icon, Id, Rect, Response, Sense, Ui};

use super::buttons::paint_icon_button;
use super::cover::cover_tile;
use super::mix;
use super::{
    style_lh, TROW_ACTIONS_W, TROW_ARTIST_LINE, TROW_COVER, TROW_DL_W, TROW_DUR_W, TROW_GAP,
    TROW_HEIGHT, TROW_PAD_X, TROW_PAD_Y, TROW_TITLE_LINE,
};

#[cfg(test)]
mod tests;

/// Height of the determinate download bar under the meta block.
const DOWNLOAD_BAR_H: f32 = 3.0;
/// Gap between the artist line's bottom and the download bar.
const DOWNLOAD_BAR_GAP: f32 = 3.0;

// ---------------------------------------------------------------------------
// track rows
// ---------------------------------------------------------------------------

/// Everything a [`track_row`] needs that does not come from `State`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TrackRowOpts {
    /// Highlight as the currently playing track (accent title + raised bg).
    pub playing: bool,
    /// Liked (filled heart).
    pub liked: bool,
    /// Offline and not downloaded: greyed and inert.
    pub unavailable: bool,
    /// Show the centred album column.
    pub show_album: bool,
    /// Show the per-row offline download slot.
    pub show_download: bool,
    pub download: DownloadStatus,
    /// Known byte-level progress of this track's in-flight download, in
    /// `0.0..=1.0`. `None` means unknown (the track is queued behind the one
    /// actively transferring): the row then only shows the indeterminate spinner
    /// in the download slot.
    pub download_progress: Option<f32>,
    /// Show the like button at the right end.
    pub show_like: bool,
    /// Force-reveal the hover controls (the reference screenshot's hovered row).
    pub reveal: bool,
}

impl Default for TrackRowOpts {
    fn default() -> Self {
        Self {
            playing: false,
            liked: false,
            unavailable: false,
            show_album: true,
            show_download: true,
            download: DownloadStatus::None,
            download_progress: None,
            show_like: true,
            reveal: false,
        }
    }
}

/// What the user did to a [`track_row`] this frame.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TrackRowResponse {
    /// The row was clicked (play) — not a nested control.
    pub played: bool,
    pub like: bool,
    /// The kebab was clicked / right-click was on the row.
    pub menu: bool,
    pub album: bool,
    /// Index (into `Track::artists`) of the clicked artist credit, if any.
    /// The caller resolves it to an artist id and navigates to the profile
    /// (the old row's `artist_links`).
    pub artist: Option<usize>,
    pub download: bool,
    pub remove_download: bool,
    /// Row rectangle, in case the caller wants to popup a menu under the kebab.
    pub rect: Rect,
    pub kebab_rect: Rect,
    pub hovered: bool,
}

/// One laid-out run of a track row's artist line: either a linkable credit
/// (`id`/`credit` set — the old `.link` button) or a plain run (separator or
/// the fallback artist string).
struct ArtistRun<'a> {
    text: &'a str,
    id: Option<&'a str>,
    credit: Option<usize>,
    rect: Rect,
}

/// What an artist-line run's interaction produced this frame.
struct ArtistHit {
    credit: usize,
    hover_t: f32,
    focus_visible: bool,
    clicked: bool,
}

/// One `.trow` track row (54 px). Lays out cover / title+artist / centred album /
/// spacer / download slot / duration / heart+kebab exactly as measured in
/// `ui-spec.md` §7.1.3, paints it, and reports the interactions.
///
/// **Interaction order matters**: nested controls (album link, download, heart,
/// kebab) are interacted *before* the row itself. The first hovered widget to see
/// `mouse_left_pressed` latches the UI's active widget, so a row registered first
/// would swallow every nested click (the old DOM's nested buttons stop
/// propagation instead). `row_resp.clicked` is therefore naturally false when a
/// nested control took the press.
///
/// `id` must be unique; derive it from a list id and the row index.
#[allow(clippy::too_many_arguments)]
pub fn track_row(
    ui: &mut Ui,
    state: &mut State,
    id: Id,
    rect: Rect,
    track: &rift_types::Track,
    index: usize,
    opts: TrackRowOpts,
) -> TrackRowResponse {
    let theme = ui.theme().clone();
    let row_h = TROW_HEIGHT;
    let row_rect = Rect::new(rect.x, rect.y, rect.w, row_h);

    // ---- geometry (CSS flex decomposition, see ui-spec.md §7.1.3) ----
    let inner = Rect::new(
        row_rect.x + TROW_PAD_X,
        row_rect.y,
        (row_rect.w - TROW_PAD_X * 2.0).max(0.0),
        row_h,
    );
    let album_style = style_lh(12.0, Weight::Regular, theme.text_dim, TROW_ARTIST_LINE);
    let artist_style = style_lh(12.0, Weight::Regular, theme.text_dim, TROW_ARTIST_LINE);
    let album_text = track.album.as_deref().unwrap_or("");
    let album_w = if opts.show_album && !album_text.is_empty() {
        ui.text_width(album_text, &album_style).min(340.0)
    } else {
        0.0
    };
    let actions_w = if opts.show_like { TROW_ACTIONS_W } else { 0.0 };
    let dl_w = if opts.show_download { TROW_DL_W } else { 0.0 };
    let fixed = TROW_COVER + TROW_GAP + album_w + dl_w + TROW_DUR_W + actions_w;
    let gaps = TROW_GAP * 5.0;
    let flex_total = (inner.w - fixed - gaps).max(0.0);
    let flex_each = flex_total * 0.5;

    let cover_rect = Rect::new(inner.x, row_rect.y + TROW_PAD_Y, TROW_COVER, TROW_COVER);
    let meta_x = cover_rect.right() + TROW_GAP;
    let meta_h = TROW_TITLE_LINE + TROW_ARTIST_LINE;
    let meta_y = row_rect.y + (row_h - meta_h) * 0.5;
    let meta_rect = Rect::new(meta_x, meta_y, flex_each, meta_h);

    // ---- determinate download bar geometry ----
    // A 3 px bar under the meta block: the meta block ends at row_top + 43.5 and
    // the row bottom is 54, so y 46.5..49.5 is clear of the artist line and of
    // the bottom edge. Width is the meta column, never the full row.
    let download_bar = Rect::new(
        meta_rect.x,
        meta_rect.y + meta_h + DOWNLOAD_BAR_GAP,
        meta_rect.w,
        DOWNLOAD_BAR_H,
    );

    // ---- artist line: per-credit links (old `artist_links`) ----
    // Every credit with an id is its own click target; separators and the plain
    // fallback are inert. Geometry is measured now so the hit rects can be
    // interacted before the row itself (nested controls win the press).
    let artist_line_y = meta_rect.y + TROW_TITLE_LINE;
    let artist_line = Rect::new(meta_rect.x, artist_line_y, meta_rect.w, TROW_ARTIST_LINE);
    let artist_runs = layout_artist_runs(ui, track, &artist_style, artist_line);

    let album_x = meta_rect.right() + TROW_GAP;
    let album_rect = Rect::new(album_x, row_rect.y, album_w, row_h);
    let dl_x = album_x + album_w + TROW_GAP + flex_each + TROW_GAP;
    let dl_rect = Rect::new(dl_x, row_rect.y + (row_h - 28.0) * 0.5, TROW_DL_W, 28.0);
    let dl_ibtn = Rect::new(
        dl_rect.x + (dl_rect.w - 28.0) * 0.5,
        dl_rect.y,
        28.0,
        dl_rect.h,
    );
    let dur_x = dl_x + dl_w + TROW_GAP;
    let dur_rect = Rect::new(dur_x, row_rect.y, TROW_DUR_W, row_h);
    let actions_x = dur_x + TROW_DUR_W + TROW_GAP;
    let like_rect = Rect::new(actions_x, row_rect.y + (row_h - 28.0) * 0.5, 28.0, 28.0);
    let kebab_rect = Rect::new(like_rect.right(), like_rect.y, 28.0, 28.0);

    // ---- interactions: nested controls first, row LAST ----
    let row_hovered = ui.hovered(row_rect);
    let reveal = row_hovered || opts.reveal;

    let album_resp = if album_w > 0.0 {
        ui.interact(album_rect, id.with(("album", index)), Sense::CLICK)
    } else {
        Response::default()
    };

    // Download slot states: the downloaded check and the failed retry are always
    // visible; the idle download button only appears on row hover (old CSS:
    // `.trow:hover .trow-dl .ibtn { opacity: 1 }`). Offline, un-downloaded rows
    // show an empty slot (ui-spec §7.1.3).
    let dl_visible = match opts.download {
        DownloadStatus::Downloaded | DownloadStatus::Failed => opts.show_download,
        DownloadStatus::Downloading => false,
        DownloadStatus::None => opts.show_download && reveal && !opts.unavailable,
    };
    let dl_resp = if dl_visible {
        ui.interact(dl_ibtn, id.with(("dl", index)), Sense::CLICK)
    } else {
        Response::default()
    };

    let heart_resp = if opts.show_like {
        ui.interact(like_rect, id.with(("like", index)), Sense::CLICK)
    } else {
        Response::default()
    };
    let kebab_resp = if reveal {
        ui.interact(kebab_rect, id.with(("menu", index)), Sense::CLICK)
    } else {
        Response::default()
    };

    // Artist credits are the first linkable run(s) of the artist line. They are
    // interacted here, before the row, so clicking a name never plays the track
    // (old `.link` buttons stopped propagation; this Ui latches the press).
    let mut artist_hits: Vec<ArtistHit> = Vec::with_capacity(track.artists.len());
    for run in &artist_runs {
        let (Some(_), Some(credit)) = (run.id, run.credit) else {
            continue;
        };
        let visible_w = run.rect.w.min((meta_rect.right() - run.rect.x).max(0.0));
        if visible_w <= 0.5 {
            continue;
        }
        let hit = Rect::new(run.rect.x, run.rect.y, visible_w, run.rect.h);
        let resp = ui.interact(hit, id.with(("artist", index, credit)), Sense::CLICK);
        if resp.hovered {
            ui.set_cursor(crate::ui::CursorShape::Pointer);
        }
        artist_hits.push(ArtistHit {
            credit,
            hover_t: resp.hover_t,
            focus_visible: resp.focus_visible,
            clicked: resp.clicked,
        });
    }

    let row_resp = ui.interact(row_rect, id, Sense::CLICK);

    // ---- paint ----
    if opts.playing || row_resp.hover_t > 0.0 {
        let color = if opts.playing {
            theme.bg_raised
        } else {
            theme
                .bg_raised
                .with_alpha((row_resp.hover_t * 255.0).round() as u8)
        };
        ui.rounded_rect(row_rect, color, theme.radius_md);
    }

    let texture = state.request_cover(&track.cover);
    cover_tile(ui, cover_rect, texture, 4.0);

    let title_color = if opts.playing {
        theme.accent
    } else {
        theme.text
    };
    let title_weight = if opts.playing {
        Weight::SemiBold
    } else {
        Weight::Medium
    };
    ui.text(
        Rect::new(meta_rect.x, meta_rect.y, meta_rect.w, TROW_TITLE_LINE),
        &track.title,
        &style_lh(13.0, title_weight, title_color, TROW_TITLE_LINE),
    );
    paint_artist_line(
        ui,
        meta_rect,
        artist_line,
        &artist_runs,
        &artist_hits,
        &theme,
        &artist_style,
    );

    // The download-slot spinner means "in flight"; the bar means "how far". It
    // appears only when the backend reported a byte fraction (a track whose
    // fraction is unknown — queued behind the active one — keeps the spinner
    // alone) and only on rows that show the download affordance at all.
    let download_fraction = match (opts.show_download, opts.download, opts.download_progress) {
        (true, DownloadStatus::Downloading, Some(fraction)) => Some(fraction),
        _ => None,
    };
    if let Some(fraction) = download_fraction {
        ui.progress(download_bar, fraction, theme.accent);
    }

    // Album link (dim, brighter + underlined on hover — `.link:hover`).
    if album_w > 0.0 {
        let color = mix(theme.text_dim, theme.text, album_resp.hover_t);
        ui.text_centered(album_rect, album_text, &album_style.with_color(color));
        if album_resp.hover_t > 0.5 {
            let w = ui.text_width(album_text, &album_style).min(album_rect.w);
            let x = album_rect.x + (album_rect.w - w) * 0.5;
            ui.hline(x, album_rect.y + row_h * 0.5 + 8.0, w, 1.0, color);
        }
        if album_resp.focus_visible {
            ui.focus_ring(album_rect, 4.0);
        }
    }

    if opts.show_download {
        paint_download_slot(
            ui,
            dl_ibtn,
            opts.download,
            reveal,
            opts.unavailable,
            &dl_resp,
            &theme,
        );
    }

    let dur = track
        .duration
        .map_or_else(String::new, |secs| crate::state::fmt_time(secs as f64));
    ui.text_right(
        dur_rect,
        &dur,
        &style_lh(12.0, Weight::Regular, theme.text_dim, TROW_ARTIST_LINE),
    );

    if opts.show_like {
        let (icon, base) = if opts.liked {
            (Icon::HeartFilled, theme.accent)
        } else {
            (Icon::Heart, theme.text_dim)
        };
        paint_icon_button(ui, like_rect, icon, 16.0, base, &heart_resp);
    }
    if reveal {
        paint_icon_button(
            ui,
            kebab_rect,
            Icon::MoreVert,
            15.0,
            theme.text_dim,
            &kebab_resp,
        );
    }

    if opts.unavailable {
        // Grey the whole row out (the old `.trow.unavailable { opacity: .4 }`;
        // the paint contract has no group opacity, so wash it with the bg color).
        ui.fill(row_rect, theme.bg.with_alpha(153));
    }
    if row_resp.focus_visible {
        ui.focus_ring_inset(row_rect, theme.radius_md);
    }

    TrackRowResponse {
        played: row_resp.clicked && !opts.unavailable,
        like: heart_resp.clicked,
        menu: kebab_resp.clicked || row_resp.secondary_clicked,
        album: album_resp.clicked,
        artist: artist_hits
            .iter()
            .find(|hit| hit.clicked)
            .map(|hit| hit.credit),
        download: dl_resp.clicked
            && matches!(opts.download, DownloadStatus::None | DownloadStatus::Failed),
        remove_download: dl_resp.clicked && opts.download == DownloadStatus::Downloaded,
        rect: row_rect,
        kebab_rect,
        hovered: row_resp.hovered,
    }
}

/// Lay out the artist line runs (per-credit links, `", "` separators or the
/// plain fallback artist string) relative to `artist_line`.
fn layout_artist_runs<'a>(
    ui: &mut Ui,
    track: &'a rift_types::Track,
    artist_style: &TextStyle,
    artist_line: Rect,
) -> Vec<ArtistRun<'a>> {
    let mut runs: Vec<ArtistRun<'a>> = Vec::with_capacity(track.artists.len() * 2 + 1);
    let mut x = artist_line.x;
    if track.artists.is_empty() {
        let w = ui.text_width(&track.artist, artist_style);
        runs.push(ArtistRun {
            text: track.artist.as_str(),
            id: None,
            credit: None,
            rect: Rect::new(x, artist_line.y, w, artist_line.h),
        });
        return runs;
    }
    for (credit, artist) in track.artists.iter().enumerate() {
        if credit > 0 {
            let sep = ", ";
            let w = ui.text_width(sep, artist_style);
            runs.push(ArtistRun {
                text: sep,
                id: None,
                credit: None,
                rect: Rect::new(x, artist_line.y, w, artist_line.h),
            });
            x += w;
        }
        let w = ui.text_width(&artist.name, artist_style);
        runs.push(ArtistRun {
            text: artist.name.as_str(),
            id: artist.id.as_deref(),
            credit: Some(credit),
            rect: Rect::new(x, artist_line.y, w, artist_line.h),
        });
        x += w;
    }
    runs
}

/// Paint the artist line (clipped to its column exactly like `.trow-artist {
/// overflow: hidden }`: long credit lists must not bleed into the album column)
/// and its focus rings. Rings are painted after the clip so they stay visible.
fn paint_artist_line(
    ui: &mut Ui,
    meta_rect: Rect,
    artist_line: Rect,
    runs: &[ArtistRun<'_>],
    hits: &[ArtistHit],
    theme: &Theme,
    artist_style: &TextStyle,
) {
    ui.with_clip(artist_line, |ui| {
        for run in runs {
            if run.rect.x >= meta_rect.right() {
                break;
            }
            let hover_t = hits
                .iter()
                .find(|hit| Some(hit.credit) == run.credit)
                .map_or(0.0, |hit| hit.hover_t);
            let color = if run.id.is_some() {
                mix(theme.text_dim, theme.text, hover_t)
            } else {
                theme.text_dim
            };
            let visible_w = run.rect.w.min((meta_rect.right() - run.rect.x).max(0.0));
            ui.text(
                Rect::new(run.rect.x, run.rect.y, visible_w.max(1.0), run.rect.h),
                run.text,
                &artist_style.with_color(color),
            );
            // `.link:hover { color: var(--text); text-decoration: underline }`.
            if hover_t > 0.5 {
                ui.hline(
                    run.rect.x,
                    run.rect.y + run.rect.h - 2.0,
                    visible_w,
                    1.0,
                    theme.text,
                );
            }
        }
    });
    for run in runs {
        let Some(credit) = run.credit else { continue };
        let Some(hit) = hits.iter().find(|hit| hit.credit == credit) else {
            continue;
        };
        if hit.focus_visible && run.rect.x < meta_rect.right() {
            ui.focus_ring(run.rect, 3.0);
        }
    }
}

/// Paint the per-row offline download slot from its already-interacted response.
fn paint_download_slot(
    ui: &mut Ui,
    rect: Rect,
    status: DownloadStatus,
    reveal: bool,
    unavailable: bool,
    resp: &Response,
    theme: &Theme,
) {
    match status {
        DownloadStatus::Downloaded => {
            paint_icon_button(ui, rect, Icon::CheckCircle, 15.0, theme.accent, resp);
        }
        DownloadStatus::Downloading => spinner(ui, rect.center_v(), 14.0, theme.text_dim),
        DownloadStatus::Failed => {
            paint_icon_button(ui, rect, Icon::Warning, 15.0, theme.danger, resp);
        }
        DownloadStatus::None => {
            if reveal && !unavailable {
                paint_icon_button(ui, rect, Icon::Download, 15.0, theme.text_dim, resp);
            }
        }
    }
}

/// A small spinner used by download states (`14px`, 2 px dim ring with a clear
/// gap, 0.7 s per revolution).
pub fn spinner(ui: &mut Ui, center: crate::ui::Vec2, size: f32, color: Color) {
    let t = ui.time() as f32;
    let angle = (t / 0.7) * std::f32::consts::TAU;
    let radius = size * 0.5 - 1.0;
    // Approximate the ring with 8 short segments and a gap.
    let segments = 8;
    for i in 0..segments {
        let a = angle + i as f32 * std::f32::consts::TAU / segments as f32;
        let x = center.x + a.cos() * radius;
        let y = center.y + a.sin() * radius;
        let alpha = (i as f32 / segments as f32 * 255.0) as u8;
        ui.fill(
            Rect::new(x - 1.0, y - 1.0, 2.0, 2.0),
            color.with_alpha(alpha.max(40)),
        );
    }
}
