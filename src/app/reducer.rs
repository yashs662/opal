//! Worker-response reducer — routes each `WorkerResponse` to the model(s)
//! it mutates. Pure model-update logic (no view code); the frame loop
//! drains the worker and calls this per response.

use std::rc::Rc;

use crate::album_art;
use crate::api::track_id_from_uri;
use crate::app::AppState;
use crate::app::cx::Cx;
use crate::views::View;
use crate::worker::{Worker, WorkerResponse};

/// Route to the right pre-auth screen when there's no usable session:
/// the setup view if the user hasn't configured a client id yet, else the
/// login view. Eases in via `go_view` (no-op if already there) + requests
/// the mount rebuild.
fn land_pre_auth(state: &mut AppState, cx: &mut Cx) {
    // Landing here is startup/logout, not a Setup→Login save, so no Back.
    state.router.came_from_setup = false;
    // From Splash (the startup credential check) go to login when a client
    // id is configured, else to first-run setup. `go_view` retweens + we
    // request the mount rebuild; it no-ops if we're already there.
    let target = if state.prefs.data.client_id().is_some() {
        View::Login
    } else {
        View::Setup
    };
    if state.router.view != target {
        state.router.go_view(target, cx.tl, cx.now);
        cx.rebuild();
    }
}

pub fn handle(state: &mut AppState, cx: &mut Cx, worker: &Rc<Worker>, resp: WorkerResponse) {
    match resp {
        WorkerResponse::PlaybackFailed { cmd } => {
            // Roll the optimistic chrome flips (play/pause icon, shuffle
            // and repeat tints) back to the authoritative snapshot — the
            // command changed nothing on the server, so the UI must not
            // pretend it did. With no snapshot at all, nothing is playing
            // anywhere: park the transport stopped.
            log::warn!("rolling back optimistic UI for failed {cmd:?}");
            let snap = state.player_ui.snapshot_clone();
            match snap.as_ref() {
                Some(p) => state.player_ui.sync(p, cx.tl, cx.now),
                None => state.player_ui.stopped(cx.tl),
            }
        }
        WorkerResponse::VolumeChanged { fraction } => {
            // Don't fight an in-flight drag — the release commit wins,
            // and its confirmation will land right back here.
            if !state.player_ui.vol_dragging.get() {
                state.player_ui.set_volume_ui(fraction.clamp(0.0, 1.0));
            }
            // Persist (debounced) so the device volume survives restarts
            // and seeds the Connect device's advertised initial volume.
            state.prefs.data.audio.volume = fraction.clamp(0.0, 1.0);
            state.prefs.mark_dirty(cx.now);
        }
        WorkerResponse::OAuthStarted { auth_url } => {
            log::info!("opening browser for OAuth");
            if let Err(e) = webbrowser::open(&auth_url) {
                log::error!("open browser: {e}");
            }
        }
        WorkerResponse::TokensRefreshed { auth } => {
            // Mid-session refresh: swap the live token only. Everything
            // else (home data, librespot session, Spirc device) keeps
            // running — the session authenticated once and stays up.
            state.auth.set(auth);
        }
        WorkerResponse::TokensRefreshFailed => {
            state.auth.refresh_failed();
        }
        WorkerResponse::OAuthComplete { auth } | WorkerResponse::TokensLoaded { auth } => {
            log::info!("auth ok — switching to Home");
            worker.fetch_home(auth.access_token.clone());
            // Build/load the playlist-membership index (disk cache → instant;
            // else a background scan) so the heart reflects playlist
            // membership, not just Liked Songs.
            worker.load_membership(auth.access_token.clone());
            // Cold start: the snapshot was seeded from disk, but the heart's
            // liked + membership need the API/index. Resolve them now for the
            // restored track (membership also re-resolves when the index
            // lands; this covers the fresh-cache fast path).
            if let Some(uri) = state.player_ui.current_track_uri() {
                if let Some(id) = track_id_from_uri(&uri) {
                    worker.check_saved(auth.access_token.clone(), id.to_string());
                }
                worker.query_membership(uri);
            }
            let (initial_volume, quality, normalize) = {
                let p = &state.prefs.data;
                (p.audio.volume, p.audio.quality, p.audio.normalize)
            };
            worker.connect_spotify_session(
                auth.access_token.clone(),
                initial_volume,
                quality,
                normalize,
            );
            state.auth.set(auth);
            if state.router.view != View::Home {
                state.router.view = View::Home;
                cx.rebuild();
            }
        }
        WorkerResponse::OAuthFailed { error } => {
            log::error!("OAuth failed: {error}");
            land_pre_auth(state, cx);
        }
        WorkerResponse::NoStoredTokens => {
            log::info!("no stored tokens — showing pre-auth screen");
            land_pre_auth(state, cx);
        }
        WorkerResponse::HomeData { data } => {
            log::info!(
                "home data ready: playlists={} recent={} top_artists={} top_tracks={}",
                data.playlists.len(),
                data.recent.len(),
                data.top_artists.len(),
                data.top_tracks.len(),
            );
            state.art.prefetch(worker, &data);
            *state.library.home.borrow_mut() = data;
            cx.rebuild();
        }
        WorkerResponse::SpotifySessionConnected { device_id } => {
            log::info!("librespot session ready — seeding initial /me/player state");
            state.devices.self_id = device_id;
            if let Some(token) = state.auth.token() {
                worker.seed_player_state(token);
            }
        }
        WorkerResponse::Devices { devices } => {
            state.devices.list = devices;
            // The popup is open (it dispatched this fetch) — rebuild so
            // the rows appear.
            cx.rebuild();
        }
        WorkerResponse::ActiveDeviceChanged { device_id, is_self } => {
            // A real *other* device is active → light the Devices icon.
            state
                .devices
                .remote_active
                .set(!is_self && !device_id.is_empty());
            state.devices.active_id = device_id;
            state.devices.playing_on_self.set(is_self);
            // Active-row highlight in the popup, if it's open.
            if state.devices.overlay.is_open() {
                cx.rebuild();
            }
        }
        WorkerResponse::ActiveDeviceVanished => {
            // The remote device that was driving playback was quit
            // mid-track. If we were showing it playing, claim playback on
            // Opal (our own Connect device) **paused** at exactly the
            // position it left — Opal becomes the active device so the
            // transport is live + responsive, but playback doesn't resume on
            // its own; the user presses play when they want it. (If it was
            // already paused, do nothing; the following `PlayerState` push
            // freezes the bar as stopped.)
            let claim = state
                .player_ui
                .with_snapshot(|p| {
                    p.is_playing.then(|| {
                        (p.track_id.clone(), p.live_progress_ms() as u32, p.context_uri.clone())
                    })
                })
                .flatten();
            if let Some((track_uri, position_ms, context_uri)) = claim {
                log::info!("active device vanished while playing — taking over on Opal (paused)");
                worker.claim_playback_paused(context_uri, track_uri, position_ms);
            }
        }
        WorkerResponse::SavedState { track_id, saved } => {
            // Only the current track's heart — a late echo for a track
            // we've skipped past must not flip the new track's state.
            let current = state
                .player_ui
                .with_snapshot(|p| track_id_from_uri(&p.track_id).map(|s| s.to_string()))
                .flatten();
            if current.as_deref() == Some(track_id.as_str()) {
                state.player_ui.liked.set(saved);
                // Liked is one of the heart's "in library" inputs — refresh
                // the tooltip (which lists Liked Songs + playlists).
                state.membership.rebuild_hint(saved);
                if state.membership.overlay.is_open() {
                    cx.rebuild();
                }
            }
        }
        WorkerResponse::QueueLoaded { tracks } => {
            // Create the reactive cover signals + dispatch fetches HERE
            // (not in the view build — builds are pure reads of `art`).
            for tr in &tracks {
                if let Some(url) = &tr.album_image_url {
                    state.art.or_signal(album_art::cache_key(url));
                    state.art.dispatch_cover(worker, url.clone());
                }
            }
            *state.library.queue.borrow_mut() = Some(tracks);
            if matches!(state.router.nav, crate::views::MainNav::Queue) {
                cx.rebuild();
            }
        }
        WorkerResponse::MembershipLoaded { playlists } => {
            log::info!("playlist-membership ready: {} playlists", playlists.len());
            state.membership.set_playlists(playlists);
            // Resolve the current track's membership now that the index is up.
            if let Some(uri) = state.player_ui.current_track_uri() {
                worker.query_membership(uri);
            }
            // The picker may already be open waiting on its rows.
            if state.membership.overlay.is_open() {
                cx.rebuild();
            }
        }
        WorkerResponse::TrackMembership {
            track_uri,
            playlist_ids,
        } => {
            // Ignore a late answer for a track we've skipped past.
            let current = state.player_ui.current_track_uri();
            if current.as_deref() == Some(track_uri.as_str()) {
                state
                    .membership
                    .set_current(playlist_ids, state.player_ui.liked.get());
                if state.membership.overlay.is_open() {
                    cx.rebuild();
                }
            }
        }
        WorkerResponse::MembershipEditFailed {
            track_uri,
            playlist_id,
            was_add,
        } => {
            // Undo the optimistic checkbox flip (re-toggle the opposite way),
            // but only if we're still on that track.
            let current = state.player_ui.current_track_uri();
            if current.as_deref() == Some(track_uri.as_str()) {
                state
                    .membership
                    .toggle_local(&playlist_id, !was_add, state.player_ui.liked.get());
                if state.membership.overlay.is_open() {
                    cx.rebuild();
                }
            }
            // Undo the optimistic live-patch of the open page (an added row
            // can be dropped by uri; a failed remove can't be cheaply re-added,
            // so drop the cache and let a re-open re-fetch the truth).
            if was_add {
                state
                    .library
                    .open_remove_track(false, &playlist_id, &track_uri);
            }
            state.library.invalidate_cached(&playlist_id);
            cx.rebuild();
        }
        WorkerResponse::SpotifySessionFailed { error } => {
            log::warn!("librespot session failed: {error}. Falling back to Web API polling.");
        }
        WorkerResponse::SpotifySessionLost => {
            // The Connect device dropped (long session) — re-bootstrap it with
            // the current (proactively-refreshed) token so playback recovers
            // without an app restart. The worker already backed off.
            if let Some(token) = state.auth.token() {
                log::warn!("librespot session lost — reconnecting Connect device");
                let (initial_volume, quality, normalize) = {
                    let p = &state.prefs.data;
                    (p.audio.volume, p.audio.quality, p.audio.normalize)
                };
                worker.connect_spotify_session(token, initial_volume, quality, normalize);
            }
        }
        WorkerResponse::PlayerState { mut player } => {
            // Overlay cached track details (artist) and request a fetch
            // for any track we haven't resolved yet. The cluster's
            // `ProvidedTrack.metadata` only carries `artist_uri`, so the
            // artist name comes from `/v1/tracks/{id}`.
            if let Some(p) = player.as_mut() {
                // Sparse update for the SAME track (e.g. a
                // DEVICES_DISAPPEARED push carries no title metadata):
                // inherit the display fields from the live snapshot
                // instead of blanking the chrome.
                if p.name.is_empty()
                    && let Some((name, artist, image)) = state
                        .player_ui
                        .with_snapshot(|prev| {
                            (prev.track_id == p.track_id).then(|| {
                                (prev.name.clone(), prev.artist.clone(), prev.album_image_url.clone())
                            })
                        })
                        .flatten()
                {
                    p.name = name;
                    if p.artist.is_empty() {
                        p.artist = artist;
                    }
                    if p.album_image_url.is_none() {
                        p.album_image_url = image;
                    }
                }
                if let Some(id) = track_id_from_uri(&p.track_id) {
                    match state.art.track_detail(id) {
                        Some(d) => {
                            // `/v1/tracks/{id}` fills whatever the cluster
                            // metadata didn't carry.
                            if p.name.is_empty() {
                                p.name = d.name.clone();
                            }
                            if !d.artist.is_empty() {
                                p.artist = d.artist.clone();
                            }
                            if p.album_image_url.is_none() {
                                p.album_image_url = d.album_image_url.clone();
                            }
                        }
                        None => {
                            if let Some(token) = state.auth.token() {
                                worker.fetch_track_details(token, id.to_string());
                            }
                        }
                    }
                }
                // Dispatch an album-art fetch when the cover actually
                // changes. Skip when it's already what's on screen (same
                // track, just a progress tick) or a fetch is already in
                // flight. The fetch is disk-backed, so re-loading a cover
                // we've seen before is cheap and yields a fresh, tree-live
                // handle — see `art_inflight` doc for why we don't cache
                // handles across tracks.
                if let Some(url) = p.album_image_url.as_ref() {
                    let key = album_art::cache_key(url);
                    if !state.art.is_shown(&key) && !state.art.is_inflight(&key) {
                        state.art.mark_inflight(key.clone());
                        worker.fetch_album_art(url.clone(), key.clone());
                    }
                    // Spotify's own accent for this cover (authoritative over
                    // the pixel-average extracted on art decode). Dispatched
                    // **independently of the art dedup** — otherwise a cover
                    // whose art is already shown / in flight would never get
                    // its accent fetched, leaving the *previous* track's
                    // accent on screen. Gated once per cover (disk-cached).
                    if !state.art.has_accent(&key) {
                        worker.fetch_accent(key);
                    }
                }
                // Track changed → re-check the heart (assume un-liked
                // until the check returns; the worker echo flips it).
                let track_changed = state
                    .player_ui
                    .with_snapshot(|prev| prev.track_id != p.track_id)
                    .unwrap_or(true);
                if track_changed {
                    state.player_ui.liked.set(false);
                    if let Some(id) = track_id_from_uri(&p.track_id)
                        && let Some(token) = state.auth.token()
                    {
                        worker.check_saved(token, id.to_string());
                    }
                    // Resolve playlist membership for the heart fill + tooltip
                    // (cheap index lookup on the worker). Clear the old state
                    // first so a stale heart doesn't linger until the answer.
                    state.membership.set_current(Vec::new(), false);
                    worker.query_membership(p.track_id.clone());
                    // Self-play has no cluster queue echo, so the queue page
                    // would freeze on a stale list as autoplay advances. While
                    // it's open and we're the active player, re-pull the Web
                    // API queue each track so the continuation stays live.
                    if state.devices.playing_on_self.get()
                        && matches!(state.router.nav, crate::views::MainNav::Queue)
                        && let Some(token) = state.auth.token()
                    {
                        worker.fetch_queue(token);
                    }
                }
                // Canvas video: fetch on a real track change (not a
                // progress tick). Gate on the cached canvas not already
                // matching this track id; clear any stale canvas first so
                // the UI falls back to art until the new one resolves.
                if let Some(id) = track_id_from_uri(&p.track_id) {
                    let have = state.canvas.path_matches(id);
                    log::debug!(
                        "canvas gate: track={id} have={have} show={}",
                        state.canvas.show.get()
                    );
                    if !have && state.canvas.show.get() {
                        state.canvas.clear_path();
                        // Stop the previous track's video now so it doesn't
                        // linger over the new track's art until the new
                        // Canvas (if any) resolves.
                        state.canvas.stop_decode();
                        worker.fetch_canvas(p.track_id.clone(), id.to_string());
                    }
                }
            }
            // Push every dynamic field into its reactive signal (all
            // dedup'd, so a same-track progress tick only bumps what
            // changed). Title/artist → text binds, is_playing → play/pause
            // image bind, shuffle/repeat → tint colour binds, progress →
            // % width bind. Nothing here needs a scene rebuild anymore.
            // Push the snapshot into the reactive chrome (all dedup'd, so a
            // same-track progress tick only bumps what changed). A `None`
            // (nothing playing on any device) keeps the last track visible —
            // the cold-start path seeds title/artist/progress from
            // `prefs.last_player`, so we just mark stopped + freeze the bar
            // rather than clobbering that restored state to a dash.
            match player.as_ref() {
                Some(p) => state.player_ui.sync(p, cx.tl, cx.now),
                None => state.player_ui.stopped(cx.tl),
            }
            state.player_ui.set_snapshot(player);
        }
        WorkerResponse::AlbumArtReady {
            key,
            handle,
            accent,
            luma,
        } => {
            state.art.clear_inflight(&key);
            // Push the resolved handle into the per-URL Home signal (if
            // any tile bound to this key) — repaints just those nodes via
            // the image bind, no rebuild.
            state.art.set_resolved(&key, handle);
            // Promote into the crossfade if this cover matches either:
            // (a) the live player (steady-state path — a live track
            //     change resolved), or
            // (b) the persisted `last_player` snapshot AND no live
            //     player has landed yet (cold-start path — disk cache
            //     hit beats the first cluster push so we'd otherwise
            //     discard the art handle and re-fetch later, costing
            //     the user a visible "blank → fade-in" delay).
            // No handle cache: the fresh handle is tree-live once
            // promoted, so it survives atlas eviction. A rapid switch
            // that moved on before the upload landed just leaves the
            // orphan handle for the atlas to evict.
            let live_match = state
                .player_ui
                .with_snapshot(|p| {
                    p.album_image_url.as_ref().map(|u| album_art::cache_key(u)).as_deref()
                        == Some(key.as_str())
                })
                .unwrap_or(false);
            let cold_start_match = !live_match
                && !state.player_ui.has_snapshot()
                && state
                    .prefs
                    .data
                    .last_player
                    .as_ref()
                    .and_then(|p| p.album_image_url.as_ref().map(|u| album_art::cache_key(u)))
                    .map(|k| k == key)
                    .unwrap_or(false);
            if live_match || cold_start_match {
                // Prefer Spotify's own extracted colour if it already
                // arrived for this cover; otherwise use the pixel-average
                // as a provisional accent (a later `AccentReady` overrides
                // it). This makes the result order-independent between the
                // two parallel requests.
                let accent = state.art.accent(&key).unwrap_or(accent);
                // No rebuild: promote swaps the handles via the reactive
                // image-handle binds and starts the crossfade tween, both
                // pumped by the lib without re-running the scene closure.
                state.backdrop.promote(handle, Some(accent), luma, cx.tl, cx.now);
                state.art.set_shown(key);
            }
        }
        WorkerResponse::AlbumArtFailed { key } => {
            state.art.clear_inflight(&key);
        }
        WorkerResponse::AccentReady { key, accent } => {
            state.art.cache_accent(key.clone(), accent);
            // Apply only if this cover is the one on screen now (or the
            // live player's) — a late arrival for a skipped track is kept
            // in the map but not tweened in. Overrides any provisional
            // pixel-average accent with Spotify's exact colour.
            let is_current = state.art.is_shown(&key)
                || state
                    .player_ui
                    .with_snapshot(|p| {
                        p.album_image_url.as_ref().map(|u| album_art::cache_key(u)).as_deref()
                            == Some(key.as_str())
                    })
                    .unwrap_or(false);
            if is_current {
                state.backdrop.set_accent(accent, cx.tl, cx.now);
            }
        }
        WorkerResponse::CanvasReady { track_id, path } => {
            // Ignore a canvas that no longer matches the current track —
            // e.g. a cold-start canvas (for the last-played track) arriving
            // after a *different* live track has already started. With no
            // snapshot yet (cold start, nothing playing) there's nothing to
            // contradict, so accept it — that's the case this restores.
            let matches_current = state
                .player_ui
                .with_snapshot(|p| track_id_from_uri(&p.track_id).map(|cur| cur == track_id))
                .flatten()
                .unwrap_or(true);
            if !matches_current {
                log::debug!("stale canvas for {track_id} — ignoring");
                return;
            }
            log::info!("canvas ready for {track_id}: {}", path.display());
            state.canvas.set_path(track_id.clone(), path.clone());
            // Only decode if still wanted (canvas enabled). A late arrival
            // for a track the user already skipped past is harmless — the
            // next track change stops/replaces this session.
            if state.canvas.show.get() {
                state.canvas.start_decode(track_id, path);
            }
        }
        WorkerResponse::CanvasNone { track_id } => {
            log::debug!("no canvas for {track_id} — album art fallback");
            if state.canvas.path_matches(&track_id) {
                state.canvas.clear_path();
            }
            // No Canvas for this track → stop any running decode + fall
            // back to art.
            state.canvas.stop_decode();
        }
        WorkerResponse::PlaylistOpened { detail, complete } => {
            let id = detail.id.clone();
            // Apply to the open pane if it's still showing this playlist:
            // overwrite metadata + seed the first page, then rebuild ONCE
            // to mount the full-length virtualised list (item_count =
            // total). Subsequent pages append without a rebuild.
            let applies = state.router.nav_is_open(&id);
            if applies {
                let buf = {
                    let mut op = state.library.open_playlist.borrow_mut();
                    op.as_mut().map(|o| {
                        o.name = detail.name.clone();
                        o.owner = detail.owner.clone();
                        o.image_url = detail.image_url.clone();
                        o.context_uri = detail.context_uri.clone();
                        o.total = detail.total;
                        o.loading = false;
                        o.complete = complete;
                        o.rows.clone()
                    })
                };
                if let Some(buf) = buf {
                    buf.borrow_mut().clear();
                    state.library.build_rows(&state.art, &buf, &detail.tracks);
                    cx.rebuild();
                }
            }
            // A `complete` response (disk-cache hit or single-page) carries
            // the whole listing — cache it in memory for an instant
            // re-open and clear the inflight gate.
            if complete {
                state.library.clear_inflight(&id);
                state.library.cache(detail);
            }
        }
        WorkerResponse::PlaylistTracks { id, tracks, done } => {
            // Append a streamed page into the live buffer — no rebuild;
            // the lazy_list reads it on scroll. (Covers fill in reactively
            // via the per-row image bind baked in `build_rows`.)
            let applies = state.router.nav_is_open(&id);
            if applies {
                let buf = state
                    .library
                    .open_playlist
                    .borrow()
                    .as_ref()
                    .map(|o| o.rows.clone());
                if let Some(buf) = buf {
                    state.library.build_rows(&state.art, &buf, &tracks);
                    // Tell the frame tick to re-materialize the lazy list:
                    // rows the user scrolled past while this page was in
                    // flight are on screen as skeletons and won't re-render
                    // from a buffer append alone.
                    state.library.rows_appended.set(true);
                }
                if done && let Some(o) = state.library.open_playlist.borrow_mut().as_mut() {
                    o.complete = true;
                }
            }
            if done {
                state.library.clear_inflight(&id);
            }
        }
        WorkerResponse::PlaylistFailed { id, error } => {
            state.library.clear_inflight(&id);
            log::warn!("playlist {id} load failed: {error}");
        }
        WorkerResponse::ArtistOpened {
            id,
            name,
            image_url,
            followers,
            top_tracks,
            albums,
        } => {
            state.library.clear_inflight(&id);
            if state.router.nav_is_artist(&id) {
                // Create the reactive cover signals + dispatch fetches HERE
                // (not in the view build) — the build holds an immutable
                // borrow of `home_art`, so `or_signal`'s borrow_mut there
                // panics. Later `AlbumArtReady` fills these via set_resolved.
                if let Some(u) = &image_url {
                    state.art.or_signal(album_art::cache_key(u));
                    state.art.dispatch_cover(worker, u.clone());
                }
                let covers = albums
                    .iter()
                    .filter_map(|al| al.image_url.as_ref())
                    .chain(top_tracks.iter().filter_map(|t| t.album_image_url.as_ref()));
                for u in covers {
                    state.art.or_signal(album_art::cache_key(u));
                    state.art.dispatch_cover(worker, u.clone());
                }
                if let Some(a) = state.library.open_artist.borrow_mut().as_mut() {
                    a.name = name;
                    a.image_url = image_url;
                    a.followers = followers;
                    a.top_tracks = top_tracks;
                    a.albums = albums;
                    a.loading = false;
                }
                cx.rebuild();
            }
        }
        WorkerResponse::ArtistFailed { id, error } => {
            state.library.clear_inflight(&id);
            log::warn!("artist {id} load failed: {error}");
        }
        WorkerResponse::TrackDetails { details } => {
            let track_id = details.track_id.clone();
            state.art.insert_track_detail(details.clone());
            // Patch the live player view if it still matches, pushing each
            // field into its reactive signal — updates the labels via the
            // text binds, no rebuild (this is the one that used to land
            // mid-crossfade). Fills the name too: sparse cluster pushes
            // (no title metadata) arrive with an empty one.
            let mut fetch_cover: Option<String> = None;
            let mut set_title = false;
            let mut set_artist = false;
            // Patch the snapshot under a short borrow; the signal sets and the
            // cover fetch happen *after* it's released (see `patch_snapshot`)
            // so no model borrow is ever held across a call.
            state.player_ui.patch_snapshot(|p| {
                if track_id_from_uri(&p.track_id) != Some(track_id.as_str()) {
                    return;
                }
                if p.name.is_empty() && !details.name.is_empty() {
                    p.name = details.name.clone();
                    set_title = true;
                }
                if !details.artist.is_empty() {
                    p.artist = details.artist.clone();
                    set_artist = true;
                }
                if p.album_image_url.is_none() && details.album_image_url.is_some() {
                    p.album_image_url = details.album_image_url.clone();
                    fetch_cover = details.album_image_url.clone();
                }
            });
            if set_title {
                state.player_ui.title.set(details.name.as_str());
            }
            if set_artist {
                state.player_ui.artist.set(details.artist.as_str());
            }
            // The cluster push had no cover either — backfill the backdrop
            // from the resolved one (outside the snapshot borrow).
            if let Some(url) = fetch_cover {
                let key = album_art::cache_key(&url);
                if !state.art.is_shown(&key) && !state.art.is_inflight(&key) {
                    state.art.mark_inflight(key.clone());
                    worker.fetch_album_art(url, key);
                }
            }
        }
    }
}
