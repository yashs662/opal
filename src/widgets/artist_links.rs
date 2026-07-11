//! Clickable multi-artist credit line — shared by the player bar and the
//! now-playing card (and anywhere else a track's artists show).
//!
//! Every credited artist renders as its own click target (id → the
//! caller's `on_artist`, typically opening the artist page), separated by
//! ", ", inside a [`Scene::marquee`] strip: a long credit list pans
//! instead of wrapping or truncating, and snaps back after a hold. While
//! the credit set hasn't resolved yet (sparse cluster pushes ship names
//! without ids — the track-details backfill rebuilds with the full set),
//! the line falls back to the reactive joined-names bind so it still
//! updates without a rebuild.

use std::rc::Rc;

use opal_gfx::event::EventCtx;
use opal_gfx::{Bind, Len, Scene, TextSignal};

use crate::api::TrackArtist;
use crate::widgets::tokens as t;

/// Artist-click callback: receives the bare artist id. View-layer callers
/// wrap their navigation (`MainNav::Artist`) so this widget stays free of
/// routing types.
pub type ArtistClickFn = Rc<dyn Fn(&mut EventCtx, &str)>;

/// Render the credit line into `s`. `name` keys the marquee strip's
/// scroll identity (stable across rebuilds, unique per surface). The
/// strip defaults to `w: Fill` (clipping at the parent's width); chain
/// on the returned builder to size it differently (e.g. a px width in
/// the player bar's fixed title column).
pub fn artist_links<'a>(
    s: &'a mut Scene,
    name: &str,
    artists: &[TrackArtist],
    fallback: TextSignal,
    font_size: f32,
    color: Bind<[f32; 4]>,
    on_artist: ArtistClickFn,
) -> opal_gfx::NodeBuilderRef<'a> {
    let artists = artists.to_vec();
    let mut strip = s.marquee(name.to_string(), move |inner| {
        inner
            .row(())
            .gap(t::SP_0)
            .child(move |line| {
                if artists.is_empty() {
                    // Credits not resolved yet — reactive joined names.
                    line.text_bound((), fallback, font_size).color(color);
                    return;
                }
                for (i, a) in artists.iter().enumerate() {
                    if i > 0 {
                        line.text((), ", ", font_size).color(color.clone());
                    }
                    let mut name_node = line.text((), a.name.clone(), font_size);
                    name_node.color(color.clone());
                    if !a.id.is_empty() {
                        let on_artist = on_artist.clone();
                        let id = a.id.clone();
                        // Brighten the name itself on hover (the interact
                        // sugar swaps the *glyph* colour on text nodes) —
                        // the click affordance without an underline.
                        name_node
                            .hover_color(t::TEXT)
                            .on_click(move |ctx| on_artist(ctx, &id));
                    }
                }
            });
    });
    strip.w(Len::Fill);
    strip
}
