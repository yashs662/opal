//! `cargo xtask bundle` — package the app as `Opal.app`, the form macOS
//! expects a GUI app in. A bare `target/release/opal` runs, but it has no
//! Dock icon or name, and a shell that starts it waits on it for the
//! window's whole life; a bundle is launched through LaunchServices
//! (Finder, the Dock, `open`), which returns at once.
//!
//! Layout written under `target/<profile>/`:
//!
//! ```text
//! Opal.app/Contents/
//!   Info.plist            from assets/macos/Info.plist
//!   PkgInfo
//!   MacOS/opal            the built executable
//!   Resources/Opal.icns   compiled from assets/logo/macos/icon-*.png
//! ```
//!
//! The icon goes through Apple's own `iconutil` and the bundle is ad-hoc
//! signed with `codesign` — both ship with macOS (no Xcode needed), which
//! is why the command itself is macOS-only. The file layout is plain
//! filesystem work and is unit-tested on every host.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::{fs, io};

use crate::{build_app, repo_root};

const APP_NAME: &str = "Opal";
const EXE_NAME: &str = "opal";
const INFO_PLIST: &str = include_str!("../../assets/macos/Info.plist");

/// Repo-relative directory of the rendered icon PNGs (`icon-<px>.png`).
const ICON_DIR: &str = "assets/logo/macos";

/// `iconutil`'s required iconset: `(source pixel size, slot name)`. Each
/// point size appears at 1x and 2x, so most renders fill two slots.
const ICONSET: [(u32, &str); 10] = [
    (16, "16x16"),
    (32, "16x16@2x"),
    (32, "32x32"),
    (64, "32x32@2x"),
    (128, "128x128"),
    (256, "128x128@2x"),
    (256, "256x256"),
    (512, "256x256@2x"),
    (512, "512x512"),
    (1024, "512x512@2x"),
];

pub fn run(args: impl Iterator<Item = String>) -> ExitCode {
    let (mut release, mut open) = (true, false);
    for a in args {
        match a.as_str() {
            "--debug" => release = false,
            "--open" => open = true,
            other => {
                eprintln!("unknown bundle option: {other}");
                return ExitCode::FAILURE;
            }
        }
    }
    if !cfg!(target_os = "macos") {
        eprintln!("bundle: an .app bundle can only be built on macOS");
        return ExitCode::FAILURE;
    }

    let root = repo_root();
    let exe = match build_app(&root, release, &[]) {
        Ok(exe) => exe,
        Err(code) => return code,
    };
    let app = match bundle(&root, &exe) {
        Ok(app) => app,
        Err(e) => {
            eprintln!("bundle failed: {e}");
            return ExitCode::FAILURE;
        }
    };
    println!("bundled {}", app.display());
    if open && let Err(e) = run_tool(Command::new("open").arg(&app)) {
        eprintln!("failed to open the bundle: {e}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

/// Build the finished bundle next to `exe` and return its path.
fn bundle(root: &Path, exe: &Path) -> io::Result<PathBuf> {
    let out_dir = exe
        .parent()
        .ok_or_else(|| io::Error::other("executable has no parent directory"))?;
    let app = assemble(root, exe, out_dir)?;
    compile_icon(root, out_dir, &app)?;
    // Ad-hoc signature over the whole bundle, last: it seals Info.plist
    // and the resources, so anything written after it would break it.
    // Apple-silicon Macs refuse to run unsigned code at all.
    run_tool(
        Command::new("codesign")
            .args(["--force", "--sign", "-"])
            .arg(&app),
    )?;
    Ok(app)
}

/// Write the bundle's directory layout, executable and metadata.
fn assemble(root: &Path, exe: &Path, out_dir: &Path) -> io::Result<PathBuf> {
    let manifest = fs::read_to_string(root.join("Cargo.toml"))?;
    let version = package_version(&manifest)
        .ok_or_else(|| io::Error::other("no [package] version in Cargo.toml"))?;

    let app = out_dir.join(format!("{APP_NAME}.app"));
    // Start clean: a file left from an older layout would ship, and would
    // sit outside the signature's seal.
    if app.exists() {
        fs::remove_dir_all(&app)?;
    }
    let contents = app.join("Contents");
    fs::create_dir_all(contents.join("MacOS"))?;
    fs::create_dir_all(contents.join("Resources"))?;
    fs::copy(exe, contents.join("MacOS").join(EXE_NAME))?;
    fs::write(
        contents.join("Info.plist"),
        INFO_PLIST
            .replace("{name}", APP_NAME)
            .replace("{executable}", EXE_NAME)
            .replace("{version}", version),
    )?;
    fs::write(contents.join("PkgInfo"), "APPL????")?;
    Ok(app)
}

/// Compile the rendered PNGs into `Resources/<name>.icns`.
fn compile_icon(root: &Path, out_dir: &Path, app: &Path) -> io::Result<()> {
    let iconset = out_dir.join(format!("{APP_NAME}.iconset"));
    write_iconset(root, &iconset)?;
    let icns = app
        .join("Contents")
        .join("Resources")
        .join(format!("{APP_NAME}.icns"));
    run_tool(
        Command::new("iconutil")
            .args(["--convert", "icns", "--output"])
            .arg(&icns)
            .arg(&iconset),
    )?;
    fs::remove_dir_all(iconset)
}

/// Lay the rendered PNGs out as the `.iconset` directory `iconutil` reads.
fn write_iconset(root: &Path, iconset: &Path) -> io::Result<()> {
    if iconset.exists() {
        fs::remove_dir_all(iconset)?;
    }
    fs::create_dir_all(iconset)?;
    for (px, slot) in ICONSET {
        fs::copy(icon_png(root, px), iconset.join(format!("icon_{slot}.png")))?;
    }
    Ok(())
}

fn icon_png(root: &Path, px: u32) -> PathBuf {
    root.join(ICON_DIR).join(format!("icon-{px}.png"))
}

/// The `[package]` table's `version` value in a Cargo manifest.
fn package_version(manifest: &str) -> Option<&str> {
    manifest
        .lines()
        .skip_while(|l| l.trim() != "[package]")
        .skip(1)
        .take_while(|l| !l.trim_start().starts_with('['))
        .find_map(|l| {
            let (key, value) = l.split_once('=')?;
            (key.trim() == "version").then(|| value.trim().trim_matches('"'))
        })
}

/// Run a system tool to completion; a non-zero exit is an error.
fn run_tool(cmd: &mut Command) -> io::Result<()> {
    let tool = cmd.get_program().to_string_lossy().into_owned();
    let status = cmd
        .status()
        .map_err(|e| io::Error::other(format!("running {tool}: {e}")))?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!("{tool} exited with {status}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("opal-xtask-test-{name}"));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn package_version_reads_the_package_table_only() {
        let manifest = "[workspace]\nmembers = []\n\n[package]\nname = \"opal\"\n\
                        version = \"1.2.3\"\n\n[dependencies]\nversion = \"9\"\n";
        assert_eq!(package_version(manifest), Some("1.2.3"));
        assert_eq!(package_version("[dependencies]\nversion = \"9\"\n"), None);
    }

    #[test]
    fn assemble_writes_a_complete_bundle_layout() {
        let out = scratch_dir("assemble");
        let exe = out.join("opal");
        fs::write(&exe, b"binary").unwrap();
        let root = repo_root();
        // A leftover from an earlier bundle must not survive the rebuild.
        let stale = out.join("Opal.app").join("Contents").join("stale");
        fs::create_dir_all(stale.parent().unwrap()).unwrap();
        fs::write(&stale, b"x").unwrap();

        let app = assemble(&root, &exe, &out).unwrap();
        let contents = app.join("Contents");
        assert!(!stale.exists());
        assert_eq!(
            fs::read(contents.join("MacOS").join("opal")).unwrap(),
            b"binary"
        );
        assert!(contents.join("Resources").is_dir());
        assert_eq!(
            fs::read_to_string(contents.join("PkgInfo")).unwrap(),
            "APPL????"
        );
        let plist = fs::read_to_string(contents.join("Info.plist")).unwrap();
        let manifest = fs::read_to_string(root.join("Cargo.toml")).unwrap();
        let version = package_version(&manifest).unwrap();
        assert!(plist.contains(&format!("<string>{version}</string>")));
        assert!(plist.contains("<string>opal</string>"));
        let unfilled = plist
            .lines()
            .filter(|l| l.contains("<string>"))
            .any(|l| l.contains('{'));
        assert!(!unfilled, "every placeholder must be filled");
        let _ = fs::remove_dir_all(out);
    }

    #[test]
    fn iconset_has_every_slot_at_its_true_pixel_size() {
        let iconset = scratch_dir("iconset").join("Opal.iconset");
        write_iconset(&repo_root(), &iconset).unwrap();
        for (px, slot) in ICONSET {
            let png = fs::read(iconset.join(format!("icon_{slot}.png"))).unwrap();
            // PNG: 8-byte signature, then IHDR — length, tag, width, height.
            assert_eq!(&png[12..16], b"IHDR", "{slot} is not a PNG");
            let dim = |at: usize| u32::from_be_bytes(png[at..at + 4].try_into().unwrap());
            assert_eq!((dim(16), dim(20)), (px, px), "{slot} has the wrong size");
        }
        let _ = fs::remove_dir_all(iconset.parent().unwrap());
    }
}
