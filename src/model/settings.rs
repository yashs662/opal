//! Settings-modal slice.
//!
//! Owns the modal [`Overlay`] (self-contained scrim/fade/input-blocking),
//! the last-measured on-disk cache usage shown in the storage bar, and
//! the cross-thread handoff slot for the (blocking) folder-picker dialog.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use opal_gfx::{Overlay, Signal, WakeHandle};

use crate::disk_cache::{self, CacheUsage};

pub struct SettingsModel {
    /// The settings modal. Owns its fade opacity + timeline key, blocks
    /// input beneath it, costs nothing when closed.
    pub overlay: Overlay,
    /// Last-measured on-disk cache usage (art vs JSON), shown in the
    /// storage bar. Recomputed off-thread on open / clear / relocate and
    /// published here by the frame tick (see [`Self::take_pending_usage`]).
    pub cache_usage: CacheUsage,
    /// "Normalize volume" toggle state — seeded from prefs, drives the
    /// switch reactively. The pref is read at session start (applies on
    /// next launch), so this only mirrors + persists the choice.
    pub normalize: Signal<bool>,
    /// Folder picked by the off-thread (blocking) cache-relocation dialog,
    /// awaiting pickup on the UI thread in the frame loop.
    pub pending_cache_dir: Arc<Mutex<Option<PathBuf>>>,
    /// Freshly-scanned cache usage from a background scan thread, awaiting
    /// pickup on the UI thread (the scan walks every cached file — far too
    /// much to do on the frame). `None` until a scan lands.
    pending_usage: Arc<Mutex<Option<CacheUsage>>>,
    /// Loop wake handle, stored once after app construction so the cache
    /// scan/clear threads can nudge the frame loop to pick up their result.
    wake: Option<Arc<WakeHandle>>,
}

impl SettingsModel {
    pub fn new(normalize: bool) -> Self {
        Self {
            // Height-morphing: the overlay springs the panel collapsed → full
            // on open (and back on close/dismiss) with no per-open plumbing.
            overlay: Overlay::new().with_morph(crate::views::home::settings::PANEL_COLLAPSED_H),
            cache_usage: CacheUsage::default(),
            normalize: Signal::new(normalize),
            pending_cache_dir: Arc::new(Mutex::new(None)),
            pending_usage: Arc::new(Mutex::new(None)),
            wake: None,
        }
    }

    /// Stash the loop wake handle (available only after the window/app is
    /// built). Lets the off-thread cache scans re-run the frame loop when
    /// their result is ready.
    pub fn set_wake(&mut self, wake: Arc<WakeHandle>) {
        self.wake = Some(wake);
    }

    /// Re-measure on-disk cache usage **off the UI thread** (settings open /
    /// cache cleared / cache relocated) — [`disk_cache::usage`] walks every
    /// cached file and must never run on the frame. The result lands in
    /// `pending_usage`; the frame tick publishes it via
    /// [`Self::take_pending_usage`].
    pub fn refresh_usage(&self) {
        let pending = self.pending_usage.clone();
        let wake = self.wake.clone();
        std::thread::spawn(move || {
            let usage = disk_cache::usage();
            *pending.lock().unwrap() = Some(usage);
            if let Some(w) = wake {
                w.wake();
            }
        });
    }

    /// Wipe every cached file (art, Canvas videos, API JSON) and re-scan,
    /// both **off the UI thread** — the delete walk and the re-measure are
    /// each O(files) and would hitch the frame. The refreshed usage lands
    /// in `pending_usage`.
    pub fn clear_cache(&self) {
        let pending = self.pending_usage.clone();
        let wake = self.wake.clone();
        std::thread::spawn(move || {
            let freed = disk_cache::clear();
            log::info!("cleared disk cache (freed {freed} bytes)");
            let usage = disk_cache::usage();
            *pending.lock().unwrap() = Some(usage);
            if let Some(w) = wake {
                w.wake();
            }
        });
    }

    /// Take a cache-usage scan stashed by a background thread, if one has
    /// landed since the last poll. Called by the frame tick, which sets it
    /// into [`Self::cache_usage`] and rebuilds the (open) settings bar.
    pub fn take_pending_usage(&self) -> Option<CacheUsage> {
        self.pending_usage.lock().unwrap().take()
    }

    /// Open the native folder picker on a worker thread (the dialog
    /// blocks) and stash the chosen path for the frame loop to apply via
    /// [`take_pending_dir`](Self::take_pending_dir); the stored wake re-runs
    /// the loop once a folder is picked.
    pub fn pick_cache_dir(&self) {
        let pending = self.pending_cache_dir.clone();
        let wake = self.wake.clone();
        std::thread::spawn(move || {
            if let Some(dir) = rfd::FileDialog::new()
                .set_title("Choose cache folder")
                .pick_folder()
            {
                *pending.lock().unwrap() = Some(dir);
                if let Some(w) = wake {
                    w.wake();
                }
            }
        });
    }

    /// Take a cache-dir pick stashed by the folder-picker thread, if one
    /// has landed since the last poll.
    pub fn take_pending_dir(&self) -> Option<PathBuf> {
        self.pending_cache_dir.lock().unwrap().take()
    }
}

impl Default for SettingsModel {
    fn default() -> Self {
        Self::new(true)
    }
}
