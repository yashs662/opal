//! Album-art fetch + decode helpers.
//!
//! Spotify serves album art at `https://i.scdn.co/image/<sha256-hex>`
//! in JPEG (occasionally PNG). The trailing hex doubles as a perfect
//! cache key. Decode → RGBA → opal-gfx atlas via
//! `opal_gfx::Uploader::upload_rgba` from the worker thread.

use crate::widgets::tokens;
use image::ImageReader;
use std::io::Cursor;

/// Brightness floor for the accent-extraction filter. Pixels whose
/// brightest channel falls below this are treated as shadow and
/// skipped — they otherwise bias the mean toward muddy desaturated
/// greys. Using `max(r, g, b)` (HSV's Value) rather than Rec.601 luma
/// matters for vibrant pure-colour covers: a fully saturated red has
/// luma ≈ 0.30 and would be skipped under a luma threshold, even
/// though it is the dominant accent.
const ACCENT_BRIGHTNESS_FLOOR: f32 = 0.4;

/// Cache key for an album-art URL. Strips the `i.scdn.co/image/` prefix
/// to use just the hex hash — same key works regardless of CDN host
/// reshuffles.
pub fn cache_key(url: &str) -> String {
    url.rsplit('/').next().unwrap_or(url).to_string()
}

/// Decode JPEG/PNG bytes into raw RGBA8. Returns `(width, height, rgba)`.
/// Caps the longest side at `max_dim` px — the atlas is ~1024² so a
/// raw 640² Spotify cover would dominate; 256² is plenty for our 96px
/// display + retina headroom and lets the atlas hold many tracks.
pub fn decode_to_rgba(bytes: &[u8], max_dim: u32) -> Option<(u32, u32, Vec<u8>)> {
    let reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    let img = reader.decode().ok()?;
    let (w, h) = (img.width(), img.height());
    let img = if w.max(h) > max_dim {
        let (nw, nh) = if w >= h {
            (
                max_dim,
                (h as f32 * max_dim as f32 / w as f32).round() as u32,
            )
        } else {
            (
                (w as f32 * max_dim as f32 / h as f32).round() as u32,
                max_dim,
            )
        };
        img.resize_exact(nw, nh, image::imageops::FilterType::Triangle)
    } else {
        img
    };
    let rgba = img.to_rgba8();
    Some((rgba.width(), rgba.height(), rgba.into_raw()))
}

/// Dominant accent colour for a decoded album cover. v1 algorithm:
/// average the RGB of every pixel whose brightest channel clears
/// `ACCENT_BRIGHTNESS_FLOOR` — skips shadows so the mean isn't pulled
/// toward muddy greys. Falls back to `tokens::ACCENT` when the cover is
/// either too dark to find any bright pixels or has no pixels at all
/// (e.g. an unexpected zero-sized decode). Alpha is always 1.0.
pub fn extract_accent(rgba: &[u8], _w: u32, _h: u32) -> [f32; 4] {
    let mut sum = [0.0_f32; 3];
    let mut n: u32 = 0;
    for px in rgba.chunks_exact(4) {
        let r = px[0] as f32 / 255.0;
        let g = px[1] as f32 / 255.0;
        let b = px[2] as f32 / 255.0;
        let brightness = r.max(g).max(b);
        if brightness >= ACCENT_BRIGHTNESS_FLOOR {
            sum[0] += r;
            sum[1] += g;
            sum[2] += b;
            n += 1;
        }
    }
    if n == 0 {
        return tokens::ACCENT;
    }
    let inv = 1.0 / n as f32;
    [sum[0] * inv, sum[1] * inv, sum[2] * inv, 1.0]
}

/// Mean WCAG relative luminance of the whole cover (0 = black, 1 =
/// white) — how *bright* the blurred ambient backdrop built from it will
/// read. Drives the adaptive glass dim: bright covers get a stronger
/// tint so the chrome on top keeps its contrast. All pixels count (no
/// shadow filter — shadows are exactly what make a backdrop dark).
pub fn mean_luminance(rgba: &[u8]) -> f32 {
    let mut sum = 0.0_f32;
    let mut n: u32 = 0;
    for px in rgba.chunks_exact(4) {
        sum += crate::widgets::color::luminance([
            px[0] as f32 / 255.0,
            px[1] as f32 / 255.0,
            px[2] as f32 / 255.0,
            1.0,
        ]);
        n += 1;
    }
    if n == 0 { 0.0 } else { sum / n as f32 }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_rgba(pixels: &[[u8; 4]]) -> Vec<u8> {
        pixels.iter().flatten().copied().collect()
    }

    #[test]
    fn extract_accent_skips_shadows() {
        // Pure red over a black field — the average MUST converge on red,
        // not on the muddy mid-grey you'd get if shadows weren't skipped.
        let pixels: Vec<[u8; 4]> = std::iter::repeat_n([0, 0, 0, 255], 9)
            .chain(std::iter::once([255, 0, 0, 255]))
            .collect();
        let rgba = make_rgba(&pixels);
        let accent = extract_accent(&rgba, 10, 1);
        assert!(
            (accent[0] - 1.0).abs() < 1e-3,
            "red channel ~1.0 got {}",
            accent[0]
        );
        assert!(accent[1].abs() < 1e-3);
        assert!(accent[2].abs() < 1e-3);
    }

    #[test]
    fn extract_accent_all_dark_falls_back_to_theme() {
        // Every pixel below the luminance floor — expect the static
        // theme accent, not a divide-by-zero.
        let rgba = make_rgba(&[[10, 10, 10, 255]; 4]);
        let accent = extract_accent(&rgba, 2, 2);
        assert_eq!(accent, tokens::ACCENT);
    }
}
