//! Accent-derived colour utilities.
//!
//! The live accent is extracted from album art, so it can land anywhere
//! in the gamut — including colours that vanish against our dark chrome
//! (Spotify's `color_dark` variant is *designed* as a background tint).
//! Everything here keeps that accent usable: WCAG-style luminance and
//! contrast math, a chooser that picks the most usable extracted variant,
//! and a lift that brightens a failing accent until icons/pills tinted
//! with it clear a minimum contrast over the chrome.

use opal_gfx::{Computed, Signal};

/// WCAG relative luminance of an sRGB colour (alpha ignored): channels
/// are linearized then weighted (Rec. 709). 0 = black, 1 = white.
pub fn luminance(c: [f32; 4]) -> f32 {
    fn lin(u: f32) -> f32 {
        if u <= 0.04045 {
            u / 12.92
        } else {
            ((u + 0.055) / 1.055).powf(2.4)
        }
    }
    0.2126 * lin(c[0]) + 0.7152 * lin(c[1]) + 0.0722 * lin(c[2])
}

/// WCAG contrast ratio between two relative luminances (1..=21).
pub fn contrast(l1: f32, l2: f32) -> f32 {
    let (hi, lo) = if l1 >= l2 { (l1, l2) } else { (l2, l1) };
    (hi + 0.05) / (lo + 0.05)
}

/// Relative luminance of the dark chrome accent-tinted elements sit on:
/// the player bar / panels — near-black with a slight backdrop bleed.
/// Deliberately a touch above true black so the lift aims high enough to
/// survive the bleed, not just a #000 swatch.
const CHROME_LUMA: f32 = 0.02;

/// Minimum contrast for accent-tinted icons/pills against the chrome.
/// 3:1 is the WCAG 1.4.11 floor for graphical objects / UI components.
const MIN_ACCENT_CONTRAST: f32 = 3.0;

/// Pick the accent the chrome should use from Spotify's extracted
/// variants (in preference order — `raw` is the dominant vibrant,
/// `light` is built for dark surfaces, `dark` for light ones), falling
/// back to `fallback` (the pixel-average) when none decoded. Whatever
/// wins is [`lift_for_chrome`]-ed so the result is *guaranteed* to clear
/// [`MIN_ACCENT_CONTRAST`] over the dark chrome.
pub fn chrome_accent(
    variants: &crate::extracted_color::ExtractedColors,
    fallback: [f32; 4],
) -> [f32; 4] {
    let candidates = [variants.raw, variants.light, variants.dark];
    // First variant that already passes keeps Spotify's exact colour.
    for c in candidates.into_iter().flatten() {
        if contrast(luminance(c), CHROME_LUMA) >= MIN_ACCENT_CONTRAST {
            return c;
        }
    }
    // None passes (a uniformly dark palette): lift the most preferred
    // available variant just enough to pass.
    let base = candidates.into_iter().flatten().next().unwrap_or(fallback);
    lift_for_chrome(base)
}

/// Brighten `c` (mix toward white, preserving hue) until it clears
/// [`MIN_ACCENT_CONTRAST`] against the chrome. No-op when it already
/// does. Binary-searches the mix factor — luminance is monotonic in it.
pub fn lift_for_chrome(c: [f32; 4]) -> [f32; 4] {
    let target = MIN_ACCENT_CONTRAST * (CHROME_LUMA + 0.05) - 0.05;
    if luminance(c) >= target {
        return c;
    }
    let mix = |t: f32| {
        [
            c[0] + (1.0 - c[0]) * t,
            c[1] + (1.0 - c[1]) * t,
            c[2] + (1.0 - c[2]) * t,
            c[3],
        ]
    };
    let (mut lo, mut hi) = (0.0_f32, 1.0_f32);
    for _ in 0..20 {
        let mid = (lo + hi) * 0.5;
        if luminance(mix(mid)) < target {
            lo = mid
        } else {
            hi = mid
        }
    }
    mix(hi)
}

/// Push a colour up to saturation/brightness floors (HSV), keeping its
/// hue. The extracted accent can come back washed out or muddy — fine as
/// a small tint on an icon, not fine where the accent *is* the emphasis
/// (a lit lyric line against dimmed ones), because a desaturated mid-tone
/// reads as "slightly different grey" rather than "this is the one".
/// Already-vivid colours pass through untouched.
pub fn vivid(c: [f32; 4], min_sat: f32, min_val: f32) -> [f32; 4] {
    let (r, g, b) = (c[0], c[1], c[2]);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let delta = max - min;
    let sat = if max <= 0.0 { 0.0 } else { delta / max };
    let val = max.max(min_val);
    let sat = sat.max(min_sat);
    // A greyscale source has no hue to saturate — brightening is all
    // that's available (and enough: it separates from the dim rest).
    if delta <= f32::EPSILON {
        return [val, val, val, c[3]];
    }
    // Hue in sixths, then the standard HSV → RGB reconstruction.
    let hue = if max == r {
        ((g - b) / delta).rem_euclid(6.0)
    } else if max == g {
        (b - r) / delta + 2.0
    } else {
        (r - g) / delta + 4.0
    };
    let cc = val * sat;
    let x = cc * (1.0 - ((hue % 2.0) - 1.0).abs());
    let m = val - cc;
    let (rr, gg, bb) = match hue as u32 {
        0 => (cc, x, 0.0),
        1 => (x, cc, 0.0),
        2 => (0.0, cc, x),
        3 => (0.0, x, cc),
        4 => (x, 0.0, cc),
        _ => (cc, 0.0, x),
    };
    [rr + m, gg + m, bb + m, c[3]]
}

/// Foreground colour (icon/text) that contrasts with the live accent:
/// whichever of white / near-black has the higher WCAG contrast against
/// it. Reactive — follows the accent crossfade.
/// Non-reactive core of [`accent_fg`] — the contrast-safe foreground (white
/// or near-black) for `a`. For callers that already hold the accent value
/// inside a multi-input `Computed` (e.g. the loading play glyph).
pub fn accent_fg_color(a: &[f32; 4]) -> [f32; 4] {
    const LIGHT: [f32; 4] = [1.0, 1.0, 1.0, 1.0];
    const DARK: [f32; 4] = [0.08, 0.08, 0.08, 1.0];
    let l = luminance(*a);
    if contrast(l, luminance(LIGHT)) >= contrast(l, luminance(DARK)) {
        LIGHT
    } else {
        DARK
    }
}

pub fn accent_fg(accent: &Signal<[f32; 4]>) -> Computed<[f32; 4]> {
    Computed::new((accent.clone(),), |(a,)| accent_fg_color(&a))
}

/// Hover tint for accent-driven icon buttons: the accent lifted toward
/// white, so a hovered icon reads brighter than both the dim resting
/// state and the plain accent an *active* toggle already wears.
pub fn accent_hover_color(a: &[f32; 4]) -> [f32; 4] {
    use opal_gfx::Lerp;
    let c = a.lerp([1.0, 1.0, 1.0, 1.0], 0.30);
    [c[0], c[1], c[2], a[3]]
}

/// Tint for an accent-lit icon toggle: hover brightens (see
/// [`accent_hover_color`]), the plain accent marks the "on" state, and
/// everything else rests dim. Shared by every player-bar toggle so the
/// three states stay identical across them.
pub fn toggle_tint(
    hover: &Signal<bool>,
    lit: &Signal<bool>,
    accent: &Signal<[f32; 4]>,
) -> Computed<[f32; 4]> {
    accent_tint(hover, lit, accent, crate::widgets::tokens::TEXT_DIM)
}

/// Accent-on-hover for a glyph with no "on" state — the top-bar chrome
/// (home, history arrows, settings), which rests at `rest` and lights to
/// the album accent under the cursor like every other icon in the app.
pub fn hover_tint(
    hover: &Signal<bool>,
    accent: &Signal<[f32; 4]>,
    rest: [f32; 4],
) -> Computed<[f32; 4]> {
    accent_tint(hover, &Signal::new(false), accent, rest)
}

/// The shared three-state rule behind [`toggle_tint`] and [`hover_tint`]:
/// hover brightens the accent, `lit` is the plain accent, everything else
/// rests at `rest`.
fn accent_tint(
    hover: &Signal<bool>,
    lit: &Signal<bool>,
    accent: &Signal<[f32; 4]>,
    rest: [f32; 4],
) -> Computed<[f32; 4]> {
    Computed::new(
        (hover.clone(), lit.clone(), accent.clone()),
        move |(h, on, acc)| {
            if h {
                accent_hover_color(&acc)
            } else if on {
                acc
            } else {
                rest
            }
        },
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn vivid_raises_a_washed_accent_and_keeps_its_hue() {
        // A muddy blue-grey: saturation and value both below the floors.
        let c = vivid([0.35, 0.38, 0.45, 1.0], 0.55, 0.95);
        let max = c[0].max(c[1]).max(c[2]);
        let min = c[0].min(c[1]).min(c[2]);
        assert!((max - 0.95).abs() < 0.01, "value floor: {max}");
        assert!((max - min) / max >= 0.54, "saturation floor");
        // Blue still dominates — the hue survived.
        assert!(c[2] > c[1] && c[1] > c[0]);
    }

    #[test]
    fn vivid_leaves_an_already_vivid_colour_alone() {
        let c = [1.0, 0.15, 0.15, 1.0];
        let out = vivid(c, 0.55, 0.95);
        // Round-trips through HSV, so compare within float slop.
        for (a, b) in out.iter().zip(c.iter()) {
            assert!((a - b).abs() < 1e-4, "{out:?} != {c:?}");
        }
    }

    #[test]
    fn vivid_only_brightens_greyscale() {
        let c = vivid([0.4, 0.4, 0.4, 1.0], 0.55, 0.95);
        assert_eq!(c, [0.95, 0.95, 0.95, 1.0]);
    }

    use super::*;
    use crate::extracted_color::ExtractedColors;

    #[test]
    fn luminance_endpoints() {
        assert!(luminance([0.0, 0.0, 0.0, 1.0]) < 1e-6);
        assert!((luminance([1.0, 1.0, 1.0, 1.0]) - 1.0).abs() < 1e-4);
    }

    #[test]
    fn contrast_black_white_is_21() {
        assert!((contrast(0.0, 1.0) - 21.0).abs() < 0.01);
    }

    #[test]
    fn lift_brightens_dark_accent_to_minimum() {
        // A deep navy — far below the floor on dark chrome.
        let lifted = lift_for_chrome([0.05, 0.05, 0.25, 1.0]);
        assert!(contrast(luminance(lifted), CHROME_LUMA) >= MIN_ACCENT_CONTRAST - 0.01);
        // Hue preserved: blue stays the dominant channel.
        assert!(lifted[2] > lifted[0] && lifted[2] > lifted[1]);
    }

    #[test]
    fn lift_keeps_passing_accent_exact() {
        let green = [0.114, 0.725, 0.329, 1.0];
        assert_eq!(lift_for_chrome(green), green);
    }

    #[test]
    fn chrome_accent_prefers_first_passing_variant() {
        let v = ExtractedColors {
            raw: Some([0.9, 0.2, 0.2, 1.0]), // bright red — passes
            light: Some([1.0, 0.8, 0.8, 1.0]),
            dark: Some([0.1, 0.02, 0.02, 1.0]),
        };
        assert_eq!(chrome_accent(&v, [0.5; 4]), [0.9, 0.2, 0.2, 1.0]);
    }

    #[test]
    fn chrome_accent_skips_dark_raw_for_passing_light() {
        let v = ExtractedColors {
            raw: Some([0.1, 0.02, 0.02, 1.0]), // too dark
            light: Some([0.95, 0.6, 0.6, 1.0]),
            dark: None,
        };
        assert_eq!(chrome_accent(&v, [0.5; 4]), [0.95, 0.6, 0.6, 1.0]);
    }

    #[test]
    fn chrome_accent_lifts_when_all_dark() {
        let v = ExtractedColors {
            raw: Some([0.12, 0.02, 0.02, 1.0]),
            light: Some([0.15, 0.05, 0.05, 1.0]),
            dark: Some([0.05, 0.01, 0.01, 1.0]),
        };
        let out = chrome_accent(&v, [0.5; 4]);
        assert!(contrast(luminance(out), CHROME_LUMA) >= MIN_ACCENT_CONTRAST - 0.01);
        assert!(out[0] > out[1] && out[0] > out[2]); // still red-dominant
    }

    #[test]
    fn accent_fg_white_on_dark_dark_on_light() {
        let acc = Signal::new([0.1, 0.1, 0.3, 1.0]);
        assert_eq!(accent_fg(&acc).read(), [1.0, 1.0, 1.0, 1.0]);
        let acc = Signal::new([0.9, 0.9, 0.5, 1.0]);
        assert_eq!(accent_fg(&acc).read(), [0.08, 0.08, 0.08, 1.0]);
    }
}
