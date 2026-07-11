//! Queue page rendered into the centre pane — the active device's play
//! queue: the playing track under a "Now playing" header, then "Next up".
//!
//! Live state, refetched on every open (no cache — it changes with every
//! skip/enqueue). Bounded (Spotify returns ~20 entries), so a plain
//! `scroll_y` column. Works whichever device is active: Spirc publishes
//! Opal's own queue to connect-state, and `/me/player/queue` reads
//! from there.

use std::rc::Rc;

use opal_gfx::{Align, CursorIcon, Justify, Len, Overflow, Scene, Signal};

use crate::api::PlaylistTrack;
use crate::model::ArtModel;
use crate::views::MainNav;
use crate::views::home::{CtxMenuFn, NavFn};
use crate::widgets::icon::{Icon, IconSet};
use crate::widgets::tokens as t;

/// Full-width row height (matches the show-all rows).
const ROW_H: f32 = t::SP_14;

/// Render the queue page into `s` (the caller's transition wrapper).
/// `queue = None` → still loading (pulsing placeholder rows). `on_skip(n)`
/// skips forward `n` tracks — clicking the N-th up-next row plays it
/// (consuming the ones before it, like Spotify's own "click in queue").
#[allow(clippy::too_many_arguments)]
pub fn view(
    s: &mut Scene,
    icons: &Rc<IconSet>,
    queue: Option<&[PlaylistTrack]>,
    art: &ArtModel,
    pulse: &Signal<f32>,
    on_navigate: NavFn,
    on_skip: Rc<dyn Fn(u32)>,
    on_context_menu: CtxMenuFn,
) {
    let nav_back = on_navigate.clone();
    let nav_rows = on_navigate.clone();
    s.col("queue_scroll")
        .w(Len::Fill)
        .h(Len::Fill)
        // Bottom inset matches the sides so the last row scrolls clear of
        // the pane edge instead of sitting flush against it.
        .pad_ltrb(t::SP_6, t::SP_2, t::SP_6, t::SP_6)
        .gap(t::SP_3)
        .scroll_y()
        .layer()
        .scrollbar(|sb| sb.auto_hide(true).margin(t::SP_0_5).thickness(t::SP_1))
        .child(move |c| {
            // Top bar: back chevron + title.
            c.row(())
                .w_px(t::TOPBAR_BTN)
                .h_px(t::TOPBAR_BTN)
                .rgba(0.0, 0.0, 0.0, 0.30)
                .hover_color(t::PANEL_HI)
                .radius(t::R_FULL)
                .center()
                .on_click(move |ctx| nav_back(ctx, MainNav::Home))
                .child(|b| icons.render(b, Icon::ChevronLeft, t::ICON_MD, t::TEXT));
            c.text((), "Queue", 28.0).color(t::TEXT).max_width_px(520.0);

            match queue {
                None => {
                    // Loading — a few pulsing placeholder rows.
                    for _ in 0..6 {
                        skeleton_row(c, pulse);
                    }
                }
                Some([]) => {
                    c.row(()).w(Len::Fill).h_px(ROW_H).center().child(|e| {
                        e.text((), "Nothing queued", 14.0).color(t::TEXT_DIM);
                    });
                }
                Some(tracks) => {
                    let mut it = tracks.iter().enumerate();
                    if let Some((_, now)) = it.next() {
                        section_label(c, "Now playing");
                        queue_row(c, now, art, None, &on_context_menu, &nav_rows);
                    }
                    section_label(c, "Next up");
                    for (i, tr) in it {
                        // Clicking the i-th item (1-based from the playing
                        // track) skips forward `i` tracks to reach it.
                        let on_skip = on_skip.clone();
                        let n = i as u32;
                        queue_row(
                            c,
                            tr,
                            art,
                            Some(Rc::new(move || on_skip(n))),
                            &on_context_menu,
                            &nav_rows,
                        );
                    }
                }
            }
        });
}

fn section_label(s: &mut Scene, label: &str) {
    s.row(())
        .w(Len::Fill)
        .h_px(t::SP_8)
        .align(Align::End)
        .child(|r| {
            r.text((), label, 16.0).color(t::TEXT);
        });
}

/// One queue row: thumb + title/artist + duration. Up-next rows are
/// clickable (`on_click` skips to them); all rows render at full opacity.
fn queue_row(
    s: &mut Scene,
    tr: &PlaylistTrack,
    art: &ArtModel,
    on_click: Option<Rc<dyn Fn()>>,
    on_context_menu: &CtxMenuFn,
    on_navigate: &NavFn,
) {
    // Signals exist (created + dispatched in the reducer's `QueueLoaded`
    // arm — view builds stay pure reads); this just binds them.
    let cover = tr
        .album_image_url
        .as_ref()
        .and_then(|u| art.signal(&crate::album_art::cache_key(u)));
    let mut row = s.row(());
    row.w(Len::Fill)
        .h_px(ROW_H)
        .pad_xy(t::SP_2, t::SP_1)
        .gap(t::SP_3)
        .align(Align::Center)
        .radius(t::R_MD);
    if let Some(f) = on_click {
        row.cursor(CursorIcon::Pointer)
            .hover_color(t::HOVER_LIFT_SUBTLE)
            .on_click(move |_| f());
    }
    // Right-click → context menu (Add to queue / Go to album / artist).
    crate::views::home::attach_context_menu(
        &mut row,
        on_context_menu,
        crate::model::MenuTarget {
            uri: tr.uri.clone(),
            album_id: tr.album_id.clone(),
            artist_id: tr.artist_id.clone(),
        },
    );
    row.child(|r| {
        r.col(()).w_px(t::THUMB_MD).h_px(t::THUMB_MD).child(|b| {
            if let Some(sig) = cover {
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
                m.text((), &tr.name, 14.0).color(t::TEXT).max_width_px(420.0);
                crate::views::home::playlist::artist_line(
                    m,
                    &tr.artists,
                    &tr.artist,
                    on_navigate,
                    420.0,
                );
            });
        r.row(()).push_end().w_px(t::SP_12).justify(Justify::End).child(|d| {
            d.text((), crate::views::home::playlist::fmt_duration(tr.duration_ms), 12.0)
                .color(t::TEXT_DIM);
        });
    });
}

fn skeleton_row(s: &mut Scene, pulse: &Signal<f32>) {
    s.row(())
        .w(Len::Fill)
        .h_px(ROW_H)
        .pad_xy(t::SP_2, t::SP_1)
        .gap(t::SP_3)
        .align(Align::Center)
        .opacity_bind(pulse.clone())
        .child(|row| {
            row.rect(())
                .w_px(t::THUMB_MD)
                .h_px(t::THUMB_MD)
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
