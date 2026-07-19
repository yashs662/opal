use crate::album_art;
use crate::api::{self, CurrentlyPlaying, HomeData, RepeatMode, TrackDetails};
use crate::auth::oauth::{self, SpotifyAuthResponse, listen_for_callback, refresh_token};
use crate::auth::token_manager::{self, StoredTokens};
use crate::disk_cache;
use crate::errors::AuthError;
use crate::extracted_color;
use crate::widgets::{color, tokens};
use crate::{cluster_listener, spirc_bootstrap, spotify_session};
use librespot_connect::{LoadRequest, LoadRequestOptions, PlayingTrack, Spirc};
use librespot_core::Session;
use librespot_core::authentication::Credentials;
use librespot_protocol::extended_metadata::{BatchedEntityRequest, EntityRequest, ExtensionQuery};
use librespot_protocol::extension_kind::ExtensionKind;
use log::{debug, error, info, warn};
use opal_gfx::{ImageHandle, Uploader, WakeHandle};
use protobuf::EnumOrUnknown;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread;
use tokio::sync::Mutex as AsyncMutex;
use tokio::sync::mpsc::{self as tmpsc, UnboundedSender};

/// Cap the longest side of decoded album art. The 2048² atlas holds
/// Decoded RGBA cap per cover. Matches Spotify's largest variant
/// (640) — keeps the now-playing pane (~308 px logical, ~616 px @2×
/// DPI) and full-window backdrop sharp. Atlas headroom comes from
/// the 4096² image atlas in opal-gfx, which fits ~40 covers at
/// this size.
const ALBUM_ART_MAX_DIM: u32 = 640;

#[derive(Debug)]
pub enum WorkerCommand {
    /// Begin the OAuth web flow with the user's configured client id.
    StartOAuth {
        client_id: String,
    },
    /// Load stored tokens on launch; `client_id` is needed only to refresh
    /// an expired pair (empty when the user hasn't configured one yet —
    /// the refresh then fails and we fall through to the login screen).
    TryLoadTokens {
        client_id: String,
    },
    FetchHome {
        access_token: String,
    },
    /// One-shot `/v1/me/player` poll to seed the initial player state.
    /// Dealer cluster pushes only on transitions — without this seed,
    /// the UI is blank from launch until the user toggles play/pause
    /// on whatever device is active.
    SeedPlayerState {
        access_token: String,
    },
    FetchTrackDetails {
        access_token: String,
        track_id: String,
    },
    /// Load a playlist's tracks (or the Liked Songs collection when
    /// `liked` is set). Result flows back as `PlaylistLoaded`.
    FetchPlaylist {
        access_token: String,
        id: String,
        liked: bool,
    },
    /// Load an album's tracks. Result flows back as `PlaylistOpened`
    /// (albums reuse the playlist track-list pipeline).
    FetchAlbum {
        access_token: String,
        id: String,
    },
    /// Load an artist page (profile + discography). Result: `ArtistOpened`.
    FetchArtist {
        access_token: String,
        id: String,
    },
    /// Lightweight artist profile only (`/v1/artists/{id}`) — the
    /// now-playing "About the artist" card, not the full artist page.
    FetchArtistCard {
        access_token: String,
        id: String,
    },
    /// Resolve a playing context's display name via the Web API — the
    /// fallback for player states that carry no `context_description`
    /// (the `/me/player` cold-start seed, our own local playback).
    /// Result: `ContextNameReady`.
    FetchContextName {
        access_token: String,
        uri: String,
    },
    /// Resolve a batch of Recents session contexts (name + owner + cover).
    /// Result: `ContextsResolved`.
    ResolveContexts {
        access_token: String,
        uris: Vec<String>,
    },
    /// Full-text search. Result: `SearchResults` (tagged with the query so a
    /// late response for a since-changed query can be dropped).
    Search {
        access_token: String,
        query: String,
    },
    /// Track credits (performers / writers / producers) via the spclient
    /// track-credits endpoint — the now-playing card's "Credits" section.
    /// Needs a live session. Result: `TrackCreditsReady`.
    FetchTrackCredits {
        track_id: String,
    },
    /// Resolve display metadata (title / artists / duration / cover) for
    /// tracks the cluster shipped as bare uris — one batched `TRACK_V4`
    /// extended-metadata request, like the official client hydrates its
    /// queue. Needs a live session. Result: `TracksHydrated`.
    HydrateTracks {
        uris: Vec<String>,
    },
    FetchAlbumArt {
        url: String,
        key: String,
    },
    /// Fetch Spotify's own extracted accent colour for a cover, via the
    /// librespot session's extended-metadata endpoint. `image_hex` is the
    /// `i.scdn.co/image/<hex>` trailing hash (our cache key).
    FetchAccent {
        image_hex: String,
    },
    /// Fetch the Spotify Canvas (looping video) URL for a track via the
    /// librespot extended-metadata endpoint (`CANVAZ`). `track_uri` is the
    /// `spotify:track:…` form; `track_id` echoes back so the UI can
    /// confirm the response still matches the current track.
    FetchCanvas {
        track_uri: String,
        track_id: String,
    },
    ConnectSpotifySession {
        access_token: String,
        /// Persisted volume preference (0..=1) — the Connect device's
        /// advertised initial volume, so a transfer to Opal doesn't
        /// snap the user back to librespot's 50% default.
        initial_volume: f32,
        /// Persisted streaming-quality preference → librespot bitrate.
        quality: crate::prefs::AudioQuality,
        /// Persisted "normalize volume" preference → librespot
        /// normalisation + limiter.
        normalize: bool,
    },
    /// Transport control on the active Connect device. `local` (Opal is
    /// the active device) drives our own Spirc directly — instant + reliable;
    /// the Web API relay to our own device can silently no-op after a long
    /// uptime. Otherwise the Web API acts on the remote device.
    Playback {
        access_token: String,
        cmd: PlaybackCmd,
        local: bool,
    },
    /// Claim playback on Opal **paused** at `position_ms`, after the active
    /// remote device vanished mid-track. Drives our own Spirc `load` with
    /// `start_playing: false` so Opal becomes the active Connect device with
    /// the track loaded but held — the user resumes when they choose.
    /// `context_uri` is the playlist/album to load (so next/prev work); when
    /// `None` we load the single track.
    ClaimPlaybackPaused {
        context_uri: Option<String>,
        track_uri: String,
        position_ms: u32,
    },
    /// Skip forward `count` tracks — "skip to" a queue item (playing the
    /// N-th queued song consumes the ones before it). `local` uses our
    /// Spirc handle (instant, reliable) when Opal is the active
    /// device; otherwise it falls back to repeated Web API `next`.
    SkipForward {
        access_token: String,
        count: u32,
        local: bool,
    },
    /// Proactively refresh the access token before it expires (dispatched
    /// by the frame tick's due-check; see `AuthModel::refresh_due`).
    RefreshTokens {
        refresh_token: String,
        client_id: String,
    },
    /// List Connect devices for the devices popup.
    FetchDevices {
        access_token: String,
    },
    /// Transfer playback to a device (and resume there). `position_ms` is
    /// `Some` only when leaving Opal itself: the Web API transfer drops
    /// our librespot device's position (the target restarts at 0:00), so we
    /// re-apply the position we track locally — see `spawn_transfer`.
    TransferPlayback {
        access_token: String,
        device_id: String,
        position_ms: Option<u32>,
    },
    /// Is the current track liked? (heart state on track change)
    CheckSaved {
        access_token: String,
        track_id: String,
    },
    /// Like / unlike a track. `artist_ids` keep the membership snapshot's
    /// reverse index current so the artist page reflects the change on its
    /// next open (no full re-scan).
    SetSaved {
        access_token: String,
        track_id: String,
        artist_ids: Vec<String>,
        saved: bool,
    },
    /// The active device's play queue, for the queue page.
    FetchQueue {
        access_token: String,
    },
    /// Append a track to the active device's queue (right-click → Add to
    /// queue). Works on remote devices too.
    AddToQueue {
        access_token: String,
        uri: String,
    },
    /// Build (or load from disk) the playlist-membership index — scans all
    /// editable playlists once, caches 6h. The heavy index lives on the
    /// worker; the UI gets the playlist list + per-track lookups.
    LoadMembership {
        access_token: String,
    },
    /// Look up which playlists the given track is in (against the loaded
    /// index) → `TrackMembership`.
    QueryMembership {
        track_uri: String,
    },
    /// Add/remove a track to/from a playlist, then update the index + disk
    /// cache. `add=false` removes.
    EditMembership {
        access_token: String,
        playlist_id: String,
        track_uri: String,
        artist_ids: Vec<String>,
        add: bool,
    },
}

/// A transport intent dispatched from a player-bar button. Resolved to
/// the matching `api::*` call on the worker; the resulting state change
/// flows back through the dealer cluster subscription, not a direct
/// response, so the UI updates via the same path as remote changes.
#[derive(Debug, Clone)]
pub enum PlaybackCmd {
    Play,
    Pause,
    Next,
    Prev,
    Shuffle(bool),
    Repeat(RepeatMode),
    /// Set the active device's volume (0..=100).
    Volume(u8),
    /// Seek the active device to an absolute position (ms).
    Seek(u32),
    /// Start a playlist/album context (or explicit track list) at an
    /// offset on the active device.
    PlayContext(api::PlayTarget),
}

#[derive(Debug, Clone)]
pub enum WorkerResponse {
    /// A transport command failed for good (after the claim-on-Opal
    /// fallback). The UI rolls back its optimistic toggle flips to the
    /// authoritative snapshot.
    PlaybackFailed {
        cmd: PlaybackCmd,
    },
    /// The active device's volume changed (local mixer event or cluster
    /// push), 0..=1.
    VolumeChanged {
        fraction: f32,
    },
    /// Proactive token refresh succeeded — swap the live token only (no
    /// re-fetch / session reconnect like `TokensLoaded`).
    TokensRefreshed {
        auth: SpotifyAuthResponse,
    },
    /// Proactive token refresh failed — the auth model backs off and the
    /// due-check retries shortly.
    TokensRefreshFailed,
    /// Connect device list (devices popup).
    Devices {
        devices: Vec<api::Device>,
    },
    /// The cluster's active device changed; `is_self` = Opal is it.
    ActiveDeviceChanged {
        device_id: String,
        is_self: bool,
    },
    /// The globally-active device dropped off the cluster mid-playback (its
    /// app was quit) with nothing taking over. If we were showing it
    /// playing, the host claims playback on Opal at the last position —
    /// otherwise the transport would freeze, dead and unresponsive.
    ActiveDeviceVanished,
    /// Liked-state of a track (heart) — both the on-track-change check
    /// and the authoritative echo after a save/unsave (a failed write
    /// sends the rolled-back value).
    SavedState {
        track_id: String,
        saved: bool,
    },
    /// The active device's queue (currently playing first).
    QueueLoaded {
        tracks: Vec<api::PlaylistTrack>,
    },
    OAuthStarted {
        auth_url: String,
    },
    OAuthComplete {
        auth: SpotifyAuthResponse,
    },
    OAuthFailed {
        error: String,
    },
    TokensLoaded {
        auth: SpotifyAuthResponse,
    },
    NoStoredTokens,
    HomeData {
        data: HomeData,
    },
    PlayerState {
        player: Option<CurrentlyPlaying>,
    },
    TrackDetails {
        details: TrackDetails,
    },
    /// First response for a playlist open: metadata + the first track
    /// page (or the *full* set when `complete`, e.g. a disk-cache hit or
    /// single-page playlist). The UI rebuilds once here to mount the
    /// header + full-length virtualised list.
    PlaylistOpened {
        detail: api::PlaylistDetail,
        complete: bool,
    },
    /// A subsequent streamed track page appended to the open playlist's
    /// live buffer — no rebuild; the virtualised list reads it on scroll.
    PlaylistTracks {
        id: String,
        tracks: Vec<api::PlaylistTrack>,
        done: bool,
    },
    PlaylistFailed {
        id: String,
        error: String,
    },
    /// An artist page resolved: profile + popular tracks + discography +
    /// the user's saved songs by this artist across the whole library
    /// (Liked Songs + every membership-indexed playlist), each with the
    /// names of the sources that contain it.
    ArtistOpened {
        id: String,
        name: String,
        image_url: Option<String>,
        followers: u64,
        top_tracks: Vec<api::PlaylistTrack>,
        albums: Vec<api::AlbumRef>,
        library_tracks: Vec<(api::PlaylistTrack, Vec<String>)>,
    },
    ArtistFailed {
        id: String,
        error: String,
    },
    /// The now-playing "About the artist" profile resolved. `bio` is the
    /// artist's extended-metadata biography (plain-ish text; may embed
    /// simple HTML tags Spotify ships, stripped worker-side), `None` when
    /// the artist has none or no session was up.
    ArtistCardReady {
        detail: api::ArtistDetail,
        bio: Option<String>,
    },
    /// A context uri's display name resolved (or `None` — unfetchable
    /// kind / request failed; the caller keeps its kind label and won't
    /// re-ask for this uri).
    ContextNameReady {
        uri: String,
        name: Option<String>,
    },
    /// Recents session contexts resolved (uri → name/owner/cover). Missing
    /// uris (unresolvable) are simply absent from the map.
    ContextsResolved {
        map: std::collections::HashMap<String, api::RecentContextInfo>,
    },
    /// Search results for `query` (empty on failure). The reducer drops it
    /// if the live query has moved on.
    SearchResults {
        query: String,
        results: api::SearchResults,
    },
    /// A track's credits resolved (possibly empty — endpoint had none).
    TrackCreditsReady {
        track_id: String,
        credits: TrackCredits,
    },
    /// Bare queue uris resolved to full rows (order = request order;
    /// tracks the batch couldn't resolve are absent).
    TracksHydrated {
        tracks: Vec<api::PlaylistTrack>,
    },
    AlbumArtReady {
        key: String,
        handle: ImageHandle,
        accent: [f32; 4],
        /// Mean luminance of the cover — how bright its blurred ambient
        /// backdrop reads; drives the adaptive glass dim.
        luma: f32,
    },
    AlbumArtFailed {
        key: String,
    },
    /// Spotify's extracted accent for a cover. `key` is the image hex so
    /// the UI can confirm it still matches the current track before
    /// applying it.
    AccentReady {
        key: String,
        accent: [f32; 4],
    },
    /// A track's Canvas video resolved (URL fetched + MP4 downloaded to
    /// the disk cache). `track_id` lets the UI confirm it still matches
    /// the current track; `path` is the cached MP4 ready to decode.
    CanvasReady {
        track_id: String,
        path: std::path::PathBuf,
    },
    /// No Canvas for the track (or fetch/download failed) — UI keeps the
    /// album art.
    CanvasNone {
        track_id: String,
    },
    SpotifySessionConnected {
        /// Opal's own librespot device id — the devices popup tags
        /// this row "This device".
        device_id: String,
    },
    SpotifySessionFailed {
        error: String,
    },
    /// The librespot `spirc_task` ended (dealer/AP socket dropped on a long
    /// session) — the Connect device is offline. The reducer reconnects with
    /// a fresh token; the worker already backed off before sending this.
    SpotifySessionLost,
    /// The playlist-membership index is ready — carries the editable
    /// playlist list (id + name) for the heart picker.
    MembershipLoaded {
        playlists: Vec<crate::model::membership::MembershipPlaylist>,
        index: std::collections::HashMap<String, Vec<String>>,
        artist_index: std::collections::HashMap<String, Vec<String>>,
        liked: std::collections::HashSet<String>,
    },
    /// Which playlists the given track is in (answer to `QueryMembership`).
    TrackMembership {
        track_uri: String,
        playlist_ids: Vec<String>,
    },
    /// An `EditMembership` failed — roll the optimistic checkbox back.
    MembershipEditFailed {
        track_uri: String,
        playlist_id: String,
        /// The edit that failed was an add (`true`) or remove (`false`).
        was_add: bool,
    },
}

pub struct Worker {
    cmd_tx: UnboundedSender<WorkerCommand>,
    resp_rx: Receiver<WorkerResponse>,
}

#[derive(Clone)]
struct Responder {
    tx: Sender<WorkerResponse>,
    wake: Arc<WakeHandle>,
}

impl Responder {
    fn send(&self, r: WorkerResponse) {
        let _ = self.tx.send(r);
        self.wake.wake();
    }
}

impl Worker {
    pub fn new(
        wake: Arc<WakeHandle>,
        uploader: Arc<Uploader>,
        eq: Arc<crate::audio_eq::EqShared>,
    ) -> Self {
        let (cmd_tx, mut cmd_rx) = tmpsc::unbounded_channel::<WorkerCommand>();
        let (resp_tx, resp_rx): (Sender<WorkerResponse>, Receiver<WorkerResponse>) = channel();
        let resp = Responder { tx: resp_tx, wake };

        thread::spawn(move || {
            // Bound the blocking pool: every disk-cache read/write and
            // cover decode rides `spawn_blocking`, and tokio's default cap
            // is 512 threads — a cold start (hundreds of covers + library
            // scans at once) balloons to ~570 OS threads and saturates
            // every core. CPU-bound decodes gain nothing past core count;
            // excess work queues instead of spawning.
            let blocking_cap = std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(8)
                .clamp(4, 16);
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .max_blocking_threads(blocking_cap)
                .build()
                .unwrap();
            // Long-lived librespot session — held on the worker so its
            // background tasks (AP socket, dealer) stay alive across
            // command iterations. `None` until `ConnectSpotifySession`
            // succeeds for the current login.
            let session: Arc<AsyncMutex<Option<Session>>> = Arc::new(AsyncMutex::new(None));
            // Spirc handle — dropping this disconnects the Connect device.
            // Held on the worker for lifetime parity with `session`.
            let spirc: Arc<AsyncMutex<Option<Spirc>>> = Arc::new(AsyncMutex::new(None));
            // The queue-context uri Opal last loaded as the active local
            // device. librespot `PlayerEvent`s don't carry the context, and the
            // dealer never echoes our own state — so without this the source
            // pill is blank while Opal is the player. Written on a local
            // `PlayContext`, stamped onto local player states, cleared when a
            // remote device takes over. A plain std mutex: the critical
            // sections are a single clone/store.
            let local_ctx: Arc<std::sync::Mutex<Option<String>>> =
                Arc::new(std::sync::Mutex::new(None));
            // Canonical playlist-membership index (track→playlists). Lives
            // here so the heavy map never crosses to the UI; the UI gets the
            // playlist list + per-track lookups + edit confirmations.
            let membership: Arc<AsyncMutex<crate::model::membership::MembershipSnapshot>> =
                Arc::new(AsyncMutex::new(Default::default()));
            // The in-flight OAuth task (callback listener on 127.0.0.1:8888).
            // A retry (e.g. after a wrong client id) must abort the previous
            // one first — otherwise the stale listener keeps the port and
            // steals the new callback, doing the token exchange with the old
            // verifier/client id → a bogus "login failed".
            let mut oauth_task: Option<tokio::task::JoinHandle<()>> = None;
            rt.block_on(async move {
                while let Some(cmd) = cmd_rx.recv().await {
                    match cmd {
                        WorkerCommand::StartOAuth { client_id } => {
                            if let Some(h) = oauth_task.take() {
                                h.abort();
                                // Await teardown so the old listener's socket
                                // is fully dropped before we rebind the port.
                                let _ = h.await;
                            }
                            oauth_task = Some(spawn_oauth(resp.clone(), client_id));
                        }
                        WorkerCommand::TryLoadTokens { client_id } => {
                            spawn_try_load(resp.clone(), client_id)
                        }
                        WorkerCommand::FetchHome { access_token } => {
                            spawn_fetch_home(resp.clone(), access_token)
                        }
                        WorkerCommand::SeedPlayerState { access_token } => {
                            spawn_seed_player(resp.clone(), access_token)
                        }
                        WorkerCommand::FetchTrackDetails {
                            access_token,
                            track_id,
                        } => spawn_fetch_track_details(resp.clone(), access_token, track_id),
                        WorkerCommand::FetchPlaylist {
                            access_token,
                            id,
                            liked,
                        } => spawn_fetch_playlist(resp.clone(), access_token, id, liked),
                        WorkerCommand::FetchAlbum { access_token, id } => {
                            spawn_fetch_album(resp.clone(), access_token, id)
                        }
                        WorkerCommand::FetchArtist { access_token, id } => {
                            spawn_fetch_artist(resp.clone(), session.clone(), access_token, id)
                        }
                        WorkerCommand::FetchArtistCard { access_token, id } => {
                            spawn_fetch_artist_card(resp.clone(), session.clone(), access_token, id)
                        }
                        WorkerCommand::FetchContextName { access_token, uri } => {
                            spawn_fetch_context_name(resp.clone(), access_token, uri)
                        }
                        WorkerCommand::ResolveContexts { access_token, uris } => {
                            spawn_resolve_contexts(resp.clone(), access_token, uris)
                        }
                        WorkerCommand::Search {
                            access_token,
                            query,
                        } => spawn_search(resp.clone(), access_token, query),
                        WorkerCommand::FetchTrackCredits { track_id } => {
                            spawn_fetch_track_credits(resp.clone(), session.clone(), track_id)
                        }
                        WorkerCommand::HydrateTracks { uris } => {
                            spawn_hydrate_tracks(resp.clone(), session.clone(), uris)
                        }
                        WorkerCommand::FetchAlbumArt { url, key } => {
                            spawn_fetch_album_art(resp.clone(), uploader.clone(), url, key)
                        }
                        WorkerCommand::FetchAccent { image_hex } => {
                            spawn_fetch_accent(resp.clone(), session.clone(), image_hex)
                        }
                        WorkerCommand::FetchCanvas {
                            track_uri,
                            track_id,
                        } => spawn_fetch_canvas(resp.clone(), session.clone(), track_uri, track_id),
                        WorkerCommand::ConnectSpotifySession {
                            access_token,
                            initial_volume,
                            quality,
                            normalize,
                        } => spawn_connect_session(
                            resp.clone(),
                            session.clone(),
                            spirc.clone(),
                            local_ctx.clone(),
                            access_token,
                            initial_volume,
                            quality,
                            normalize,
                            eq.clone(),
                        ),
                        WorkerCommand::Playback {
                            access_token,
                            cmd,
                            local,
                        } => spawn_playback(
                            resp.clone(),
                            session.clone(),
                            spirc.clone(),
                            local_ctx.clone(),
                            access_token,
                            cmd,
                            local,
                        ),
                        WorkerCommand::ClaimPlaybackPaused {
                            context_uri,
                            track_uri,
                            position_ms,
                        } => spawn_claim_paused(spirc.clone(), context_uri, track_uri, position_ms),
                        WorkerCommand::SkipForward {
                            access_token,
                            count,
                            local,
                        } => spawn_skip_forward(spirc.clone(), access_token, count, local),
                        WorkerCommand::RefreshTokens {
                            refresh_token,
                            client_id,
                        } => spawn_refresh_tokens(resp.clone(), refresh_token, client_id),
                        WorkerCommand::FetchDevices { access_token } => {
                            spawn_fetch_devices(resp.clone(), access_token)
                        }
                        WorkerCommand::TransferPlayback {
                            access_token,
                            device_id,
                            position_ms,
                        } => spawn_transfer(access_token, device_id, position_ms),
                        WorkerCommand::CheckSaved {
                            access_token,
                            track_id,
                        } => spawn_check_saved(resp.clone(), access_token, track_id),
                        WorkerCommand::SetSaved {
                            access_token,
                            track_id,
                            artist_ids,
                            saved,
                        } => spawn_set_saved(
                            resp.clone(),
                            membership.clone(),
                            access_token,
                            track_id,
                            artist_ids,
                            saved,
                        ),
                        WorkerCommand::FetchQueue { access_token } => {
                            spawn_fetch_queue(resp.clone(), access_token)
                        }
                        WorkerCommand::AddToQueue { access_token, uri } => {
                            tokio::spawn(async move {
                                if let Err(e) = api::add_to_queue(&access_token, &uri).await {
                                    warn!("add_to_queue({uri}) failed: {e}");
                                }
                            });
                        }
                        WorkerCommand::LoadMembership { access_token } => {
                            spawn_load_membership(resp.clone(), membership.clone(), access_token)
                        }
                        WorkerCommand::QueryMembership { track_uri } => {
                            spawn_query_membership(resp.clone(), membership.clone(), track_uri)
                        }
                        WorkerCommand::EditMembership {
                            access_token,
                            playlist_id,
                            track_uri,
                            artist_ids,
                            add,
                        } => spawn_edit_membership(
                            resp.clone(),
                            membership.clone(),
                            access_token,
                            playlist_id,
                            track_uri,
                            artist_ids,
                            add,
                        ),
                    }
                }
            });
        });

        Self { cmd_tx, resp_rx }
    }

    pub fn start_oauth(&self, client_id: String) {
        let _ = self.cmd_tx.send(WorkerCommand::StartOAuth { client_id });
    }
    pub fn try_load_tokens(&self, client_id: String) {
        let _ = self.cmd_tx.send(WorkerCommand::TryLoadTokens { client_id });
    }
    pub fn fetch_home(&self, access_token: String) {
        let _ = self.cmd_tx.send(WorkerCommand::FetchHome { access_token });
    }
    pub fn seed_player_state(&self, access_token: String) {
        let _ = self
            .cmd_tx
            .send(WorkerCommand::SeedPlayerState { access_token });
    }
    pub fn fetch_track_details(&self, access_token: String, track_id: String) {
        let _ = self.cmd_tx.send(WorkerCommand::FetchTrackDetails {
            access_token,
            track_id,
        });
    }
    pub fn fetch_playlist(&self, access_token: String, id: String, liked: bool) {
        let _ = self.cmd_tx.send(WorkerCommand::FetchPlaylist {
            access_token,
            id,
            liked,
        });
    }
    pub fn fetch_album(&self, access_token: String, id: String) {
        let _ = self
            .cmd_tx
            .send(WorkerCommand::FetchAlbum { access_token, id });
    }
    pub fn fetch_artist(&self, access_token: String, id: String) {
        let _ = self
            .cmd_tx
            .send(WorkerCommand::FetchArtist { access_token, id });
    }
    pub fn fetch_artist_card(&self, access_token: String, id: String) {
        let _ = self
            .cmd_tx
            .send(WorkerCommand::FetchArtistCard { access_token, id });
    }
    pub fn fetch_context_name(&self, access_token: String, uri: String) {
        let _ = self
            .cmd_tx
            .send(WorkerCommand::FetchContextName { access_token, uri });
    }
    pub fn resolve_contexts(&self, access_token: String, uris: Vec<String>) {
        let _ = self
            .cmd_tx
            .send(WorkerCommand::ResolveContexts { access_token, uris });
    }
    pub fn search(&self, access_token: String, query: String) {
        let _ = self.cmd_tx.send(WorkerCommand::Search {
            access_token,
            query,
        });
    }
    pub fn fetch_track_credits(&self, track_id: String) {
        let _ = self
            .cmd_tx
            .send(WorkerCommand::FetchTrackCredits { track_id });
    }
    pub fn hydrate_tracks(&self, uris: Vec<String>) {
        let _ = self.cmd_tx.send(WorkerCommand::HydrateTracks { uris });
    }
    pub fn fetch_album_art(&self, url: String, key: String) {
        let _ = self.cmd_tx.send(WorkerCommand::FetchAlbumArt { url, key });
    }
    pub fn fetch_accent(&self, image_hex: String) {
        let _ = self.cmd_tx.send(WorkerCommand::FetchAccent { image_hex });
    }
    pub fn fetch_canvas(&self, track_uri: String, track_id: String) {
        let _ = self.cmd_tx.send(WorkerCommand::FetchCanvas {
            track_uri,
            track_id,
        });
    }
    pub fn connect_spotify_session(
        &self,
        access_token: String,
        initial_volume: f32,
        quality: crate::prefs::AudioQuality,
        normalize: bool,
    ) {
        let _ = self.cmd_tx.send(WorkerCommand::ConnectSpotifySession {
            access_token,
            initial_volume,
            quality,
            normalize,
        });
    }
    pub fn skip_forward(&self, access_token: String, count: u32, local: bool) {
        let _ = self.cmd_tx.send(WorkerCommand::SkipForward {
            access_token,
            count,
            local,
        });
    }
    pub fn playback(&self, access_token: String, cmd: PlaybackCmd, local: bool) {
        let _ = self.cmd_tx.send(WorkerCommand::Playback {
            access_token,
            cmd,
            local,
        });
    }
    pub fn claim_playback_paused(
        &self,
        context_uri: Option<String>,
        track_uri: String,
        position_ms: u32,
    ) {
        let _ = self.cmd_tx.send(WorkerCommand::ClaimPlaybackPaused {
            context_uri,
            track_uri,
            position_ms,
        });
    }
    pub fn refresh_tokens(&self, refresh_token: String, client_id: String) {
        let _ = self.cmd_tx.send(WorkerCommand::RefreshTokens {
            refresh_token,
            client_id,
        });
    }
    pub fn fetch_devices(&self, access_token: String) {
        let _ = self
            .cmd_tx
            .send(WorkerCommand::FetchDevices { access_token });
    }
    pub fn transfer_playback(
        &self,
        access_token: String,
        device_id: String,
        position_ms: Option<u32>,
    ) {
        let _ = self.cmd_tx.send(WorkerCommand::TransferPlayback {
            access_token,
            device_id,
            position_ms,
        });
    }
    pub fn check_saved(&self, access_token: String, track_id: String) {
        let _ = self.cmd_tx.send(WorkerCommand::CheckSaved {
            access_token,
            track_id,
        });
    }
    pub fn set_saved(
        &self,
        access_token: String,
        track_id: String,
        artist_ids: Vec<String>,
        saved: bool,
    ) {
        let _ = self.cmd_tx.send(WorkerCommand::SetSaved {
            access_token,
            track_id,
            artist_ids,
            saved,
        });
    }
    pub fn fetch_queue(&self, access_token: String) {
        let _ = self.cmd_tx.send(WorkerCommand::FetchQueue { access_token });
    }
    pub fn add_to_queue(&self, access_token: String, uri: String) {
        let _ = self
            .cmd_tx
            .send(WorkerCommand::AddToQueue { access_token, uri });
    }
    pub fn load_membership(&self, access_token: String) {
        let _ = self
            .cmd_tx
            .send(WorkerCommand::LoadMembership { access_token });
    }
    pub fn query_membership(&self, track_uri: String) {
        let _ = self
            .cmd_tx
            .send(WorkerCommand::QueryMembership { track_uri });
    }
    pub fn edit_membership(
        &self,
        access_token: String,
        playlist_id: String,
        track_uri: String,
        artist_ids: Vec<String>,
        add: bool,
    ) {
        let _ = self.cmd_tx.send(WorkerCommand::EditMembership {
            access_token,
            playlist_id,
            track_uri,
            artist_ids,
            add,
        });
    }
    pub fn poll(&self) -> Option<WorkerResponse> {
        self.resp_rx.try_recv().ok()
    }
}

fn spawn_oauth(resp: Responder, client_id: String) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let (url, verifier) = oauth::get_spotify_auth_url(&client_id);
        resp.send(WorkerResponse::OAuthStarted { auth_url: url });
        match listen_for_callback(verifier, client_id).await {
            Ok(auth) => {
                debug!("OAuth complete");
                let stored = StoredTokens::from(auth.clone());
                if let Err(e) = token_manager::save_tokens(&stored) {
                    error!("save tokens: {e}");
                }
                resp.send(WorkerResponse::OAuthComplete { auth });
            }
            Err(e) => {
                resp.send(WorkerResponse::OAuthFailed {
                    error: e.to_string(),
                });
            }
        }
    })
}

/// The persisted Recents archive key + cap. The Web API only exposes a
/// rolling ~50-item window, so older days would vanish; since past plays
/// never change we accumulate them here across sessions.
const RECENTS_HISTORY_KEY: &str = "recents_history";
const RECENTS_HISTORY_CAP: usize = 1000;

/// Merge freshly-fetched plays into the persisted Recents archive: dedup by
/// `played_at` (unique per play), keep newest-first, cap, persist. Returns
/// the archive with consecutive repeat runs collapsed for display. Blocking
/// fs — call from `spawn_blocking`.
fn merge_recents_history(fresh: Vec<api::RecentTrack>) -> Vec<api::RecentTrack> {
    let mut hist: Vec<api::RecentTrack> =
        disk_cache::read_json(RECENTS_HISTORY_KEY, std::time::Duration::MAX).unwrap_or_default();
    let seen: std::collections::HashSet<String> =
        hist.iter().map(|t| t.played_at.clone()).collect();
    for t in fresh {
        if !t.played_at.is_empty() && !seen.contains(&t.played_at) {
            hist.push(t);
        }
    }
    hist.sort_by(|a, b| b.played_at.cmp(&a.played_at));
    hist.truncate(RECENTS_HISTORY_CAP);
    disk_cache::write_json(RECENTS_HISTORY_KEY, &hist);
    // Collapse consecutive repeat plays (the archive keeps them raw).
    let mut out = hist;
    out.dedup_by(|a, b| a.id == b.id);
    out
}

fn spawn_fetch_home(resp: Responder, access_token: String) {
    tokio::spawn(async move {
        let (profile, playlists, recent, top_artists, top_tracks) = tokio::join!(
            api::get_me(&access_token),
            api::get_playlists(&access_token),
            api::get_recently_played(&access_token),
            api::get_top_artists(&access_token, 12),
            api::get_top_tracks(&access_token, 12),
        );
        let mut data = HomeData::default();
        match profile {
            Ok(p) => data.profile = Some(p),
            Err(e) => warn!("get_me failed: {e}"),
        }
        match playlists {
            Ok(ps) => data.playlists = ps,
            Err(e) => warn!("get_playlists failed: {e}"),
        }
        // Merge the fresh window into the persisted archive (empty on a
        // fetch failure — the archive still surfaces past days).
        let fresh = recent
            .inspect_err(|e| warn!("get_recently_played failed: {e}"))
            .unwrap_or_default();
        data.recent = tokio::task::spawn_blocking(move || merge_recents_history(fresh))
            .await
            .unwrap_or_default();
        match top_artists {
            Ok(a) => data.top_artists = a,
            Err(e) => warn!("get_top_artists failed: {e}"),
        }
        match top_tracks {
            Ok(t) => data.top_tracks = t,
            Err(e) => warn!("get_top_tracks failed: {e}"),
        }
        // Chained "latest release": newest album from #1 top artist.
        // Skipped silently if top_artists came back empty.
        if let Some(top) = data.top_artists.first() {
            match api::get_artist_albums(&access_token, &top.id, 5).await {
                Ok(mut albums) => data.latest_release = albums.drain(..).next(),
                Err(e) => warn!("get_artist_albums for top artist failed: {e}"),
            }
        }
        info!(
            "home data: profile={} playlists={} recent={} top_artists={} top_tracks={} latest_release={}",
            data.profile.is_some(),
            data.playlists.len(),
            data.recent.len(),
            data.top_artists.len(),
            data.top_tracks.len(),
            data.latest_release.is_some(),
        );
        resp.send(WorkerResponse::HomeData { data });
    });
}

fn spawn_seed_player(resp: Responder, access_token: String) {
    tokio::spawn(async move {
        match api::get_currently_playing(&access_token).await {
            Ok(player) => {
                info!(
                    "seeded initial player state from /me/player: present={}",
                    player.is_some()
                );
                resp.send(WorkerResponse::PlayerState { player });
            }
            Err(e) => warn!("seed /me/player failed: {e}"),
        }
    });
}

/// How long a cover's extracted accent stays cached. The colour is
/// immutable for a given cover (keyed by the image hash), so this is long
/// — it just bounds growth, like the art cache.
const ACCENT_TTL: std::time::Duration = std::time::Duration::from_secs(60 * 60 * 24 * 30);

/// Resolve Spotify's own extracted accent colour for a cover. Checks the
/// JSON disk cache first (instant, no session — kills the track-change
/// window where the new art shows with the *previous* accent because the
/// session round-trip lagged), then falls back to the `EXTRACTED_COLOR`
/// extended-metadata query, caching the result. No-ops silently if there's
/// no session yet and no cache (UI keeps the art-derived pixel average).
///
/// The cache stores the raw variant triple (`colors_<hex>`); the chrome's
/// chosen + contrast-lifted accent is derived on every read so the
/// contrast policy can evolve without fighting month-old cache entries.
fn spawn_fetch_accent(
    resp: Responder,
    session_slot: Arc<AsyncMutex<Option<Session>>>,
    image_hex: String,
) {
    tokio::spawn(async move {
        let cache_key = format!("colors_{image_hex}");
        // Cache hit → apply immediately, no session needed.
        if let Some(colors) = tokio::task::spawn_blocking({
            let cache_key = cache_key.clone();
            move || {
                disk_cache::read_json::<extracted_color::ExtractedColors>(&cache_key, ACCENT_TTL)
            }
        })
        .await
        .ok()
        .flatten()
        {
            resp.send(WorkerResponse::AccentReady {
                key: image_hex,
                accent: color::chrome_accent(&colors, tokens::ACCENT),
            });
            return;
        }
        let session = { session_slot.lock().await.clone() };
        let Some(session) = session else {
            debug!("accent fetch skipped — no session yet ({image_hex})");
            return;
        };
        // The extracted-colour extension is keyed by the cover's image
        // URI, not the track URI.
        let image_uri = format!("spotify:image:{image_hex}");
        match fetch_extracted_color(&session, &image_uri).await {
            Some(colors) => {
                debug!("extracted colors {image_hex} -> {colors:?}");
                tokio::task::spawn_blocking(move || disk_cache::write_json(&cache_key, &colors))
                    .await
                    .ok();
                resp.send(WorkerResponse::AccentReady {
                    key: image_hex,
                    accent: color::chrome_accent(&colors, tokens::ACCENT),
                });
            }
            None => debug!("no extracted color for {image_hex}"),
        }
    });
}

async fn fetch_extracted_color(
    session: &Session,
    image_uri: &str,
) -> Option<extracted_color::ExtractedColors> {
    let req = BatchedEntityRequest {
        entity_request: vec![EntityRequest {
            entity_uri: image_uri.to_string(),
            query: vec![ExtensionQuery {
                extension_kind: EnumOrUnknown::new(ExtensionKind::EXTRACTED_COLOR),
                ..Default::default()
            }],
            ..Default::default()
        }],
        ..Default::default()
    };
    let mut res = match session.spclient().get_extended_metadata(req).await {
        Ok(r) => r,
        Err(e) => {
            warn!("extracted-color request failed ({image_uri}): {e}");
            return None;
        }
    };
    // BatchedExtensionResponse → first entity → first extension → bytes.
    let mut arr = res.extended_metadata.pop()?;
    let mut data = arr.extension_data.pop()?;
    let any = data.extension_data.take()?;
    crate::extracted_color::parse_colors(&any.value)
}

/// Per-track Canvas metadata persisted to the JSON disk cache so we don't
/// re-hit Spotify's spclient (and don't need a live librespot session) for
/// a track we've already resolved. An empty `url` is a **negative** cache
/// entry — "this track has no video canvas" — so we don't re-query tracks
/// that never had one. Keyed by `canvas_meta_<track_id>`.
#[derive(serde::Serialize, serde::Deserialize)]
struct CanvasMeta {
    /// Canvas video URL, or empty for "no video canvas".
    url: String,
}

/// How long a track→canvas mapping stays valid. Canvas rarely changes for
/// a given track, so this is generous; it bounds growth + lets a removed
/// canvas eventually re-resolve, not catch same-day edits.
const CANVAS_META_TTL: std::time::Duration = std::time::Duration::from_secs(60 * 60 * 24 * 7);

/// Resolve + cache a track's Spotify Canvas video. Resolution order: first
/// the per-track metadata cache (no session / no network needed), then the
/// CANVAZ extended-metadata query (needs a live session). The resolved URL
/// (or a negative marker) is written back to the metadata cache, and the
/// MP4 bytes themselves are disk-cached separately. Responds `CanvasReady
/// { path }` for a video canvas or `CanvasNone` otherwise (no canvas /
/// image-only / fetch fail) so the UI falls back to album art.
fn spawn_fetch_canvas(
    resp: Responder,
    session_slot: Arc<AsyncMutex<Option<Session>>>,
    track_uri: String,
    track_id: String,
) {
    tokio::spawn(async move {
        let meta_key = format!("canvas_meta_{track_id}");
        // 1. Metadata-cache hit → resolve without touching the session.
        let cached = tokio::task::spawn_blocking({
            let meta_key = meta_key.clone();
            move || disk_cache::read_json::<CanvasMeta>(&meta_key, CANVAS_META_TTL)
        })
        .await
        .ok()
        .flatten();
        let url = match cached {
            Some(meta) if !meta.url.is_empty() => {
                debug!("canvas meta-cache hit {track_id}");
                meta.url
            }
            Some(_) => {
                // Negative cache: known to have no video canvas.
                debug!("canvas meta-cache hit (none) {track_id}");
                resp.send(WorkerResponse::CanvasNone { track_id });
                return;
            }
            None => {
                // 2. Cache miss → query spclient (needs a session). Without
                // one yet, bail *without* negative-caching so the retry
                // after the session connects can still resolve it.
                let session = { session_slot.lock().await.clone() };
                let Some(session) = session else {
                    debug!("canvas fetch deferred — no session yet ({track_id})");
                    resp.send(WorkerResponse::CanvasNone { track_id });
                    return;
                };
                let entry = fetch_canvas_entry(&session, &track_uri).await;
                let video = entry.as_ref().map(|e| e.kind.is_video()).unwrap_or(false);
                let url = if video {
                    entry.map(|e| e.url).unwrap_or_default()
                } else {
                    String::new()
                };
                // Write back (positive or negative) so we don't re-query.
                let write_url = url.clone();
                tokio::task::spawn_blocking(move || {
                    disk_cache::write_json(&meta_key, &CanvasMeta { url: write_url });
                })
                .await
                .ok();
                if url.is_empty() {
                    debug!("no video canvas for {track_id}");
                    resp.send(WorkerResponse::CanvasNone { track_id });
                    return;
                }
                url
            }
        };
        // Cache key = trailing path segment of the canvas URL (a stable
        // hash + `.mp4`), prefixed so it never collides with art keys.
        let key = format!("canvas_{}", canvas_cache_key(&url));
        // Disk-cache hit → skip the network.
        if let Some(path) = tokio::task::spawn_blocking({
            let key = key.clone();
            move || disk_cache::path(&key)
        })
        .await
        .ok()
        .flatten()
        {
            debug!("canvas disk-cache hit {track_id}");
            resp.send(WorkerResponse::CanvasReady { track_id, path });
            return;
        }
        let Some(bytes) = fetch_art_bytes(&url).await else {
            warn!("canvas download failed ({url})");
            resp.send(WorkerResponse::CanvasNone { track_id });
            return;
        };
        let path = tokio::task::spawn_blocking(move || {
            disk_cache::write(&key, &bytes);
            disk_cache::path(&key)
        })
        .await
        .ok()
        .flatten();
        match path {
            Some(path) => {
                debug!("canvas cached {track_id} ({} bytes)", path.display());
                resp.send(WorkerResponse::CanvasReady { track_id, path });
            }
            None => resp.send(WorkerResponse::CanvasNone { track_id }),
        }
    });
}

/// Trailing filename of a canvas URL (a stable hash), filesystem-safe.
fn canvas_cache_key(url: &str) -> String {
    url.rsplit('/')
        .next()
        .unwrap_or(url)
        .replace(['?', '&', '='], "_")
}

async fn fetch_canvas_entry(
    session: &Session,
    track_uri: &str,
) -> Option<crate::canvas::CanvasEntry> {
    let req = BatchedEntityRequest {
        entity_request: vec![EntityRequest {
            entity_uri: track_uri.to_string(),
            query: vec![ExtensionQuery {
                extension_kind: EnumOrUnknown::new(ExtensionKind::CANVAZ),
                ..Default::default()
            }],
            ..Default::default()
        }],
        ..Default::default()
    };
    let mut res = match session.spclient().get_extended_metadata(req).await {
        Ok(r) => r,
        Err(e) => {
            warn!("canvas request failed ({track_uri}): {e}");
            return None;
        }
    };
    debug!(
        "canvas xmeta {track_uri}: outer={} entries",
        res.extended_metadata.len()
    );
    let mut arr = res.extended_metadata.pop()?;
    debug!("canvas xmeta inner={} entries", arr.extension_data.len());
    let mut data = arr.extension_data.pop()?;
    let any = data.extension_data.take()?;
    debug!(
        "canvas xmeta type_url={:?} value_len={}",
        any.type_url,
        any.value.len()
    );
    crate::canvas::parse_canvas(&any.value)
}

/// Claim playback on Opal **paused** at `position_ms`, after the active
/// remote device dropped off the cluster mid-track. Drives our own Spirc
/// `load` with `start_playing: false`, which activates Opal as the Connect
/// device and loads the track held (no audible restart-then-pause). The
/// resulting paused state flows back through the local-player path, so the
/// chrome + transport become live and responsive on Opal. A missing local
/// Spirc (session not up) is a no-op — there's nothing to claim onto.
fn spawn_claim_paused(
    spirc_slot: Arc<AsyncMutex<Option<Spirc>>>,
    context_uri: Option<String>,
    track_uri: String,
    position_ms: u32,
) {
    tokio::spawn(async move {
        let guard = spirc_slot.lock().await;
        let Some(spirc) = guard.as_ref() else {
            warn!("claim-paused: no local Spirc — cannot take over");
            return;
        };
        let options = LoadRequestOptions {
            start_playing: false,
            seek_to: position_ms,
            context_options: None,
            playing_track: Some(PlayingTrack::Uri(track_uri.clone())),
        };
        // Prefer the real context (playlist/album) so next/prev keep working;
        // fall back to the single track when the vanished device reported none.
        let req = match context_uri {
            Some(ctx) => LoadRequest::from_context_uri(ctx, options),
            None => LoadRequest::from_tracks(vec![track_uri], options),
        };
        if let Err(e) = spirc.load(req) {
            warn!("claim-paused load failed: {e}");
        } else {
            info!("claimed playback on Opal (paused @ {position_ms}ms)");
        }
    });
}

/// Skip forward `count` tracks. When `local` (Opal is the active
/// device) and our Spirc exists, advance the queue in-process — instant
/// and reliable. Otherwise repeatedly hit Web API `next` on the active
/// (remote) device, spaced out a little so the rapid skips don't race or
/// trip rate limits. The resulting state change flows back via the
/// cluster / local-player path like any other transport.
fn spawn_skip_forward(
    spirc_slot: Arc<AsyncMutex<Option<Spirc>>>,
    access_token: String,
    count: u32,
    local: bool,
) {
    tokio::spawn(async move {
        if count == 0 {
            return;
        }
        if local {
            let guard = spirc_slot.lock().await;
            if let Some(spirc) = guard.as_ref() {
                for _ in 0..count {
                    if let Err(e) = spirc.next() {
                        warn!("spirc skip failed: {e}");
                        break;
                    }
                }
                return;
            }
            // No local Spirc after all — fall through to Web API.
        }
        for i in 0..count {
            if let Err(e) = api::next_track(&access_token).await {
                warn!("skip-forward next() failed at {i}/{count}: {e}");
                break;
            }
            if i + 1 < count {
                tokio::time::sleep(std::time::Duration::from_millis(120)).await;
            }
        }
    });
}

/// True for the Web API's "no active device" failure (404 with the
/// NO_ACTIVE_DEVICE reason).
fn is_no_active_device(e: &AuthError) -> bool {
    matches!(e, AuthError::Api(body, Some(404)) if body.contains("NO_ACTIVE_DEVICE"))
}

fn spawn_playback(
    resp: Responder,
    session_slot: Arc<AsyncMutex<Option<Session>>>,
    spirc_slot: Arc<AsyncMutex<Option<Spirc>>>,
    local_ctx: Arc<std::sync::Mutex<Option<String>>>,
    access_token: String,
    cmd: PlaybackCmd,
    local: bool,
) {
    tokio::spawn(async move {
        // Cold-start resume with no captured context (self-play never reports
        // one): fall back to the track's album so playback continues past the
        // one track instead of ending dead. Cheap + cached (immutable track
        // metadata); best-effort — a lookup failure just resumes the single
        // track as before.
        let cmd = match cmd {
            PlaybackCmd::PlayContext(api::PlayTarget::Resume {
                uri,
                position_ms,
                context_uri,
            }) => {
                // Keep a real, resumable context (playlist/album/…). Drop a
                // missing one OR Spotify's "spotify:web-api" pseudo-context
                // (the connect-state reports it for Web-API-initiated
                // playback; handing it back 400s "Invalid context uri") and
                // fall back to the track's album so playback continues past
                // the one track instead of ending dead.
                let real_context = context_uri.filter(|c| !c.is_empty() && c != "spotify:web-api");
                let context_uri = match real_context {
                    Some(c) => Some(c),
                    None => match api::track_id_from_uri(&uri) {
                        Some(id) => api::get_track(&access_token, id)
                            .await
                            .ok()
                            .filter(|d| !d.album_id.is_empty())
                            .map(|d| format!("spotify:album:{}", d.album_id)),
                        None => None,
                    },
                };
                PlaybackCmd::PlayContext(api::PlayTarget::Resume {
                    uri,
                    position_ms,
                    context_uri,
                })
            }
            other => other,
        };
        // Remember the context Opal is about to load so the contextless local
        // player states can wear the right source pill. Stored for every
        // `PlayContext` (not just `local`): it's only ever *read* by the local
        // stamp, which fires only when Opal is genuinely the player, and a
        // remote takeover clears it — so this also covers a cold start with no
        // active device where the load lands on Opal.
        if let PlaybackCmd::PlayContext(target) = &cmd {
            *local_ctx.lock().unwrap() = target.context_uri().map(str::to_string);
        }
        // When Opal is the active device, drive our own Spirc directly.
        // The Web API round-trip (Spotify → dealer relay → our Spirc) can
        // silently no-op after a long uptime if the relay path goes stale —
        // the command 200s but nothing happens, leaving the optimistic
        // play/pause flipped. The local Spirc call is instant and can't go
        // stale that way. `PlayContext` is a context load, not a transport
        // verb, so it always takes the Web API path below.
        if local && !matches!(cmd, PlaybackCmd::PlayContext(_)) {
            let guard = spirc_slot.lock().await;
            if let Some(spirc) = guard.as_ref() {
                let r = match cmd.clone() {
                    PlaybackCmd::Play => spirc.play(),
                    PlaybackCmd::Pause => spirc.pause(),
                    PlaybackCmd::Next => spirc.next(),
                    PlaybackCmd::Prev => spirc.prev(),
                    PlaybackCmd::Shuffle(on) => spirc.shuffle(on),
                    // context + track are independent flags; set both for
                    // every mode so leaving Track actually clears the track
                    // flag (otherwise repeat(false) only drops context and the
                    // device stays stuck repeating one).
                    PlaybackCmd::Repeat(mode) => {
                        let (ctx, track) = match mode {
                            RepeatMode::Track => (false, true),
                            RepeatMode::Context => (true, false),
                            RepeatMode::Off => (false, false),
                        };
                        spirc.repeat(ctx).and_then(|()| spirc.repeat_track(track))
                    }
                    PlaybackCmd::Seek(ms) => spirc.set_position_ms(ms),
                    PlaybackCmd::Volume(pct) => {
                        spirc.set_volume((pct.min(100) as u32 * u16::MAX as u32 / 100) as u16)
                    }
                    PlaybackCmd::PlayContext(_) => unreachable!("guarded above"),
                };
                match r {
                    Ok(()) => debug!("spirc cmd {cmd:?} ok"),
                    Err(e) => {
                        warn!("spirc cmd {cmd:?} failed: {e}");
                        resp.send(WorkerResponse::PlaybackFailed { cmd });
                    }
                }
                return;
            }
            // No local Spirc after all — fall through to the Web API.
        }
        let result = match cmd.clone() {
            PlaybackCmd::Play => api::play(&access_token, None).await,
            PlaybackCmd::Pause => api::pause(&access_token).await,
            PlaybackCmd::Next => api::next_track(&access_token).await,
            PlaybackCmd::Prev => api::previous_track(&access_token).await,
            PlaybackCmd::Shuffle(on) => api::set_shuffle(&access_token, on).await,
            PlaybackCmd::Repeat(mode) => api::set_repeat(&access_token, mode).await,
            PlaybackCmd::Seek(ms) => api::seek(&access_token, ms, None).await,
            PlaybackCmd::Volume(pct) => api::set_volume(&access_token, pct).await,
            PlaybackCmd::PlayContext(target) => {
                api::play_context(&access_token, target, None).await
            }
        };
        // No active device + a "start playing" intent → Opal IS a
        // playable Connect device (real rodio sink): retry the same
        // command targeted at our own librespot device id, so playback
        // simply starts here instead of dead-ending on a 404.
        let result = match result {
            Err(ref e) if is_no_active_device(e) => {
                let device_id = {
                    session_slot
                        .lock()
                        .await
                        .as_ref()
                        .map(|s| s.device_id().to_string())
                };
                match (device_id, cmd.clone()) {
                    (Some(id), PlaybackCmd::Play) => {
                        info!("no active device — resuming on Opal ({id})");
                        api::play(&access_token, Some(&id)).await
                    }
                    (Some(id), PlaybackCmd::PlayContext(target)) => {
                        info!("no active device — playing on Opal ({id})");
                        api::play_context(&access_token, target, Some(&id)).await
                    }
                    // Pause/Next/Seek/… with nothing playing anywhere:
                    // there is no state to act on; surface the failure.
                    _ => result,
                }
            }
            other => other,
        };
        match result {
            Ok(()) => debug!("playback cmd {cmd:?} ok"),
            Err(e) => {
                warn!("playback cmd {cmd:?} failed: {e}");
                // Tell the UI so it can roll back its optimistic flip —
                // otherwise the play button shows "pause" while nothing
                // is actually playing.
                resp.send(WorkerResponse::PlaybackFailed { cmd });
            }
        }
    });
}

fn spawn_fetch_track_details(resp: Responder, access_token: String, track_id: String) {
    tokio::spawn(async move {
        match api::get_track(&access_token, &track_id).await {
            Ok(details) => resp.send(WorkerResponse::TrackDetails { details }),
            Err(e) => warn!("get_track({track_id}) failed: {e}"),
        }
    });
}

/// Disk-cache TTL for playlist track listings. Longer than the UI's
/// in-memory cache (which covers within-session re-opens) so a relaunch
/// re-opening a big playlist skips re-paging the whole thing from the
/// Web API, but short enough that edits made elsewhere surface within
/// the hour. Listings are mutable, so unlike album art this is hours,
/// not days.
const PLAYLIST_DISK_TTL: std::time::Duration = std::time::Duration::from_secs(60 * 30);

/// Disk key for a playlist/album detail listing. **Versioned**: bump the
/// prefix whenever `PlaylistTrack` grows fields the UI depends on (e.g.
/// the per-artist ids behind the clickable credit spans) — old-schema
/// entries deserialize with silent defaults, which shows up as features
/// working on freshly-cached pages and not on stale ones. A bump orphans
/// the old files (the json cache cap evicts them) and refetches honestly.
fn detail_cache_key(id: &str) -> String {
    format!("detail_v2_{id}")
}

/// Hard ceiling on streamed tracks — guards against a pathological
/// `total` driving an unbounded loop. 10k covers every realistic
/// library; the windowed-play UX matters more than completeness beyond.
const MAX_STREAM_TRACKS: usize = 10_000;

/// Load an album page. Albums fit in one request (≤ 50 tracks), so this is
/// a single `PlaylistOpened { complete: true }` — no streaming. The disk
/// cache (shared with playlists, keyed by id) makes re-opens instant.
fn spawn_fetch_album(resp: Responder, access_token: String, id: String) {
    tokio::spawn(async move {
        let key = detail_cache_key(&id);
        let cached = tokio::task::spawn_blocking(move || {
            disk_cache::read_json::<api::PlaylistDetail>(&key, PLAYLIST_DISK_TTL)
        })
        .await
        .ok()
        .flatten();
        if let Some(detail) = cached {
            resp.send(WorkerResponse::PlaylistOpened {
                detail,
                complete: true,
            });
            return;
        }
        match api::get_album(&access_token, &id).await {
            Ok(detail) => {
                let key = detail_cache_key(&id);
                let to_cache = detail.clone();
                tokio::task::spawn_blocking(move || disk_cache::write_json(&key, &to_cache));
                resp.send(WorkerResponse::PlaylistOpened {
                    detail,
                    complete: true,
                });
            }
            Err(e) => {
                warn!("get_album({id}) failed: {e}");
                resp.send(WorkerResponse::PlaylistFailed {
                    id,
                    error: e.to_string(),
                });
            }
        }
    });
}

/// Fetch the now-playing "About the artist" card: the Web API profile
/// (name / photo / followers — `get_json`'s disk cache makes repeats
/// near-free) plus the extended-metadata biography when a session is up.
/// Profile failure is silent — the section simply stays hidden.
fn spawn_fetch_artist_card(
    resp: Responder,
    session_slot: Arc<AsyncMutex<Option<Session>>>,
    access_token: String,
    id: String,
) {
    tokio::spawn(async move {
        let detail = match api::get_artist(&access_token, &id).await {
            Ok(d) => d,
            Err(e) => {
                warn!("artist card ({id}) failed: {e}");
                return;
            }
        };
        let bio = fetch_artist_bio(&session_slot, &id).await;
        resp.send(WorkerResponse::ArtistCardReady { detail, bio });
    });
}

/// How long an artist's biography stays cached. Edited ~never; this just
/// bounds growth like the other metadata caches.
const ARTIST_BIO_TTL: std::time::Duration = std::time::Duration::from_secs(60 * 60 * 24 * 30);

/// Disk-cache shape for a biography. An empty `text` is a **negative**
/// entry — "this artist has no bio" — so we don't re-query every track.
#[derive(serde::Serialize, serde::Deserialize)]
struct ArtistBio {
    text: String,
}

/// Resolve an artist's biography: JSON disk cache first, then the
/// `ARTIST_V4` extended-metadata query (needs a live session — skipped
/// silently without one, uncached so a later fetch can still succeed).
async fn fetch_artist_bio(
    session_slot: &Arc<AsyncMutex<Option<Session>>>,
    artist_id: &str,
) -> Option<String> {
    let cache_key = format!("artist_bio_{artist_id}");
    if let Ok(Some(cached)) = tokio::task::spawn_blocking({
        let k = cache_key.clone();
        move || disk_cache::read_json::<ArtistBio>(&k, ARTIST_BIO_TTL)
    })
    .await
    {
        return (!cached.text.is_empty()).then_some(cached.text);
    }
    let session = { session_slot.lock().await.clone() };
    let Some(session) = session else {
        debug!("artist bio skipped — no session yet ({artist_id})");
        return None;
    };
    let req = BatchedEntityRequest {
        entity_request: vec![EntityRequest {
            entity_uri: format!("spotify:artist:{artist_id}"),
            query: vec![ExtensionQuery {
                extension_kind: EnumOrUnknown::new(ExtensionKind::ARTIST_V4),
                ..Default::default()
            }],
            ..Default::default()
        }],
        ..Default::default()
    };
    let mut res = match session.spclient().get_extended_metadata(req).await {
        Ok(r) => r,
        Err(e) => {
            warn!("artist bio request failed ({artist_id}): {e}");
            return None;
        }
    };
    let text = (|| {
        let mut arr = res.extended_metadata.pop()?;
        let mut data = arr.extension_data.pop()?;
        let any = data.extension_data.take()?;
        let artist = <librespot_protocol::metadata::Artist as protobuf::Message>::parse_from_bytes(
            &any.value,
        )
        .ok()?;
        let bio = artist.biography.first()?;
        Some(strip_tags(bio.text()))
    })()
    .unwrap_or_default();
    let store = ArtistBio { text: text.clone() };
    tokio::task::spawn_blocking(move || disk_cache::write_json(&cache_key, &store))
        .await
        .ok();
    (!text.is_empty()).then_some(text)
}

/// Drop the simple HTML tags (`<a href=…>`, `<b>`, `<br>`) Spotify embeds
/// in biography text, keeping the visible characters.
fn strip_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    out
}

/// Resolve a context uri's display name via the Web API. Failure reports
/// `name: None` (not silence) so the model can negative-cache the uri and
/// stop re-asking on every push.
fn spawn_fetch_context_name(resp: Responder, access_token: String, uri: String) {
    tokio::spawn(async move {
        let name = match api::get_context_name(&access_token, &uri).await {
            Ok(n) => n,
            Err(e) => {
                warn!("context name ({uri}) failed: {e}");
                None
            }
        };
        resp.send(WorkerResponse::ContextNameReady { uri, name });
    });
}

/// Resolve a batch of Recents session contexts (name + owner + cover),
/// concurrently. Reuses the disk-cached meta fetches, so re-opening the
/// Recents page is free. Unresolvable uris are absent from the result.
fn spawn_resolve_contexts(resp: Responder, access_token: String, uris: Vec<String>) {
    tokio::spawn(async move {
        let futs = uris.into_iter().map(|uri| {
            let token = access_token.clone();
            async move {
                api::resolve_context_meta(&token, &uri)
                    .await
                    .map(|info| (uri, info))
            }
        });
        let map = futures::future::join_all(futs)
            .await
            .into_iter()
            .flatten()
            .collect();
        resp.send(WorkerResponse::ContextsResolved { map });
    });
}

/// Run a search; empty results on failure so the UI clears its spinner.
fn spawn_search(resp: Responder, access_token: String, query: String) {
    tokio::spawn(async move {
        let results = api::search(&access_token, &query)
            .await
            .inspect_err(|e| warn!("search ({query}) failed: {e}"))
            .unwrap_or_default();
        resp.send(WorkerResponse::SearchResults { query, results });
    });
}

/// One person in a credits group: display roles pre-joined ("Composer •
/// Lyricist"); `artist_id` non-empty when they have a Spotify artist
/// profile (the row navigates to it).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CreditPerson {
    pub name: String,
    pub roles: String,
    pub artist_id: String,
}

/// One credits section, mirroring Spotify's grouping ("Composition &
/// Lyrics", "Production & Engineering", "Performers").
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CreditGroup {
    pub title: String,
    pub people: Vec<CreditPerson>,
}

/// A track's full credits: the grouped sections plus the source labels
/// ("Craft Recordings"). Also the disk-cache shape.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct TrackCredits {
    pub groups: Vec<CreditGroup>,
    pub sources: Vec<String>,
}

impl TrackCredits {
    pub fn is_empty(&self) -> bool {
        self.groups.is_empty() && self.sources.is_empty()
    }
}

/// How long a track's credits stay cached — immutable in practice.
const CREDITS_TTL: std::time::Duration = std::time::Duration::from_secs(60 * 60 * 24 * 30);

/// Resolve a track's credits: JSON disk cache first (covers the empty
/// list as a negative entry), then the spclient track-credits endpoint.
/// Skipped silently without a session (uncached, retried on next track).
fn spawn_fetch_track_credits(
    resp: Responder,
    session_slot: Arc<AsyncMutex<Option<Session>>>,
    track_id: String,
) {
    tokio::spawn(async move {
        let cache_key = format!("credits_{track_id}");
        if let Ok(Some(credits)) = tokio::task::spawn_blocking({
            let k = cache_key.clone();
            move || disk_cache::read_json::<TrackCredits>(&k, CREDITS_TTL)
        })
        .await
        {
            resp.send(WorkerResponse::TrackCreditsReady { track_id, credits });
            return;
        }
        let session = { session_slot.lock().await.clone() };
        let Some(session) = session else {
            debug!("credits fetch skipped — no session yet ({track_id})");
            return;
        };
        let endpoint = format!("/track-credits-view/v0/experimental/{track_id}/credits");
        let body = match session
            .spclient()
            .request(&http::Method::GET, &endpoint, None, None)
            .await
        {
            Ok(b) => b,
            Err(e) => {
                warn!("track credits ({track_id}) failed: {e}");
                return;
            }
        };
        let credits = parse_credits(&body);
        let store = credits.clone();
        tokio::task::spawn_blocking(move || disk_cache::write_json(&cache_key, &store))
            .await
            .ok();
        resp.send(WorkerResponse::TrackCreditsReady { track_id, credits });
    });
}

/// spclient credits JSON → grouped display sections, keeping Spotify's
/// own order and mapping the raw group keys to the official client's
/// headings (Writers → "Composition & Lyrics", Producers → "Production
/// & Engineering"). Each person keeps their subroles ("Composer •
/// Lyricist") and, when they have an artist profile, the bare artist id
/// so the row can navigate to it.
fn parse_credits(body: &[u8]) -> TrackCredits {
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct R {
        #[serde(default)]
        role_credits: Vec<Role>,
        #[serde(default)]
        source_names: Vec<String>,
    }
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Role {
        #[serde(default)]
        role_title: String,
        #[serde(default)]
        artists: Vec<Person>,
    }
    #[derive(serde::Deserialize)]
    struct Person {
        #[serde(default)]
        name: String,
        #[serde(default)]
        uri: String,
        #[serde(default)]
        subroles: Vec<String>,
    }
    let Ok(r) = serde_json::from_slice::<R>(body) else {
        return TrackCredits::default();
    };
    let groups = r
        .role_credits
        .into_iter()
        .filter_map(|role| {
            let people: Vec<CreditPerson> = role
                .artists
                .into_iter()
                .filter(|p| !p.name.is_empty())
                .map(|p| CreditPerson {
                    name: p.name,
                    roles: p
                        .subroles
                        .iter()
                        .map(|s| title_case(s))
                        .collect::<Vec<_>>()
                        .join(" \u{2022} "),
                    artist_id: p
                        .uri
                        .strip_prefix("spotify:artist:")
                        .unwrap_or_default()
                        .to_string(),
                })
                .collect();
            if people.is_empty() {
                return None;
            }
            let title = match role.role_title.to_ascii_lowercase().as_str() {
                "writers" => "Composition & Lyrics".to_string(),
                "producers" => "Production & Engineering".to_string(),
                "performers" => "Performers".to_string(),
                _ => title_case(&role.role_title),
            };
            Some(CreditGroup { title, people })
        })
        .collect();
    TrackCredits {
        groups,
        sources: r
            .source_names
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect(),
    }
}

/// "main artist" → "Main Artist".
fn title_case(s: &str) -> String {
    s.split_whitespace()
        .map(|w| {
            let mut cs = w.chars();
            match cs.next() {
                Some(f) => f.to_uppercase().collect::<String>() + cs.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Load an artist page: profile + discography (newest-first albums), in
/// parallel. `get_json`'s disk cache makes re-opens cheap.
fn spawn_fetch_artist(
    resp: Responder,
    session_slot: Arc<AsyncMutex<Option<Session>>>,
    access_token: String,
    id: String,
) {
    tokio::spawn(async move {
        // The user's country is the `market` for top-tracks; `get_me` is
        // disk-cached (SLOW), so this is near-free after the first call.
        let market = api::get_me(&access_token)
            .await
            .map(|p| p.country)
            .unwrap_or_default();
        let (profile, top_tracks, albums) = tokio::join!(
            api::get_artist(&access_token, &id),
            api::get_artist_top_tracks(&access_token, &id, &market),
            api::get_artist_albums(&access_token, &id, 20),
        );
        // Partial failures degrade to empty sections, but loudly — a
        // silent swallow here once hid a whole API regression.
        let top_tracks = top_tracks
            .inspect_err(|e| warn!("artist top-tracks ({id}) failed: {e}"))
            .unwrap_or_default();
        let albums = albums
            .inspect_err(|e| warn!("artist albums ({id}) failed: {e}"))
            .unwrap_or_default();
        match profile {
            Ok(p) => {
                // The user's saved songs by this artist, across the whole
                // library. The reverse index makes the set complete; page
                // caches resolve most metadata for free, and the handful in
                // never-opened playlists are hydrated over the network so the
                // section matches what now-playing/the playlist pages know.
                let mut library_tracks = {
                    let artist_id = id.clone();
                    let artist_name = p.name.clone();
                    let (resolved, missing) = tokio::task::spawn_blocking(move || {
                        artist_library_scan(&artist_id, &artist_name)
                    })
                    .await
                    .unwrap_or_default();
                    if !missing.is_empty()
                        && let Some(session) = session_slot.lock().await.clone()
                    {
                        let uris: Vec<String> = missing.iter().map(|(u, _)| u.clone()).collect();
                        let hydrated = fetch_tracks_v4(&session, &uris).await;
                        let sources_of: std::collections::HashMap<String, Vec<String>> =
                            missing.into_iter().collect();
                        let mut out = resolved;
                        for t in hydrated {
                            let src = sources_of.get(&t.uri).cloned().unwrap_or_default();
                            out.push((t, src));
                        }
                        out
                    } else {
                        resolved
                    }
                };
                // Newest resolution order is arbitrary; keep saved-by-artist
                // rows stable by title so the section doesn't reshuffle.
                library_tracks.sort_by(|(a, _), (b, _)| a.name.cmp(&b.name));
                resp.send(WorkerResponse::ArtistOpened {
                    id,
                    name: p.name,
                    image_url: p.image_url,
                    followers: p.followers,
                    top_tracks,
                    albums,
                    library_tracks,
                })
            }
            Err(e) => {
                warn!("get_artist({id}) failed: {e}");
                resp.send(WorkerResponse::ArtistFailed {
                    id,
                    error: e.to_string(),
                });
            }
        }
    });
}

/// Resolve bare track uris to full display rows via ONE batched
/// `TRACK_V4` extended-metadata request per ~40 uris (the official
/// client hydrates its queue the same way — cluster `next_tracks` often
/// ship uri-only for context/autoplay entries). Silent no-op without a
/// session; unresolvable entries are simply absent from the result.
fn spawn_hydrate_tracks(
    resp: Responder,
    session_slot: Arc<AsyncMutex<Option<Session>>>,
    uris: Vec<String>,
) {
    tokio::spawn(async move {
        if uris.is_empty() {
            return;
        }
        let session = { session_slot.lock().await.clone() };
        let Some(session) = session else {
            debug!(
                "track hydration skipped — no session yet ({} uris)",
                uris.len()
            );
            return;
        };
        let tracks = fetch_tracks_v4(&session, &uris).await;
        info!(
            "track hydration resolved {}/{} uris",
            tracks.len(),
            uris.len()
        );
        resp.send(WorkerResponse::TracksHydrated { tracks });
    });
}

/// Resolve `uris` to full track rows via batched `TRACK_V4` extended
/// metadata (one request per ~40 uris). Unresolvable entries are simply
/// absent from the result. Shared by the queue hydration + the artist
/// page's "In your library" completion.
async fn fetch_tracks_v4(session: &Session, uris: &[String]) -> Vec<api::PlaylistTrack> {
    let mut tracks: Vec<api::PlaylistTrack> = Vec::with_capacity(uris.len());
    for chunk in uris.chunks(40) {
        let req = BatchedEntityRequest {
            entity_request: chunk
                .iter()
                .map(|uri| EntityRequest {
                    entity_uri: uri.clone(),
                    query: vec![ExtensionQuery {
                        extension_kind: EnumOrUnknown::new(ExtensionKind::TRACK_V4),
                        ..Default::default()
                    }],
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        };
        let res = match session.spclient().get_extended_metadata(req).await {
            Ok(r) => r,
            Err(e) => {
                warn!("track hydration batch failed: {e}");
                continue;
            }
        };
        for arr in res.extended_metadata {
            for mut data in arr.extension_data {
                let uri = data.entity_uri.clone();
                let Some(any) = data.extension_data.take() else {
                    continue;
                };
                let Ok(t) =
                    <librespot_protocol::metadata::Track as protobuf::Message>::parse_from_bytes(
                        &any.value,
                    )
                else {
                    continue;
                };
                if let Some(row) = proto_track_to_row(uri, &t) {
                    tracks.push(row);
                }
            }
        }
    }
    tracks
}

/// Map a metadata-proto `Track` to our domain [`api::PlaylistTrack`].
/// Cover = the largest `album.cover`/`cover_group` image as an
/// `i.scdn.co` URL; artist gids → base62 ids for the clickable lines.
fn proto_track_to_row(
    uri: String,
    t: &librespot_protocol::metadata::Track,
) -> Option<api::PlaylistTrack> {
    let id = api::track_id_from_uri(&uri)?.to_string();
    let name = t.name().to_string();
    if name.is_empty() {
        return None;
    }
    let artists: Vec<api::TrackArtist> = t
        .artist
        .iter()
        .map(|a| api::TrackArtist {
            id: librespot_core::SpotifyId::from_raw(a.gid())
                .map(|g| g.to_base62())
                .unwrap_or_default(),
            name: a.name().to_string(),
        })
        .collect();
    let artist = artists
        .iter()
        .map(|a| a.name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let album = t.album.as_ref();
    let album_id = album
        .and_then(|al| librespot_core::SpotifyId::from_raw(al.gid()).ok())
        .map(|g| g.to_base62())
        .unwrap_or_default();
    // Largest cover from either the flat list or the grouped one.
    let album_image_url = album
        .and_then(|al| {
            al.cover
                .iter()
                .chain(al.cover_group.image.iter())
                .max_by_key(|img| img.width())
        })
        .map(|img| {
            let hex: String = img.file_id().iter().map(|b| format!("{b:02x}")).collect();
            format!("https://i.scdn.co/image/{hex}")
        });
    Some(api::PlaylistTrack {
        id,
        uri,
        name: name.clone(),
        artist,
        album: album.map(|al| al.name().to_string()).unwrap_or_default(),
        album_image_url,
        duration_ms: t.duration().max(0) as u64,
        artist_id: artists.first().map(|a| a.id.clone()).unwrap_or_default(),
        artists,
        album_id,
        playable: true,
    })
}

/// Aggregate the user's saved tracks by `artist_id` across the library:
/// Liked Songs + every playlist in the membership index, each with the
/// names of the sources that contain it. Source lists resolve through
/// the index (complete even when a playlist's page cache is missing);
/// track metadata joins from the available page caches. Returns the
/// resolved rows plus the uris of saved-by-this-artist tracks whose
/// metadata *wasn't* in any page cache — the caller hydrates those over
/// the network so a track in a never-opened playlist still shows.
/// Blocking (fs reads) — call from `spawn_blocking`.
/// A saved track paired with the names of the sources (playlists / Liked
/// Songs) that contain it — one artist-library row.
type ArtistLibraryRow = (api::PlaylistTrack, Vec<String>);
/// `artist_library_scan`'s result: rows resolved from caches, plus
/// `(uri, source_names)` for saved-by-this-artist tracks needing a
/// metadata fetch.
type ArtistLibraryScan = (Vec<ArtistLibraryRow>, Vec<(String, Vec<String>)>);

fn artist_library_scan(artist_id: &str, artist_name: &str) -> ArtistLibraryScan {
    use crate::model::membership::MembershipSnapshot;
    let by_artist = |t: &api::PlaylistTrack| {
        // Cache rows written before the per-artist ids existed deserialize
        // with empty id fields — fall back to a display-name match so the
        // section works until those caches naturally rewrite.
        t.artist_id == artist_id
            || t.artists.iter().any(|a| a.id == artist_id)
            || (t.artist_id.is_empty()
                && t.artists.is_empty()
                && !artist_name.is_empty()
                && t.artist
                    .split(", ")
                    .any(|n| n.eq_ignore_ascii_case(artist_name)))
    };
    let mut out: Vec<(api::PlaylistTrack, Vec<String>)> = Vec::new();
    let mut add =
        |t: &api::PlaylistTrack, src: &str| match out.iter_mut().find(|(e, _)| e.uri == t.uri) {
            Some((_, sources)) => {
                if !sources.iter().any(|s| s == src) {
                    sources.push(src.to_string());
                }
            }
            None => out.push((t.clone(), vec![src.to_string()])),
        };
    if let Some(d) = disk_cache::read_json::<api::PlaylistDetail>(
        &detail_cache_key(api::LIKED_SONGS_ID),
        PLAYLIST_DISK_TTL,
    ) {
        for t in d.tracks.iter().filter(|t| by_artist(t)) {
            add(t, "Liked Songs");
        }
    }
    let snap =
        disk_cache::read_json::<MembershipSnapshot>(MEMBERSHIP_KEY, std::time::Duration::MAX);
    let name_of: std::collections::HashMap<&str, &str> = snap
        .as_ref()
        .map(|s| {
            s.playlists
                .iter()
                .map(|p| (p.id.as_str(), p.name.as_str()))
                .collect()
        })
        .unwrap_or_default();
    if let Some(snap) = &snap {
        for p in &snap.playlists {
            if let Some(d) = disk_cache::read_json::<api::PlaylistDetail>(
                &detail_cache_key(&p.id),
                PLAYLIST_DISK_TTL,
            ) {
                for t in d.tracks.iter().filter(|t| by_artist(t)) {
                    add(t, &p.name);
                }
            }
        }
    }
    // Complete each resolved track's source list — "Liked Songs" plus every
    // playlist from the index (a playlist whose page cache is missing still
    // names its membership here).
    let sources_of = |uri: &str| -> Vec<String> {
        let mut names = Vec::new();
        if snap
            .as_ref()
            .map(|s| s.liked.contains(uri))
            .unwrap_or(false)
        {
            names.push("Liked Songs".to_string());
        }
        if let Some(ids) = snap.as_ref().and_then(|s| s.index.get(uri)) {
            names.extend(
                ids.iter()
                    .filter_map(|pid| name_of.get(pid.as_str()).map(|n| n.to_string())),
            );
        }
        names
    };
    for (track, sources) in out.iter_mut() {
        for name in sources_of(&track.uri) {
            if !sources.iter().any(|s| s == &name) {
                sources.push(name);
            }
        }
    }
    // The reverse index answers "saved tracks by this artist" completely —
    // including tracks in playlists whose page cache we never wrote, and
    // liked-only tracks. Any not already resolved above needs a metadata
    // fetch by the caller.
    let mut missing: Vec<(String, Vec<String>)> = Vec::new();
    if let Some(snap) = &snap
        && let Some(uris) = snap.artist_index.get(artist_id)
    {
        for uri in uris {
            // Still saved? (a remove prunes the forward index / liked set, so
            // a stale reverse entry is filtered out here.) Already resolved?
            let still_saved = snap.index.contains_key(uri) || snap.liked.contains(uri);
            if !still_saved || out.iter().any(|(t, _)| &t.uri == uri) {
                continue;
            }
            missing.push((uri.clone(), sources_of(uri)));
        }
    }
    (out, missing)
}

/// The user's Liked Songs collection context uri
/// (`spotify:user:{id}:collection`) — `get_me` is disk-cached (SLOW), so
/// this is free after the first call. `None` if the profile can't resolve
/// (the caller falls back to a bare uris window).
async fn collection_context_uri(access_token: &str) -> Option<String> {
    api::get_me(access_token)
        .await
        .ok()
        .filter(|p| !p.id.is_empty())
        .map(|p| format!("spotify:user:{}:collection", p.id))
}

fn spawn_fetch_playlist(resp: Responder, access_token: String, id: String, liked: bool) {
    tokio::spawn(async move {
        // 1. Disk cache first — a fresh hit delivers the whole listing in
        //    one `complete` response (no re-paging the CDN/API).
        let key = detail_cache_key(&id);
        let cached = tokio::task::spawn_blocking(move || {
            disk_cache::read_json::<api::PlaylistDetail>(&key, PLAYLIST_DISK_TTL)
        })
        .await
        .ok()
        .flatten();
        if let Some(mut detail) = cached {
            info!(
                "playlist '{}' disk-cache hit: {} tracks",
                detail.name,
                detail.tracks.len()
            );
            // Older cache entries predate the collection context — patch
            // it in so cached Liked Songs plays with a context too.
            if liked && detail.context_uri.is_none() {
                detail.context_uri = collection_context_uri(&access_token).await;
            }
            resp.send(WorkerResponse::PlaylistOpened {
                detail,
                complete: true,
            });
            return;
        }

        // 2. Metadata first (playlists only — Liked Songs gets its total
        //    from the first page) so the header + scrollbar appear before
        //    any track page lands.
        let (name, owner, image_url, context_uri) = if liked {
            // Liked Songs IS a playable context — `spotify:user:{id}:collection`,
            // the same uri the official client plays it through. Playing with
            // a context (instead of a bare uris window) is what keeps
            // continuation + autoplay + every connected client's now-playing
            // coherent.
            let ctx = collection_context_uri(&access_token).await;
            ("Liked Songs".to_string(), String::new(), None, ctx)
        } else {
            match api::playlist_meta(&access_token, &id).await {
                Ok(m) => (
                    m.name,
                    m.owner,
                    m.image_url,
                    Some(format!("spotify:playlist:{id}")),
                ),
                Err(e) => {
                    warn!("playlist_meta({id}) failed: {e}");
                    resp.send(WorkerResponse::PlaylistFailed {
                        id,
                        error: e.to_string(),
                    });
                    return;
                }
            }
        };

        // 3. Stream track pages. The first page rides a `PlaylistOpened`
        //    (mounts the list); the rest are `PlaylistTracks` appended to
        //    the live buffer with no rebuild.
        let page_size = if liked {
            api::LIKED_PAGE
        } else {
            api::PLAYLIST_PAGE
        };
        let mut offset = 0u32;
        let mut first = true;
        let mut total = 0u32;
        let mut accumulated: Vec<api::PlaylistTrack> = Vec::new();
        loop {
            let url = if liked {
                api::liked_tracks_url(offset, page_size)
            } else {
                api::playlist_tracks_url(&id, offset, page_size)
            };
            // A transient failure (rate limit, network blip) mid-stream
            // must not strand the rest of the playlist — retry with a
            // short backoff before giving up. On a persistent failure the
            // UI is told either way: `PlaylistFailed` clears the inflight
            // gate (and `complete` stays false), so re-opening refetches
            // instead of being locked out forever.
            let mut attempt = 0u32;
            let page = loop {
                match api::fetch_tracks_page(&access_token, &url).await {
                    Ok(p) => break Some(p),
                    Err(e) if attempt < 2 => {
                        attempt += 1;
                        warn!("fetch_tracks_page({id} @{offset}) attempt {attempt} failed: {e}");
                        tokio::time::sleep(std::time::Duration::from_millis(400 * attempt as u64))
                            .await;
                    }
                    Err(e) => {
                        warn!("fetch_tracks_page({id} @{offset}) failed for good: {e}");
                        resp.send(WorkerResponse::PlaylistFailed {
                            id: id.clone(),
                            error: e.to_string(),
                        });
                        break None;
                    }
                }
            };
            let Some(page) = page else {
                break;
            };
            total = page.total;
            let next = offset + page_size;
            let done = page.raw_count < page_size
                || page.raw_count == 0
                || (total > 0 && next >= total)
                || accumulated.len() + page.tracks.len() >= MAX_STREAM_TRACKS;
            accumulated.extend(page.tracks.iter().cloned());
            if first {
                let detail = api::PlaylistDetail {
                    id: id.clone(),
                    name: name.clone(),
                    owner: owner.clone(),
                    image_url: image_url.clone(),
                    context_uri: context_uri.clone(),
                    tracks: page.tracks,
                    total,
                };
                resp.send(WorkerResponse::PlaylistOpened {
                    detail,
                    complete: done,
                });
                first = false;
            } else {
                resp.send(WorkerResponse::PlaylistTracks {
                    id: id.clone(),
                    tracks: page.tracks,
                    done,
                });
            }
            if done {
                break;
            }
            offset = next;
        }

        // 4. Write the assembled listing to disk for instant re-opens.
        if !first {
            let detail = api::PlaylistDetail {
                id: id.clone(),
                name,
                owner,
                image_url,
                context_uri,
                tracks: accumulated,
                total,
            };
            let key = detail_cache_key(&id);
            tokio::task::spawn_blocking(move || disk_cache::write_json(&key, &detail));
        }
    });
}

/// Global cap on concurrent album-art network fetches. Spotify's CDN
/// generally tolerates parallel requests, but a full Home view can
/// kick off 30–50 covers at once; throttling keeps us friendly + means
/// a 429 from anywhere can't snowball into a flood of retries.
const ART_CONCURRENCY: usize = 4;

fn art_throttle() -> &'static Arc<tokio::sync::Semaphore> {
    static SEM: std::sync::OnceLock<Arc<tokio::sync::Semaphore>> = std::sync::OnceLock::new();
    SEM.get_or_init(|| Arc::new(tokio::sync::Semaphore::new(ART_CONCURRENCY)))
}

/// Fetch the raw image bytes for `url`. Honors `Retry-After` on 429 +
/// retries once on transient failure. Caller-side throttle in
/// `spawn_fetch_album_art` bounds concurrency.
async fn fetch_art_bytes(url: &str) -> Option<Vec<u8>> {
    for attempt in 1..=2 {
        let resp = match reqwest::get(url).await {
            Ok(r) => r,
            Err(e) => {
                warn!("album art fetch failed ({url}) attempt {attempt}: {e}");
                continue;
            }
        };
        let status = resp.status();
        // 429: back off for the server-advertised window before retrying.
        // 5xx: brief pause + retry once.
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            let wait = resp
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(2);
            warn!("album art 429 ({url}) — sleeping {wait}s");
            tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
            continue;
        }
        if !status.is_success() {
            warn!("album art status {status} ({url}) attempt {attempt}");
            if status.is_server_error() && attempt == 1 {
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }
            continue;
        }
        match resp.bytes().await {
            Ok(b) => return Some(b.to_vec()),
            Err(e) => warn!("album art read body failed ({url}) attempt {attempt}: {e}"),
        }
    }
    None
}

fn spawn_fetch_album_art(resp: Responder, uploader: Arc<Uploader>, url: String, key: String) {
    tokio::spawn(async move {
        // 1. Disk cache first — a hit skips the network entirely, which
        //    is what kills the track-change "stuck on old art" window for
        //    any cover seen before and stops re-hammering the CDN.
        let key_for_disk = key.clone();
        let cached = tokio::task::spawn_blocking(move || disk_cache::read(&key_for_disk))
            .await
            .ok()
            .flatten();
        let (bytes, from_network): (Vec<u8>, bool) = match cached {
            Some(b) => {
                debug!("album art disk-cache hit key={key}");
                (b, false)
            }
            None => {
                // Bound concurrent network fetches across all in-flight
                // art tasks. Held only for the actual GET (decode + atlas
                // upload run uncapped).
                let _permit = art_throttle().acquire().await.ok();
                match fetch_art_bytes(&url).await {
                    Some(b) => (b, true),
                    None => {
                        resp.send(WorkerResponse::AlbumArtFailed { key });
                        return;
                    }
                }
            }
        };
        // 2. Decode off the network task — image::decode is blocking CPU
        //    work that would stall the tokio worker. Accent extraction +
        //    the disk write-back (network fetches only) ride the same
        //    spawn_blocking so we never re-walk the buffer on the UI side.
        let key_for_decode = key.clone();
        let decoded = tokio::task::spawn_blocking(move || {
            if from_network {
                disk_cache::write(&key_for_decode, &bytes);
            }
            let (w, h, rgba) = album_art::decode_to_rgba(&bytes, ALBUM_ART_MAX_DIM)?;
            // Provisional accent (a later `AccentReady` overrides it) —
            // lifted so even a muddy pixel-average reads on the chrome.
            let accent = color::lift_for_chrome(album_art::extract_accent(&rgba, w, h));
            let luma = album_art::mean_luminance(&rgba);
            Some((w, h, rgba, accent, luma))
        })
        .await
        .ok()
        .flatten();
        let Some((w, h, rgba, accent, luma)) = decoded else {
            warn!("album art decode failed for key={key}");
            resp.send(WorkerResponse::AlbumArtFailed { key });
            return;
        };
        // Hand off to the UI thread for atlas upload. Callback fires on
        // the UI thread and ships the resolved handle back through the
        // existing response channel.
        let resp_for_cb = resp.clone();
        uploader.upload_rgba(w, h, rgba, move |maybe_handle| match maybe_handle {
            Some(handle) => resp_for_cb.send(WorkerResponse::AlbumArtReady {
                key,
                handle,
                accent,
                luma,
            }),
            None => {
                warn!("uploader rejected album art upload");
                resp_for_cb.send(WorkerResponse::AlbumArtFailed { key });
            }
        });
    });
}

#[allow(clippy::too_many_arguments)]
fn spawn_connect_session(
    resp: Responder,
    session_slot: Arc<AsyncMutex<Option<Session>>>,
    spirc_slot: Arc<AsyncMutex<Option<Spirc>>>,
    local_ctx: Arc<std::sync::Mutex<Option<String>>>,
    access_token: String,
    initial_volume: f32,
    quality: crate::prefs::AudioQuality,
    normalize: bool,
    eq: Arc<crate::audio_eq::EqShared>,
) {
    tokio::spawn(async move {
        let s = spotify_session::new_session();
        *session_slot.lock().await = Some(s.clone());

        let creds = Credentials::with_access_token(access_token);
        let boot =
            match spirc_bootstrap::start(s, creds, initial_volume, quality, normalize, eq).await {
                Ok(b) => b,
                Err(e) => {
                    error!("spirc bootstrap failed: {e}");
                    resp.send(WorkerResponse::SpotifySessionFailed {
                        error: e.to_string(),
                    });
                    return;
                }
            };
        info!("spirc connect device registered as 'Opal'");
        let spirc_bootstrap::SpircBootstrap {
            spirc,
            spirc_task,
            cluster_sub,
            player_events,
        } = boot;
        *spirc_slot.lock().await = Some(spirc);

        // Drive the Connect device event loop. If it ends, the dealer/AP
        // socket dropped (a long-idle session, a network blip) — the device
        // is offline and playback can't recover on its own. Back off briefly,
        // then ask the host to reconnect with a fresh token (the reducer
        // re-dispatches `ConnectSpotifySession`); the back-off keeps a hard
        // failure from hot-looping.
        let resp_for_task = resp.clone();
        tokio::spawn(async move {
            spirc_task.await;
            warn!("spirc_task ended — Connect device offline, reconnecting");
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            resp_for_task.send(WorkerResponse::SpotifySessionLost);
        });

        // Drain cluster updates into UI-thread responses (remote devices'
        // playback, the active device's volume, and which device is
        // active — `is_self` lights the "playing on Opal" chrome).
        let resp_for_cluster = resp.clone();
        let self_device_id = {
            let guard = session_slot.lock().await;
            guard
                .as_ref()
                .map(|s| s.device_id().to_string())
                .unwrap_or_default()
        };
        let self_id_for_connected = self_device_id.clone();
        let self_id_for_local = self_device_id.clone();
        // Last-announced active device id, SHARED between the cluster and
        // local-player tasks. It must be shared: the dealer never echoes our
        // own connect-state, so a transfer *to* Opal produces no cluster
        // push — the local task announces it instead. If the dedup were
        // per-task, the cluster task's stale `last_active` would then also
        // swallow the *next* remote switch, leaving the devices UI stuck.
        let last_active = std::sync::Arc::new(std::sync::Mutex::new(None::<String>));
        let last_active_cluster = last_active.clone();
        let local_ctx_cluster = local_ctx.clone();
        tokio::spawn(async move {
            cluster_listener::run(
                cluster_sub,
                move |player, volume, active_device, queue, vanished| {
                    if let Some(v) = volume {
                        resp_for_cluster.send(WorkerResponse::VolumeChanged { fraction: v });
                    }
                    // Emit BEFORE the PlayerState below: the reducer reads the
                    // still-live snapshot (correct track + position) to decide the
                    // takeover, before PlayerState wipes it to "stopped".
                    if vanished {
                        resp_for_cluster.send(WorkerResponse::ActiveDeviceVanished);
                    }
                    {
                        let mut la = last_active_cluster.lock().unwrap();
                        if *la != active_device {
                            *la = active_device.clone();
                            let id = active_device.unwrap_or_default();
                            let is_self = !id.is_empty() && id == self_device_id;
                            // A remote took over: the context we stored for local
                            // playback no longer describes what's playing, so drop
                            // it — otherwise a later transfer back to Opal could
                            // stamp a stale source pill before a fresh load.
                            if !id.is_empty() && !is_self {
                                *local_ctx_cluster.lock().unwrap() = None;
                            }
                            resp_for_cluster.send(WorkerResponse::ActiveDeviceChanged {
                                is_self,
                                device_id: id,
                            });
                        }
                    }
                    // Full, live queue off connect-state — replaces the capped
                    // Web API list whenever a remote device is the active player.
                    if let Some(tracks) = queue {
                        resp_for_cluster.send(WorkerResponse::QueueLoaded { tracks });
                    }
                    resp_for_cluster.send(WorkerResponse::PlayerState { player });
                },
            )
            .await;
        });

        // Drain LOCAL player events — the only state source while
        // Opal itself is the active device (no dealer self-echo).
        let resp_for_local = resp.clone();
        let resp_for_local_vol = resp.clone();
        let last_active_local = last_active;
        let local_ctx_local = local_ctx;
        tokio::spawn(async move {
            crate::local_player::run(
                player_events,
                move |mut player| {
                    let playing = player.as_ref().map(|p| p.is_playing).unwrap_or(false);
                    let mut claim_self = false;
                    {
                        let mut la = last_active_local.lock().unwrap();
                        let remote_active = la
                            .as_deref()
                            .map(|a| !a.is_empty() && a != self_id_for_local)
                            .unwrap_or(false);
                        // Playback moved to a REMOTE device: our Spirc winds
                        // down and emits a stray pause/stop. The cluster is
                        // authoritative for the remote, so ignore these local
                        // events — otherwise they flip the UI to "paused" (and
                        // re-claim Opal as active) over the remote's real
                        // playing state. A local *playing* event still means
                        // Opal is genuinely active (transfer-to-self, which the
                        // dealer never echoes), so it's always honoured.
                        if remote_active && !playing {
                            return;
                        }
                        // Opal is emitting live playback ⇒ it is the active
                        // Connect device. Announce once per activation so the
                        // chrome leaves the previous remote's highlight,
                        // updating the shared dedup so a later switch back to a
                        // remote still fires.
                        if player.is_some() && la.as_deref() != Some(self_id_for_local.as_str()) {
                            *la = Some(self_id_for_local.clone());
                            claim_self = true;
                        }
                    }
                    if claim_self {
                        resp_for_local.send(WorkerResponse::ActiveDeviceChanged {
                            is_self: true,
                            device_id: self_id_for_local.clone(),
                        });
                    }
                    // Stamp the context Opal loaded onto this state — librespot
                    // events carry none, so the source pill would otherwise be
                    // blank. Clearing the name forces the reducer's Web-API
                    // resolver to name the (possibly changed) context.
                    if let Some(p) = player.as_mut()
                        && let Some(ctx) = local_ctx_local.lock().unwrap().clone()
                        && p.context_uri.as_deref() != Some(ctx.as_str())
                    {
                        p.context_uri = Some(ctx);
                        p.context_name = None;
                    }
                    resp_for_local.send(WorkerResponse::PlayerState { player });
                },
                move |fraction| resp_for_local_vol.send(WorkerResponse::VolumeChanged { fraction }),
            )
            .await;
        });

        resp.send(WorkerResponse::SpotifySessionConnected {
            device_id: self_id_for_connected,
        });
    });
}

fn spawn_fetch_devices(resp: Responder, access_token: String) {
    tokio::spawn(async move {
        match api::get_devices(&access_token).await {
            Ok(devices) => resp.send(WorkerResponse::Devices { devices }),
            Err(e) => warn!("get_devices failed: {e}"),
        }
    });
}

fn spawn_transfer(access_token: String, device_id: String, position_ms: Option<u32>) {
    tokio::spawn(async move {
        // The cluster push after the transfer is the UI's confirmation.
        match position_ms {
            // Leaving Opal: the Web API transfer doesn't carry our
            // librespot device's position (the target would restart at
            // 0:00). Transfer PAUSED, seek the target to the position we
            // tracked locally, then resume — all repositioning happens while
            // paused, so there's no audible restart-then-jump.
            Some(pos) => {
                if let Err(e) = api::transfer_playback(&access_token, &device_id, false).await {
                    warn!("transfer to {device_id} failed: {e}");
                    return;
                }
                if let Err(e) = api::seek(&access_token, pos, Some(&device_id)).await {
                    warn!("seek on transfer to {device_id} failed: {e}");
                }
                if let Err(e) = api::play(&access_token, Some(&device_id)).await {
                    warn!("resume on transfer to {device_id} failed: {e}");
                }
            }
            // Transferring between other devices (or to Opal): the source
            // hands its position off correctly, so a plain resume-on-transfer
            // preserves it.
            None => {
                if let Err(e) = api::transfer_playback(&access_token, &device_id, true).await {
                    warn!("transfer to {device_id} failed: {e}");
                }
            }
        }
    });
}

fn spawn_check_saved(resp: Responder, access_token: String, track_id: String) {
    tokio::spawn(async move {
        match api::is_track_saved(&access_token, &track_id).await {
            Ok(saved) => resp.send(WorkerResponse::SavedState { track_id, saved }),
            Err(e) => warn!("is_track_saved({track_id}) failed: {e}"),
        }
    });
}

fn spawn_set_saved(
    resp: Responder,
    membership: MembershipArc,
    access_token: String,
    track_id: String,
    artist_ids: Vec<String>,
    saved: bool,
) {
    tokio::spawn(async move {
        match api::set_track_saved(&access_token, &track_id, saved).await {
            // Echo the committed state (idempotent for the optimistic UI),
            // and drop the stale Liked Songs page cache so a re-open shows
            // the change.
            Ok(()) => {
                tokio::task::spawn_blocking(api::invalidate_liked_tracks);
                // Keep the persisted membership snapshot current so the artist
                // page's "In your library" reflects the like on its next open
                // (a stale reverse-index entry after an unlike is filtered out
                // by the scan's saved-state check, so only adds need patching).
                let uri = format!("spotify:track:{track_id}");
                {
                    let mut guard = membership.lock().await;
                    if saved {
                        guard.liked.insert(uri.clone());
                        for aid in &artist_ids {
                            let v = guard.artist_index.entry(aid.clone()).or_default();
                            if !v.contains(&uri) {
                                v.push(uri.clone());
                            }
                        }
                    } else {
                        guard.liked.remove(&uri);
                    }
                }
                persist_membership(&membership).await;
                resp.send(WorkerResponse::SavedState { track_id, saved });
            }
            Err(e) => {
                warn!("set_track_saved({track_id}, {saved}) failed: {e}");
                // Roll the optimistic heart back.
                resp.send(WorkerResponse::SavedState {
                    track_id,
                    saved: !saved,
                });
            }
        }
    });
}

fn spawn_fetch_queue(resp: Responder, access_token: String) {
    tokio::spawn(async move {
        match api::get_queue(&access_token).await {
            Ok(tracks) => resp.send(WorkerResponse::QueueLoaded { tracks }),
            Err(e) => warn!("get_queue failed: {e}"),
        }
    });
}

/// Disk-cache key for the playlist-membership index snapshot.
// Bump the suffix to discard older snapshots and force one fresh scan:
// `_v2` added `artist_index`; `_v3` re-scans after fixing a decode bug that
// dropped tracks with a null `artists` field (leaving gaps in the index).
const MEMBERSHIP_KEY: &str = "playlist_membership_v3";
/// How many playlists to scan concurrently when (re)building the index.
const MEMBERSHIP_SCAN_CONCURRENCY: usize = 5;

type MembershipArc = Arc<AsyncMutex<crate::model::membership::MembershipSnapshot>>;

/// Persist the current index snapshot to disk (cloned under the lock, written
/// off-thread). Re-read on startup so the heart is correct before any scan.
async fn persist_membership(membership: &MembershipArc) {
    let snap = membership.lock().await.clone();
    tokio::task::spawn_blocking(move || disk_cache::write_json(MEMBERSHIP_KEY, &snap));
}

/// Build (or load from disk) the membership index. Disk hit within the 6h
/// TTL → instant. Miss/stale → scan every editable playlist's track URIs and
/// cache the result. Either way the UI gets the editable playlist list.
fn spawn_load_membership(resp: Responder, membership: MembershipArc, access_token: String) {
    use crate::model::membership::{MembershipPlaylist, MembershipSnapshot};
    tokio::spawn(async move {
        // 1. Fresh disk cache (within TTL) — use as-is, no scan needed.
        let fresh = tokio::task::spawn_blocking(|| {
            disk_cache::read_json::<MembershipSnapshot>(MEMBERSHIP_KEY, api::ttl::MUTABLE)
        })
        .await
        .ok()
        .flatten();
        if let Some(snap) = fresh {
            let playlists = snap.playlists.clone();
            let index = snap.index.clone();
            let artist_index = snap.artist_index.clone();
            let liked = snap.liked.clone();
            info!("membership: {} playlists from disk cache", playlists.len());
            *membership.lock().await = snap;
            resp.send(WorkerResponse::MembershipLoaded {
                playlists,
                index,
                artist_index,
                liked,
            });
            return;
        }
        // 2. Stale-but-present cache — show it *immediately* (so the heart +
        // picker work on startup), then revalidate in the background below and
        // swap the fresh result in when the scan finishes.
        let stale = tokio::task::spawn_blocking(|| {
            disk_cache::read_json::<MembershipSnapshot>(MEMBERSHIP_KEY, std::time::Duration::MAX)
        })
        .await
        .ok()
        .flatten();
        if let Some(snap) = stale {
            let playlists = snap.playlists.clone();
            let index = snap.index.clone();
            let artist_index = snap.artist_index.clone();
            let liked = snap.liked.clone();
            info!(
                "membership: {} playlists from stale cache — revalidating",
                playlists.len()
            );
            *membership.lock().await = snap;
            resp.send(WorkerResponse::MembershipLoaded {
                playlists,
                index,
                artist_index,
                liked,
            });
        }
        // 3. Scan. Editable = owned or collaborative (only those take writes).
        let me = api::get_me(&access_token)
            .await
            .map(|p| p.id)
            .unwrap_or_default();
        let editable: Vec<api::LibraryPlaylist> = match api::get_my_playlists(&access_token).await {
            Ok(all) => all.into_iter().filter(|p| p.editable(&me)).collect(),
            Err(e) => {
                warn!("membership: get_my_playlists failed: {e}");
                return;
            }
        };
        info!("membership: scanning {} editable playlists", editable.len());
        let mut index: std::collections::HashMap<String, Vec<String>> =
            std::collections::HashMap::new();
        let mut artist_index: std::collections::HashMap<String, Vec<String>> =
            std::collections::HashMap::new();
        for chunk in editable.chunks(MEMBERSHIP_SCAN_CONCURRENCY) {
            let futs = chunk.iter().map(|p| {
                let token = access_token.clone();
                let id = p.id.clone();
                async move { (id.clone(), api::playlist_track_uris(&token, &id).await) }
            });
            for (id, res) in futures::future::join_all(futs).await {
                match res {
                    Ok(rows) => {
                        for (uri, artist_ids) in rows {
                            for aid in artist_ids {
                                let v = artist_index.entry(aid).or_default();
                                if !v.contains(&uri) {
                                    v.push(uri.clone());
                                }
                            }
                            index.entry(uri).or_default().push(id.clone());
                        }
                    }
                    Err(e) => warn!("membership: scan playlist {id} failed: {e}"),
                }
            }
        }
        // Fold Liked Songs into the reverse index too — "saved" means liked
        // OR in an editable playlist, and a liked-only track must still show
        // on the artist page. (`/me/tracks` dodges the Dev-Mode `/items`
        // 403s.) Kept out of the forward `index`, which is playlists only.
        let mut liked: std::collections::HashSet<String> = std::collections::HashSet::new();
        match api::liked_track_artist_uris(&access_token).await {
            Ok(rows) => {
                for (uri, artist_ids) in rows {
                    for aid in artist_ids {
                        let v = artist_index.entry(aid).or_default();
                        if !v.contains(&uri) {
                            v.push(uri.clone());
                        }
                    }
                    liked.insert(uri);
                }
            }
            Err(e) => warn!("membership: liked scan failed: {e}"),
        }
        let playlists: Vec<MembershipPlaylist> = editable
            .into_iter()
            .map(|p| MembershipPlaylist {
                id: p.id,
                name: p.name,
            })
            .collect();
        info!(
            "membership: index built — {} playlist tracks across {} playlists, {} liked, {} artists",
            index.len(),
            playlists.len(),
            liked.len(),
            artist_index.len(),
        );
        let index_for_ui = index.clone();
        let artist_index_for_ui = artist_index.clone();
        let liked_for_ui = liked.clone();
        *membership.lock().await = MembershipSnapshot {
            playlists: playlists.clone(),
            index,
            artist_index,
            liked,
        };
        persist_membership(&membership).await;
        resp.send(WorkerResponse::MembershipLoaded {
            playlists,
            index: index_for_ui,
            artist_index: artist_index_for_ui,
            liked: liked_for_ui,
        });
    });
}

/// Look up the current track's playlist membership against the index.
fn spawn_query_membership(resp: Responder, membership: MembershipArc, track_uri: String) {
    tokio::spawn(async move {
        let playlist_ids = membership
            .lock()
            .await
            .index
            .get(&track_uri)
            .cloned()
            .unwrap_or_default();
        resp.send(WorkerResponse::TrackMembership {
            track_uri,
            playlist_ids,
        });
    });
}

/// Add/remove a track to/from a playlist, then update the index + disk cache.
/// On API failure, tell the UI to roll its optimistic checkbox back.
fn spawn_edit_membership(
    resp: Responder,
    membership: MembershipArc,
    access_token: String,
    playlist_id: String,
    track_uri: String,
    artist_ids: Vec<String>,
    add: bool,
) {
    tokio::spawn(async move {
        let res = if add {
            api::add_to_playlist(&access_token, &playlist_id, &track_uri).await
        } else {
            api::remove_from_playlist(&access_token, &playlist_id, &track_uri).await
        };
        if let Err(e) = res {
            warn!("membership edit ({playlist_id}, add={add}) failed: {e}");
            resp.send(WorkerResponse::MembershipEditFailed {
                track_uri,
                playlist_id,
                was_add: add,
            });
            return;
        }
        // Update the in-memory index for `track_uri`, then re-persist so the
        // artist page's next open sees it. Only adds patch the reverse index
        // — a stale entry after a remove is filtered by the scan's
        // saved-state check.
        {
            let mut guard = membership.lock().await;
            let entry = guard.index.entry(track_uri.clone()).or_default();
            if add {
                if !entry.contains(&playlist_id) {
                    entry.push(playlist_id.clone());
                }
                for aid in &artist_ids {
                    let v = guard.artist_index.entry(aid.clone()).or_default();
                    if !v.contains(&track_uri) {
                        v.push(track_uri.clone());
                    }
                }
            } else {
                entry.retain(|p| p != &playlist_id);
                if entry.is_empty() {
                    guard.index.remove(&track_uri);
                }
            }
        }
        persist_membership(&membership).await;
        // Drop the playlist's cached track-list pages so re-opening it
        // shows the just-added/removed track instead of stale data.
        tokio::task::spawn_blocking(move || api::invalidate_playlist_tracks(&playlist_id));
    });
}

/// Proactive token refresh (mid-session — the startup path lives in
/// `spawn_try_load`). Persists the rotated tokens so the next launch
/// starts from the fresh pair.
fn spawn_refresh_tokens(resp: Responder, refresh: String, client_id: String) {
    tokio::spawn(async move {
        match refresh_token(&refresh, &client_id).await {
            Ok(auth) => {
                info!("access token refreshed proactively");
                let prev = token_manager::load_tokens().ok();
                let stored = StoredTokens::from_refresh(auth.clone(), prev.as_ref());
                let _ = token_manager::save_tokens(&stored);
                resp.send(WorkerResponse::TokensRefreshed { auth });
            }
            Err(e) => {
                warn!("proactive token refresh failed: {e}");
                resp.send(WorkerResponse::TokensRefreshFailed);
            }
        }
    });
}

fn spawn_try_load(resp: Responder, client_id: String) {
    tokio::spawn(async move {
        match token_manager::load_tokens() {
            Ok(tokens) => {
                // Self-heal: if the stored token was minted before a
                // scope addition (constants.rs SPOTIFY_ACCESS_SCOPES),
                // it'll 401 on the new endpoints. Drop and force re-auth.
                if !tokens.has_scopes(crate::constants::SPOTIFY_ACCESS_SCOPES) {
                    info!("stored token missing required scopes — wiping + re-auth");
                    let _ = token_manager::delete_tokens();
                    resp.send(WorkerResponse::NoStoredTokens);
                    return;
                }
                // Spotify caps refresh tokens at ~180 days. Past that the
                // refresh grant fails, so wipe the dead credentials and send
                // the user back to the login screen instead of erroring.
                if tokens.refresh_expired() {
                    info!("refresh token older than 180 days — wiping + re-auth");
                    let _ = token_manager::delete_tokens();
                    resp.send(WorkerResponse::NoStoredTokens);
                    return;
                }
                if tokens.is_expired() {
                    info!("refreshing expired token");
                    match refresh_token(&tokens.refresh_token, &client_id).await {
                        Ok(auth) => {
                            let stored = StoredTokens::from_refresh(auth.clone(), Some(&tokens));
                            let _ = token_manager::save_tokens(&stored);
                            resp.send(WorkerResponse::TokensLoaded { auth });
                        }
                        Err(e) => {
                            error!("refresh failed: {e}");
                            resp.send(WorkerResponse::NoStoredTokens);
                        }
                    }
                } else {
                    resp.send(WorkerResponse::TokensLoaded {
                        auth: tokens.to_auth_response(),
                    });
                }
            }
            Err(e) => {
                debug!("no stored tokens: {e}");
                resp.send(WorkerResponse::NoStoredTokens);
            }
        }
    });
}
