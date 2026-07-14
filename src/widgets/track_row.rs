//! Shared interactive track row — the row style flat track lists use
//! (artist page sections; the queue mirrors the same pieces), so the
//! affordances stay consistent everywhere: click activates (play/skip),
//! per-artist clickable credit line, right-click opens the shared
//! context menu (add to queue / go to album / go to artist / add to
//! playlist…), and the trailing heart opens the like picker targeted at
//! the row.
//!
//! Composes the existing single-sources-of-truth:
//! [`crate::views::home::attach_context_menu`] for the gesture and
//! [`crate::views::home::playlist::artist_line`] for the credit spans;
//! [`like_heart`] here is the third shared piece (queue rows use it too).

use std::rc::Rc;

use opal_gfx::{Align, Computed, CursorIcon, ImageHandle, Justify, Len, Overflow, Scene, Signal};

use crate::api::PlaylistTrack;
use crate::model::MenuTarget;
use crate::views::MainNav;
use crate::views::home::{CtxMenuFn, NavFn};
use crate::widgets::icon::{Icon, IconSet};
use crate::widgets::tokens as t;

/// Row height (logical px) — the one geometry both the playlist page's
/// column header and every flat list agree on.
pub const ROW_H: f32 = t::SP_14;
/// Bounded width of the album / sources strip. Matches the playlist
/// header's "Album" column so the labels line up; content wider than this
/// marquees instead of truncating.
const ALBUM_W: f32 = t::SP_48;

/// Everything one row needs. Built per rebuild by the surface.
pub struct TrackRow {
    /// 1-based rank shown in the leading gutter; `None` hides it.
    pub index: Option<u32>,
    /// The full track — feeds the context menu + like picker targets.
    pub track: PlaylistTrack,
    /// Resolved cover signal (reactive; `None` = plain placeholder).
    pub cover: Option<Signal<Option<ImageHandle>>>,
    /// Pre-formatted run time ("3:22").
    pub duration: String,
    /// What clicking the row does (play a target, skip the queue, …).
    pub activate: Rc<dyn Fn()>,
    /// Extra sources shown after the album ("Liked Songs • Chill") — the
    /// artist page's "which playlists have this" answer. `None` elsewhere.
    pub sources: Option<String>,
    /// Fills the heart (the row is known to be in the library).
    pub in_library: bool,
    /// Playable — a local/region-blocked track renders dim + inert.
    pub playable: bool,
}

/// Shared emitter bundle — clone-cheap, built once per surface.
pub struct TrackRowActions {
    pub on_context_menu: CtxMenuFn,
    /// Open the like picker targeted at this row (the handler opens the
    /// overlay and resolves membership).
    pub on_like: crate::views::home::LikeForFn,
    /// Artist-span navigation (the credit line).
    pub on_navigate: NavFn,
    pub icons: Rc<IconSet>,
    pub accent: Signal<[f32; 4]>,
}

/// Render one row into `s`. The single row renderer for every flat track
/// list (playlist / liked / album / artist page); the columns —
/// `# · thumb · title+artist · album(+sources) · heart · duration` — line
/// up with the playlist page's sticky column header.
pub fn track_row(s: &mut Scene, row: TrackRow, actions: &TrackRowActions) {
    let activate = row.activate.clone();
    let mut node = s.row(());
    node.w(Len::Fill)
        .h_px(ROW_H)
        .pad_xy(t::SP_3, t::SP_1)
        .gap(t::SP_3)
        .align(Align::Center)
        .radius(t::R_MD);
    if row.playable {
        node.hover_color(t::HOVER_LIFT_SUBTLE)
            .cursor(CursorIcon::Pointer)
            .on_click(move |_| activate());
    } else {
        // Local file / region-blocked: visible so the list reads complete,
        // but faded and inert.
        node.opacity(0.4);
    }
    crate::views::home::attach_context_menu(
        &mut node,
        &actions.on_context_menu,
        MenuTarget::for_track(&row.track),
    );
    node.child(|r| {
        if let Some(i) = row.index {
            r.row(()).w_px(t::SP_7).center().child(|c| {
                c.text((), format!("{i}"), 13.0).color(t::TEXT_DIM);
            });
        }
        r.col(()).w_px(t::THUMB_SM).h_px(t::THUMB_SM).child(|b| {
            if let Some(sig) = row.cover.clone() {
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
        r.col(())
            .w(Len::Fill)
            .h(Len::Fill)
            .gap(t::SP_0_5)
            .justify(Justify::Center)
            .overflow_x(Overflow::Hidden)
            .child(|m| {
                m.text((), row.track.name.clone(), 14.0)
                    .color(t::TEXT)
                    .max_width_px(360.0);
                crate::views::home::playlist::artist_line(
                    m,
                    &row.track.artists,
                    &row.track.artist,
                    &row.track.artist_id,
                    &actions.on_navigate,
                    360.0,
                );
            });
        // Album (+ sources) — its own bounded column, marqueeing only when
        // the combined text overflows (album alone usually fits; album plus
        // the artist page's playlist sources usually doesn't).
        let album = row.track.album.clone();
        let album_id = row.track.album_id.clone();
        let sources = row.sources.clone();
        let nav = actions.on_navigate.clone();
        r.marquee((), move |strip| {
            if album_id.is_empty() {
                strip.text((), album, 12.0).color(t::TEXT_DIM);
            } else {
                strip
                    .text((), album, 12.0)
                    .color(t::TEXT_DIM)
                    .cursor(CursorIcon::Pointer)
                    .hover_color(t::TEXT)
                    .on_click(move |ctx| {
                        nav(
                            ctx,
                            MainNav::Album {
                                id: album_id.clone(),
                            },
                        )
                    });
            }
            if let Some(src) = sources {
                strip
                    .text((), format!("  \u{2022}  {src}"), 12.0)
                    .color(t::TEXT_DIM);
            }
        })
        .w_px(ALBUM_W)
        .h(Len::Fill)
        .align(Align::Center);
        // Heart — opens the like picker targeted at this row.
        like_heart(
            r,
            &actions.icons,
            &actions.accent,
            row.track.clone(),
            row.in_library,
            actions.on_like.clone(),
        );
        // Duration.
        r.row(()).w_px(t::SP_12).justify(Justify::End).child(|c| {
            c.text((), row.duration.clone(), 12.0).color(t::TEXT_DIM);
        });
    });
}

/// The row heart — opens the like picker targeted at `track`. Filled
/// hearts wear the accent outright; outlines lift dim → accent on hover.
/// Shared by [`track_row`] and the queue's rows so the affordance is the
/// same everywhere.
pub fn like_heart(
    s: &mut Scene,
    icons: &Rc<IconSet>,
    accent: &Signal<[f32; 4]>,
    track: PlaylistTrack,
    filled: bool,
    on_like: crate::views::home::LikeForFn,
) {
    let hovered = Signal::new(false);
    let tint = Computed::new((hovered.clone(), accent.clone()), move |(h, acc)| {
        if filled || h { acc } else { t::TEXT_DIM }
    });
    let glyph = if filled {
        Icon::HeartFilled
    } else {
        Icon::Heart
    };
    let icons = icons.clone();
    s.row(())
        .w_px(t::SP_7)
        .h_px(t::SP_7)
        .center()
        .cursor(CursorIcon::Pointer)
        .on_hover(hovered)
        .on_click(move |ctx| on_like(ctx, track.clone()))
        .child(|c| {
            icons.render(c, glyph, t::ICON_SM, tint.clone());
        });
}
