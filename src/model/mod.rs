//! Application model — the app's state decomposed into cohesive,
//! self-owned sub-models (one per domain) instead of the former single
//! `AppState` god-struct.
//!
//! Each sub-model owns its slice of reactive state (Rc-backed `Signal`s
//! plus plain data) and the methods that mutate it. State lives here, on
//! the host; views (in the `opal` binary crate) read it and bind to
//! it. This split is also what keeps future hot-reload viable —
//! subsecond can patch view/logic fn bodies but cannot reload struct
//! layout, so the model structs stay put while the fns around them
//! reload.
//!
//! Migration is incremental: slices move out of `main.rs::AppState` one
//! at a time, each compiling green. Already migrated:
//!   - [`art`] — shared cover/accent/track-detail resolution cache.
//!   - [`auth`] — live OAuth session + token accessor.
//!   - [`backdrop`] — album-art backdrop + accent crossfade.
//!   - [`canvas`] — Spotify Canvas video decode + dim/hover.
//!   - [`library`] — Home feed data + playlist loading/caching.
//!   - [`player`] — reactive player-chrome + authoritative snapshot.
//!   - [`prefs`] — persisted preferences + panel widths + debounced save.
//!   - [`router`] — view + centre-pane nav + entrance transition.
//!   - [`settings`] — settings modal overlay + cache usage + dir handoff.

pub mod art;
pub mod auth;
pub mod backdrop;
pub mod canvas;
pub mod devices;
pub mod eq;
pub mod library;
pub mod membership;
pub mod menu;
pub mod player;
pub mod prefs;
pub mod router;
pub mod settings;

pub use art::ArtModel;
pub use auth::AuthModel;
pub use backdrop::BackdropModel;
pub use canvas::CanvasModel;
pub use devices::DevicesModel;
pub use eq::EqModel;
pub use library::LibraryModel;
pub use membership::MembershipModel;
pub use menu::{MenuModel, MenuTarget};
pub use player::PlayerModel;
pub use prefs::PrefsModel;
pub use router::RouterModel;
pub use settings::SettingsModel;
