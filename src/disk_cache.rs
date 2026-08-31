//! On-disk cache for fetched album-art bytes.
//!
//! Spotify album covers are immutable for a given image id, so the raw
//! JPEG/PNG bytes can be cached on disk indefinitely — re-fetching the
//! same cover hammers the CDN for nothing and re-introduces the
//! track-change "stuck on old art" window while the network round-trips.
//! Keyed by [`crate::album_art::cache_key`] (the trailing hex of the
//! `i.scdn.co/image/<hex>` URL), which is filesystem-safe.
//!
//! Stored bytes are the *encoded* image (not decoded RGBA): smaller on
//! disk and the decode is cheap enough to redo on load. Entries carry an
//! mtime-based TTL and the directory is held under a rough LRU size cap;
//! both are enforced best-effort. Every operation degrades to a silent
//! no-op on any IO error — the cache is an optimisation, never a
//! correctness dependency, so a read-only disk or missing cache dir just
//! falls back to the network path.
//!
//! All functions block on the filesystem; call them from
//! `tokio::task::spawn_blocking`, never directly on an async task.

use std::fs;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use serde::Serialize;
use serde::de::DeserializeOwned;

/// Entries older than this (by mtime, refreshed on each read) are
/// treated as absent and deleted. Covers don't change, so this is long
/// — it exists to bound unbounded growth from one-off plays, not to
/// catch staleness.
const TTL: Duration = Duration::from_secs(60 * 60 * 24 * 30);

/// Soft ceiling on total cache size. When `write` pushes past this, the
/// oldest entries (by mtime) are evicted down to [`EVICT_TARGET`].
const MAX_BYTES: u64 = 256 * 1024 * 1024;

/// Evict down to this on overflow rather than exactly `MAX_BYTES` so we
/// don't repack on every single write once full.
const EVICT_TARGET: u64 = 200 * 1024 * 1024;

/// User-chosen cache root override. `None` → the OS cache dir. Set once
/// at startup from prefs via [`set_root`]; read under a lock so a settings
/// change can relocate the cache live.
static CACHE_ROOT: std::sync::RwLock<Option<PathBuf>> = std::sync::RwLock::new(None);

/// Override the cache root directory (the parent of `art/` + `json/`).
/// `None` restores the OS default. Pass an absolute directory.
pub fn set_root(dir: Option<PathBuf>) {
    if let Ok(mut g) = CACHE_ROOT.write() {
        *g = dir;
    }
}

/// The active cache root (`<override>/opal` or `<os-cache>/opal`).
fn root() -> Option<PathBuf> {
    let over = CACHE_ROOT.read().ok().and_then(|g| g.clone());
    match over {
        Some(d) => Some(d.join("opal")),
        None => dirs::cache_dir().map(|d| d.join("opal")),
    }
}

/// `<root>/art`, created on first use. `None` if no root or the directory
/// can't be created.
fn cache_dir() -> Option<PathBuf> {
    let dir = root()?.join("art");
    fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

/// `<root>/audio` — handed to librespot's `Cache` as its audio location,
/// so streamed tracks land under the same user-relocatable cache root as
/// art/json. librespot manages the contents itself (2-level fan-out dirs
/// with an LRU size cap that touches mtimes on read); we only report usage
/// and clear it. Created on first use.
pub fn audio_dir() -> Option<PathBuf> {
    let dir = root()?.join("audio");
    fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

/// Reject anything that isn't a bare filename (defence-in-depth against
/// path traversal — keys are hex / base62 / known sentinels in practice).
fn is_safe_key(key: &str) -> bool {
    !(key.is_empty() || key.contains(['/', '\\']) || key == "." || key == "..")
}

/// Map a cache key to its on-disk path under the art dir.
fn entry_path(key: &str) -> Option<PathBuf> {
    if !is_safe_key(key) {
        return None;
    }
    cache_dir().map(|d| d.join(key))
}

/// Whether a cache entry file exists for `key` (ignores TTL — used to
/// walk contiguous paginated entries when invalidating a collection).
pub fn exists(key: &str) -> bool {
    entry_path(key).map(|p| p.exists()).unwrap_or(false)
}

/// Disk key for a playlist/album detail listing. **Versioned**: bump the
/// prefix whenever `PlaylistTrack` grows fields the UI depends on (e.g.
/// the per-artist ids behind the clickable credit spans) — old-schema
/// entries deserialize with silent defaults, which shows up as features
/// working on freshly-cached pages and not on stale ones. A bump orphans
/// the old files (the json cache cap evicts them) and refetches honestly.
///
/// Lives here, next to the cache it names, so *every* holder of a detail
/// listing agrees on the key: the worker writes it, and the library slice
/// deletes it when a mutation makes the listing wrong.
pub fn detail_key(id: &str) -> String {
    format!("detail_v2_{id}")
}

/// Delete the **JSON** cache entry for `key` (best-effort) — the tier
/// `write_json` stores into. Mutating a resource server-side has to drop
/// this as well as any in-memory copy, or a restart inside the TTL reads
/// the pre-mutation listing straight back off disk.
pub fn remove_json(key: &str) {
    if let Some(path) = json_path(key) {
        let _ = fs::remove_file(path);
    }
}

/// Delete the cache entry for `key` (best-effort). Used to invalidate a
/// resource after we mutate it server-side, so the next read re-fetches.
pub fn remove(key: &str) {
    if let Some(path) = entry_path(key) {
        let _ = fs::remove_file(path);
    }
}

/// On-disk path for `key` if a (non-expired) entry exists — for
/// consumers that need a file path rather than bytes (e.g. an MP4 the
/// video decoder reads directly). Refreshes mtime like [`read`] so the
/// entry stays LRU-hot. `None` on miss / expiry / unsafe key.
pub fn path(key: &str) -> Option<PathBuf> {
    let path = entry_path(key)?;
    let meta = fs::metadata(&path).ok()?;
    let age = meta
        .modified()
        .ok()
        .and_then(|m| SystemTime::now().duration_since(m).ok())
        .unwrap_or(Duration::ZERO);
    if age > TTL {
        let _ = fs::remove_file(&path);
        return None;
    }
    if let Ok(f) = fs::OpenOptions::new().write(true).open(&path) {
        let _ = f.set_modified(SystemTime::now());
    }
    Some(path)
}

/// Read cached bytes for `key`, or `None` on miss / expiry / IO error.
/// On a hit the entry's mtime is refreshed so the LRU eviction in
/// [`write`] treats recently-used covers as hot.
pub fn read(key: &str) -> Option<Vec<u8>> {
    let path = entry_path(key)?;
    let meta = fs::metadata(&path).ok()?;
    let age = meta
        .modified()
        .ok()
        .and_then(|m| SystemTime::now().duration_since(m).ok())
        .unwrap_or(Duration::ZERO);
    if age > TTL {
        let _ = fs::remove_file(&path);
        return None;
    }
    let bytes = fs::read(&path).ok()?;
    // Touch mtime = "used now" for LRU. Best-effort; ignore failure.
    if let Ok(f) = fs::OpenOptions::new().write(true).open(&path) {
        let _ = f.set_modified(SystemTime::now());
    }
    Some(bytes)
}

/// Persist `bytes` for `key`, then enforce the size cap. Best-effort:
/// any IO failure is swallowed.
pub fn write(key: &str, bytes: &[u8]) {
    let Some(path) = entry_path(key) else { return };
    if fs::write(&path, bytes).is_err() {
        return;
    }
    enforce_cap();
}

/// If the cache directory exceeds [`MAX_BYTES`], delete oldest-by-mtime
/// entries until under [`EVICT_TARGET`].
fn enforce_cap() {
    let Some(dir) = cache_dir() else { return };
    let Ok(rd) = fs::read_dir(&dir) else { return };
    let mut entries: Vec<(PathBuf, SystemTime, u64)> = Vec::new();
    let mut total: u64 = 0;
    for e in rd.flatten() {
        let Ok(meta) = e.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        let len = meta.len();
        let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        total += len;
        entries.push((e.path(), mtime, len));
    }
    if total <= MAX_BYTES {
        return;
    }
    entries.sort_by_key(|(_, mtime, _)| *mtime); // oldest first
    for (path, _, len) in entries {
        if total <= EVICT_TARGET {
            break;
        }
        if fs::remove_file(&path).is_ok() {
            total = total.saturating_sub(len);
        }
    }
}

// ============================================================================
// API JSON cache
//
// Caches deserialized API payloads (playlist track listings, etc.) as
// JSON on disk, keyed by a caller-supplied id. Unlike the album-art
// bytes above (immutable → 30-day TTL, mtime-refreshed LRU), JSON
// listings are *mutable* — a user can edit a playlist — so the caller
// passes a much shorter TTL and reads do NOT touch mtime (an entry ages
// out from its original fetch time, never kept alive by re-reads).
// ============================================================================

/// Soft ceiling on the JSON cache dir; evict oldest past this.
const JSON_MAX_BYTES: u64 = 64 * 1024 * 1024;
const JSON_EVICT_TARGET: u64 = 48 * 1024 * 1024;

/// `<root>/json`, created on first use.
fn json_dir() -> Option<PathBuf> {
    let dir = root()?.join("json");
    fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

fn json_path(key: &str) -> Option<PathBuf> {
    if !is_safe_key(key) {
        return None;
    }
    json_dir().map(|d| d.join(format!("{key}.json")))
}

/// Read the raw cached JSON bytes for `key`, or `None` on miss / expiry
/// (older than `ttl`) / IO error. Does not refresh mtime — an entry ages
/// out from its original fetch time, never kept alive by re-reads. This is
/// the byte-level primitive the [`crate::api`] HTTP cache stores raw
/// responses through; [`read_json`] is the typed wrapper over it.
pub fn read_raw_json(key: &str, ttl: Duration) -> Option<Vec<u8>> {
    let path = json_path(key)?;
    let meta = fs::metadata(&path).ok()?;
    let age = meta
        .modified()
        .ok()
        .and_then(|m| SystemTime::now().duration_since(m).ok())
        .unwrap_or(Duration::ZERO);
    if age > ttl {
        let _ = fs::remove_file(&path);
        return None;
    }
    fs::read(&path).ok()
}

/// Persist raw `bytes` for `key`, then enforce the size cap. Best-effort:
/// any IO failure is swallowed.
pub fn write_raw_json(key: &str, bytes: &[u8]) {
    let Some(path) = json_path(key) else { return };
    if fs::write(&path, bytes).is_err() {
        return;
    }
    enforce_json_cap();
}

/// Read + deserialize a cached JSON value for `key`, or `None` on miss /
/// expiry (older than `ttl`) / IO / parse error. Does not refresh mtime.
pub fn read_json<T: DeserializeOwned>(key: &str, ttl: Duration) -> Option<T> {
    serde_json::from_slice(&read_raw_json(key, ttl)?).ok()
}

/// Serialize + persist `value` for `key`, then enforce the size cap.
/// Best-effort: any IO / serialize failure is swallowed.
pub fn write_json<T: Serialize>(key: &str, value: &T) {
    let Ok(bytes) = serde_json::to_vec(value) else {
        return;
    };
    write_raw_json(key, &bytes);
}

fn enforce_json_cap() {
    let Some(dir) = json_dir() else { return };
    evict_dir(&dir, JSON_MAX_BYTES, JSON_EVICT_TARGET);
}

/// Shared oldest-by-mtime eviction for a cache directory.
fn evict_dir(dir: &PathBuf, max_bytes: u64, target: u64) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    let mut entries: Vec<(PathBuf, SystemTime, u64)> = Vec::new();
    let mut total: u64 = 0;
    for e in rd.flatten() {
        let Ok(meta) = e.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        let len = meta.len();
        let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        total += len;
        entries.push((e.path(), mtime, len));
    }
    if total <= max_bytes {
        return;
    }
    entries.sort_by_key(|(_, mtime, _)| *mtime);
    for (path, _, len) in entries {
        if total <= target {
            break;
        }
        if fs::remove_file(&path).is_ok() {
            total = total.saturating_sub(len);
        }
    }
}

// ============================================================================
// Usage reporting + management (for the settings cache UI)
// ============================================================================

/// Filename prefix Canvas video files carry in the `art/` dir (`canvas_…`),
/// so usage can split them out from album-art bytes.
const CANVAS_PREFIX: &str = "canvas_";

/// On-disk byte usage per cache category, for the settings breakdown.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CacheUsage {
    /// Album-art image bytes (`art/`, non-`canvas_` files).
    pub art: u64,
    /// Canvas video bytes (`art/canvas_*`).
    pub canvas: u64,
    /// Cached API JSON + canvas metadata (`json/`).
    pub json: u64,
    /// Streamed-track audio bytes (`audio/`, librespot-managed).
    pub audio: u64,
}

impl CacheUsage {
    pub fn total(&self) -> u64 {
        self.art + self.canvas + self.json + self.audio
    }
}

/// Sum the byte size of every file directly under `dir`.
fn dir_bytes(dir: Option<PathBuf>) -> u64 {
    let Some(dir) = dir else { return 0 };
    let Ok(rd) = fs::read_dir(&dir) else { return 0 };
    rd.flatten()
        .filter_map(|e| e.metadata().ok())
        .filter(|m| m.is_file())
        .map(|m| m.len())
        .sum()
}

/// Sum file bytes under `dir` recursively — the audio cache fans files
/// out into 2-character subdirectories (librespot's layout).
fn dir_bytes_recursive(dir: Option<PathBuf>) -> u64 {
    let Some(dir) = dir else { return 0 };
    let Ok(rd) = fs::read_dir(&dir) else { return 0 };
    rd.flatten()
        .map(|e| match e.metadata() {
            Ok(m) if m.is_file() => m.len(),
            Ok(m) if m.is_dir() => dir_bytes_recursive(Some(e.path())),
            _ => 0,
        })
        .sum()
}

/// Split the `art/` dir into (album-art, Canvas-video) byte totals by the
/// `canvas_` filename prefix.
fn art_canvas_bytes(dir: Option<PathBuf>) -> (u64, u64) {
    let Some(dir) = dir else { return (0, 0) };
    let Ok(rd) = fs::read_dir(&dir) else {
        return (0, 0);
    };
    let (mut art, mut canvas) = (0u64, 0u64);
    for e in rd.flatten() {
        let Ok(meta) = e.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        let is_canvas = e
            .file_name()
            .to_str()
            .map(|n| n.starts_with(CANVAS_PREFIX))
            .unwrap_or(false);
        if is_canvas {
            canvas += meta.len();
        } else {
            art += meta.len();
        }
    }
    (art, canvas)
}

/// Current on-disk cache usage. Blocking (walks the dirs) — call from
/// `spawn_blocking`, not an async task or the UI hot path.
pub fn usage() -> CacheUsage {
    let (art, canvas) = art_canvas_bytes(cache_dir());
    CacheUsage {
        art,
        canvas,
        json: dir_bytes(json_dir()),
        audio: dir_bytes_recursive(audio_dir()),
    }
}

/// The active cache root directory (for display in settings). `None` if no
/// cache dir is available on this platform.
pub fn root_dir() -> Option<PathBuf> {
    root()
}

/// Delete every cached file (art + json + audio). Best-effort; returns
/// the number of bytes freed (a track currently streaming keeps its file
/// locked and survives — fine, it's still valid cache). Blocking — call
/// from `spawn_blocking`.
pub fn clear() -> u64 {
    let before = usage().total();
    for dir in [cache_dir(), json_dir()].into_iter().flatten() {
        if let Ok(rd) = fs::read_dir(&dir) {
            for e in rd.flatten() {
                if e.metadata().map(|m| m.is_file()).unwrap_or(false) {
                    let _ = fs::remove_file(e.path());
                }
            }
        }
    }
    // Audio fans out into subdirectories — clear those recursively.
    if let Some(dir) = audio_dir()
        && let Ok(rd) = fs::read_dir(&dir)
    {
        for e in rd.flatten() {
            let p = e.path();
            let _ = if p.is_dir() {
                fs::remove_dir_all(&p)
            } else {
                fs::remove_file(&p)
            };
        }
    }
    before.saturating_sub(usage().total())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Point the cache at a fresh temp dir for the length of one test.
    fn temp_root(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("opal-cache-test-{name}"));
        let _ = fs::remove_dir_all(&dir);
        set_root(Some(dir.clone()));
        dir
    }

    #[test]
    fn remove_json_drops_the_entry_a_mutation_invalidated() {
        let dir = temp_root("remove-json");
        let key = detail_key("37i9dQZF1DXcBWIGoYBM5M");
        write_json(&key, &vec!["a".to_string(), "b".to_string()]);
        assert!(
            read_json::<Vec<String>>(&key, Duration::MAX).is_some(),
            "written entry should read back"
        );
        remove_json(&key);
        assert!(
            read_json::<Vec<String>>(&key, Duration::MAX).is_none(),
            "invalidated entry must not survive — a restart would read it"
        );
        set_root(None);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn detail_key_is_stable_and_namespaced() {
        // The worker writes under this key and the library slice deletes
        // under it; they must agree exactly.
        assert_eq!(detail_key("abc"), "detail_v2_abc");
    }
}
