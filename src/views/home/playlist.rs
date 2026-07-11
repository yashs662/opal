//! Playlist (and Liked Songs) detail page rendered into the centre pane.
//!
//! Loads **progressively**: the header + scrollbar appear from metadata
//! before any track lands, the first page mounts the virtualised list,
//! and later pages stream into a shared live buffer that the `lazy_list`
//! reads on scroll — no blocking "loading all 989 songs" screen, no
//! full-list rebuilds while paging. Rows past the loaded count render a
//! lightweight skeleton until their page arrives.
//!
//! The buffer ([`RowBuf`]) is owned by `AppState` and mutated on the UI
//! thread as worker pages arrive; the render closure here just indexes
//! it. Covers are baked into each [`PlaylistRow`] as a reactive `Signal`
//! (resolved/dispatched when the row is appended), so a cover arrival
//! repaints just that thumb with no rebuild.

use std::cell::RefCell;
use std::rc::Rc;

use opal_gfx::{Align, Computed, ImageHandle, Justify, Len, Overflow, Scene, Signal};

use crate::api::PlayTarget;
use crate::views::MainNav;
use crate::views::home::{NavFn, PlayFn};
use crate::widgets::color::accent_fg;
use crate::widgets::icon::{Icon, IconSet};
use crate::widgets::tokens as t;

/// Track-row height. Thumb (40) + breathing room.
const ROW_H: f32 = t::SP_14;

// --- Collapsing detail-header geometry (logical px) -----------------------
//
// The page is a single scroller: the hero is **row 0** of the lazy list so it
// scrolls away 1:1 with the tracks, and a compact bar + column header are
// pinned overlays whose opacity is driven by `detail_collapse` (written each
// frame from this scroller's offset in `app::frame::tick`). These constants
// are read there too — keep them `pub`.

/// Fixed hero (cover + title + Play) block height.
pub const HERO_H: f32 = t::SP_64 + t::SP_8;
/// Column-header strip height (`# Title Album Time`).
pub const COLHEADER_H: f32 = t::SP_8;
/// Pinned compact-bar height (mini Play + title). Roomy in Y so the bar
/// breathes rather than crowding the controls.
pub const BAR_H: f32 = t::SP_16;
/// Scroll distance over which the hero collapses into the bar.
pub const COLLAPSE_RANGE: f32 = HERO_H - BAR_H;

/// Spotify's `PUT /me/player/play` caps the inline `uris` array. For the
/// context-less Liked Songs we send a window from the clicked track so
/// playback begins there and queues the following tracks.
const URIS_WINDOW: usize = 100;

/// A fully-baked track row — built when the track is appended to the
/// buffer (so the cover `Signal` is resolved off the shared art map on
/// the UI thread). The render closure just reads these.
pub struct PlaylistRow {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration: String,
    pub uri: String,
    /// Reactive cover handle (bound via `image_bound`). `None` if the
    /// track has no cover URL; the inner `Signal` stays `None` until the
    /// cover is lazily fetched (see `cover_url` + `request_cover`).
    pub art: Option<Signal<Option<ImageHandle>>>,
    /// Source URL, kept so the cover can be fetched **lazily** the first
    /// time the row scrolls into view — avoids dispatching thousands of
    /// downloads up front for a long playlist.
    pub cover_url: Option<String>,
    /// All credited artists (id + name) for the clickable artist line.
    pub artists: Vec<crate::api::TrackArtist>,
    /// Album + first-artist id for the right-click menu's "Go to …".
    pub album_id: String,
    pub artist_id: String,
    /// False for local files / region-unavailable tracks — the row still
    /// renders (faded) but takes no clicks and never enters a play queue.
    pub playable: bool,
}

/// Request a track cover be fetched (called when a row materializes).
/// Idempotent + gated on the consumer side.
pub type CoverFn = Rc<dyn Fn(String)>;

/// Shared, growable track buffer for the open playlist. `AppState` owns
/// it and appends streamed pages; the `lazy_list` render closure holds a
/// clone and reads it per visible row.
pub type RowBuf = Rc<RefCell<Vec<PlaylistRow>>>;

/// Everything the view needs for one render. Built per rebuild from
/// `AppState.open_playlist`; cheap (small metadata clones + Rc handles).
pub struct PlaylistViewData {
    pub name: String,
    pub owner: String,
    /// Reported track total — drives the list length + scrollbar even
    /// before every page has streamed in.
    pub total: u32,
    pub liked: bool,
    /// Hero eyebrow label — "Playlist" or "Album".
    pub kind_label: &'static str,
    /// Metadata not yet arrived (header shows the sidebar-known name, the
    /// list shows skeletons).
    pub loading: bool,
    pub cover: Option<Signal<Option<ImageHandle>>>,
    /// `spotify:playlist:…` for real playlists; `None` for Liked Songs.
    pub context_uri: Option<String>,
    pub rows: RowBuf,
    /// Fetch a row's cover the first time it scrolls into view.
    pub request_cover: CoverFn,
    /// Skeleton pulse opacity — ping-pong tweened while pages are still
    /// streaming (`LibraryModel::skeleton_pulse`), parked at 1.0 after.
    pub pulse: Signal<f32>,
    /// Right-click a track row → context menu.
    pub on_context_menu: crate::views::home::CtxMenuFn,
}

/// Render the centre-pane content for the open playlist. Children are
/// added to `s` (the caller's slide/fade transition wrapper).
///
/// The whole page is one scroller: the hero is **row 0** of the lazy list
/// (so it scrolls away 1:1), the column header is **row 1**, and tracks
/// follow. `collapse` (0 expanded → 1 collapsed, written each frame from
/// the scroll offset) slides + fades the pinned sticky bar down from above:
/// while expanded it sits off the top edge, so it never paints or hit-tests
/// over the hero.
#[allow(clippy::too_many_arguments)]
pub fn view(
    s: &mut Scene,
    icons: &Rc<IconSet>,
    data: &PlaylistViewData,
    accent: &Signal<[f32; 4]>,
    collapse: &Signal<f32>,
    // Content-scoped scroller name (`MainNav::detail_scroll_node`) —
    // rebuilds preserve scroll by name, navigation resets it by changing
    // the name. `app::frame::tick` reads the offset under the same name.
    scroll_node: &str,
    on_play: PlayFn,
    on_navigate: NavFn,
) {
    let loaded = data.rows.borrow().len() as u32;
    let count = data.total.max(loaded);
    // Track rows after [hero, column-header]: real count, skeletons while
    // loading, or a single empty-state row when genuinely empty.
    let track_n = if count > 0 {
        count
    } else if data.loading {
        12
    } else {
        1
    };

    // Closure captures (the render fn is `Fn`, called per visible row).
    let rows = data.rows.clone();
    let ctx = data.context_uri.clone();
    let request_cover = data.request_cover.clone();
    let on_play_rows = on_play.clone();
    let icons_h = icons.clone();
    let accent_h = accent.clone();
    let on_play_h = on_play.clone();
    let nav_h = on_navigate.clone();
    let hero = HeroData::new(data);
    let empty_loading = data.loading;
    let collapse_rows = collapse.clone();
    let pulse = data.pulse.clone();
    let on_ctx_menu = data.on_context_menu.clone();
    let nav_rows = on_navigate.clone();

    s.lazy_list(scroll_node, track_n + 2, ROW_H, move |sc, i| match i {
        0 => hero_block(sc, &icons_h, &hero, &accent_h, &on_play_h, &nav_h),
        1 => column_header(sc, &collapse_rows),
        _ => {
            let ti = i - 2;
            let len = rows.borrow().len() as u32;
            if count > 0 && ti < len {
                let buf = rows.borrow();
                track_row(
                    sc,
                    &buf[ti as usize],
                    ti,
                    &on_play_rows,
                    &ctx,
                    &rows,
                    &request_cover,
                    &on_ctx_menu,
                    &nav_rows,
                );
            } else if count > 0 || empty_loading {
                skeleton_row(sc, ti, &pulse);
            } else {
                empty_row(sc);
            }
        }
    })
    .w(Len::Fill)
    .h(Len::Fill)
    .pad_ltrb(t::SP_3, t::SP_0, t::SP_3, t::SP_4)
    // Hero is a tall first row; the rest stay at ROW_H.
    .lazy_list_row_height(0, HERO_H)
    .lazy_list_row_height(1, COLHEADER_H)
    // Compositor scroll layer: the materialized window rasters once into a
    // tall texture; scrolling moves the composite window, no re-raster.
    .layer()
    // The bar's top inset is driven each frame from `collapse` (see
    // `app::frame::tick`): 0 while the glass header is hidden, growing to the
    // header height as it slides in — so the bar shrinks/grows smoothly with
    // the overlay and is never hidden behind it. (No static inset here.)
    .scrollbar(|sb| sb.auto_hide(true).margin(t::SP_0_5).thickness(t::SP_1));

    // Pinned compact bar — slides + fades down from above the pane as the
    // hero collapses (off-screen while expanded, so no stray hit-tests).
    sticky_bar(s, icons, data, accent, collapse, &on_play, &on_navigate);
}

/// Build the playback target for the track at `index`. Real playlists
/// play their context at the offset; the Liked Songs collection context
/// anchors by *track uri* instead (its server-side order is
/// added-at-desc, which can drift from the listed index — and the uri
/// anchor doubles as the 400-fallback recovery point). With no context
/// at all, a capped window of URIs from the clicked track.
fn make_target(context_uri: &Option<String>, rows: &RowBuf, index: u32) -> PlayTarget {
    match context_uri {
        Some(uri) if uri.ends_with(":collection") => {
            let track_uri = rows
                .borrow()
                .get(index as usize)
                .map(|r| r.uri.clone())
                .unwrap_or_default();
            PlayTarget::ContextAt {
                context_uri: uri.clone(),
                track_uri,
            }
        }
        Some(uri) => PlayTarget::Context {
            context_uri: uri.clone(),
            offset: index,
        },
        None => {
            let buf = rows.borrow();
            // Unplayable rows can't go in the request — Spotify rejects
            // local/blocked URIs — so the window is built from playable
            // ones only. The clicked row itself is always playable
            // (disabled rows take no clicks).
            let uris = buf
                .iter()
                .skip(index as usize)
                .filter(|r| r.playable)
                .take(URIS_WINDOW)
                .map(|r| r.uri.clone())
                .collect();
            PlayTarget::Uris { uris, offset: 0 }
        }
    }
}

/// Hero-block inputs cloned out of [`PlaylistViewData`] so the (`Fn`) lazy
/// render closure can own them across rebuilds.
struct HeroData {
    name: String,
    owner: String,
    cover: Option<Signal<Option<ImageHandle>>>,
    liked: bool,
    total: u32,
    kind_label: &'static str,
    loading: bool,
    context_uri: Option<String>,
    rows: RowBuf,
    has_tracks: bool,
}

impl HeroData {
    fn new(d: &PlaylistViewData) -> Self {
        Self {
            name: d.name.clone(),
            owner: d.owner.clone(),
            cover: d.cover.clone(),
            liked: d.liked,
            total: d.total,
            kind_label: d.kind_label,
            loading: d.loading,
            context_uri: d.context_uri.clone(),
            rows: d.rows.clone(),
            has_tracks: d.total > 0 || !d.rows.borrow().is_empty(),
        }
    }
}

/// Row 0 of the list: back chevron + big cover/title + the big Play pill.
/// A fixed `HERO_H` block that scrolls away with the content.
fn hero_block(
    s: &mut Scene,
    icons: &Rc<IconSet>,
    d: &HeroData,
    accent: &Signal<[f32; 4]>,
    on_play: &PlayFn,
    on_navigate: &NavFn,
) {
    s.col(())
        .w(Len::Fill)
        .h_px(HERO_H)
        .pad_ltrb(t::SP_3, t::SP_3, t::SP_3, t::SP_0)
        .gap(t::SP_2)
        .justify(Justify::End)
        .child(|hero| {
            // Back chevron pinned top-left of the hero (abs so the justified
            // cover/title sink to the bottom); scrolls away with the hero.
            let nav = on_navigate.clone();
            hero.row(())
                .abs(0.0, 0.0)
                .w_px(t::TOPBAR_BTN)
                .h_px(t::TOPBAR_BTN)
                .rgba(0.0, 0.0, 0.0, 0.30)
                .hover_color(t::PANEL_HI)
                .radius(t::R_FULL)
                .center()
                .on_click(move |ctx| nav(ctx, MainNav::Home))
                .child(|c| icons.render(c, Icon::ChevronLeft, t::ICON_MD, t::TEXT));
            // Cover + title block.
            hero.row(())
                .w(Len::Fill)
                .gap(t::SP_5)
                .align(Align::End)
                .child(|h| {
                    cover_art(h, icons, d.cover.clone(), d.liked);
                    h.col(()).gap(t::SP_2).justify(Justify::End).child(|m| {
                        m.text((), d.kind_label, 12.0).color(t::TEXT_DIM);
                        m.text((), &d.name, 32.0).color(t::TEXT).max_width_px(520.0);
                        m.row(()).gap(t::SP_1_5).align(Align::Center).child(|sub| {
                            if !d.owner.is_empty() {
                                sub.text((), &d.owner, 12.0).color(t::TEXT);
                                sub.text((), "•", 12.0).color(t::TEXT_DIM);
                            }
                            sub.text((), count_label(d.total, d.loading), 12.0)
                                .color(t::TEXT_DIM);
                        });
                    });
                });
            // Action row: big Play pill.
            let on_play = on_play.clone();
            let rows = d.rows.clone();
            let ctx = d.context_uri.clone();
            let has_tracks = d.has_tracks;
            let acc = accent.clone();
            hero.row(())
                .w(Len::Fill)
                .h_px(t::SP_16)
                .gap(t::SP_4)
                .align(Align::Center)
                .child(move |a| {
                    let fg = accent_fg(&acc);
                    let mut pill = a.row(());
                    pill.w_px(t::SP_14)
                        .h_px(t::SP_14)
                        .center()
                        .color(acc)
                        .radius(t::R_FULL);
                    if has_tracks {
                        pill.hover_opacity(0.85)
                            .on_click(move |_| on_play(make_target(&ctx, &rows, 0)));
                    } else {
                        pill.opacity(0.4);
                    }
                    pill.child(|p| icons.render(p, Icon::Play, t::ICON_LG, fg));
                });
        });
}

/// The `# / Title / Time` column labels — shared by the in-list header
/// (row 1) and the pinned sticky bar.
fn strip_items(h: &mut Scene) {
    h.row(()).w_px(t::SP_7).center().child(|x| {
        x.text((), "#", 12.0).color(t::TEXT_DIM);
    });
    // Transparent spacer matching the row's thumb, so each label sits over
    // its column (Title over the track titles, not over the thumbs).
    h.rect(())
        .w_px(t::THUMB_SM)
        .h_px(t::SP_PX)
        .rgba(0.0, 0.0, 0.0, 0.0);
    h.col(()).w(Len::Fill).child(|x| {
        x.text((), "Title", 12.0).color(t::TEXT_DIM);
    });
    h.col(()).w_px(t::SP_48).child(|x| {
        x.text((), "Album", 12.0).color(t::TEXT_DIM);
    });
    h.row(()).w_px(t::SP_12).justify(Justify::End).child(|x| {
        x.text((), "Time", 12.0).color(t::TEXT_DIM);
    });
}

/// Collapse value at which the sticky glass' bottom edge first touches
/// the in-list column header: from here to `collapse == 1` the glass
/// sweeps over the header, and at exactly 1 the header's rect coincides
/// with the overlay's copy of it — which is what makes the handoff
/// (in-list fades out under the glass, overlay fades in on it) read as
/// the original header *sticking onto* the overlay. Derived, not tuned:
/// glass bottom = `c·total_h`, header top = `HERO_H − c·COLLAPSE_RANGE`.
fn header_touch_c() -> f32 {
    HERO_H / (BAR_H + COLHEADER_H + COLLAPSE_RANGE)
}

/// Row 1: the column-label strip that scrolls under the sticky bar.
/// Hidden once fully collapsed — it sits exactly under the overlay's
/// (now visible) copy by then, and hiding it keeps its frosted ghost
/// from bleeding through the glass.
fn column_header(s: &mut Scene, collapse: &Signal<f32>) {
    let vis = Computed::new((collapse.clone(),), |(c,)| if c >= 1.0 { 0.0 } else { 1.0 });
    s.row(())
        .w(Len::Fill)
        .h_px(COLHEADER_H)
        .pad_xy(t::SP_3, t::SP_0)
        .gap(t::SP_3)
        .align(Align::Center)
        .opacity_bind(vis)
        .child(strip_items);
}

/// Single centred row used when a playlist is genuinely empty.
fn empty_row(s: &mut Scene) {
    s.row(()).w(Len::Fill).h_px(ROW_H).center().child(|c| {
        c.text((), "No songs here yet", 14.0).color(t::TEXT_DIM);
    });
}

/// Pinned compact bar (back + mini Play + title) over a repeat of the
/// column labels. Absolutely positioned and slid down from above the pane
/// by `collapse`: at 0 it sits fully off the top edge (no paint, no hit
/// over the hero); at 1 it rests flush at the top. Opacity tracks the same
/// signal so it dissolves in as it arrives.
fn sticky_bar(
    s: &mut Scene,
    icons: &Rc<IconSet>,
    data: &PlaylistViewData,
    accent: &Signal<[f32; 4]>,
    collapse: &Signal<f32>,
    on_play: &PlayFn,
    on_navigate: &NavFn,
) {
    let title = data.name.clone();
    let has_tracks = data.total > 0 || !data.rows.borrow().is_empty();
    let rows = data.rows.clone();
    let ctx = data.context_uri.clone();
    let on_play = on_play.clone();
    let nav = on_navigate.clone();
    let acc = accent.clone();
    let total_h = BAR_H + COLHEADER_H;
    let touch = header_touch_c();
    let collapse_h = collapse.clone();
    // Slide from y = -(bar height) (fully above) up to y = 0 (flush).
    let slide = Computed::new((collapse.clone(),), move |(c,)| {
        [0.0, (c.clamp(0.0, 1.0) - 1.0) * total_h]
    });
    // Frosted glass over the content scrolling beneath it: the per-glass
    // backdrop pass composites every layer below this one (ambient art +
    // root + the track-list scroll layer), so the header genuinely frosts
    // the rows sliding under it.
    s.glass(())
        .pos(slide)
        .w(Len::Fill)
        .h_px(total_h)
        .blur(10.0)
        .rgba(t::PANEL[0], t::PANEL[1], t::PANEL[2], 0.72)
        // Round only the TOP corners to match the centre pane (`main_area`,
        // R_LG); bottom stays square (it meets the list + hairline). Needed
        // because this is its own `.layer()` — composited separately, so the
        // pane's rounded clip doesn't apply to it.
        .radii(t::R_LG, t::R_LG, 0.0, 0.0)
        .opacity_bind(collapse.clone())
        // Promote to its own composite layer ABOVE the track-list layer so
        // the per-glass backdrop pass frosts the list scrolling beneath it
        // (a glass only sees layers below its own in z-order).
        .layer()
        .child(move |c| {
            c.row(())
                .w(Len::Fill)
                .h_px(BAR_H)
                .pad_xy(t::SP_4, t::SP_0)
                .gap(t::SP_3)
                .align(Align::Center)
                .child(move |bar| {
                    bar.row(())
                        .w_px(t::TOPBAR_BTN)
                        .h_px(t::TOPBAR_BTN)
                        .rgba(0.0, 0.0, 0.0, 0.30)
                        .hover_color(t::PANEL_HI)
                        .radius(t::R_FULL)
                        .center()
                        .on_click(move |ctx| nav(ctx, MainNav::Home))
                        .child(|c| icons.render(c, Icon::ChevronLeft, t::ICON_MD, t::TEXT));
                    let fg = accent_fg(&acc);
                    let mut pill = bar.row(());
                    pill.w_px(t::SP_10)
                        .h_px(t::SP_10)
                        .center()
                        .color(acc)
                        .radius(t::R_FULL);
                    if has_tracks {
                        pill.hover_opacity(0.85)
                            .on_click(move |_| on_play(make_target(&ctx, &rows, 0)));
                    } else {
                        pill.opacity(0.4);
                    }
                    pill.child(|p| icons.render(p, Icon::Play, t::ICON_MD, fg));
                    bar.text((), &title, 16.0)
                        .color(t::TEXT)
                        .max_width_px(360.0);
                });
            // Overlay copy of the column header. Geometry mirrors the
            // in-list one exactly — the glass spans the full pane, so its
            // inset must be the list's pad (SP_3) plus the header row's
            // own pad (SP_3) for the columns to line up. Faded in over
            // the window where the glass sweeps across the in-list header
            // (they coincide pixel-exact at collapse = 1), so the header
            // appears to stick onto the overlay rather than duplicate it.
            let dock = Computed::new((collapse_h.clone(),), move |(c,)| {
                ((c - touch) / (1.0 - touch)).clamp(0.0, 1.0)
            });
            c.row(())
                .w(Len::Fill)
                .h_px(COLHEADER_H)
                .pad_xy(t::SP_3 + t::SP_3, t::SP_0)
                .gap(t::SP_3)
                .align(Align::Center)
                .opacity_bind(dock)
                .child(strip_items);
            // Hairline at the bottom edge — a crisp divider on top of the
            // glass so the sticky header reads as distinct from the list
            // below, not a seam/gap between two panes.
            c.rect(())
                .abs(0.0, total_h - t::SP_PX)
                .w(Len::Fill)
                .h_px(t::SP_PX)
                .rgba(1.0, 1.0, 1.0, 0.10);
        });
}

/// Header count label — "Loading…" until tracks land, then "N songs".
fn count_label(total: u32, loading: bool) -> String {
    if total == 0 {
        if loading {
            "Loading…".to_string()
        } else {
            "0 songs".to_string()
        }
    } else if total == 1 {
        "1 song".to_string()
    } else {
        format!("{total} songs")
    }
}

/// Square cover. Liked Songs has no image — render the signature
/// purple-ish tile with a heart instead.
fn cover_art(
    s: &mut Scene,
    icons: &Rc<IconSet>,
    art: Option<Signal<Option<ImageHandle>>>,
    liked: bool,
) {
    s.col(()).w_px(t::THUMB_2XL).h_px(t::THUMB_2XL).child(|b| {
        if liked {
            b.rect(())
                .abs(0.0, 0.0)
                .w(Len::Fill)
                .h(Len::Fill)
                .rgba(0.36, 0.20, 0.78, 1.0)
                .radius(t::R_LG);
            b.row(())
                .abs(0.0, 0.0)
                .w(Len::Fill)
                .h(Len::Fill)
                .center()
                .child(|c| {
                    icons.render(c, Icon::Heart, t::ICON_XL, t::TEXT);
                });
            return;
        }
        // One node — the cover paints its own rounded loading fill, no rect
        // stacked behind it to leak through the corner.
        if let Some(sig) = art {
            b.image_bound((), sig)
                .abs(0.0, 0.0)
                .w(Len::Fill)
                .h(Len::Fill)
                .radius(t::R_LG)
                .placeholder_fill(t::PLACEHOLDER);
        } else {
            b.rect(())
                .abs(0.0, 0.0)
                .w(Len::Fill)
                .h(Len::Fill)
                .rgba(t::PLACEHOLDER[0], t::PLACEHOLDER[1], t::PLACEHOLDER[2], 1.0)
                .radius(t::R_LG);
        }
    });
}

#[allow(clippy::too_many_arguments)]
fn track_row(
    s: &mut Scene,
    r: &PlaylistRow,
    index: u32,
    on_play: &PlayFn,
    context_uri: &Option<String>,
    rows: &RowBuf,
    request_cover: &CoverFn,
    on_context_menu: &crate::views::home::CtxMenuFn,
    on_navigate: &NavFn,
) {
    // Lazily fetch this row's cover the first time it materializes (and
    // isn't resolved yet). The consumer gates on inflight/resolved, so
    // repeated materializes are cheap no-ops.
    if let Some(url) = &r.cover_url
        && r.art.as_ref().map(|s| s.get().is_none()).unwrap_or(false)
    {
        request_cover(url.clone());
    }
    let mut row = s.row(());
    row.w(Len::Fill)
        .h_px(ROW_H)
        .pad_xy(t::SP_3, t::SP_1)
        .gap(t::SP_3)
        .align(Align::Center)
        .radius(t::R_MD);
    if r.playable {
        let on_play = on_play.clone();
        let rows = rows.clone();
        let ctx = context_uri.clone();
        row.hover_color(t::HOVER_LIFT_SUBTLE)
            .on_click(move |_| on_play(make_target(&ctx, &rows, index)));
    } else {
        // Local file / region-blocked: visible so the playlist reads
        // complete, but faded and inert — no hover lift, no click.
        row.opacity(0.4);
    }
    // Right-click → context menu (Add to queue / Go to album / artist).
    crate::views::home::attach_context_menu(
        &mut row,
        on_context_menu,
        crate::model::MenuTarget {
            uri: r.uri.clone(),
            album_id: r.album_id.clone(),
            artist_id: r.artist_id.clone(),
        },
    );
    row.child(|row| {
            row.row(()).w_px(t::SP_7).center().child(|c| {
                c.text((), format!("{}", index + 1), 13.0)
                    .color(t::TEXT_DIM);
            });
            // Thumb.
            row.col(()).w_px(t::THUMB_SM).h_px(t::THUMB_SM).child(|b| {
                if let Some(sig) = r.art.clone() {
                    b.image_bound((), sig)
                        .abs(0.0, 0.0)
                        .w(Len::Fill)
                        .h(Len::Fill)
                        .radius(t::R_SM)
                        .placeholder_fill(t::PLACEHOLDER);
                } else {
                    b.rect(())
                        .abs(0.0, 0.0)
                        .w(Len::Fill)
                        .h(Len::Fill)
                        .rgba(t::PLACEHOLDER[0], t::PLACEHOLDER[1], t::PLACEHOLDER[2], 1.0)
                        .radius(t::R_SM);
                }
            });
            // Title + artist(s).
            row.col(())
                .w(Len::Fill)
                .gap(t::SP_0_5)
                .h(Len::Fill)
                .justify(Justify::Center)
                .overflow_x(Overflow::Hidden)
                .child(|m| {
                    m.text((), &r.title, 14.0)
                        .color(t::TEXT)
                        .max_width_px(360.0);
                    artist_line(m, &r.artists, &r.artist, on_navigate, 360.0);
                });
            // Album.
            row.col(())
                .w_px(t::SP_48)
                .h(Len::Fill)
                .justify(Justify::Center)
                .overflow_x(Overflow::Hidden)
                .child(|m| {
                    m.text((), &r.album, 12.0)
                        .color(t::TEXT_DIM)
                        .max_width_px(t::SP_48);
                });
            // Duration.
            row.row(()).w_px(t::SP_12).justify(Justify::End).child(|c| {
                c.text((), &r.duration, 12.0).color(t::TEXT_DIM);
            });
        });
}

/// The artist line for a track row: one clickable span per credited
/// artist (each → its artist page), comma-separated. Falls back to the
/// plain joined `fallback` string when the per-artist list is absent
/// (older cache entries). Reusable across track/queue rows.
pub(crate) fn artist_line(
    s: &mut Scene,
    artists: &[crate::api::TrackArtist],
    fallback: &str,
    on_navigate: &NavFn,
    max_w: f32,
) {
    if artists.is_empty() {
        s.text((), fallback, 12.0).color(t::TEXT_DIM).max_width_px(max_w);
        return;
    }
    s.row(())
        .w(Len::Fill)
        .align(Align::Center)
        .overflow_x(Overflow::Hidden)
        .child(|line| {
            for (i, a) in artists.iter().enumerate() {
                if i > 0 {
                    line.text((), ", ", 12.0).color(t::TEXT_DIM);
                }
                if a.id.is_empty() {
                    line.text((), &a.name, 12.0).color(t::TEXT_DIM);
                    continue;
                }
                let nav = on_navigate.clone();
                let id = a.id.clone();
                line.text((), &a.name, 12.0)
                    .color(t::TEXT_DIM)
                    .cursor(opal_gfx::CursorIcon::Pointer)
                    .hover_color(t::TEXT)
                    .on_click(move |ctx| nav(ctx, MainNav::Artist { id: id.clone() }));
            }
        });
}

/// Placeholder for a not-yet-streamed row — index number + grey bars.
fn skeleton_row(s: &mut Scene, index: u32, pulse: &Signal<f32>) {
    s.row(())
        .w(Len::Fill)
        .h_px(ROW_H)
        .pad_xy(t::SP_3, t::SP_1)
        .gap(t::SP_3)
        .align(Align::Center)
        // Soft loading pulse — the bound signal ping-pongs on the
        // timeline while pages stream, and opacity cascades down the
        // subtree so the whole placeholder breathes as one.
        .opacity_bind(pulse.clone())
        .child(|row| {
            row.row(()).w_px(t::SP_7).center().child(|c| {
                c.text((), format!("{}", index + 1), 13.0)
                    .color(t::TEXT_DIM);
            });
            row.rect(())
                .w_px(t::THUMB_SM)
                .h_px(t::THUMB_SM)
                .rgba(t::PLACEHOLDER[0], t::PLACEHOLDER[1], t::PLACEHOLDER[2], 0.6)
                .radius(t::R_SM);
            row.col(())
                .w(Len::Fill)
                .gap(t::SP_1_5)
                .justify(Justify::Center)
                .h(Len::Fill)
                .child(|m| {
                    m.rect(())
                        .w_px(t::SP_40)
                        .h_px(t::SP_2)
                        .rgba(1.0, 1.0, 1.0, 0.08)
                        .radius(t::R_SM);
                    m.rect(())
                        .w_px(t::SP_24)
                        .h_px(t::SP_2)
                        .rgba(1.0, 1.0, 1.0, 0.05)
                        .radius(t::R_SM);
                });
        });
}

/// `ms` → `m:ss`.
pub fn fmt_duration(ms: u64) -> String {
    let secs = ms / 1000;
    format!("{}:{:02}", secs / 60, secs % 60)
}
