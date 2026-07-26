//! OS media integration — registers Opal as a media app.
//!
//! Wraps [`souvlaki`]: SMTC on Windows (the volume-flyout media panel +
//! global hardware media keys), MPRIS over D-Bus on Linux, and
//! `MPNowPlayingInfoCenter` / `MPRemoteCommandCenter` on macOS.
//!
//! Two directions, both driven from the frame tick:
//! - **In**: hardware media keys / OS panel buttons land on souvlaki's
//!   background thread; the attach callback forwards them onto an mpsc
//!   channel and wakes the loop. [`MediaModel::drain`] hands them to the
//!   tick, which maps them to the same `Msg::Transport` intents the
//!   player-bar buttons emit — one transport path for every input.
//! - **Out**: [`MediaModel::sync`] diffs the player snapshot each tick
//!   and pushes metadata (title/artist/cover/duration) on track change
//!   and playback status (+ position anchor) on play/pause — so the OS
//!   panel mirrors the chrome without per-frame OS calls.

use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

use opal_gfx::WakeHandle;
use souvlaki::{
    MediaControlEvent, MediaControls, MediaMetadata, MediaPlayback, MediaPosition, PlatformConfig,
};

use crate::api::CurrentlyPlaying;

pub struct MediaModel {
    controls: MediaControls,
    rx: Receiver<MediaControlEvent>,
    /// Track id of the last metadata push — dedups `set_metadata`.
    last_track: Option<String>,
    /// Last pushed playback flag — dedups `set_playback`.
    last_playing: Option<bool>,
}

impl MediaModel {
    /// Register with the OS. `hwnd` is the main window's Win32 handle
    /// (required by SMTC; `None` and unused elsewhere). Fail-soft: the
    /// integration is a nicety, so a platform error just logs and the
    /// app runs without it.
    pub fn new(hwnd: Option<*mut std::ffi::c_void>, wake: Arc<WakeHandle>) -> Option<Self> {
        #[cfg(windows)]
        if hwnd.is_none() {
            log::warn!("media controls: no HWND — SMTC registration skipped");
            return None;
        }
        let config = PlatformConfig {
            dbus_name: "opal",
            display_name: "Opal",
            hwnd,
        };
        let mut controls = match MediaControls::new(config) {
            Ok(c) => c,
            Err(e) => {
                log::warn!("media controls unavailable: {e:?}");
                return None;
            }
        };
        let (tx, rx): (Sender<MediaControlEvent>, Receiver<MediaControlEvent>) = channel();
        if let Err(e) = controls.attach(move |event| {
            // souvlaki's thread — just queue + wake; the frame tick drains.
            let _ = tx.send(event);
            wake.wake();
        }) {
            log::warn!("media controls attach failed: {e:?}");
            return None;
        }
        log::info!("media controls registered");
        Some(Self {
            controls,
            rx,
            last_track: None,
            last_playing: None,
        })
    }

    /// Media-key / OS-panel events queued since the last tick.
    pub fn drain(&mut self) -> Vec<MediaControlEvent> {
        self.rx.try_iter().collect()
    }

    /// Mirror the player state out to the OS panel. Cheap when nothing
    /// changed (two compares); pushes only on a track or playing edge.
    /// `progress_ms` anchors the OS timeline at each push — the panel
    /// interpolates (or just displays it), we don't stream per-frame.
    pub fn sync(&mut self, snap: Option<&CurrentlyPlaying>, playing: bool, progress_ms: u64) {
        let Some(p) = snap else {
            if self.last_track.take().is_some() {
                self.last_playing = None;
                let _ = self.controls.set_playback(MediaPlayback::Stopped);
            }
            return;
        };
        let track_changed = self.last_track.as_deref() != Some(p.track_id.as_str());
        if track_changed {
            self.last_track = Some(p.track_id.clone());
            let meta = MediaMetadata {
                title: Some(p.name.as_str()),
                artist: Some(p.artist.as_str()),
                album: None,
                cover_url: p.album_image_url.as_deref(),
                duration: Some(Duration::from_millis(p.duration_ms)),
            };
            if let Err(e) = self.controls.set_metadata(meta) {
                log::warn!("media controls set_metadata: {e:?}");
            }
        }
        if track_changed || self.last_playing != Some(playing) {
            self.last_playing = Some(playing);
            let progress = Some(MediaPosition(Duration::from_millis(progress_ms)));
            let status = if playing {
                MediaPlayback::Playing { progress }
            } else {
                MediaPlayback::Paused { progress }
            };
            if let Err(e) = self.controls.set_playback(status) {
                log::warn!("media controls set_playback: {e:?}");
            }
        }
    }
}
