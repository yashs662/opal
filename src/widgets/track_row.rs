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
    /// Sources line ("Liked Songs • Chill"), dim at the trailing edge —
    /// the artist page's "which playlists have this" answer.
    pub sources: Option<String>,
    /// Fills the heart (the row is known to be in the library).
    pub in_library: bool,
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

/// Render one row into `s`.
pub fn track_row(s: &mut Scene, row: TrackRow, actions: &TrackRowActions) {
    let activate = row.activate.clone();
    let mut node = s.row(());
    node.w(Len::Fill)
        .h_px(t::SP_12)
        .pad_xy(t::SP_2, t::SP_1)
        .gap(t::SP_3)
        .align(Align::Center)
        .radius(t::R_MD)
        .hover_color(t::HOVER_LIFT_SUBTLE)
        .cursor(CursorIcon::Pointer)
        .on_click(move |_| activate());
    crate::views::home::attach_context_menu(
        &mut node,
        &actions.on_context_menu,
        MenuTarget {
            uri: row.track.uri.clone(),
            album_id: row.track.album_id.clone(),
            artist_id: row.track.artist_id.clone(),
            track: Some(Box::new(row.track.clone())),
        },
    );
    node.child(|r| {
        if let Some(i) = row.index {
            r.row(()).w_px(t::SP_6).center().child(|c| {
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
        r.col(())
            .w(Len::Fill)
            .h(Len::Fill)
            .gap(t::SP_0_5)
            .justify(Justify::Center)
            .overflow_x(Overflow::Hidden)
            .child(|m| {
                m.text((), row.track.name.clone(), 14.0)
                    .color(t::TEXT)
                    .max_width_px(420.0);
                crate::views::home::playlist::artist_line(
                    m,
                    &row.track.artists,
                    &row.track.artist,
                    &row.track.artist_id,
                    &actions.on_navigate,
                    420.0,
                );
                if row.track.album_id.is_empty() {
                    m.text((), row.track.album.clone(), 12.0)
                        .color(t::TEXT_DIM)
                        .max_width_px(420.0);
                } else {
                    let nav = actions.on_navigate.clone();
                    let id = row.track.album_id.clone();
                    m.text((), row.track.album.clone(), 12.0)
                        .color(t::TEXT_DIM)
                        .max_width_px(420.0)
                        .cursor(CursorIcon::Pointer)
                        .hover_color(t::TEXT)
                        .on_click(move |ctx| nav(ctx, MainNav::Album { id: id.clone() }));
                }
            });
        // Trailing group: sources • heart • duration, pinned together so
        // the columns line up across rows.
        r.row(())
            .push_end()
            .gap(t::SP_3)
            .align(Align::Center)
            .child(|end| {
                if let Some(src) = row.sources.clone() {
                    end.text((), src, 11.0)
                        .color(t::TEXT_DIM)
                        .max_width_px(220.0);
                }
                like_heart(
                    end,
                    &actions.icons,
                    &actions.accent,
                    row.track.clone(),
                    row.in_library,
                    actions.on_like.clone(),
                );
                end.row(())
                    .w_px(t::SP_10)
                    .justify(Justify::End)
                    .child(|c| {
                        c.text((), row.duration.clone(), 12.0).color(t::TEXT_DIM);
                    });
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
    let glyph = if filled { Icon::HeartFilled } else { Icon::Heart };
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
