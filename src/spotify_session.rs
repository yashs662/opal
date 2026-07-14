use std::path::PathBuf;

use librespot_core::cache::Cache;
use librespot_core::{Session, SessionConfig};

/// Ceiling for the streamed-audio cache. ~2.5 MB per track at 320 kbps
/// Vorbis ⇒ roughly 800 tracks. librespot evicts least-recently-played
/// (it touches mtimes on every read) once full.
const AUDIO_CACHE_MAX_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// Build an *un-connected* librespot Session. The actual `Session::connect`
/// is performed inside `Spirc::new` — calling it ourselves before Spirc
/// invalidates the AP socket the moment Spirc re-connects with its own
/// credentials (manifests as `Service unavailable { Session is not connected }`
/// at Spirc init). Mirrors `librespot/examples/play_connect.rs`.
///
/// We expose this as its own factory so `add_listen_for(...)` (which buffers
/// against the session's pre-connect builder) can be wired before Spirc
/// starts driving the dealer.
///
/// The session carries librespot's audio file cache (under our cache root,
/// see `disk_cache::audio_dir`): tracks are immutable, so replaying a song
/// reads it from disk instead of re-streaming the CDN. The right policy
/// for immutable content is the size-capped LRU librespot implements —
/// a TTL would only force pointless refetches.
pub fn new_session() -> Session {
    let cache = crate::disk_cache::audio_dir().and_then(|dir| {
        Cache::new(
            None::<PathBuf>,
            None,
            Some(dir),
            Some(AUDIO_CACHE_MAX_BYTES),
        )
        .inspect_err(|e| log::warn!("audio cache unavailable — streaming uncached: {e}"))
        .ok()
    });
    // Autoplay: when the current context/queue runs out, librespot resolves
    // a station of recommended tracks and keeps playing — matching the
    // official client instead of stopping dead ("no more tracks left").
    // `Some(true)` forces it on regardless of the account toggle.
    let config = SessionConfig {
        autoplay: Some(true),
        ..SessionConfig::default()
    };
    Session::new(config, cache)
}
