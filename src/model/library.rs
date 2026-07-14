//! Library slice — the Home feed data + playlist loading/caching.
//!
//! Owns the fetched [`HomeData`], the open centre-pane playlist (a live
//! streaming row buffer the worker pages fill), the playlist TTL cache,
//! and the in-flight gate. Playlists load **progressively**: a shell from
//! sidebar-known metadata appears immediately, the first page mounts the
//! virtualised list, and later pages stream into the shared buffer the
//! `lazy_list` reads on scroll — no blocking "loading all 989 songs".

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::rc::Rc;
use std::time::{Duration, Instant};

use opal_gfx::Signal;

use crate::album_art;
use crate::api::{AlbumRef, HomeData, PlaylistDetail, PlaylistTrack};
use crate::bounded::BoundedMap;
use crate::model::ArtModel;
use crate::views::home::playlist::{self, PlaylistRow, RowBuf};
use crate::worker::Worker;

/// How long a cached playlist stays fresh before a re-open re-fetches it.
/// Long enough to make back-and-forth navigation free, short enough that
/// edits made elsewhere show up within a few minutes.
const PLAYLIST_TTL: Duration = Duration::from_secs(300);

/// FIFO cap on the in-memory playlist-detail cache — a backstop against
/// unbounded growth over a long session, set well above any realistic
/// distinct-playlist count so it never evicts what the user actually
/// revisits. A re-open of an evicted (or TTL-stale) playlist re-fetches.
const PLAYLIST_CACHE_CAP: usize = 256;

/// A loaded playlist plus the wall-clock at which it was fetched — drives
/// the in-memory TTL cache so re-opening within [`PLAYLIST_TTL`] reuses
/// the data instead of re-hitting the Web API.
struct CachedPlaylist {
    detail: PlaylistDetail,
    fetched: Instant,
}

/// The playlist currently open in the centre pane. Holds the metadata
/// plus a **live, growable** row buffer the streaming worker pages fill —
/// the view's `lazy_list` reads it on scroll, so later pages appear
/// without a rebuild. `total` drives the list length from the first
/// response so the scrollbar is correct before everything has streamed.
pub struct OpenPlaylist {
    pub liked: bool,
    pub name: String,
    pub owner: String,
    pub image_url: Option<String>,
    pub context_uri: Option<String>,
    pub total: u32,
    pub rows: RowBuf,
    /// Metadata not yet arrived (header shows the sidebar-known name).
    pub loading: bool,
    /// Every page has streamed in.
    pub complete: bool,
}

/// The artist page open in the centre pane: profile + popular + library +
/// discography.
pub struct OpenArtist {
    pub name: String,
    pub image_url: Option<String>,
    pub followers: u64,
    pub top_tracks: Vec<PlaylistTrack>,
    pub albums: Vec<AlbumRef>,
    /// The user's saved songs by this artist across the whole library —
    /// Liked Songs + every membership-indexed playlist — each with the
    /// names of the sources containing it ("Liked Songs", "Chill"). The
    /// page's "In your library" section (capped there; the full list
    /// opens as a synthetic playlist page).
    pub library_tracks: Vec<(PlaylistTrack, Vec<String>)>,
    /// Profile/discography not yet arrived.
    pub loading: bool,
}

pub struct LibraryModel {
    /// The Home feed (greeting, recents, top artists, playlists, …).
    pub home: HomeData,
    /// The playlist (or album) open in the centre pane (live streaming buffer).
    /// The inner `rows` (`RowBuf`) is intentionally still `Rc<RefCell<…>>`:
    /// it's shared with the tree's `'static` lazy-list closure, which appends
    /// pages and reads on scroll independently of the model borrow.
    pub open_playlist: Option<OpenPlaylist>,
    /// The artist page open in the centre pane.
    pub open_artist: Option<OpenArtist>,
    /// Playlist detail TTL cache (id → detail + fetch time). Liked Songs
    /// lives here under `api::LIKED_SONGS_ID`. FIFO-capped (see
    /// [`PLAYLIST_CACHE_CAP`]) so a long browsing session can't grow it
    /// without bound; the cap is far above any session's distinct-playlist
    /// count, so it never evicts in normal use.
    playlist_cache: BoundedMap<String, CachedPlaylist>,
    /// Playlist ids with a fetch in flight — gate so navigating back and
    /// forth doesn't dispatch duplicate loads.
    playlist_inflight: HashSet<String>,
    /// Set by the reducer when a streamed page appended rows to the live
    /// buffer; consumed by `app::frame::tick`, which re-materializes the
    /// open detail page's lazy rows — otherwise rows the user already
    /// scrolled past (materialized as skeletons because the scroll outran
    /// the stream) would stay skeletons forever.
    pub rows_appended: bool,
    /// The active device's play queue (currently playing first), for the
    /// queue page. `None` = not loaded / loading; refetched on every
    /// open (live state, no cache).
    pub queue: Option<Vec<PlaylistTrack>>,
    /// Skeleton-row pulse opacity, ping-pong tweened while the open
    /// detail page is still streaming (driven by `app::frame::tick`).
    pub skeleton_pulse: Signal<f32>,
    /// Whether the pulse tween is currently running (mirror, so the frame
    /// tick can start/stop it on state edges instead of every frame).
    pub pulse_on: bool,
    /// Last time-of-day bucket the home greeting was built for (0/1/2 =
    /// morning/afternoon/evening; 255 = unset). The frame tick rebuilds when
    /// it changes so the greeting stays current even if the app idles on the
    /// feed across a boundary. A background timer wakes the loop at each one.
    pub greeting_bucket: Cell<u8>,
    /// Resolved Recents session contexts (uri → name/owner/cover). The
    /// Recents page shows a generic "Playlist"/count label until these land.
    pub recent_contexts: std::collections::HashMap<String, crate::api::RecentContextInfo>,
    /// Context uris we've already dispatched a resolve for — so re-opening
    /// Recents doesn't re-request (the fetch is disk-cached anyway).
    pub recent_contexts_requested: HashSet<String>,
    /// Recents session keys the user has expanded (survives rebuild).
    pub expanded_recents: HashSet<String>,
}

impl LibraryModel {
    pub fn new() -> Self {
        Self {
            home: HomeData::default(),
            open_playlist: None,
            open_artist: None,
            playlist_cache: BoundedMap::new(PLAYLIST_CACHE_CAP),
            playlist_inflight: HashSet::new(),
            rows_appended: false,
            queue: None,
            skeleton_pulse: Signal::new(1.0),
            pulse_on: false,
            greeting_bucket: Cell::new(u8::MAX),
            recent_contexts: std::collections::HashMap::new(),
            recent_contexts_requested: HashSet::new(),
            expanded_recents: HashSet::new(),
        }
    }

    // --- in-flight gate + TTL cache -----------------------------------

    pub fn is_inflight(&self, id: &str) -> bool {
        self.playlist_inflight.contains(id)
    }

    pub fn clear_inflight(&mut self, id: &str) {
        self.playlist_inflight.remove(id);
    }

    /// Cache a fully-loaded playlist for an instant re-open.
    pub fn cache(&mut self, detail: PlaylistDetail) {
        self.playlist_cache.insert(
            detail.id.clone(),
            CachedPlaylist {
                detail,
                fetched: Instant::now(),
            },
        );
    }

    /// A fresh (within TTL) cached detail clone, if any.
    fn cached_detail(&self, id: &str) -> Option<PlaylistDetail> {
        self.playlist_cache
            .get(id)
            .filter(|c| c.fetched.elapsed() < PLAYLIST_TTL)
            .map(|c| c.detail.clone())
    }

    // --- row baking ---------------------------------------------------

    /// Bake `tracks` into [`PlaylistRow`]s appended to `buf`. Each cover
    /// gets a reactive `Signal` off the shared art cache (so an arriving
    /// handle repaints just that thumb), but the **fetch is not dispatched
    /// here** — the cover downloads lazily when the row scrolls into view,
    /// so opening a 989-track playlist doesn't kick off 989 downloads.
    pub fn build_rows(
        &self,
        art: &mut ArtModel,
        buf: &RowBuf,
        tracks: &[PlaylistTrack],
        liked_page: bool,
        membership: &crate::model::membership::MembershipModel,
    ) {
        let mut out = buf.borrow_mut();
        out.reserve(tracks.len());
        for t in tracks {
            let cover = t
                .album_image_url
                .as_ref()
                .map(|u| art.or_signal(album_art::cache_key(u)));
            // Single saved-state source of truth — same check every heart uses.
            let in_library = liked_page || membership.is_saved(&t.uri);
            out.push(PlaylistRow {
                title: t.name.clone(),
                artist: t.artist.clone(),
                album: t.album.clone(),
                duration: playlist::fmt_duration(t.duration_ms),
                duration_ms: t.duration_ms,
                uri: t.uri.clone(),
                art: cover,
                cover_url: t.album_image_url.clone(),
                artists: t.artists.clone(),
                album_id: t.album_id.clone(),
                artist_id: t.artist_id.clone(),
                playable: t.playable,
                in_library,
            });
        }
    }

    // --- live membership edits ----------------------------------------

    /// Drop a playlist's in-memory cached detail so the next open re-fetches
    /// (the disk-cache pages are evicted worker-side). Pairs with the live
    /// patches below so an edit is reflected both now and on re-open.
    pub fn invalidate_cached(&mut self, id: &str) {
        self.playlist_cache.remove(id);
    }

    /// Whether the playlist `id` (or Liked Songs, when `liked`) is the page
    /// currently open in the centre pane.
    fn open_is(open: &OpenPlaylist, liked: bool, id: &str) -> bool {
        if liked {
            open.liked
        } else {
            open.context_uri.as_deref() == Some(format!("spotify:playlist:{id}").as_str())
        }
    }

    /// If the affected playlist (or Liked Songs) is open, append a row for
    /// `track` live — playlists gain it at the end, Liked Songs at the top
    /// (newest-first). No-op (returns false) when that page isn't open or the
    /// track is already present.
    pub fn open_add_track(
        &mut self,
        art: &mut ArtModel,
        liked: bool,
        id: &str,
        track: &PlaylistTrack,
        membership: &crate::model::membership::MembershipModel,
    ) -> bool {
        // Extract the shared row buffer (an `Rc` clone) under the
        let rows = {
            let Some(open) = self.open_playlist.as_mut() else {
                return false;
            };
            if !Self::open_is(open, liked, id)
                || open.rows.borrow().iter().any(|r| r.uri == track.uri)
            {
                return false;
            }
            open.total += 1;
            open.rows.clone()
        };
        self.build_rows(art, &rows, std::slice::from_ref(track), liked, membership);
        if liked {
            // Liked Songs lists most-recent first — move the just-appended
            rows.borrow_mut().rotate_right(1);
        }
        self.rows_appended = true;
        true
    }

    /// If the affected playlist (or Liked Songs) is open, drop every row for
    /// `uri` live. Returns whether anything was removed.
    pub fn open_remove_track(&mut self, liked: bool, id: &str, uri: &str) -> bool {
        let Some(open) = self.open_playlist.as_mut() else {
            return false;
        };
        if !Self::open_is(open, liked, id) {
            return false;
        }
        let before = open.rows.borrow().len();
        open.rows.borrow_mut().retain(|r| r.uri != uri);
        let removed = (before - open.rows.borrow().len()) as u32;
        if removed == 0 {
            return false;
        }
        open.total = open.total.saturating_sub(removed);
        self.rows_appended = true;
        true
    }

    // --- opening / loading --------------------------------------------

    /// Set up `open_playlist` for a nav target. A fresh in-memory cache
    /// hit populates the row buffer fully (instant). Otherwise a shell is
    /// built from the sidebar-known name/cover (header shows immediately)
    /// and a streaming fetch is dispatched.
    /// Open a synthetic, fully-local playlist page (e.g. "artist X in
    /// your library") — the whole playlist pipeline (view, scroll,
    /// playback) with no fetch and no cache; complete from birth. The
    /// caller routes to `MainNav::Playlist` with its own synthetic id
    /// (which scopes the page's scroll identity); no context uri ⇒ rows
    /// play via the uris-window fallback.
    pub fn open_synthetic(
        &mut self,
        art: &mut ArtModel,
        name: String,
        tracks: Vec<PlaylistTrack>,
        membership: &crate::model::membership::MembershipModel,
    ) {
        let buf: RowBuf = Rc::new(RefCell::new(Vec::new()));
        self.build_rows(art, &buf, &tracks, true, membership);
        self.open_artist = None;
        self.open_playlist = Some(OpenPlaylist {
            liked: false,
            name,
            owner: String::new(),
            image_url: None,
            context_uri: None,
            total: tracks.len() as u32,
            rows: buf,
            loading: false,
            complete: true,
        });
    }

    pub fn open_for(
        &mut self,
        art: &mut ArtModel,
        worker: &Worker,
        token: Option<String>,
        id: &str,
        liked: bool,
        membership: &crate::model::membership::MembershipModel,
    ) {
        if let Some(detail) = self.cached_detail(id) {
            let buf: RowBuf = Rc::new(RefCell::new(Vec::new()));
            self.build_rows(art, &buf, &detail.tracks, liked, membership);
            self.open_playlist = Some(OpenPlaylist {
                liked,
                name: detail.name,
                owner: detail.owner,
                image_url: detail.image_url,
                context_uri: detail.context_uri,
                total: detail.total,
                rows: buf,
                loading: false,
                complete: true,
            });
            return;
        }

        // Shell from whatever the sidebar already knows, so the header
        // isn't blank while metadata + the first page stream in.
        let (name, image_url) = if liked {
            ("Liked Songs".to_string(), None)
        } else {
            self.home
                .playlists
                .iter()
                .find(|p| p.id == id)
                .map(|p| (p.name.clone(), p.image_url.clone()))
                .unwrap_or((String::new(), None))
        };
        let context_uri = if liked {
            None
        } else {
            Some(format!("spotify:playlist:{id}"))
        };
        let buf: RowBuf = Rc::new(RefCell::new(Vec::new()));
        self.open_playlist = Some(OpenPlaylist {
            liked,
            name,
            owner: String::new(),
            image_url,
            context_uri,
            total: 0,
            rows: buf,
            loading: true,
            complete: false,
        });
        self.ensure_loaded(worker, token, id, liked);
    }

    /// Open an album in the centre pane. Albums reuse [`OpenPlaylist`] (they
    /// have a `context_uri` + a track list); a fresh cache hit populates the
    /// buffer instantly, otherwise a blank shell shows while the single-shot
    /// album fetch lands. Mirrors [`Self::open_for`].
    pub fn open_album(
        &mut self,
        art: &mut ArtModel,
        worker: &Worker,
        token: Option<String>,
        id: &str,
        membership: &crate::model::membership::MembershipModel,
    ) {
        if let Some(detail) = self.cached_detail(id) {
            let buf: RowBuf = Rc::new(RefCell::new(Vec::new()));
            self.build_rows(art, &buf, &detail.tracks, false, membership);
            self.open_playlist = Some(OpenPlaylist {
                liked: false,
                name: detail.name,
                owner: detail.owner,
                image_url: detail.image_url,
                context_uri: detail.context_uri,
                total: detail.total,
                rows: buf,
                loading: false,
                complete: true,
            });
            return;
        }
        let buf: RowBuf = Rc::new(RefCell::new(Vec::new()));
        self.open_playlist = Some(OpenPlaylist {
            liked: false,
            name: String::new(),
            owner: String::new(),
            image_url: None,
            context_uri: Some(format!("spotify:album:{id}")),
            total: 0,
            rows: buf,
            loading: true,
            complete: false,
        });
        self.ensure_loaded_album(worker, token, id);
    }

    /// Open an artist page. Sets a loading shell + dispatches the profile +
    /// discography fetch; the result lands via `ArtistOpened`.
    pub fn open_artist(&mut self, worker: &Worker, token: Option<String>, id: &str) {
        self.open_artist = Some(OpenArtist {
            name: String::new(),
            image_url: None,
            followers: 0,
            top_tracks: Vec::new(),
            albums: Vec::new(),
            library_tracks: Vec::new(),
            loading: true,
        });
        if self.is_inflight(id) {
            return;
        }
        let Some(token) = token else {
            log::warn!("artist load skipped — no auth token");
            return;
        };
        self.playlist_inflight.insert(id.to_string());
        worker.fetch_artist(token, id.to_string());
    }

    /// Dispatch a one-shot album fetch unless one is already in flight.
    pub fn ensure_loaded_album(&mut self, worker: &Worker, token: Option<String>, id: &str) {
        if self.is_inflight(id) {
            return;
        }
        let Some(token) = token else {
            log::warn!("album load skipped — no auth token");
            return;
        };
        self.playlist_inflight.insert(id.to_string());
        worker.fetch_album(token, id.to_string());
    }

    /// Dispatch a streaming playlist fetch unless a load is already in
    /// flight. Liked Songs routes through the same path under its sentinel
    /// id. `token` is the live access token (read at call time).
    pub fn ensure_loaded(&mut self, worker: &Worker, token: Option<String>, id: &str, liked: bool) {
        if self.is_inflight(id) {
            return;
        }
        let Some(token) = token else {
            log::warn!("playlist load skipped — no auth token");
            return;
        };
        self.playlist_inflight.insert(id.to_string());
        worker.fetch_playlist(token, id.to_string(), liked);
    }
}

impl Default for LibraryModel {
    fn default() -> Self {
        Self::new()
    }
}
