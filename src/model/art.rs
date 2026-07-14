//! Art-resolution cache slice.
//!
//! The shared per-URL cover-handle map (bound by home tiles, playlist
//! rows, and player art), the in-flight dedup gate, the cover→accent
//! cache, the "currently shown" key, and the `/v1/tracks/{id}` detail
//! cache. Resolutions push handles into the reactive signals so an art
//! arrival repaints just the affected nodes — no scene rebuild.

use std::collections::{HashMap, HashSet};

use opal_gfx::{ImageHandle, Signal};

use crate::album_art;
use crate::api::{HomeData, TrackDetails};
use crate::bounded::BoundedMap;
use crate::worker::Worker;

/// FIFO cap for the per-cover accent + `/tracks/{id}` detail caches. Sized
/// far above any realistic session's unique-track count (≈ a 200-hour
/// listen), so it never evicts in normal use — it's only a backstop against
/// unbounded growth. Eviction (oldest cover/track first) never touches what's
/// on screen, and re-resolves hit the on-disk cache, so UX is unaffected.
const ART_CACHE_CAP: usize = 4096;

pub struct ArtModel {
    /// Per-URL (cache_key) reactive cover handle for every cover shown
    /// anywhere — tiles/rows/player bind their image to it, so an art
    /// arrival repaints just those nodes. `None` until resolved. The view
    /// looks these up narrowly via [`Self::signal`] / [`Self::or_signal`] —
    /// it must NOT hold a long-lived `borrow()` of this map across a build,
    /// or an interleaved `or_signal` (`borrow_mut`) double-borrows at runtime.
    pub home_art: HashMap<String, Signal<Option<ImageHandle>>>,
    /// cache_keys with a fetch in flight — gate so a cover doesn't get a
    /// second fetch while the first resolves.
    inflight: HashSet<String>,
    /// cache_key of the cover currently promoted into the backdrop, so
    /// repeated PlayerState pushes for the same track don't re-promote.
    shown_key: Option<String>,
    /// Spotify's own extracted accent per cover (authoritative over the
    /// pixel-average), kept so a late art/accent arrival promotes the
    /// right colour regardless of which request resolves first.
    accents: BoundedMap<String, [f32; 4]>,
    /// `/v1/tracks/{id}` results keyed by bare track ID — the cluster
    /// carries only `artist_uri`, so the resolved artist name comes from
    /// here.
    track_details: BoundedMap<String, TrackDetails>,
}

impl ArtModel {
    pub fn new() -> Self {
        Self {
            home_art: HashMap::new(),
            inflight: HashSet::new(),
            shown_key: None,
            accents: BoundedMap::new(ART_CACHE_CAP),
            track_details: BoundedMap::new(ART_CACHE_CAP),
        }
    }

    // --- reactive cover handles ---------------------------------------

    /// Existing-or-fresh reactive handle for `key` (creates `None` on
    /// miss). Rows/tiles bind to the returned signal.
    pub fn or_signal(&mut self, key: String) -> Signal<Option<ImageHandle>> {
        self.home_art
            .entry(key)
            .or_insert_with(|| Signal::new(None))
            .clone()
    }

    /// Read-only lookup of an existing handle signal.
    pub fn signal(&self, key: &str) -> Option<Signal<Option<ImageHandle>>> {
        self.home_art.get(key).cloned()
    }

    /// Push a resolved handle into the matching signal (repaints bound
    /// nodes, no rebuild). No-op if nothing bound to `key`.
    pub fn set_resolved(&self, key: &str, handle: ImageHandle) {
        if let Some(sig) = self.home_art.get(key) {
            sig.set(Some(handle));
        }
    }

    // --- in-flight gate -----------------------------------------------

    pub fn is_inflight(&self, key: &str) -> bool {
        self.inflight.contains(key)
    }

    pub fn mark_inflight(&mut self, key: String) {
        self.inflight.insert(key);
    }

    pub fn clear_inflight(&mut self, key: &str) {
        self.inflight.remove(key);
    }

    // --- shown key + accent cache -------------------------------------

    pub fn is_shown(&self, key: &str) -> bool {
        self.shown_key.as_deref() == Some(key)
    }

    pub fn set_shown(&mut self, key: String) {
        self.shown_key = Some(key);
    }

    pub fn has_accent(&self, key: &str) -> bool {
        self.accents.contains_key(key)
    }

    pub fn cache_accent(&mut self, key: String, accent: [f32; 4]) {
        self.accents.insert(key, accent);
    }

    /// Spotify's cached accent for `key`, if it arrived already.
    pub fn accent(&self, key: &str) -> Option<[f32; 4]> {
        self.accents.get(key).copied()
    }

    // --- track details ------------------------------------------------

    pub fn insert_track_detail(&mut self, details: TrackDetails) {
        self.track_details.insert(details.track_id.clone(), details);
    }

    /// Full cached `/v1/tracks/{id}` detail (name + artist + cover), if
    /// resolved — patches sparse cluster updates that arrive without
    /// title metadata (e.g. `DEVICES_DISAPPEARED` pushes).
    pub fn track_detail(&self, track_id: &str) -> Option<TrackDetails> {
        self.track_details.get(track_id).cloned()
    }

    // --- fetch dispatch -----------------------------------------------

    /// Lazily fetch a track cover (called when a row materializes). Gated:
    /// already-resolved / in-flight covers are no-ops.
    pub fn dispatch_cover(&mut self, worker: &Worker, url: String) {
        let key = album_art::cache_key(&url);
        if let Some(sig) = self.signal(&key)
            && sig.get().is_some()
        {
            return;
        }
        if self.is_inflight(&key) {
            return;
        }
        self.mark_inflight(key.clone());
        worker.fetch_album_art(url, key);
    }

    /// Re-hydrate the backdrop cover from a persisted URL on cold start:
    /// fetch the art (disk-cache hit → near-instant) plus Spotify's
    /// extracted accent, so the launch backdrop is already populated +
    /// correctly tinted before the first live cluster push (instead of the
    /// washed-out pixel-average until the next track change).
    pub fn rehydrate_cover(&mut self, url: &str, worker: &Worker) {
        let key = album_art::cache_key(url);
        self.mark_inflight(key.clone());
        worker.fetch_album_art(url.to_string(), key.clone());
        worker.fetch_accent(key);
    }

    /// Ensure a reactive handle exists per cover URL in `data`, and
    /// dispatch a fetch for each key that's neither in flight nor already
    /// resolved. Later `AlbumArtReady` arrivals fill the signals.
    pub fn prefetch(&mut self, worker: &Worker, data: &HomeData) {
        let (pl, pl_with) = count_with_image(&data.playlists, |p| p.image_url.is_some());
        let (rc, rc_with) = count_with_image(&data.recent, |t| t.album_image_url.is_some());
        let (ta, ta_with) = count_with_image(&data.top_artists, |a| a.image_url.is_some());
        let (tt, tt_with) = count_with_image(&data.top_tracks, |t| t.album_image_url.is_some());
        log::info!(
            "home art coverage: playlists {pl_with}/{pl}, recent {rc_with}/{rc}, \
             top_artists {ta_with}/{ta}, top_tracks {tt_with}/{tt}, \
             latest_release {}",
            if data
                .latest_release
                .as_ref()
                .and_then(|a| a.image_url.as_ref())
                .is_some()
            {
                "1/1"
            } else {
                "0/1"
            },
        );

        let urls = data
            .playlists
            .iter()
            .filter_map(|p| p.image_url.as_ref())
            // Sidebar library icons fetch the tiny (64 px) tier separately —
            // distinct scdn key from the full-res home tile, so both load.
            .chain(
                data.playlists
                    .iter()
                    .filter_map(|p| p.image_url_small.as_ref()),
            )
            .chain(
                data.recent
                    .iter()
                    .filter_map(|t| t.album_image_url.as_ref()),
            )
            .chain(data.top_artists.iter().filter_map(|a| a.image_url.as_ref()))
            .chain(
                data.top_tracks
                    .iter()
                    .filter_map(|t| t.album_image_url.as_ref()),
            )
            .chain(
                data.latest_release
                    .iter()
                    .filter_map(|a| a.image_url.as_ref()),
            );
        let mut dispatched = 0_usize;
        for url in urls {
            let key = album_art::cache_key(url);
            let sig = self
                .home_art
                .entry(key.clone())
                .or_insert_with(|| Signal::new(None))
                .clone();
            if sig.get().is_some() || self.inflight.contains(&key) {
                continue;
            }
            self.inflight.insert(key.clone());
            worker.fetch_album_art(url.clone(), key);
            dispatched += 1;
        }
        log::info!("dispatched {dispatched} new art fetches");
    }
}

impl Default for ArtModel {
    fn default() -> Self {
        Self::new()
    }
}

fn count_with_image<T>(items: &[T], has: impl Fn(&T) -> bool) -> (usize, usize) {
    (items.len(), items.iter().filter(|i| has(i)).count())
}
