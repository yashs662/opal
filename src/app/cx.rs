//! Per-frame / per-event context.
//!
//! Bundles the handful of shell-owned handles that model `apply`/`tick`
//! methods (and, in Phase 1, component `view`/`update`) need every frame:
//! the [`Timeline`], the current instant, and the rebuild token. Threading
//! one `&mut Cx` replaces passing `timeline`/`now`/`rebuild` as separate
//! params everywhere.
//!
//! The [`Worker`] is deliberately **not** in here: the frame loop drains
//! it with `while let Some(r) = worker.poll()` while passing `&mut Cx`
//! into the handler, so keeping the worker a separate borrow avoids a
//! self-borrow conflict (and it's a cheap `Rc` to thread alongside).
//!
//! [`Worker`]: crate::worker::Worker

use std::cell::Cell;
use std::rc::Rc;
use std::time::Instant;

use opal_gfx::{Timeline, WindowAction};

pub struct Cx<'a> {
    /// Animation timeline — `animate`/`stop_for` tweens keyed by signal.
    pub tl: &'a mut Timeline,
    /// The instant this frame/event is being processed at.
    pub now: Instant,
    /// Scene-rebuild request token. Flip via [`Cx::rebuild`].
    rebuild: &'a Rc<Cell<bool>>,
    /// Window action the reducer asked for (e.g. close after detaching the
    /// engine). The frame tick hands it to the engine once the drain ends.
    window_action: Option<WindowAction>,
}

impl<'a> Cx<'a> {
    pub fn new(tl: &'a mut Timeline, now: Instant, rebuild: &'a Rc<Cell<bool>>) -> Self {
        Self {
            tl,
            now,
            rebuild,
            window_action: None,
        }
    }

    /// Ask the shell to perform a window action (close / minimise / …)
    /// after this frame's intents are applied.
    pub fn window(&mut self, action: WindowAction) {
        self.window_action = Some(action);
    }

    /// Take the pending window action (frame tick only).
    pub fn take_window_action(&mut self) -> Option<WindowAction> {
        self.window_action.take()
    }

    /// Request one scene rebuild after this frame (a deliberate, one-shot
    /// structural swap — distinct from the reactive bind path).
    pub fn rebuild(&self) {
        self.rebuild.set(true);
    }
}
