//! Local librespot player → UI state bridge.
//!
//! When **Opal itself** is the active Connect device, the dealer does
//! not echo our own connect-state back to us — the cluster subscription
//! ([`crate::cluster_listener`]) goes silent and the chrome would never
//! learn what we're playing. This task listens to the player's own event
//! channel instead (the same source of truth the audio pipeline uses) and
//! folds the events into the app's [`CurrentlyPlaying`] domain struct.
//!
//! Both feeds drive the same `WorkerResponse::PlayerState` path, and they
//! are naturally disjoint: remote playback → cluster pushes (this player
//! is idle, no events); local playback → player events (no self-echo from
//! the dealer). Volume rides its own callback — it's device state, not
//! track state.

use std::time::Instant;

use librespot_metadata::audio::UniqueFields;
use librespot_playback::player::{PlayerEvent, PlayerEventChannel};
use log::{debug, info};

use crate::api::{CurrentlyPlaying, RepeatMode};

/// Drain the local player's event channel forever. `on_state` receives a
/// fresh [`CurrentlyPlaying`] after every meaningful transition (`None` =
/// stopped); `on_volume` the mixer volume as a 0..=1 fraction.
pub async fn run<S, V>(mut events: PlayerEventChannel, mut on_state: S, mut on_volume: V)
where
    S: FnMut(Option<CurrentlyPlaying>),
    V: FnMut(f32),
{
    info!("local player listener started");
    // Running state, folded from events. Track metadata arrives once per
    // track (`TrackChanged`); play/pause/seek then only touch the
    // position/flags. Nothing is emitted until the first track lands.
    let mut current: Option<CurrentlyPlaying> = None;
    while let Some(event) = events.recv().await {
        match event {
            PlayerEvent::TrackChanged { audio_item } => {
                let credited: Vec<crate::api::TrackArtist> = match &audio_item.unique_fields {
                    UniqueFields::Track { artists, .. } => artists
                        .iter()
                        .map(|a| crate::api::TrackArtist {
                            id: a.id.to_id(),
                            name: a.name.clone(),
                        })
                        .collect(),
                    UniqueFields::Episode { show_name, .. } => vec![crate::api::TrackArtist {
                        id: String::new(),
                        name: show_name.clone(),
                    }],
                    UniqueFields::Local { artists, .. } => artists
                        .clone()
                        .into_iter()
                        .filter(|a| !a.is_empty())
                        .map(|a| crate::api::TrackArtist {
                            id: String::new(),
                            name: a,
                        })
                        .collect(),
                };
                let artist = credited
                    .iter()
                    .map(|a| a.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                let artist_id = credited.first().map(|a| a.id.clone()).unwrap_or_default();
                // Largest cover for the backdrop + now-playing pane (the
                // small thumbs downsample from the same handle).
                let album_image_url = audio_item
                    .covers
                    .iter()
                    .max_by_key(|c| c.width)
                    .map(|c| c.url.clone());
                let prev = current.take();
                current = Some(CurrentlyPlaying {
                    track_id: audio_item.uri.clone(),
                    name: audio_item.name.clone(),
                    artist,
                    artist_id,
                    artists: credited,
                    album_image_url,
                    // Playing/Paused follows immediately with the truth.
                    is_playing: prev.as_ref().map(|p| p.is_playing).unwrap_or(false),
                    progress_ms: 0,
                    progress_anchor: Instant::now(),
                    duration_ms: audio_item.duration_ms as u64,
                    shuffle: prev.as_ref().map(|p| p.shuffle).unwrap_or(false),
                    repeat: prev.as_ref().map(|p| p.repeat).unwrap_or(RepeatMode::Off),
                    // Local PlayerEvents don't carry the context; keep any
                    // previously-known one so it isn't lost across tracks.
                    context_uri: prev.as_ref().and_then(|p| p.context_uri.clone()),
                    context_name: prev.as_ref().and_then(|p| p.context_name.clone()),
                });
                on_state(current.clone());
            }
            PlayerEvent::Playing { position_ms, .. } => {
                if let Some(p) = current.as_mut() {
                    p.is_playing = true;
                    p.progress_ms = position_ms as u64;
                    p.progress_anchor = Instant::now();
                    on_state(current.clone());
                }
            }
            PlayerEvent::Paused { position_ms, .. } => {
                if let Some(p) = current.as_mut() {
                    p.is_playing = false;
                    p.progress_ms = position_ms as u64;
                    p.progress_anchor = Instant::now();
                    on_state(current.clone());
                }
            }
            PlayerEvent::Seeked { position_ms, .. } => {
                if let Some(p) = current.as_mut() {
                    p.progress_ms = position_ms as u64;
                    p.progress_anchor = Instant::now();
                    on_state(current.clone());
                }
            }
            PlayerEvent::Stopped { .. } => {
                current = None;
                on_state(None);
            }
            // A track couldn't be loaded (region/availability or a transient
            // 0-byte fetch). Spirc auto-skips to the next track, but that
            // takes a moment; drop the chrome to not-playing now so it
            // doesn't keep showing the failed track as if it were playing
            // until the skip's `TrackChanged` lands. If nothing follows in
            // the queue, this stays as the honest "stopped on last track".
            PlayerEvent::Unavailable { track_id, .. } => {
                info!("track unavailable, Spirc will skip: {track_id}");
                if let Some(p) = current.as_mut() {
                    p.is_playing = false;
                    on_state(current.clone());
                }
            }
            PlayerEvent::ShuffleChanged { shuffle } => {
                if let Some(p) = current.as_mut() {
                    p.shuffle = shuffle;
                    on_state(current.clone());
                }
            }
            PlayerEvent::RepeatChanged { context, track } => {
                if let Some(p) = current.as_mut() {
                    p.repeat = if track {
                        RepeatMode::Track
                    } else if context {
                        RepeatMode::Context
                    } else {
                        RepeatMode::Off
                    };
                    on_state(current.clone());
                }
            }
            PlayerEvent::VolumeChanged { volume } => {
                on_volume(volume as f32 / u16::MAX as f32);
            }
            other => debug!("local player event (ignored): {other:?}"),
        }
    }
    debug!("local player event channel ended");
}
