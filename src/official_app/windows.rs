//! Win32 implementation of the official-client engine.
//!
//! Process discovery via `sysinfo`, window discovery via `EnumWindows`
//! over the client's CEF windows, and show/hide via `ShowWindow`. All of
//! it is Windows-only by nature, which is why this file only exists in a
//! Windows build — see the module's `unsupported` twin.

use std::path::PathBuf;
use std::time::{Duration, Instant};

/// How long to wait for a freshly-launched client to create its window.
/// Cold start on a slow disk is a few seconds; the poll below exits as soon
/// as the window appears, so this is only the giving-up point.
const WINDOW_WAIT: Duration = Duration::from_secs(20);
const WINDOW_POLL: Duration = Duration::from_millis(250);

use super::{ENGINE, EngineState, Ownership, Restore, show_window};

/// Where the desktop client lives. The Microsoft Store build lands in
/// `WindowsApps` under a versioned package dir and is launched through its
/// app-execution alias instead, so it is checked second.
pub fn locate() -> Option<PathBuf> {
    let candidates = [
        dirs::data_dir().map(|d| d.join("Spotify").join("Spotify.exe")),
        dirs::data_local_dir().map(|d| d.join("Microsoft").join("WindowsApps").join("Spotify.exe")),
    ];
    candidates.into_iter().flatten().find(|p| p.exists())
}

/// True when at least one Spotify process is alive.
pub fn is_running() -> bool {
    !pids().is_empty()
}

mod win {
    use windows_sys::Win32::Foundation::{HWND, LPARAM, RECT};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetClassNameW, GetWindowRect, GetWindowTextLengthW, GetWindowThreadProcessId,
        IsWindowVisible, SW_HIDE, SW_SHOWMINNOACTIVE, SW_SHOWNOACTIVATE, ShowWindow,
    };
    use windows_sys::core::BOOL;

    /// `EnumWindows` reads this as "keep going". The scan never stops early
    /// — every candidate is collected so the largest can win.
    const CONTINUE: BOOL = 1;

    /// Collected during [`EnumWindows`]: the pids we're looking for, and
    /// every candidate window found for them.
    struct Search {
        pids: Vec<u32>,
        /// `(hwnd, area)` for each candidate, largest wins.
        found: Vec<(HWND, i64)>,
        /// Only consider windows currently on screen (used when hiding).
        visible_only: bool,
    }

    /// Chromium hosts its real windows in this class. Spotify is CEF-based,
    /// so the main window is always one of these — which rules out the
    /// message-only and helper windows its process tree also owns.
    const CEF_CLASS_PREFIX: &str = "Chrome_WidgetWin_";

    fn class_name(hwnd: HWND) -> String {
        let mut buf = [0u16; 128];
        let n = unsafe { GetClassNameW(hwnd, buf.as_mut_ptr(), buf.len() as i32) };
        String::from_utf16_lossy(&buf[..n.max(0) as usize])
    }

    unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let search = unsafe { &mut *(lparam as *mut Search) };
        let mut pid: u32 = 0;
        unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
        if !search.pids.contains(&pid) {
            return CONTINUE;
        }
        // A titled CEF window. Tooltips and popups share the class but are
        // untitled; the main window carries the track name (or "Spotify").
        if unsafe { GetWindowTextLengthW(hwnd) } == 0 {
            return CONTINUE;
        }
        if !class_name(hwnd).starts_with(CEF_CLASS_PREFIX) {
            return CONTINUE;
        }
        if search.visible_only && unsafe { IsWindowVisible(hwnd) } == 0 {
            return CONTINUE;
        }
        // Rank by area: whatever else the client has open (mini-player, a
        // dialog), the main window is the biggest. A hidden window keeps
        // its last rect, so this still works after we've hidden it.
        let mut rect = RECT {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        };
        let area = if unsafe { GetWindowRect(hwnd, &mut rect) } != 0 {
            (rect.right - rect.left) as i64 * (rect.bottom - rect.top) as i64
        } else {
            0
        };
        search.found.push((hwnd, area));
        CONTINUE
    }

    /// The client's main window, if it has one right now. `visible_only`
    /// distinguishes "no window yet" from "window exists but is hidden".
    pub fn main_window(visible_only: bool) -> Option<HWND> {
        let pids = super::pids();
        if pids.is_empty() {
            return None;
        }
        let mut search = Search {
            pids,
            found: Vec::new(),
            visible_only,
        };
        unsafe { EnumWindows(Some(enum_proc), &mut search as *mut Search as LPARAM) };
        search
            .found
            .into_iter()
            .max_by_key(|&(_, area)| area)
            .map(|(hwnd, _)| hwnd)
    }

    /// Exposed for the discovery diagnostic below.
    #[cfg(test)]
    pub fn class_name_of(hwnd: HWND) -> String {
        class_name(hwnd)
    }

    pub fn hide(hwnd: HWND) {
        unsafe { ShowWindow(hwnd, SW_HIDE) };
    }

    /// Restore without stealing focus — the user asked for their client
    /// back, not for it to jump in front of whatever they're doing.
    pub fn show(hwnd: HWND) {
        unsafe { ShowWindow(hwnd, SW_SHOWNOACTIVATE) };
    }

    /// Put it back as a taskbar button rather than on screen: reachable and
    /// still playing, but nothing pops up.
    pub fn show_minimized(hwnd: HWND) {
        unsafe { ShowWindow(hwnd, SW_SHOWMINNOACTIVE) };
    }
}

/// Pids of every live Spotify process. `sysinfo` is already a dependency
/// (librespot uses it for the client-token platform data).
fn pids() -> Vec<u32> {
    use sysinfo::{ProcessRefreshKind, RefreshKind, System};
    let sys = System::new_with_specifics(
        RefreshKind::nothing().with_processes(ProcessRefreshKind::nothing()),
    );
    sys.processes()
        .values()
        .filter(|p| {
            p.name()
                .to_str()
                .is_some_and(|n| n.eq_ignore_ascii_case("spotify.exe"))
        })
        .map(|p| p.pid().as_u32())
        .collect()
}

/// Bring the engine up, whatever state it's in, and put its window in the
/// state [`show_window`] asks for (hidden by default).
///
/// The four cases, all reachable in normal use:
/// - **not running** → launch, wait for the window, hide it (`Launched`)
/// - **running, visible** → hide it, leave the process alone (`Adopted`)
/// - **running, already hidden** → an orphan Opal left behind; claim it as
///   `Launched` so it gets cleaned up this time
/// - **running, no window yet** (mid-startup) → wait for it, then hide
///
/// Returns the state the worker must hold to undo this later.
pub fn acquire() -> Result<EngineState, String> {
    let already_running = is_running();

    if !already_running {
        let exe = locate().ok_or_else(|| "Spotify desktop client not installed".to_string())?;
        log::info!(
            "launching official client as playback engine: {}",
            exe.display()
        );
        std::process::Command::new(&exe)
            // `--minimized` asks it to start out of the way, so there's a
            // smaller window (ha) where it can flash on screen before we
            // hide it. Unrecognised flags are ignored by the client.
            .arg("--minimized")
            .spawn()
            .map_err(|e| format!("launching Spotify: {e}"))?;
    }

    let ownership = if already_running {
        Ownership::Adopted
    } else {
        Ownership::Launched
    };

    // Fast path for a client that was already up: if it *has* a window, we
    // know its state right now and there is nothing to wait for. Skipping
    // this and polling for a visible window would burn the whole WINDOW_WAIT
    // whenever the client is already hidden (an orphan from a previous Opal
    // run, or minimised to tray) — the engine would sit in "Starting" for
    // twenty seconds before adopting a client that was ready all along.
    if already_running && win::main_window(false).is_some() {
        let state = match win::main_window(true) {
            Some(h) => {
                let hidden_by_us = !show_window();
                if hidden_by_us {
                    win::hide(h);
                    log::info!("official client hidden ({ownership:?})");
                } else {
                    log::info!("official client adopted, window left on screen ({ownership:?})");
                }
                EngineState {
                    ownership,
                    hidden_by_us,
                }
            }
            None => {
                // Running with a hidden window is a state the client never
                // puts itself in — it's the fingerprint of an earlier Opal
                // session that hid it and died before releasing. Claim it as
                // ours (`Launched`) so this run cleans it up on exit;
                // adopting it instead is what lets orphans pile up, one
                // invisible 800 MB client per crash.
                log::info!(
                    "official client running but hidden — orphan from a previous session, claiming it"
                );
                if show_window()
                    && let Some(h) = win::main_window(false)
                {
                    win::show(h);
                }
                EngineState {
                    ownership: Ownership::Launched,
                    hidden_by_us: false,
                }
            }
        };
        *ENGINE.lock().unwrap() = Some(state);
        return Ok(state);
    }

    // No window yet: a fresh launch, or a client still starting up. This is
    // the only case that genuinely has to wait.
    let deadline = Instant::now() + WINDOW_WAIT;
    let hwnd = loop {
        if let Some(h) = win::main_window(true) {
            break Some(h);
        }
        if Instant::now() >= deadline {
            break None;
        }
        std::thread::sleep(WINDOW_POLL);
    };

    match hwnd {
        Some(h) => {
            let hidden_by_us = !show_window();
            if hidden_by_us {
                win::hide(h);
                log::info!("official client hidden ({ownership:?})");
            } else {
                win::show(h);
                log::info!("official client up, window left on screen ({ownership:?})");
            }
            let state = EngineState {
                ownership,
                hidden_by_us,
            };
            *ENGINE.lock().unwrap() = Some(state);
            Ok(state)
        }
        // No visible window. Either it's already hidden (fine — it still
        // registers as a Connect device, which is all we need), or it never
        // started (only an error if we're the ones who launched it).
        // Running, but never produced a window at all within the wait. Unlike
        // the hidden-window case above this is genuinely ambiguous — a slow
        // start, or a client sitting in the tray — so it is *not* claimed as
        // an orphan; killing a client that was merely slow would be worse
        // than leaving one behind.
        None if is_running() => {
            log::info!("official client already running with no visible window");
            let state = EngineState {
                ownership,
                hidden_by_us: false,
            };
            *ENGINE.lock().unwrap() = Some(state);
            Ok(state)
        }
        None => Err("Spotify client did not start".to_string()),
    }
}

/// Undo [`acquire`]: bring back a window we hid, and close a process we
/// started. A client the user was already running keeps running.
///
/// Idempotent — releasing when nothing is held does nothing, so the exit
/// hook can call it unconditionally.
pub fn release(restore: Restore) {
    let Some(state) = ENGINE.lock().unwrap().take() else {
        return;
    };
    match state.ownership {
        Ownership::Launched => {
            // Ours: close it and give the memory back. `taskkill` (not a raw
            // TerminateProcess) so the whole CEF process tree goes.
            //
            // Fire-and-forget — `spawn`, never `status`. This runs on the UI
            // thread from the exit hook, and tearing down a seven-process CEF
            // tree takes seconds: waiting for it stops Opal pumping messages
            // and Windows paints the closing app "not responding". The child
            // outlives us (no job object), so the kill still completes after
            // Opal is gone. CREATE_NO_WINDOW keeps a console from flashing.
            log::info!("closing official client (launched by Opal)");
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            use std::os::windows::process::CommandExt;
            if let Err(e) = std::process::Command::new("taskkill")
                .args(["/IM", "Spotify.exe", "/T", "/F"])
                .creation_flags(CREATE_NO_WINDOW)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
            {
                log::warn!("closing official client failed: {e}");
            }
        }
        Ownership::Adopted => {
            if state.hidden_by_us
                && let Some(h) = win::main_window(false)
            {
                log::info!("restoring official client window ({restore:?})");
                match restore {
                    Restore::Show => win::show(h),
                    Restore::Minimized => win::show_minimized(h),
                }
            }
        }
    }
}

/// Drive the client's window to whatever [`show_window`] currently asks
/// for, and report whether anything had to move.
///
/// Two jobs, one `EnumWindows` pass: re-hide a window that came back on its
/// own (an update prompt, a `spotify:` link handoff, a tray-icon click) and
/// apply a flip of the "show the Spotify window" setting. Called from the
/// worker's periodic tick while the engine is active, and once directly on
/// a toggle so the change is immediate. No-ops when the window already
/// matches the policy.
pub fn enforce_window_state() -> bool {
    if show_window() {
        // Only a *hidden* window needs acting on; `main_window(true)` is the
        // visible-only scan, so "has a window but not a visible one" is the
        // one case to fix.
        if win::main_window(true).is_some() {
            return false;
        }
        match win::main_window(false) {
            Some(h) => {
                log::debug!("showing the official client's window");
                win::show(h);
                // We no longer owe the user a re-show on release.
                if let Ok(mut g) = ENGINE.lock()
                    && let Some(state) = g.as_mut()
                {
                    state.hidden_by_us = false;
                }
                true
            }
            None => false,
        }
    } else {
        match win::main_window(true) {
            Some(h) => {
                log::debug!("official client window on screen — hiding it");
                win::hide(h);
                if let Ok(mut g) = ENGINE.lock()
                    && let Some(state) = g.as_mut()
                {
                    state.hidden_by_us = true;
                }
                true
            }
            None => false,
        }
    }
}

/// Diagnostic for the discovery half of the engine — run it against a live
/// client rather than guessing at process/window shapes:
/// `cargo test official_app -- --ignored --nocapture`. Read-only: it never
/// hides or launches anything.
#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "requires the official client installed/running"]
    fn discovery() {
        println!("exe:      {:?}", super::locate());
        println!("pids:     {:?}", super::pids());
        let visible = super::win::main_window(true);
        println!("visible:  {visible:?}");
        println!("any:      {:?}", super::win::main_window(false));
        if let Some(h) = visible {
            println!("class:    {:?}", super::win::class_name_of(h));
        }
    }
}
