use std::time::{Instant, SystemTime, UNIX_EPOCH};

use futures::StreamExt;
use librespot_core::dealer::Subscription;
use librespot_core::dealer::protocol::PayloadValue;
use librespot_protocol::connect::ClusterUpdate;
use librespot_protocol::connect::ClusterUpdateReason;
use librespot_protocol::player::PlayerState as ProtoPlayerState;
use librespot_protocol::player::ProvidedTrack;
use log::{debug, info, warn};
use protobuf::Message as _;

use crate::api::{CurrentlyPlaying, PlaylistTrack, RepeatMode, TrackArtist};

/// Drain the dealer `hm://connect-state/v1/cluster` subscription forever,
/// emitting our domain `CurrentlyPlaying` (plus the active device's
/// volume as a 0..=1 fraction, when reported) for each cluster update.
///
/// Spotify's cluster carries the *globally-active* playback — the same
/// state the official app shows regardless of which device is the
/// audio output. So even if Opal isn't the active device, we still
/// reflect what the user's phone / web player is doing. (Our OWN
/// playback never arrives here — the dealer doesn't echo a device's
/// state back to itself; that path is `local_player`.)
///
/// The 4th callback arg is the active device's *full* play queue (the
/// playing track followed by `next_tracks`), emitted only when Spotify's
/// `queue_revision` changes — this is the uncapped, live alternative to the
/// ~20-entry `/me/player/queue` Web API. `None` means "queue unchanged
/// since the last push" (skip the rebuild).
///
/// The 5th arg (`vanished`) is `true` only on the push where the
/// *globally-active* device dropped off the cluster (`DEVICES_DISAPPEARED`
/// leaving no active device) — e.g. the phone/web player that was driving
/// playback was quit mid-track. Spotify's player_state on that push is
/// bogus (still "playing" at position 0), so we suppress it (`player =
/// None`) and let the host take over on Opal instead of showing a frozen,
/// dead transport.
pub async fn run<F>(mut sub: Subscription, mut on_update: F)
where
    F: FnMut(
        Option<CurrentlyPlaying>,
        Option<f32>,
        Option<String>,
        Option<Vec<PlaylistTrack>>,
        bool,
    ),
{
    info!("cluster listener started — awaiting connect-state pushes");
    // Hash of the last emitted queue (revision + track uris) — see the
    // signature dedup below.
    let mut last_queue_sig: Option<u64> = None;
    while let Some(msg) = sub.next().await {
        let bytes = match msg.payload {
            PayloadValue::Raw(b) => b,
            PayloadValue::Empty => {
                debug!("cluster msg with empty payload — skipping");
                continue;
            }
            PayloadValue::Json(j) => {
                debug!("cluster msg unexpectedly JSON-encoded: {j}");
                continue;
            }
        };
        let update = match ClusterUpdate::parse_from_bytes(&bytes) {
            Ok(u) => u,
            Err(e) => {
                warn!("failed to parse ClusterUpdate protobuf: {e}");
                continue;
            }
        };
        info!(
            "cluster update: reason={:?} ack={} devices_changed={:?}",
            update.update_reason, update.ack_id, update.devices_that_changed
        );
        let devices_disappeared =
            update.update_reason.enum_value() == Ok(ClusterUpdateReason::DEVICES_DISAPPEARED);
        let Some(cluster) = update.cluster.into_option() else {
            // No cluster at all after a device dropped off ⇒ the active
            // player is gone. Signal the takeover so the host can claim
            // playback on Opal.
            info!("  cluster: <empty>");
            on_update(None, None, None, None, devices_disappeared);
            continue;
        };
        info!(
            "  cluster: active_device={} devices={:?}",
            cluster.active_device_id,
            cluster.device.keys().collect::<Vec<_>>()
        );
        // The active device's reported volume (0..=65535 → fraction).
        let volume = cluster
            .device
            .get(&cluster.active_device_id)
            .map(|d| (d.volume as f32 / u16::MAX as f32).clamp(0.0, 1.0));
        let active_device =
            (!cluster.active_device_id.is_empty()).then(|| cluster.active_device_id.clone());
        // The active device just dropped off the cluster (its app quit) and
        // no other device took over. Spotify still ships a stale "playing"
        // player_state here (frozen at position 0), which would leave Opal
        // showing a dead, unresponsive transport. Suppress it and flag the
        // takeover instead — the host resumes on Opal at the last position.
        if devices_disappeared && active_device.is_none() {
            info!("  active device vanished — signalling takeover");
            on_update(None, volume, None, None, true);
            continue;
        }
        let Some(state) = cluster.player_state.into_option() else {
            info!("  player_state: <empty>");
            on_update(None, volume, active_device, None, false);
            continue;
        };
        if !state.track.metadata.is_empty() {
            info!(
                "  track metadata keys: {:?}",
                state.track.metadata.keys().collect::<Vec<_>>()
            );
        }
        // Build the full queue (playing track + next_tracks) only when the
        // *content* changed. `queue_revision` alone isn't enough: Spotify
        // bumps it for explicit queue edits, but the context/autoplay
        // continuation refilling `next_tracks` right after an edit rides
        // later pushes under the SAME revision — keying on the revision
        // left the queue page showing just the added track. Hash the
        // uris AND the display metadata: entries often arrive as bare
        // uris first and hydrate (title/duration) on a later push with
        // identical uris — a uri-only signature deduped the hydration
        // away, freezing the page on blank rows. Unrelated pushes
        // (progress ticks, volume) still dedup.
        let signature = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            state.queue_revision.hash(&mut h);
            if let Some(now) = state.track.as_ref() {
                now.uri.hash(&mut h);
            }
            for t in &state.next_tracks {
                t.uri.hash(&mut h);
                t.metadata.get("title").hash(&mut h);
                t.metadata.get("duration").hash(&mut h);
            }
            h.finish()
        };
        let queue = if last_queue_sig != Some(signature) {
            last_queue_sig = Some(signature);
            let mut q = Vec::with_capacity(1 + state.next_tracks.len());
            if let Some(now) = state.track.as_ref() {
                q.push(provided_to_track(now));
            }
            q.extend(state.next_tracks.iter().map(provided_to_track));
            info!("  -> cluster queue: {} tracks (content changed)", q.len());
            Some(q)
        } else {
            None
        };

        let cp = into_currently_playing(state);
        info!(
            "  -> CurrentlyPlaying: name='{}' artist='{}' playing={} progress={}/{} img={:?}",
            cp.name, cp.artist, cp.is_playing, cp.progress_ms, cp.duration_ms, cp.album_image_url
        );
        on_update(Some(cp), volume, active_device, queue, false);
    }
    debug!("cluster subscription stream ended");
}

/// Map a connect-state `ProvidedTrack` (a queue entry) to our domain
/// [`PlaylistTrack`]. Mirrors the metadata-key reading in
/// [`into_currently_playing`]; the artist *name* is in the metadata but the
/// artist *id* only as `artist_uri` (so the clickable line resolves the
/// first artist; later artists get a name but no id until detail-fetched).
fn provided_to_track(pt: &ProvidedTrack) -> PlaylistTrack {
    let md = &pt.metadata;
    let name = md.get("title").cloned().unwrap_or_default();
    let artist_id = id_from_uri(&pt.artist_uri);
    let album_id = id_from_uri(&pt.album_uri);

    let artists = artists_from_metadata(md, &artist_id);
    let artist = artists
        .iter()
        .map(|a| a.name.as_str())
        .collect::<Vec<_>>()
        .join(", ");

    let album_image_url = md
        .get("image_xlarge_url")
        .or_else(|| md.get("image_large_url"))
        .or_else(|| md.get("image_url"))
        .or_else(|| md.get("image_small_url"))
        .map(|s| spotify_image_uri_to_https(s));
    let duration_ms = md.get("duration").and_then(|d| d.parse().ok()).unwrap_or(0);
    let id = crate::api::track_id_from_uri(&pt.uri)
        .unwrap_or_default()
        .to_string();

    PlaylistTrack {
        id,
        uri: pt.uri.clone(),
        name,
        artist,
        album: String::new(),
        album_image_url,
        duration_ms,
        artists,
        album_id,
        artist_id,
        playable: true,
    }
}

/// Last colon-separated segment of a `spotify:kind:ID` uri (the bare id).
/// Empty in → empty out.
fn id_from_uri(uri: &str) -> String {
    if uri.is_empty() {
        String::new()
    } else {
        uri.rsplit(':').next().unwrap_or_default().to_string()
    }
}

/// Ordered artist credits from connect-state track metadata:
/// `artist_name` (single) or `artist_name:0..n` (multi). Only the first
/// carries an id (from `artist_uri` — the metadata has no per-artist
/// uris; the rest resolve when the track-details backfill lands).
fn artists_from_metadata(
    md: &std::collections::HashMap<String, String>,
    first_artist_id: &str,
) -> Vec<TrackArtist> {
    let mut artists: Vec<TrackArtist> = Vec::new();
    if let Some(a) = md.get("artist_name") {
        artists.push(TrackArtist {
            id: first_artist_id.to_string(),
            name: a.clone(),
        });
    } else {
        let mut i = 0;
        while let Some(a) = md.get(&format!("artist_name:{i}")) {
            let id = if i == 0 {
                first_artist_id.to_string()
            } else {
                String::new()
            };
            artists.push(TrackArtist {
                id,
                name: a.clone(),
            });
            i += 1;
        }
    }
    artists
}

fn into_currently_playing(state: ProtoPlayerState) -> CurrentlyPlaying {
    // The context's display name ("Chill", "Daily Mix 2", "<song> Radio")
    // rides the state's context metadata — exactly what the official
    // client shows as the queue source.
    let context_name = state
        .context_metadata
        .get("context_description")
        .filter(|d| !d.is_empty())
        .cloned();
    let track = state.track.unwrap_or_default();
    let md = &track.metadata;
    let artist_id = id_from_uri(&track.artist_uri);
    let name = md.get("title").cloned().unwrap_or_default();
    let artists = artists_from_metadata(md, &artist_id);
    let artist = artists
        .iter()
        .map(|a| a.name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    // Prefer the largest variant Spotify offers (xlarge/large ≈ 640px)
    // so the now-playing pane + full-window backdrop render crisp. The
    // 56px player-bar thumb bilinear-downsamples from the same handle.
    // Medium (`image_url` ≈ 300px) and small are last-resort fallbacks.
    let album_image_url = md
        .get("image_xlarge_url")
        .or_else(|| md.get("image_large_url"))
        .or_else(|| md.get("image_url"))
        .or_else(|| md.get("image_small_url"))
        .map(|s| spotify_image_uri_to_https(s));

    let is_playing = !state.is_paused && state.is_playing;
    // `position_as_of_timestamp` is the position sampled at Spotify's server
    // `timestamp` (Unix ms) — NOT now. While playing, the real position has
    // advanced by (now - timestamp) since then. Spotify only re-samples the
    // timestamp on a state transition (play/pause/seek/track), so an
    // unrelated cluster re-broadcast — e.g. a new device merely *joining* the
    // session with playback already running on a third device — ships the
    // stale transition-time pair. Anchoring that raw position to
    // `Instant::now()` (as we used to) threw away the elapsed gap, snapping
    // the bar back and making the song appear to restart. Fold the gap in
    // here so `progress_anchor = now` stays consistent. (Matches librespot's
    // own `update_position_in_relation`: position += now - timestamp.)
    let base = state.position_as_of_timestamp.max(0) as u64;
    let progress_ms = if is_playing && state.timestamp > 0 {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let elapsed = (now_ms - state.timestamp).max(0) as u64;
        base + elapsed
    } else {
        base
    };
    let progress_anchor = Instant::now();
    let duration_ms = state.duration.max(0) as u64;

    // Repeat enum on PlayerState comes through as `repeating_context`
    // / `repeating_track` flags — Spotify doesn't model the three-state
    // explicitly here.
    let opts = state.options.unwrap_or_default();
    let repeat = if opts.repeating_track {
        RepeatMode::Track
    } else if opts.repeating_context {
        RepeatMode::Context
    } else {
        RepeatMode::Off
    };
    let shuffle = opts.shuffling_context;

    let track_id = track.uri.clone();
    let context_uri = (!state.context_uri.is_empty()).then(|| state.context_uri.clone());

    CurrentlyPlaying {
        track_id,
        name,
        artist,
        artist_id,
        artists,
        album_image_url,
        is_playing,
        progress_ms,
        progress_anchor,
        duration_ms,
        shuffle,
        repeat,
        context_uri,
        context_name,
    }
}

/// `spotify:image:HEX` → `https://i.scdn.co/image/HEX`. Pass through any
/// other shape (already https, or unknown) untouched.
fn spotify_image_uri_to_https(uri: &str) -> String {
    if let Some(hex) = uri.strip_prefix("spotify:image:") {
        format!("https://i.scdn.co/image/{hex}")
    } else {
        uri.to_string()
    }
}
