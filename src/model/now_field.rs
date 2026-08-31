//! Which row wears the now-playing field, and the row it just left.
//!
//! A track change swaps the field from one row to another. Both ends are
//! kept for the length of the handover so the outgoing row can fade out
//! instead of vanishing — the fade itself runs on the shader clock (see
//! `opal_gfx::NowPlaying`), so this only has to remember *when* the swap
//! happened and drop the outgoing row once it's over.

use std::time::{Duration, Instant};

/// How long the outgoing row lingers. Matches the shader's fade so the
/// row is dropped exactly as it reaches zero.
pub const HANDOVER: Duration = Duration::from_millis(450);

#[derive(Default)]
pub struct NowFieldModel {
    /// The playing track's uri — the row that wears the field.
    pub current: Option<String>,
    /// The row it just left, fading out until `HANDOVER` elapses.
    pub leaving: Option<String>,
    /// When the swap happened (drives both fades).
    pub since: Option<Instant>,
}

impl NowFieldModel {
    /// Move the field to `uri`, returning whether it actually moved (the
    /// caller then rebuilds so the rows swap). The previous row starts
    /// fading out. Re-asserting the same track is a no-op, so a progress
    /// push can't restart the animation.
    pub fn set_current(&mut self, uri: Option<String>, now: Instant) -> bool {
        if self.current == uri {
            return false;
        }
        self.leaving = self.current.take();
        self.current = uri;
        self.since = Some(now);
        true
    }

    /// Seconds since the swap — what the shader resumes its envelope
    /// from, so a rebuild mid-fade doesn't restart it.
    pub fn elapsed(&self, now: Instant) -> f32 {
        self.since
            .map(|t| now.duration_since(t).as_secs_f32())
            .unwrap_or(f32::MAX)
    }

    /// Drop the outgoing row once its fade is done. Returns true when
    /// something changed (the caller rebuilds to shed the dead node —
    /// otherwise an invisible animated effect would keep the loop
    /// rendering forever).
    pub fn tick(&mut self, now: Instant) -> bool {
        if self.leaving.is_some()
            && self
                .since
                .is_some_and(|t| now.duration_since(t) >= HANDOVER)
        {
            self.leaving = None;
            return true;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_swap_moves_the_field_and_keeps_the_old_row() {
        let mut m = NowFieldModel::default();
        let t0 = Instant::now();
        assert!(m.set_current(Some("a".into()), t0));
        assert_eq!(m.current.as_deref(), Some("a"));
        assert_eq!(m.leaving, None);
        assert!(m.set_current(Some("b".into()), t0));
        assert_eq!(m.current.as_deref(), Some("b"));
        assert_eq!(m.leaving.as_deref(), Some("a"));
    }

    #[test]
    fn re_asserting_the_same_track_does_not_restart_the_fade() {
        let mut m = NowFieldModel::default();
        let t0 = Instant::now();
        m.set_current(Some("a".into()), t0);
        let since = m.since;
        // A progress push a second later must not move `since`, or the
        // field would restart its fade on every tick.
        assert!(!m.set_current(Some("a".into()), t0 + Duration::from_secs(1)));
        assert_eq!(m.since, since);
    }

    #[test]
    fn the_outgoing_row_is_dropped_once_its_fade_is_over() {
        let mut m = NowFieldModel::default();
        let t0 = Instant::now();
        m.set_current(Some("a".into()), t0);
        m.set_current(Some("b".into()), t0);
        assert!(!m.tick(t0 + HANDOVER / 2), "still fading");
        assert_eq!(m.leaving.as_deref(), Some("a"));
        assert!(m.tick(t0 + HANDOVER), "fade over → drop it");
        assert_eq!(m.leaving, None);
        // Idempotent: nothing left to drop, so no further rebuilds.
        assert!(!m.tick(t0 + HANDOVER * 2));
    }
}
