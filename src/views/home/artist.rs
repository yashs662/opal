//! Artist page rendered into the centre pane — a hero (circular artist
//! image + name) over a horizontally-scrolling discography of album tiles.
//!
//! Lighter than the playlist/album pages: artists have a bounded album
//! count, so this is a plain `scroll_y` column (no virtualised list) and
//! no collapsing header. Each album tile opens its [`MainNav::Album`] page,
//! reusing [`crate::views::home::main_pane::tile`].

use std::rc::Rc;

use opal_gfx::{Align, ImageHandle, Justify, Len, Overflow, Scene, Signal};

use crate::api::PlayTarget;
use crate::views::MainNav;
use crate::views::home::{NavFn, PlayFn};
use crate::widgets::icon::{Icon, IconSet};
use crate::widgets::tokens as t;

/// One discography tile — title, year, resolved cover, album id.
pub struct ArtistAlbumTile {
    pub name: String,
    pub year: String,
    pub cover: Option<Signal<Option<ImageHandle>>>,
    pub id: String,
}

/// One "Popular" track row — title, resolved cover, run time, playback URI.
pub struct ArtistTrack {
    pub title: String,
    pub cover: Option<Signal<Option<ImageHandle>>>,
    pub duration: String,
    pub uri: String,
}

/// Everything the artist page needs for one render. Built per rebuild from
/// `library.open_artist`.
pub struct ArtistViewData {
    pub name: String,
    pub image: Option<Signal<Option<ImageHandle>>>,
    pub followers: u64,
    pub loading: bool,
    pub popular: Vec<ArtistTrack>,
    /// The user's liked songs by this artist (capped) — rendered before
    /// the discography; rows play within the Liked Songs collection
    /// context when one resolved.
    pub liked: Vec<ArtistTrack>,
    /// `spotify:user:{id}:collection` when the profile is known — the
    /// liked rows' playing context.
    pub liked_context: Option<String>,
    pub albums: Vec<ArtistAlbumTile>,
}

/// Render the artist page into `s` (the caller's transition wrapper).
/// `scroll_node` is the content-scoped scroller name (rebuilds preserve
/// scroll by identity; a different artist ⇒ different name ⇒ fresh top).
pub fn view(
    s: &mut Scene,
    icons: &Rc<IconSet>,
    data: &ArtistViewData,
    scroll_node: &str,
    on_play: PlayFn,
    on_navigate: NavFn,
) {
    let nav_back = on_navigate.clone();
    s.col(scroll_node)
        .w(Len::Fill)
        .h(Len::Fill)
        // Bottom inset matches the sides — see the queue scroller.
        .pad_ltrb(t::SP_6, t::SP_2, t::SP_6, t::SP_6)
        .gap(t::SP_5)
        .scroll_y()
        .layer()
        .scrollbar(|sb| sb.auto_hide(true).margin(t::SP_0_5).thickness(t::SP_1))
        .child(move |c| {
            // Back chevron.
            c.row(())
                .w_px(t::TOPBAR_BTN)
                .h_px(t::TOPBAR_BTN)
                .rgba(0.0, 0.0, 0.0, 0.30)
                .hover_color(t::PANEL_HI)
                .radius(t::R_FULL)
                .center()
                .on_click(move |ctx| nav_back(ctx, MainNav::Home))
                .child(|b| icons.render(b, Icon::ChevronLeft, t::ICON_MD, t::TEXT));

            // Hero: circular artist image + name.
            c.row(())
                .w(Len::Fill)
                .h_px(t::SP_44)
                .gap(t::SP_5)
                .align(Align::End)
                .child(|hero| {
                    hero.col(())
                        .w_px(t::THUMB_2XL)
                        .h_px(t::THUMB_2XL)
                        .child(|b| {
                            if let Some(sig) = data.image.clone() {
                                b.image_bound((), sig)
                                    .abs(0.0, 0.0)
                                    .w(Len::Fill)
                                    .h(Len::Fill)
                                    .radius(t::R_FULL)
                                    .placeholder_fill(t::PLACEHOLDER);
                            } else {
                                b.rect(())
                                    .abs(0.0, 0.0)
                                    .w(Len::Fill)
                                    .h(Len::Fill)
                                    .rgba(t::PLACEHOLDER[0], t::PLACEHOLDER[1], t::PLACEHOLDER[2], 1.0)
                                    .radius(t::R_FULL);
                            }
                        });
                    hero.col(()).gap(t::SP_2).justify(Justify::End).child(|m| {
                        m.text((), "Artist", 12.0).color(t::TEXT_DIM);
                        let title = if data.name.is_empty() && data.loading {
                            "Loading\u{2026}"
                        } else {
                            &data.name
                        };
                        m.text((), title, 32.0).color(t::TEXT).max_width_px(520.0);
                        if data.followers > 0 {
                            m.text((), fmt_followers(data.followers), 12.0)
                                .color(t::TEXT_DIM);
                        }
                    });
                });

            // Popular tracks.
            if !data.popular.is_empty() {
                c.row(())
                    .w(Len::Fill)
                    .h_px(t::SP_7)
                    .align(Align::Center)
                    .child(|h| {
                        h.text((), "Popular", 18.0).color(t::TEXT);
                    });
                for (i, track) in data.popular.iter().enumerate() {
                    let target = PlayTarget::Uris {
                        uris: vec![track.uri.clone()],
                        offset: 0,
                    };
                    track_row(c, i as u32, track, target, &on_play);
                }
            }

            // The user's liked songs by this artist — the membership
            // graph's view of them, ahead of the discography. Rows play
            // within the Liked Songs collection when its context is
            // known (continuation stays inside the user's library).
            if !data.liked.is_empty() {
                c.row(())
                    .w(Len::Fill)
                    .h_px(t::SP_7)
                    .align(Align::Center)
                    .child(|h| {
                        h.text((), "Liked songs", 18.0).color(t::TEXT);
                    });
                for (i, track) in data.liked.iter().enumerate() {
                    let target = match &data.liked_context {
                        Some(ctx) => PlayTarget::ContextAt {
                            context_uri: ctx.clone(),
                            track_uri: track.uri.clone(),
                        },
                        None => PlayTarget::Uris {
                            uris: vec![track.uri.clone()],
                            offset: 0,
                        },
                    };
                    track_row(c, i as u32, track, target, &on_play);
                }
            }

            // Discography.
            c.row(())
                .w(Len::Fill)
                .h_px(t::SP_7)
                .align(Align::Center)
                .child(|h| {
                    h.text((), "Discography", 18.0).color(t::TEXT);
                });
            if data.albums.is_empty() {
                if !data.loading {
                    c.text((), "No releases", 14.0).color(t::TEXT_DIM);
                }
            } else {
                // Reuse the shared card strip (cover + arrows-on-hover).
                let cards = data
                    .albums
                    .iter()
                    .map(|al| crate::views::home::main_pane::Card {
                        title: al.name.clone(),
                        subtitle: al.year.clone(),
                        cover: al.cover.clone(),
                        target: Some(MainNav::Album { id: al.id.clone() }),
                        menu: None,
                    })
                    .collect();
                crate::views::home::main_pane::card_row(
                    c,
                    icons,
                    on_navigate.clone(),
                    None,
                    None,
                    cards,
                );
            }
        });
}

/// One track row (Popular / Liked songs): rank + thumb + title + run
/// time; clicking plays `target`.
fn track_row(s: &mut Scene, index: u32, track: &ArtistTrack, target: PlayTarget, on_play: &PlayFn) {
    let on_play = on_play.clone();
    s.row(())
        .w(Len::Fill)
        .h_px(t::SP_12)
        .pad_xy(t::SP_2, t::SP_1)
        .gap(t::SP_3)
        .align(Align::Center)
        .radius(t::R_MD)
        .hover_color(t::HOVER_LIFT_SUBTLE)
        .on_click(move |_| on_play(target.clone()))
        .child(|r| {
            r.row(()).w_px(t::SP_6).center().child(|c| {
                c.text((), format!("{}", index + 1), 13.0)
                    .color(t::TEXT_DIM);
            });
            r.col(()).w_px(t::THUMB_SM).h_px(t::THUMB_SM).child(|b| {
                if let Some(sig) = track.cover.clone() {
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
                .justify(Justify::Center)
                .overflow_x(Overflow::Hidden)
                .child(|m| {
                    m.text((), &track.title, 14.0)
                        .color(t::TEXT)
                        .max_width_px(420.0);
                });
            // Run time, right-aligned.
            r.row(())
                .push_end()
                .w_px(t::SP_12)
                .justify(Justify::End)
                .child(|c| {
                    c.text((), &track.duration, 12.0).color(t::TEXT_DIM);
                });
        });
}

/// Compact follower count: "1.2M followers" / "12.3K followers" / "742
/// followers". Shared with the now-playing "About the artist" card.
pub(crate) fn fmt_followers(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M followers", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K followers", n as f64 / 1_000.0)
    } else {
        format!("{n} followers")
    }
}
