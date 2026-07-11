//! Spotify Canvas (looping video) slice.
//!
//! Owns the resolved/cached clip path, the off-thread decode session, the
//! engine `FrameSink` + live target node shared with that thread, and the
//! media window's hover-reveal chrome. The decode thread presents frames at a
//! running deadline (so decode time doesn't drop the effective fps) onto
//! the now-playing `.external()` node, following scene rebuilds via the
//! shared [`node`](Self::node) slot.
//!
//! Cross-slice inputs (the live player, the worker `fetch_canvas`
//! dispatch) stay in the caller; this slice just exposes the decode
//! lifecycle + the per-frame ticks.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use opal_gfx::node::NodeId;
use opal_gfx::{Curve, FrameSink, Signal, Timeline};

use crate::api::{CurrentlyPlaying, track_id_from_uri};
use crate::worker::Worker;

/// Resting alpha of the dark scrim over the Canvas video — dimmed until
/// hovered so the pane doesn't outshine the rest of the UI. The view
/// derives the scrim colour from [`CanvasModel::hover_t`].
pub const CANVAS_DIM_ALPHA: f32 = 0.5;

/// Tween duration for the pane's hover chrome (video brightness + the
/// hide arrow, both riding [`CanvasModel::hover_t`]).
const HOVER_REVEAL_DURATION: Duration = Duration::from_millis(280);

/// A running Canvas-video decode: the track it's decoding and the flag
/// the decode thread polls so a track change (or canvas-off) can stop it.
struct CanvasSession {
    track_id: String,
    stop: Arc<AtomicBool>,
}

pub struct CanvasModel {
    /// Whether to show the looping Canvas video in now-playing (persisted
    /// via prefs; toggled in settings; consumed here).
    pub show: Signal<bool>,
    /// UI-thread mirror of [`has_video`](Self::has_video), read at
    /// scene-build time to choose the now-playing layout. Cell (not
    /// Signal): the swap is a deliberate rebuild, not a reactive bind.
    pub active: bool,
    /// Hover state of the now-playing media window (set by `.on_hover`).
    pub hover: Signal<bool>,
    /// Hover-reveal progress for the pane's chrome: tweened 0 → 1 on
    /// hover, back on leave. Bound to the hide button's opacity AND
    /// (inverted) to the video dim scrim — dimmed at rest, full
    /// brightness on hover.
    pub hover_t: Signal<f32>,

    /// Resolved + cached clip for the current track: `(track_id, mp4_path)`.
    /// Set by `CanvasReady`, cleared on track change / `CanvasNone`.
    path: Option<(String, std::path::PathBuf)>,
    /// Engine handle for pushing decoded frames onto the now-playing
    /// `.external()` node. `None` until installed after the `App` is built.
    frame_sink: Option<Arc<FrameSink>>,
    /// Live `NodeId` of the now-playing canvas node, refreshed each frame
    /// (the id is not stable across rebuilds). Shared with the decode
    /// thread; `None` when the node isn't in the current tree.
    node: Arc<Mutex<Option<NodeId>>>,
    /// Set true by the decode thread on its first frame, cleared on stop.
    /// [`tick_active`](Self::tick_active) mirrors it into [`active`](Self::active).
    has_video: Arc<AtomicBool>,
    /// Last observed `hover` value, so the brightness tween only (re)starts
    /// on a hover *transition*.
    hover_last: bool,
    /// Active decode session. Replaced on track change; `None` when idle.
    decode: Option<CanvasSession>,
    /// Monotonic decode-session counter. Each `start_decode` takes the next
    /// value and tags every frame it pushes with it, so the GPU side drops a
    /// previous clip's resident frames the instant this one's first frame
    /// lands — no stale frames can ever be looped into the new clip.
    epoch: u64,
}

impl CanvasModel {
    pub fn new(show_canvas: bool) -> Self {
        Self {
            show: Signal::new(show_canvas),
            active: false,
            hover: Signal::new(false),
            hover_t: Signal::new(0.0),
            path: None,
            frame_sink: None,
            node: Arc::new(Mutex::new(None)),
            has_video: Arc::new(AtomicBool::new(false)),
            hover_last: false,
            decode: None,
            epoch: 0,
        }
    }

    /// Install the engine frame sink (after the `App` is built).
    pub fn set_frame_sink(&mut self, sink: Arc<FrameSink>) {
        self.frame_sink = Some(sink);
    }

    // --- cached clip path ---------------------------------------------

    /// Clone the cached `(track_id, path)`, if any.
    pub fn cached_path(&self) -> Option<(String, std::path::PathBuf)> {
        self.path.clone()
    }

    /// Whether the cached clip is for `track_id`.
    pub fn path_matches(&self, track_id: &str) -> bool {
        self.path.as_ref().map(|(t, _)| t == track_id).unwrap_or(false)
    }

    pub fn set_path(&mut self, track_id: String, path: std::path::PathBuf) {
        self.path = Some((track_id, path));
    }

    pub fn clear_path(&mut self) {
        self.path = None;
    }

    // --- per-frame ticks ----------------------------------------------

    /// Keep the live canvas node id in sync so the decode thread targets
    /// the correct node even after a scene rebuild.
    pub fn sync_node(&self, resolved: Option<NodeId>) {
        let mut slot = self.node.lock().unwrap();
        if slot.is_none() && resolved.is_some() {
            log::debug!("canvas node resolved: {resolved:?}");
        }
        *slot = resolved;
    }

    /// Mirror the decode thread's "video is flowing" flag into the
    /// build-time layout flag. Returns `true` on a change (caller rebuilds
    /// so the now-playing pane swaps album-art ↔ full-bleed video).
    pub fn tick_active(&mut self) -> bool {
        let want = self.has_video.load(Ordering::Relaxed);
        if want != self.active {
            self.active = want;
            if !want {
                // Falling edge: sweep the external texture again.
                // `stop_decode` already cleared once, but the decode
                // thread may have committed one more frame between the
                // stop flag being set and its next check — by this frame
                // it has seen the flag, so this clear is the last word
                // (no lingering final frame over the album art).
                if let (Some(sink), Some(node)) =
                    (self.frame_sink.clone(), *self.node.lock().unwrap())
                {
                    sink.clear(node);
                }
            }
            true
        } else {
            false
        }
    }

    /// Fade the media window's hover chrome on hover transitions: shown
    /// while hovered, hidden at rest.
    pub fn tick_hover(&mut self, tl: &mut Timeline, now: Instant) {
        let hov = self.hover.get();
        if hov != self.hover_last {
            self.hover_last = hov;
            let target = if hov { 1.0 } else { 0.0 };
            tl.animate(&self.hover_t, target, Curve::EaseInOut, HOVER_REVEAL_DURATION, now);
        }
    }

    // --- decode lifecycle ---------------------------------------------

    /// Spawn (or replace) the decode thread for `track_id`, reading the
    /// cached MP4 at `path`. Any prior session is stopped first. The
    /// thread decodes in a loop, presenting each frame at a running
    /// deadline, and pushes to the now-playing external node via the
    /// `FrameSink`, targeting [`node`](Self::node) read fresh each frame so
    /// it follows rebuilds. No-op if already decoding this track or the
    /// frame sink isn't installed yet.
    pub fn start_decode(&mut self, track_id: String, path: std::path::PathBuf) {
        if self
            .decode
            .as_ref()
            .map(|s| s.track_id == track_id)
            .unwrap_or(false)
        {
            return;
        }
        log::debug!("start_canvas_decode {track_id}");
        self.stop_decode();
        let Some(sink) = self.frame_sink.clone() else {
            return;
        };
        let node = self.node.clone();
        let has_video = self.has_video.clone();
        // Unique generation for this decode — tags every pushed frame so the
        // GPU side evicts the previous clip's resident set on our first frame.
        self.epoch += 1;
        let epoch = self.epoch;
        let stop = Arc::new(AtomicBool::new(false));
        let stop_thread = stop.clone();
        let spawned = std::thread::Builder::new()
            .name("canvas-decode".into())
            .spawn(move || {
                let Ok(bytes) = std::fs::read(&path) else {
                    log::warn!("canvas decode: failed to read {}", path.display());
                    return;
                };
                let Some(mut video) = crate::video::CanvasPlayer::open(&bytes) else {
                    log::warn!("canvas decode: {} is not a decodable AVC clip", path.display());
                    return;
                };
                log::debug!("canvas decode: {} samples", video.frame_count());

                // Two-phase playback, both honouring "no per-frame CPU→GPU
                // transfer once looping":
                //   1. Build phase — decode the first pass, uploading each
                //      frame *once* to its own GPU texture (`push_frame`),
                //      showing it live. The whole loop ends up resident in
                //      VRAM; the frame durations are recorded here.
                //   2. Loop phase — replay by `select(index)`, a view re-bind
                //      with no pixel transfer (≈ 0 CPU/GPU).
                // A scene rebuild reassigns the canvas node id; we `migrate`
                // the resident set to the new id rather than re-uploading.
                let mut building = true;
                let mut durations: Vec<std::time::Duration> = Vec::new();
                let mut idx = 0usize;
                let mut bound: Option<NodeId> = None;
                let mut first = true;
                // Present at a running deadline rather than sleeping a full
                // interval *after* the work — otherwise each frame costs
                // `work + interval`, dropping below the clip's native fps.
                let mut next_at = std::time::Instant::now();
                while !stop_thread.load(Ordering::Relaxed) {
                    // Reconcile the resident set with the live node id: a
                    // rebuild swaps the id, so move the textures across once.
                    let cur = *node.lock().unwrap();
                    match (bound, cur) {
                        (Some(b), Some(c)) if b != c => {
                            sink.migrate(b, c);
                            bound = Some(c);
                        }
                        (None, Some(c)) => bound = Some(c),
                        _ => {}
                    }

                    // Re-check the stop flag right before any commit: a
                    // stop raced mid-iteration would otherwise push one
                    // more frame *after* the main thread's texture clear,
                    // leaving a stale frame on screen.
                    if stop_thread.load(Ordering::Relaxed) {
                        break;
                    }
                    let dur = if building {
                        match video.next_pass_frame() {
                            Some(frame) => {
                                let dur = frame.duration;
                                // Only commit a frame once we have a node to
                                // attach it to, keeping durations aligned with
                                // the resident set.
                                if let Some(b) = bound {
                                    if first {
                                        log::debug!(
                                            "canvas decode: {}x{} @ {:?} (resident)",
                                            frame.width,
                                            frame.height,
                                            frame.duration
                                        );
                                        first = false;
                                        // First frame → swap to the video layout.
                                        has_video.store(true, Ordering::Relaxed);
                                    }
                                    durations.push(dur);
                                    sink.push_frame(b, epoch, frame.width, frame.height, frame.rgba);
                                }
                                dur
                            }
                            None => {
                                building = false;
                                if durations.is_empty() {
                                    log::warn!(
                                        "canvas decode: stopped — clip yielded no decodable frame"
                                    );
                                    break;
                                }
                                continue;
                            }
                        }
                    } else {
                        // Loop phase: just re-bind the next resident frame.
                        if let Some(b) = bound {
                            sink.select(b, epoch, idx);
                        }
                        let dur = durations[idx];
                        idx = (idx + 1) % durations.len();
                        dur
                    };

                    // Sleep only the remainder of this frame's interval.
                    next_at += dur;
                    let now = std::time::Instant::now();
                    if next_at > now {
                        std::thread::sleep(next_at - now);
                    } else {
                        next_at = now;
                    }
                }
                // Session over — queue the last word on our own frames.
                // The pre-commit stop check above can still race: a stop
                // arriving during the multi-ms decode lands the commit
                // *after* the main thread's clears, re-registering a stale
                // frame nothing would ever sweep. This clear is queued from
                // the same thread as those commits, so FIFO ordering makes
                // it final; the epoch guard keeps it a no-op once a newer
                // session owns the node.
                if let Some(b) = bound {
                    sink.clear_epoch(b, epoch);
                }
            });
        match spawned {
            Ok(_) => {
                self.decode = Some(CanvasSession { track_id, stop });
            }
            Err(e) => log::warn!("canvas decode: failed to spawn thread: {e}"),
        }
    }

    /// React to the `show_canvas` toggle flipping. Turned on mid-track:
    /// decode the cached clip **if it's for the current track**, else
    /// fetch for the current track (a stale cached clip from an earlier
    /// track must never replay). Turned off: stop decoding + drop the
    /// video texture. The caller persists the (debounced) pref separately.
    pub fn on_toggle(&mut self, snapshot: Option<&CurrentlyPlaying>, worker: &Worker) {
        if self.show.get() {
            // Cached-path ids are bare (`path_matches` contract); the
            // snapshot carries the full `spotify:track:…` uri.
            let current = snapshot.and_then(|p| track_id_from_uri(&p.track_id));
            match self.cached_path() {
                Some((track_id, path)) if current == Some(track_id.as_str()) => {
                    self.start_decode(track_id, path)
                }
                _ => {
                    self.clear_path();
                    if let Some(p) = snapshot
                        && let Some(id) = track_id_from_uri(&p.track_id)
                    {
                        worker.fetch_canvas(p.track_id.clone(), id.to_string());
                    }
                }
            }
        } else {
            self.stop_decode();
        }
    }

    /// Stop the active decode (if any) and clear the now-playing external
    /// texture so the UI falls back to album art.
    pub fn stop_decode(&mut self) {
        if let Some(old) = self.decode.take() {
            old.stop.store(true, Ordering::Relaxed);
        }
        self.has_video.store(false, Ordering::Relaxed);
        if let (Some(sink), Some(node)) =
            (self.frame_sink.clone(), *self.node.lock().unwrap())
        {
            sink.clear(node);
        }
    }
}
