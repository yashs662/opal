//! Stand-in for platforms without an engine implementation.
//!
//! Keeps the call sites (worker, views, exit hook) free of `cfg` noise: they
//! call the same functions everywhere, and here they report "no client, can
//! do nothing". [`super::SUPPORTED`] is what the UI checks to hide the
//! setting outright.

use std::path::PathBuf;

use super::{EngineState, Restore};

pub fn locate() -> Option<PathBuf> {
    None
}

pub fn acquire() -> Result<EngineState, String> {
    Err("the Spotify-client engine is only supported on Windows".to_string())
}

pub fn release(_restore: Restore) {}

pub fn enforce_window_state() -> bool {
    false
}
