//! Two-layer album-art crossfade widget.

use opal_gfx::{Computed, ImageHandle, Len, Scene, Signal};

use crate::widgets::tokens as t;

/// Fully-opaque white tint for the outgoing (under) crossfade layer.
pub const OPAQUE_TINT: [f32; 4] = [1.0, 1.0, 1.0, 1.0];

/// Incoming-layer tint: white with alpha rising 0 → 1 as the crossfade
/// advances, so the new cover fades in *over* the outgoing one.
///
/// The outgoing layer underneath stays fully opaque (a plain `[1,1,1,1]`
/// literal, no bind). Crucially this is NOT a symmetric dual fade: if both
/// layers cross-faded (prev `1-t`, curr `t`) their combined coverage dips
/// to ~75% at the midpoint and the dark glass backdrop bleeds through — a
/// murky mid-transition. Holding the outgoing layer opaque keeps full
/// coverage throughout, so it's a clean A→B dissolve. Painter order
/// (outgoing declared first) guarantees incoming draws on top.
pub fn fade_in_alpha(crossfade_t: &Signal<f32>) -> Computed<[f32; 4]> {
    Computed::new((crossfade_t.clone(),), move |(t,)| {
        [1.0, 1.0, 1.0, t.clamp(0.0, 1.0)]
    })
}

/// Two stacked album-art layers that crossfade on track change, sized to
/// fill the parent box. Reuses the backdrop's `crossfade_t` + prev/curr
/// handles so panel art dissolves in lockstep with the ambient backdrop
/// instead of snapping. Dim placeholder when neither handle resolves. Both
/// layers are `abs(0,0)` so they overlap — the parent must have a definite
/// size for `Fill` to resolve against.
/// Flat (no composite layer) variant of [`crossfaded_art`]: the same
/// opaque-under / fade-in-over image pair drawn directly into the parent,
/// with no rounding of its own — clip/round via the parent's
/// `overflow(Hidden)` + radius instead. **Use this inside scroll
/// containers**: a nested `.layer()` promotion doesn't ride the scroll
/// layer's offset, so the layered variant paints at the unscrolled
/// position (engine gap — revisit if composite-rounded art is ever
/// needed in scroll content).
pub fn crossfaded_art_flat(
    c: &mut Scene,
    prev: &Signal<Option<ImageHandle>>,
    curr: &Signal<Option<ImageHandle>>,
    crossfade_t: &Signal<f32>,
) {
    let base = Computed::new(
        (prev.clone(), curr.clone(), crossfade_t.clone()),
        |(p, cu, t)| if t >= 1.0 && cu.is_some() { None } else { p },
    );
    let fade = fade_in_alpha(crossfade_t);
    c.image_bound((), base)
        .abs(0.0, 0.0)
        .w(Len::Fill)
        .h(Len::Fill)
        .placeholder_fill(t::PLACEHOLDER)
        .color(OPAQUE_TINT);
    c.image_bound((), curr.clone())
        .abs(0.0, 0.0)
        .w(Len::Fill)
        .h(Len::Fill)
        .color(fade);
}

pub fn crossfaded_art(
    c: &mut Scene,
    prev: &Signal<Option<ImageHandle>>,
    curr: &Signal<Option<ImageHandle>>,
    crossfade_t: &Signal<f32>,
    radius: f32,
) {
    // The two covers crossfade by stacking (outgoing held opaque, incoming
    // fading in over it — a dual fade would dip coverage to ~75% mid-way and
    // bleed the backdrop). Stacked rounded layers leak the back one through
    // their anti-aliased corner, so instead of rounding each cover we group
    // them into ONE composite `.layer()` and round its *result* once: the
    // covers draw SQUARE inside, blend cleanly in the layer texture, and the
    // single composite corner is artifact-free through the whole dissolve —
    // not just when settled. The dim loading placeholder folds into the base
    // cover's own fill, so there's never a third stacked layer.
    let base = Computed::new(
        (prev.clone(), curr.clone(), crossfade_t.clone()),
        |(p, cu, t)| if t >= 1.0 && cu.is_some() { None } else { p },
    );
    let fade = fade_in_alpha(crossfade_t);
    c.col(())
        .abs(0.0, 0.0)
        .w(Len::Fill)
        .h(Len::Fill)
        // `.layer()` + radius = round the composited group once (see the
        // engine's composite-time `round_rect`). Inner covers stay square.
        .radius(radius)
        .layer()
        .child(move |inner| {
            inner
                .image_bound((), base)
                .abs(0.0, 0.0)
                .w(Len::Fill)
                .h(Len::Fill)
                .placeholder_fill(t::PLACEHOLDER)
                .color(OPAQUE_TINT);
            inner
                .image_bound((), curr.clone())
                .abs(0.0, 0.0)
                .w(Len::Fill)
                .h(Len::Fill)
                .color(fade);
        });
}
