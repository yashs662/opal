//! Persisted-preferences slice — the debounced save pipeline.
//!
//! Owns the live [`UserPreferences`] plus the signal-backed resizable
//! panel widths and the debounce bookkeeping. Splitter drags and setting
//! toggles call [`PrefsModel::mark_dirty`]; [`PrefsModel::tick`] (run
//! from the frame loop) writes once the prefs have been quiescent for
//! [`PREFS_DEBOUNCE`], coalescing a drag burst (~60 events/sec) into a
//! single disk write.
//!
//! Cross-slice inputs (the live player snapshot + the canvas toggle) are
//! passed *in* to the save methods rather than reached for, so this slice
//! stays self-contained.

use std::time::{Duration, Instant};

use opal_gfx::{Curve, Signal, Timeline};

use crate::api::CurrentlyPlaying;
use crate::prefs::{StoredPlayer, UserPreferences};

/// How long to wait after the last pref mutation before writing the
/// file. Smooths out splitter-drag bursts into a single write per drag.
const PREFS_DEBOUNCE: Duration = Duration::from_millis(500);

pub struct PrefsModel {
    /// The serialized preferences. Mutated in place (cache dir, last
    /// player, panel widths) and written by the debounced save.
    pub data: UserPreferences,
    /// Resizable panel widths in logical px, driven live by the splitters
    /// (`width_px_bind`); snapshotted back into `data.panels` on save.
    pub sidebar_w: Signal<f32>,
    pub now_playing_w: Signal<f32>,
    /// Earliest unsaved change since the last save. `None` = clean.
    dirty_since: Option<Instant>,
    /// Throwaway signal anchoring a timeline tween that keeps the loop
    /// awake until the debounce deadline. Value never read or rendered.
    save_anchor: Signal<f32>,
}

impl PrefsModel {
    pub fn new(prefs: UserPreferences) -> Self {
        let sidebar_w = Signal::new(prefs.panels.sidebar_w);
        let now_playing_w = Signal::new(prefs.panels.now_playing_w);
        Self {
            sidebar_w,
            now_playing_w,
            data: prefs,
            dirty_since: None,
            save_anchor: Signal::new(0.0),
        }
    }

    /// Mark prefs dirty without writing — the actual save runs later in
    /// [`Self::tick`] after [`PREFS_DEBOUNCE`] of quiescence. Sliding the
    /// timestamp forward on every call resets the debounce window, so a
    /// continuous splitter drag yields **one** save after the drag ends.
    pub fn mark_dirty(&mut self, now: Instant) {
        self.dirty_since = Some(now);
    }

    /// Copy the live player snapshot into prefs. **Only when one exists** —
    /// a fast close before the first cluster push (or with nothing
    /// playing) preserves the previously persisted snapshot instead of
    /// wiping it to blank.
    fn snapshot_player(&mut self, player: Option<&CurrentlyPlaying>) {
        if let Some(p) = player {
            self.data.last_player = Some(StoredPlayer {
                track_id: p.track_id.clone(),
                name: p.name.clone(),
                artist: p.artist.clone(),
                album_image_url: p.album_image_url.clone(),
                progress_ms: p.live_progress_ms().min(p.duration_ms),
                duration_ms: p.duration_ms,
                context_uri: p.context_uri.clone(),
            });
        }
    }

    /// Snapshot the signal-backed values (player + panel widths + canvas
    /// flag) into the serialized prefs, then write to disk.
    fn flush(&mut self, player: Option<&CurrentlyPlaying>, show_canvas: bool) -> std::io::Result<()> {
        self.snapshot_player(player);
        self.data.panels.sidebar_w = self.sidebar_w.get();
        self.data.panels.now_playing_w = self.now_playing_w.get();
        self.data.show_canvas = show_canvas;
        self.data.save()
    }

    /// Debounced save tick (run from the frame loop). Writes once the
    /// prefs have been dirty for [`PREFS_DEBOUNCE`]; otherwise re-anchors
    /// the throwaway tween so the loop keeps firing up to the deadline
    /// even after the last user event (idempotent — `animate` on the same
    /// signal replaces any in-flight tween).
    pub fn tick(
        &mut self,
        player: Option<&CurrentlyPlaying>,
        show_canvas: bool,
        tl: &mut Timeline,
        now: Instant,
    ) {
        let Some(dirty_at) = self.dirty_since else {
            return;
        };
        let elapsed = now.saturating_duration_since(dirty_at);
        if elapsed >= PREFS_DEBOUNCE {
            match self.flush(player, show_canvas) {
                Ok(()) => log::debug!("prefs saved"),
                Err(e) => log::warn!("prefs save failed: {e}"),
            }
            self.dirty_since = None;
            tl.stop_for(&self.save_anchor);
        } else {
            let remaining = PREFS_DEBOUNCE - elapsed + Duration::from_millis(50);
            self.save_anchor.set(0.0);
            tl.animate(&self.save_anchor, 1.0, Curve::Linear, remaining, now);
        }
    }

    /// Wipe **all** preferences back to defaults and persist immediately
    /// (the "Reset preferences" action on the login screen). Re-seeds the
    /// signal-backed panel widths too so the live UI matches, and clears the
    /// configured client id — the caller then routes back to the setup view.
    /// Best-effort write; logged, not propagated.
    pub fn reset(&mut self) {
        let defaults = UserPreferences::default();
        self.sidebar_w.set(defaults.panels.sidebar_w);
        self.now_playing_w.set(defaults.panels.now_playing_w);
        self.data = defaults;
        self.dirty_since = None;
        match self.data.save() {
            Ok(()) => log::info!("preferences reset to defaults"),
            Err(e) => log::warn!("preferences reset save failed: {e}"),
        }
    }

    /// Force a final flush on app close — picks up a mouse-up we might
    /// have missed (drag released outside the window) and persists the
    /// live snapshot so the next launch re-hydrates immediately.
    pub fn flush_on_exit(&mut self, player: Option<&CurrentlyPlaying>, show_canvas: bool) {
        match self.flush(player, show_canvas) {
            Ok(()) => log::info!("prefs flushed on exit"),
            Err(e) => log::warn!("prefs flush on exit failed: {e}"),
        }
    }
}
