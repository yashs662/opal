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

/// Where a line is in its own moment, as the shaders read it:
/// `[active, start, duration, rate, frozen]`.
///
/// Derived, never stamped — `start` is the shader clock the line began
/// at, so while playback runs at speed it stays *constant* frame over
/// frame, and the params only need pushing when something actually
/// changes (a new line, a pause, a seek). See [`LyricsModel::line_clock`].
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LineClock {
    pub active: bool,
    pub start: f32,
    pub duration: f32,
    pub playing: bool,
    pub progress: f32,
}

impl LineClock {
    /// The shader params. One definition, used by the view when it builds
    /// a line and by the frame loop when it pushes an update.
    pub fn params(self) -> [f32; 5] {
        [
            if self.active { 1.0 } else { 0.0 },
            self.start,
            self.duration,
            if self.playing { 1.0 } else { 0.0 },
            self.progress,
        ]
    }

    /// Whether this is a different enough moment from `other` to be worth
    /// pushing. While playing, `start` jitters by a frame of tween error;
    /// re-uploading params for that would be a re-flatten every frame.
    pub fn differs_from(self, other: Self) -> bool {
        self.active != other.active
            || self.playing != other.playing
            || (self.duration - other.duration).abs() > 0.01
            || (self.start - other.start).abs() > 0.08
            || (!self.playing && (self.progress - other.progress).abs() > 0.005)
    }
}

/// How long the last line of a song is treated as lasting — there is no
/// next line to end it, and a sweep needs some span to run over.
const LAST_LINE_MS: u32 = 6_000;

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
    /// The moment last pushed to the shaders, so the frame loop can tell
    /// a real change (new line, pause, seek) from the clock simply
    /// advancing — which needs no push at all.
    pub pushed_clock: LineClock,
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
            pushed_clock: LineClock::default(),
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

    /// Where line `i` is in its own moment at `position_ms`, for the
    /// shaders. Any line that isn't the one at the playhead gets the
    /// resting clock, which every effect answers by leaving its glyphs
    /// exactly where the layout put them.
    pub fn line_clock(
        &self,
        i: usize,
        position_ms: u32,
        playing: bool,
        effect_time: f32,
    ) -> LineClock {
        let Some(l) = self.lyrics.as_ref() else {
            return LineClock::default();
        };
        if !l.synced || self.active.get() != i {
            return LineClock::default();
        }
        let Some(line) = l.lines.get(i) else {
            return LineClock::default();
        };
        // A line runs until the next one starts; the last one has nothing
        // to end it, so it gets a sensible span to sweep over.
        let end_ms = l
            .lines
            .get(i + 1)
            .map(|n| n.start_ms)
            .unwrap_or(line.start_ms + LAST_LINE_MS);
        let duration = (end_ms.saturating_sub(line.start_ms) as f32 / 1000.0).max(0.2);
        let elapsed = (position_ms.saturating_sub(line.start_ms) as f32 / 1000.0).max(0.0);
        LineClock {
            active: true,
            // The clock the line *began* at — the shader runs the sweep
            // from there, so a rebuild mid-line resumes instead of
            // restarting, and a steady playhead never moves this value.
            start: effect_time - elapsed,
            duration,
            playing,
            progress: (elapsed / duration).clamp(0.0, 1.0),
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
    fn only_the_line_at_the_playhead_gets_an_active_clock() {
        let m = model(&[0, 4000]);
        m.active.set(1);
        let idle = m.line_clock(0, 5000, true, 100.0);
        assert!(!idle.active);
        assert_eq!(idle.params()[0], 0.0);
        let live = m.line_clock(1, 5000, true, 100.0);
        assert!(live.active && live.playing);
        // Started a second ago, and runs on past the last timestamp.
        assert!((live.start - 99.0).abs() < 1e-3);
        assert!((live.duration - LAST_LINE_MS as f32 / 1000.0).abs() < 1e-3);
    }

    #[test]
    fn a_steady_playhead_keeps_the_same_start_so_nothing_is_pushed() {
        let m = model(&[0, 4000]);
        m.active.set(0);
        // A second of playback advances the position and the clock
        // together, so the line's start is unmoved — no param push.
        let a = m.line_clock(0, 1000, true, 10.0);
        let b = m.line_clock(0, 2000, true, 11.0);
        assert!(!b.differs_from(a));
        // A seek moves it, and that does need pushing.
        let c = m.line_clock(0, 3500, true, 11.0);
        assert!(c.differs_from(b));
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
