//! Album-art backdrop + accent crossfade state.
//!
//! Owns the two layered cover handles (outgoing/incoming), the slow
//! backdrop crossfade and the faster foreground-panel crossfade, and the
//! dominant accent colour driving the accent-tinted chrome. Everything is
//! reactive: [`BackdropModel::promote`] swaps the handles via the lib's
//! image-handle binds and drives the tweens through the `Timeline`, so a
//! track change cross-dissolves with **no scene rebuild**.

use std::time::{Duration, Instant};

use opal_gfx::{Curve, ImageHandle, Signal, Timeline};

use crate::widgets::tokens;

/// How long the backdrop crossfade + accent colour transition takes on
/// track change. An ambient cross-dissolve — the previous cover fades
/// out as the next fades in over this window. 600 ms reads as an abrupt
/// snap, 3 s drags; ~1.5 s is the sweet spot.
const CROSSFADE_DURATION: Duration = Duration::from_millis(1500);

/// Foreground panel-art crossfade — deliberately much snappier than the
/// backdrop so the cover/thumb feel responsive on track change while the
/// big blurred backdrop + accent catch up behind them.
const PANEL_CROSSFADE_DURATION: Duration = Duration::from_millis(450);

pub struct BackdropModel {
    /// Outgoing backdrop layer — the previous track's art, held opaque
    /// under the incoming layer so the dissolve has full coverage (no
    /// background bleed at the midpoint).
    pub prev: Signal<Option<ImageHandle>>,
    /// Incoming backdrop layer — the current track's art, fading in.
    pub curr: Signal<Option<ImageHandle>>,
    /// 0 → 1 slow crossfade progress (backdrop). The incoming layer is a
    /// `.layer_opacity(crossfade_t)` composite layer, so the lib drives
    /// its composite opacity each frame (composite-only, no image
    /// re-raster).
    pub crossfade_t: Signal<f32>,
    /// 0 → 1 fast crossfade for the foreground panel art (now-playing
    /// cover + player-bar thumb) — small + in focus, so a snappy swap
    /// reads better while the big blurred backdrop lags behind.
    pub panel_t: Signal<f32>,
    /// Dominant colour of the current track's art, driving the
    /// accent-tinted UI (play pill, active toggles, login button).
    /// Always contrast-safe over the dark chrome — chosen/lifted at the
    /// worker (`color::chrome_accent` / `lift_for_chrome`).
    pub accent: Signal<[f32; 4]>,
    /// Mean luminance of the current cover — how bright the blurred
    /// ambient backdrop reads. Drives the adaptive glass dim (bright art
    /// gets a stronger tint so the chrome on top keeps contrast). Rides
    /// the **slow** crossfade tween, in step with the backdrop dissolve
    /// it compensates for.
    pub art_luma: Signal<f32>,
}

impl BackdropModel {
    pub fn new() -> Self {
        Self {
            prev: Signal::new(None),
            curr: Signal::new(None),
            crossfade_t: Signal::new(1.0),
            panel_t: Signal::new(1.0),
            accent: Signal::new(tokens::ACCENT),
            art_luma: Signal::new(0.0),
        }
    }

    /// True once the incoming cover fully covers the base fill (opaque +
    /// crossfade settled) — lets the shell drop the base background draw.
    pub fn covered(&self) -> bool {
        self.curr.get().is_some() && self.crossfade_t.get() >= 1.0
    }

    /// Promote `next` as the current cover. If it differs from what's
    /// shown, stash the outgoing handle in `prev`, snap both crossfades to
    /// 0, and tween them to 1.0 (slow backdrop + fast panel). `accent`,
    /// when given, rides the **fast** panel tween so the foreground chrome
    /// re-tints in step with the cover swap (the big blurred backdrop
    /// still lags on the slow tween). `accent = None` keeps the previous
    /// accent so a cache-miss doesn't flash to the default.
    pub fn promote(
        &self,
        next: ImageHandle,
        accent: Option<[f32; 4]>,
        luma: f32,
        tl: &mut Timeline,
        now: Instant,
    ) {
        let current = self.curr.get();
        if current != Some(next) {
            self.prev.set(current);
            self.curr.set(Some(next));
            self.crossfade_t.set(0.0);
            tl.animate(
                &self.crossfade_t,
                1.0,
                Curve::EaseInOut,
                CROSSFADE_DURATION,
                now,
            );
            self.panel_t.set(0.0);
            tl.animate(
                &self.panel_t,
                1.0,
                Curve::EaseInOut,
                PANEL_CROSSFADE_DURATION,
                now,
            );
        }
        tl.animate(
            &self.art_luma,
            luma,
            Curve::EaseInOut,
            CROSSFADE_DURATION,
            now,
        );
        if let Some(c) = accent {
            tl.animate(
                &self.accent,
                c,
                Curve::EaseInOut,
                PANEL_CROSSFADE_DURATION,
                now,
            );
        }
    }

    /// Tween only the accent — a late `AccentReady` overriding the
    /// provisional pixel-average with Spotify's exact colour.
    pub fn set_accent(&self, accent: [f32; 4], tl: &mut Timeline, now: Instant) {
        tl.animate(
            &self.accent,
            accent,
            Curve::EaseInOut,
            PANEL_CROSSFADE_DURATION,
            now,
        );
    }
}

impl Default for BackdropModel {
    fn default() -> Self {
        Self::new()
    }
}
