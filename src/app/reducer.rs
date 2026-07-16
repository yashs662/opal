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
            state.library.home = data;
            cx.rebuild();
        }
        WorkerResponse::SpotifySessionConnected { device_id } => {
            log::info!("librespot session ready — seeding initial /me/player state");
            // Cold-start seed for the card sections from the restored
            // snapshot — with nothing playing anywhere, no player push
            // will ever arrive to trigger them. Credits specifically
            // need this session: a pre-session attempt was skipped
            // worker-side, so clear its in-flight key and re-run.
            state.player_ui.np_credits_inflight = None;
            refresh_np_sections(state, worker, cx);
            state.devices.self_id = device_id;
            // Transport can act now — drop the loading state on the play button.
            state.player_ui.session_ready.set(true);
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
                        (
                            p.track_id.clone(),
                            p.live_progress_ms() as u32,
                            p.context_uri.clone(),
                        )
                    })
                })
                .flatten();
            if let Some((track_uri, position_ms, context_uri)) = claim {
                log::info!("active device vanished while playing — taking over on Opal (paused)");
                worker.claim_playback_paused(context_uri, track_uri, position_ms);
            }
        }
        WorkerResponse::SavedState { track_id, saved } => {
            // The playing track's heart — a late echo for a track we've
            // skipped past must not flip the new track's state.
            let current = state
                .player_ui
                .with_snapshot(|p| track_id_from_uri(&p.track_id).map(|s| s.to_string()))
                .flatten();
            if current.as_deref() == Some(track_id.as_str()) {
                state.player_ui.liked.set(saved);
                // Liked is one of the heart's "in library" inputs — refresh
                // the tooltip (which lists Liked Songs + playlists).
                state.membership.rebuild_hint(saved);
            }
            // The picker's target (any track) resolves independently.
            if state.membership.target.id() == track_id {
                state.membership.target_liked = saved;
            }
            if state.membership.overlay.is_open() {
                cx.rebuild();
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
            // Cluster context/autoplay entries often arrive as bare uris
            // (no title/duration/cover) — resolve them in one batched
            // metadata request; `TracksHydrated` patches the rows in.
            let bare: Vec<String> = tracks
                .iter()
                .filter(|t| t.name.is_empty())
                .map(|t| t.uri.clone())
                .collect();
            if !bare.is_empty() {
                worker.hydrate_tracks(bare);
            }
            state.library.queue = Some(tracks);
            if matches!(state.router.nav, crate::views::MainNav::Queue) {
                cx.rebuild();
            }
        }
        WorkerResponse::TracksHydrated { tracks } => {
            if tracks.is_empty() {
                return;
            }
            let Some(queue) = state.library.queue.as_mut() else {
                return;
            };
            // Patch resolved rows into the live queue by uri (still-blank
            // entries only — a fresher QueueLoaded may have landed since).
            let mut patched = false;
            for row in queue.iter_mut().filter(|r| r.name.is_empty()) {
                if let Some(full) = tracks.iter().find(|t| t.uri == row.uri) {
                    *row = full.clone();
                    patched = true;
                }
            }
            if patched {
                for tr in state.library.queue.as_deref().unwrap_or_default() {
                    if let Some(url) = &tr.album_image_url {
                        state.art.or_signal(album_art::cache_key(url));
                        state.art.dispatch_cover(worker, url.clone());
                    }
                }
                if matches!(state.router.nav, crate::views::MainNav::Queue) {
                    cx.rebuild();
                }
            }
        }
        WorkerResponse::MembershipLoaded {
            playlists,
            index,
            artist_index,
            liked,
        } => {
            log::info!("playlist-membership ready: {} playlists", playlists.len());
            state
                .membership
                .set_playlists(playlists, index.clone(), artist_index, liked);
            // Resolve the current track's membership now that the index is up.
            if let Some(uri) = state.player_ui.current_track_uri() {
                worker.query_membership(uri);
            }
            if let Some(open) = state.library.open_playlist.as_ref() {
                let rows = open.rows.clone();
                let liked_page = open.liked;
                let mut rows = rows.borrow_mut();
                for row in rows.iter_mut() {
                    row.in_library = liked_page || state.membership.is_saved(&row.uri);
                }
            }
            if state.library.open_playlist.is_some() {
                cx.rebuild();
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
            // The playing track's heart (late answers for skipped tracks
            // are ignored) …
            let current = state.player_ui.current_track_uri();
            if current.as_deref() == Some(track_uri.as_str()) {
                state
                    .membership
                    .set_current(playlist_ids.clone(), state.player_ui.liked.get());
            }
            // … and the picker's target (any track), independently.
            if state.membership.target.uri() == track_uri {
                state.membership.target_ids = playlist_ids.into_iter().collect();
                state.membership.target_ready = true;
            }
            if state.membership.overlay.is_open() {
                cx.rebuild();
            }
        }
        WorkerResponse::MembershipEditFailed {
            track_uri,
            playlist_id,
            was_add,
        } => {
            // Undo the optimistic checkbox flip (re-toggle the opposite
            // way) on whichever states hold this track.
            if state.membership.target.uri() == track_uri {
                state.membership.toggle_target_local(&playlist_id, !was_add);
            }
            let current = state.player_ui.current_track_uri();
            if current.as_deref() == Some(track_uri.as_str()) {
                state.membership.toggle_current_local(
                    &playlist_id,
                    !was_add,
                    state.player_ui.liked.get(),
                );
            }
            let entry = state.membership.index.entry(track_uri.clone()).or_default();
            if was_add {
                entry.retain(|p| p != &playlist_id);
                if entry.is_empty() {
                    state.membership.index.remove(&track_uri);
                }
            } else if !entry.contains(&playlist_id) {
                entry.push(playlist_id.clone());
            }
            if state.membership.overlay.is_open() {
                cx.rebuild();
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
            // No Connect device, but the user can still drive a remote device
            // over the Web API — enable transport rather than leave it loading.
            state.player_ui.session_ready.set(true);
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
                                (
                                    prev.name.clone(),
                                    prev.artist.clone(),
                                    prev.album_image_url.clone(),
                                )
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
                            if p.artist_id.is_empty() {
                                p.artist_id = d.artist_id.clone();
                            }
                            if p.artists.is_empty() {
                                p.artists = d.artists.clone();
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
                // matching this track id. The stale clip is cleared even
                // while canvas is *disabled* — otherwise a toggle-off →
                // track change → toggle-on replays the previous track's
                // cached clip. Only the fetch is gated on `show`.
                if let Some(id) = track_id_from_uri(&p.track_id) {
                    let have = state.canvas.path_matches(id);
                    log::debug!(
                        "canvas gate: track={id} have={have} show={}",
                        state.canvas.show.get()
                    );
                    if !have {
                        state.canvas.clear_path();
                        // Stop the previous track's video now so it doesn't
                        // linger over the new track's art until the new
                        // Canvas (if any) resolves.
                        state.canvas.stop_decode();
                        if state.canvas.show.get() {
                            worker.fetch_canvas(p.track_id.clone(), id.to_string());
                        }
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
            // The clickable multi-artist lines are built from the snapshot
            // at scene-build time (per-artist click targets can't ride a
            // text bind), so a changed credit set needs a rebuild. Same-
            // track progress ticks compare equal and skip.
            if let Some(p) = player {
                let artists_changed = state
                    .player_ui
                    .with_snapshot(|prev| prev.artists != p.artists)
                    .unwrap_or(!p.artists.is_empty());
                state.player_ui.set_snapshot(Some(p));
                if artists_changed {
                    cx.rebuild();
                }
            }
            // `None` (nothing playing on any device) keeps the existing
            // snapshot — the restored last track stays the anchor for the
            // heart, canvas, resume and the card sections; overwriting it
            // with `None` here used to blank all of them on a cold start
            // with no active session anywhere.
            refresh_np_sections(state, worker, cx);
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
                    p.album_image_url
                        .as_ref()
                        .map(|u| album_art::cache_key(u))
                        .as_deref()
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
                state
                    .backdrop
                    .promote(handle, Some(accent), luma, cx.tl, cx.now);
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
                        p.album_image_url
                            .as_ref()
                            .map(|u| album_art::cache_key(u))
                            .as_deref()
                            == Some(key.as_str())
                    })
                    .unwrap_or(false);
            if is_current {
                state.backdrop.set_accent(accent, cx.tl, cx.now);
            }
        }
        WorkerResponse::ArtistCardReady { detail, bio } => {
            state.player_ui.np_about_inflight = None;
            // Drop a late arrival that no longer matches the current
            // track's artist (rapid skips) — the in-flight fetch for the
            // right artist is still coming.
            let current = state
                .player_ui
                .with_snapshot(|p| p.artist_id.clone())
                .unwrap_or_default();
            if !current.is_empty() && current != detail.id {
                log::debug!("stale artist card for {} — ignoring", detail.id);
                return;
            }
            // Register + fetch the artist image so the resolved handle has
            // a signal to land in (the card binds it reactively).
            if let Some(url) = detail.image_url.clone() {
                state.art.or_signal(crate::album_art::cache_key(&url));
                state.art.dispatch_cover(worker, url);
            }
            state.player_ui.np_about = Some(detail);
            state.player_ui.np_bio = bio;
            cx.rebuild();
        }
        WorkerResponse::ContextNameReady { uri, name } => {
            state.player_ui.context_inflight = None;
            // Remember even a failed resolution (empty name) so the
            // per-push gate doesn't re-dispatch this uri forever.
            state.player_ui.resolved_context =
                Some((uri.clone(), name.clone().unwrap_or_default()));
            // Apply to the live pill only if the playing context still
            // matches and the pushes still carry no name of their own.
            if let Some(n) = name
                && state
                    .player_ui
                    .with_snapshot(|p| {
                        p.context_name.is_none() && p.context_uri.as_deref() == Some(uri.as_str())
                    })
                    .unwrap_or(false)
            {
                state.player_ui.context_label.set(n.as_str());
            }
        }
        WorkerResponse::ContextsResolved { map } => {
            // Register + dispatch each context cover so the Recents session
            // rows can bind a reactive thumb, then store the resolved info.
            for info in map.values() {
                if let Some(url) = info.cover_url.clone() {
                    let key = album_art::cache_key(&url);
                    state.art.or_signal(key);
                    state.art.dispatch_cover(worker, url);
                }
            }
            state.library.recent_contexts.extend(map);
            cx.rebuild();
        }
        WorkerResponse::SearchResults { query, results } => {
            // Drop a late response whose query the user has moved past.
            if state.search.dispatched != query {
                return;
            }
            // Register + dispatch every result cover so the tiles/rows can
            // bind a reactive thumb.
            let covers = results
                .tracks
                .iter()
                .filter_map(|t| t.album_image_url.clone())
                .chain(results.artists.iter().filter_map(|a| a.image_url.clone()))
                .chain(results.albums.iter().filter_map(|a| a.image_url.clone()))
                .chain(results.playlists.iter().filter_map(|p| p.image_url.clone()));
            for url in covers {
                state.art.or_signal(album_art::cache_key(&url));
                state.art.dispatch_cover(worker, url);
            }
            state.search.results = Some(results);
            // Grow/shrink the modal to fit the new results (overlay morph).
            let target = crate::views::home::search_modal::target_h(&state.search);
            state.search.overlay.morph_to(cx.tl, cx.now, target);
            cx.rebuild();
        }
        WorkerResponse::TrackCreditsReady { track_id, credits } => {
            state.player_ui.np_credits_inflight = None;
            // Key the rows by track so a late arrival for a skipped track
            // can never caption the current one.
            let matches_current = state
                .player_ui
                .with_snapshot(|p| track_id_from_uri(&p.track_id).map(|cur| cur == track_id))
                .flatten()
                .unwrap_or(false);
            if !matches_current {
                log::debug!("stale credits for {track_id} — ignoring");
                return;
            }
            state.player_ui.np_credits = Some((track_id, credits));
            cx.rebuild();
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
                let buf = state.library.open_playlist.as_mut().map(|o| {
                    o.name = detail.name.clone();
                    o.owner = detail.owner.clone();
                    o.image_url = detail.image_url.clone();
                    o.context_uri = detail.context_uri.clone();
                    o.total = detail.total;
                    o.loading = false;
                    o.complete = complete;
                    o.rows.clone()
                });
                if let Some(buf) = buf {
                    let liked = state
                        .library
                        .open_playlist
                        .as_ref()
                        .map(|o| o.liked)
                        .unwrap_or(false);
                    buf.borrow_mut().clear();
                    state.library.build_rows(
                        &mut state.art,
                        &buf,
                        &detail.tracks,
                        liked,
                        &state.membership,
                    );
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
                let buf = state.library.open_playlist.as_ref().map(|o| o.rows.clone());
                if let Some(buf) = buf {
                    let liked = state
                        .library
                        .open_playlist
                        .as_ref()
                        .map(|o| o.liked)
                        .unwrap_or(false);
                    state.library.build_rows(
                        &mut state.art,
                        &buf,
                        &tracks,
                        liked,
                        &state.membership,
                    );
                    // Tell the frame tick to re-materialize the lazy list:
                    // rows the user scrolled past while this page was in
                    // flight are on screen as skeletons and won't re-render
                    // from a buffer append alone.
                    state.library.rows_appended = true;
                }
                if done && let Some(o) = state.library.open_playlist.as_mut() {
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
            library_tracks,
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
                    .chain(top_tracks.iter().filter_map(|t| t.album_image_url.as_ref()))
                    .chain(
                        library_tracks
                            .iter()
                            .filter_map(|(t, _)| t.album_image_url.as_ref()),
                    );
                for u in covers {
                    state.art.or_signal(album_art::cache_key(u));
                    state.art.dispatch_cover(worker, u.clone());
                }
                if let Some(a) = state.library.open_artist.as_mut() {
                    a.name = name;
                    a.image_url = image_url;
                    a.followers = followers;
                    a.top_tracks = top_tracks;
                    a.albums = albums;
                    a.library_tracks = library_tracks;
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
            let mut backfilled_artist_id = false;
            let mut backfilled_artists = false;
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
                if p.artist_id.is_empty() && !details.artist_id.is_empty() {
                    p.artist_id = details.artist_id.clone();
                    backfilled_artist_id = true;
                }
                if p.artists.is_empty() && !details.artists.is_empty() {
                    p.artists = details.artists.clone();
                    backfilled_artists = true;
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
            // The cluster push carried no artist_uri — the about-card gate
            // couldn't run on the push itself, so run it now that the
            // detail fetch resolved the artist.
            if backfilled_artist_id {
                refresh_artist_card(state, worker, cx, &details.artist_id);
            }
            // The multi-artist lines build from the snapshot — a filled-in
            // credit set needs the click targets rebuilt.
            if backfilled_artists {
                cx.rebuild();
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

/// Fill the now-playing card's track-derived sections — about-artist,
/// credits, the source-pill name — for whatever the snapshot currently
/// holds. Every gate self-dedups (shown/in-flight keys), so this is safe
/// from any trigger: a live push, the cold-start restore (with no active
/// session anywhere, no push ever arrives to do it), or session-connect
/// (credits ride the librespot session, not the Web API token).
fn refresh_np_sections(state: &mut AppState, worker: &Worker, cx: &mut Cx) {
    let Some((track_id, artist_id, context_uri, context_named)) =
        state.player_ui.with_snapshot(|p| {
            (
                p.track_id.clone(),
                p.artist_id.clone(),
                p.context_uri.clone(),
                p.context_name.is_some(),
            )
        })
    else {
        return;
    };
    refresh_artist_card(state, worker, cx, &artist_id);
    // Credits: refresh when the track actually changes. The stale rows
    // are dropped right away so the previous track never captions this one.
    if let Some(id) = track_id_from_uri(&track_id)
        && state.player_ui.np_credits.as_ref().map(|(t, _)| t.as_str()) != Some(id)
        && state.player_ui.np_credits_inflight.as_deref() != Some(id)
    {
        if state.player_ui.np_credits.take().is_some() {
            cx.rebuild();
        }
        state.player_ui.np_credits_inflight = Some(id.to_string());
        worker.fetch_track_credits(id.to_string());
    }
    // Queue-source pill: the state carries no display name (cold-start
    // seed/restore, our own local playback) — resolve it once per context
    // uri via the Web API. The resolved (or failed) entry gates re-dispatch.
    if !context_named
        && let Some(uri) = context_uri.as_deref()
        && state
            .player_ui
            .resolved_context
            .as_ref()
            .map(|(u, _)| u.as_str())
            != Some(uri)
        && state.player_ui.context_inflight.as_deref() != Some(uri)
        && let Some(token) = state.auth.token()
    {
        state.player_ui.context_inflight = Some(uri.to_string());
        worker.fetch_context_name(token, uri.to_string());
    }
}

/// Keep the now-playing "About the artist" section on the *current*
/// artist: when `artist_id` differs from what's shown (and isn't already
/// in flight), drop the stale section immediately and dispatch the card
/// fetch. Callable from any artist-id source — the player push, or the
/// track-details backfill when the push shipped no `artist_uri`.
fn refresh_artist_card(state: &mut AppState, worker: &Worker, cx: &mut Cx, artist_id: &str) {
    if artist_id.is_empty()
        || state.player_ui.np_about.as_ref().map(|a| a.id.as_str()) == Some(artist_id)
        || state.player_ui.np_about_inflight.as_deref() == Some(artist_id)
    {
        return;
    }
    let Some(token) = state.auth.token() else {
        return;
    };
    if state.player_ui.np_about.take().is_some() {
        state.player_ui.np_bio = None;
        cx.rebuild();
    }
    state.player_ui.np_about_inflight = Some(artist_id.to_string());
    worker.fetch_artist_card(token, artist_id.to_string());
}
