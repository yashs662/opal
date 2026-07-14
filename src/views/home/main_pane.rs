//! Centre pane — a [`Component`] that wraps a constant panel around a
//! slide/fade transition layer whose content swaps between the Home feed
//! and a playlist page. The panel itself never transitions — only its
//! inner content — so a nav change reads as the content sliding up +
//! fading in, not the whole pane flickering.

use std::rc::Rc;

use std::time::Duration;

use opal_gfx::{
    Align, Computed, Curve, ImageHandle, Justify, Len, NodeId, Scene, Signal, animated,
};

use crate::album_art;
use crate::api::PlayTarget;
use crate::api::{AlbumRef, HomeData};
use crate::model::ArtModel;
use crate::views::home::playlist::{self, PlaylistViewData};
use crate::views::home::{CtxMenuFn, NavFn, PlayFn};
use crate::views::{HomeSection, MainNav};
use crate::widgets::color::accent_fg;
use crate::widgets::component::Component;
use crate::widgets::icon::{Icon, IconSet};
use crate::widgets::thumb::thumb;
use crate::widgets::tokens as t;

/// Max tiles rendered per home row. The row is a horizontal scroller, so
/// this is sized to overflow the widest screen (≈12 × ~190px ≈ 2280px) —
/// fullscreen fills edge-to-edge with scroll headroom rather than running
/// out after a handful. Fetch limits (worker + recently-played) match.
const HOME_ROW_TILES: usize = 12;

pub struct MainPane<'a> {
    pub icons: &'a Rc<IconSet>,
    pub home: &'a HomeData,
    /// The art model — looked up narrowly per tile (`art.signal(key)`), so
    /// no wide `home_art` borrow is held across the build.
    pub art: &'a ArtModel,
    pub accent: &'a Signal<[f32; 4]>,
    /// What the pane shows (Home feed vs a playlist page).
    pub nav: &'a MainNav,
    /// View data for the open playlist/album (`Some` when `nav` is one).
    pub playlist: Option<&'a PlaylistViewData>,
    /// View data for the open artist page (`Some` when `nav` is an Artist).
    pub artist: Option<&'a crate::views::home::artist::ArtistViewData>,
    /// View data for a "Show all" list (`Some` when `nav` is ShowAll, except
    /// Recently played — see `recents`).
    pub show_all: Option<&'a crate::views::home::show_all::ShowAllViewData>,
    /// Session-grouped Recently-played page (`Some` for that section only).
    pub recents: Option<&'a crate::views::home::recents::RecentsViewData>,
    /// Expand/collapse a Recents session group.
    pub on_toggle_recent: Rc<dyn Fn(String)>,
    /// The active device's queue (`None` while loading; `nav` is Queue).
    pub queue: Option<&'a [crate::api::PlaylistTrack]>,
    /// Skeleton pulse signal (queue loading placeholders).
    pub pulse: &'a Signal<f32>,
    /// Skip forward N tracks — clicking a queue row jumps to it.
    pub on_skip: Rc<dyn Fn(u32)>,
    /// Right-click a track row → open the context menu.
    pub on_context_menu: crate::views::home::CtxMenuFn,
    /// 0 → 1 entrance transition progress on nav change.
    pub main_t: &'a Signal<f32>,
    /// Detail-page header collapse (0 expanded → 1 collapsed), driven each
    /// frame from the scroll offset; slides + fades the sticky bar.
    pub detail_collapse: &'a Signal<f32>,
    pub on_play: PlayFn,
    pub on_navigate: NavFn,
    /// Shared row affordances (context menu / like heart / artist spans)
    /// for the flat track lists (artist page, queue).
    pub row_actions: crate::widgets::track_row::TrackRowActions,
    /// Open the full "in your library by this artist" synthetic page.
    pub on_show_all_library: Rc<dyn Fn()>,
}

impl Component for MainPane<'_> {
    fn view(&self, s: &mut Scene) {
        s.col("main_area")
            .w(Len::Fill)
            .h(Len::Fill)
            .rgba(t::PANEL[0], t::PANEL[1], t::PANEL[2], 0.75)
            .radius(t::R_LG)
            .clip()
            .child(|outer| {
                // Transition wrapper — abs-fill so the slide offset doesn't
                // disturb flow. `main_t` 0→1 drives a subtle upward slide +
                // opacity fade-in on every nav change; steady state parks at
                // offset 0, fully opaque.
                let slide = Computed::new((self.main_t.clone(),), |(tt,)| {
                    [0.0, (1.0 - tt.clamp(0.0, 1.0)) * 14.0]
                });
                let fade = Computed::new((self.main_t.clone(),), |(tt,)| tt.clamp(0.0, 1.0));
                outer
                    .col("main_content")
                    .pos(slide)
                    .w(Len::Fill)
                    .h(Len::Fill)
                    .opacity_bind(fade)
                    .child(|content| match self.nav {
                        MainNav::Home => self.home_feed(content),
                        MainNav::Playlist { .. } | MainNav::Album { .. } => {
                            if let Some(pv) = self.playlist {
                                let scroll_node = self
                                    .nav
                                    .detail_scroll_node()
                                    .expect("detail nav has scroller");
                                playlist::view(
                                    content,
                                    self.icons,
                                    pv,
                                    self.accent,
                                    self.detail_collapse,
                                    &scroll_node,
                                    self.on_play.clone(),
                                    self.on_navigate.clone(),
                                );
                            }
                        }
                        MainNav::Artist { id } => {
                            if let Some(av) = self.artist {
                                crate::views::home::artist::view(
                                    content,
                                    self.icons,
                                    av,
                                    &format!("artist_scroll:{id}"),
                                    self.on_play.clone(),
                                    self.on_navigate.clone(),
                                    &self.row_actions,
                                    self.on_show_all_library.clone(),
                                );
                            }
                        }
                        MainNav::ShowAll { section } => {
                            if let Some(rv) = self.recents {
                                crate::views::home::recents::view(
                                    content,
                                    self.icons,
                                    rv,
                                    &format!("show_all_scroll:{section:?}"),
                                    self.on_navigate.clone(),
                                    self.on_play.clone(),
                                    self.on_context_menu.clone(),
                                    self.on_toggle_recent.clone(),
                                );
                            } else if let Some(sv) = self.show_all {
                                crate::views::home::show_all::view(
                                    content,
                                    self.icons,
                                    sv,
                                    &format!("show_all_scroll:{section:?}"),
                                    self.on_navigate.clone(),
                                    self.on_play.clone(),
                                    self.on_context_menu.clone(),
                                );
                            }
                        }
                        MainNav::Queue => {
                            crate::views::home::queue::view(
                                content,
                                self.icons,
                                self.queue,
                                self.art,
                                self.pulse,
                                self.on_navigate.clone(),
                                self.on_skip.clone(),
                                self.on_context_menu.clone(),
                                self.row_actions.on_like.clone(),
                                self.row_actions.accent.clone(),
                            );
                        }
                    });
            });
    }
}

impl MainPane<'_> {
    fn home_feed(&self, content: &mut Scene) {
        let icons = self.icons;
        let home = self.home;
        let art = self.art;
        let accent = self.accent;
        let nav = self.on_navigate.clone();
        let ctx_menu = self.on_context_menu.clone();
        let on_play = self.on_play.clone();
        let prefix = greeting_prefix(greeting_bucket());
        let greeting = match home.profile.as_ref() {
            Some(p) if !p.display_name.is_empty() => format!("{prefix}, {}", p.display_name),
            _ => prefix.to_string(),
        };
        // Scrolling content body — all sections hit real endpoints.
        // Named so the rebuild scroll-preservation keys it precisely: the
        // home feed is one stable thing, so its scroll position survives
        // navigating to a detail page and back (and any other rebuild).
        content
            .col("home_feed_scroll")
            .w(Len::Fill)
            .h(Len::Fill)
            .pad(t::SP_6)
            .gap(t::SP_5)
            .scroll_y()
            // Compositor scroll layer: the feed body rasters once into a
            // content-sized texture; scrolling recomposites the window.
            .layer()
            .child(|c| {
                c.text((), greeting, 26.0)
                    .color(t::TEXT)
                    .max_width_px(520.0);

                // Spotlit new release (newest album from #1 top artist).
                if let Some(rel) = home.latest_release.as_ref() {
                    section_header(c, &format!("New release from {}", rel.artist), None, &nav);
                    new_release_card(c, icons, rel, art, accent, nav.clone());
                }

                section_header(
                    c,
                    "Recently played",
                    Some(MainNav::ShowAll {
                        section: HomeSection::Recent,
                    }),
                    &nav,
                );
                tile_row(
                    c,
                    icons,
                    home.recent.iter().take(HOME_ROW_TILES),
                    art,
                    nav.clone(),
                    Some(ctx_menu.clone()),
                    Some(on_play.clone()),
                    |t| {
                        (
                            t.name.clone(),
                            t.artist.clone(),
                            t.album_image_url.clone(),
                            Some(MainNav::Album {
                                id: t.album_id.clone(),
                            }),
                            Some(crate::model::MenuTarget::for_track(&t.to_track())),
                        )
                    },
                );

                section_header(
                    c,
                    "Your top artists",
                    Some(MainNav::ShowAll {
                        section: HomeSection::TopArtists,
                    }),
                    &nav,
                );
                tile_row(
                    c,
                    icons,
                    home.top_artists.iter().take(HOME_ROW_TILES),
                    art,
                    nav.clone(),
                    None,
                    None,
                    |a| {
                        (
                            a.name.clone(),
                            "Artist".to_string(),
                            a.image_url.clone(),
                            Some(MainNav::Artist { id: a.id.clone() }),
                            None,
                        )
                    },
                );

                section_header(
                    c,
                    "Your top tracks",
                    Some(MainNav::ShowAll {
                        section: HomeSection::TopTracks,
                    }),
                    &nav,
                );
                tile_row(
                    c,
                    icons,
                    home.top_tracks.iter().take(HOME_ROW_TILES),
                    art,
                    nav.clone(),
                    Some(ctx_menu.clone()),
                    Some(on_play.clone()),
                    |t| {
                        (
                            t.name.clone(),
                            t.artist.clone(),
                            t.album_image_url.clone(),
                            Some(MainNav::Album {
                                id: t.album_id.clone(),
                            }),
                            Some(crate::model::MenuTarget::for_track(&t.to_track())),
                        )
                    },
                );
            });
    }
}

/// Horizontal strip of tiles with click-to-page arrow overlays. `nav` is
/// the centre-pane navigation callback; each entry's `MainNav` target (if
/// any) is fired on click.
/// One card in a [`card_row`]: cover + title/subtitle, opening `target` on
/// click. The reusable unit shared by the home tile strips + artist
/// discography (DRY).
pub(crate) struct Card {
    pub title: String,
    pub subtitle: String,
    pub cover: Option<Signal<Option<ImageHandle>>>,
    pub target: Option<MainNav>,
    /// Right-click target for song tiles (recents / top tracks). `None`
    /// for non-song tiles (artists, playlists, albums) → no menu.
    pub menu: Option<crate::model::MenuTarget>,
}

/// Adapter: build a [`card_row`] from an iterator + a per-item labeller.
/// Used by the home feed (recents, top artists/tracks, playlists).
#[allow(clippy::type_complexity)]
#[allow(clippy::too_many_arguments)]
fn tile_row<T>(
    s: &mut Scene,
    icons: &Rc<IconSet>,
    items: impl Iterator<Item = T>,
    art: &ArtModel,
    nav: NavFn,
    on_context_menu: Option<CtxMenuFn>,
    on_play: Option<PlayFn>,
    label: impl Fn(
        &T,
    ) -> (
        String,
        String,
        Option<String>,
        Option<MainNav>,
        Option<crate::model::MenuTarget>,
    ),
) {
    let cards: Vec<Card> = items
        .map(|t| {
            let (title, subtitle, url, target, menu) = label(&t);
            let cover = url
                .as_ref()
                .and_then(|u| art.signal(&album_art::cache_key(u)));
            Card {
                title,
                subtitle,
                cover,
                target,
                menu,
            }
        })
        .collect();
    card_row(s, icons, nav, on_context_menu, on_play, cards);
}

/// Logical-px fallback page step when the scroller's measured width isn't
/// available (pre-first-layout). Normally we page by ~80% of the visible
/// width, read live from the node rect.
const SCROLL_PAGE_FALLBACK: f32 = 600.0;

/// Reusable horizontal strip of [`Card`]s with click-to-page arrow bars that
/// reveal on hover. Shared by the home tile rows + the artist discography so
/// the scroller + tiles + arrow affordance live in one place (DRY). Empty
/// `cards` renders skeleton placeholders (loading state).
pub(crate) fn card_row(
    s: &mut Scene,
    icons: &Rc<IconSet>,
    nav: NavFn,
    on_context_menu: Option<CtxMenuFn>,
    on_play: Option<PlayFn>,
    cards: Vec<Card>,
) {
    let icons = icons.clone();
    // Hover state of the whole strip (ancestor-hover: true while the cursor
    // is over any tile/gap/arrow inside it) → drives the arrow bars' fade.
    let hovered = Signal::new(false);
    // Fade the arrow bars in/out on strip hover (animated opacity bind).
    let arrows_vis = animated(
        Computed::new((hovered.clone(),), |(h,)| if h { 1.0 } else { 0.0 }),
        Curve::EaseInOut,
        Duration::from_millis(160),
    );
    s.col(())
        .w(Len::Fill)
        .h_px(t::SP_56)
        .on_hover(hovered)
        .child(move |wrap| {
            let row_id = {
                let mut scroll = wrap.row(());
                scroll.w(Len::Fill).h(Len::Fill).gap(t::SP_3_5).scroll_x();
                let id = scroll.id();
                scroll.child(move |g| {
                    if cards.is_empty() {
                        for _ in 0..8 {
                            tile(g, "\u{2014}", "", None, None, None, &nav, None, None);
                        }
                    } else {
                        for c in &cards {
                            tile(
                                g,
                                &c.title,
                                &c.subtitle,
                                c.cover.clone(),
                                c.target.clone(),
                                c.menu.clone(),
                                &nav,
                                on_context_menu.as_ref(),
                                on_play.as_ref(),
                            );
                        }
                    }
                });
                id
            };
            scroll_arrows(wrap, &icons, row_id, arrows_vis);
        });
}

/// Overlay the two click-to-page arrow bars at the strip's left/right edges.
/// The overlay row has no handler, so it's hit/hover-transparent — tiles
/// beneath stay interactive; only the bars (and the wrapper's `on_hover`)
/// register. `vis` (0..1, from the strip's hover) fades the whole overlay.
fn scroll_arrows(
    s: &mut Scene,
    icons: &IconSet,
    row_id: NodeId,
    vis: impl Into<opal_gfx::Bind<f32>>,
) {
    s.row(())
        .abs(0.0, 0.0)
        .w(Len::Fill)
        .h(Len::Fill)
        .justify(Justify::SpaceBetween)
        .align(Align::Center)
        .opacity_bind(vis)
        .child(|a| {
            arrow_bar(a, icons, Icon::ChevronLeft, row_id, -1.0);
            arrow_bar(a, icons, Icon::ChevronRight, row_id, 1.0);
        });
}

/// One tall vertical arrow bar that springs `row_id` one page in `dir`
/// (-1 = left, +1 = right). Page = 80% of the scroller's live width
/// (physical px), so it matches DPI without the click ctx needing scale.
fn arrow_bar(s: &mut Scene, icons: &IconSet, icon: Icon, row_id: NodeId, dir: f32) {
    s.row(())
        .w_px(t::SP_8)
        .h(Len::Fill)
        .rgba(0.0, 0.0, 0.0, 0.55)
        .hover_color([0.0, 0.0, 0.0, 0.82])
        .radius(t::R_MD)
        .center()
        .on_click(move |ctx| {
            let cur = ctx.tree.scroll_offset(row_id);
            let page = ctx
                .tree
                .get(row_id)
                .map(|n| n.rect[2] * 0.8)
                .filter(|w| *w > 1.0)
                .unwrap_or(SCROLL_PAGE_FALLBACK);
            ctx.tree
                .set_scroll_target(row_id, [cur[0] + dir * page, cur[1]]);
        })
        .child(|c| icons.render(c, icon, t::ICON_MD, t::TEXT));
}

/// Section title plus a "Show all" affordance on the right. When
/// `show_all` is `Some`, the affordance is clickable and opens that
/// section's full-width list; `None` omits it (e.g. the single new-release
/// spotlight, which has nothing to expand).
fn section_header(s: &mut Scene, title: &str, show_all: Option<MainNav>, nav: &NavFn) {
    s.row(())
        .w(Len::Fill)
        .h_px(t::SP_7)
        .align(Align::Center)
        .child(|h| {
            h.text((), title, 18.0).color(t::TEXT);
            if let Some(target) = show_all {
                let nav = nav.clone();
                h.row(())
                    .push_end()
                    .hover_opacity(0.7)
                    .on_click(move |ctx| nav(ctx, target.clone()))
                    .child(|r| {
                        r.text((), "Show all", 12.0).color(t::TEXT_DIM);
                    });
            }
        });
}

/// A single home/discography tile: square cover + title + subtitle, opening
/// `target` on click. Reused by the artist page's discography grid.
#[allow(clippy::too_many_arguments)]
pub(crate) fn tile(
    s: &mut Scene,
    title: &str,
    sub: &str,
    art: Option<Signal<Option<ImageHandle>>>,
    target: Option<MainNav>,
    menu: Option<crate::model::MenuTarget>,
    nav: &NavFn,
    on_context_menu: Option<&CtxMenuFn>,
    on_play: Option<&PlayFn>,
) {
    let mut b = s.col(());
    b.w_px(t::TILE_W)
        .h(Len::Fill)
        // Pad must match (TILE_W - TILE_THUMB)/2 so the square art exactly
        // fills the content box — uniform side margins, and equal to the
        // vertical gap so every edge/gap in the tile is one 8px step.
        .pad(t::SP_2)
        .gap(t::SP_2)
        .rgba(t::PANEL_HI[0], t::PANEL_HI[1], t::PANEL_HI[2], 1.0)
        .hover_color(t::HOVER_LIFT)
        .radius(t::R_LG);
    // A tile that carries a menu target IS a song (recents / top tracks):
    // clicking *plays* it (in its album context so the queue continues),
    // matching the song-row behaviour — not "open the album", which made
    // recents feel like a row of collections. Right-click still offers Go
    // to album. Non-song tiles (artists, playlists, albums) navigate.
    let play_target = match (menu.as_ref(), on_play) {
        (Some(m), Some(_)) if m.album_id.is_empty() => Some(PlayTarget::Uris {
            uris: vec![m.uri.clone()],
            offset: 0,
        }),
        (Some(m), Some(_)) => Some(PlayTarget::ContextAt {
            context_uri: format!("spotify:album:{}", m.album_id),
            track_uri: m.uri.clone(),
        }),
        _ => None,
    };
    if let (Some(pt), Some(on_play)) = (play_target, on_play) {
        let on_play = on_play.clone();
        b.on_click(move |_| on_play(pt.clone()));
    } else if let Some(target) = target {
        let nav = nav.clone();
        b.on_click(move |ctx| nav(ctx, target.clone()));
    }
    // Right-click → context menu (song tiles only).
    if let (Some(menu), Some(on_ctx)) = (menu, on_context_menu) {
        crate::views::home::attach_context_menu(&mut b, on_ctx, menu);
    }
    b.child(|card| {
        card.col(())
            .w_px(t::TILE_THUMB)
            .h_px(t::TILE_THUMB)
            .child(|b| {
                if let Some(sig) = art {
                    b.image_bound((), sig)
                        .abs(0.0, 0.0)
                        .w(Len::Fill)
                        .h(Len::Fill)
                        .radius(t::R_MD)
                        .placeholder_fill(t::PLACEHOLDER);
                } else {
                    b.rect(())
                        .abs(0.0, 0.0)
                        .w(Len::Fill)
                        .h(Len::Fill)
                        .rgba(t::PLACEHOLDER[0], t::PLACEHOLDER[1], t::PLACEHOLDER[2], 1.0)
                        .radius(t::R_MD);
                }
            });
        card.text((), title, 13.0)
            .color(t::TEXT)
            .max_width_px(t::TILE_TEXT_MAX);
        card.text((), sub, 11.0)
            .color(t::TEXT_DIM)
            .max_width_px(t::TILE_TEXT_MAX);
    });
}

/// Wide spotlight card: large art + title/artist + an accent play pill.
/// Clicking the card opens the album's detail page.
fn new_release_card(
    s: &mut Scene,
    icons: &IconSet,
    album: &AlbumRef,
    art: &ArtModel,
    accent: &Signal<[f32; 4]>,
    nav: NavFn,
) {
    let art_sig = album
        .image_url
        .as_ref()
        .and_then(|u| art.signal(&album_art::cache_key(u)));
    let album_id = album.id.clone();
    let mut b = s.row(());
    b.w(Len::Fill)
        .h_px(t::SP_32)
        .pad(t::SP_3_5)
        .gap(t::SP_4)
        .align(Align::Center)
        .rgba(t::PANEL_HI[0], t::PANEL_HI[1], t::PANEL_HI[2], 1.0)
        .hover_color(t::HOVER_LIFT)
        .radius(t::R_2XL)
        .on_click(move |ctx| {
            nav(
                ctx,
                MainNav::Album {
                    id: album_id.clone(),
                },
            )
        });
    b.child(|c| {
        thumb(c, art_sig, t::THUMB_XL, t::R_MD);
        c.col(())
            .h(Len::Fill)
            .gap(t::SP_1)
            .justify(Justify::Center)
            .child(|m| {
                m.text((), &album.release_date, 11.0).color(t::TEXT_DIM);
                m.text((), &album.name, 20.0)
                    .color(t::TEXT)
                    .max_width_px(360.0);
                m.text((), &album.artist, 12.0)
                    .color(t::TEXT_DIM)
                    .max_width_px(360.0);
            });
        c.row(())
            .push_end()
            .w_px(t::BTN_H_LG)
            .h_px(t::BTN_H_LG)
            .center()
            .color(accent.clone())
            .hover_opacity(0.85)
            .radius(t::R_FULL)
            .child(|p| {
                icons.render(p, Icon::Play, t::ICON_MD, accent_fg(accent));
            });
    });
}

/// Time-of-day bucket for the home greeting: 0 = morning, 1 = afternoon,
/// 2 = evening (local time). The frame tick rebuilds the feed when this
/// changes so the greeting stays current across a boundary.
pub(crate) fn greeting_bucket() -> u8 {
    use chrono::Timelike;
    match chrono::Local::now().hour() {
        5..=11 => 0,
        12..=16 => 1,
        _ => 2,
    }
}

/// The greeting text for a [`greeting_bucket`].
fn greeting_prefix(bucket: u8) -> &'static str {
    match bucket {
        0 => "Good morning",
        1 => "Good afternoon",
        _ => "Good evening",
    }
}

/// Seconds until the next greeting boundary (05:00 / 12:00 / 17:00 local) —
/// the background timer sleeps this long, then wakes the loop so the tick
/// re-evaluates the bucket.
pub(crate) fn secs_to_next_greeting_boundary() -> u64 {
    use chrono::Timelike;
    let now = chrono::Local::now();
    let now_s = now.hour() as i64 * 3600 + now.minute() as i64 * 60 + now.second() as i64;
    const BOUNDS: [i64; 3] = [5 * 3600, 12 * 3600, 17 * 3600];
    let next = BOUNDS
        .iter()
        .copied()
        .find(|&b| b > now_s)
        .unwrap_or(BOUNDS[0] + 86_400);
    (next - now_s).max(1) as u64
}
