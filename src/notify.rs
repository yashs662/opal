//! Log tap — turns the app's own `warn!` / `error!` lines into notices the
//! UI can show (toasts + the notification history), so a release build
//! with no console still surfaces what went wrong.
//!
//! The logger installed by [`init`] wraps `env_logger`: every record still
//! reaches the console exactly as before, and every **WARN / ERROR**
//! record is *also* captured here, routed by [`RULES`], and parked in a
//! small inbox the frame tick drains ([`drain`]). INFO and below are never
//! captured — they are console-only (and filtered out entirely in release).
//!
//! # Configuring what shows
//! * **Per call site** — give the record a `toast` key with a short,
//!   user-facing title and it pops a toast; the formatted message becomes
//!   the toast body and the history detail:
//!   ```ignore
//!   log::warn!(toast = "Couldn't switch device"; "transfer to {id} failed: {e}");
//!   ```
//! * **Globally** — [`RULES`] decides the default route for a record by
//!   target prefix and level: toast it, keep it in history only, or drop it.
//!   A `toast` key upgrades a history-only record to a toast; it never
//!   revives a dropped one.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use log::{Level, LevelFilter, Log, Metadata, Record};
use opal_gfx::WakeHandle;

/// How bad a notice is — the two levels the tap captures.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Warning,
    Error,
}

/// Which notices pop up as toasts — the user's setting (Settings →
/// Pop-up notifications). Only the pop-up is filtered: every notice still
/// lands in the history and lights the bell, so a silenced one is always one
/// click away.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ToastLevel {
    /// Warnings and errors.
    #[default]
    All,
    ErrorsOnly,
    /// No pop-ups at all.
    Off,
}

impl ToastLevel {
    /// Does a notice of `severity` pop up at this level?
    pub fn allows(self, severity: Severity) -> bool {
        match self {
            ToastLevel::All => true,
            ToastLevel::ErrorsOnly => severity == Severity::Error,
            ToastLevel::Off => false,
        }
    }
}

/// Where a captured record goes.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Route {
    /// Pop a toast *and* record it in the history.
    Toast,
    /// Record it in the history only.
    History,
    /// Ignore it (console only). No default rule drops anything yet — this
    /// is the knob for silencing a noisy target.
    #[allow(dead_code)]
    Drop,
}

/// One routing rule: records whose target starts with `target` (a module
/// path prefix, `""` = any) at `level` or worse take `route`.
pub struct Rule {
    pub target: &'static str,
    pub level: Level,
    pub route: Route,
}

/// Default routing, first match wins. Records matching nothing are dropped.
pub const RULES: &[Rule] = &[
    // The GPU backend reports driver/loader chatter (the Vulkan loader's
    // startup messages) at ERROR — keep it on file, never interrupt.
    Rule {
        target: "wgpu_hal",
        level: Level::Warn,
        route: Route::History,
    },
    // Every error is worth interrupting for.
    Rule {
        target: "",
        level: Level::Error,
        route: Route::Toast,
    },
    // Warnings are diagnostics until a call site names a toast title for
    // one — then it's something the user should hear about.
    Rule {
        target: "",
        level: Level::Warn,
        route: Route::History,
    },
];

/// The record key a call site sets to opt a warning into a toast; its
/// value is the toast's title.
const TOAST_KEY: &str = "toast";

/// Captured notices waiting for the UI. Bounded so a warning storm with no
/// UI to drain it (before the window exists, or a wedged frame) can't grow
/// without limit — the oldest are dropped first.
const INBOX_CAP: usize = 256;

/// A captured WARN/ERROR record, ready for the UI.
#[derive(Clone, Debug)]
pub struct Notice {
    pub severity: Severity,
    /// The call site's `toast` title, if it gave one.
    pub title: Option<String>,
    /// The formatted log message.
    pub body: String,
    /// The record's target (module path), for the history detail line.
    pub target: String,
    pub at: chrono::DateTime<chrono::Local>,
    /// Pop a toast (vs history only).
    pub toast: bool,
}

static INBOX: Mutex<Vec<Notice>> = Mutex::new(Vec::new());
/// Lock-free "anything to drain?" flag — the frame tick checks it every
/// frame, so the idle path must not take the mutex.
static PENDING: AtomicBool = AtomicBool::new(false);
static WAKE: OnceLock<Arc<WakeHandle>> = OnceLock::new();

/// Install the tapping logger. `filter` is the console's default
/// `env_logger` filter (`RUST_LOG` still overrides it); capture of WARN /
/// ERROR happens regardless of it.
pub fn init(filter: &str) {
    let inner =
        env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(filter)).build();
    let max = inner.filter().max(LevelFilter::Warn);
    if log::set_boxed_logger(Box::new(Tap { inner })).is_ok() {
        log::set_max_level(max);
    }
}

/// Hand the tap the loop's wake handle, so a notice logged from a worker
/// thread wakes an idle UI to show it. Notices logged before this are kept
/// and shown on the first frame.
pub fn attach(wake: Arc<WakeHandle>) {
    let _ = WAKE.set(wake);
}

/// Take every notice captured since the last drain. One relaxed atomic load
/// when there's nothing new.
pub fn drain() -> Vec<Notice> {
    if !PENDING.swap(false, Ordering::Acquire) {
        return Vec::new();
    }
    std::mem::take(&mut *INBOX.lock().unwrap_or_else(|e| e.into_inner()))
}

struct Tap {
    inner: env_logger::Logger,
}

impl Log for Tap {
    fn enabled(&self, meta: &Metadata) -> bool {
        meta.level() <= Level::Warn || self.inner.enabled(meta)
    }

    fn log(&self, record: &Record) {
        // env_logger applies its own filter here — console output unchanged.
        self.inner.log(record);
        if record.level() <= Level::Warn {
            capture(record);
        }
    }

    fn flush(&self) {
        self.inner.flush();
    }
}

fn capture(record: &Record) {
    let target = record.target();
    let level = record.level();
    let Some(rule) = RULES.iter().find(|r| {
        level <= r.level
            && (r.target.is_empty()
                || target == r.target
                || target
                    .strip_prefix(r.target)
                    .is_some_and(|rest| rest.starts_with("::")))
    }) else {
        return;
    };
    let title = record
        .key_values()
        .get(log::kv::Key::from_str(TOAST_KEY))
        .map(|v| v.to_string());
    let toast = match rule.route {
        Route::Drop => return,
        Route::Toast => true,
        Route::History => title.is_some(),
    };
    let notice = Notice {
        severity: if level == Level::Error {
            Severity::Error
        } else {
            Severity::Warning
        },
        title,
        body: record.args().to_string(),
        target: target.to_string(),
        at: chrono::Local::now(),
        toast,
    };
    {
        let mut inbox = INBOX.lock().unwrap_or_else(|e| e.into_inner());
        if inbox.len() >= INBOX_CAP {
            inbox.remove(0);
        }
        inbox.push(notice);
    }
    PENDING.store(true, Ordering::Release);
    if let Some(wake) = WAKE.get() {
        wake.wake();
    }
}
