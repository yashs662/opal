//! Dev-tooling entry point (`cargo xtask <command>`), replacing the old
//! per-OS `debug/run.ps1` / `run.sh` launchers with one cross-platform
//! binary. Dependency-free on purpose — it shells out to `cargo` and the
//! OS process tools, nothing more.
//!
//! Commands:
//! - `debug [--release] [config.json]` — run the scripted-input +
//!   screenshot harness (see `debug/README.md`): kill any stale `opal`
//!   process (whose file lock otherwise breaks the build mid-session),
//!   build with the `automation` feature, and run against the given
//!   config (default `debug/home.json`). `--release` runs the optimized
//!   build — debug-vs-release perf comparisons.
//! - `bundle [--debug] [--open]` — macOS: build (release unless
//!   `--debug`) and package `target/<profile>/Opal.app`, icon and all;
//!   `--open` launches it. See [`bundle`].

mod bundle;

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, ExitStatus};

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("debug") => debug_harness(args),
        Some("bundle") => bundle::run(args),
        _ => {
            eprintln!(
                "usage: cargo xtask debug [--release] [config.json]\n       \
                 cargo xtask bundle [--debug] [--open]"
            );
            ExitCode::FAILURE
        }
    }
}

/// Build the app (`cargo build <extra> [--release]`) and return the path
/// of the executable it produced. `Err` carries the exit code to end with.
fn build_app(root: &Path, release: bool, extra: &[&str]) -> Result<PathBuf, ExitCode> {
    let mut cmd = Command::new("cargo");
    cmd.arg("build").args(extra).current_dir(root);
    if release {
        cmd.arg("--release");
    }
    match cmd.status() {
        Ok(s) if s.success() => {}
        Ok(s) => return Err(exit_code(s)),
        Err(e) => {
            eprintln!("failed to invoke cargo: {e}");
            return Err(ExitCode::FAILURE);
        }
    }
    Ok(root
        .join("target")
        .join(if release { "release" } else { "debug" })
        .join(format!("opal{}", std::env::consts::EXE_SUFFIX)))
}

/// A child's exit status as ours (a signal death reads as plain failure).
fn exit_code(status: ExitStatus) -> ExitCode {
    ExitCode::from(status.code().unwrap_or(1) as u8)
}

/// Repo root = parent of this crate's manifest dir (`<root>/xtask`).
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives one level under the repo root")
        .to_path_buf()
}

fn debug_harness(args: impl Iterator<Item = String>) -> ExitCode {
    let root = repo_root();
    // `--release` runs the harness against an optimized build (perf work:
    // debug-vs-release comparisons); any other arg is the config path.
    let mut release = false;
    let mut config: Option<String> = None;
    for a in args {
        match a.as_str() {
            "--release" => release = true,
            _ => config = Some(a),
        }
    }
    let config = config.unwrap_or_else(|| "debug/home.json".into());
    if !root.join(&config).is_file() && !Path::new(&config).is_file() {
        eprintln!("config not found: {config}");
        return ExitCode::FAILURE;
    }

    kill_stale_opal();

    let exe = match build_app(&root, release, &["--features", "automation"]) {
        Ok(exe) => exe,
        Err(code) => return code,
    };
    match Command::new(exe)
        .args(["--config", &config])
        .current_dir(&root)
        .status()
    {
        Ok(s) if s.success() => ExitCode::SUCCESS,
        Ok(s) => exit_code(s),
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
