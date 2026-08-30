//! The official Spotify client, driven as a headless playback engine.
//!
//! Spotify streams lossless FLAC only to its own apps — every Connect
//! endpoint, librespot included, gets Ogg Vorbis 320 regardless of account
//! tier. So for lossless, Opal runs the real client with its window hidden
//! and drives it over Connect like any other device: Opal is the UI, the
//! official client is the audio pipeline. The hiding is a policy, not a
//! requirement — see [`set_show_window`] for the "show the Spotify window"
//! setting.
//!
//! Nothing here modifies Spotify. It is process launch plus `ShowWindow` —
//! the same calls a taskbar utility makes.
//!
//! # Ownership
//!
//! Whether *we* started the process decides what happens on the way out. A
//! client we launched is ours to close; one the user already had running is
//! theirs, so we only ever restore the window we hid. See [`Ownership`].

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

/// The engine we're currently holding, if any.
///
/// Process-global on purpose: what we owe the user on the way out (re-show a
/// window, close a process we launched) has to be discharged from whichever
/// thread gets there first — the worker on a toggle, or the shell's exit
/// hook when Opal closes. Splitting that record between the two invites
/// exactly the leak this guards against: a hidden client with no owner.
static ENGINE: Mutex<Option<EngineState>> = Mutex::new(None);

/// Whether the engine is currently held (drives the exit hook + UI truth).
pub fn is_active() -> bool {
    ENGINE.lock().is_ok_and(|g| g.is_some())
}

/// User policy for the engine's window: hidden (the default — Opal is the
/// UI) or on screen. Read by [`acquire`] and by the worker's periodic
/// [`enforce_window_state`] tick, both off the UI thread, so it lives in an
/// atomic rather than the app state.
static SHOW_WINDOW: AtomicBool = AtomicBool::new(false);

/// Whether the user asked to see the client's own window.
pub fn show_window() -> bool {
    SHOW_WINDOW.load(Ordering::Relaxed)
}

/// Set the window policy. Takes effect on the next [`enforce_window_state`]
/// (the worker applies it immediately on a toggle) and on the next
/// [`acquire`].
pub fn set_show_window(show: bool) {
    SHOW_WINDOW.store(show, Ordering::Relaxed);
}

/// Who started the client, which decides the teardown.
#[cfg_attr(not(windows), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ownership {
    /// Opal launched it — closing it again on the way out is fair game and
    /// reclaims its (substantial) memory.
    Launched,
    /// It was already running. We may have hidden its window, but the
    /// process belongs to the user: restore the window, never kill it.
    Adopted,
}

/// The engine's live state, held by the worker across commands.
#[cfg_attr(not(windows), allow(dead_code))]
#[derive(Debug, Clone, Copy)]
pub struct EngineState {
    pub ownership: Ownership,
    /// True if *we* hid the window, i.e. we owe the user a re-show. A client
    /// the user had already minimised to tray is left exactly as found.
    pub hidden_by_us: bool,
}

/// How a released window should come back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Restore {
    /// On screen — the user turned the engine off, which is how they reach
    /// the client (its own audio-quality setting lives in there).
    Show,
    /// Minimised — Opal is quitting. The client stays reachable in the
    /// taskbar and keeps playing, without a window leaping up as the app
    /// the user just closed disappears.
    Minimized,
}

/// Whether this platform can host the engine at all.
///
/// Hiding another application's window is inherently platform-specific:
/// Win32 does it with one `ShowWindow` call and no permission, macOS needs
/// Accessibility/Automation consent through AppleScript, and Wayland has no
/// protocol for it at all. Only Windows is implemented — the UI reads this
/// to leave the setting out entirely rather than offer a control that can
/// only fail.
pub const SUPPORTED: bool = cfg!(windows);

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::{acquire, enforce_window_state, locate, release};

#[cfg(not(windows))]
mod unsupported;
#[cfg(not(windows))]
pub use unsupported::{acquire, enforce_window_state, locate, release};
