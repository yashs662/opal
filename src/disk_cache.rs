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
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
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
    if !is_fresh(&path, TTL) {
        return None;
    }
    // Touch mtime = "used now" for LRU. Best-effort; ignore failure.
    if let Ok(f) = fs::OpenOptions::new().write(true).open(&path) {
        let _ = f.set_modified(SystemTime::now());
    }
    Some(path)
}

/// Read cached bytes for `key`, or `None` on miss / expiry / IO error.
/// On a hit the entry's mtime is refreshed so the LRU eviction in
/// [`write`] treats recently-used covers as hot.
pub fn read(key: &str) -> Option<Vec<u8>> {
    fs::read(path(key)?).ok()
}

/// Persist `bytes` for `key`, then enforce the size cap. Best-effort:
/// any IO failure is swallowed.
pub fn write(key: &str, bytes: &[u8]) {
    let Some(path) = entry_path(key) else { return };
    store(&path, bytes, MAX_BYTES, EVICT_TARGET);
}

/// Whether `path` holds an entry younger than `ttl` (by mtime). An
/// expired entry is deleted on the way out, so a miss is always clean.
fn is_fresh(path: &Path, ttl: Duration) -> bool {
    let Ok(meta) = fs::metadata(path) else {
        return false;
    };
    if age(&meta) > ttl {
        let _ = fs::remove_file(path);
        return false;
    }
    true
}

/// Time since the file was last modified (zero when the mtime is
/// unreadable or in the future — treat it as just written).
fn age(meta: &fs::Metadata) -> Duration {
    meta.modified()
        .ok()
        .and_then(|m| SystemTime::now().duration_since(m).ok())
        .unwrap_or(Duration::ZERO)
}

/// Every regular file directly under `dir`, with its metadata. Empty when
/// the directory can't be read.
fn files(dir: &Path) -> impl Iterator<Item = (PathBuf, fs::Metadata)> {
    fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let meta = e.metadata().ok()?;
            meta.is_file().then(|| (e.path(), meta))
        })
}

/// The one write path for both cache tiers: land `bytes` at `path`
/// atomically, then hold the entry's directory under its size cap.
fn store(path: &Path, bytes: &[u8], max_bytes: u64, evict_target: u64) {
    if write_atomic(path, bytes).is_ok()
        && let Some(dir) = path.parent()
    {
        evict_dir(dir, max_bytes, evict_target);
    }
}

/// Distinguishes this process's concurrent temp files from one another.
static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// Suffix of [`write_atomic`]'s in-progress files. No cache key ends in it.
const TMP_SUFFIX: &str = ".tmp";

/// A temp file this old was orphaned by a crash mid-write (a live write
/// holds one for milliseconds); eviction sweeps it.
const TMP_ORPHAN_AGE: Duration = Duration::from_secs(60 * 60);

/// Replace the file at `path` with `bytes` in one step: write a sibling
/// temp file, then rename it over the target. A reader holding the path
/// sees the old contents or the new, never a truncated middle — a plain
/// `fs::write` truncates first, so a concurrent reader (the Canvas decode
/// thread opening a clip a second fetch is re-saving) got a partial file.
/// It also makes a crash mid-write leave the previous file intact.
///
/// The temp name is unique per process + call, so two writers of the same
/// key can't share one; the loser's rename simply lands second. It is
/// short and independent of the target's name, so a long key can't be
/// pushed past a path limit (Windows `MAX_PATH`, 255-byte names on macOS)
/// by the suffix. Same directory = same volume, which the rename needs on
/// every platform.
///
/// Windows refuses the replace while another handle has the target open
/// without delete-sharing (std's own readers share it; a scanner might
/// not). That surfaces as an `Err` with the old file untouched.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let tmp = path.with_file_name(format!(
        ".{}.{}{TMP_SUFFIX}",
        std::process::id(),
        TMP_SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    fs::write(&tmp, bytes)
        .and_then(|()| fs::rename(&tmp, path))
        .inspect_err(|_| {
            let _ = fs::remove_file(&tmp);
        })
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
    if !is_fresh(&path, ttl) {
        return None;
    }
    fs::read(&path).ok()
}

/// Persist raw `bytes` for `key`, then enforce the size cap. Best-effort:
/// any IO failure is swallowed.
pub fn write_raw_json(key: &str, bytes: &[u8]) {
    let Some(path) = json_path(key) else { return };
    store(&path, bytes, JSON_MAX_BYTES, JSON_EVICT_TARGET);
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

/// Whether `path` is one of [`write_atomic`]'s in-progress files.
fn is_tmp(path: &Path) -> bool {
    file_name_matches(path, |n| n.ends_with(TMP_SUFFIX))
}

/// Test `path`'s final component as UTF-8 (`false` when it isn't).
fn file_name_matches(path: &Path, pred: impl FnOnce(&str) -> bool) -> bool {
    path.file_name().and_then(|n| n.to_str()).is_some_and(pred)
}

/// Shared oldest-by-mtime eviction for a cache directory: past
/// `max_bytes`, delete oldest entries until under `target`. Also sweeps
/// temp files a crashed write left behind.
fn evict_dir(dir: &Path, max_bytes: u64, target: u64) {
    let mut entries: Vec<(PathBuf, SystemTime, u64)> = Vec::new();
    let mut total: u64 = 0;
    for (path, meta) in files(dir) {
        if is_tmp(&path) && age(&meta) > TMP_ORPHAN_AGE {
            let _ = fs::remove_file(&path);
            continue;
        }
        let len = meta.len();
        let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        total += len;
        entries.push((path, mtime, len));
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
    dir.map_or(0, |dir| files(&dir).map(|(_, meta)| meta.len()).sum())
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
    let (mut art, mut canvas) = (0u64, 0u64);
    for (path, meta) in files(&dir) {
        if file_name_matches(&path, |n| n.starts_with(CANVAS_PREFIX)) {
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
        for (path, _) in files(&dir) {
            let _ = fs::remove_file(path);
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

    /// A fresh, empty scratch dir that never touches the global root (so
    /// these run in parallel with the root-swapping test above).
    fn scratch_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("opal-cache-test-{name}"));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn write_atomic_replaces_and_leaves_no_temp_file() {
        let dir = scratch_dir("atomic-replace");
        let path = dir.join("entry");
        write_atomic(&path, b"first").unwrap();
        write_atomic(&path, b"second").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"second");
        let names: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .collect();
        assert_eq!(names, ["entry"], "temp file must not outlive the write");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn write_atomic_replaces_a_file_a_reader_holds_open() {
        // The Canvas case exactly: the decode thread has the clip open
        // while a second fetch re-saves it. The replace must land (POSIX
        // unlink semantics on macOS, delete-sharing handles on Windows)
        // and the open handle must keep reading the bytes it opened.
        use std::io::Read;
        let dir = scratch_dir("atomic-held-open");
        let path = dir.join("clip");
        write_atomic(&path, b"old clip").unwrap();
        let mut held = fs::File::open(&path).unwrap();
        write_atomic(&path, b"new clip").expect("replace under an open reader");
        let mut seen = Vec::new();
        held.read_to_end(&mut seen).unwrap();
        assert_eq!(seen, b"old clip");
        assert_eq!(fs::read(&path).unwrap(), b"new clip");
        drop(held);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn eviction_sweeps_orphaned_temp_files_only() {
        let dir = scratch_dir("orphan-sweep");
        let (orphan, live, entry) = (
            dir.join(format!(".1.0{TMP_SUFFIX}")),
            dir.join(format!(".1.1{TMP_SUFFIX}")),
            dir.join("entry"),
        );
        for p in [&orphan, &live, &entry] {
            fs::write(p, b"x").unwrap();
        }
        let old = SystemTime::now() - TMP_ORPHAN_AGE * 2;
        for p in [&orphan, &entry] {
            let f = fs::OpenOptions::new().write(true).open(p).unwrap();
            f.set_modified(old).unwrap();
        }
        evict_dir(&dir, u64::MAX, u64::MAX);
        assert!(!orphan.exists(), "stale temp file must be swept");
        assert!(live.exists(), "an in-progress write must survive");
        assert!(entry.exists(), "age alone never evicts a real entry");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn write_atomic_never_exposes_a_partial_file() {
        // A reader racing a re-save must get a whole payload every time —
        // the truncate-then-fill window is what broke Canvas decode.
        const LEN: usize = 4 * 1024 * 1024;
        let dir = scratch_dir("atomic-race");
        let path = dir.join("clip");
        write_atomic(&path, &vec![0u8; LEN]).unwrap();
        let writer = std::thread::spawn({
            let path = path.clone();
            move || {
                for fill in 1..=40u8 {
                    // A replace the OS refuses mid-read is dropped whole,
                    // which is still not a partial file.
                    let _ = write_atomic(&path, &vec![fill; LEN]);
                }
            }
        });
        while !writer.is_finished() {
            let bytes = fs::read(&path).expect("target must always exist");
            assert_eq!(bytes.len(), LEN, "reader saw a truncated file");
            assert!(
                bytes.iter().all(|b| *b == bytes[0]),
                "reader saw two writes interleaved"
            );
        }
        writer.join().unwrap();
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn detail_key_is_stable_and_namespaced() {
        // The worker writes under this key and the library slice deletes
        // under it; they must agree exactly.
        assert_eq!(detail_key("abc"), "detail_v2_abc");
    }
}
