//! Dev-tooling entry point (`cargo xtask <command>`), replacing the old
//! per-OS `debug/run.ps1` / `run.sh` launchers with one cross-platform
//! binary. Dependency-free on purpose — it shells out to `cargo` and the
//! OS process tools, nothing more.
//!
//! Commands:
//! - `debug [config.json]` — run the scripted-input + screenshot harness
//!   (see `debug/README.md`): kill any stale `opal` process (whose file
//!   lock otherwise breaks the build mid-session), build with the
//!   `automation` feature, and run against the given config
//!   (default `debug/home.json`).

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("debug") => debug_harness(args.next()),
        _ => {
            eprintln!("usage: cargo xtask debug [config.json]");
            ExitCode::FAILURE
        }
    }
}

/// Repo root = parent of this crate's manifest dir (`<root>/xtask`).
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives one level under the repo root")
        .to_path_buf()
}

fn debug_harness(config: Option<String>) -> ExitCode {
    let root = repo_root();
    let config = config.unwrap_or_else(|| "debug/home.json".into());
    if !root.join(&config).is_file() && !Path::new(&config).is_file() {
        eprintln!("config not found: {config}");
        return ExitCode::FAILURE;
    }

    kill_stale_opal();

    let build = Command::new("cargo")
        .args(["build", "--features", "automation"])
        .current_dir(&root)
        .status();
    match build {
        Ok(s) if s.success() => {}
        Ok(s) => return ExitCode::from(s.code().unwrap_or(1) as u8),
        Err(e) => {
            eprintln!("failed to invoke cargo: {e}");
            return ExitCode::FAILURE;
        }
    }

    let exe = root
        .join("target/debug")
        .join(format!("opal{}", std::env::consts::EXE_SUFFIX));
    match Command::new(exe)
        .args(["--config", &config])
        .current_dir(&root)
        .status()
    {
        Ok(s) if s.success() => ExitCode::SUCCESS,
        Ok(s) => ExitCode::from(s.code().unwrap_or(1) as u8),
        Err(e) => {
            eprintln!("failed to launch opal: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Kill any stale `opal` process — a lingering instance from an aborted
/// run holds the executable's file lock and fails the next build's link
/// step. Best-effort: "no such process" is the happy path.
fn kill_stale_opal() {
    #[cfg(windows)]
    let status = Command::new("taskkill")
        .args(["/F", "/IM", "opal.exe"])
        .output();
    #[cfg(not(windows))]
    let status = Command::new("pkill").args(["-x", "opal"]).output();
    if let Ok(out) = status
        && out.status.success()
    {
        // Give the OS a beat to release the executable's file lock.
        std::thread::sleep(std::time::Duration::from_millis(300));
        println!("killed stale opal instance");
    }
}
