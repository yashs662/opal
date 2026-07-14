//! Playlist-membership slice — which of the user's playlists (plus Liked
//! Songs) contain a track.
//!
//! Spotify has no reverse lookup (track → playlists), so the worker scans
//! every editable playlist once, builds a `track_uri → [playlist_id]`
//! index, caches it to disk with a 6h TTL, and updates it incrementally on
//! add/remove. This model is the UI-facing *view*: the picker's playlist
//! list, the current track's membership (drives the heart + checkboxes),
//! and the picker popup state. The heavy index stays in the worker.

use std::collections::HashMap;
use std::collections::HashSet;

use opal_gfx::{Overlay, Signal, TextSignal};
use serde::{Deserialize, Serialize};

/// One editable playlist (picker row + name lookup).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MembershipPlaylist {
    pub id: String,
    pub name: String,
}

/// Disk-persisted membership snapshot — the worker's canonical copy. The UI
/// only ever sees `playlists` + per-track lookups derived from `index`.
#[derive(Default, Clone, Serialize, Deserialize)]
pub struct MembershipSnapshot {
    /// Editable playlists scanned, in library order.
    pub playlists: Vec<MembershipPlaylist>,
    /// `spotify:track:… → [playlist_id]` — which playlists contain a track.
    pub index: std::collections::HashMap<String, Vec<String>>,
    /// `artist_id → [track_uri]` — the reverse index that answers "which
    /// saved tracks are by this artist", built from the same scan so the
    /// artist page's library section is complete without opening each
    /// containing playlist. `default` so pre-`artist_index` caches still
    /// deserialize (a re-scan fills it).
    #[serde(default)]
    pub artist_index: std::collections::HashMap<String, Vec<String>>,
    /// `spotify:track:…` uris in Liked Songs — the liked half of the
    /// "saved by this artist" answer (the forward `index` is playlists
    /// only). Used by the artist page's library scan, not the playlist
    /// heart. `default` for older caches.
    #[serde(default)]
    pub liked: std::collections::HashSet<String>,
}

/// The track the picker popup acts on — a full row so an add can
/// live-patch open pages (title/cover/duration/artists), whatever
/// surface opened the picker (player-bar heart, a row heart, the
/// context menu's "Add to playlist…").
#[derive(Clone, Default)]
pub struct MembershipTarget {
    pub track: crate::api::PlaylistTrack,
}

impl MembershipTarget {
    /// `spotify:track:…` URI (playlist add/remove).
    pub fn uri(&self) -> &str {
        &self.track.uri
    }
    /// Bare hex id (Liked-Songs save/unsave).
    pub fn id(&self) -> &str {
        &self.track.id
    }
}

pub struct MembershipModel {
    /// Editable playlists — picker rows + id→name lookup. From the worker's
    /// `MembershipLoaded`.
    pub playlists: Vec<MembershipPlaylist>,
    /// `spotify:track:… → [playlist_id]` — editable playlist membership
    /// loaded from the worker's index cache.
    pub index: HashMap<String, Vec<String>>,
    /// `artist_id → [track_uri]` — reverse index for the artist page's
    /// "In your library" section (mirrors the worker snapshot).
    pub artist_index: HashMap<String, Vec<String>>,
    /// Every liked-song uri. With [`index`](Self::index) this is the single
    /// source of truth for "is this track saved" — see [`is_saved`].
    pub liked: HashSet<String>,
    /// Index loaded/built this session (picker shows a spinner until then).
    pub ready: bool,
    /// The **playing** track's playlist ids — drives the player-bar heart
    /// fill + tooltip (always tracks playback, independent of the picker).
    pub current: HashSet<String>,
    /// Playing track is in ≥1 playlist; combined with `liked` for the heart
    /// fill.
    pub in_playlist: Signal<bool>,
    /// Heart tooltip: the playlist names (+ "Liked Songs") the playing track
    /// belongs to, or empty.
    pub hint: TextSignal,
    /// The picker popup's scrim/fade/dismiss owner (same primitive as the
    /// devices / settings popups).
    pub overlay: Overlay,
    /// The track the open picker acts on — any track, not just the playing
    /// one (row hearts / context menu retarget it).
    pub target: MembershipTarget,
    /// The **target** track's playlist ids — the picker checkboxes. Kept
    /// separate from [`current`](Self::current) so pointing the picker at
    /// an arbitrary row never corrupts the player-bar heart.
    pub target_ids: HashSet<String>,
    /// The target track's Liked-Songs state (the picker's first checkbox).
    pub target_liked: bool,
    /// Target membership resolved (rows render greyed until the worker
    /// lookup answers — instant when the index is in memory).
    pub target_ready: bool,
}

impl MembershipModel {
    pub fn new() -> Self {
        Self {
            playlists: Vec::new(),
            index: HashMap::new(),
            artist_index: HashMap::new(),
            liked: HashSet::new(),
            ready: false,
            current: HashSet::new(),
            in_playlist: Signal::new(false),
            hint: TextSignal::new(""),
            overlay: Overlay::new(),
            target: MembershipTarget::default(),
            target_ids: HashSet::new(),
            target_liked: false,
            target_ready: false,
        }
    }

    /// Point the picker at a track (a like icon / menu item opened it).
    /// Membership + liked state re-resolve asynchronously; `seed` (the
    /// playing track's known state, when the target IS the playing track)
    /// makes the checkboxes correct on the very first paint.
    pub fn set_target(
        &mut self,
        track: crate::api::PlaylistTrack,
        seed: Option<(HashSet<String>, bool)>,
    ) {
        self.target = MembershipTarget { track };
        match seed {
            Some((ids, liked)) => {
                self.target_ids = ids;
                self.target_liked = liked;
                self.target_ready = true;
            }
            None => {
                self.target_ids = HashSet::new();
                self.target_liked = false;
                self.target_ready = false;
            }
        }
    }

    /// Apply the loaded/refreshed playlist list (the index landed).
    pub fn set_playlists(
        &mut self,
        playlists: Vec<MembershipPlaylist>,
        index: HashMap<String, Vec<String>>,
        artist_index: HashMap<String, Vec<String>>,
        liked: HashSet<String>,
    ) {
        self.playlists = playlists;
        self.index = index;
        self.artist_index = artist_index;
        self.liked = liked;
        self.ready = true;
    }

    /// **The** saved-state check every row heart queries: a track is saved
    /// iff it's liked or in ≥1 editable playlist. One source of truth so the
    /// heart reads identically on the playlist, album, and artist pages.
    pub fn is_saved(&self, uri: &str) -> bool {
        self.liked.contains(uri) || self.index.contains_key(uri)
    }

    /// Replace the current track's membership (from the worker's lookup) and
    /// refresh the derived heart state + tooltip.
    pub fn set_current(&mut self, ids: Vec<String>, liked: bool) {
        let set: HashSet<String> = ids.into_iter().collect();
        self.in_playlist.set(!set.is_empty());
        self.current = set;
        self.rebuild_hint(liked);
    }

    /// Optimistically flip one playlist's membership for the **target**
    /// track (the picker checkbox). The caller mirrors into
    /// [`toggle_current_local`](Self::toggle_current_local) when the target
    /// is also the playing track — this model doesn't know what's playing.
    pub fn toggle_target_local(&mut self, playlist_id: &str, add: bool) {
        if add {
            self.target_ids.insert(playlist_id.to_string());
        } else {
            self.target_ids.remove(playlist_id);
        }
    }

    /// Optimistically flip one playlist's membership for the **playing**
    /// track (the bar heart + tooltip).
    pub fn toggle_current_local(&mut self, playlist_id: &str, add: bool, liked: bool) {
        if add {
            self.current.insert(playlist_id.to_string());
        } else {
            self.current.remove(playlist_id);
        }
        self.in_playlist.set(!self.current.is_empty());
        self.rebuild_hint(liked);
    }

    /// Whether the target track is in playlist `id` (picker checkbox state).
    pub fn target_contains(&self, id: &str) -> bool {
        self.target_ids.contains(id)
    }

    /// Rebuild the heart tooltip from the current membership + liked flag.
    /// "Liked Songs, Chill, Focus" — or empty when in nothing.
    pub fn rebuild_hint(&self, liked: bool) {
        let mut parts: Vec<&str> = Vec::new();
        if liked {
            parts.push("Liked Songs");
        }
        for p in self.playlists.iter() {
            if self.current.contains(&p.id) {
                parts.push(p.name.as_str());
            }
        }
        self.hint.set(parts.join(", ").as_str());
    }
}

impl Default for MembershipModel {
    fn default() -> Self {
        Self::new()
    }
}
