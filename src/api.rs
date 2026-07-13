//! Spotify Web API bindings + the app's domain structs parsed from them.
//!
//! `dead_code` is allowed module-wide on purpose: this is a data-binding
//! layer that captures the full shape of each entity (ids, totals, avatar
//! URLs, …) even where the UI doesn't consume every field *yet*. Those
//! fields are wired up as features land (clickable tiles need `id`, the
//! profile chip needs `avatar_url`, etc.) — they are scaffolding, not rot.
#![allow(dead_code)]

use std::hash::{Hash, Hasher};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::disk_cache;
use crate::errors::AuthError;

const API: &str = "https://api.spotify.com/v1";

/// Cache TTLs per endpoint class — the single knob for "how long is a
/// cached Web API response good for". Every [`get_json`] caller picks one;
/// adding an endpoint is a one-line choice here, and the response is then
/// cached on disk automatically (see [`get_json`]). Use [`ttl::NONE`] to
/// bypass the cache for live/volatile reads.
///
/// Cache entries are keyed by URL only (query params included), not by the
/// authenticated user — fine for this single-account app; sign-out clears
/// the cache via the settings "Clear cache" button.
pub mod ttl {
    use std::time::Duration;
    const HOUR: u64 = 3600;
    const DAY: u64 = 24 * HOUR;
    /// Immutable resources keyed by id (track + album metadata): never
    /// change, so the long bound just caps growth.
    pub const IMMUTABLE: Duration = Duration::from_secs(30 * DAY);
    /// Slowly-changing user data: profile, top artists/tracks (recomputed
    /// ~weekly), artist discography (catch new releases within a day).
    pub const SLOW: Duration = Duration::from_secs(DAY);
    /// User-editable collections: playlist list + metadata + track pages.
    pub const MUTABLE: Duration = Duration::from_secs(6 * HOUR);
    /// Volatile feeds: recently-played.
    pub const VOLATILE: Duration = Duration::from_secs(10 * 60);
    /// Bypass the cache entirely (live player state).
    pub const NONE: Duration = Duration::ZERO;
}

/// Disk-cache key for a GET URL: a stable hash of the full URL (query
/// params included, so each distinct request gets its own entry). The
/// `DefaultHasher` seed is fixed, so the key is stable across runs. Bearer
/// token lives in a header, not the URL, so token refresh never busts it.
fn url_key(url: &str) -> String {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    url.hash(&mut h);
    format!("api_{:016x}", h.finish())
}

#[derive(Debug, Clone)]
pub struct Profile {
    /// Spotify user id — owner-match for "which playlists can I edit".
    pub id: String,
    pub display_name: String,
    pub avatar_url: Option<String>,
    /// ISO 3166-1 alpha-2 country (from `user-read-private`). Used as the
    /// `market` for endpoints that require one (artist top-tracks).
    pub country: String,
}

#[derive(Debug, Clone)]
pub struct PlaylistRef {
    pub id: String,
    pub name: String,
    /// Full-res (640 px) cover — the "Made For You" home tile.
    pub image_url: Option<String>,
    /// Tiny (64 px) cover — the sidebar library icon. Same album, smaller
    /// scdn tier (sidebar rows are ~48 logical px); kept separate so the
    /// two consumers don't share one over- or under-sized fetch.
    pub image_url_small: Option<String>,
}

#[derive(Debug, Clone)]
pub struct RecentTrack {
    pub id: String,
    pub name: String,
    pub artist: String,
    /// Album id — the tile opens this album's detail page.
    pub album_id: String,
    pub album_image_url: Option<String>,
    /// ISO-8601 `played_at` timestamp (`YYYY-MM-DDT…Z`); the leading date
    /// drives the "Today/Yesterday/…" grouping on the Show-all page.
    pub played_at: String,
}

#[derive(Debug, Clone)]
pub struct ArtistRef {
    pub id: String,
    pub name: String,
    pub image_url: Option<String>,
}

#[derive(Debug, Clone)]
pub struct TrackRef {
    pub id: String,
    pub name: String,
    pub artist: String,
    /// Album id — the tile opens this album's detail page.
    pub album_id: String,
    pub album_image_url: Option<String>,
}

#[derive(Debug, Clone)]
pub struct AlbumRef {
    pub id: String,
    pub name: String,
    pub artist: String,
    pub image_url: Option<String>,
    /// `YYYY-MM-DD`, `YYYY-MM`, or `YYYY` — Spotify's precision varies.
    pub release_date: String,
}

/// A single track inside a playlist (or the Liked Songs collection).
/// `uri` is the `spotify:track:…` form Web API playback needs; `id` is
/// the bare hex (album-art cache keys etc.).
/// One artist credited on a track — id + name, for showing every artist
/// and making each a clickable link to its artist page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrackArtist {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PlaylistTrack {
    pub id: String,
    pub uri: String,
    pub name: String,
    /// All credited artists' names joined with ", " — the plain-string
    /// display fallback. The clickable per-artist rendering uses
    /// [`Self::artists`].
    pub artist: String,
    pub album: String,
    pub album_image_url: Option<String>,
    pub duration_ms: u64,
    /// Every credited artist (id + name), in order, for the multi-artist
    /// clickable line. Empty on older cache entries → fall back to the
    /// joined `artist` string.
    #[serde(default)]
    pub artists: Vec<TrackArtist>,
    /// Album id + first-artist id for the right-click "Go to album/artist"
    /// menu actions. Empty when the source didn't carry them (older cache
    /// entries, local files).
    #[serde(default)]
    pub album_id: String,
    #[serde(default)]
    pub artist_id: String,
    /// False for tracks the Web API can't start: local files on another
    /// device and region-unavailable tracks (`is_playable: false` under
    /// `market=from_token`). Shown faded + non-interactive. Defaults
    /// true so typed cache entries from before the field existed don't
    /// render everything disabled.
    #[serde(default = "default_true")]
    pub playable: bool,
}

fn default_true() -> bool {
    true
}

/// A fully-loaded playlist (metadata + first page of tracks). Liked
/// Songs is modelled as one of these with a synthetic name/owner and
/// `context_uri = None` (it has no playable context URI on the Web API,
/// so playback falls back to an explicit `uris` list — see [`PlayTarget`]).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaylistDetail {
    pub id: String,
    pub name: String,
    pub owner: String,
    pub image_url: Option<String>,
    /// `spotify:playlist:…` for real playlists; `None` for Liked Songs.
    pub context_uri: Option<String>,
    pub tracks: Vec<PlaylistTrack>,
    /// Total tracks reported by Spotify (may exceed `tracks.len()` since
    /// we only load the first page).
    pub total: u32,
}

#[derive(Debug, Clone, Default)]
pub struct HomeData {
    pub profile: Option<Profile>,
    pub playlists: Vec<PlaylistRef>,
    pub recent: Vec<RecentTrack>,
    pub top_artists: Vec<ArtistRef>,
    pub top_tracks: Vec<TrackRef>,
    /// Newest album from the user's #1 top artist — our "New release"
    /// stand-in. `/v1/browse/new-releases` got deprecated for new apps
    /// in Nov 2024 alongside featured-playlists + recommendations.
    pub latest_release: Option<AlbumRef>,
}

#[derive(Debug, Clone)]
pub struct CurrentlyPlaying {
    pub track_id: String,
    pub name: String,
    pub artist: String,
    pub album_image_url: Option<String>,
    pub is_playing: bool,
    /// Position at the moment `progress_anchor` was sampled. Cluster
    /// updates push only on state transitions (play/pause/seek/track),
    /// not on a tick — so this is a snapshot, not a live position.
    /// Call `live_progress_ms` for an interpolated value.
    pub progress_ms: u64,
    /// Local wall-clock at the time the anchor was captured. Used to
    /// interpolate progress between cluster pushes.
    pub progress_anchor: Instant,
    pub duration_ms: u64,
    pub shuffle: bool,
    pub repeat: RepeatMode,
    /// The playing context (`spotify:album:…` / `spotify:playlist:…`), if any.
    /// Persisted so a cold-start resume can restart *within* the context and
    /// keep playing past the one track (librespot autoplay needs a context).
    pub context_uri: Option<String>,
    /// Display name of the playing context ("Chill", "Daily Mix 2",
    /// "<song> Radio", …) — the dealer cluster ships it as
    /// `context_metadata["context_description"]`. `None` when the source
    /// didn't carry one (Web API seed, local events); the UI then falls
    /// back to the context *kind*.
    pub context_name: Option<String>,
    /// Bare id of the track's first artist (cluster `artist_uri` / Web
    /// API item) — drives the now-playing "About the artist" fetch.
    /// Empty when the source didn't carry it.
    pub artist_id: String,
    /// Every credited artist (id + name), in order — the clickable
    /// per-artist lines in the player bar + now-playing card. Empty when
    /// the source was sparse (the track-details backfill fills it, like
    /// `artist_id`); display falls back to the joined `artist` string.
    pub artists: Vec<TrackArtist>,
}

impl CurrentlyPlaying {
    /// Position right now, interpolated from `progress_ms + (now - anchor)`
    /// while playing. Clamps to `duration_ms`. Mirrors how the official
    /// Spotify client ticks its progress bar between server pushes.
    pub fn live_progress_ms(&self) -> u64 {
        if !self.is_playing {
            return self.progress_ms.min(self.duration_ms);
        }
        let elapsed = Instant::now()
            .saturating_duration_since(self.progress_anchor)
            .as_millis() as u64;
        (self.progress_ms + elapsed).min(self.duration_ms)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RepeatMode {
    #[default]
    Off,
    Track,
    Context,
}

pub async fn get_me(token: &str) -> Result<Profile, AuthError> {
    #[derive(Deserialize)]
    struct R {
        #[serde(default)]
        id: String,
        #[serde(default)]
        display_name: String,
        #[serde(default)]
        images: Vec<RawImg>,
        #[serde(default)]
        country: String,
    }
    let r: R = get_json(token, &format!("{API}/me"), ttl::SLOW).await?;
    Ok(Profile {
        id: r.id,
        display_name: r.display_name,
        avatar_url: pick_thumb(&r.images),
        country: r.country,
    })
}

pub async fn get_playlists(token: &str) -> Result<Vec<PlaylistRef>, AuthError> {
    #[derive(Deserialize)]
    struct R {
        items: Vec<Item>,
    }
    #[derive(Deserialize)]
    struct Item {
        id: String,
        #[serde(default)]
        name: String,
        #[serde(default)]
        images: Vec<RawImg>,
    }
    let r: R = get_json(token, &format!("{API}/me/playlists?limit=20"), ttl::MUTABLE).await?;
    Ok(r.items
        .into_iter()
        .map(|p| PlaylistRef {
            id: p.id,
            name: p.name,
            // Full res for the "Made For You" home tile; tiny for the
            // sidebar library icon — same album, two scdn tiers.
            image_url: pick_full(&p.images),
            image_url_small: pick_tiny(&p.images),
        })
        .collect())
}

/// One of the user's playlists, with the bits needed to decide editability
/// (owner + collaborative) — only owned or collaborative playlists can take
/// add/remove writes. Used to build the track→playlists membership index.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LibraryPlaylist {
    pub id: String,
    pub name: String,
    pub owner_id: String,
    pub collaborative: bool,
}

impl LibraryPlaylist {
    /// Can the current user (`me`) add/remove tracks here?
    pub fn editable(&self, me: &str) -> bool {
        self.collaborative || (!me.is_empty() && self.owner_id == me)
    }
}

/// Uncached GET → JSON. The membership scan caches the *derived index*, not
/// the raw playlist pages, so these go straight to the network.
async fn http_get_json<T: serde::de::DeserializeOwned>(
    token: &str,
    url: &str,
) -> Result<T, AuthError> {
    let res = reqwest::Client::new().get(url).bearer_auth(token).send().await?;
    let status = res.status();
    if !status.is_success() {
        let body = res.text().await.unwrap_or_default();
        return Err(AuthError::Api(body, Some(status.as_u16())));
    }
    Ok(res.json::<T>().await?)
}

/// Every playlist in the user's library (paginated `/me/playlists`),
/// fetched fresh. The caller filters to editable ones + caches the derived
/// membership index.
pub async fn get_my_playlists(token: &str) -> Result<Vec<LibraryPlaylist>, AuthError> {
    #[derive(Deserialize)]
    struct Page {
        items: Vec<Item>,
        next: Option<String>,
    }
    #[derive(Deserialize)]
    struct Item {
        id: String,
        #[serde(default)]
        name: String,
        #[serde(default)]
        collaborative: bool,
        owner: Owner,
    }
    #[derive(Deserialize)]
    struct Owner {
        #[serde(default)]
        id: String,
    }
    let mut out = Vec::new();
    let mut url = format!("{API}/me/playlists?limit=50");
    loop {
        let page: Page = http_get_json(token, &url).await?;
        for it in page.items {
            out.push(LibraryPlaylist {
                id: it.id,
                name: it.name,
                owner_id: it.owner.id,
                collaborative: it.collaborative,
            });
        }
        match page.next {
            Some(n) => url = n,
            None => break,
        }
    }
    Ok(out)
}

/// Every `spotify:track:…` URI in a playlist (paginated, uri-only fields).
/// Local files and other non-track items are skipped.
pub async fn playlist_track_uris(token: &str, playlist_id: &str) -> Result<Vec<String>, AuthError> {
    #[derive(Deserialize)]
    struct Page {
        items: Vec<Item>,
        next: Option<String>,
    }
    #[derive(Deserialize)]
    struct Item {
        // Playlist `/items` rows wrap the track in `item` (post-migration).
        item: Option<Track>,
    }
    #[derive(Deserialize)]
    struct Track {
        #[serde(default)]
        uri: String,
    }
    let mut out = Vec::new();
    // `/items` (was `/tracks`, now 403 for Dev-Mode apps).
    let mut url =
        format!("{API}/playlists/{playlist_id}/items?fields=items(item(uri)),next&limit=100");
    loop {
        let page: Page = http_get_json(token, &url).await?;
        for it in page.items {
            if let Some(t) = it.item
                && t.uri.starts_with("spotify:track:")
            {
                out.push(t.uri);
            }
        }
        match page.next {
            Some(n) => url = n,
            None => break,
        }
    }
    Ok(out)
}

/// Add `track_uri` to a playlist (`POST /playlists/{id}/items` — the
/// Mar-2026 migration replaced `/tracks`, which now 403s for Dev-Mode apps).
pub async fn add_to_playlist(
    token: &str,
    playlist_id: &str,
    track_uri: &str,
) -> Result<(), AuthError> {
    let body = serde_json::json!({ "uris": [track_uri] });
    let res = reqwest::Client::new()
        .post(format!("{API}/playlists/{playlist_id}/items"))
        .bearer_auth(token)
        .json(&body)
        .send()
        .await?;
    playlist_write_result(res).await
}

/// Remove all occurrences of `track_uri` from a playlist
/// (`DELETE /playlists/{id}/items`). The migration also renamed the body
/// param `tracks` → `items`.
pub async fn remove_from_playlist(
    token: &str,
    playlist_id: &str,
    track_uri: &str,
) -> Result<(), AuthError> {
    let body = serde_json::json!({ "items": [{ "uri": track_uri }] });
    let res = reqwest::Client::new()
        .request(
            reqwest::Method::DELETE,
            format!("{API}/playlists/{playlist_id}/items"),
        )
        .bearer_auth(token)
        .json(&body)
        .send()
        .await?;
    playlist_write_result(res).await
}

async fn playlist_write_result(res: reqwest::Response) -> Result<(), AuthError> {
    let status = res.status();
    if status.is_success() {
        return Ok(());
    }
    let body = res.text().await.unwrap_or_default();
    Err(AuthError::Api(body, Some(status.as_u16())))
}

/// Sentinel id used everywhere (nav state, cache key) to mean the Liked
/// Songs collection rather than a real playlist.
pub const LIKED_SONGS_ID: &str = "__liked__";

/// Deserialize a field that may be **explicitly `null`** into its
/// `Default`. `#[serde(default)]` alone only covers a *missing* field —
/// Spotify sends `"id": null` (etc.) for local files and market-
/// unavailable tracks, and one such track used to fail the whole page
/// parse, stranding the rest of the playlist on skeletons.
fn null_default<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}

/// Join artist display names with ", " (the plain-string fallback).
fn join_artist_names(artists: &[TrackArtist]) -> String {
    artists
        .iter()
        .map(|a| a.name.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

// Shared deserialize shape for the playlist + saved-tracks endpoints.
// Spotify's 2026 Dev-Mode migration moved playlist contents to
// `GET /playlists/{id}/items`, whose wrapper field is `item` (the old
// `track` is deprecated there). `/me/tracks` is unchanged and wraps each
// row in `track`. Both fields are genuine current fields on their
// respective endpoints, so the row carries both and `track()` resolves
// the populated one (preferring the non-deprecated `item`).
#[derive(Deserialize)]
struct RawItem {
    #[serde(default)]
    item: Option<RawTrack>,
    #[serde(default)]
    track: Option<RawTrack>,
}

impl RawItem {
    fn track(self) -> Option<RawTrack> {
        self.item.or(self.track)
    }
}
#[derive(Deserialize)]
struct RawTrack {
    #[serde(default, deserialize_with = "null_default")]
    id: String,
    #[serde(default, deserialize_with = "null_default")]
    uri: String,
    #[serde(default, deserialize_with = "null_default")]
    name: String,
    #[serde(default, deserialize_with = "null_default")]
    duration_ms: u64,
    #[serde(default, deserialize_with = "null_default")]
    artists: Vec<RawArtist>,
    #[serde(default, deserialize_with = "null_default")]
    album: RawAlbum,
    /// Local file on another device — listed in the playlist but not
    /// startable through the Web API.
    #[serde(default, deserialize_with = "null_default")]
    is_local: bool,
    /// Present (and honest) only when the request carries a `market`;
    /// `false` = not available in the user's region. Absent ⇒ playable.
    #[serde(default)]
    is_playable: Option<bool>,
}
#[derive(Deserialize)]
struct RawArtist {
    #[serde(default, deserialize_with = "null_default")]
    id: String,
    #[serde(default, deserialize_with = "null_default")]
    name: String,
}
#[derive(Deserialize, Default)]
struct RawAlbum {
    #[serde(default, deserialize_with = "null_default")]
    id: String,
    #[serde(default, deserialize_with = "null_default")]
    name: String,
    #[serde(default, deserialize_with = "null_default")]
    images: Vec<RawImg>,
}
#[derive(Deserialize)]
struct RawImg {
    #[serde(default, deserialize_with = "null_default")]
    url: String,
    /// Spotify reports each image's pixel width (`null` for some sources).
    #[serde(default)]
    width: Option<u32>,
}

/// Pick the **smallest** image whose width is ≥ `min_w`. Spotify returns
/// a widest-first `[640, 300, 64]` array per album/artist. Matching the
/// fetched resolution to the on-screen display size is a big win: a
/// playlist row thumb shows at ~40 logical px (~80 physical at 2× DPI),
/// so the 640 px cover is ~250× the pixels drawn — decoding + uploading
/// 640² (1.6 MB) per row is what stalls a fast scroll over a large list.
/// The ~300 px variant is crisp at every thumb/tile size here and ~5×
/// cheaper to fetch + decode + upload. Falls back to the first (largest)
/// entry when nothing meets `min_w` or width metadata is absent.
fn pick_image_at_least(images: &[RawImg], min_w: u32) -> Option<String> {
    let mut best: Option<(u32, &str)> = None;
    let mut any: Option<&str> = None;
    for img in images {
        // Null-tolerant parse can leave an empty url — never a usable pick.
        if img.url.is_empty() {
            continue;
        }
        if any.is_none() {
            any = Some(&img.url);
        }
        if let Some(w) = img.width
            && w >= min_w
            && best.as_ref().map(|(bw, _)| w < *bw).unwrap_or(true)
        {
            best = Some((w, &img.url));
        }
    }
    best.map(|(_, u)| u).or(any).map(str::to_string)
}

// Spotify album art comes in three fixed tiers — **640 / 300 / 64 px**
// (640 is the ceiling; there is no higher variant via the API). Match the
// fetched tier to the on-screen display box (× 2 for DPI) so images are
// crisp without over-fetching:
//
//   • now-playing cover, home tiles, "new release" card, playlist header
//     — display ≥ ~160 logical px (≥ 320 physical) → the **640** tier
//     (300 upscaled into a 320 px box reads slightly soft).
//   • playlist track rows — ~40 logical px, but there are 1000s of them,
//     so the **300** tier keeps fast-scroll fetch/decode/upload cheap.
//   • sidebar library rows — tiny (~48 logical px) and few, so the **64**
//     tier is plenty and lightest.

/// Playlist-row thumbs (~300 px): smallest variant ≥ this.
const ROW_MIN_W: u32 = 160;
/// Sidebar library icons (~64 px): smallest variant ≥ this.
const SIDEBAR_MIN_W: u32 = 48;

/// Row-thumb pick (≈ 300 px) — for the virtualized playlist track list.
fn pick_thumb(images: &[RawImg]) -> Option<String> {
    pick_image_at_least(images, ROW_MIN_W)
}

/// Tiny pick (≈ 64 px) — sidebar library icons.
fn pick_tiny(images: &[RawImg]) -> Option<String> {
    pick_image_at_least(images, SIDEBAR_MIN_W)
}

/// Full-resolution pick: the largest (640 px) variant. Now-playing cover,
/// home tiles, "new release" card, playlist header — anything shown large
/// enough that 300 px upscales visibly.
fn pick_full(images: &[RawImg]) -> Option<String> {
    pick_image_at_least(images, u32::MAX)
}

impl RawTrack {
    fn into_track(self) -> PlaylistTrack {
        let playable = !self.is_local && self.is_playable.unwrap_or(true) && !self.uri.is_empty();
        let artists: Vec<TrackArtist> = self
            .artists
            .into_iter()
            .map(|a| TrackArtist {
                id: a.id,
                name: a.name,
            })
            .collect();
        let artist = join_artist_names(&artists);
        let artist_id = artists.first().map(|a| a.id.clone()).unwrap_or_default();
        PlaylistTrack {
            id: self.id,
            uri: self.uri,
            name: self.name,
            artist,
            album: self.album.name,
            album_image_url: pick_thumb(&self.album.images),
            duration_ms: self.duration_ms,
            artists,
            album_id: self.album.id,
            artist_id,
            playable,
        }
    }
}

fn tracks_from_items(items: Vec<RawItem>) -> Vec<PlaylistTrack> {
    items
        .into_iter()
        .filter_map(RawItem::track)
        // Keep unplayable-but-displayable tracks (local files, region
        // blocks) — they render faded. Drop only nameless husks (a fully
        // null track object) that have nothing to show.
        .filter(|t| !t.id.is_empty() || !t.name.is_empty())
        .map(RawTrack::into_track)
        .collect()
}

/// Page size for the streaming track loads. Playlist-tracks endpoint
/// caps at 100; saved-tracks (`/me/tracks`) caps at 50.
pub const PLAYLIST_PAGE: u32 = 100;
pub const LIKED_PAGE: u32 = 50;

/// Lightweight playlist metadata (no tracks) — fetched first so the
/// header + scrollbar length appear before any track page lands.
#[derive(Debug, Clone)]
pub struct PlaylistMeta {
    pub name: String,
    pub owner: String,
    pub image_url: Option<String>,
    pub total: u32,
}

pub async fn playlist_meta(token: &str, playlist_id: &str) -> Result<PlaylistMeta, AuthError> {
    #[derive(Deserialize)]
    struct R {
        #[serde(default)]
        name: String,
        #[serde(default)]
        owner: Owner,
        #[serde(default)]
        images: Vec<RawImg>,
        #[serde(default)]
        tracks: TotalOnly,
    }
    #[derive(Deserialize, Default)]
    struct Owner {
        #[serde(default)]
        display_name: String,
    }
    #[derive(Deserialize, Default)]
    struct TotalOnly {
        #[serde(default)]
        total: u32,
    }
    let fields = "name,owner(display_name),images,tracks.total";
    let r: R = get_json(
        token,
        &format!("{API}/playlists/{playlist_id}?fields={fields}"),
        ttl::MUTABLE,
    )
    .await?;
    Ok(PlaylistMeta {
        name: r.name,
        owner: r.owner.display_name,
        // Playlist header cover (large) — full res.
        image_url: pick_full(&r.images),
        total: r.tracks.total,
    })
}

/// One page of tracks plus the endpoint's reported `total` and the raw
/// item count (incl. nulls — needed to detect the last page when some
/// entries get filtered out).
#[derive(Debug, Clone)]
pub struct TracksPage {
    pub tracks: Vec<PlaylistTrack>,
    pub total: u32,
    pub raw_count: u32,
}

pub async fn fetch_tracks_page(token: &str, url: &str) -> Result<TracksPage, AuthError> {
    #[derive(Deserialize)]
    struct Page {
        #[serde(default)]
        total: u32,
        #[serde(default)]
        items: Vec<RawItem>,
    }
    let page: Page = get_json(token, url, ttl::MUTABLE).await?;
    let raw_count = page.items.len() as u32;
    Ok(TracksPage {
        tracks: tracks_from_items(page.items),
        total: page.total,
        raw_count,
    })
}

/// URL for a page of a real playlist's tracks (fields-masked).
/// `market=from_token` makes Spotify report `is_playable` per track
/// (region availability against the account's country) — without it
/// every track looks playable until the play request 403s.
pub fn playlist_tracks_url(playlist_id: &str, offset: u32, limit: u32) -> String {
    // 2026 migration: contents moved to `/items` (the old `/tracks` now 403s
    // for Dev-Mode apps) and each row's wrapper is `item` (not `track`).
    let fields = "total,items(item(id,uri,name,duration_ms,is_local,is_playable,artists(id,name),album(id,name,images)))";
    format!(
        "{API}/playlists/{playlist_id}/items?market=from_token&limit={limit}&offset={offset}&fields={fields}"
    )
}

/// URL for a page of the saved-tracks (Liked Songs) collection.
/// `market=from_token` for `is_playable`, as above.
pub fn liked_tracks_url(offset: u32, limit: u32) -> String {
    format!("{API}/me/tracks?market=from_token&limit={limit}&offset={offset}")
}

/// Drop every cached page of a track collection after we mutate it
/// (add/remove/like), so the next open re-fetches live data. Pages are
/// contiguous from offset 0; walk forward evicting each cached page and
/// stop at the first offset that was never cached — no network, no guess
/// at the track count. Blocking fs; call off the async runtime.
fn evict_track_pages(mut url_at: impl FnMut(u32) -> String, page: u32) {
    let mut offset = 0u32;
    loop {
        let key = url_key(&url_at(offset));
        if !disk_cache::exists(&key) {
            break;
        }
        disk_cache::remove(&key);
        offset += page;
    }
}

/// Invalidate a playlist's cached track-list pages (after add/remove).
pub fn invalidate_playlist_tracks(playlist_id: &str) {
    let id = playlist_id.to_string();
    evict_track_pages(|off| playlist_tracks_url(&id, off, PLAYLIST_PAGE), PLAYLIST_PAGE);
}

/// Invalidate the Liked Songs cached pages (after like/unlike).
pub fn invalidate_liked_tracks() {
    evict_track_pages(|off| liked_tracks_url(off, LIKED_PAGE), LIKED_PAGE);
}

pub async fn get_recently_played(token: &str) -> Result<Vec<RecentTrack>, AuthError> {
    #[derive(Deserialize)]
    struct R {
        items: Vec<Item>,
    }
    #[derive(Deserialize)]
    struct Item {
        track: Track,
        #[serde(default)]
        played_at: String,
    }
    #[derive(Deserialize)]
    struct Track {
        #[serde(default, deserialize_with = "null_default")]
        id: String,
        #[serde(default, deserialize_with = "null_default")]
        name: String,
        #[serde(default, deserialize_with = "null_default")]
        artists: Vec<Artist>,
        #[serde(default, deserialize_with = "null_default")]
        album: Album,
    }
    #[derive(Deserialize, Default)]
    struct Artist {
        #[serde(default, deserialize_with = "null_default")]
        name: String,
    }
    #[derive(Deserialize, Default)]
    struct Album {
        #[serde(default, deserialize_with = "null_default")]
        id: String,
        #[serde(default, deserialize_with = "null_default")]
        images: Vec<RawImg>,
    }
    // Fetch more than we show: collapsing repeat-listens below can eat a
    // good chunk of the raw page.
    let r: R = get_json(
        token,
        &format!("{API}/me/player/recently-played?limit=30"),
        ttl::VOLATILE,
    )
    .await?;
    let mut out: Vec<RecentTrack> = r
        .items
        .into_iter()
        .map(|i| RecentTrack {
            id: i.track.id,
            name: i.track.name,
            artist: i
                .track
                .artists
                .into_iter()
                .next()
                .map(|a| a.name)
                .unwrap_or_default(),
            album_id: i.track.album.id,
            // Home "Recently played" tiles (TILE_THUMB ≈ 320 px physical).
            album_image_url: pick_full(&i.track.album.images),
            played_at: i.played_at,
        })
        .collect();
    // Spotify logs every play, so a song on repeat shows up as a run of
    // identical consecutive entries. Collapse each run to its most recent
    // play (items arrive newest-first) — a history of "X, X, X, X" tells
    // the user nothing the first X doesn't.
    out.dedup_by(|next, kept| next.id == kept.id);
    Ok(out)
}

/// User's top artists for the past ~4 weeks (`short_term`). Up to
/// `limit` items, highest-rank first.
pub async fn get_top_artists(token: &str, limit: u32) -> Result<Vec<ArtistRef>, AuthError> {
    #[derive(Deserialize)]
    struct R {
        items: Vec<Item>,
    }
    #[derive(Deserialize)]
    struct Item {
        #[serde(default)]
        id: String,
        #[serde(default)]
        name: String,
        #[serde(default)]
        images: Vec<RawImg>,
    }
    let url = format!("{API}/me/top/artists?time_range=short_term&limit={limit}");
    let r: R = get_json(token, &url, ttl::SLOW).await?;
    Ok(r.items
        .into_iter()
        .map(|a| ArtistRef {
            id: a.id,
            name: a.name,
            // Home "Your top artists" tiles.
            image_url: pick_full(&a.images),
        })
        .collect())
}

/// User's top tracks for the past ~4 weeks (`short_term`).
pub async fn get_top_tracks(token: &str, limit: u32) -> Result<Vec<TrackRef>, AuthError> {
    #[derive(Deserialize)]
    struct R {
        items: Vec<Item>,
    }
    #[derive(Deserialize)]
    struct Item {
        #[serde(default)]
        id: String,
        #[serde(default)]
        name: String,
        #[serde(default)]
        artists: Vec<Artist>,
        album: Album,
    }
    #[derive(Deserialize)]
    struct Artist {
        #[serde(default)]
        name: String,
    }
    #[derive(Deserialize)]
    struct Album {
        #[serde(default)]
        id: String,
        #[serde(default)]
        images: Vec<RawImg>,
    }
    let url = format!("{API}/me/top/tracks?time_range=short_term&limit={limit}");
    let r: R = get_json(token, &url, ttl::SLOW).await?;
    Ok(r.items
        .into_iter()
        .map(|t| TrackRef {
            id: t.id,
            name: t.name,
            artist: t
                .artists
                .into_iter()
                .next()
                .map(|a| a.name)
                .unwrap_or_default(),
            album_id: t.album.id,
            // Home "Your top tracks" tiles.
            album_image_url: pick_full(&t.album.images),
        })
        .collect())
}

/// Artist header info (name + image) for the artist page. Discography is
/// fetched separately via [`get_artist_albums`].
#[derive(Debug, Clone)]
pub struct ArtistDetail {
    pub id: String,
    pub name: String,
    pub image_url: Option<String>,
    /// Total followers — shown under the artist name.
    pub followers: u64,
}

/// Fetch an artist's profile (`/v1/artists/{id}`). Name changes ~never, so
/// a long-ish `SLOW` TTL is plenty.
pub async fn get_artist(token: &str, artist_id: &str) -> Result<ArtistDetail, AuthError> {
    #[derive(Deserialize)]
    struct R {
        #[serde(default)]
        id: String,
        #[serde(default)]
        name: String,
        #[serde(default)]
        images: Vec<RawImg>,
        #[serde(default)]
        followers: Followers,
    }
    #[derive(Deserialize, Default)]
    struct Followers {
        #[serde(default)]
        total: u64,
    }
    let r: R = get_json(token, &format!("{API}/artists/{artist_id}"), ttl::SLOW).await?;
    Ok(ArtistDetail {
        id: if r.id.is_empty() {
            artist_id.to_string()
        } else {
            r.id
        },
        name: r.name,
        // Artist hero image (large).
        image_url: pick_full(&r.images),
        followers: r.followers.total,
    })
}

/// Resolve a playing context's display name ("Chill", the album title,
/// the artist name) from its uri via the Web API. Fallback for pushes
/// that carry no `context_description` — the `/me/player` cold-start
/// seed and our own local playback. `None` for kinds without a
/// fetchable name (the caller keeps its kind label).
pub async fn get_context_name(token: &str, uri: &str) -> Result<Option<String>, AuthError> {
    #[derive(Deserialize)]
    struct Named {
        #[serde(default)]
        name: String,
    }
    let mut parts = uri.split(':');
    let (Some("spotify"), Some(kind), Some(id)) = (parts.next(), parts.next(), parts.next())
    else {
        return Ok(None);
    };
    let url = match kind {
        "playlist" => format!("{API}/playlists/{id}?fields=name"),
        "album" => format!("{API}/albums/{id}"),
        "artist" => format!("{API}/artists/{id}"),
        "show" => format!("{API}/shows/{id}"),
        _ => return Ok(None),
    };
    let r: Named = get_json(token, &url, ttl::SLOW).await?;
    Ok((!r.name.is_empty()).then_some(r.name))
}

/// An artist's most popular tracks (`/v1/artists/{id}/top-tracks`). Requires
/// a `market` (the user's country); falls back to `US` if unknown. Mapped to
/// `PlaylistTrack` so the artist page reuses the track-row rendering.
pub async fn get_artist_top_tracks(
    token: &str,
    artist_id: &str,
    market: &str,
) -> Result<Vec<PlaylistTrack>, AuthError> {
    #[derive(Deserialize)]
    struct R {
        #[serde(default)]
        tracks: Vec<RawTrack>,
    }
    let market = if market.is_empty() { "US" } else { market };
    let url = format!("{API}/artists/{artist_id}/top-tracks?market={market}");
    let r: R = get_json(token, &url, ttl::SLOW).await?;
    Ok(r.tracks
        .into_iter()
        .filter(|t| !t.id.is_empty())
        .map(RawTrack::into_track)
        .collect())
}

/// Albums by an artist, sorted newest-first by `release_date`. We
/// request `include_groups=album,single` (skip appearances + compilations)
/// and re-sort client-side because Spotify's default order is not
/// guaranteed to be by date.
pub async fn get_artist_albums(
    token: &str,
    artist_id: &str,
    limit: u32,
) -> Result<Vec<AlbumRef>, AuthError> {
    #[derive(Deserialize)]
    struct R {
        items: Vec<Item>,
    }
    #[derive(Deserialize)]
    struct Item {
        #[serde(default)]
        id: String,
        #[serde(default)]
        name: String,
        #[serde(default)]
        artists: Vec<Artist>,
        #[serde(default)]
        images: Vec<RawImg>,
        #[serde(default)]
        release_date: String,
    }
    #[derive(Deserialize)]
    struct Artist {
        #[serde(default)]
        name: String,
    }
    // Spotify's 2026 Dev-Mode slimming rejects the old page sizes with
    // 400 "Invalid limit" — 5 is the largest known-accepted value, so
    // page in 5s (each page disk-cached) up to the requested count.
    const PAGE: u32 = 5;
    let mut albums: Vec<AlbumRef> = Vec::new();
    let mut offset = 0;
    while (albums.len() as u32) < limit {
        let url = format!(
            "{API}/artists/{artist_id}/albums?offset={offset}&limit={PAGE}&include_groups=album,single"
        );
        let r: R = get_json(token, &url, ttl::SLOW).await?;
        let page_len = r.items.len() as u32;
        albums.extend(r.items.into_iter().map(|a| AlbumRef {
            id: a.id,
            name: a.name,
            artist: a
                .artists
                .into_iter()
                .next()
                .map(|a| a.name)
                .unwrap_or_default(),
            // "New release" spotlight card (THUMB_XL) — full res.
            image_url: pick_full(&a.images),
            release_date: a.release_date,
        }));
        if page_len < PAGE {
            break;
        }
        offset += PAGE;
    }
    albums.truncate(limit as usize);
    // Lexicographic sort on `YYYY[-MM[-DD]]` is chronological.
    albums.sort_by(|a, b| b.release_date.cmp(&a.release_date));
    Ok(albums)
}

/// Full album page: metadata + first page (≤ 50) of tracks, mapped to the
/// shared [`PlaylistDetail`] so an album reuses the playlist track-list
/// pipeline (cache, view, playback). `context_uri` is `spotify:album:{id}`
/// (albums are a playable context). `owner` carries the album artist;
/// `total` is the loaded count (albums past 50 tracks aren't paged).
pub async fn get_album(token: &str, album_id: &str) -> Result<PlaylistDetail, AuthError> {
    #[derive(Deserialize)]
    struct R {
        #[serde(default)]
        name: String,
        #[serde(default)]
        artists: Vec<Artist>,
        #[serde(default)]
        images: Vec<RawImg>,
        #[serde(default)]
        tracks: Tracks,
    }
    #[derive(Deserialize, Default)]
    struct Tracks {
        #[serde(default)]
        items: Vec<Item>,
    }
    #[derive(Deserialize)]
    struct Item {
        #[serde(default, deserialize_with = "null_default")]
        id: String,
        #[serde(default, deserialize_with = "null_default")]
        uri: String,
        #[serde(default, deserialize_with = "null_default")]
        name: String,
        #[serde(default, deserialize_with = "null_default")]
        duration_ms: u64,
        #[serde(default, deserialize_with = "null_default")]
        artists: Vec<Artist>,
        /// `market=from_token` ⇒ region availability per track.
        #[serde(default)]
        is_playable: Option<bool>,
    }
    #[derive(Deserialize, Default)]
    struct Artist {
        #[serde(default, deserialize_with = "null_default")]
        id: String,
        #[serde(default, deserialize_with = "null_default")]
        name: String,
    }
    let r: R = get_json(
        token,
        &format!("{API}/albums/{album_id}?market=from_token&limit=50"),
        ttl::IMMUTABLE,
    )
    .await?;
    let album_name = r.name.clone();
    let artist = r
        .artists
        .into_iter()
        .next()
        .map(|a| a.name)
        .unwrap_or_default();
    let image_url = pick_full(&r.images);
    let row_thumb = pick_thumb(&r.images);
    let tracks: Vec<PlaylistTrack> = r
        .tracks
        .items
        .into_iter()
        .filter(|t| !t.id.is_empty())
        .map(|t| {
            let artists: Vec<TrackArtist> = t
                .artists
                .into_iter()
                .map(|a| TrackArtist {
                    id: a.id,
                    name: a.name,
                })
                .collect();
            let artist = join_artist_names(&artists);
            let artist_id = artists.first().map(|a| a.id.clone()).unwrap_or_default();
            PlaylistTrack {
                id: t.id,
                playable: t.is_playable.unwrap_or(true) && !t.uri.is_empty(),
                uri: t.uri,
                name: t.name,
                artist,
                album: album_name.clone(),
                // Album tracks share the album cover; row-thumb tier for the list.
                album_image_url: row_thumb.clone(),
                duration_ms: t.duration_ms,
                artists,
                album_id: album_id.to_string(),
                artist_id,
            }
        })
        .collect();
    let total = tracks.len() as u32;
    Ok(PlaylistDetail {
        id: album_id.to_string(),
        name: r.name,
        owner: artist,
        image_url,
        context_uri: Some(format!("spotify:album:{album_id}")),
        tracks,
        total,
    })
}

/// Bare-ID lookup against `/v1/tracks/{id}`. Used to fill the artist
/// name (which `ProvidedTrack.metadata` doesn't carry — only an
/// `artist_uri`) on each `track_id` change.
pub async fn get_track(token: &str, track_id: &str) -> Result<TrackDetails, AuthError> {
    #[derive(Deserialize)]
    struct R {
        #[serde(default)]
        id: String,
        #[serde(default)]
        name: String,
        #[serde(default)]
        artists: Vec<Artist>,
        #[serde(default)]
        album: Album,
    }
    #[derive(Deserialize, Default)]
    struct Artist {
        #[serde(default)]
        id: String,
        #[serde(default)]
        name: String,
    }
    #[derive(Deserialize, Default)]
    struct Album {
        #[serde(default)]
        id: String,
        #[serde(default)]
        images: Vec<RawImg>,
    }
    // Track metadata is immutable — the hot path (refetched on every
    // track change) gets the longest TTL so repeated plays never re-hit.
    let r: R = get_json(token, &format!("{API}/tracks/{track_id}"), ttl::IMMUTABLE).await?;
    let artists: Vec<TrackArtist> = r
        .artists
        .into_iter()
        .map(|a| TrackArtist {
            id: a.id,
            name: a.name,
        })
        .collect();
    Ok(TrackDetails {
        track_id: r.id,
        name: r.name,
        artist: join_artist_names(&artists),
        artist_id: artists.first().map(|a| a.id.clone()).unwrap_or_default(),
        artists,
        // Now-playing cover (large + full-window blurred backdrop) — full res.
        album_image_url: pick_full(&r.album.images),
        album_id: r.album.id,
    })
}

#[derive(Debug, Clone)]
pub struct TrackDetails {
    pub track_id: String,
    pub name: String,
    pub artist: String,
    /// First artist's bare id — backfills sparse cluster pushes (some ship
    /// no `artist_uri`), keeping artist-keyed features (the about card)
    /// reactive on every track change.
    pub artist_id: String,
    /// Every credited artist (id + name) — backfills the clickable
    /// multi-artist lines the same way.
    pub artists: Vec<TrackArtist>,
    pub album_image_url: Option<String>,
    pub album_id: String,
}

/// Strip the `spotify:track:` URI prefix to get the bare ID Web API needs.
/// Returns `None` if the input isn't a track URI.
pub fn track_id_from_uri(uri: &str) -> Option<&str> {
    uri.strip_prefix("spotify:track:")
}

pub async fn get_currently_playing(token: &str) -> Result<Option<CurrentlyPlaying>, AuthError> {
    #[derive(Deserialize)]
    struct R {
        #[serde(default)]
        is_playing: bool,
        #[serde(default)]
        progress_ms: u64,
        #[serde(default)]
        shuffle_state: bool,
        #[serde(default)]
        repeat_state: String,
        item: Option<Track>,
        #[serde(default)]
        context: Option<Ctx>,
    }
    #[derive(Deserialize)]
    struct Ctx {
        #[serde(default)]
        uri: String,
    }
    #[derive(Deserialize)]
    struct Track {
        #[serde(default)]
        id: String,
        #[serde(default)]
        uri: String,
        #[serde(default)]
        name: String,
        #[serde(default)]
        duration_ms: u64,
        #[serde(default)]
        artists: Vec<Artist>,
        #[serde(default)]
        album: Album,
    }
    #[derive(Deserialize)]
    struct Artist {
        #[serde(default)]
        id: String,
        #[serde(default)]
        name: String,
    }
    #[derive(Deserialize, Default)]
    struct Album {
        #[serde(default)]
        images: Vec<RawImg>,
    }

    let res = reqwest::Client::new()
        .get(format!("{API}/me/player"))
        .bearer_auth(token)
        .send()
        .await?;
    let status = res.status();
    if status.as_u16() == 204 {
        return Ok(None);
    }
    if !status.is_success() {
        let body = res.text().await.unwrap_or_default();
        return Err(AuthError::Api(body, Some(status.as_u16())));
    }
    let r: R = res.json().await?;
    let Some(item) = r.item else { return Ok(None) };
    let repeat = match r.repeat_state.as_str() {
        "track" => RepeatMode::Track,
        "context" => RepeatMode::Context,
        _ => RepeatMode::Off,
    };
    Ok(Some(CurrentlyPlaying {
        // Use the full `spotify:track:…` URI to match the cluster path
        // (the rest of the app — canvas fetch, track-id parsing — expects
        // the URI form, not the bare id).
        track_id: if item.uri.is_empty() {
            format!("spotify:track:{}", item.id)
        } else {
            item.uri
        },
        name: item.name,
        artist: item
            .artists
            .iter()
            .map(|a| a.name.as_str())
            .collect::<Vec<_>>()
            .join(", "),
        artist_id: item
            .artists
            .first()
            .map(|a| a.id.clone())
            .unwrap_or_default(),
        artists: item
            .artists
            .into_iter()
            .map(|a| TrackArtist {
                id: a.id,
                name: a.name,
            })
            .collect(),
        // Now-playing cover — full res (large + blurred backdrop).
        album_image_url: pick_full(&item.album.images),
        is_playing: r.is_playing,
        progress_ms: r.progress_ms,
        progress_anchor: Instant::now(),
        duration_ms: item.duration_ms,
        shuffle: r.shuffle_state,
        repeat,
        context_uri: r
            .context
            .map(|c| c.uri)
            .filter(|u| !u.is_empty()),
        // `/me/player` has no context display name (only uri/type);
        // the UI falls back to the kind until a cluster push names it.
        context_name: None,
    }))
}

/// Transport control against the user's **active** Connect device.
/// These hit the Web API player endpoints (not librespot/Spirc) on
/// purpose: Opal registers as a Connect device with a NullSink, so
/// taking over via Spirc would route audio into silence. The Web API
/// commands instead steer whatever device is already playing (phone,
/// desktop app, etc.), and the dealer cluster subscription pushes the
/// resulting state change back so the UI reflects it.
///
/// All endpoints return `204 No Content` on success. A `404` means
/// "no active device" — surfaced as `AuthError::Api` for the caller to
/// log; there's nothing to control until the user starts playback
/// somewhere.
async fn player_command(token: &str, method: reqwest::Method, path: &str) -> Result<(), AuthError> {
    let res = reqwest::Client::new()
        .request(method, format!("{API}{path}"))
        .bearer_auth(token)
        // PUT/POST with an empty body — Spotify rejects a missing
        // Content-Length on some of these, so set it explicitly.
        .header(reqwest::header::CONTENT_LENGTH, 0)
        .send()
        .await?;
    let status = res.status();
    if status.is_success() {
        return Ok(());
    }
    let body = res.text().await.unwrap_or_default();
    Err(AuthError::Api(body, Some(status.as_u16())))
}

/// Resume playback. `device_id = None` targets the active device;
/// `Some(id)` targets that device explicitly — the no-active-device
/// fallback, where the worker resumes on Opal's own librespot player.
pub async fn play(token: &str, device_id: Option<&str>) -> Result<(), AuthError> {
    let path = match device_id {
        Some(id) => format!("/me/player/play?device_id={id}"),
        None => "/me/player/play".to_string(),
    };
    player_command(token, reqwest::Method::PUT, &path).await
}

/// What to start playing on the active device. Real playlists/albums use
/// a `context_uri` (so Spotify queues the whole context); the Liked
/// Songs collection has no playable context URI, so it ships an explicit
/// `uris` list. Both carry an `offset` = the index to start at.
/// [`Self::ContextAt`] starts a context at a specific *track* (offset by
/// URI, not index) — for entry points that know the song but not its
/// position, like a recently-played row starting its album.
#[derive(Debug, Clone)]
pub enum PlayTarget {
    Context { context_uri: String, offset: u32 },
    ContextAt { context_uri: String, track_uri: String },
    Uris { uris: Vec<String>, offset: u32 },
    /// Resume the last track at an absolute position (ms) — the cold-start
    /// play button starting the last-played track exactly where the chrome
    /// shows the progress bar, so display and action stay coherent. When a
    /// `context_uri` is known it restarts *within* that context (so playback
    /// continues past the one track + librespot autoplay can take over);
    /// otherwise it plays the single track.
    Resume {
        uri: String,
        position_ms: u32,
        context_uri: Option<String>,
    },
}

impl PlayTarget {
    /// The queue-context uri this intent loads, if any (`Uris` is an ad-hoc
    /// list with no context). Used to stamp the source pill while Opal is the
    /// active local device — librespot `PlayerEvent`s don't carry it.
    pub fn context_uri(&self) -> Option<&str> {
        match self {
            PlayTarget::Context { context_uri, .. }
            | PlayTarget::ContextAt { context_uri, .. } => Some(context_uri),
            PlayTarget::Resume { context_uri, .. } => context_uri.as_deref(),
            PlayTarget::Uris { .. } => None,
        }
    }

    /// The bare track URI this intent centres on, if any — the anchor used to
    /// recover when Spotify rejects the context (algorithmic playlists 400
    /// "Non supported context uri"): we look up the track's album and replay
    /// within that. `Context`/`Uris` carry no single track to anchor on.
    fn anchor_track_uri(&self) -> Option<&str> {
        match self {
            PlayTarget::ContextAt { track_uri, .. } => Some(track_uri),
            PlayTarget::Resume {
                uri,
                context_uri: Some(_),
                ..
            } => Some(uri),
            _ => None,
        }
    }

    /// This intent rebased onto a different context (the track's album), so
    /// playback continues and librespot autoplay keeps suggesting content.
    fn with_context(&self, ctx: String) -> PlayTarget {
        match self {
            PlayTarget::ContextAt { track_uri, .. } => PlayTarget::ContextAt {
                context_uri: ctx,
                track_uri: track_uri.clone(),
            },
            PlayTarget::Resume {
                uri, position_ms, ..
            } => PlayTarget::Resume {
                uri: uri.clone(),
                position_ms: *position_ms,
                context_uri: Some(ctx),
            },
            other => other.clone(),
        }
    }

    /// This intent stripped to the single track (no context) — last resort
    /// when even the album context can't be resolved. Plays, but won't
    /// autoplay past the one track.
    fn track_only(&self) -> Option<PlayTarget> {
        match self {
            PlayTarget::ContextAt { track_uri, .. } => Some(PlayTarget::Uris {
                uris: vec![track_uri.clone()],
                offset: 0,
            }),
            PlayTarget::Resume {
                uri, position_ms, ..
            } => Some(PlayTarget::Resume {
                uri: uri.clone(),
                position_ms: *position_ms,
                context_uri: None,
            }),
            _ => None,
        }
    }
}

/// Start playback of a context (playlist/album) or explicit track list.
/// `device_id = None` targets the user's active Connect device; `Some(id)`
/// targets that device explicitly (the no-active-device fallback — play on
/// Opal's own librespot player). Body shape mirrors the official
/// client's `PUT /me/player/play`. A `404` (no active device) surfaces
/// as `AuthError::Api`.
pub async fn play_context(
    token: &str,
    target: PlayTarget,
    device_id: Option<&str>,
) -> Result<(), AuthError> {
    let url = match device_id {
        Some(id) => format!("{API}/me/player/play?device_id={id}"),
        None => format!("{API}/me/player/play"),
    };
    let first = play_target(token, &url, &target).await;
    // Algorithmic/personalized playlists (`spotify:playlist:37i9dQZF1E…` —
    // Daily Mix, Discover Weekly, radio) have no fixed track list the public
    // Web API will load as a context: it 400s "Non supported context uri".
    // Recover by re-anchoring on the track's album, so playback continues and
    // librespot autoplay keeps suggesting content past it. If even the album
    // can't be resolved, play the track alone (no autoplay, but it plays).
    let Err(AuthError::Api(_, Some(400))) = &first else {
        return first;
    };
    let Some(track_uri) = target.anchor_track_uri() else {
        return first;
    };
    let album_ctx = match track_id_from_uri(track_uri) {
        Some(id) => get_track(token, id)
            .await
            .ok()
            .filter(|d| !d.album_id.is_empty())
            .map(|d| format!("spotify:album:{}", d.album_id)),
        None => None,
    };
    if let Some(ctx) = album_ctx {
        log::warn!("context uri rejected — re-anchoring on album {ctx}");
        if play_target(token, &url, &target.with_context(ctx))
            .await
            .is_ok()
        {
            return Ok(());
        }
    }
    if let Some(t) = target.track_only() {
        log::warn!("album re-anchor failed — degrading to track-only");
        return play_target(token, &url, &t).await;
    }
    first
}

fn play_body(target: &PlayTarget) -> serde_json::Value {
    match target {
        PlayTarget::Context {
            context_uri,
            offset,
        } => serde_json::json!({
            "context_uri": context_uri,
            "offset": { "position": offset },
        }),
        PlayTarget::ContextAt {
            context_uri,
            track_uri,
        } => serde_json::json!({
            "context_uri": context_uri,
            "offset": { "uri": track_uri },
        }),
        PlayTarget::Uris { uris, offset } => serde_json::json!({
            "uris": uris,
            "offset": { "position": offset },
        }),
        // Within a context: restart at the track + position, but keep the
        // queue/context so playback continues (and autoplay can engage).
        PlayTarget::Resume {
            uri,
            position_ms,
            context_uri: Some(ctx),
        } => serde_json::json!({
            "context_uri": ctx,
            "offset": { "uri": uri },
            "position_ms": position_ms,
        }),
        // No context known — single track at position.
        PlayTarget::Resume {
            uri,
            position_ms,
            context_uri: None,
        } => serde_json::json!({
            "uris": [uri],
            "position_ms": position_ms,
        }),
    }
}

async fn play_target(token: &str, url: &str, target: &PlayTarget) -> Result<(), AuthError> {
    let res = reqwest::Client::new()
        .put(url)
        .bearer_auth(token)
        .json(&play_body(target))
        .send()
        .await?;
    let status = res.status();
    if status.is_success() {
        return Ok(());
    }
    let body = res.text().await.unwrap_or_default();
    Err(AuthError::Api(body, Some(status.as_u16())))
}

pub async fn pause(token: &str) -> Result<(), AuthError> {
    player_command(token, reqwest::Method::PUT, "/me/player/pause").await
}

/// Append a track to the active device's play queue (the one queue write
/// the Web API exposes — works on remote devices too). `track_uri` is the
/// `spotify:track:…` form.
pub async fn add_to_queue(token: &str, track_uri: &str) -> Result<(), AuthError> {
    // A track URI is `spotify:track:<base62>` — only the colons need
    // percent-encoding for the query string.
    let enc = track_uri.replace(':', "%3A");
    player_command(
        token,
        reqwest::Method::POST,
        &format!("/me/player/queue?uri={enc}"),
    )
    .await
}

/// A Connect device visible to the account, for the devices popup.
#[derive(Debug, Clone)]
pub struct Device {
    pub id: String,
    pub name: String,
    /// Spotify's device type string ("Computer", "Smartphone", "Speaker"…).
    pub kind: String,
    pub is_active: bool,
    pub volume_percent: Option<u8>,
}

/// All Connect devices currently visible. Never cached — the list is
/// live state (devices appear/disappear with the apps that host them).
pub async fn get_devices(token: &str) -> Result<Vec<Device>, AuthError> {
    #[derive(Deserialize)]
    struct R {
        #[serde(default)]
        devices: Vec<Item>,
    }
    #[derive(Deserialize)]
    struct Item {
        #[serde(default, deserialize_with = "null_default")]
        id: String,
        #[serde(default, deserialize_with = "null_default")]
        name: String,
        #[serde(default, deserialize_with = "null_default")]
        r#type: String,
        #[serde(default)]
        is_active: bool,
        #[serde(default)]
        volume_percent: Option<u8>,
    }
    let r: R = get_json(token, &format!("{API}/me/player/devices"), ttl::NONE).await?;
    Ok(r.devices
        .into_iter()
        .filter(|d| !d.id.is_empty())
        .map(|d| Device {
            id: d.id,
            name: d.name,
            kind: d.r#type,
            is_active: d.is_active,
            volume_percent: d.volume_percent,
        })
        .collect())
}

/// Transfer playback to `device_id`. `play = true` resumes there
/// immediately (the official client's behaviour when you pick a device).
pub async fn transfer_playback(token: &str, device_id: &str, play: bool) -> Result<(), AuthError> {
    let body = serde_json::json!({ "device_ids": [device_id], "play": play });
    let res = reqwest::Client::new()
        .put(format!("{API}/me/player"))
        .bearer_auth(token)
        .json(&body)
        .send()
        .await?;
    let status = res.status();
    if status.is_success() {
        return Ok(());
    }
    let body = res.text().await.unwrap_or_default();
    Err(AuthError::Api(body, Some(status.as_u16())))
}

/// Whether the current user has saved (liked) `track_id`.
pub async fn is_track_saved(token: &str, track_id: &str) -> Result<bool, AuthError> {
    // 2026 migration: `/me/tracks/contains` (IDs) → `/me/library/contains`
    // (Spotify URIs); now 403s for Dev-Mode apps. Same `[bool,…]` response.
    // Live state — a like toggled on another device must show here.
    let uri = format!("spotify:track:{track_id}");
    let r: Vec<bool> = get_json(
        token,
        &format!("{API}/me/library/contains?uris={uri}"),
        ttl::NONE,
    )
    .await?;
    Ok(r.first().copied().unwrap_or(false))
}

/// Save / unsave (like / unlike) a track for the current user.
pub async fn set_track_saved(token: &str, track_id: &str, saved: bool) -> Result<(), AuthError> {
    // 2026 migration: `PUT/DELETE /me/tracks?ids=` → `PUT/DELETE /me/library`
    // with a JSON body of Spotify URIs (the `?ids=` form now 403s).
    let method = if saved {
        reqwest::Method::PUT
    } else {
        reqwest::Method::DELETE
    };
    let body = serde_json::json!({ "uris": [format!("spotify:track:{track_id}")] });
    let res = reqwest::Client::new()
        .request(method, format!("{API}/me/library"))
        .bearer_auth(token)
        .json(&body)
        .send()
        .await?;
    let status = res.status();
    if status.is_success() {
        return Ok(());
    }
    let body = res.text().await.unwrap_or_default();
    Err(AuthError::Api(body, Some(status.as_u16())))
}

/// The active device's play queue: the playing track + what's next, in
/// order. Never cached — it changes with every skip/enqueue. When
/// Opal is the active device this still works: Spirc publishes its
/// queue to connect-state and the endpoint reads from there.
pub async fn get_queue(token: &str) -> Result<Vec<PlaylistTrack>, AuthError> {
    #[derive(Deserialize)]
    struct R {
        #[serde(default)]
        currently_playing: Option<RawTrack>,
        #[serde(default)]
        queue: Vec<RawTrack>,
    }
    let r: R = get_json(token, &format!("{API}/me/player/queue"), ttl::NONE).await?;
    Ok(r.currently_playing
        .into_iter()
        .chain(r.queue)
        .map(RawTrack::into_track)
        .collect())
}

/// Set the active device's volume (0..=100). When Opal is the active
/// device, Spotify routes this back to our Spirc, which adjusts the
/// SoftMixer — confirmation arrives via the local `VolumeChanged` event.
pub async fn set_volume(token: &str, percent: u8) -> Result<(), AuthError> {
    let pct = percent.min(100);
    player_command(
        token,
        reqwest::Method::PUT,
        &format!("/me/player/volume?volume_percent={pct}"),
    )
    .await
}

pub async fn next_track(token: &str) -> Result<(), AuthError> {
    player_command(token, reqwest::Method::POST, "/me/player/next").await
}

pub async fn previous_track(token: &str) -> Result<(), AuthError> {
    player_command(token, reqwest::Method::POST, "/me/player/previous").await
}

pub async fn set_shuffle(token: &str, on: bool) -> Result<(), AuthError> {
    player_command(
        token,
        reqwest::Method::PUT,
        &format!("/me/player/shuffle?state={on}"),
    )
    .await
}

pub async fn set_repeat(token: &str, mode: RepeatMode) -> Result<(), AuthError> {
    let state = match mode {
        RepeatMode::Off => "off",
        RepeatMode::Track => "track",
        RepeatMode::Context => "context",
    };
    player_command(
        token,
        reqwest::Method::PUT,
        &format!("/me/player/repeat?state={state}"),
    )
    .await
}

/// Seek to `position_ms`. `device_id` targets a specific device (used by
/// the transfer-from-self path to position the *new* device before it
/// resumes); `None` seeks whichever device is active (the scrubbable
/// progress bar — drag/click to seek, with a hover preview of the target
/// timestamp).
pub async fn seek(
    token: &str,
    position_ms: u32,
    device_id: Option<&str>,
) -> Result<(), AuthError> {
    let path = match device_id {
        Some(id) => format!("/me/player/seek?position_ms={position_ms}&device_id={id}"),
        None => format!("/me/player/seek?position_ms={position_ms}"),
    };
    player_command(token, reqwest::Method::PUT, &path).await
}

/// GET + deserialize a Web API JSON endpoint, transparently cached on disk.
///
/// The disk cache is keyed by [`url_key`] and stores the **raw response
/// bytes** (not the deserialized `T`) — so every endpoint, current or
/// future, is cached for free just by passing a `ttl`; no per-endpoint
/// `Serialize` impl is needed. A non-expired cache hit skips the network
/// entirely. `ttl == ttl::NONE` (zero) bypasses the cache on both read and
/// write. Only successful (`2xx`) responses are cached, and only after they
/// parse — error bodies and malformed payloads never poison the cache.
async fn get_json<T: for<'de> Deserialize<'de>>(
    token: &str,
    url: &str,
    ttl: Duration,
) -> Result<T, AuthError> {
    let key = url_key(url);
    // Cache read (off-thread — the cache is blocking fs IO).
    if !ttl.is_zero() {
        let k = key.clone();
        if let Ok(Some(bytes)) =
            tokio::task::spawn_blocking(move || disk_cache::read_raw_json(&k, ttl)).await
            && let Ok(value) = serde_json::from_slice::<T>(&bytes)
        {
            return Ok(value);
        }
    }
    let res = reqwest::Client::new()
        .get(url)
        .bearer_auth(token)
        .send()
        .await?;
    if !res.status().is_success() {
        let status = res.status().as_u16();
        let body = res.text().await.unwrap_or_default();
        return Err(AuthError::Api(body, Some(status)));
    }
    let bytes = res.bytes().await?;
    let value: T = serde_json::from_slice(&bytes)?;
    // Persist for next time — best-effort, off the async runtime.
    if !ttl.is_zero() {
        let raw = bytes.to_vec();
        tokio::task::spawn_blocking(move || disk_cache::write_raw_json(&key, &raw));
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Local files / market-unavailable tracks arrive with explicit
    /// `null` fields (`"id": null`), which `#[serde(default)]` alone
    /// does NOT cover — a single such track at any page offset used to
    /// fail the whole page parse and strand the rest of the playlist on
    /// skeletons. `null_default` must absorb them.
    #[test]
    fn playlist_item_with_null_track_fields_parses() {
        // Post-migration playlist `/items` rows wrap the track in `item`.
        let json = r#"{
            "item": {
                "id": null,
                "uri": null,
                "name": "Local file",
                "duration_ms": null,
                "artists": [{ "name": null }],
                "album": { "name": null, "images": [{ "url": null, "width": null }] }
            }
        }"#;
        let item: RawItem = serde_json::from_str(json).expect("null fields must parse");
        let t = item.track().expect("track present");
        assert_eq!(t.id, "");
        assert_eq!(t.uri, "");
        assert_eq!(t.name, "Local file");
        assert_eq!(t.duration_ms, 0);
        // An all-null image is unusable — the pickers must skip it.
        assert_eq!(pick_thumb(&t.album.images), None);
    }

    #[test]
    fn item_resolver_handles_both_wrappers() {
        // Playlist `/items` → `item`; saved `/me/tracks` → `track`. Both map.
        let from_item: RawItem =
            serde_json::from_str(r#"{ "item": { "name": "P" } }"#).unwrap();
        assert_eq!(from_item.track().unwrap().name, "P");
        let from_track: RawItem =
            serde_json::from_str(r#"{ "track": { "name": "S" } }"#).unwrap();
        assert_eq!(from_track.track().unwrap().name, "S");
        let empty: RawItem = serde_json::from_str(r#"{ "item": null }"#).unwrap();
        assert!(empty.track().is_none());
    }

    /// Local files and region-blocked tracks stay IN the list (rendered
    /// faded) — only nameless husks are dropped — and both map to
    /// `playable: false`.
    #[test]
    fn unplayable_tracks_are_kept_but_marked() {
        let json = r#"[
            { "item": { "id": null, "uri": "spotify:local:abc", "name": "Local song",
                         "is_local": true, "artists": [], "album": { "images": [] } } },
            { "item": { "id": "x1", "uri": "spotify:track:x1", "name": "Blocked here",
                         "is_playable": false, "artists": [], "album": { "images": [] } } },
            { "item": { "id": "x2", "uri": "spotify:track:x2", "name": "Fine",
                         "is_playable": true, "artists": [], "album": { "images": [] } } },
            { "item": { "id": null, "uri": null, "name": "", "artists": [],
                         "album": { "images": [] } } }
        ]"#;
        let items: Vec<RawItem> = serde_json::from_str(json).unwrap();
        let tracks = tracks_from_items(items);
        assert_eq!(tracks.len(), 3, "husk dropped, unplayables kept");
        assert!(!tracks[0].playable, "local file is not playable");
        assert!(!tracks[1].playable, "region-blocked is not playable");
        assert!(tracks[2].playable);
    }
}
