//! User preferences — persisted across sessions as JSON in the OS
//! config directory.
//!
//! Schema is versioned (`version` field) so future migrations can detect
//! and adapt older files. Every field carries `#[serde(default)]` so
//! adding a new field is forward-compatible: an old preferences file
//! missing the field deserializes cleanly, the new field picks up its
//! Default value, and the next save writes the upgraded shape.
//!
//! Loading is fail-soft: any error (missing file, malformed JSON,
//! permission denied) yields [`UserPreferences::default`]. Saving is
//! best-effort — a write failure is logged but does not propagate.
//!
//! Scope today: panel sizes, window geometry, audio prefs. Extend by
//! adding a field-with-`#[serde(default)]` to [`UserPreferences`] or
//! one of its child structs; no migration needed for additive changes.

use std::fs;
use std::io;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Bump on any **incompatible** schema change (renamed fields, removed
/// fields with semantic load-bearers, changed types). Additive
/// changes don't need a bump — `#[serde(default)]` covers them.
pub const SCHEMA_VERSION: u32 = 1;

/// Top-level preferences. Every nested field defaults so partial /
/// older JSON files load cleanly.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct UserPreferences {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub panels: PanelPrefs,
    #[serde(default)]
    pub window: WindowPrefs,
    #[serde(default)]
    pub audio: AudioPrefs,
    /// Snapshot of the last track that was playing when the app exited.
    /// Restored on next launch as the "what was I listening to?" hint —
    /// populates the player chrome before any live cluster push lands so
    /// the UI isn't blank during the seconds between session-connect and
    /// the first dealer state. Overwritten the moment a real cluster
    /// update arrives.
    #[serde(default)]
    pub last_player: Option<StoredPlayer>,
    /// Show the looping Canvas video in the now-playing pane when a track
    /// has one. The playback pipeline isn't built yet; this persists the
    /// user's choice so it's honoured the moment canvas support lands.
    #[serde(default = "default_show_canvas")]
    pub show_canvas: bool,
    /// User-chosen cache directory (parent of `opal/art` + `json`).
    /// `None` = the OS cache dir. Lets the user relocate the on-disk cache
    /// (album art, Canvas videos, API JSON) to another drive/folder.
    #[serde(default)]
    pub cache_dir: Option<String>,
    /// The user's own Spotify app client id, used for the OAuth web flow.
    /// `None`/empty = not yet configured: the login view shows the setup
    /// screen (paste-field + dashboard instructions) instead of the
    /// "Log in" button. There is intentionally no bundled fallback — the
    /// consent screen names the registering app, so every user brings their
    /// own. See [`Self::client_id`].
    #[serde(default)]
    pub spotify_client_id: Option<String>,
    /// Recent searches (results the user opened) — powers the search modal's
    /// "Recent searches" list, persisted so it survives restarts.
    #[serde(default)]
    pub search_history: Vec<crate::model::search::SearchHistoryEntry>,
}

fn default_version() -> u32 {
    SCHEMA_VERSION
}

fn default_show_canvas() -> bool {
    true
}

impl Default for UserPreferences {
    fn default() -> Self {
        Self {
            version: SCHEMA_VERSION,
            panels: PanelPrefs::default(),
            window: WindowPrefs::default(),
            audio: AudioPrefs::default(),
            last_player: None,
            show_canvas: default_show_canvas(),
            cache_dir: None,
            spotify_client_id: None,
            search_history: Vec::new(),
        }
    }
}

/// Minimal snapshot of the live `CurrentlyPlaying` — just the fields
/// the player chrome reads. `is_playing` is intentionally **not**
/// persisted: the app can't keep playing while closed, and a stored
/// `true` would make the cold-start UI lie about playback state.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct StoredPlayer {
    pub track_id: String,
    pub name: String,
    pub artist: String,
    pub album_image_url: Option<String>,
    pub progress_ms: u64,
    pub duration_ms: u64,
    /// Playing context (`spotify:album:…`/`spotify:playlist:…`) at save time,
    /// so a cold-start resume restarts within it and keeps playing past the
    /// one track. `None` when nothing carried a context (e.g. self-play that
    /// never reported one — the worker then falls back to the track's album).
    #[serde(default)]
    pub context_uri: Option<String>,
    /// Context display name at save time — re-hydrates the queue-source
    /// label on cold start.
    #[serde(default)]
    pub context_name: Option<String>,
    /// First-artist id at save time — re-hydrates the "About the artist"
    /// section on cold start.
    #[serde(default)]
    pub artist_id: String,
    /// Every credited artist at save time — re-hydrates the clickable
    /// multi-artist lines on cold start.
    #[serde(default)]
    pub artists: Vec<crate::api::TrackArtist>,
}

/// Sidebar width in **logical** pixels (`0` = fully collapsed — the
/// splitter can re-open it) + the now-playing pane's visibility. The
/// now-playing pane has a fixed width; only whether it's shown persists.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct PanelPrefs {
    #[serde(default = "default_sidebar_w")]
    pub sidebar_w: f32,
    #[serde(default = "default_now_playing_open")]
    pub now_playing_open: bool,
}

fn default_sidebar_w() -> f32 {
    320.0
}
fn default_now_playing_open() -> bool {
    true
}

impl Default for PanelPrefs {
    fn default() -> Self {
        Self {
            sidebar_w: default_sidebar_w(),
            now_playing_open: default_now_playing_open(),
        }
    }
}

/// Snap a stored panel width into a known-good state. Defends against:
/// - hand-edited / corrupted JSON values outside `[min, max]`
/// - schema additions where `min`/`max` moved past an existing save
/// - off-by-one drift from float round-trips
///
/// Below the midpoint between `collapsed` and `min`, snap **down** to
/// `collapsed` (preserving the user's intent to hide the panel). Above
/// it, clamp to `[min, max]`. A panel without a collapsed state can pass
/// `collapsed = min` to disable the snap entirely.
pub fn clamp_panel_width(w: f32, min: f32, max: f32, collapsed: f32) -> f32 {
    let midpoint = (collapsed + min) * 0.5;
    if w < midpoint {
        collapsed
    } else {
        w.clamp(min, max)
    }
}

/// Last known window geometry — used to restore size + position on
/// launch. All fields optional; missing → winit picks a default.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct WindowPrefs {
    /// Logical-px inner size. `None` → fall back to the hardcoded
    /// default in `main.rs`.
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// Outer position in screen-space px. `None` → OS picks.
    pub x: Option<i32>,
    pub y: Option<i32>,
    #[serde(default)]
    pub maximized: bool,
}

/// Playback / audio preferences, applied at session start (the librespot
/// player + Connect device are built once per connect).
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct AudioPrefs {
    /// Master volume, 0.0..=1.0. Seeds the Connect device's advertised
    /// volume + the slider; updated from every volume confirmation.
    #[serde(default = "default_volume")]
    pub volume: f32,
    /// Bitrate tier — librespot streams 96 / 160 / 320 kbps OGG Vorbis.
    /// Changing it in settings applies from the next app start.
    #[serde(default)]
    pub quality: AudioQuality,
    /// Volume normalisation + peak limiter (Spotify's "Normalize volume").
    /// Off by default; when on, matches loudness across tracks and prevents
    /// loud masters from clipping. Applies from the next app start.
    #[serde(default = "default_normalize")]
    pub normalize: bool,
    /// 10-band graphic equaliser — enabled flag + per-band gains + saved
    /// custom presets. Applied live (see `audio_eq`), so a change takes
    /// effect immediately, not on next launch.
    #[serde(default)]
    pub eq: EqPrefs,
}

/// Persisted equaliser state. `bands` is the ten ISO-octave gains in dB
/// (see [`crate::audio_eq::BAND_FREQS`]); `custom` holds user-saved
/// presets by name.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct EqPrefs {
    #[serde(default)]
    pub enabled: bool,
    /// Per-band gains (dB). Missing/short arrays fall back to flat.
    #[serde(default)]
    pub bands: Vec<f32>,
    /// User-saved custom presets (name → ten band gains).
    #[serde(default)]
    pub custom: Vec<EqCustomPreset>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct EqCustomPreset {
    pub name: String,
    pub bands: Vec<f32>,
}

fn default_volume() -> f32 {
    0.8
}

fn default_normalize() -> bool {
    false
}

impl Default for AudioPrefs {
    fn default() -> Self {
        Self {
            volume: default_volume(),
            quality: AudioQuality::default(),
            normalize: default_normalize(),
            eq: EqPrefs::default(),
        }
    }
}

/// Streaming quality tier. Defaults to High (320 kbps — the ceiling any
/// third-party client can stream; lossless rides DRM librespot can't
/// decrypt). Low/Normal exist for constrained connections.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AudioQuality {
    Low,
    Normal,
    #[default]
    High,
}

impl UserPreferences {
    /// The configured Spotify client id, or `None` when unset/blank.
    /// Whitespace-only values count as unset so a stray space saved into
    /// the field doesn't masquerade as a real id.
    pub fn client_id(&self) -> Option<String> {
        self.spotify_client_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    }

    /// Read + parse the JSON file. Returns [`Self::default`] on any
    /// failure (missing file, malformed JSON, permission denied) so a
    /// fresh install / corrupted state always boots cleanly.
    pub fn load() -> Self {
        let Some(path) = preferences_path() else {
            return Self::default();
        };
        match fs::read_to_string(&path) {
            Ok(text) => match serde_json::from_str::<Self>(&text) {
                Ok(prefs) => {
                    log::info!("loaded user prefs from {}", path.display());
                    prefs
                }
                Err(e) => {
                    log::warn!(
                        "malformed prefs at {}: {e} — using defaults",
                        path.display()
                    );
                    Self::default()
                }
            },
            Err(e) if e.kind() == io::ErrorKind::NotFound => Self::default(),
            Err(e) => {
                log::warn!(
                    "failed to read prefs at {}: {e} — using defaults",
                    path.display()
                );
                Self::default()
            }
        }
    }

    /// Pretty-print to the on-disk JSON file. Creates the parent dir
    /// if missing. Best-effort — caller logs but does not propagate.
    pub fn save(&self) -> io::Result<()> {
        let Some(path) = preferences_path() else {
            return Err(io::Error::other("no config dir"));
        };
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| io::Error::other(format!("serialize: {e}")))?;
        fs::write(&path, json)?;
        Ok(())
    }
}

/// `<config_dir>/opal/preferences.json`. `None` if the OS doesn't
/// expose a config dir (extremely rare; e.g. some headless containers).
pub fn preferences_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("opal").join("preferences.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_round_trip_through_json() {
        let prefs = UserPreferences::default();
        let json = serde_json::to_string_pretty(&prefs).unwrap();
        let back: UserPreferences = serde_json::from_str(&json).unwrap();
        assert_eq!(back.version, SCHEMA_VERSION);
        assert_eq!(back.panels.sidebar_w, 320.0);
        assert_eq!(back.audio.quality, AudioQuality::High);
    }

    #[test]
    fn missing_fields_use_defaults() {
        // Old-shape file with only one nested field — additive forward
        // compat. Every other field falls back to its Default.
        let json = r#"{"panels": {"sidebar_w": 280.0}}"#;
        let prefs: UserPreferences = serde_json::from_str(json).unwrap();
        assert_eq!(prefs.panels.sidebar_w, 280.0);
        assert!(prefs.panels.now_playing_open, "default kicks in");
        assert_eq!(prefs.audio.volume, 0.8);
        assert_eq!(prefs.version, SCHEMA_VERSION);
    }

    #[test]
    fn empty_object_yields_full_defaults() {
        let prefs: UserPreferences = serde_json::from_str("{}").unwrap();
        assert_eq!(prefs.panels.sidebar_w, 320.0);
        assert_eq!(prefs.window.width, None);
        assert!(!prefs.window.maximized);
    }
}
