//! `update` — the single reducer for view-emitted [`Msg`]s.
//!
//! The event-side counterpart to [`crate::app::reducer`] (which routes
//! *worker responses*). Both are pure model-update logic with no view code;
//! concentrating every mutation in these two functions is what will make the
//! eventual `&mut AppState` ownership flip mechanical (see `PLAN_TEA.md`).
//! For now it takes `&Rc<AppState>` + interior mutability, exactly like the
//! worker reducer.

use crate::api::{self, PlayTarget, track_id_from_uri};
use crate::app::AppState;
use crate::app::cx::Cx;
use crate::app::msg::{Msg, MsgQueue};
use crate::views::View;
use crate::views::home::{PlayerAction, navigate, target_artist_ids, target_track};
use crate::worker::{PlaybackCmd, Worker};

/// Drain every view intent queued since the last frame, applying each to the
/// models. Called from the frame tick, where a real [`Cx`] is available.
///
/// `pop_front` per iteration keeps the queue borrow short-lived, so an
/// `update` that itself enqueues a follow-up `Msg` can't double-borrow — the
/// new message is simply processed on a later iteration.
pub fn drain(state: &mut AppState, worker: &Worker, msgs: &MsgQueue, cx: &mut Cx) {
    while let Some(msg) = msgs.borrow_mut().pop_front() {
        update(state, worker, cx, msg);
    }
}

/// Apply one view intent to the models. `cx` carries the timeline / instant /
/// rebuild token for intents that animate or restructure the scene.
pub fn update(state: &mut AppState, worker: &Worker, cx: &mut Cx, msg: Msg) {
    match msg {
        Msg::Navigate(nav) => navigate(state, cx, worker, nav),
        Msg::NavBack => crate::views::home::navigate_back(state, cx, worker),
        Msg::NavForward => crate::views::home::navigate_forward(state, cx, worker),

        Msg::Transport(action) => {
            // Ignore transport until the Connect session is ready — on cold
            // start there's no device to act on, so an early press would be a
            // silent no-op (the play button shows a loading pulse meanwhile).
            if !state.player_ui.session_ready.get() {
                return;
            }
            let Some(token) = state.auth.token() else {
                log::warn!("playback action ignored — no auth token");
                return;
            };
            let cmd = match action {
                PlayerAction::PlayPause => {
                    let was_playing = state.player_ui.toggle_play();
                    let local = state.devices.playing_on_self.get();
                    if was_playing {
                        worker.playback(token, PlaybackCmd::Pause, local);
                        return;
                    }
                    // Resume. On cold start nothing is actually playing on any
                    // device (no live push yet — the snapshot is just the
                    // persisted seed), so a bare Web API resume 404s / no-ops:
                    // start the last-played track at exactly the position the
                    // chrome already shows.
                    if !state.player_ui.live {
                        let last = state.prefs.data.last_player.as_ref().map(|p| {
                            (
                                p.track_id.clone(),
                                p.progress_ms as u32,
                                p.context_uri.clone(),
                            )
                        });
                        if let Some((uri, position_ms, context_uri)) = last {
                            worker.playback(
                                token,
                                PlaybackCmd::PlayContext(PlayTarget::Resume {
                                    uri,
                                    position_ms,
                                    context_uri,
                                }),
                                false,
                            );
                            return;
                        }
                    }
                    worker.playback(token, PlaybackCmd::Play, local);
                    return;
                }
                PlayerAction::Next => PlaybackCmd::Next,
                PlayerAction::Prev => PlaybackCmd::Prev,
                PlayerAction::ToggleShuffle => {
                    PlaybackCmd::Shuffle(state.player_ui.toggle_shuffle())
                }
                PlayerAction::CycleRepeat => PlaybackCmd::Repeat(state.player_ui.cycle_repeat()),
                PlayerAction::SetVolume(pct) => PlaybackCmd::Volume(pct),
            };
            // Drive our own Spirc directly when Opal is the active device
            // (instant + reliable; the Web API relay to self can go stale).
            let local = state.devices.playing_on_self.get();
            worker.playback(token, cmd, local);
        }

        Msg::Play(target) => {
            let Some(token) = state.auth.token() else {
                log::warn!("play ignored — no auth token");
                return;
            };
            state.player_ui.is_playing.set(true);
            // PlayContext is a context load, not a transport verb — it always
            // takes the Web API path, so `false` here is the documented default.
            worker.playback(token, PlaybackCmd::PlayContext(target), false);
        }

        Msg::RequestCover(url) => state.art.dispatch_cover(worker, url),

        Msg::CanvasToggle => {
            state.prefs.mark_dirty(cx.now);
            state
                .canvas
                .on_toggle(state.player_ui.snapshot.as_ref(), worker);
        }

        Msg::SignOut => {
            state.auth.sign_out();
            // Leaving Home — snap the modal shut so it isn't up next sign-in.
            state.settings.overlay.reset();
            // Logout lands on Login directly (not from Setup) → no Back.
            state.router.came_from_setup = false;
            state.router.view = View::Login;
            cx.rebuild();
        }

        Msg::SettingsOpen => {
            state.settings.refresh_usage();
            cx.rebuild();
        }

        Msg::DevicesOpen => {
            // Fresh list on every open — devices are live state.
            if let Some(token) = state.auth.token() {
                worker.fetch_devices(token);
            }
            cx.rebuild();
        }

        Msg::LikeOpen => {
            // Point the picker at the current track, seeding its checkbox
            // state from the bar heart's already-resolved membership so the
            // popup is correct on first paint.
            if let Some(track) = state.player_ui.with_snapshot(|p| {
                let id = track_id_from_uri(&p.track_id)
                    .unwrap_or_default()
                    .to_string();
                api::PlaylistTrack {
                    id,
                    uri: p.track_id.clone(),
                    name: p.name.clone(),
                    artist: p.artist.clone(),
                    album: String::new(),
                    album_image_url: p.album_image_url.clone(),
                    duration_ms: p.duration_ms,
                    artists: p.artists.clone(),
                    album_id: String::new(),
                    artist_id: p.artist_id.clone(),
                    playable: true,
                }
            }) {
                let seed = (
                    state.membership.current.clone(),
                    state.player_ui.liked.get(),
                );
                state.membership.set_target(track, Some(seed));
            }
            cx.rebuild();
        }

        Msg::LikeOpenFor(track) => {
            // Row hearts / the context menu's "Add to playlist…": target an
            // arbitrary track. Membership + liked resolve **synchronously**
            // from the in-memory index + liked SoT (no worker round-trip, no
            // network) so the popup's list is populated the instant it opens.
            // The overlay opens here (not at the emitter) so every surface
            // gets identical behavior from the one message.
            let uri = track.uri.clone();
            let ids: std::collections::HashSet<String> = state
                .membership
                .index
                .get(&uri)
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .collect();
            let liked = state.membership.liked.contains(&uri);
            state.membership.set_target(*track, Some((ids, liked)));
            state.membership.overlay.open(cx.tl, cx.now);
            cx.rebuild();
        }

        Msg::LikeTogglePlaylist { playlist_id, add } => {
            let Some(token) = state.auth.token() else {
                return;
            };
            let uri = state.membership.target.uri().to_string();
            if uri.is_empty() {
                return;
            }
            let artist_ids = target_artist_ids(state);
            // Optimistic flip (checkbox) — the worker confirms, and rolls
            // back via MembershipEditFailed on error. The bar heart follows
            // only when the target IS the playing track.
            state.membership.toggle_target_local(&playlist_id, add);
            if state.player_ui.current_track_uri().as_deref() == Some(uri.as_str()) {
                state.membership.toggle_current_local(
                    &playlist_id,
                    add,
                    state.player_ui.liked.get(),
                );
            }
            let entry = state.membership.index.entry(uri.clone()).or_default();
            if add {
                if !entry.contains(&playlist_id) {
                    entry.push(playlist_id.clone());
                }
            } else {
                entry.retain(|p| p != &playlist_id);
                if entry.is_empty() {
                    state.membership.index.remove(&uri);
                }
            }
            // Live-patch the open page if it's this playlist, and drop its
            // in-memory cache so a re-open also reflects the edit.
            if add {
                if let Some(track) = target_track(state) {
                    state.library.open_add_track(
                        &mut state.art,
                        false,
                        &playlist_id,
                        &track,
                        &state.membership,
                    );
                }
            } else {
                state.library.open_remove_track(false, &playlist_id, &uri);
            }
            state.library.invalidate_cached(&playlist_id);
            worker.edit_membership(token, playlist_id, uri, artist_ids, add);
            cx.rebuild();
        }

        Msg::LikeToggleLiked(add) => {
            let Some(token) = state.auth.token() else {
                return;
            };
            let target_uri = state.membership.target.uri().to_string();
            let id = state.membership.target.id().to_string();
            if id.is_empty() {
                return;
            }
            let artist_ids = target_artist_ids(state);
            // Optimistic flip; the worker's SavedState echo reconciles.
            // The playing track's heart follows only when it IS the target.
            state.membership.target_liked = add;
            // Keep the saved-state source of truth live so every row heart
            // (not just this page) reflects the like immediately.
            if add {
                state.membership.liked.insert(target_uri.clone());
            } else {
                state.membership.liked.remove(&target_uri);
            }
            if state.player_ui.current_track_uri().as_deref() == Some(target_uri.as_str()) {
                state.player_ui.liked.set(add);
                state.membership.rebuild_hint(add);
            }
            // Live-patch the open Liked Songs page + drop its in-memory cache.
            if add {
                if let Some(track) = target_track(state) {
                    state.library.open_add_track(
                        &mut state.art,
                        true,
                        api::LIKED_SONGS_ID,
                        &track,
                        &state.membership,
                    );
                }
            } else {
                state
                    .library
                    .open_remove_track(true, api::LIKED_SONGS_ID, &target_uri);
            }
            state.library.invalidate_cached(api::LIKED_SONGS_ID);
            worker.set_saved(token, id, artist_ids, add);
            cx.rebuild();
        }

        Msg::OpenArtistLibrary => {
            // Synthetic playlist page from the open artist's aggregated
            // library rows — reuses the whole playlist pipeline (view,
            // scroll, playback) with no fetch.
            let Some((artist, rows)) = state.library.open_artist.as_ref().map(|a| {
                (
                    a.name.clone(),
                    a.library_tracks
                        .iter()
                        .map(|(t, _)| t.clone())
                        .collect::<Vec<_>>(),
                )
            }) else {
                return;
            };
            if rows.is_empty() {
                return;
            }
            let id = format!("__library__{artist}");
            let name = format!("{artist} in your library");
            state
                .library
                .open_synthetic(&mut state.art, name, rows, &state.membership);
            state.router.go(
                crate::views::MainNav::Playlist { id, liked: false },
                cx.tl,
                cx.now,
            );
            cx.rebuild();
        }

        Msg::Transfer(device_id) => {
            let Some(token) = state.auth.token() else {
                return;
            };
            // When leaving Opal itself, carry our locally-tracked position —
            // the Web API transfer drops the librespot device's position and
            // the target would otherwise restart at 0:00.
            let position_ms = state.devices.playing_on_self.get().then(|| {
                state
                    .player_ui
                    .with_snapshot(|p| p.live_progress_ms() as u32)
                    .unwrap_or(0)
            });
            log::info!("transferring playback to {device_id} (pos={position_ms:?})");
            worker.transfer_playback(token, device_id, position_ms);
        }

        Msg::SetQuality(q) => {
            state.prefs.data.audio.quality = q;
            state.prefs.mark_dirty(cx.now);
            cx.rebuild();
        }

        Msg::ToggleNormalize => {
            // The toggle already flipped the signal; persist its new value
            // (applied at the next session start).
            let on = state.settings.normalize.get();
            state.prefs.data.audio.normalize = on;
            state.prefs.mark_dirty(cx.now);
        }

        Msg::EqBandCommitted => {
            // The drag updated the band signal + shared surface live; on
            // release, re-derive whether the shape still matches a named
            // preset (the panel shows "Custom" otherwise) and persist.
            state.eq.refresh_selected();
            state.prefs.data.audio.eq = state.eq.to_prefs();
            state.prefs.mark_dirty(cx.now);
            cx.rebuild();
        }

        Msg::EqToggleEnabled => {
            let on = state.eq.enabled.get();
            state.eq.set_enabled(on);
            state.prefs.data.audio.eq = state.eq.to_prefs();
            state.prefs.mark_dirty(cx.now);
        }

        Msg::EqTogglePresetOpen => {
            let open = state.eq.preset_open.get();
            state.eq.preset_open.set(!open);
            cx.rebuild();
        }

        Msg::EqApplyPreset(index) => {
            state.eq.preset_open.set(false);
            if let Some(bands) = state.eq.apply_preset(index) {
                // Glide each slider to the preset's value so the change reads
                // as motion, not a snap (the shared surface already jumped).
                for (i, target) in bands.iter().enumerate() {
                    cx.tl.animate(
                        &state.eq.bands[i],
                        *target,
                        opal_gfx::Curve::EaseInOut,
                        std::time::Duration::from_millis(220),
                        cx.now,
                    );
                }
                state.prefs.data.audio.eq = state.eq.to_prefs_with_bands(bands);
                state.prefs.mark_dirty(cx.now);
                cx.rebuild();
            }
        }

        Msg::EqDeleteCustom(index) => {
            state.eq.delete_custom(index);
            state.prefs.data.audio.eq = state.eq.to_prefs();
            state.prefs.mark_dirty(cx.now);
            cx.rebuild();
        }

        Msg::EqStartRename(index) => {
            state.eq.start_rename(index);
            cx.rebuild();
        }

        Msg::EqCommitRename(index) => {
            state.eq.commit_rename(index);
            state.prefs.data.audio.eq = state.eq.to_prefs();
            state.prefs.mark_dirty(cx.now);
            cx.rebuild();
        }

        Msg::EqSaveCustom => {
            state.eq.preset_open.set(false);
            let name = state.eq.next_custom_name();
            state.eq.save_custom(name);
            state.prefs.data.audio.eq = state.eq.to_prefs();
            state.prefs.mark_dirty(cx.now);
            cx.rebuild();
        }

        Msg::ToggleRecentSession(key) => {
            if !state.library.expanded_recents.remove(&key) {
                state.library.expanded_recents.insert(key);
            }
            cx.rebuild();
        }
        Msg::Skip(count) => {
            let Some(token) = state.auth.token() else {
                return;
            };
            // Local (Spirc) skip when Opal is the active device — instant +
            // reliable; else repeated Web API next on the remote device.
            let local = state.devices.playing_on_self.get();
            worker.skip_forward(token, count, local);
        }

        Msg::OpenContextMenu { pos, target } => {
            state.menu.show(target, pos);
            cx.rebuild();
        }

        Msg::AddQueue(uri) => {
            if let Some(token) = state.auth.token() {
                worker.add_to_queue(token, uri);
            }
        }

        Msg::MenuClose => {
            state.menu.close();
            cx.rebuild();
        }

        Msg::NowPlayingToggle => {
            // Flip + slide: everything downstream is signal-bound (pane
            // width/fade ride the open fraction, player-bar toggle tint
            // rides the flag), so no rebuild — the collapse is a pure
            // tween the layout follows.
            let open = !state.prefs.now_playing_open.get();
            state.prefs.now_playing_open.set(open);
            cx.tl.animate(
                &state.prefs.now_playing_open_t,
                if open { 1.0 } else { 0.0 },
                opal_gfx::Curve::EaseInOut,
                std::time::Duration::from_millis(280),
                cx.now,
            );
            state.prefs.mark_dirty(cx.now);
        }

        Msg::ClearCache => {
            // Off-thread clear + re-scan; the storage bar repaints when the
            // fresh usage lands (frame tick → `take_pending_usage`).
            state.settings.clear_cache();
            cx.rebuild();
        }

        Msg::ChangeCacheDir => state.settings.pick_cache_dir(),

        Msg::MarkDirty => state.prefs.mark_dirty(cx.now),

        Msg::StartLogin => {
            if let Some(id) = state.prefs.data.client_id() {
                worker.start_oauth(id);
            }
        }

        Msg::BackToSetup => {
            state.router.go_view(View::Setup, cx.tl, cx.now);
            cx.rebuild();
        }

        Msg::ResetPrefs => {
            state.prefs.reset();
            state.auth.sign_out();
            state.router.go_view(View::Setup, cx.tl, cx.now);
            cx.rebuild();
        }

        Msg::SaveClientId(id) => {
            state.prefs.data.spotify_client_id = Some(id);
            if let Err(e) = state.prefs.data.save() {
                log::warn!("saving client id failed: {e}");
            }
            // Reached Login *from* Setup → offer a Back affordance there.
            state.router.came_from_setup = true;
            state.router.go_view(View::Login, cx.tl, cx.now);
            cx.rebuild();
        }
    }
}
