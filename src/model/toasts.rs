//! Notification slice — the live toast stack, the notification history,
//! and the "unseen" badge on the top-bar bell.
//!
//! Notices come from the log tap ([`crate::notify`]) and are drained by the
//! frame tick into [`ToastModel::ingest`]. Every toast is a fixed-size card
//! whose motion is pure composite (`layer_offset_x/_y` + `layer_opacity`
//! binds on signals owned here): entering, reflowing and leaving never
//! relayout or re-raster. The scene only rebuilds when the *set* of toasts
//! changes. Expiry is a timed wake ([`ToastModel::next_due`] →
//! `SceneCtx::wake_at`), so a toast on screen costs nothing between its
//! animations.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use opal_gfx::{Align, Curve, Justify, Overlay, Signal, Timeline};

use crate::notify::{Notice, Severity, ToastLevel};
use crate::views::toasts as geom;

/// Most toasts on screen at once; a newer one pushes the oldest out.
const MAX_LIVE: usize = 4;
/// Notification-history length (newest kept).
const HISTORY_CAP: usize = 200;
/// How far (logical px) a toast slides in from / out to the right.
const SLIDE: f32 = 48.0;
/// Extra time a toast lingers once the cursor leaves it, so reading one
/// never races its timer.
const HOVER_GRACE: Duration = Duration::from_millis(1500);
const ENTER: Duration = Duration::from_millis(420);
const FADE: Duration = Duration::from_millis(200);
const REFLOW: Duration = Duration::from_millis(380);
/// Snappy, barely-overshooting spring for slides and reflow.
const SPRING: Curve = Curve::Spring {
    stiffness: 340.0,
    damping: 30.0,
};

/// How long a toast stays up before leaving on its own.
fn lifetime(severity: Severity) -> Duration {
    match severity {
        Severity::Warning => Duration::from_secs(6),
        Severity::Error => Duration::from_secs(10),
    }
}

/// One toast on screen. The signals are bound by the view; only this
/// model writes them.
pub struct LiveToast {
    pub id: u64,
    pub severity: Severity,
    pub title: String,
    pub body: String,
    /// Identical notices folded into this toast (shown as "×n").
    pub count: u32,
    /// Composite X offset — slides in from the right / out to it.
    pub slide: Signal<f32>,
    /// Composite Y offset — the toast's slot in the stack.
    pub slot: Signal<f32>,
    pub opacity: Signal<f32>,
    /// Hovered → the expiry waits for the cursor to leave.
    pub hover: Signal<bool>,
    phase: Phase,
}

#[derive(Copy, Clone)]
enum Phase {
    /// On screen until this instant.
    Shown { until: Instant },
    /// Fading out; removed from the stack at this instant.
    Leaving { gone: Instant },
}

impl LiveToast {
    fn shown(&self) -> bool {
        matches!(self.phase, Phase::Shown { .. })
    }

    fn due(&self) -> Instant {
        match self.phase {
            Phase::Shown { until } => until,
            Phase::Leaving { gone } => gone,
        }
    }
}

/// One line of the notification history.
pub struct HistoryEntry {
    pub severity: Severity,
    /// The call site's toast title, if it gave one.
    pub title: Option<String>,
    pub body: String,
    pub target: String,
    /// When it (last) happened.
    pub at: chrono::DateTime<chrono::Local>,
    /// Consecutive identical notices folded into this entry.
    pub count: u32,
}

impl HistoryEntry {
    /// The full entry as plain text — what "click to copy" puts on the
    /// clipboard.
    pub fn to_text(&self) -> String {
        let level = match self.severity {
            Severity::Warning => "WARN",
            Severity::Error => "ERROR",
        };
        let title = self
            .title
            .as_deref()
            .map(|t| format!("{t} — "))
            .unwrap_or_default();
        format!(
            "[{}] {level} {}: {title}{}",
            self.at.format("%Y-%m-%d %H:%M:%S"),
            self.target,
            self.body
        )
    }
}

pub struct ToastModel {
    /// Toasts on screen, newest first.
    pub live: Vec<LiveToast>,
    /// Every captured warning/error, newest first, capped.
    pub history: VecDeque<HistoryEntry>,
    /// Worst notice since the history was last opened — the bell's badge.
    pub unseen: Signal<Option<Severity>>,
    /// The history panel (hangs under the top-bar bell).
    pub overlay: Overlay,
    /// Which notices pop up (the user's setting); the rest go to history only.
    pub level: ToastLevel,
    next_id: u64,
}

impl ToastModel {
    pub fn new(level: ToastLevel) -> Self {
        Self {
            live: Vec::new(),
            history: VecDeque::new(),
            unseen: Signal::new(None),
            overlay: Overlay::new()
                .with_morph(geom::panel_collapsed_h())
                .with_anchor(Justify::Start)
                .with_align(Align::End)
                .with_inset([0.0, geom::STACK_TOP, geom::EDGE, 0.0]),
            level,
            next_id: 0,
        }
    }

    /// Take in freshly captured notices: record each in the history and pop
    /// a toast for the ones routed to one. Returns whether the toast set
    /// changed (→ the caller rebuilds the scene).
    pub fn ingest(&mut self, notices: Vec<Notice>, tl: &mut Timeline, now: Instant) -> bool {
        let mut changed = false;
        // Open history shows everything as it lands — nothing to flag.
        let watching = self.overlay.is_open();
        for n in notices {
            if !watching && self.unseen.get() < Some(n.severity) {
                self.unseen.set(Some(n.severity));
            }
            if n.toast && self.level.allows(n.severity) {
                changed |= self.push_toast(&n, now);
            }
            self.record(n);
        }
        if changed {
            self.evict_overflow(tl, now);
            self.reflow(tl, now);
        }
        // An open panel that was showing the empty state grows to the list.
        if watching {
            self.morph_history(tl, now);
        }
        // The history panel lists entries at build time.
        changed || (watching && !self.history.is_empty())
    }

    /// Fold `n` into an identical toast already up, or add a new one at the
    /// top of the stack (starting off to the right, transparent — the
    /// reflow below slides it in).
    fn push_toast(&mut self, n: &Notice, now: Instant) -> bool {
        let title = n.title.clone().unwrap_or_else(|| default_title(n.severity));
        let until = now + lifetime(n.severity);
        if let Some(t) = self
            .live
            .iter_mut()
            .find(|t| t.shown() && t.title == title && t.body == n.body)
        {
            t.count += 1;
            t.phase = Phase::Shown { until };
            return true;
        }
        self.next_id += 1;
        self.live.insert(
            0,
            LiveToast {
                id: self.next_id,
                severity: n.severity,
                title,
                body: n.body.clone(),
                count: 1,
                slide: Signal::new(SLIDE),
                slot: Signal::new(0.0),
                opacity: Signal::new(0.0),
                hover: Signal::new(false),
                phase: Phase::Shown { until },
            },
        );
        true
    }

    fn record(&mut self, n: Notice) {
        if let Some(last) = self.history.front_mut()
            && last.severity == n.severity
            && last.target == n.target
            && last.body == n.body
        {
            last.count += 1;
            last.at = n.at;
            return;
        }
        self.history.push_front(HistoryEntry {
            severity: n.severity,
            title: n.title,
            body: n.body,
            target: n.target,
            at: n.at,
            count: 1,
        });
        self.history.truncate(HISTORY_CAP);
    }

    /// Change which notices pop up. Toasts already up that the new level
    /// silences leave now rather than lingering out their timer.
    pub fn set_level(&mut self, level: ToastLevel, tl: &mut Timeline, now: Instant) {
        self.level = level;
        let silenced: Vec<usize> = (0..self.live.len())
            .filter(|&i| self.live[i].shown() && !level.allows(self.live[i].severity))
            .collect();
        if silenced.is_empty() {
            return;
        }
        for i in silenced {
            self.leave(i, tl, now);
        }
        self.reflow(tl, now);
    }

    /// Send a toast off early (its close button). Returns whether anything
    /// changed.
    pub fn dismiss(&mut self, id: u64, tl: &mut Timeline, now: Instant) -> bool {
        let Some(i) = self.live.iter().position(|t| t.id == id && t.shown()) else {
            return false;
        };
        self.leave(i, tl, now);
        self.reflow(tl, now);
        true
    }

    /// Advance timers: start the exit of expired toasts (unless hovered) and
    /// drop the ones whose exit has finished. Returns whether the toast set
    /// changed (→ rebuild). Cheap when nothing is due.
    pub fn tick(&mut self, tl: &mut Timeline, now: Instant) -> bool {
        let mut left = false;
        for i in 0..self.live.len() {
            if let Phase::Shown { until } = self.live[i].phase
                && until <= now
            {
                if self.live[i].hover.get() {
                    self.live[i].phase = Phase::Shown {
                        until: now + HOVER_GRACE,
                    };
                } else {
                    self.leave(i, tl, now);
                    left = true;
                }
            }
        }
        if left {
            self.reflow(tl, now);
        }
        let before = self.live.len();
        self.live
            .retain(|t| !matches!(t.phase, Phase::Leaving { gone } if gone <= now));
        self.live.len() != before
    }

    /// The next instant a toast needs attention (expiry or end of exit).
    pub fn next_due(&self) -> Option<Instant> {
        self.live.iter().map(LiveToast::due).min()
    }

    /// Open the history panel (fade + height spring, like every popup) and
    /// clear the badge.
    pub fn open_history(&mut self, tl: &mut Timeline, now: Instant) {
        self.unseen.set(None);
        let target = geom::panel_target_h(self.history.is_empty());
        self.overlay.morph_open(tl, now, target);
    }

    /// Empty the history; an open panel springs down to the empty state.
    pub fn clear_history(&mut self, tl: &mut Timeline, now: Instant) {
        self.history.clear();
        self.unseen.set(None);
        if self.overlay.is_shown() {
            self.morph_history(tl, now);
        }
    }

    /// Re-spring the open panel to fit what it now lists (a no-op when the
    /// height already matches).
    fn morph_history(&self, tl: &mut Timeline, now: Instant) {
        let target = geom::panel_target_h(self.history.is_empty());
        self.overlay.morph_to(tl, now, target);
    }

    /// Start toast `i`'s exit: fade + slide out, removed once it's gone.
    fn leave(&mut self, i: usize, tl: &mut Timeline, now: Instant) {
        let t = &mut self.live[i];
        t.phase = Phase::Leaving { gone: now + FADE };
        tl.animate(&t.opacity, 0.0, Curve::EaseInOut, FADE, now);
        tl.animate(&t.slide, SLIDE, Curve::EaseInOut, FADE, now);
    }

    /// Past the cap, the oldest shown toasts make room.
    fn evict_overflow(&mut self, tl: &mut Timeline, now: Instant) {
        let shown: Vec<usize> = (0..self.live.len())
            .filter(|&i| self.live[i].shown())
            .collect();
        for &i in shown.iter().skip(MAX_LIVE) {
            self.leave(i, tl, now);
        }
    }

    /// Slide every shown toast to its slot (newest at the top) and in from
    /// the edge. A leaving toast keeps its place while it fades; the rest
    /// close the gap over it.
    fn reflow(&mut self, tl: &mut Timeline, now: Instant) {
        let mut slot = 0.0;
        for t in self.live.iter().filter(|t| t.shown()) {
            tl.animate(&t.slot, slot, SPRING, REFLOW, now);
            tl.animate(&t.slide, 0.0, SPRING, ENTER, now);
            tl.animate(&t.opacity, 1.0, Curve::EaseInOut, FADE, now);
            slot += geom::TOAST_H + geom::GAP;
        }
    }
}

impl Default for ToastModel {
    fn default() -> Self {
        Self::new(ToastLevel::default())
    }
}

/// Title for a notice routed to a toast without one of its own.
fn default_title(severity: Severity) -> String {
    match severity {
        Severity::Warning => "Warning",
        Severity::Error => "Something went wrong",
    }
    .to_string()
}
