//! Artist page rendered into the centre pane — a hero (circular artist
//! image + name) over "Popular", the user's own "In your library" saves,
//! and a horizontally-scrolling discography of album tiles.
//!
//! Lighter than the playlist/album pages: artists have a bounded album
//! count, so this is a plain `scroll_y` column (no virtualised list) and
//! no collapsing header. Track rows are the shared
//! [`crate::widgets::track_row`] (play / right-click menu / heart), so
//! the affordances match every other flat list. Each album tile opens
//! its [`MainNav::Album`] page, reusing
//! [`crate::views::home::main_pane::tile`].

use std::rc::Rc;

use opal_gfx::{Align, ImageHandle, Justify, Len, Scene, Signal};

use crate::api::PlayTarget;
use crate::views::MainNav;
use crate::views::home::{NavFn, PlayFn};
use crate::widgets::icon::IconSet;
use crate::widgets::tokens as t;
use crate::widgets::track_row::{TrackRow, TrackRowActions, track_row};

/// How many "In your library" rows the page previews before deferring to
/// the "Show all" synthetic playlist page.
const LIBRARY_PREVIEW: usize = 5;

/// One discography tile — title, year, resolved cover, album id.
pub struct ArtistAlbumTile {
    pub name: String,
    pub year: String,
    pub cover: Option<Signal<Option<ImageHandle>>>,
    pub id: String,
}

/// One track row on the page — the shared row widget's inputs plus the
/// page-level extras (sources, membership).
pub struct ArtistTrackRow {
    pub track: crate::api::PlaylistTrack,
    pub cover: Option<Signal<Option<ImageHandle>>>,
    pub duration: String,
    /// "Liked Songs • Chill" — which library sources hold this track.
    pub sources: Option<String>,
    pub in_library: bool,
}

/// Everything the artist page needs for one render. Built per rebuild from
/// `library.open_artist`.
pub struct ArtistViewData {
    pub name: String,
    pub image: Option<Signal<Option<ImageHandle>>>,
    pub followers: u64,
    pub loading: bool,
    pub popular: Vec<ArtistTrackRow>,
    /// The user's saved songs by this artist across the whole library
    /// (full list — the view previews [`LIBRARY_PREVIEW`]).
    pub library: Vec<ArtistTrackRow>,
    pub albums: Vec<ArtistAlbumTile>,
}

/// Render the artist page into `s` (the caller's transition wrapper).
/// `scroll_node` is the content-scoped scroller name (rebuilds preserve
/// scroll by identity; a different artist ⇒ different name ⇒ fresh top).
#[allow(clippy::too_many_arguments)]
pub fn view(
    s: &mut Scene,
    icons: &Rc<IconSet>,
    data: &ArtistViewData,
    scroll_node: &str,
    on_play: PlayFn,
    on_navigate: NavFn,
    actions: &TrackRowActions,
    on_show_all_library: Rc<dyn Fn()>,
) {
    s.col(scroll_node)
        .w(Len::Fill)
        .h(Len::Fill)
        // Top/bottom insets match the sides so content breathes instead
        // of hugging the pane edges. (Back = the top-bar history arrows.)
        .pad(t::SP_6)
        .gap(t::SP_5)
        .scroll_y()
        .layer()
        .scrollbar(|sb| sb.auto_hide(true).margin(t::SP_0_5).thickness(t::SP_1))
        .child(move |c| {
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
                section_header(c, "Popular", None);
                for (i, row) in data.popular.iter().enumerate() {
                    let uri = row.track.uri.clone();
                    let play = on_play.clone();
                    track_row(
                        c,
                        TrackRow {
                            index: Some(i as u32 + 1),
                            track: row.track.clone(),
                            cover: row.cover.clone(),
                            duration: row.duration.clone(),
                            activate: Rc::new(move || {
                                play(PlayTarget::Uris {
                                    uris: vec![uri.clone()],
                                    offset: 0,
                                })
                            }),
                            sources: row.sources.clone(),
                            in_library: row.in_library,
                        },
                        actions,
                    );
                }
            }

            // The user's saved songs by this artist — the membership
            // graph's view of them, ahead of the discography. Rows play
            // the library set from the clicked row; the full list opens
            // as a synthetic playlist page.
            if !data.library.is_empty() {
                let show_all = (data.library.len() > LIBRARY_PREVIEW)
                    .then(|| on_show_all_library.clone());
                section_header(c, "In your library", show_all);
                let uris: Vec<String> = data
                    .library
                    .iter()
                    .filter(|r| r.track.playable)
                    .map(|r| r.track.uri.clone())
                    .collect();
                for (i, row) in data.library.iter().take(LIBRARY_PREVIEW).enumerate() {
                    let play = on_play.clone();
                    let uris = uris.clone();
                    track_row(
                        c,
                        TrackRow {
                            index: Some(i as u32 + 1),
                            track: row.track.clone(),
                            cover: row.cover.clone(),
                            duration: row.duration.clone(),
                            activate: Rc::new(move || {
                                play(PlayTarget::Uris {
                                    uris: uris.clone(),
                                    offset: i as u32,
                                })
                            }),
                            sources: row.sources.clone(),
                            in_library: row.in_library,
                        },
                        actions,
                    );
                }
            }

            // Discography.
            section_header(c, "Discography", None);
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

/// A section heading with an optional trailing "Show all" action —
/// mirrors the home feed's section headers.
fn section_header(s: &mut Scene, title: &str, show_all: Option<Rc<dyn Fn()>>) {
    let title = title.to_string();
    s.row(())
        .w(Len::Fill)
        .h_px(t::SP_7)
        .align(Align::Center)
        .justify(Justify::SpaceBetween)
        .child(move |h| {
            h.text((), &title, 18.0).color(t::TEXT);
            if let Some(open) = show_all {
                h.text((), "Show all", 12.0)
                    .color(t::TEXT_DIM)
                    .hover_color(t::TEXT)
                    .cursor(opal_gfx::CursorIcon::Pointer)
                    .on_click(move |_| open());
            }
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
