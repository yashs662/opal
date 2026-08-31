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

use crate::api::{PlaylistTrack, QueueEntry};
use crate::model::ArtModel;
use crate::views::home::{CtxMenuFn, NavFn};
use crate::widgets::icon::IconSet;
use crate::widgets::thumb::thumb;
use crate::widgets::tokens as t;

/// Full-width row height (matches the show-all rows).
const ROW_H: f32 = t::SP_14;

/// Render the queue page into `s` (the caller's transition wrapper).
/// `queue = None` → still loading (pulsing placeholder rows).
/// `on_queue_jump(i)` plays the i-th entry, consuming the ones before it
/// — Spotify's own "click in queue" semantics.
#[allow(clippy::too_many_arguments)]
pub fn view(
    s: &mut Scene,
    icons: &Rc<IconSet>,
    queue: Option<&[QueueEntry]>,
    membership: &crate::model::MembershipModel,
    art: &ArtModel,
    pulse: &Signal<f32>,
    on_navigate: NavFn,
    on_queue_jump: Rc<dyn Fn(usize)>,
    on_context_menu: CtxMenuFn,
    on_like: crate::views::home::LikeForFn,
    accent: Signal<[f32; 4]>,
) {
    let nav_rows = on_navigate.clone();
    s.col("queue_scroll")
        .w(Len::Fill)
        .h(Len::Fill)
        // SP_3 side gutter + SP_3 on every heading/row = SP_6 content
        // inset all round: titles, headings and row *content* share one
        // 24px edge, and a row's hover pill overhangs into the gutter
        // instead of pushing its content further in. Top matches the
        // sides (it used to be SP_2 — cramped under the top bar).
        .pad_ltrb(t::SP_3, t::SP_6, t::SP_3, t::SP_6)
        .gap(t::SP_3)
        .scroll_y()
        .layer()
        .scrollbar(|sb| sb.auto_hide(true).margin(t::SP_0_5).thickness(t::SP_1))
        .child(move |c| {
            // (Back = the top-bar history arrows.)
            c.row(()).w(Len::Fill).pad_xy(t::SP_3, t::SP_0).child(|r| {
                r.text((), "Queue", 28.0).color(t::TEXT).max_width_px(520.0);
            });

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
                Some(entries) => {
                    let mut it = entries.iter().enumerate();
                    if let Some((_, now)) = it.next() {
                        section_label(c, "Now playing");
                        queue_row(
                            c,
                            icons,
                            &now.track,
                            membership,
                            art,
                            None,
                            &on_context_menu,
                            &nav_rows,
                            &on_like,
                            &accent,
                            true,
                        );
                    }
                    section_label(c, "Next up");
                    for (i, e) in it {
                        let on_queue_jump = on_queue_jump.clone();
                        queue_row(
                            c,
                            icons,
                            &e.track,
                            membership,
                            art,
                            Some(Rc::new(move || on_queue_jump(i))),
                            &on_context_menu,
                            &nav_rows,
                            &on_like,
                            &accent,
                            false,
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
        .pad_xy(t::SP_3, t::SP_0)
        .align(Align::End)
        .child(|r| {
            r.text((), label, 16.0).color(t::TEXT);
        });
}

/// One queue row: thumb + title/artist + heart + duration. Up-next rows
/// are clickable (`on_click` jumps to them); all rows render at full
/// opacity.
#[allow(clippy::too_many_arguments)]
fn queue_row(
    s: &mut Scene,
    icons: &Rc<IconSet>,
    tr: &PlaylistTrack,
    membership: &crate::model::MembershipModel,
    art: &ArtModel,
    on_click: Option<Rc<dyn Fn()>>,
    on_context_menu: &CtxMenuFn,
    on_navigate: &NavFn,
    on_like: &crate::views::home::LikeForFn,
    accent: &Signal<[f32; 4]>,
    // `now_playing`: this row is the playing track (the "Now playing"
    // entry) — same animated field it gets in every other list.
    now_playing: bool,
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
        .pad_xy(t::SP_3, t::SP_1)
        .gap(t::SP_3)
        .align(Align::Center)
        .radius(t::R_MD);
    if let Some(f) = on_click {
        row.cursor(CursorIcon::Pointer)
            .hover_color(t::HOVER_LIFT_SUBTLE)
            .on_click(move |_| f());
    }
    // Right-click → the shared context menu, with the full row so "Add
    // to playlist…" shows too.
    crate::views::home::attach_context_menu(
        &mut row,
        on_context_menu,
        crate::model::MenuTarget::for_track(tr),
    );
    let uri = tr.uri.clone();
    let accent_field = accent.clone();
    row.child(move |r| {
        if now_playing {
            crate::widgets::track_row::now_playing_field(r, &uri, &accent_field);
        }
        thumb(r, cover, t::THUMB_MD, t::R_SM);
        r.col(())
            .w(Len::Fill)
            .h(Len::Fill)
            .gap(t::SP_0_5)
            .justify(Justify::Center)
            .overflow_x(Overflow::Hidden)
            .child(|m| {
                m.text((), &tr.name, 14.0)
                    .color(t::TEXT)
                    .max_width_px(420.0);
                crate::views::home::playlist::artist_line(
                    m,
                    &tr.artists,
                    &tr.artist,
                    &tr.artist_id,
                    on_navigate,
                    420.0,
                );
            });
        // Trailing group: heart (opens the like picker for this row) +
        // duration — the shared affordance every flat list carries.
        let heart_track = tr.clone();
        // Same `is_saved` (liked ∪ playlist membership) every other list's
        // heart reads — the queue used to hardcode "unsaved".
        let saved = membership.is_saved(&tr.uri);
        let duration = crate::views::home::playlist::fmt_duration(tr.duration_ms);
        let icons = icons.clone();
        let accent = accent.clone();
        let on_like = on_like.clone();
        r.row(())
            .push_end()
            .gap(t::SP_3)
            .align(Align::Center)
            .child(move |end| {
                crate::widgets::track_row::like_heart(
                    end,
                    &icons,
                    &accent,
                    heart_track,
                    saved,
                    on_like,
                );
                end.row(()).w_px(t::SP_10).justify(Justify::End).child(|d| {
                    d.text((), duration, 12.0).color(t::TEXT_DIM);
                });
            });
    });
}

fn skeleton_row(s: &mut Scene, pulse: &Signal<f32>) {
    s.row(())
        .w(Len::Fill)
        .h_px(ROW_H)
        .pad_xy(t::SP_3, t::SP_1)
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
