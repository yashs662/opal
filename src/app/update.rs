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
use crate::model::MembershipTarget;
use crate::views::View;
use crate::views::home::{PlayerAction, navigate, target_track};
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

        Msg::Transport(action) => {
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
                            (p.track_id.clone(), p.progress_ms as u32, p.context_uri.clone())
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
                PlayerAction::ToggleShuffle => PlaybackCmd::Shuffle(state.player_ui.toggle_shuffle()),
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
            // Point the picker at the current track (uri + bare id for the
            // Liked-Songs row + header), then rebuild so the popup mounts.
            if let Some(target) = state.player_ui.with_snapshot(|p| {
                let id = track_id_from_uri(&p.track_id).unwrap_or_default().to_string();
                MembershipTarget {
                    uri: p.track_id.clone(),
                    id,
                    name: p.name.clone(),
                    artist: p.artist.clone(),
                }
            }) {
                state.membership.set_target(target);
            }
            cx.rebuild();
        }

        Msg::LikeTogglePlaylist { playlist_id, add } => {
            let Some(token) = state.auth.token() else { return };
            let uri = state.membership.target.uri.clone();
            if uri.is_empty() {
                return;
            }
            // Optimistic flip (heart + checkbox) — the worker confirms, and
            // rolls back via MembershipEditFailed on error.
            state
                .membership
                .toggle_local(&playlist_id, add, state.player_ui.liked.get());
            // Live-patch the open page if it's this playlist, and drop its
            // in-memory cache so a re-open also reflects the edit.
            if add {
                if let Some(track) = target_track(state) {
                    state.library.open_add_track(&mut state.art, false, &playlist_id, &track);
                }
            } else {
                state.library.open_remove_track(false, &playlist_id, &uri);
            }
            state.library.invalidate_cached(&playlist_id);
            worker.edit_membership(token, playlist_id, uri, add);
            cx.rebuild();
        }

        Msg::LikeToggleLiked(add) => {
            let Some(token) = state.auth.token() else { return };
            let target_uri = state.membership.target.uri.clone();
            let id = state.membership.target.id.clone();
            if id.is_empty() {
                return;
            }
            // Optimistic flip; the worker's SavedState echo reconciles.
            state.player_ui.liked.set(add);
            state.membership.rebuild_hint(add);
            // Live-patch the open Liked Songs page + drop its in-memory cache.
            if add {
                if let Some(track) = target_track(state) {
                    state
                        .library
                        .open_add_track(&mut state.art, true, api::LIKED_SONGS_ID, &track);
                }
            } else {
                state
                    .library
                    .open_remove_track(true, api::LIKED_SONGS_ID, &target_uri);
            }
            state.library.invalidate_cached(api::LIKED_SONGS_ID);
            worker.set_saved(token, id, add);
            cx.rebuild();
        }

        Msg::Transfer(device_id) => {
            let Some(token) = state.auth.token() else { return };
            // When leaving Opal itself, carry our locally-tracked position —
            // the Web API transfer drops the librespot device's position and
            // the target would otherwise restart at 0:00.
            let position_ms = state
                .devices
                .playing_on_self
                .get()
                .then(|| state.player_ui.with_snapshot(|p| p.live_progress_ms() as u32).unwrap_or(0));
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

        Msg::Skip(count) => {
            let Some(token) = state.auth.token() else { return };
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
