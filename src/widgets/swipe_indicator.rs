//! Edge arrows for the two-finger swipe (browser-style back/forward).
//!
//! Rides the engine's live pull signal: a disc slides in from the edge
//! the gesture points at, its arrow growing with the pull; past the
//! release point (`|pull| >= 1`) the disc fills with the accent, and the
//! arrow keeps stretching a little further to `SWIPE_NAV_MAX_STRETCH`
//! so a long pull still reads as motion. Everything is bound — no
//! rebuilds while the fingers move — and the engine tweens the pull back
//! to zero on release, which is the whole exit animation.

use opal_gfx::{Align, Computed, Len, SWIPE_NAV_MAX_STRETCH, Scene, Signal};

use crate::widgets::color::{AccentSurface, surface_fg, surface_fill};
use crate::widgets::icon::{Icon, IconSet};
use crate::widgets::tokens as t;

/// Disc diameter.
const DISC: f32 = t::SP_11;
/// Arrow size at the start of the pull and at the release point.
const ARROW_MIN: f32 = t::SP_2_5;
const ARROW_MAX: f32 = t::SP_5;
/// Extra arrow growth over the stretch past the release point.
const ARROW_STRETCH: f32 = t::SP_1;
/// How far the disc slides in from the edge as the pull completes.
const SLIDE: f32 = t::SP_4;

/// Full-window, non-interactive overlay: one indicator per edge. Sits
/// under the modals so a swipe never draws over a dialog.
pub fn view(
    s: &mut Scene,
    icons: &IconSet,
    pull: &Signal<f32>,
    can_back: &Signal<bool>,
    can_forward: &Signal<bool>,
    accent: &Signal<[f32; 4]>,
) {
    s.row(())
        .abs(0.0, 0.0)
        .w(Len::Fill)
        .h(Len::Fill)
        .align(Align::Center)
        .rgba(0.0, 0.0, 0.0, 0.0)
        .child(|row| {
            edge(row, icons, Icon::ChevronLeft, pull, can_back, accent, 1.0);
            row.rect(()).w(Len::Fill).rgba(0.0, 0.0, 0.0, 0.0);
            edge(
                row,
                icons,
                Icon::ChevronRight,
                pull,
                can_forward,
                accent,
                -1.0,
            );
        });
}

/// One edge's indicator. `sign` picks the half of the pull it answers to
/// (+1 back / left edge, −1 forward / right edge); the other half reads
/// as zero here.
fn edge(
    s: &mut Scene,
    icons: &IconSet,
    icon: Icon,
    pull: &Signal<f32>,
    can: &Signal<bool>,
    accent: &Signal<[f32; 4]>,
    sign: f32,
) {
    // Own share of the pull, gated on there being a page that way.
    let p = Computed::new((pull.clone(), can.clone()), move |(v, c)| {
        if c { (v * sign).max(0.0) } else { 0.0 }
    });
    let opacity = Computed::new((p.clone(),), |(p,)| (p * 2.0).min(1.0));
    let slide = Computed::new((p.clone(),), |(p,)| SLIDE * p.min(1.0));
    let arrow = Computed::new((p.clone(),), |(p,)| {
        let base = ARROW_MIN + (ARROW_MAX - ARROW_MIN) * p.min(1.0);
        let over =
            (p - 1.0).clamp(0.0, SWIPE_NAV_MAX_STRETCH - 1.0) / (SWIPE_NAV_MAX_STRETCH - 1.0);
        let size = base + ARROW_STRETCH * over;
        [size, size]
    });
    let armed = Computed::new((p.clone(),), |(p,)| p >= 1.0);
    let disc = Computed::new((armed.clone(), accent.clone()), |(a, acc)| {
        if a {
            surface_fill(&acc, AccentSurface::Solid)
        } else {
            t::PANEL
        }
    });
    let glyph = Computed::new((armed, accent.clone()), |(a, acc)| {
        if a {
            surface_fg(&acc, AccentSurface::Solid)
        } else {
            t::TEXT
        }
    });

    // Leading gap for the left edge, trailing for the right: the disc
    // starts flush with the edge and eases inward as the pull builds.
    if sign > 0.0 {
        s.rect(())
            .width_px_bind(slide.clone())
            .rgba(0.0, 0.0, 0.0, 0.0);
    }
    s.row(())
        .w_px(DISC)
        .h_px(DISC)
        .radius(t::R_FULL)
        .center()
        .color(disc)
        .opacity_bind(opacity)
        .child(|c| {
            c.image((), icons.get(icon)).size_bind(arrow).color(glyph);
        });
    if sign < 0.0 {
        s.rect(()).width_px_bind(slide).rgba(0.0, 0.0, 0.0, 0.0);
    }
}
