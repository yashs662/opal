//! Per-frame tick — the app's frame-loop logic, factored out of `main`.
//!
//! Drains the worker (routing each response through the [`reducer`]), runs
//! the per-domain ticks (canvas node sync + active/dim, debounced prefs
//! save), applies a pending cache relocation, and hides the dead base
//! background once the album-art backdrop fully covers it. Pure shell
//! logic — no view building.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Instant;

use opal_gfx::{SceneCtx, Timeline};

use crate::app::AppState;
use crate::app::cx::Cx;
use crate::app::msg::MsgQueue;
use crate::app::{reducer, update};
use crate::disk_cache;
use crate::worker::Worker;

pub fn tick(
    state: &mut AppState,
    worker: &Rc<Worker>,
    rebuild: &Rc<Cell<bool>>,
    msgs: &MsgQueue,
    ctx: &mut SceneCtx,
    tl: &mut Timeline,
    now: Instant,
) {
    let mut cx = Cx::new(tl, now, rebuild);
    // A hot-patch landed since the last tick: rebuild so the patched
    // `Component::view` bodies run. No-op unless the `hotreload` feature is on.
    if crate::hotreload::take_patched() {
        cx.rebuild();
    }
    // Keep the live canvas node id in sync so the decode thread targets the
    // correct node even after a scene rebuild.
    state.canvas.sync_node(ctx.node("now_playing_canvas"));
    // Drive the collapsing detail-page header from its scroll offset. Runs
    // every active (scroll) frame; only sets a Signal — the sticky bar's
    // position/opacity binds pick it up with no rebuild. Absent node (Home
    // feed) settles it back to 0.
    {
        use crate::views::home::playlist as pl;
        // Scroller name is content-scoped (`MainNav::detail_scroll_node`)
        // so rebuilds preserve the offset while navigation resets it. Read
        // the cached name (no per-frame `format!`).
        let scroll_node = state.router.detail_scroll_node();
        if let Some(id) = scroll_node.and_then(|n| ctx.node(n)) {
            // scroll offset is physical px; collapse range is logical.
            let off = ctx.tree.scroll_offset(id)[1] / ctx.scale.max(1.0);
            let collapse = (off / pl::COLLAPSE_RANGE).clamp(0.0, 1.0);
            if (state.router.detail_collapse.get() - collapse).abs() > 0.001 {
                state.router.detail_collapse.set(collapse);
            }
            // Track the bar's top inset to the glass header as it slides in,
            // so the bar shrinks/grows smoothly with the overlay (and is never
            // hidden behind it). Derived from the header height — not hardcoded.
            ctx.tree.with_scrollbar_style(id, |st| {
                st.inset_start = collapse * (pl::BAR_H + pl::COLHEADER_H)
            });
        } else if state.router.detail_collapse.get() != 0.0 {
            state.router.detail_collapse.set(0.0);
        }
    }
    // Publish a background cache-usage scan (settings open / clear /
    // relocate dispatched it off-thread) and repaint the storage bar.
    if let Some(usage) = state.settings.take_pending_usage() {
        state.settings.cache_usage = usage;
        cx.rebuild();
    }
    // Apply a cache relocation picked by the folder dialog: point the disk
    // cache at the new dir, persist it, rebuild so the storage bar refreshes.
    if let Some(dir) = state.settings.take_pending_dir() {
        disk_cache::set_root(Some(dir.clone()));
        state.prefs.data.cache_dir = Some(dir.display().to_string());
        state.settings.refresh_usage();
        state.prefs.mark_dirty(cx.now);
        cx.rebuild();
        log::info!("cache relocated to {}", dir.display());
    }
    // Hide the base background fill once the opaque album-art backdrop fully
    // covers it — the bg behind it is dead pixels. Re-shown mid-crossfade.
    if let Some(bg) = ctx.node("home_bg") {
        let covered = state.backdrop.covered();
        ctx.tree.set_visible(bg, !covered);
    }
    // Mirror the decode thread's "video is flowing" flag into the layout
    // flag; on a change, rebuild so now-playing swaps art ↔ video.
    if state.canvas.tick_active() {
        cx.rebuild();
    }
    // Smoothly tween the Canvas dim overlay on hover transitions.
    state.canvas.tick_dim(cx.tl, cx.now);
    // Refresh the elapsed-time label (once per second, off the live tween).
    state.player_ui.tick_clock();
    // Commit a seek on the release edge of a progress-bar drag.
    if let Some(ms) = state.player_ui.tick_seek(cx.tl)
        && let Some(token) = state.auth.token()
    {
        let local = state.devices.playing_on_self.get();
        worker.playback(token, crate::worker::PlaybackCmd::Seek(ms), local);
    }
    // Proactively refresh the access token before it expires — a long
    // listening session must never start 401-ing mid-flight. Two Cell
    // reads per frame on the cold path; dispatches exactly once per due
    // window (the in-flight gate holds until the response lands).
    if let Some(rt) = state.auth.refresh_due(cx.now) {
        log::info!("access token nearing expiry — refreshing");
        worker.refresh_tokens(rt, state.prefs.data.client_id().unwrap_or_default());
    }
    // Apply view-emitted intents (clicks/nav/etc.) queued since last frame.
    update::drain(state, worker, msgs, &mut cx);
    // Drain worker responses through the reducer.
    while let Some(resp) = worker.poll() {
        reducer::handle(state, &mut cx, worker, resp);
    }
    // A streamed page appended rows → re-materialize the open detail
    // page's lazy rows, turning any already-on-screen skeletons (fast
    // scroll outran the stream) into real tracks.
    if std::mem::take(&mut state.library.rows_appended) {
        let scroll_node = state.router.detail_scroll_node();
        if let Some(name) = scroll_node
            && let Some(id) = ctx.node(name)
        {
            ctx.tree.invalidate_lazy_list(id);
        }
    }
    // Pulse the skeleton rows while the open detail page is still
    // streaming. Started/stopped on the state edge (not re-armed every
    // frame); the ping-pong tween then runs itself on the timeline.
    {
        let streaming = state
            .library
            .open_playlist
            .as_ref()
            .map(|o| o.loading || !o.complete)
            .unwrap_or(false);
        let queue_loading = matches!(state.router.nav, crate::views::MainNav::Queue)
            && state.library.queue.is_none();
        let want = queue_loading
            || (streaming && state.router.detail_scroll_node().is_some());
        if want != state.library.pulse_on {
            let pulse = &state.library.skeleton_pulse;
            if want {
                cx.tl.animate_pingpong(
                    pulse,
                    1.0,
                    0.45,
                    opal_gfx::Curve::EaseInOut,
                    std::time::Duration::from_millis(650),
                    cx.now,
                );
            } else {
                cx.tl.stop_for(pulse);
                pulse.set(1.0);
            }
            state.library.pulse_on = want;
        }
    }
    // Debounced prefs save (panel widths + last-player snapshot).
    state.prefs.tick(
        state.player_ui.snapshot.as_ref(),
        state.canvas.show.get(),
        cx.tl,
        cx.now,
    );
}
