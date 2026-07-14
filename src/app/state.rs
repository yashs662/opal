//! `AppState` — the shared application state: the composition of every
//! domain model. Held behind an `Rc` and handed to the views (which read
//! their slices) and the app controllers (frame tick + reducer, which
//! mutate them). This is the "minimal shared state" `main` owns; the
//! per-domain logic lives on the models themselves.

use crate::model::{
    ArtModel, AuthModel, BackdropModel, CanvasModel, DevicesModel, EqModel, LibraryModel,
    MembershipModel, MenuModel, PlayerModel, PrefsModel, RouterModel, SettingsModel,
};
use crate::prefs::UserPreferences;

pub struct AppState {
    /// View-routing slice: top-level view + centre-pane nav + entrance
    /// transition tween.
    pub router: RouterModel,
    /// Live OAuth session + token accessor.
    pub auth: AuthModel,
    /// Library slice: Home feed data, the open centre-pane playlist (live
    /// streaming buffer), playlist TTL cache + in-flight gate.
    pub library: LibraryModel,
    /// Spotify Canvas slice: cached clip path, off-thread decode session,
    /// frame sink + live target node, dim/hover overlay, `show_canvas`.
    pub canvas: CanvasModel,
    /// Art-resolution cache: shared per-URL cover handles, in-flight gate,
    /// cover→accent cache, currently-shown key, `/v1/tracks/{id}` cache.
    pub art: ArtModel,
    /// Album-art backdrop + accent crossfade slice.
    pub backdrop: BackdropModel,
    /// Reactive player-chrome slice (title/artist/transport/progress +
    /// authoritative snapshot).
    pub player_ui: PlayerModel,
    /// Settings-modal slice: the `Overlay`, cache usage, dir-picker handoff.
    pub settings: SettingsModel,
    /// Connect-devices slice: the devices popup + active-device chrome.
    pub devices: DevicesModel,
    /// Right-click context-menu slice (track row actions).
    pub menu: MenuModel,
    /// Playlist-membership slice: which playlists contain a track + the
    /// heart picker popup.
    pub membership: MembershipModel,
    /// Persisted-preferences slice + panel widths + debounced save.
    pub prefs: PrefsModel,
    /// 10-band equaliser slice: the reactive slider mirror + the shared
    /// lock-free control surface the audio sink reads.
    pub eq: EqModel,
}

impl AppState {
    pub fn from_prefs(prefs: UserPreferences) -> Self {
        // Seed the player chrome from the persisted snapshot so cold start
        // renders the last-played track immediately instead of a dash. The
        // first live cluster push overwrites these; if Spotify has nothing
        // playing on launch, the snapshot stays visible.
        let (title, artist, progress, progress_ms, duration_ms) = match prefs.last_player.as_ref() {
            Some(p) => {
                let frac = if p.duration_ms > 0 {
                    (p.progress_ms as f32 / p.duration_ms as f32).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                (
                    p.name.as_str(),
                    p.artist.as_str(),
                    frac,
                    p.progress_ms,
                    p.duration_ms,
                )
            }
            None => ("\u{2014}", "", 0.0, 0, 0),
        };
        // The restored track, as a paused snapshot — so it counts as the
        // *current* track for the heart (liked + playlist membership), the
        // canvas, and resume, not just the chrome labels. Without this the
        // membership/liked checks (which key off the snapshot) never fire on
        // cold start, leaving the heart blank until the track actually
        // changes. The first live cluster push overwrites it. Built here and
        // handed to `PlayerModel::seed` so the field is set at construction
        // (no post-init `borrow_mut` on the live cell).
        let restored = prefs
            .last_player
            .as_ref()
            .map(|p| crate::api::CurrentlyPlaying {
                track_id: p.track_id.clone(),
                name: p.name.clone(),
                artist: p.artist.clone(),
                artist_id: p.artist_id.clone(),
                artists: p.artists.clone(),
                album_image_url: p.album_image_url.clone(),
                is_playing: false,
                progress_ms: p.progress_ms,
                progress_anchor: std::time::Instant::now(),
                duration_ms: p.duration_ms,
                shuffle: false,
                repeat: crate::api::RepeatMode::Off,
                context_uri: p.context_uri.clone(),
                context_name: p.context_name.clone(),
            });
        Self {
            router: RouterModel::new(),
            auth: AuthModel::new(),
            library: LibraryModel::new(),
            canvas: CanvasModel::new(prefs.show_canvas),
            art: ArtModel::new(),
            backdrop: BackdropModel::new(),
            player_ui: PlayerModel::seed(
                title,
                artist,
                progress,
                progress_ms,
                duration_ms,
                prefs.audio.volume,
                restored,
            ),
            settings: SettingsModel::new(prefs.audio.normalize),
            devices: DevicesModel::new(),
            menu: MenuModel::new(),
            membership: MembershipModel::new(),
            eq: EqModel::from_prefs(&prefs.audio.eq),
            prefs: PrefsModel::new(prefs),
        }
    }
}
