//! Now-playing pane — the first [`Component`].
//!
//! The pane holds the Spotify Canvas aspect: its width is derived from
//! its measured height so the content area is **always 9:16** (the frame
//! tick feeds `np_pane_w`), which is why it isn't user-resizable. Two
//! content modes:
//!
//! - **Canvas playing** — the 9:16 clip is the pane's full-bleed
//!   background (zero layout footprint, exactly filling the 9:16 pane).
//!   The track card peeks above the pane's bottom edge, glassy-dark so it
//!   doesn't compete with the video, blending to the full accent as the
//!   engine's spring scroll + overscroll bounce reveal its body over the
//!   backdrop.
//! - **No canvas** — a plain padded, rounded square cover at the top with
//!   the card directly below it, already in its final accent state (no
//!   peek, no scroll choreography, and no second cover in the body).
//!
//! A hover-revealed arrow (top-right) slide-collapses the whole pane (an
//! open-fraction tween scaling the bound width — no rebuild); the player
//! bar's panel toggle brings it back.

use std::rc::Rc;

use opal_gfx::{Computed, Justify, Len, Lerp, Overflow, Scene, Signal};

use crate::model::{BackdropModel, CanvasModel, PlayerModel};
use crate::widgets::color::{accent_fg_color, accent_hover_color};
use crate::widgets::component::Component;
use crate::widgets::crossfade::crossfaded_art_flat;
use crate::widgets::icon::{Icon, IconSet};
use crate::widgets::tokens as t;

/// How much of the track card peeks above the pane's bottom edge when
/// unscrolled (canvas mode) — just under the accent header's height, so
/// only the header shows until the user scrolls. Read by the frame tick
/// to size the above-card block from the live viewport height.
pub const CARD_PEEK: f32 = 60.0;

/// Scroll distance (logical px) over which the card blends from its
/// resting glassy-dark to the full accent. Read by the frame tick, which
/// derives `np_card_t` from the spring-smoothed scroll offset.
pub const CARD_REVEAL_RANGE: f32 = 150.0;

/// Card-text truncation width — sized for the narrowest sensible pane so
/// titles never collide with the padding regardless of window height.
const CARD_TEXT_MAX: f32 = 280.0;

pub struct NowPlaying<'a> {
    pub backdrop: &'a BackdropModel,
    pub player: &'a PlayerModel,
    pub canvas: &'a CanvasModel,
    /// Cover/image resolution cache — the "About the artist" photo binds
    /// its per-URL signal.
    pub art: &'a crate::model::ArtModel,
    /// Open fraction (0..=1) — the toggle tweens it for the
    /// slide-collapse; it scales the bound pane width, so content stays
    /// at the full width and clips (slides off the right edge instead of
    /// squishing).
    pub open_t: &'a Signal<f32>,
    pub icons: &'a Rc<IconSet>,
    /// Hide the pane (the hover arrow in its top-right corner).
    pub on_toggle: Rc<dyn Fn()>,
    /// Centre-pane navigation — the about-artist name and every credits
    /// row with an artist profile open their artist page through it.
    pub nav: crate::views::home::NavFn,
}

impl Component for NowPlaying<'_> {
    fn view(&self, s: &mut Scene) {
        // Fade rides the slide: group opacity derived from the same width
        // tween, so the pane dissolves as it slips out — and at 0 the
        // subtree goes invisible, skipping render + hit-testing entirely
        // while hidden.
        let pane_w = self.player.np_pane_w.clone();
        let outer_w = Computed::new((self.open_t.clone(), pane_w.clone()), |(k, w)| {
            k.clamp(0.0, 1.0) * (w + t::NOW_PLAYING_GUTTER)
        });
        s.row("now_playing")
            .width_px_bind(outer_w)
            .h(Len::Fill)
            .overflow(Overflow::Hidden, Overflow::Hidden)
            .opacity_bind(self.open_t.clone())
            .child(|outer| {
                // Gutter folded into the animated width so a collapse
                // reaches a true 0 (no orphaned spacing).
                outer.col(()).w_px(t::NOW_PLAYING_GUTTER).h(Len::Fill);
                outer
                    .col(())
                    .width_px_bind(pane_w)
                    .h(Len::Fill)
                    .rgba(t::PANEL[0], t::PANEL[1], t::PANEL[2], 0.75)
                    .radius(t::R_LG)
                    .overflow(Overflow::Hidden, Overflow::Hidden)
                    .on_hover(self.canvas.hover.clone())
                    .child(|pane| {
                        // Painter order: video backdrop behind, scroll
                        // content over it, hide arrow on top.
                        self.canvas_backdrop(pane);
                        self.scroller(pane);
                        self.hide_button(pane);
                    });
            });
    }
}

impl NowPlaying<'_> {
    /// Full-pane Canvas backdrop. The 9:16 clip takes **no layout space** —
    /// it's an absolute background exactly filling the pane (which holds
    /// 9:16 by construction; centre + clip absorb the rounding-remainder
    /// pixels), with the track card scrolling over it. This is also the
    /// permanent home of the `now_playing_canvas` external node, so the
    /// decode thread always has a live target: it renders nothing until
    /// frames land, and the first frame flips [`CanvasModel::tick_active`]
    /// → the cover in the scroll content gives way to the video showing
    /// through.
    fn canvas_backdrop(&self, pane: &mut Scene) {
        // Visibility mirrors `canvas.active` from the frame tick: no live
        // video ⇒ the whole backdrop (external node + dim scrim) is culled,
        // so a stale frame in the GPU registry can never reach the screen.
        pane.col("now_playing_backdrop")
            .abs(0.0, 0.0)
            .visible(self.canvas.active)
            .w(Len::Fill)
            .h(Len::Fill)
            .overflow(Overflow::Hidden, Overflow::Hidden)
            .center()
            .child(|bg| {
                // The external composite scissors to the ancestor overflow
                // rect (the pane) rounded by the node's own radius — that's
                // how the video honours the pane's rounded corners.
                bg.rect("now_playing_canvas")
                    .external()
                    .radius(t::R_LG)
                    .w(Len::Fill)
                    .aspect_ratio(9.0 / 16.0);
                // Dim scrim over the video: dark at rest so the pane
                // doesn't outshine the chrome, tweening to full
                // brightness while hovered (same tween as the hide
                // arrow's reveal) — but re-dimming as the card is pulled
                // up (`np_card_t`), hover or not: a bright loop behind
                // the card fights the content. The scrim sits under the
                // scroller, so the card itself never darkens.
                let dim = Computed::new(
                    (self.canvas.hover_t.clone(), self.player.np_card_t.clone()),
                    |(h, k)| {
                        let strength = (1.0 - h.clamp(0.0, 1.0)).max(k.clamp(0.0, 1.0));
                        [
                            0.0,
                            0.0,
                            0.0,
                            crate::model::canvas::CANVAS_DIM_ALPHA * strength,
                        ]
                    },
                );
                bg.rect(())
                    .abs(0.0, 0.0)
                    .w(Len::Fill)
                    .h(Len::Fill)
                    .radius(t::R_LG)
                    .color(dim);
            });
    }

    /// The pane's single scroll region, both modes: canvas playing → an
    /// empty spacer block (viewport − `CARD_PEEK`, sized by the frame
    /// tick) over the video, so the card peeks at the pane's bottom edge;
    /// no canvas → a padded rounded square cover with the card directly
    /// below it, fully visible. Spring-smoothed with overscroll bounce so
    /// any scroll (the card pull, or overflowing future card content) has
    /// real physics.
    fn scroller(&self, pane: &mut Scene) {
        pane.col("now_playing_scroll")
            .w(Len::Fill)
            .h(Len::Fill)
            .scroll_y()
            .overscroll(true)
            .scrollbar(|sb| sb.auto_hide(true).margin(t::SP_0_5).thickness(t::SP_1))
            .child(|sc| {
                if self.canvas.active {
                    sc.col(())
                        .w(Len::Fill)
                        .height_px_bind(self.player.np_fill_h.clone())
                        .child(|top| self.context_pill(top));
                } else {
                    sc.col(())
                        .w(Len::Fill)
                        .pad_ltrb(t::SP_2, t::SP_2, t::SP_2, t::SP_0)
                        .child(|c| {
                            c.col(())
                                .w(Len::Fill)
                                .square()
                                .overflow(Overflow::Hidden, Overflow::Hidden)
                                .radius(t::R_MD)
                                .child(|a| {
                                    crossfaded_art_flat(
                                        a,
                                        &self.backdrop.prev,
                                        &self.backdrop.curr,
                                        &self.backdrop.panel_t,
                                    );
                                });
                            self.context_pill(c);
                        });
                }
                self.card(sc);
            });
    }

    /// The queue-source pill (the context's display name — "Chill",
    /// "Daily Mix 2" — falling back to the kind) on a dark disc for
    /// contrast over any art/video. An **abs child of the scroller's
    /// first block** (zero layout footprint, so the card-peek math is
    /// untouched) — it rides the scroll and clears out of the way as the
    /// card is pulled up. Both callers pad the pill the same absolute
    /// offset: the art block's own SP_2 inset makes the total match the
    /// cover's proportional margin.
    fn context_pill(&self, block: &mut Scene) {
        // Unknown context (fresh cold start, contextless play) shows
        // nothing — a pill reading just "—" is noise. Opacity-bound (0
        // also skips hit-testing), so it appears the moment a state
        // names one, no rebuild.
        let shown = Computed::new((self.player.context_known.clone(),), |(k,)| {
            if k { 1.0 } else { 0.0 }
        });
        block
            .row(())
            .abs(t::SP_2, t::SP_2)
            .h_px(t::SP_7)
            .pad_xy(t::SP_2_5, t::SP_0)
            .align(opal_gfx::Align::Center)
            .rgba(0.0, 0.0, 0.0, 0.55)
            .radius(t::R_FULL)
            .opacity_bind(shown)
            .child(|pill| {
                pill.text_bound((), self.player.context_label.clone(), 11.0)
                    .color(t::TEXT)
                    .max_width_px(CARD_TEXT_MAX);
            });
    }

    /// The hide arrow, top-right, fading in with the pane's hover tween
    /// (`canvas.hover_t`) — pane chrome, so it stays put while the
    /// content scrolls (unlike the pill). On a dark disc for contrast
    /// over any art/video.
    fn hide_button(&self, m: &mut Scene) {
        let hover = Signal::new(false);
        let tint = Computed::new((hover.clone(), self.backdrop.accent.clone()), |(h, acc)| {
            if h { accent_hover_color(&acc) } else { t::TEXT }
        });
        let on_toggle = self.on_toggle.clone();
        // Over the full-bleed video the pane edge is the visual edge; in
        // art mode the cover is inset SP_2, so the arrow steps in one more
        // unit to keep a proportional margin to the cover's corner.
        let inset = if self.canvas.active { t::SP_2 } else { t::SP_4 };
        m.row(())
            .abs(0.0, inset)
            .w(Len::Fill)
            .justify(Justify::End)
            .align(opal_gfx::Align::Center)
            .pad_ltrb(inset, t::SP_0, inset, t::SP_0)
            .child(|strip| {
                strip
                    .row(())
                    .w_px(t::SP_7)
                    .h_px(t::SP_7)
                    .center()
                    .rgba(0.0, 0.0, 0.0, 0.55)
                    .radius(t::R_FULL)
                    .opacity_bind(self.canvas.hover_t.clone())
                    .on_hover(hover)
                    .on_click(move |_| on_toggle())
                    .child(|c| {
                        self.icons.render(c, Icon::ChevronRight, t::ICON_SM, tint);
                    });
            });
    }

    /// The track card — **one** component for both modes. Its colours ride
    /// `np_card_t`: in canvas mode the frame tick maps the spring-smoothed
    /// scroll offset into it (glassy-dark at rest so it doesn't pull the
    /// eye from the video, blending to the full accent as it's pulled up);
    /// with no canvas the tick pins it at 1, so the same card simply sits
    /// below the cover in its final accent state. The body holds the
    /// "About the artist" section (photo + name + followers) and the track
    /// facts — never the album cover, which is already on screen.
    fn card(&self, sc: &mut Scene) {
        let canvas_mode = self.canvas.active;
        let accent = &self.backdrop.accent;
        let reveal = &self.player.np_card_t;
        // Header fill: translucent black → the live accent.
        let header_bg = Computed::new((reveal.clone(), accent.clone()), |(k, a)| {
            [0.0, 0.0, 0.0, 0.42].lerp(a, k)
        });
        // Text: plain chrome white at rest → the accent's contrast-safe
        // foreground once the header wears the accent.
        let fg = Computed::new((reveal.clone(), accent.clone()), |(k, a)| {
            t::TEXT.lerp(accent_fg_color(&a), k)
        });
        // Artist line: same blend, slightly recessed.
        let fg_dim = Computed::new((reveal.clone(), accent.clone()), |(k, a)| {
            let c = t::TEXT.lerp(accent_fg_color(&a), k);
            [c[0], c[1], c[2], 0.78]
        });
        // Body fill: near-transparent dark → solid panel.
        let body_bg = Computed::new((reveal.clone(),), |(k,)| {
            [0.0, 0.0, 0.0, 0.30].lerp([t::PANEL_HI[0], t::PANEL_HI[1], t::PANEL_HI[2], 1.0], k)
        });
        // Flush with the pane bottom in canvas mode (the peek position);
        // a normal gap below the cover otherwise.
        let pad_top = if canvas_mode { t::SP_0 } else { t::SP_2 };
        sc.col(())
            .w(Len::Fill)
            .pad_ltrb(t::SP_2, pad_top, t::SP_2, t::SP_2)
            .child(|card| {
                card.col(())
                    .w(Len::Fill)
                    .color(header_bg)
                    .radii(t::R_MD, t::R_MD, 0.0, 0.0)
                    .pad(t::SP_3)
                    .gap(t::SP_0_5)
                    .child(|h| {
                        h.text_bound((), self.player.title.clone(), 15.0)
                            .color(fg)
                            .max_width_px(CARD_TEXT_MAX);
                        let artists = self
                            .player
                            .with_snapshot(|p| p.artists.clone())
                            .unwrap_or_default();
                        let nav = self.nav.clone();
                        crate::widgets::artist_links::artist_links(
                            h,
                            "np_card_artists",
                            &artists,
                            self.player.artist.clone(),
                            12.0,
                            fg_dim.clone().into(),
                            Rc::new(move |ctx, id| {
                                nav(ctx, crate::views::MainNav::Artist { id: id.to_string() })
                            }),
                        );
                    });
                card.col(())
                    .w(Len::Fill)
                    .color(body_bg)
                    .radii(0.0, 0.0, t::R_MD, t::R_MD)
                    .overflow(Overflow::Hidden, Overflow::Hidden)
                    .child(|body| {
                        self.about_artist(body);
                        self.credits(body);
                    });
            });
    }

    /// "About the artist" — the card body's hero: the artist photo
    /// (16:9 cover-crop, full-bleed, captioned) with name, follower
    /// count and the biography paragraph below. Skipped entirely until
    /// the profile resolves (the reducer fetches it on artist change and
    /// rebuilds).
    fn about_artist(&self, body: &mut Scene) {
        let Some(about) = &self.player.np_about else {
            return;
        };
        let photo = about
            .image_url
            .as_ref()
            .and_then(|u| self.art.signal(&crate::album_art::cache_key(u)));
        if let Some(sig) = photo {
            body.col(())
                .w(Len::Fill)
                .aspect_ratio(16.0 / 9.0)
                .overflow(Overflow::Hidden, Overflow::Hidden)
                .child(|m| {
                    m.image_bound((), sig)
                        .abs(0.0, 0.0)
                        .w(Len::Fill)
                        .h(Len::Fill)
                        .image_cover()
                        .placeholder_fill(t::PLACEHOLDER)
                        .color([1.0, 1.0, 1.0, 1.0]);
                    // Caption pill for contrast over any photo.
                    m.row(())
                        .abs(t::SP_2, t::SP_2)
                        .h_px(t::SP_6)
                        .pad_xy(t::SP_2_5, t::SP_0)
                        .align(opal_gfx::Align::Center)
                        .rgba(0.0, 0.0, 0.0, 0.55)
                        .radius(t::R_FULL)
                        .child(|pill| {
                            pill.text((), "About the artist", 11.0).color(t::TEXT);
                        });
                });
        }
        // Name (a row into the artist page) + followers + the bio teaser.
        let nav = self.nav.clone();
        let artist_id = about.id.clone();
        body.col(())
            .w(Len::Fill)
            .pad_ltrb(t::SP_1, t::SP_2, t::SP_1, t::SP_3)
            .gap(t::SP_0_5)
            .child(|txt| {
                txt.col(())
                    .w(Len::Fill)
                    .pad_xy(t::SP_2, t::SP_1)
                    .gap(t::SP_0_5)
                    .radius(t::R_SM)
                    .hover_color(t::HOVER_LIFT_SUBTLE)
                    .on_click(move |ctx| {
                        nav(
                            ctx,
                            crate::views::MainNav::Artist {
                                id: artist_id.clone(),
                            },
                        )
                    })
                    .child(|name| {
                        name.text((), about.name.clone(), 14.0)
                            .color(t::TEXT)
                            .max_width_px(CARD_TEXT_MAX);
                        if about.followers > 0 {
                            name.text((), super::artist::fmt_followers(about.followers), 12.0)
                                .color(t::TEXT_DIM);
                        }
                    });
                if let Some(bio) = &self.player.np_bio {
                    txt.col(())
                        .w(Len::Fill)
                        .pad_xy(t::SP_2, t::SP_0)
                        .child(|b| {
                            // Fill-width + wrap → the paragraph flows to the
                            // pane's live width and re-wraps on resize (the pane
                            // width tracks window height via the 9:16 aspect).
                            b.text((), clamp_bio(bio), 12.0)
                                .color(t::TEXT_DIM)
                                .w(Len::Fill)
                                .wrap();
                        });
                }
            });
    }

    /// "Credits" — Spotify's grouped sections (Composition & Lyrics /
    /// Production & Engineering / Performers, plus Sources), one row per
    /// person with their roles. Rows with an artist profile navigate to
    /// it. Skipped until resolved or when the endpoint has none.
    fn credits(&self, body: &mut Scene) {
        let Some((_, credits)) = &self.player.np_credits else {
            return;
        };
        if credits.is_empty() {
            return;
        }
        // Hairline over the section when the about block sits above it.
        if self.player.np_about.is_some() {
            body.col(())
                .w(Len::Fill)
                .pad_xy(t::SP_3, t::SP_0)
                .child(|d| {
                    d.rect(()).w(Len::Fill).h_px(1.0).rgba(1.0, 1.0, 1.0, 0.08);
                });
        }
        body.col(())
            .w(Len::Fill)
            .pad_ltrb(t::SP_1, t::SP_3, t::SP_1, t::SP_3)
            .gap(t::SP_3)
            .child(|c| {
                c.row(()).pad_xy(t::SP_2, t::SP_0).child(|h| {
                    h.text((), "Credits", 14.0).color(t::TEXT);
                });
                for group in &credits.groups {
                    c.col(()).w(Len::Fill).gap(t::SP_1).child(|g| {
                        g.row(()).pad_xy(t::SP_2, t::SP_0).child(|h| {
                            h.text((), group.title.clone(), 11.0).color(t::TEXT_DIM);
                        });
                        for person in &group.people {
                            self.credit_row(g, person);
                        }
                    });
                }
                if !credits.sources.is_empty() {
                    c.col(()).w(Len::Fill).gap(t::SP_1).child(|g| {
                        g.row(()).pad_xy(t::SP_2, t::SP_0).child(|h| {
                            h.text((), "Sources", 11.0).color(t::TEXT_DIM);
                        });
                        g.col(()).w(Len::Fill).pad_xy(t::SP_2, t::SP_0).child(|s| {
                            for src in &credits.sources {
                                s.text((), src.clone(), 12.0)
                                    .color(t::TEXT)
                                    .max_width_px(CARD_TEXT_MAX);
                            }
                        });
                    });
                }
            });
    }

    /// One credits person: name over their roles. With an artist profile
    /// the row hover-lifts and opens the artist page.
    fn credit_row(&self, g: &mut Scene, person: &crate::worker::CreditPerson) {
        let clickable = !person.artist_id.is_empty();
        let mut row = g.col(());
        row.w(Len::Fill)
            .pad_xy(t::SP_2, t::SP_1)
            .gap(t::SP_0_5)
            .radius(t::R_SM);
        if clickable {
            let nav = self.nav.clone();
            let id = person.artist_id.clone();
            row.hover_color(t::HOVER_LIFT_SUBTLE)
                .on_click(move |ctx| nav(ctx, crate::views::MainNav::Artist { id: id.clone() }));
        }
        let name = person.name.clone();
        let roles = person.roles.clone();
        row.child(|r| {
            r.text((), name, 13.0)
                .color(t::TEXT)
                .max_width_px(CARD_TEXT_MAX);
            if !roles.is_empty() {
                r.text((), roles, 11.0)
                    .color(t::TEXT_DIM)
                    .max_width_px(CARD_TEXT_MAX);
            }
        });
    }
}

/// Word-boundary clamp for the biography paragraph — Spotify's own card
/// shows a teaser, not the full essay; the pane's card shouldn't scroll
/// forever either.
fn clamp_bio(bio: &str) -> String {
    const MAX: usize = 340;
    if bio.len() <= MAX {
        return bio.to_string();
    }
    // Last whitespace within the cap, walked by char so a multi-byte
    // codepoint straddling the boundary can't split.
    let mut cut = 0;
    for (i, c) in bio.char_indices() {
        if i > MAX {
            break;
        }
        if c.is_whitespace() {
            cut = i;
        }
    }
    if cut == 0 {
        // Single unbroken run — fall back to the last full char in cap.
        cut = bio
            .char_indices()
            .take_while(|(i, _)| *i <= MAX)
            .last()
            .map(|(i, _)| i)
            .unwrap_or(0);
    }
    format!("{}\u{2026}", bio[..cut].trim_end())
}
