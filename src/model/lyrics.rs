//! Timed lyrics for the playing track.
//!
//! Lines come from Spotify's own `color-lyrics` service (see
//! `worker::spawn_fetch_lyrics`) and are line-synced: each carries the
//! millisecond it starts at. The view highlights the line the playhead is
//! inside; [`LyricsModel::active`] is the reactive index it binds to, so a
//! highlight change costs one signal write, not a scene rebuild.

use opal_gfx::{Curve, Signal, Timeline};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::time::{Duration, Instant};

/// One synced line. `start_ms` is the playback position it appears at;
/// blank `text` marks an instrumental gap (Spotify ships those as empty
/// lines and the page renders them as breathing room).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LyricLine {
    pub start_ms: u32,
    pub text: String,
}

/// A track's lyrics as Opal stores (and caches) them.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TrackLyrics {
    pub lines: Vec<LyricLine>,
    /// Line timings are real. Unsynced lyrics still render, just without
    /// a moving highlight.
    pub synced: bool,
    /// Rights-holder credit ("Musixmatch") — shown in the page footer
    /// because the provider requires attribution.
    pub provider: String,
}

/// Line-to-line handover: how long the outgoing line takes to shrink
/// back and the incoming one to grow. Short enough to feel locked to the
/// music, long enough to read as motion rather than a jump.
const HANDOVER_DURATION: Duration = Duration::from_millis(220);
const HANDOVER_CURVE: Curve = Curve::CubicBezier([0.16, 1.0, 0.3, 1.0]);

/// How far the sync pill sits below its resting place while hidden.
const PILL_RISE: f32 = 16.0;
const PILL_DURATION: Duration = Duration::from_millis(180);
/// The chrome's standard ease-out (same shape the overlays use).
const PILL_CURVE: Curve = Curve::CubicBezier([0.16, 1.0, 0.3, 1.0]);

/// Where the page is in the fetch cycle.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum LyricsStatus {
    /// Nothing requested yet for this track.
    #[default]
    Idle,
    Loading,
    Ready,
    /// Spotify has no lyrics for this track (or the fetch failed).
    Unavailable,
}

pub struct LyricsModel {
    /// The track the loaded lines belong to — a cluster push for a
    /// different track invalidates them.
    pub track_id: String,
    pub status: LyricsStatus,
    pub lyrics: Option<TrackLyrics>,
    /// Index of the line the playhead is inside, as the view's colour
    /// and size binds read it. `usize::MAX` = before the first line.
    pub active: Signal<usize>,
    /// The line that *was* lit, so the handover can animate both ends at
    /// once — the old line shrinking back as the new one grows.
    pub prev: Signal<usize>,
    /// 0 → 1 across a handover. Both lines read it (in opposite
    /// directions), so one tween drives the whole transition.
    pub handover: Signal<f32>,
    /// Auto-scroll is engaged: the page keeps the lit line centred. A
    /// manual scroll drops it (the user is reading elsewhere and having
    /// the page yank itself back is the worst version of this feature);
    /// the sync pill puts it back.
    pub follow: bool,
    /// The scroll target the page last set itself, read back *after* the
    /// engine clamped/snapped it. A target that differs from this is the
    /// user's wheel, which is how a manual scroll is detected.
    pub commanded_y: f32,
    /// A sync-pill press waiting to be honoured: re-centre on the active
    /// line even though the highlight itself didn't move.
    resync: bool,
    /// Sync pill entry/exit — opacity + a small upward slide, both
    /// composite-only binds on the pill's own layer.
    pub pill_opacity: Signal<f32>,
    pub pill_y: Signal<f32>,
    /// Tracks that came back without lyrics this session. Negative results
    /// are *not* disk-cached (lyrics get added later), but re-asking on
    /// every open of a lyric-less track would hammer the endpoint.
    missing: HashSet<String>,
}

impl Default for LyricsModel {
    fn default() -> Self {
        Self {
            track_id: String::new(),
            status: LyricsStatus::Idle,
            lyrics: None,
            active: Signal::new(usize::MAX),
            prev: Signal::new(usize::MAX),
            handover: Signal::new(1.0),
            follow: true,
            commanded_y: 0.0,
            resync: false,
            pill_opacity: Signal::new(0.0),
            pill_y: Signal::new(PILL_RISE),
            missing: HashSet::new(),
        }
    }
}

impl LyricsModel {
    /// Does `track_id` still need a fetch? False while one is in flight,
    /// once the lines are in, and for a track already known to have none.
    pub fn needs_fetch(&self, track_id: &str) -> bool {
        !track_id.is_empty()
            && !self.missing.contains(track_id)
            && (self.track_id != track_id || self.status == LyricsStatus::Idle)
    }

    /// Engage or drop auto-scroll, animating the sync pill in/out with
    /// it. Idempotent — re-asserting the current mode doesn't restart the
    /// tween.
    pub fn set_follow(&mut self, on: bool, tl: &mut Timeline, now: Instant) {
        if self.follow == on {
            return;
        }
        self.follow = on;
        // Dropping follow shows the pill (it's the way back); engaging it
        // again hides it. It slides up as it fades in.
        let (opacity, y) = if on { (0.0, PILL_RISE) } else { (1.0, 0.0) };
        tl.animate(&self.pill_opacity, opacity, PILL_CURVE, PILL_DURATION, now);
        tl.animate(&self.pill_y, y, PILL_CURVE, PILL_DURATION, now);
        if on {
            self.resync = true;
        }
    }

    /// Take a pending sync-pill re-centre request.
    pub fn take_resync(&mut self) -> bool {
        std::mem::take(&mut self.resync)
    }

    /// Mark a fetch in flight for `track_id`, dropping the previous
    /// track's lines so the page can't show the wrong song's words.
    pub fn begin(&mut self, track_id: &str) {
        self.track_id = track_id.to_string();
        self.status = LyricsStatus::Loading;
        self.lyrics = None;
        self.active.set(usize::MAX);
        self.prev.set(usize::MAX);
        self.handover.set(1.0);
        // A new song starts followed again, with the pill parked (the page
        // scrolls back to the top on its own anyway).
        self.follow = true;
        self.commanded_y = 0.0;
        self.pill_opacity.set(0.0);
        self.pill_y.set(PILL_RISE);
    }

    /// Apply a fetch result. A response for a track that is no longer the
    /// requested one is dropped (the user skipped while it was in flight).
    pub fn resolve(&mut self, track_id: &str, lyrics: Option<TrackLyrics>) {
        if self.track_id != track_id {
            return;
        }
        match lyrics {
            Some(l) if !l.lines.is_empty() => {
                self.lyrics = Some(l);
                self.status = LyricsStatus::Ready;
            }
            _ => {
                self.missing.insert(track_id.to_string());
                self.lyrics = None;
                self.status = LyricsStatus::Unavailable;
            }
        }
        self.active.set(usize::MAX);
    }

    /// The line index playing at `position_ms` — the last line that has
    /// started. `usize::MAX` before the first one (intro).
    pub fn line_at(&self, position_ms: u32) -> usize {
        let Some(l) = self.lyrics.as_ref() else {
            return usize::MAX;
        };
        match l.lines.partition_point(|line| line.start_ms <= position_ms) {
            0 => usize::MAX,
            n => n - 1,
        }
    }

    /// Push the line playing at `position_ms` into [`Self::active`],
    /// starting the handover tween, and report the new index when it
    /// actually moved — the frame loop uses that edge to scroll the line
    /// into view (and nothing else runs on a tick where the highlight is
    /// unchanged).
    pub fn tick(&self, position_ms: u32, tl: &mut Timeline, now: Instant) -> Option<usize> {
        let idx = self.line_at(position_ms);
        if idx == self.active.get() {
            return None;
        }
        self.prev.set(self.active.get());
        self.active.set(idx);
        // Restart from 0 so the outgoing line shrinks from wherever the
        // last handover left it and the incoming one grows in.
        self.handover.set(0.0);
        tl.animate(&self.handover, 1.0, HANDOVER_CURVE, HANDOVER_DURATION, now);
        Some(idx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(starts: &[u32]) -> LyricsModel {
        let mut m = LyricsModel {
            track_id: "t".into(),
            ..Default::default()
        };
        m.resolve(
            "t",
            Some(TrackLyrics {
                lines: starts
                    .iter()
                    .map(|&start_ms| LyricLine {
                        start_ms,
                        text: "x".into(),
                    })
                    .collect(),
                synced: true,
                provider: "Musixmatch".into(),
            }),
        );
        m
    }

    #[test]
    fn line_at_picks_the_last_started_line() {
        let m = model(&[0, 1000, 2500]);
        assert_eq!(m.line_at(0), 0);
        assert_eq!(m.line_at(999), 0);
        assert_eq!(m.line_at(1000), 1);
        assert_eq!(m.line_at(9999), 2);
    }

    #[test]
    fn intro_before_the_first_line_lights_nothing() {
        let m = model(&[1500]);
        assert_eq!(m.line_at(0), usize::MAX);
        assert_eq!(m.line_at(1499), usize::MAX);
    }

    #[test]
    fn tick_reports_only_the_change_edge() {
        let m = model(&[0, 1000]);
        let mut tl = Timeline::new();
        let now = Instant::now();
        assert_eq!(m.tick(0, &mut tl, now), Some(0));
        assert_eq!(m.tick(500, &mut tl, now), None);
        assert_eq!(m.tick(1200, &mut tl, now), Some(1));
        // The outgoing line is remembered so both ends can animate.
        assert_eq!(m.prev.get(), 0);
    }

    #[test]
    fn a_track_without_lyrics_is_not_refetched() {
        let mut m = LyricsModel::default();
        m.begin("t");
        m.resolve("t", None);
        assert_eq!(m.status, LyricsStatus::Unavailable);
        assert!(!m.needs_fetch("t"));
        assert!(m.needs_fetch("other"));
    }

    #[test]
    fn a_late_response_for_a_skipped_track_is_dropped() {
        let mut m = LyricsModel::default();
        m.begin("new");
        m.resolve(
            "old",
            Some(TrackLyrics {
                lines: vec![LyricLine {
                    start_ms: 0,
                    text: "x".into(),
                }],
                synced: true,
                provider: String::new(),
            }),
        );
        assert_eq!(m.status, LyricsStatus::Loading);
        assert!(m.lyrics.is_none());
    }
}
