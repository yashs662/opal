//! Pure-Rust H.264/MP4 decode for Spotify Canvas clips.
//!
//! A Canvas is a short looping video (`re_mp4` demuxes the container,
//! `openh264` decodes the H.264 stream). No system `ffmpeg` dependency.
//!
//! MP4 stores each NAL unit length-prefixed (AVCC); openh264 wants
//! start-code-prefixed Annex-B. We convert each sample to Annex-B once
//! up front and keep the (still-compressed) access units in memory — a
//! Canvas is a few seconds at a small resolution, so the encoded clip is
//! a few MB. The clip is decoded **once** (a single pass via
//! [`CanvasPlayer`]); each frame is uploaded straight to a GPU texture and
//! the loop replays from VRAM, so there's no system-RAM frame cache and no
//! per-frame CPU→GPU transfer once looping.
//!
//! Samples are fed in decode order. Spotify Canvas clips are simple
//! (baseline/main profile, no B-frames in practice) so decode order
//! matches display order; we don't reorder by composition timestamp.

use std::time::Duration;

use openh264::OpenH264API;
use openh264::decoder::{Decoder, DecoderConfig, Flush};
use openh264::formats::YUVSource;
use re_mp4::{Mp4, StsdBoxContent, TrackKind};

/// One decoded frame: tightly-packed RGBA8 (`width * height * 4`) plus how
/// long it should stay on screen before the next one.
pub struct VideoFrame {
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub duration: Duration,
}

/// Demuxed H.264 Canvas clip + an openh264 decoder. Decoded one pass via
/// [`CanvasVideo::decode_at_cursor`] (driven by [`CanvasPlayer`]); the loop
/// is replayed from GPU-resident frames, so this never re-decodes.
pub struct CanvasVideo {
    decoder: Decoder,
    /// Annex-B access units (one per sample, in decode order) + each
    /// sample's display duration.
    samples: Vec<(Vec<u8>, Duration)>,
    next: usize,
}

const START_CODE: [u8; 4] = [0, 0, 0, 1];

/// Canvas aspect the now-playing slot expects (vertical 9:16).
const CANVAS_ASPECT: f32 = 9.0 / 16.0;
/// Don't crop within this much of the target ratio (rounding slack on a
/// clip that's already ~9:16).
const ASPECT_EPS: f32 = 0.01;

/// Cap on frame height (px); taller frames are area-downscaled preserving
/// 9:16. Frames live in VRAM (one texture each), replayed by re-binding, so
/// height drives the resident VRAM, not any per-frame transfer. The budget
/// below shrinks this further when a clip has too many frames to fit.
const CANVAS_TARGET_HEIGHT: u32 = 1080;

/// Ceiling on the resident frame set (bytes of VRAM). A long or 60 fps clip
/// has enough frames to blow past [`CANVAS_TARGET_HEIGHT`]'s implied size, so
/// the effective height is shrunk to keep the whole set under this.
const CANVAS_CACHE_BUDGET: u64 = 192 * 1024 * 1024;

impl CanvasVideo {
    /// Demux `mp4_bytes`, pick the first H.264 video track, and prime the
    /// decoder with its parameter sets. `None` if the bytes don't parse,
    /// carry no AVC video track, or the decoder can't initialise.
    pub fn open(mp4_bytes: &[u8]) -> Option<Self> {
        let mp4 = Mp4::read_bytes(mp4_bytes).ok()?;

        // First AVC (H.264) video track.
        let track = mp4
            .tracks()
            .values()
            .find(|t| t.kind == Some(TrackKind::Video))?;
        let avc1 = match &track.trak(&mp4).mdia.minf.stbl.stsd.contents {
            StsdBoxContent::Avc1(avc1) => avc1,
            _ => return None, // not H.264 — we only decode AVC Canvas clips
        };

        // NAL length prefix size (1..4 bytes), and SPS/PPS → Annex-B.
        let length_size = (avc1.avcc.length_size_minus_one & 0x3) as usize + 1;
        let mut headers = Vec::new();
        for nal in avc1
            .avcc
            .sequence_parameter_sets
            .iter()
            .chain(&avc1.avcc.picture_parameter_sets)
        {
            headers.extend_from_slice(&START_CODE);
            headers.extend_from_slice(&nal.bytes);
        }
        if headers.is_empty() {
            return None;
        }

        // Convert each sample's length-prefixed NALs to Annex-B.
        let mut samples = Vec::with_capacity(track.samples.len());
        for sample in &track.samples {
            let bytes = mp4_bytes.get(sample.byte_range())?;
            let au = avcc_to_annex_b(bytes, length_size)?;
            if au.is_empty() {
                continue;
            }
            // Clamp to a sane range: a corrupt/zero timescale or a bogus
            // last-sample duration can yield 0s (busy-spin) or thousands of
            // seconds (the thread sleeps forever, freezing on one frame).
            let secs = (sample.duration as f64 / track.timescale.max(1) as f64).clamp(0.01, 1.0);
            samples.push((au, Duration::from_secs_f64(secs)));
        }
        if samples.is_empty() {
            return None;
        }

        // NoFlush: the decoder is fed a continuous stream and looped, so it
        // must keep reference-frame state between AUs. The crate default
        // (`Flush::Flush`) flushes after every decode, which corrupts that
        // state mid-stream and errors every P-frame (only I-frames survive).
        let api = OpenH264API::from_source();
        let config = DecoderConfig::new().flush_after_decode(Flush::NoFlush);
        let mut decoder = Decoder::with_api_config(api, config).ok()?;
        // Prime with parameter sets (yields no picture).
        let _ = decoder.decode(&headers);

        Some(Self {
            decoder,
            samples,
            next: 0,
        })
    }

    /// Number of demuxed samples (≈ frames) in the clip.
    pub fn frame_count(&self) -> usize {
        self.samples.len()
    }

    /// Decode the sample at the cursor and advance it, returning the cropped
    /// frame. `None` when the access unit yields no picture (need-more-data
    /// or a recoverable hiccup) — callers skip to the next sample. Caller
    /// must ensure the cursor is in range (see [`at_end`](Self::at_end)).
    fn decode_at_cursor(&mut self) -> Option<VideoFrame> {
        let idx = self.next;
        self.next += 1;
        let (au, dur) = &self.samples[idx];
        let dur = *dur;
        match self.decoder.decode(au) {
            Ok(Some(yuv)) => {
                let (w, h) = yuv.dimensions();
                let mut rgba = vec![0u8; w * h * 4];
                yuv.write_rgba8(&mut rgba);
                // Some Canvas clips ship wider than 9:16 (e.g. 948x720); the
                // official client shows a centred 9:16 crop. Match it so the
                // vertical now-playing slot doesn't squish them.
                let (rgba, cw, ch) = crop_center_portrait(rgba, w as u32, h as u32);
                Some(VideoFrame {
                    rgba,
                    width: cw,
                    height: ch,
                    duration: dur,
                })
            }
            Ok(None) => None,
            Err(_) => None,
        }
    }

    /// Cursor sits past the last sample (one full decode pass done).
    fn at_end(&self) -> bool {
        self.next >= self.samples.len()
    }
}

/// Drives the **first decode pass** of a Canvas clip for the decode thread.
/// Each call yields the next decoded + budget-downscaled frame; the thread
/// uploads it once to the GPU as a resident frame in that node's set, so the
/// pixels live in VRAM and the loop replays by re-binding a texture view —
/// no per-frame CPU→GPU transfer and no system-RAM frame cache. Returns
/// `None` at the end of the single pass (it does **not** loop): by then the
/// whole loop is GPU-resident and the thread cycles it via the frame sink.
pub struct CanvasPlayer {
    video: CanvasVideo,
    /// Budgeted frame height (area-downscaled to this) — bounds resident
    /// VRAM for long / 60 fps clips. See [`budgeted_target_height`].
    target_h: u32,
    /// First pass finished — further calls return `None`.
    done: bool,
}

impl CanvasPlayer {
    /// Demux + open the clip. Cheap — no decode here, so the thread isn't
    /// stalled and the first frame shows as soon as it decodes. `None` on
    /// the same conditions as [`CanvasVideo::open`].
    pub fn open(mp4_bytes: &[u8]) -> Option<Self> {
        let video = CanvasVideo::open(mp4_bytes)?;
        let target_h = budgeted_target_height(video.frame_count());
        Some(Self {
            video,
            target_h,
            done: false,
        })
    }

    pub fn frame_count(&self) -> usize {
        self.video.frame_count()
    }

    /// Next frame of the first pass, downscaled to the budgeted height.
    /// `None` once the pass ends (no looping). Skips no-picture samples
    /// within the call so the cadence isn't broken.
    pub fn next_pass_frame(&mut self) -> Option<VideoFrame> {
        if self.done {
            return None;
        }
        loop {
            if self.video.at_end() {
                self.done = true;
                return None;
            }
            let Some(f) = self.video.decode_at_cursor() else {
                continue;
            };
            let (rgba, w, h) = downscale_to_height(f.rgba, f.width, f.height, self.target_h);
            return Some(VideoFrame {
                rgba,
                width: w,
                height: h,
                duration: f.duration,
            });
        }
    }
}

/// Largest frame height (≤ [`CANVAS_TARGET_HEIGHT`]) whose total resident
/// VRAM for `frame_count` 9:16 frames stays under [`CANVAS_CACHE_BUDGET`].
/// Scales height by `sqrt(budget / size_at_cap)` since size grows with
/// height² (9:16 → width ∝ height). Floored so it never collapses to mush.
fn budgeted_target_height(frame_count: usize) -> u32 {
    let cap = CANVAS_TARGET_HEIGHT;
    if frame_count == 0 {
        return cap;
    }
    let w = (cap as u64 * 9 / 16).max(1);
    let at_cap = frame_count as u64 * w * cap as u64 * 4;
    if at_cap <= CANVAS_CACHE_BUDGET {
        return cap;
    }
    let scale = (CANVAS_CACHE_BUDGET as f64 / at_cap as f64).sqrt();
    ((cap as f64 * scale) as u32).max(180)
}

/// Area-average downscale tightly-packed RGBA8 to `target_h` (preserving
/// aspect), never upscaling. Returns the source unchanged when it's already
/// no taller than `target_h`. Each destination pixel averages the source box
/// it covers — run once per frame at preload, so quality over speed.
fn downscale_to_height(src: Vec<u8>, sw: u32, sh: u32, target_h: u32) -> (Vec<u8>, u32, u32) {
    if sh <= target_h || sh == 0 || sw == 0 {
        return (src, sw, sh);
    }
    let dh = target_h;
    let dw = ((sw as u64 * dh as u64) / sh as u64).max(1) as u32;
    let mut out = vec![0u8; (dw * dh * 4) as usize];
    for dy in 0..dh {
        let sy0 = (dy as u64 * sh as u64 / dh as u64) as u32;
        let sy1 = ((((dy + 1) as u64 * sh as u64 / dh as u64) as u32).max(sy0 + 1)).min(sh);
        for dx in 0..dw {
            let sx0 = (dx as u64 * sw as u64 / dw as u64) as u32;
            let sx1 = ((((dx + 1) as u64 * sw as u64 / dw as u64) as u32).max(sx0 + 1)).min(sw);
            let (mut r, mut g, mut b, mut a, mut n) = (0u32, 0u32, 0u32, 0u32, 0u32);
            for sy in sy0..sy1 {
                let row = (sy * sw * 4) as usize;
                for sx in sx0..sx1 {
                    let o = row + (sx * 4) as usize;
                    r += src[o] as u32;
                    g += src[o + 1] as u32;
                    b += src[o + 2] as u32;
                    a += src[o + 3] as u32;
                    n += 1;
                }
            }
            let n = n.max(1);
            let o = ((dy * dw + dx) * 4) as usize;
            out[o] = (r / n) as u8;
            out[o + 1] = (g / n) as u8;
            out[o + 2] = (b / n) as u8;
            out[o + 3] = (a / n) as u8;
        }
    }
    (out, dw, dh)
}

/// Centre-crop tightly-packed RGBA8 to a 9:16 portrait rect, matching the
/// official client's handling of non-portrait Canvas clips. Returns the
/// source unchanged when it's already ~9:16. Crops the wider axis: too-wide
/// clips lose left/right, too-tall clips lose top/bottom.
fn crop_center_portrait(src: Vec<u8>, w: u32, h: u32) -> (Vec<u8>, u32, u32) {
    let cur = w as f32 / h as f32;
    if (cur - CANVAS_ASPECT).abs() < ASPECT_EPS {
        return (src, w, h);
    }
    let (cw, ch) = if cur > CANVAS_ASPECT {
        ((h as f32 * CANVAS_ASPECT).round() as u32, h)
    } else {
        (w, (w as f32 / CANVAS_ASPECT).round() as u32)
    };
    let cw = cw.clamp(1, w);
    let ch = ch.clamp(1, h);
    let x0 = (w - cw) / 2;
    let y0 = (h - ch) / 2;
    let row_bytes = (cw * 4) as usize;
    let mut out = vec![0u8; row_bytes * ch as usize];
    for row in 0..ch {
        let src_off = (((y0 + row) * w + x0) * 4) as usize;
        let dst_off = row as usize * row_bytes;
        out[dst_off..dst_off + row_bytes].copy_from_slice(&src[src_off..src_off + row_bytes]);
    }
    (out, cw, ch)
}

/// Rewrite a length-prefixed (AVCC) sample into start-code-prefixed
/// (Annex-B) bytes. `None` if a declared NAL length runs past the buffer.
fn avcc_to_annex_b(mut buf: &[u8], length_size: usize) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(buf.len() + 8);
    while buf.len() >= length_size {
        let mut len = 0usize;
        for &b in &buf[..length_size] {
            len = (len << 8) | b as usize;
        }
        buf = &buf[length_size..];
        let nal = buf.get(..len)?;
        out.extend_from_slice(&START_CODE);
        out.extend_from_slice(nal);
        buf = &buf[len..];
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn avcc_single_nal_to_annex_b() {
        // 4-byte length (3) + payload AB CD EF.
        let sample = [0, 0, 0, 3, 0xAB, 0xCD, 0xEF];
        let out = avcc_to_annex_b(&sample, 4).unwrap();
        assert_eq!(out, [0, 0, 0, 1, 0xAB, 0xCD, 0xEF]);
    }

    #[test]
    fn avcc_two_nals_to_annex_b() {
        let sample = [0, 0, 0, 2, 0x11, 0x22, 0, 0, 0, 1, 0x33];
        let out = avcc_to_annex_b(&sample, 4).unwrap();
        assert_eq!(out, [0, 0, 0, 1, 0x11, 0x22, 0, 0, 0, 1, 0x33]);
    }

    #[test]
    fn avcc_truncated_length_is_none() {
        // Declares 10 bytes but only 2 follow.
        let sample = [0, 0, 0, 10, 0xAB, 0xCD];
        assert!(avcc_to_annex_b(&sample, 4).is_none());
    }

    #[test]
    fn portrait_clip_not_cropped() {
        let (px, w, h) = crop_center_portrait(vec![7u8; 9 * 16 * 4], 9, 16);
        assert_eq!((w, h), (9, 16));
        assert_eq!(px.len(), 9 * 16 * 4);
    }

    #[test]
    fn wide_clip_cropped_to_portrait_width() {
        // 948x720 → keep height, width = 720 * 9/16 = 405, centred.
        let (px, w, h) = crop_center_portrait(vec![0u8; 948 * 720 * 4], 948, 720);
        assert_eq!((w, h), (405, 720));
        assert_eq!(px.len(), (405 * 720 * 4) as usize);
    }

    #[test]
    fn crop_takes_center_columns() {
        // 4x2, target portrait → cw = (2 * 9/16).round() = 1, x0 = 1.
        // Tag each pixel's first byte with its column so we can check which
        // column survived.
        let (w, h) = (4u32, 2u32);
        let mut src = vec![0u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                src[((y * w + x) * 4) as usize] = x as u8;
            }
        }
        let (px, cw, ch) = crop_center_portrait(src, w, h);
        assert_eq!((cw, ch), (1, 2));
        assert_eq!(px[0], 1); // (4 - 1) / 2 = 1
    }

    #[test]
    fn budget_keeps_cap_for_short_clip() {
        // A handful of frames fits the budget at any sane cap → full cap.
        assert_eq!(budgeted_target_height(10), CANVAS_TARGET_HEIGHT);
    }

    #[test]
    fn budget_shrinks_height_for_long_clip() {
        // A long clip (240+ frames at a 1080 cap) exceeds budget → drops.
        let h = budgeted_target_height(480);
        assert!(h < CANVAS_TARGET_HEIGHT, "expected shrink, got {h}");
        assert!(h >= 180, "floored, got {h}");
        // Resulting cache must actually fit the budget.
        let w = (h as u64 * 9 / 16).max(1);
        assert!(480 * w * h as u64 * 4 <= CANVAS_CACHE_BUDGET);
    }

    #[test]
    fn downscale_keeps_small_frames() {
        let (px, w, h) = downscale_to_height(vec![5u8; 270 * 480 * 4], 270, 480, 480);
        assert_eq!((w, h), (270, 480));
        assert_eq!(px.len(), 270 * 480 * 4);
    }

    #[test]
    fn downscale_caps_height_and_keeps_aspect() {
        // 405x720 capped at 480 → 270x480 (9:16 preserved).
        let (px, w, h) = downscale_to_height(vec![0u8; 405 * 720 * 4], 405, 720, 480);
        assert_eq!((w, h), (270, 480));
        assert_eq!(px.len(), (270 * 480 * 4) as usize);
    }

    #[test]
    fn downscale_averages_block() {
        // 2x2 solid value → 1x1 keeps the average (here, identical pixels).
        let src = vec![
            10, 20, 30, 40, 10, 20, 30, 40, 10, 20, 30, 40, 10, 20, 30, 40,
        ];
        let (px, w, h) = downscale_to_height(src, 2, 2, 1);
        assert_eq!((w, h), (1, 1));
        assert_eq!(px, vec![10, 20, 30, 40]);
    }

    #[test]
    fn open_garbage_is_none() {
        assert!(CanvasVideo::open(&[0xFF; 16]).is_none());
    }
}
