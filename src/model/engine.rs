//! Playback-engine slice — the official Spotify client driven as a hidden
//! lossless engine (see [`crate::official_app`]).
//!
//! Exists because "is the engine on" is not a boolean the preference can
//! answer. Bringing the client up takes seconds (cold launch, window wait,
//! Connect registration), and it can fail outright — so the UI needs the
//! difference between *asked for*, *coming up*, and *actually playing*, or
//! it lies to the user in the gap. The pref only records intent; this
//! records reality.

/// Where the engine is in its lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EngineStatus {
    /// Opal's own device plays; the client isn't involved.
    #[default]
    Off,
    /// Launching / hiding / waiting for its Connect device. The toggle is
    /// locked here — the work is in flight and a second click can't help.
    Starting,
    /// Playing through the client. This is what lights the "Lossless" pill.
    Active,
}

pub struct EngineModel {
    pub status: EngineStatus,
    /// Whether the desktop client is installed at all. Probed once at
    /// startup (a file-exists check); without it the toggle is inert and
    /// says why, rather than failing after the user flips it.
    pub installed: bool,
}

impl EngineModel {
    /// Probes for the client once — cheap, and the answer can't change
    /// meaningfully mid-session.
    pub fn new() -> Self {
        Self {
            status: EngineStatus::Off,
            installed: crate::official_app::SUPPORTED && crate::official_app::locate().is_some(),
        }
    }

    /// The engine is holding playback.
    pub fn is_active(&self) -> bool {
        self.status == EngineStatus::Active
    }

    /// Work is in flight — the toggle must not accept another click.
    pub fn is_busy(&self) -> bool {
        self.status == EngineStatus::Starting
    }

    /// Can the user act on the toggle at all?
    pub fn is_interactive(&self) -> bool {
        self.installed && !self.is_busy()
    }
}

impl Default for EngineModel {
    fn default() -> Self {
        Self::new()
    }
}
