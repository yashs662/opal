//! 10-band graphic equaliser — RBJ peaking biquads applied in the sink's
//! f64 path, before the single f64→f32 conversion to the OS mixer.
//!
//! Two halves:
//! - [`EqShared`] — lock-free control surface. The UI thread writes band
//!   gains / enabled through atomics and bumps a generation counter; the
//!   audio thread reads them with `Relaxed` loads. No mutex ever touches
//!   the playback thread.
//! - [`EqProcessor`] — the per-sink DSP state (a cascade of ten biquads
//!   per channel). It recomputes coefficients only when the generation
//!   changes, so steady-state playback is just the biquad multiplies.
//!
//! Cost/quality: biquads are IIR, so there is **zero added latency** and
//! the maths runs in the same f64 the decoder already produces. When the
//! EQ is disabled — or enabled but perfectly flat — processing is skipped
//! entirely, so an untouched EQ is free.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

/// Number of graphic-EQ bands.
pub const NUM_BANDS: usize = 10;

/// ISO octave-spaced band centre frequencies (Hz) — the standard 10-band
/// graphic-EQ layout (31 Hz … 16 kHz).
pub const BAND_FREQS: [f32; NUM_BANDS] = [
    31.0, 62.0, 125.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0, 8000.0, 16000.0,
];

/// Short axis labels for the UI (`"31"`, `"1k"`, `"16k"`).
pub const BAND_LABELS: [&str; NUM_BANDS] = [
    "31", "62", "125", "250", "500", "1k", "2k", "4k", "8k", "16k",
];

/// Per-band gain range, ±dB. The sliders and presets clamp to this.
pub const GAIN_DB_MAX: f32 = 12.0;

/// Q for octave-wide peaking filters. Q = √2 gives roughly one-octave
/// bandwidth at the -3 dB points, so adjacent bands overlap smoothly
/// rather than leaving dips or piling up.
const BAND_Q: f64 = std::f64::consts::SQRT_2;

/// Lock-free EQ control surface, shared (`Arc`) between the UI thread and
/// the audio thread. Gains are stored as `f32` bit patterns in `AtomicU32`;
/// `generation` bumps on any change so the audio thread knows to reload +
/// recompute without polling every field each packet.
pub struct EqShared {
    enabled: AtomicBool,
    /// Per-band gain in dB, as `f32::to_bits`.
    gains: [AtomicU32; NUM_BANDS],
    /// Incremented on every mutation; the processor recomputes coefficients
    /// when its cached value falls behind.
    generation: AtomicU64,
}

impl EqShared {
    pub fn new(enabled: bool, gains_db: [f32; NUM_BANDS]) -> Arc<Self> {
        Arc::new(Self {
            enabled: AtomicBool::new(enabled),
            gains: std::array::from_fn(|i| AtomicU32::new(gains_db[i].to_bits())),
            generation: AtomicU64::new(1),
        })
    }

    /// Set one band's gain (dB, clamped to ±[`GAIN_DB_MAX`]) and bump the
    /// generation so the audio thread picks it up.
    pub fn set_band(&self, i: usize, gain_db: f32) {
        if i >= NUM_BANDS {
            return;
        }
        let g = gain_db.clamp(-GAIN_DB_MAX, GAIN_DB_MAX);
        self.gains[i].store(g.to_bits(), Ordering::Relaxed);
        self.bump();
    }

    /// Replace every band gain at once (preset apply).
    pub fn set_all(&self, gains_db: &[f32; NUM_BANDS]) {
        for (i, g) in gains_db.iter().enumerate() {
            let g = g.clamp(-GAIN_DB_MAX, GAIN_DB_MAX);
            self.gains[i].store(g.to_bits(), Ordering::Relaxed);
        }
        self.bump();
    }

    pub fn set_enabled(&self, on: bool) {
        self.enabled.store(on, Ordering::Relaxed);
        self.bump();
    }

    fn bump(&self) {
        self.generation.fetch_add(1, Ordering::Release);
    }

    /// Current generation — the processor compares this against its cached
    /// value to decide whether to reload.
    fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    fn gains(&self) -> [f32; NUM_BANDS] {
        std::array::from_fn(|i| f32::from_bits(self.gains[i].load(Ordering::Relaxed)))
    }
}

/// One RBJ peaking biquad in transposed Direct-Form II (good f64 numerical
/// behaviour, one add of state per sample).
#[derive(Clone, Copy)]
struct Biquad {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
    z1: f64,
    z2: f64,
}

impl Biquad {
    /// Identity (unity passthrough).
    const fn identity() -> Self {
        Self {
            b0: 1.0,
            b1: 0.0,
            b2: 0.0,
            a1: 0.0,
            a2: 0.0,
            z1: 0.0,
            z2: 0.0,
        }
    }

    /// RBJ "cookbook" peaking-EQ coefficients (normalised by a0). Preserves
    /// the running state `z1/z2` so a live gain tweak doesn't click.
    fn set_peaking(&mut self, f0: f64, q: f64, gain_db: f64, fs: f64) {
        let a = 10f64.powf(gain_db / 40.0);
        let w0 = 2.0 * std::f64::consts::PI * f0 / fs;
        let (sin_w0, cos_w0) = w0.sin_cos();
        let alpha = sin_w0 / (2.0 * q);
        let a0 = 1.0 + alpha / a;
        self.b0 = (1.0 + alpha * a) / a0;
        self.b1 = (-2.0 * cos_w0) / a0;
        self.b2 = (1.0 - alpha * a) / a0;
        self.a1 = (-2.0 * cos_w0) / a0;
        self.a2 = (1.0 - alpha / a) / a0;
    }

    #[inline]
    fn process(&mut self, x: f64) -> f64 {
        let y = self.b0 * x + self.z1;
        self.z1 = self.b1 * x - self.a1 * y + self.z2;
        self.z2 = self.b2 * x - self.a2 * y;
        y
    }

    fn reset_state(&mut self) {
        self.z1 = 0.0;
        self.z2 = 0.0;
    }

    /// Magnitude of the transfer function |H(e^jω)| at `freq` (linear).
    /// Evaluates the normalised biquad (a0 = 1) on the unit circle.
    fn magnitude_at(&self, freq: f64, fs: f64) -> f64 {
        let w = 2.0 * std::f64::consts::PI * freq / fs;
        let (s1, c1) = w.sin_cos();
        let (s2, c2) = (2.0 * w).sin_cos();
        // Numerator b0 + b1 e^-jw + b2 e^-2jw, denominator 1 + a1 e^-jw + a2 e^-2jw.
        let num_re = self.b0 + self.b1 * c1 + self.b2 * c2;
        let num_im = -(self.b1 * s1 + self.b2 * s2);
        let den_re = 1.0 + self.a1 * c1 + self.a2 * c2;
        let den_im = -(self.a1 * s1 + self.a2 * s2);
        let num = (num_re * num_re + num_im * num_im).sqrt();
        let den = (den_re * den_re + den_im * den_im).sqrt();
        if den > 0.0 { num / den } else { 1.0 }
    }
}

/// Combined EQ magnitude response in dB at `freq` for the given band
/// gains — the sum (in dB) of every peaking band's response. This is the
/// *tone shape* the EQ imparts (auto-headroom, a uniform makeup trim, is
/// deliberately excluded so the curve reads as boosts/cuts, not an
/// overall level drop). Overlapping octave bands sum to a smooth curve,
/// which is what the settings visualization draws.
pub fn response_db(gains: &[f32; NUM_BANDS], freq: f64, fs: f64) -> f64 {
    let mut total_db = 0.0;
    for (b, &g) in gains.iter().enumerate() {
        if g.abs() < 1e-3 {
            continue;
        }
        let mut bq = Biquad::identity();
        bq.set_peaking(BAND_FREQS[b] as f64, BAND_Q, g as f64, fs);
        total_db += 20.0 * bq.magnitude_at(freq, fs).log10();
    }
    total_db
}

/// Per-sink DSP state: a cascade of [`NUM_BANDS`] biquads for each of the
/// two channels, plus the current pre-gain (auto-headroom). Reads its
/// control values from the shared [`EqShared`], recomputing coefficients
/// only when the generation advances.
pub struct EqProcessor {
    shared: Arc<EqShared>,
    fs: f64,
    gen_seen: u64,
    active: bool,
    /// Pre-gain applied before the cascade so summed band boosts can't push
    /// peaks into clipping (10^(-maxBoost/20)); 1.0 when nothing is boosted.
    headroom: f64,
    /// `[channel][band]`.
    bands: [[Biquad; NUM_BANDS]; 2],
}

impl EqProcessor {
    pub fn new(shared: Arc<EqShared>, sample_rate: u32) -> Self {
        let mut me = Self {
            shared,
            fs: sample_rate as f64,
            gen_seen: 0,
            active: false,
            headroom: 1.0,
            bands: [[Biquad::identity(); NUM_BANDS]; 2],
        };
        me.reload();
        me
    }

    /// Recompute coefficients + headroom from the shared control values.
    /// `active` is false when the EQ is disabled or perfectly flat, so
    /// [`process_interleaved`](Self::process_interleaved) can early-out.
    fn reload(&mut self) {
        self.gen_seen = self.shared.generation();
        let enabled = self.shared.is_enabled();
        let gains = self.shared.gains();
        let any_boost = gains.iter().cloned().fold(0.0f32, f32::max).max(0.0);
        let flat = gains.iter().all(|g| g.abs() < 1e-3);
        self.active = enabled && !flat;
        if !self.active {
            return;
        }
        // Attenuate ahead of the cascade by the largest single-band boost so
        // that band alone can't clip; overlapping boosts still lean on the
        // final safety clamp, but this covers the common case cheaply.
        self.headroom = 10f64.powf(-(any_boost as f64) / 20.0);
        for ch in 0..2 {
            for b in 0..NUM_BANDS {
                self.bands[ch][b].set_peaking(
                    BAND_FREQS[b] as f64,
                    BAND_Q,
                    gains[b] as f64,
                    self.fs,
                );
            }
        }
    }

    /// Apply the EQ in place to an interleaved stereo f64 buffer. Cheap
    /// no-op when disabled/flat. Picks up control changes at packet
    /// granularity (a fresh generation triggers one coefficient recompute).
    pub fn process_interleaved(&mut self, samples: &mut [f64]) {
        if self.shared.generation() != self.gen_seen {
            let was_active = self.active;
            self.reload();
            // Entering active from idle: clear stale state so a long-parked
            // filter doesn't thump on the first boosted sample.
            if self.active && !was_active {
                for cascade in &mut self.bands {
                    for bq in cascade.iter_mut() {
                        bq.reset_state();
                    }
                }
            }
        }
        if !self.active {
            return;
        }
        for frame in samples.chunks_mut(2) {
            for (ch, s) in frame.iter_mut().enumerate() {
                let mut x = *s * self.headroom;
                for bq in &mut self.bands[ch] {
                    x = bq.process(x);
                }
                // Final safety clamp: overlapping boosts can still exceed the
                // per-band headroom estimate; a hard clamp beats wrap/NaN
                // reaching the DAC.
                *s = x.clamp(-1.0, 1.0);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FS: u32 = 44_100;

    /// A flat (all-zero) EQ leaves samples untouched — bit-exact passthrough.
    #[test]
    fn flat_is_passthrough() {
        let shared = EqShared::new(true, [0.0; NUM_BANDS]);
        let mut eq = EqProcessor::new(shared, FS);
        let mut buf: Vec<f64> = (0..512).map(|i| (i as f64 * 0.01).sin() * 0.5).collect();
        let orig = buf.clone();
        eq.process_interleaved(&mut buf);
        assert_eq!(buf, orig, "flat EQ must not alter the signal");
    }

    /// A disabled EQ is a no-op even with non-zero gains staged.
    #[test]
    fn disabled_is_passthrough() {
        let shared = EqShared::new(false, [GAIN_DB_MAX; NUM_BANDS]);
        let mut eq = EqProcessor::new(shared, FS);
        let mut buf: Vec<f64> = (0..256).map(|i| (i as f64).cos() * 0.3).collect();
        let orig = buf.clone();
        eq.process_interleaved(&mut buf);
        assert_eq!(buf, orig);
    }

    /// Steady-state RMS of a `f0` sine driven through one peaking biquad.
    fn band_rms(f0: f64, gain_db: f64) -> f64 {
        let mut bq = Biquad::identity();
        bq.set_peaking(f0, BAND_Q, gain_db, FS as f64);
        let n = 16384;
        // Discard the transient, measure the settled tail.
        let mut acc = 0.0;
        let mut count = 0;
        for i in 0..n {
            let x = (2.0 * std::f64::consts::PI * f0 * i as f64 / FS as f64).sin() * 0.4;
            let y = bq.process(x);
            if i >= n / 2 {
                acc += y * y;
                count += 1;
            }
        }
        (acc / count as f64).sqrt()
    }

    /// A peaking biquad boosts a tone at its centre frequency and cuts it
    /// by the symmetric amount — the core filter maths (headroom aside).
    #[test]
    fn peaking_biquad_boosts_and_cuts() {
        let f0 = BAND_FREQS[5] as f64; // 1 kHz
        let flat = band_rms(f0, 0.0);
        let boosted = band_rms(f0, GAIN_DB_MAX as f64);
        let cut = band_rms(f0, -(GAIN_DB_MAX as f64));
        // +12 dB ≈ ×3.98 at the centre; allow wide slack for the finite tail.
        assert!(boosted > flat * 3.0, "boost {boosted} vs flat {flat}");
        assert!(cut < flat * 0.35, "cut {cut} vs flat {flat}");
    }

    /// Auto-headroom keeps a single max-boosted band from raising the
    /// absolute peak (a pure tone at that band nets ~unity) — the
    /// anti-clip guarantee, verified end-to-end through the processor.
    #[test]
    fn headroom_prevents_single_band_clip() {
        let f0 = BAND_FREQS[3] as f64; // 250 Hz
        let mut gains = [0.0; NUM_BANDS];
        gains[3] = GAIN_DB_MAX;
        let shared = EqShared::new(true, gains);
        let mut eq = EqProcessor::new(shared, FS);
        let mut buf: Vec<f64> = (0..16384)
            .flat_map(|i| {
                let s = (2.0 * std::f64::consts::PI * f0 * i as f64 / FS as f64).sin() * 0.95;
                [s, s]
            })
            .collect();
        eq.process_interleaved(&mut buf);
        // Never clips, and stays in the same ballpark as the input (not +12 dB).
        assert!(buf.iter().all(|x| x.abs() <= 1.0));
        let peak = buf.iter().fold(0.0f64, |m, &x| m.max(x.abs()));
        assert!(peak <= 1.0 && peak > 0.5, "peak {peak} bounded but audible");
    }

    /// A live generation bump (UI edit) is picked up on the next buffer.
    #[test]
    fn live_edit_takes_effect() {
        let shared = EqShared::new(true, [0.0; NUM_BANDS]);
        let mut eq = EqProcessor::new(shared.clone(), FS);
        // Flat → skips, so a constant block stays constant.
        let mut buf = vec![0.2f64; 512];
        eq.process_interleaved(&mut buf);
        assert!(buf.iter().all(|&x| (x - 0.2).abs() < 1e-9));
        // Boost a band live; the next block must be processed (values move).
        shared.set_band(4, GAIN_DB_MAX);
        let mut buf2: Vec<f64> = (0..512).map(|i| (i as f64 * 0.05).sin() * 0.4).collect();
        let orig = buf2.clone();
        eq.process_interleaved(&mut buf2);
        assert!(buf2 != orig, "a live band edit must engage processing");
    }

    /// The response curve peaks near a boosted band's centre and is ~flat
    /// far from it — the shape the settings visualization draws.
    #[test]
    fn response_curve_tracks_boosted_band() {
        let mut gains = [0.0; NUM_BANDS];
        gains[2] = 8.0; // 125 Hz boost
        let at_band = response_db(&gains, BAND_FREQS[2] as f64, FS as f64);
        let far = response_db(&gains, BAND_FREQS[9] as f64, FS as f64); // 16 kHz
        assert!(
            at_band > 6.0,
            "near the boosted band the curve rises: {at_band}"
        );
        assert!(far.abs() < 1.0, "far from it the curve is ~flat: {far}");
        // Flat gains → flat (0 dB) curve everywhere.
        let flat = [0.0; NUM_BANDS];
        assert!(response_db(&flat, 1000.0, FS as f64).abs() < 1e-6);
    }

    /// Output never exceeds full scale even with every band slammed to max
    /// against a loud signal (the safety clamp holds).
    #[test]
    fn output_stays_bounded() {
        let shared = EqShared::new(true, [GAIN_DB_MAX; NUM_BANDS]);
        let mut eq = EqProcessor::new(shared, FS);
        let mut buf: Vec<f64> = (0..4096)
            .map(|i| (i as f64 * 0.3).sin().signum() * 0.95) // near-square, loud
            .collect();
        eq.process_interleaved(&mut buf);
        assert!(buf.iter().all(|x| x.abs() <= 1.0 && x.is_finite()));
    }
}
