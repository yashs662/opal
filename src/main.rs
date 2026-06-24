#![cfg_attr(
    all(target_os = "windows", not(debug_assertions)),
    windows_subsystem = "windows"
)]

mod album_art;
mod api;
mod app;
mod auth;
mod bounded;
mod canvas;
mod cluster_listener;
mod constants;
#[cfg(feature = "automation")]
mod debug_config;
mod disk_cache;
mod errors;
mod extracted_color;
mod hotreload;
mod local_player;
mod model;
mod prefs;
mod spirc_bootstrap;
mod spotify_session;
mod video;
mod views;
mod widgets;
mod worker;

use std::rc::Rc;

use opal_gfx::App;

use crate::app::AppState;
use crate::model::CanvasModel;
use crate::prefs::UserPreferences;
use crate::views::View;
use crate::widgets::tokens;
use crate::worker::Worker;

const W: u32 = 1280;
const H: u32 = 780;

/// Register the platform-native credential store as keyring-core's default.
/// Must run before any `keyring_core::Entry` is created.
fn init_credential_store() {
    #[cfg(windows)]
    let store = windows_native_keyring_store::Store::new();
    #[cfg(target_os = "macos")]
    let store = apple_native_keyring_store::keychain::Store::new();
    #[cfg(target_os = "linux")]
    let store = linux_keyutils_keyring_store::Store::new();

    match store {
        Ok(s) => keyring_core::set_default_store(s),
        Err(e) => log::warn!("credential store init failed, tokens won't persist: {e}"),
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Debug-only launch config (REMOVABLE — `automation` feature). Parsed
    // before logging so it can override the filter.
    #[cfg(feature = "automation")]
    let debug_cfg = debug_config::from_args();

    // `opal_gfx=debug` would spam per-frame `[loop] WaitUntil(...)` +
    // active-tick lines while the progress tween runs (60 fps during
    // playback). Drop the lib to `info`; keep `opal` at debug.
    let default_filter = "info,wgpu_hal=warn,wgpu_core=warn,opal=debug,opal_gfx=info";
    #[cfg(feature = "automation")]
    let filter = debug_cfg
        .as_ref()
        .and_then(|c| c.log_filter.clone())
        .unwrap_or_else(|| default_filter.to_string());
    #[cfg(not(feature = "automation"))]
    let filter = default_filter.to_string();
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(filter)).init();

    // keyring-core has no built-in store: register the OS-native one once,
    // before any Entry use (token load below). Fail-soft — a registration
    // error just means later token I/O surfaces a keyring error.
    init_credential_store();

    // Load persisted preferences before any window work — initial size
    // + panel widths come from here. Fail-soft: a missing or malformed
    // file yields defaults so first launch always boots.
    let mut prefs = UserPreferences::load();
    // Point the disk cache at the user-chosen directory (if any) before any
    // fetch can touch it.
    disk_cache::set_root(prefs.cache_dir.as_ref().map(std::path::PathBuf::from));
    // Snap any out-of-range panel widths back into a valid state —
    // handles corrupted JSON, schema additions where MIN/MAX moved past
    // a saved value, and the float-drift edge cases. Values close to
    // the collapsed snap stay collapsed; everything else clamps to
    // `[MIN, MAX]`.
    prefs.panels.sidebar_w = prefs::clamp_panel_width(
        prefs.panels.sidebar_w,
        tokens::SIDEBAR_MIN,
        tokens::SIDEBAR_MAX,
        tokens::SIDEBAR_COLLAPSED,
    );
    prefs.panels.now_playing_w = prefs::clamp_panel_width(
        prefs.panels.now_playing_w,
        tokens::NOW_PLAYING_MIN,
        tokens::NOW_PLAYING_MAX,
        0.0, // now-playing collapses fully
    );
    let win_w = prefs.window.width.unwrap_or(W);
    let win_h = prefs.window.height.unwrap_or(H);
    // Debug config may pin the window size (REMOVABLE — shadows the above).
    #[cfg(feature = "automation")]
    let (win_w, win_h) = match debug_cfg.as_ref().and_then(|c| c.window) {
        Some([w, h]) => (w, h),
        None => (win_w, win_h),
    };

    // The whole app state lives behind one root `RefCell`: the build phase
    // takes a shared `borrow()`, the frame tick a `borrow_mut()`, and the
    // frame loop runs them in distinct non-overlapping passes. (Flatten-first
    // step toward the TEA ownership flip — see PLAN_TEA.md.)
    let state = Rc::new(std::cell::RefCell::new(AppState::from_prefs(prefs)));
    let force_home = std::env::var_os("OPAL_FORCE_HOME").is_some();
    #[cfg(feature = "automation")]
    let force_home = force_home || debug_cfg.as_ref().map(|c| c.force_home).unwrap_or(false);
    if force_home {
        state.borrow_mut().router.view = View::Home;
    } else if state.borrow().prefs.data.client_id().is_none() {
        // No client id yet → go straight to first-run setup instead of
        // flashing the Splash "checking credentials" (there's nothing to
        // check, and an expired token couldn't be refreshed without an id).
        state.borrow_mut().router.view = View::Setup;
    }

    let mut app = App::new("Opal", win_w, win_h)
        .decorations(false)
        .window_corner_radius(tokens::R_XL)
        // CPU splash painted before the GPU back-end loads — fills the
        // blank ~2 s cold-start gap with the brand logo + wordmark (same
        // mark + text the login header uses). See opal_gfx::splash.
        .splash(opal_gfx::SplashConfig {
            logo_svg: include_bytes!("../assets/logo/geometric-opal.svg").to_vec(),
            wordmark: "Opal".to_string(),
            logo_px: 64.0,
            wordmark_px: tokens::TEXT_4XL,
            gap_px: 16.0,
            wordmark_color: tokens::TEXT,
            bg_color: tokens::BG,
        })
        .capture_from_env();
    // Taskbar / alt-tab icon. Decoded from the bundled 256px PNG (the same
    // art embedded as the exe icon via build.rs). Fail-soft: a decode error
    // just leaves winit on the exe-icon fallback.
    match image::load_from_memory(include_bytes!("../assets/logo/png/icon-256.png")) {
        Ok(img) => {
            let rgba = img.to_rgba8();
            let (w, h) = rgba.dimensions();
            app = app.window_icon_rgba(w, h, rgba.into_raw());
        }
        Err(e) => log::warn!("window icon decode failed: {e}"),
    }
    let icons = std::rc::Rc::new(widgets::icon::load_all(&mut app));
    let rebuild = app.rebuild_token();
    // View-intent queue: callbacks push a `Msg` (via `Dispatch`, which also
    // wakes the loop); the frame tick drains it through `app::update::drain`
    // (TEA Stage 1 — see PLAN_TEA.md).
    let msgs = app::msg::queue();
    let dispatch = app::msg::Dispatch::new(msgs.clone(), app.wake_handle());
    // Connect to the dx devserver for runtime hot-patching (no-op unless the
    // `hotreload` feature is on). The patch handler latches a flag + wakes the
    // loop; the per-frame tick drains it into a scene rebuild.
    hotreload::connect(app.wake_handle());
    let worker = Rc::new(Worker::new(app.wake_handle(), app.uploader()));
    // Stored tokens can only be refreshed with the user's own client id;
    // empty when unconfigured (then an expired pair just routes to login).
    worker.try_load_tokens(state.borrow().prefs.data.client_id().unwrap_or_default());
    // Hand the state the engine's frame sink so the Canvas decode thread
    // can push video frames onto the now-playing external node.
    state.borrow().canvas.set_frame_sink(app.frame_sink());
    // Stage the Canvas dim gradient — the model owns the gradient shape;
    // here we only do the GPU upload and hand the handle back.
    let (gw, gh, px) = CanvasModel::dim_grad_rgba();
    state.borrow().canvas.set_dim_grad(app.stage_image_rgba(gw, gh, px));

    // Re-hydrate the album-art backdrop from the persisted last track so
    // it's populated before the user sees Home (disk-cache → near-instant).
    let last_cover = state
        .borrow()
        .prefs
        .data
        .last_player
        .as_ref()
        .and_then(|p| p.album_image_url.clone());
    if let Some(url) = last_cover {
        state.borrow().art.rehydrate_cover(&url, &worker);
    }
    // Re-hydrate the last track's Canvas too (when enabled), so the
    // now-playing pane loops its video on cold start rather than only the
    // static cover. The canvas meta + mp4 are disk-cached for a just-played
    // track, so this resolves cache-first without a live session; the
    // `CanvasReady` handler decodes it (and ignores it if a different live
    // track has meanwhile started — see the reducer guard).
    let (last_uri, show_canvas) = {
        let st = state.borrow();
        let d = &st.prefs.data;
        (
            d.last_player.as_ref().map(|p| p.track_id.clone()),
            d.show_canvas,
        )
    };
    if show_canvas
        && let Some(uri) = last_uri
        && let Some(id) = api::track_id_from_uri(&uri)
    {
        worker.fetch_canvas(uri.clone(), id.to_string());
    }

    // The two views own their components + callbacks; the router state
    // (`state.router.view`) selects which one builds each scene rebuild —
    // `main` no longer composes any UI itself.
    // Hand the loop wake to settings so its off-thread cache scans (and the
    // folder picker) can nudge the frame loop when their result is ready.
    state.borrow_mut().settings.set_wake(app.wake_handle());
    let home_view = views::home::HomeView::new(state.clone(), dispatch, icons.clone());
    let login_view =
        views::login::LoginView::new(state.clone(), worker.clone(), icons.clone(), rebuild.clone());
    let setup_view = views::setup::SetupView::new(state.clone(), icons.clone(), rebuild.clone());

    let app = {
        let state = state.clone();
        // Route the build through `hotreload::call`: it's the subsecond
        // re-entry point, so an applied patch re-runs the patched `view`
        // bodies on the next rebuild. Plain call-through when the feature
        // is off.
        app.scene(move |s| {
            // Read the active view under a short shared borrow, released
            // before the view's own `build` takes its build-phase borrow.
            let view = state.borrow().router.view;
            hotreload::call(|| match view {
                View::Setup => setup_view.build(s),
                View::Splash | View::Login => login_view.build(s),
                View::Home => home_view.build(s),
            })
        })
    };

    let app = {
        let state = state.clone();
        let worker = worker.clone();
        let rebuild = rebuild.clone();
        let msgs = msgs.clone();
        app.on_frame(move |ctx, tl, now| {
            app::frame::tick(&state, &worker, &rebuild, &msgs, ctx, tl, now)
        })
    };

    // Force a final prefs flush on app close — picks up any mouse-up
    // event we might have missed (e.g. drag released outside the
    // window) and persists the live player snapshot so the next
    // launch can re-hydrate the chrome immediately.
    let state_for_exit = state.clone();
    let app = app.on_exit(move || {
        let mut guard = state_for_exit.borrow_mut();
        // Reborrow to a plain `&mut AppState` so the disjoint field borrows
        // below (mut `prefs` + shared `player_ui`/`canvas`) are allowed — a
        // `RefMut` would deref-borrow the whole guard.
        let st = &mut *guard;
        st.prefs.flush_on_exit(
            st.player_ui.snapshot.as_ref(),
            st.canvas.show.get(),
        );
    });

    // Attach a scripted-input run if the debug config carries one
    // (REMOVABLE — `automation` feature).
    #[cfg(feature = "automation")]
    let app = match debug_cfg.as_ref().map(|c| c.script()) {
        Some(script) if !script.steps.is_empty() => app.automation(script),
        _ => app,
    };

    app.run()
}
