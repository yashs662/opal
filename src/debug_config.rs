//! Debug-only launch config + scripted-input loader — **REMOVABLE**.
//!
//! Gated behind the `automation` feature. Replaces the brittle
//! `OPAL_FORCE_HOME` / `OPAL_AUTOCAPTURE` env-var path (which kept
//! failing under PowerShell `$env:` scoping + stale-exe locks) with a
//! reliable JSON config file passed via `--config <path>`. The config can
//! force a startup view, set window size, override the log filter, and
//! carry an automation `script` of timed user actions + screenshots.
//!
//! ## Removing before ship
//! Delete this file, the `automation` feature in `Cargo.toml`, the
//! `#[cfg(feature = "automation")]` lines in `main.rs` (search
//! `debug_config`), and the `debug/` dir. Nothing else references it.

use std::path::PathBuf;
use std::time::Duration;

use opal_gfx::{Script, Step};
use serde::Deserialize;

/// Parsed launch config.
#[derive(Debug, Deserialize, Default)]
pub struct DebugConfig {
    /// Boot straight to Home (skip Splash/Login) — same as the old
    /// `OPAL_FORCE_HOME`.
    #[serde(default)]
    pub force_home: bool,
    /// `[width, height]` logical-px window size.
    #[serde(default)]
    pub window: Option<[u32; 2]>,
    /// `env_logger` filter override (e.g. `"info,opal=debug"`).
    #[serde(default)]
    pub log_filter: Option<String>,
    /// Timed scripted actions. Empty = launch + sit (no automation).
    #[serde(default)]
    pub script: Vec<RawStep>,
}

/// One JSON step. Exactly one action field should be set; the first
/// present (in match order) wins. Coordinates are physical px.
#[derive(Debug, Deserialize)]
pub struct RawStep {
    #[serde(default)]
    pub wait_ms: Option<u64>,
    #[serde(default)]
    pub screenshot: Option<String>,
    #[serde(default)]
    pub move_mouse: Option<[f32; 2]>,
    #[serde(default)]
    pub click: Option<[f32; 2]>,
    #[serde(default)]
    pub right_click: Option<[f32; 2]>,
    #[serde(default)]
    pub hover: Option<[f32; 2]>,
    /// Dwell for `hover` (default 400 ms).
    #[serde(default)]
    pub dwell_ms: Option<u64>,
    #[serde(default)]
    pub scroll: Option<[f32; 2]>,
    /// Wheel lines for `scroll` (e.g. `[0, -5]` = scroll down 5 lines).
    #[serde(default)]
    pub by: Option<[f32; 2]>,
    #[serde(default)]
    pub drag: Option<[[f32; 2]; 2]>,
}

impl RawStep {
    fn into_step(self) -> Option<Step> {
        if let Some(ms) = self.wait_ms {
            return Some(Step::Wait(Duration::from_millis(ms)));
        }
        if let Some(p) = self.screenshot {
            return Some(Step::Screenshot(PathBuf::from(p)));
        }
        if let Some(p) = self.move_mouse {
            return Some(Step::MoveMouse(p));
        }
        if let Some(p) = self.click {
            return Some(Step::Click(p));
        }
        if let Some(p) = self.right_click {
            return Some(Step::RightClick(p));
        }
        if let Some(p) = self.hover {
            let d = Duration::from_millis(self.dwell_ms.unwrap_or(400));
            return Some(Step::Hover(p, d));
        }
        if let Some(p) = self.scroll {
            return Some(Step::Scroll(p, self.by.unwrap_or([0.0, 0.0])));
        }
        if let Some([a, b]) = self.drag {
            return Some(Step::Drag(a, b));
        }
        None
    }
}

impl DebugConfig {
    /// Build the engine `Script` from the raw steps (dropping any empty
    /// step objects).
    pub fn script(&self) -> Script {
        Script::new(
            self.script
                .iter()
                .cloned()
                .filter_map(|s| s.into_step())
                .collect(),
        )
    }
}

// `RawStep` needs Clone for `script()` to consume by value cheaply.
impl Clone for RawStep {
    fn clone(&self) -> Self {
        Self {
            wait_ms: self.wait_ms,
            screenshot: self.screenshot.clone(),
            move_mouse: self.move_mouse,
            click: self.click,
            right_click: self.right_click,
            hover: self.hover,
            dwell_ms: self.dwell_ms,
            scroll: self.scroll,
            by: self.by,
            drag: self.drag,
        }
    }
}

/// Read `--config <path>` from the process args, parse the JSON. Returns
/// `None` if no `--config` flag was given. Logs + returns `None` on a
/// read/parse error (so a bad config doesn't hard-crash the app).
pub fn from_args() -> Option<DebugConfig> {
    let mut args = std::env::args().skip(1);
    let mut path: Option<String> = None;
    while let Some(a) = args.next() {
        if a == "--config" {
            path = args.next();
            break;
        } else if let Some(p) = a.strip_prefix("--config=") {
            path = Some(p.to_string());
            break;
        }
    }
    let path = path?;
    match std::fs::read_to_string(&path) {
        Ok(text) => match serde_json::from_str::<DebugConfig>(&text) {
            Ok(cfg) => {
                log::info!("[debug-config] loaded {path}");
                Some(cfg)
            }
            Err(e) => {
                log::error!("[debug-config] parse {path} failed: {e}");
                None
            }
        },
        Err(e) => {
            log::error!("[debug-config] read {path} failed: {e}");
            None
        }
    }
}
