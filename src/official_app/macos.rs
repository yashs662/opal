//! macOS implementation of the official-client engine.
//!
//! Everything goes through `NSRunningApplication`: public AppKit, thread
//! safe, and it needs no Accessibility or Automation consent — hiding an app
//! this way is exactly what ⌘H does. "Hidden" here is app-level (no windows
//! on screen, still in the Dock, still playing), the mac analogue of the
//! Win32 `SW_HIDE` the Windows twin uses on the main window.
//!
//! Launching goes through `open -j`, which starts the app already hidden,
//! so a fresh client never flashes on screen at all.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use objc2::rc::Retained;
use objc2_app_kit::NSRunningApplication;
use objc2_foundation::NSString;

use super::{ENGINE, EngineState, Ownership, Restore, show_window};

const BUNDLE_ID: &str = "com.spotify.client";

/// How long to wait for a freshly-launched client to finish launching.
/// Cold start is a few seconds; the poll exits as soon as it's up, so this
/// is only the giving-up point.
const LAUNCH_WAIT: Duration = Duration::from_secs(20);
const LAUNCH_POLL: Duration = Duration::from_millis(250);

/// Where the desktop client lives: the system Applications folder, or the
/// per-user one for a drag-install that skipped admin rights.
pub fn locate() -> Option<PathBuf> {
    let candidates = [
        Some(PathBuf::from("/Applications/Spotify.app")),
        dirs::home_dir().map(|h| h.join("Applications").join("Spotify.app")),
    ];
    candidates.into_iter().flatten().find(|p| p.exists())
}

/// The live client, if any. A fresh object each call — `NSRunningApplication`
/// snapshots its time-varying properties, so re-fetching is how a poll sees
/// a state change.
fn client() -> Option<Retained<NSRunningApplication>> {
    let apps = NSRunningApplication::runningApplicationsWithBundleIdentifier(&NSString::from_str(
        BUNDLE_ID,
    ));
    apps.to_vec().into_iter().find(|a| !a.isTerminated())
}

/// Bring the engine up, whatever state it's in, and put it in the state
/// [`show_window`] asks for (hidden by default).
///
/// - **not running** → `open` it (already hidden unless the policy says
///   show), wait for launch to finish (`Launched`)
/// - **running** → apply the policy, leave the process alone (`Adopted`)
///
/// Unlike Windows, a hidden-but-running client is *not* treated as an
/// orphan: ⌘H is an ordinary thing for a user to have done, and a hidden
/// app is plainly visible in the Dock, so there is nothing invisible to
/// clean up.
pub fn acquire() -> Result<EngineState, String> {
    if let Some(app) = client() {
        let hidden_by_us = if show_window() {
            if app.isHidden() {
                app.unhide();
            }
            log::info!("official client adopted, left on screen");
            false
        } else if app.isHidden() {
            log::info!("official client adopted, already hidden");
            false
        } else {
            app.hide();
            log::info!("official client adopted and hidden");
            true
        };
        let state = EngineState {
            ownership: Ownership::Adopted,
            hidden_by_us,
        };
        *ENGINE.lock().unwrap() = Some(state);
        return Ok(state);
    }

    let bundle = locate().ok_or_else(|| "Spotify desktop client not installed".to_string())?;
    log::info!(
        "launching official client as playback engine: {}",
        bundle.display()
    );
    let hidden = !show_window();
    // `-g`: don't bring it to the foreground. `-j`: launch hidden.
    let mut cmd = std::process::Command::new("open");
    cmd.arg("-g");
    if hidden {
        cmd.arg("-j");
    }
    cmd.arg("-a").arg(&bundle);
    let status = cmd
        .status()
        .map_err(|e| format!("launching Spotify: {e}"))?;
    if !status.success() {
        return Err(format!("launching Spotify: open exited with {status}"));
    }

    let deadline = Instant::now() + LAUNCH_WAIT;
    loop {
        if let Some(app) = client()
            && app.isFinishedLaunching()
        {
            // `-j` is only a launch hint; make the policy stick.
            if hidden && !app.isHidden() {
                app.hide();
            }
            log::info!(
                "official client up ({})",
                if hidden { "hidden" } else { "on screen" }
            );
            let state = EngineState {
                ownership: Ownership::Launched,
                hidden_by_us: hidden,
            };
            *ENGINE.lock().unwrap() = Some(state);
            return Ok(state);
        }
        if Instant::now() >= deadline {
            return Err("Spotify client did not start".to_string());
        }
        std::thread::sleep(LAUNCH_POLL);
    }
}

/// Undo [`acquire`]: unhide a client we hid, and quit one we started. A
/// client the user was already running keeps running.
///
/// Idempotent — releasing when nothing is held does nothing, so the exit
/// hook can call it unconditionally.
pub fn release(restore: Restore) {
    let Some(state) = ENGINE.lock().unwrap().take() else {
        return;
    };
    let Some(app) = client() else {
        return;
    };
    match state.ownership {
        Ownership::Launched => {
            // Ours: a graceful quit request, which returns immediately —
            // the client tears itself down after Opal is gone.
            log::info!("closing official client (launched by Opal)");
            if !app.terminate() {
                log::warn!("closing official client failed");
            }
        }
        Ownership::Adopted => {
            if state.hidden_by_us {
                match restore {
                    Restore::Show => {
                        log::info!("restoring official client (unhide)");
                        app.unhide();
                    }
                    // Opal is quitting: a hidden app is the mac's "minimised
                    // and reachable" — in the Dock, still playing, nothing
                    // leaps on screen. Leave it exactly there.
                    Restore::Minimized => {
                        log::info!("leaving official client hidden (Dock-reachable)");
                    }
                }
            }
        }
    }
}

/// Drive the client to whatever [`show_window`] currently asks for, and
/// report whether anything had to move. Re-hides a client that unhid itself
/// (a `spotify:` link handoff, a Dock click) and applies a flip of the
/// "show the Spotify window" setting. No-ops when it already matches.
pub fn enforce_window_state() -> bool {
    let Some(app) = client() else {
        return false;
    };
    let want_hidden = !show_window();
    if app.isHidden() == want_hidden {
        return false;
    }
    if want_hidden {
        log::debug!("official client on screen — hiding it");
        app.hide();
    } else {
        log::debug!("showing the official client");
        app.unhide();
    }
    if let Ok(mut g) = ENGINE.lock()
        && let Some(state) = g.as_mut()
    {
        state.hidden_by_us = want_hidden;
    }
    true
}

/// Discovery diagnostic — run against a live client:
/// `cargo test official_app -- --ignored --nocapture`. Read-only.
#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "requires the official client installed/running"]
    fn discovery() {
        println!("bundle:   {:?}", super::locate());
        let app = super::client();
        println!("running:  {}", app.is_some());
        if let Some(a) = app {
            println!("hidden:   {}", a.isHidden());
            println!("launched: {}", a.isFinishedLaunching());
        }
    }
}
