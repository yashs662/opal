//! View-routing slice — the top-level view + centre-pane nav + the
//! entrance transition.
//!
//! `view` selects Splash/Login/Home; `nav` selects what the Home centre
//! pane shows (feed vs a playlist page). [`RouterModel::go`] flips `nav`
//! and restarts the slide/fade-in tween that the scene rebuild mounts the
//! new content under — the one place a deliberate rebuild is correct
//! (the content is structurally different; the reactive path can't
//! restructure the tree).

use std::time::{Duration, Instant};

use opal_gfx::{Curve, Signal, Timeline};

use crate::views::{MainNav, View};

/// Centre-pane content transition duration on nav change.
const MAIN_NAV_DURATION: Duration = Duration::from_millis(260);

/// Ease-out for the centre-pane entrance — fast start, gentle settle.
const NAV_CURVE: Curve = Curve::CubicBezier([0.16, 1.0, 0.3, 1.0]);

pub struct RouterModel {
    pub view: View,
    /// 0 → 1 fade/slide progress for a top-level view change (Splash ↔ Setup
    /// ↔ Login ↔ Home), retween'd by [`RouterModel::go_view`]. Parks at 1.0.
    /// The pre-auth views (`setup`/`login`) wrap their content in it so a
    /// view switch eases in instead of hard-cutting.
    pub view_t: Signal<f32>,
    /// True only when the Login view was reached *from* Setup (the user just
    /// saved a client id), so Login shows a "Back" affordance to edit it.
    /// Cleared on every other path to Login (startup, logout) — there's
    /// nowhere meaningful to go "back" to in those cases.
    pub came_from_setup: bool,
    /// What the Home centre pane is showing (feed vs a playlist page).
    pub nav: MainNav,
    /// Cached scroller-node name for the current `nav` (`detail_scroll:{id}`
    /// for a detail page, else `None`). Recomputed once per nav change in
    /// [`Self::go`] so the frame tick — which needs it every active frame to
    /// drive the collapsing header — reads it without re-`format!`-ing a
    /// fresh `String` each frame.
    nav_scroll_node: Option<String>,
    /// 0 → 1 slide/fade progress for the centre-pane content, retween'd on
    /// every nav change. Parks at 1.0 (settled).
    pub main_t: Signal<f32>,
    /// Detail-page header collapse, 0 (hero fully expanded) → 1 (collapsed
    /// into the sticky bar). Driven each frame from the open detail page's
    /// scroll offset (see `app::frame::tick`); the view slides + fades the
    /// sticky bar from it. Reset to 0 on every nav.
    pub detail_collapse: Signal<f32>,
    /// Nav history behind the top-bar back/forward arrows. [`Self::go`]
    /// pushes the outgoing nav onto `back` and clears `forward`;
    /// [`Self::pop_back`]/[`Self::pop_forward`] walk them.
    back: Vec<MainNav>,
    forward: Vec<MainNav>,
    /// Reactive can-go flags — the arrows' tints ride these (no rebuild).
    pub can_back: Signal<bool>,
    pub can_forward: Signal<bool>,
}

impl RouterModel {
    pub fn new() -> Self {
        Self {
            view: View::default(),
            view_t: Signal::new(1.0),
            came_from_setup: false,
            nav: MainNav::default(),
            // Default nav is the Home feed → no detail scroller.
            nav_scroll_node: None,
            main_t: Signal::new(1.0),
            detail_collapse: Signal::new(0.0),
            back: Vec::new(),
            forward: Vec::new(),
            can_back: Signal::new(false),
            can_forward: Signal::new(false),
        }
    }

    /// Switch the mounted top-level view and restart its entrance tween
    /// (`view_t` 0 → 1). No-op if already on `view`. The caller still owns
    /// requesting the one-shot scene rebuild that mounts the new view.
    pub fn go_view(&mut self, view: View, tl: &mut Timeline, now: Instant) {
        if self.view == view {
            return;
        }
        self.view = view;
        self.view_t.set(0.0);
        tl.animate(&self.view_t, 1.0, NAV_CURVE, MAIN_NAV_DURATION, now);
    }

    /// Whether the centre pane is showing the detail page (playlist or album)
    /// for `id`. Used by the reducer to decide if a `PlaylistOpened`/`Tracks`
    /// response still applies to the open pane (albums reuse that response).
    pub fn nav_is_open(&self, id: &str) -> bool {
        match &self.nav {
            MainNav::Playlist { id: nid, .. } | MainNav::Album { id: nid } => nid == id,
            MainNav::Home | MainNav::Artist { .. } | MainNav::ShowAll { .. } | MainNav::Queue => {
                false
            }
        }
    }

    /// The current detail page's scroller-node name (`detail_scroll:{id}`),
    /// or `None` on the feed/artist/queue. Cached — reading it allocates
    /// nothing, so the per-frame collapsing-header drive is alloc-free.
    pub fn detail_scroll_node(&self) -> Option<&str> {
        self.nav_scroll_node.as_deref()
    }

    /// Whether the centre pane is showing the artist page for `id`.
    pub fn nav_is_artist(&self, id: &str) -> bool {
        matches!(&self.nav, MainNav::Artist { id: nid } if nid == id)
    }

    /// Flip nav to `nav`, record the outgoing page in the back history
    /// (a forward-branch is discarded, browser-style), and restart the
    /// entrance transition — the scene rebuild mounts the new content;
    /// the tween fades + slides it in over ~260 ms.
    pub fn go(&mut self, nav: MainNav, tl: &mut Timeline, now: Instant) {
        if nav == self.nav {
            return;
        }
        self.push_back_entry();
        self.forward.clear();
        self.apply(nav, tl, now);
    }

    /// Step back through the history (the top-bar back arrow). Returns the
    /// nav to re-prepare (fetches re-run caller-side), `None` when empty.
    pub fn pop_back(&mut self, tl: &mut Timeline, now: Instant) -> Option<MainNav> {
        let target = self.back.pop()?;
        // Ephemeral pages (synthetic listings whose data lives only while
        // open) can't be revisited via "forward" — drop them instead of
        // stranding the arrow on an empty page.
        if !Self::is_ephemeral(&self.nav) {
            self.forward.push(self.nav.clone());
        }
        self.apply(target.clone(), tl, now);
        Some(target)
    }

    /// Step forward again (the top-bar forward arrow).
    pub fn pop_forward(&mut self, tl: &mut Timeline, now: Instant) -> Option<MainNav> {
        let target = self.forward.pop()?;
        self.push_back_entry();
        self.apply(target.clone(), tl, now);
        Some(target)
    }

    fn push_back_entry(&mut self) {
        if !Self::is_ephemeral(&self.nav) {
            self.back.push(self.nav.clone());
        }
    }

    /// Synthetic pages built from in-memory state (the artist "in your
    /// library" listing) — valid to leave, not to return to via history.
    fn is_ephemeral(nav: &MainNav) -> bool {
        matches!(nav, MainNav::Playlist { id, .. } if id.starts_with("__library__"))
    }

    /// The shared tail of every nav change: cache the scroller name, swap
    /// the nav, reset the collapse, restart the entrance tween, refresh
    /// the arrow flags.
    fn apply(&mut self, nav: MainNav, tl: &mut Timeline, now: Instant) {
        // Recompute the cached scroller name once, here — the frame tick then
        // reads it every active frame without allocating.
        self.nav_scroll_node = nav.detail_scroll_node();
        self.nav = nav;
        // New page starts scrolled to top → header fully expanded.
        self.detail_collapse.set(0.0);
        self.main_t.set(0.0);
        tl.animate(&self.main_t, 1.0, NAV_CURVE, MAIN_NAV_DURATION, now);
        self.can_back.set(!self.back.is_empty());
        self.can_forward.set(!self.forward.is_empty());
    }
}

impl Default for RouterModel {
    fn default() -> Self {
        Self::new()
    }
}
