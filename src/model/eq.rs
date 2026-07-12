//! Equaliser slice — the UI-facing view of the 10-band graphic EQ.
//!
//! The authoritative control surface is [`crate::audio_eq::EqShared`] (an
//! `Arc` of atomics the audio thread reads lock-free). This model owns a
//! clone of it plus the reactive mirror the settings panel binds to: a
//! `Signal` per band, an `enabled` flag, and the selected-preset index.
//! Writing a band updates both the signal (so the slider paints instantly)
//! and the shared surface (so the audio thread hears it), and marks the
//! selection custom.

use std::rc::Rc;
use std::sync::Arc;

use opal_gfx::Signal;

use crate::audio_eq::{EqShared, NUM_BANDS};
use crate::prefs::{EqCustomPreset, EqPrefs};

/// A named set of band gains (dB).
#[derive(Clone)]
pub struct EqPreset {
    pub name: Rc<str>,
    pub bands: [f32; NUM_BANDS],
    /// User-saved (deletable) vs a built-in.
    pub custom: bool,
}

/// Built-in presets, in display order. `Flat` first (the reset).
/// Bands are 31 / 62 / 125 / 250 / 500 / 1k / 2k / 4k / 8k / 16k Hz.
fn builtin_presets() -> Vec<EqPreset> {
    let p = |name: &str, bands: [f32; NUM_BANDS]| EqPreset {
        name: Rc::from(name),
        bands,
        custom: false,
    };
    vec![
        p("Flat", [0.0; NUM_BANDS]),
        // Spotify's "Small speakers": lift the lows, roll off the highs to
        // compensate for tiny drivers.
        p("Small speakers", [4.0, 4.0, 3.0, 1.0, 0.0, -1.0, -2.0, -3.0, -4.0, -5.0]),
        p("Bass boost", [6.0, 5.0, 4.0, 2.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]),
        p("Treble boost", [0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 2.5, 4.0, 5.0, 6.0]),
        p("Vocal", [-2.0, -1.0, 0.0, 2.0, 4.0, 4.0, 3.0, 1.0, 0.0, -1.0]),
        p("Rock", [4.0, 3.0, 1.5, 0.0, -1.0, -0.5, 1.5, 3.0, 3.5, 4.0]),
        p("Electronic", [5.0, 4.0, 1.0, 0.0, -1.0, 1.0, 0.0, 1.0, 3.0, 5.0]),
        p("Loudness", [5.0, 3.0, 0.0, 0.0, -1.0, 0.0, 0.0, 1.0, 3.0, 5.0]),
        p("Podcast", [-4.0, -3.0, -1.0, 2.0, 4.0, 4.0, 3.0, 2.0, 0.0, -2.0]),
    ]
}

pub struct EqModel {
    /// The lock-free surface the audio sink reads. Cloned into the worker
    /// at session bootstrap so both threads share one control block.
    shared: Arc<EqShared>,
    /// Master enable — mirrors `shared.enabled`.
    pub enabled: Signal<bool>,
    /// Per-band gain (dB) — the sliders bind to these; kept in lock-step
    /// with the shared surface.
    pub bands: [Signal<f32>; NUM_BANDS],
    /// Index into [`Self::presets`] of the matching preset, or `-1` when the
    /// current gains are a hand-edited "Custom" shape.
    pub selected: Signal<i32>,
    /// Whether the presets dropdown is expanded (UI-only, not persisted).
    pub preset_open: Signal<bool>,
    /// Built-ins followed by the user's saved custom presets.
    presets: Vec<EqPreset>,
}

impl EqModel {
    pub fn from_prefs(eq: &EqPrefs) -> Self {
        let bands_db = read_bands(&eq.bands);
        let shared = EqShared::new(eq.enabled, bands_db);
        let mut presets = builtin_presets();
        for c in &eq.custom {
            presets.push(EqPreset {
                name: Rc::from(c.name.as_str()),
                bands: read_bands(&c.bands),
                custom: true,
            });
        }
        let bands = std::array::from_fn(|i| Signal::new(bands_db[i]));
        let selected = Signal::new(match_preset(&presets, &bands_db));
        Self {
            shared,
            enabled: Signal::new(eq.enabled),
            bands,
            selected,
            preset_open: Signal::new(false),
            presets,
        }
    }


    /// Clone of the shared control surface, for the worker/sink.
    pub fn shared(&self) -> Arc<EqShared> {
        self.shared.clone()
    }

    /// All selectable presets (built-ins + custom), in display order.
    pub fn presets(&self) -> &[EqPreset] {
        &self.presets
    }

    /// Current band gains as a plain array.
    pub fn band_values(&self) -> [f32; NUM_BANDS] {
        std::array::from_fn(|i| self.bands[i].get())
    }

    /// Flip the master enable (signal + shared surface).
    pub fn set_enabled(&self, on: bool) {
        self.enabled.set(on);
        self.shared.set_enabled(on);
    }

    /// Re-derive the selected-preset index from the current band values —
    /// called after a slider drag settles so the panel flips to "Custom"
    /// (or back onto a named preset if the drag happened to match one).
    pub fn refresh_selected(&self) {
        self.selected
            .set(match_preset(&self.presets, &self.band_values()));
    }

    /// Next auto-generated custom-preset name (`Custom 1`, `Custom 2`, …).
    pub fn next_custom_name(&self) -> String {
        let n = self.presets.iter().filter(|p| p.custom).count() + 1;
        format!("Custom {n}")
    }

    /// Push the selected preset's gains onto the shared surface and select
    /// it. Signal tweening is the caller's job (it holds the timeline); this
    /// returns the target gains so the caller can animate each band signal.
    pub fn apply_preset(&self, index: usize) -> Option<[f32; NUM_BANDS]> {
        let preset = self.presets.get(index)?;
        let bands = preset.bands;
        self.shared.set_all(&bands);
        self.selected.set(index as i32);
        Some(bands)
    }

    /// Save the current gains as a new custom preset (or overwrite one with
    /// the same name), select it, and return its index.
    pub fn save_custom(&mut self, name: String) -> usize {
        let bands = self.band_values();
        if let Some(pos) = self
            .presets
            .iter()
            .position(|p| p.custom && p.name.as_ref() == name)
        {
            self.presets[pos].bands = bands;
            self.selected.set(pos as i32);
            return pos;
        }
        self.presets.push(EqPreset {
            name: Rc::from(name.as_str()),
            bands,
            custom: true,
        });
        let idx = self.presets.len() - 1;
        self.selected.set(idx as i32);
        idx
    }

    /// Persist-ready snapshot from the current sliders.
    pub fn to_prefs(&self) -> EqPrefs {
        self.to_prefs_with_bands(self.band_values())
    }

    /// Persist-ready snapshot using explicit band gains — used on preset
    /// apply, where the sliders are still mid-tween but the intended
    /// (target) gains are known, so the stored value is correct immediately.
    pub fn to_prefs_with_bands(&self, bands: [f32; NUM_BANDS]) -> EqPrefs {
        EqPrefs {
            enabled: self.enabled.get(),
            bands: bands.to_vec(),
            custom: self
                .presets
                .iter()
                .filter(|p| p.custom)
                .map(|p| EqCustomPreset {
                    name: p.name.to_string(),
                    bands: p.bands.to_vec(),
                })
                .collect(),
        }
    }
}

/// Coerce a persisted (possibly short / over-long) gain vector into a
/// fixed 10-band array, defaulting missing bands to flat.
fn read_bands(v: &[f32]) -> [f32; NUM_BANDS] {
    std::array::from_fn(|i| v.get(i).copied().unwrap_or(0.0))
}

/// Index of the preset whose gains match `bands` (within a small epsilon),
/// or `-1` for a custom shape.
fn match_preset(presets: &[EqPreset], bands: &[f32; NUM_BANDS]) -> i32 {
    presets
        .iter()
        .position(|p| p.bands.iter().zip(bands).all(|(a, b)| (a - b).abs() < 0.05))
        .map(|i| i as i32)
        .unwrap_or(-1)
}
