//! Left sidebar — "Your Library", a [`Component`].
//!
//! Reads the library's playlist list + the shared art cache, the live
//! accent (filter chips), the current nav (row selection), and the
//! resizable width; raises nav intents through `on_navigate`. Collapses
//! to an icon-only rail as the splitter drags the width down.

use std::rc::Rc;

use opal_gfx::{Align, Computed, ImageHandle, Justify, Len, Overflow, Scene, Signal};

use crate::album_art;
use crate::api::{HomeData, LIKED_SONGS_ID};
use crate::model::ArtModel;
use crate::views::MainNav;
use crate::views::home::NavFn;
use crate::widgets::chip::chip;
use crate::widgets::component::Component;
use crate::widgets::icon::{Icon, IconSet};
use crate::widgets::thumb::thumb;
use crate::widgets::tokens as t;

pub struct Sidebar<'a> {
    pub width: &'a Signal<f32>,
    pub accent: &'a Signal<[f32; 4]>,
    pub nav: &'a MainNav,
    pub on_navigate: NavFn,
    pub home: &'a HomeData,
    /// The art model — narrow per-row lookups (`art.signal(key)`); no held
    /// `home_art` borrow.
    pub art: &'a ArtModel,
    pub icons: &'a Rc<IconSet>,
}

impl Component for Sidebar<'_> {
    fn view(&self, s: &mut Scene) {
        let w = self.width;
        let icons = self.icons;
        s.col("sidebar")
            .width_px_bind(w.clone())
            .h(Len::Fill)
            .rgba(t::PANEL[0], t::PANEL[1], t::PANEL[2], 0.75)
            .radius(t::R_LG)
            // Clip horizontally so the fixed-width children (thumb, header
            // text, chips) don't paint past the panel edge when the
            // splitter drags the width below their natural size.
            .overflow_x(Overflow::Hidden)
            .child(|c| {
                // Header — collapses to 0 height in icon-only mode so "Your
                // Library" doesn't truncate. The Home icon+label is a click
                // target back to the Home feed.
                c.row(())
                    .w(Len::Fill)
                    .height_px_bind(collapsed_height(w, t::ROW_H_LG))
                    .pad_xy(t::SP_4, t::SP_0)
                    .gap(t::SP_2)
                    .align(Align::Center)
                    .overflow_y(Overflow::Hidden)
                    .child(|h| {
                        let nav = self.on_navigate.clone();
                        h.row(())
                            .h(Len::Fill)
                            .gap(t::SP_2)
                            .align(Align::Center)
                            .hover_opacity(0.7)
                            .on_click(move |ctx| nav(ctx, MainNav::Home))
                            .child(|hl| {
                                icons.render(hl, Icon::Home, t::ICON_MD, t::TEXT);
                                hl.text((), "Your Library", 14.0).color(t::TEXT);
                            });
                        h.row(()).push_end().child(|r| {
                            icons.render(r, Icon::Plus, t::ICON_MD, t::TEXT_DIM);
                        });
                    });
                // Library filter chips — same collapse behavior.
                c.row(())
                    .w(Len::Fill)
                    .height_px_bind(collapsed_height(w, t::SP_11))
                    .pad_xy(t::SP_3, t::SP_0)
                    .gap(t::SP_2)
                    .align(Align::Center)
                    .overflow_y(Overflow::Hidden)
                    .child(|chips| {
                        chip(chips, "Playlists", true, self.accent);
                        chip(chips, "Artists", false, self.accent);
                        chip(chips, "Albums", false, self.accent);
                    });
                c.col(())
                    .w(Len::Fill)
                    .h(Len::Fill)
                    .pad_xy(t::SP_1_5, t::SP_1_5)
                    .gap(t::SP_1)
                    .scroll_y()
                    // Compositor scroll layer: the library list rasters once
                    // into a content-sized texture; scrolling moves the
                    // composite window, not the rows. Glass-free.
                    .layer()
                    // Auto-hide so the collapsed sidebar's right edge reads
                    // as a clean panel border, not a reserved scroll gutter.
                    .scrollbar(|s| s.auto_hide(true).margin(t::SP_0_5).thickness(t::SP_1))
                    .child(|c| {
                        // Liked Songs — pinned first. Spotify doesn't surface
                        // the saved-tracks collection via /me/playlists, so
                        // it's synthesised here.
                        library_row(
                            c,
                            icons,
                            "Liked Songs",
                            "Playlist",
                            None,
                            true,
                            nav_is(self.nav, LIKED_SONGS_ID),
                            w,
                            MainNav::Playlist {
                                id: LIKED_SONGS_ID.to_string(),
                                liked: true,
                            },
                            &self.on_navigate,
                        );
                        for p in &self.home.playlists {
                            // Sidebar icons use the tiny (64 px) cover tier;
                            // the home tile uses full-res — distinct scdn key,
                            // so both coexist in the art map.
                            let sig = p
                                .image_url_small
                                .as_ref()
                                .and_then(|u| self.art.signal(&album_art::cache_key(u)));
                            library_row(
                                c,
                                icons,
                                &p.name,
                                "Playlist",
                                sig,
                                false,
                                nav_is(self.nav, &p.id),
                                w,
                                MainNav::Playlist {
                                    id: p.id.clone(),
                                    liked: false,
                                },
                                &self.on_navigate,
                            );
                        }
                    });
            });
    }
}

/// Is the centre pane currently showing the playlist with this `id`?
fn nav_is(nav: &MainNav, id: &str) -> bool {
    matches!(nav, MainNav::Playlist { id: nid, .. } if nid == id)
}

/// Reactive height for the header + chip rows. Collapses to 0 in icon-only
/// mode (combined with `.overflow_y(Hidden)` so children get clipped out
/// of the 0-px box instead of painting past).
fn collapsed_height(sidebar_w: &Signal<f32>, expanded: f32) -> Computed<f32> {
    Computed::new((sidebar_w.clone(),), move |(w,)| {
        if w < t::SIDEBAR_COLLAPSE_THRESHOLD {
            0.0
        } else {
            expanded
        }
    })
}

/// Reactive spacer between thumb and text in a library row — the
/// gap-replacement: the row uses `gap=0` so the spacer can shrink to 0 px
/// when collapsed, putting the thumb against `pad_x` with no trailing dead
/// space (a real `gap()` is fixed at layout time and would reserve it).
fn collapsed_spacer(sidebar_w: &Signal<f32>) -> Computed<f32> {
    Computed::new((sidebar_w.clone(),), move |(w,)| {
        if w < t::SIDEBAR_COLLAPSE_THRESHOLD {
            0.0
        } else {
            t::SIDEBAR_TEXT_SPACER
        }
    })
}

/// Reactive width for the library-row text column. Goes to 0 collapsed;
/// otherwise allocates whatever's left after the row chrome (nested
/// paddings + thumb + spacer, encoded in `SIDEBAR_TEXT_CHROME`).
fn collapsed_text_width(sidebar_w: &Signal<f32>) -> Computed<f32> {
    Computed::new((sidebar_w.clone(),), move |(w,)| {
        if w < t::SIDEBAR_COLLAPSE_THRESHOLD {
            0.0
        } else {
            (w - t::SIDEBAR_TEXT_CHROME).max(t::SP_8)
        }
    })
}

#[allow(clippy::too_many_arguments)]
fn library_row(
    s: &mut Scene,
    icons: &IconSet,
    title: &str,
    subtitle: &str,
    art: Option<Signal<Option<ImageHandle>>>,
    liked: bool,
    selected: bool,
    sidebar_w: &Signal<f32>,
    nav_target: MainNav,
    on_navigate: &NavFn,
) {
    let nav = on_navigate.clone();
    let mut row = s.row(());
    row.w(Len::Fill)
        .h_px(t::SP_16)
        .pad_xy(t::SP_1_5, t::SP_1_5)
        .gap(t::SP_0)
        .align(Align::Center)
        .radius(t::R_MD)
        .on_click(move |ctx| nav(ctx, nav_target.clone()));
    // Selected row sits on the panel-highlight fill; others stay
    // transparent and just lift on hover.
    if selected {
        row.rgba(t::PANEL_HI[0], t::PANEL_HI[1], t::PANEL_HI[2], 1.0);
    } else {
        row.hover_color(t::HOVER_LIFT_SUBTLE);
    }
    // Tooltip with the full name — the whole point when collapsed (icon only,
    // no label), and still handy expanded when a long name truncates.
    row.hover_hint(title);
    row.child(|r| {
        if liked {
            liked_thumb(r, icons, t::THUMB_LG, t::R_SM);
        } else {
            thumb(r, art, t::THUMB_LG, t::R_SM);
        }
        // Reactive spacer — replaces `gap` so the trailing space vanishes
        // when collapsed (see `collapsed_spacer`).
        r.rect(())
            .width_px_bind(collapsed_spacer(sidebar_w))
            .h_px(t::SP_PX)
            .rgba(0.0, 0.0, 0.0, 0.0);
        // Text col — width also collapses to 0 so the row's natural size
        // matches `SIDEBAR_COLLAPSED` exactly; overflow_x clips the
        // still-laid-out glyphs out of the 0-width box.
        r.col(())
            .gap(t::SP_0_5)
            .h(Len::Fill)
            .justify(Justify::Center)
            .width_px_bind(collapsed_text_width(sidebar_w))
            .overflow_x(Overflow::Hidden)
            .child(|m| {
                m.text((), title, 13.0).color(t::TEXT).max_width_px(240.0);
                m.text((), subtitle, 11.0).color(t::TEXT_DIM);
            });
    });
}

/// The signature purple Liked-Songs tile (a gradient stand-in + heart).
fn liked_thumb(s: &mut Scene, icons: &IconSet, size: f32, radius: f32) {
    crate::widgets::thumb::liked_tile(s, icons, size, radius, t::ICON_SM);
}
