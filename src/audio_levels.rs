//! Live spectrum of what Opal is playing — the signal the lyric shaders
//! move to.
//!
//! Same split as the EQ (see [`crate::audio_eq`]): the audio thread owns
//! the DSP ([`LevelAnalyzer`]) and publishes through a lock-free
//! [`AudioLevels`]; the frame loop reads it and hands it to the engine,
//! which uploads it once per frame in the shader globals. No mutex, no
//! channel, no allocation on the playback thread.
//!
//! The analyzer sits **after** the EQ, on the samples that actually reach
//! the speakers — a boosted bass band shows up in the animation the way it
//! does in the room.
//!
//! It only sees audio Opal itself decodes. With the hidden official client
//! as the playback engine (see `official_app`) the samples never pass
//! through this process, so the levels stay at zero and every effect falls
//! back to its clock-driven motion.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use crate::audio_eq::Biquad;

/// Bands published to the shaders. Matches `opal_gfx`'s `fx_band` bound —
/// the shader indexes 0..BANDS, so the two must agree.
pub const BANDS: usize = 8;

/// Band centre frequencies (Hz), octave-spaced from the low end to
/// presence. Deliberately not the EQ's ten: eight is what the shader
/// carries, and a lyric animation wants "kick / body / voice / air", not a
/// mastering-grade analyzer.
const BAND_FREQS: [f64; BANDS] = [60.0, 120.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0, 8000.0];
/// Band width. Wider than the EQ's octave Q so neighbouring bands overlap
/// and a note sliding between them reads as movement rather than a jump
/// from one band to the next.
const BAND_Q: f64 = 1.2;

/// Envelope times. The rise is fast enough that a kick lands on the beat,
/// the fall slow enough that the letters settle instead of flickering.
const ATTACK_SECS: f64 = 0.012;
const RELEASE_SECS: f64 = 0.16;

/// Full-scale reference for the log mapping: how far below 0 dBFS a band
/// can sit and still register. Quiet passages then still move the words,
/// which a linear mapping (everything hugging zero) does not.
const FLOOR_DB: f64 = -55.0;

/// How often the analyzer publishes, in seconds of audio. The UI reads it
/// at most once a frame, so pushing more often is wasted work; pushing
/// less often steps visibly.
const PUBLISH_SECS: f64 = 1.0 / 120.0;

/// Lock-free hand-off from the playback thread to the frame loop: a
/// broadband level plus per-band energy, all 0..1.
#[derive(Debug)]
pub struct AudioLevels {
    /// `f32::to_bits` of the broadband level.
    level: AtomicU32,
    bands: [AtomicU32; BANDS],
}

impl AudioLevels {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            level: AtomicU32::new(0),
            bands: std::array::from_fn(|_| AtomicU32::new(0)),
        })
    }

    /// Publish one analysis frame. `Relaxed` throughout: each value stands
    /// alone, and a reader that sees a band from one frame next to a band
    /// from the next is visually indistinguishable.
    pub fn publish(&self, level: f32, bands: [f32; BANDS]) {
        self.level.store(level.to_bits(), Ordering::Relaxed);
        for (slot, v) in self.bands.iter().zip(bands) {
            slot.store(v.to_bits(), Ordering::Relaxed);
        }
    }

    /// Drop everything to zero — playback stopped, so the effects should
    /// coast back to their clock-driven rest rather than freezing on the
    /// last frame of the song.
    pub fn silence(&self) {
        self.publish(0.0, [0.0; BANDS]);
    }

    /// The latest frame, for the frame loop.
    pub fn read(&self) -> (f32, [f32; BANDS]) {
        (
            f32::from_bits(self.level.load(Ordering::Relaxed)),
            std::array::from_fn(|i| f32::from_bits(self.bands[i].load(Ordering::Relaxed))),
        )
    }
}

/// The playback-thread half: a band-pass per band, each feeding an
/// envelope follower, published as normalised 0..1 energies.
pub struct LevelAnalyzer {
    shared: Arc<AudioLevels>,
    filters: [Biquad; BANDS],
    /// Smoothed energy per band, and broadband — in linear amplitude,
    /// mapped to 0..1 only at publish time.
    env: [f64; BANDS],
    level_env: f64,
    /// Per-sample envelope coefficients, derived from the sample rate.
    attack: f64,
    release: f64,
    /// Samples until the next publish, so the store isn't hit per sample.
    countdown: u32,
    publish_every: u32,
}

impl LevelAnalyzer {
    pub fn new(shared: Arc<AudioLevels>, sample_rate: u32) -> Self {
        let fs = sample_rate as f64;
        let mut filters = [Biquad::identity(); BANDS];
        for (f, centre) in filters.iter_mut().zip(BAND_FREQS) {
            f.set_bandpass(centre, BAND_Q, fs);
        }
        Self {
            shared,
            filters,
            env: [0.0; BANDS],
            level_env: 0.0,
            attack: coefficient(ATTACK_SECS, fs),
            release: coefficient(RELEASE_SECS, fs),
            countdown: 0,
            publish_every: (fs * PUBLISH_SECS) as u32,
        }
    }

    /// Analyse one interleaved packet. Channels are summed to mono first:
    /// the effects want the energy of the mix, and it halves the filtering.
    pub fn feed(&mut self, samples: &[f64], channels: usize) {
        if channels == 0 {
            return;
        }
        for frame in samples.chunks_exact(channels) {
            let mono = frame.iter().sum::<f64>() / channels as f64;
            self.level_env = follow(self.level_env, mono.abs(), self.attack, self.release);
            for (band, filter) in self.env.iter_mut().zip(self.filters.iter_mut()) {
                let y = filter.process(mono).abs();
                *band = follow(*band, y, self.attack, self.release);
            }
            if self.countdown == 0 {
                self.countdown = self.publish_every.max(1);
                // Band-pass output of a mixed track sits well below the
                // broadband peak, so each band is normalised against its
                // own scale rather than the mix's.
                let bands = std::array::from_fn(|i| normalize(self.env[i] * BAND_GAIN));
                self.shared.publish(normalize(self.level_env), bands);
            }
            self.countdown -= 1;
        }
    }
}

/// Make-up applied to a band envelope before normalising. One band of a
/// mix carries a fraction of its energy; without this the bands would sit
/// near the floor while the broadband level looked healthy.
const BAND_GAIN: f64 = 4.0;

/// One-pole smoothing coefficient for a time constant of `secs`.
fn coefficient(secs: f64, fs: f64) -> f64 {
    (-1.0 / (secs * fs)).exp()
}

/// Envelope follower: jump toward a rising signal, ease away from a
/// falling one.
fn follow(state: f64, x: f64, attack: f64, release: f64) -> f64 {
    let c = if x > state { attack } else { release };
    x + c * (state - x)
}

/// Linear amplitude → 0..1, on a dB scale from [`FLOOR_DB`] to full
/// scale. Perceptual: the words then respond to a quiet verse instead of
/// waiting for the chorus.
fn normalize(amp: f64) -> f32 {
    if amp <= 1e-6 {
        return 0.0;
    }
    let db = 20.0 * amp.log10();
    (((db - FLOOR_DB) / -FLOOR_DB).clamp(0.0, 1.0)) as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Feed `secs` of a sine at `freq` and return the published frame.
    fn analyse(freq: f64, amplitude: f64, secs: f64) -> (f32, [f32; BANDS]) {
        let fs = 44_100u32;
        let shared = AudioLevels::new();
        let mut a = LevelAnalyzer::new(shared.clone(), fs);
        let n = (fs as f64 * secs) as usize;
        let samples: Vec<f64> = (0..n)
            .map(|i| {
                let t = i as f64 / fs as f64;
                amplitude * (std::f64::consts::TAU * freq * t).sin()
            })
            .collect();
        a.feed(&samples, 1);
        shared.read()
    }

    #[test]
    fn a_bass_tone_lands_in_the_low_bands() {
        let (level, bands) = analyse(60.0, 0.8, 0.5);
        assert!(level > 0.5, "broadband level should register: {level}");
        let low = bands[0].max(bands[1]);
        let high = bands[6].max(bands[7]);
        assert!(low > high, "60 Hz should read low, not high: {bands:?}");
    }

    #[test]
    fn a_treble_tone_lands_in_the_high_bands() {
        let (_, bands) = analyse(8000.0, 0.8, 0.5);
        let low = bands[0].max(bands[1]);
        let high = bands[6].max(bands[7]);
        assert!(high > low, "8 kHz should read high, not low: {bands:?}");
    }

    #[test]
    fn silence_publishes_nothing() {
        let shared = AudioLevels::new();
        let mut a = LevelAnalyzer::new(shared.clone(), 44_100);
        a.feed(&vec![0.0; 4410], 1);
        let (level, bands) = shared.read();
        assert_eq!(level, 0.0);
        assert!(bands.iter().all(|&b| b == 0.0));
    }

    #[test]
    fn stopping_clears_the_last_frame() {
        let shared = AudioLevels::new();
        shared.publish(0.9, [0.9; BANDS]);
        shared.silence();
        assert_eq!(shared.read(), (0.0, [0.0; BANDS]));
    }
}
